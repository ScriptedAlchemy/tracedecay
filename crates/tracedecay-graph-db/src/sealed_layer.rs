//! Layered sealed generations: a small delta over a sealed base.
//!
//! A refresh that changes a handful of files would otherwise re-encode and
//! re-prove every row of its generation. A layered generation instead keeps
//! the cold-built container of an earlier generation, its *base*, shared by
//! hard link, and seals only what differs from it:
//!
//! ```text
//! <layered generation>/
//!   generation.grafeo   <- delta rows, under this generation's namespace
//!   base.grafeo         <- hard link to the base's container
//!   base.attachment     <- hard link to the base's producer attachment
//!   base.rows.index     <- hard link to the base's row digest index
//!   hidden.json         <- base row identities this generation removes
//!   sealed.json         <- receipt, naming the base and both row sums
//! ```
//!
//! A row the delta carries shadows the base row of the same identity, a
//! hidden identity serves nothing from the base, and every other base row
//! serves as is. Every layered generation is a delta over a cold base, never
//! over another delta, so reads always merge exactly two stores and the
//! delta is cumulative since that base; the producer decides when a delta is
//! large enough to rebuild cold instead.
//!
//! The recovered digest is a set digest (see [`GraphRowDigestSum`]), so the
//! delta's digest is the base's row sum minus the base rows it hides or
//! shadows plus its own rows. The base's row index (see `row_index`) holds
//! each base row's digest, so a refresh never opens the base graph. A cold
//! build of the same rows records the same digest, which is what lets the
//! journal replay a layered publication cold.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};
use tracedecay_store::runtime::GraphRecoveredGenerationDigestV1;

use crate::generation::{
    GraphRowDigestSum, RowLanes, read_spilled_entity, recovered_digest_from_row_sum,
};
use crate::projection_read::{IdentityScope, query_identity_page};
use crate::row_index::{ENTITY_ROW_OFFSETS_FILE, EntityRowOffsets, ROW_INDEX_FILE, RowIndex};
use crate::schema::{
    ENTITY_ID_PROPERTY, ENTITY_LABEL, RELATION_ID_PROPERTY, RELATION_LABEL,
    entity_projection_label, relation_projection_label,
};
use crate::state::{latest_projection, load_entity, load_relation};
use crate::traversal::{GraphTraversalDirection, TraversalRequest};
use crate::verified_marker::marker_path;
use crate::{
    GraphBudgetKind, GraphCancellation, GraphDb, GraphDbError, GraphEntity, GraphEntityId,
    GraphEntityRef, GraphGenerationId, GraphGenerationManifestIdentity, GraphGenerationRelation,
    GraphGenerationRowSpill, GraphNamespace, GraphProjectionIdentity, GraphProjectionPage,
    GraphProjectionReadRequest, GraphProjectionTelemetry, GraphProjectionTelemetryRequest,
    GraphRelation, GraphRelationId, GraphRelationKind, GraphRelationRef, GraphRelationTarget,
    GraphWatermark, NeverCancelled, SourceGeneration, SpilledGraphGeneration,
    VerifiedTraversalResult, VerifiedTraversalVisit,
};

pub(crate) const LAYERED_BASE_CONTAINER_FILE: &str = "base.grafeo";
pub(crate) const LAYERED_BASE_ATTACHMENT_FILE: &str = "base.attachment";
pub(crate) const LAYERED_BASE_ROW_INDEX_FILE: &str = "base.rows.index";
pub(crate) const LAYERED_BASE_ENTITY_ROWS_FILE: &str = "base.entities.rows";
pub(crate) const LAYERED_BASE_ENTITY_ROW_OFFSETS_FILE: &str = "base.entity-rows.offsets";
pub(crate) const LAYERED_HIDDEN_FILE: &str = "hidden.json";
/// A flat generation's producer attachment, sealed beside its container.
pub(crate) const GENERATION_ATTACHMENT_FILE: &str = "attachment";

fn layered_io(context: &str, error: std::io::Error) -> GraphDbError {
    GraphDbError::unavailable(format!("layered sealed generation {context}: {error}"))
}

/// What a layered generation's receipt records about its base, enough to
/// reopen and re-prove the hard-linked base container on its own.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct SealedBaseReceiptV1 {
    pub(crate) generation: String,
    pub(crate) source_generation: String,
    pub(crate) watermark: String,
    pub(crate) physical_namespace: String,
    pub(crate) recovered_digest: String,
    pub(crate) entities: usize,
    pub(crate) relations: usize,
    pub(crate) row_sum: String,
    /// Sum of the base rows the layer hides or shadows.
    pub(crate) hidden_row_sum: String,
}

impl SealedBaseReceiptV1 {
    pub(crate) fn identity(
        &self,
        projection: &GraphProjectionIdentity,
    ) -> Result<GraphGenerationManifestIdentity, GraphDbError> {
        Ok(GraphGenerationManifestIdentity::new(
            projection.clone(),
            GraphGenerationId::new(self.generation.clone())?,
            SourceGeneration::new(self.source_generation.clone())?,
            GraphWatermark::new(self.watermark.clone())?,
            Vec::new(),
        ))
    }
}

/// Base row identities a layered generation removes.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HiddenRowsV1 {
    pub(crate) entities: Vec<GraphEntityId>,
    pub(crate) relations: Vec<GraphRelationId>,
}

/// Why a generation offers no sealed base for a refresh to layer over; the
/// refresh then seals cold.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphSealedBaseAbsenceV1 {
    /// Sealed stores are switched off, or the store has no disk.
    SealedStoreUnavailable,
    /// No sealed artifact of the generation exists.
    NoArtifact,
    /// The artifact was sealed under another receipt or graph format.
    SupersededArtifact,
    /// The artifact's post-reopen proof was never recorded.
    UnprovenArtifact,
    /// The artifact was sealed from staging rows and records no row sum.
    NoRowSum,
    /// The recorded row sum does not digest to the recorded head.
    RowSumMismatch,
    /// The artifact's producer sealed no attachment to layer from.
    NoAttachment,
    /// The artifact carries no row index to derive a delta's digest from.
    NoRowIndex,
    /// The artifact carries no point-readable canonical entity rows.
    NoEntityRows,
    /// The row index does not record the rows the base's row sum holds.
    RowIndexMismatch,
}

/// The files a sealed base is made of, beside its receipt.
pub(crate) struct SealedBaseFilesV1 {
    pub(crate) container: PathBuf,
    pub(crate) attachment: PathBuf,
    pub(crate) row_index: PathBuf,
    pub(crate) entity_rows: PathBuf,
    pub(crate) entity_row_offsets: PathBuf,
}

impl SealedBaseFilesV1 {
    /// A flat generation's container and base files, or why it has none.
    pub(crate) fn flat(directory: &Path) -> Result<Self, GraphSealedBaseAbsenceV1> {
        Self::find(
            directory,
            crate::sealed_store::SEALED_STORE_DATABASE_FILE,
            GENERATION_ATTACHMENT_FILE,
            ROW_INDEX_FILE,
            crate::generation::ENTITIES_FILE,
            ENTITY_ROW_OFFSETS_FILE,
        )
    }

    /// A layered generation's linked base files, or why it has none.
    pub(crate) fn layered(directory: &Path) -> Result<Self, GraphSealedBaseAbsenceV1> {
        Self::find(
            directory,
            LAYERED_BASE_CONTAINER_FILE,
            LAYERED_BASE_ATTACHMENT_FILE,
            LAYERED_BASE_ROW_INDEX_FILE,
            LAYERED_BASE_ENTITY_ROWS_FILE,
            LAYERED_BASE_ENTITY_ROW_OFFSETS_FILE,
        )
    }

    fn find(
        directory: &Path,
        container: &str,
        attachment: &str,
        row_index: &str,
        entity_rows: &str,
        entity_row_offsets: &str,
    ) -> Result<Self, GraphSealedBaseAbsenceV1> {
        let attachment = directory.join(attachment);
        if !attachment.is_file() {
            return Err(GraphSealedBaseAbsenceV1::NoAttachment);
        }
        let row_index = directory.join(row_index);
        if !row_index.is_file() {
            return Err(GraphSealedBaseAbsenceV1::NoRowIndex);
        }
        let entity_rows = directory.join(entity_rows);
        let entity_row_offsets = directory.join(entity_row_offsets);
        if !entity_rows.is_file() || !entity_row_offsets.is_file() {
            return Err(GraphSealedBaseAbsenceV1::NoEntityRows);
        }
        Ok(Self {
            container: directory.join(container),
            attachment,
            row_index,
            entity_rows,
            entity_row_offsets,
        })
    }
}

/// A sealed cold generation a refresh may layer over, resolved from the
/// generation a publication replaces.
#[derive(Clone)]
pub struct GraphSealedBaseV1 {
    inner: Arc<SealedBaseInner>,
}

struct SealedBaseInner {
    identity: GraphGenerationManifestIdentity,
    physical_namespace: GraphNamespace,
    recovered_digest: String,
    entities: usize,
    relations: usize,
    row_sum: GraphRowDigestSum,
    container: PathBuf,
    attachment: PathBuf,
    /// Every base row's digest, for the point lookups that derive a delta's
    /// digest; the base graph itself is never opened.
    index: RowIndex,
    entity_rows: PathBuf,
    entity_row_offsets: EntityRowOffsets,
}

impl std::fmt::Debug for GraphSealedBaseV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GraphSealedBaseV1")
            .field("generation", &self.inner.identity.generation)
            .field("entities", &self.inner.entities)
            .field("relations", &self.inner.relations)
            .finish_non_exhaustive()
    }
}

impl GraphSealedBaseV1 {
    /// A base over `files` whose receipt records `rows` and `row_sum`, or
    /// [`GraphSealedBaseAbsenceV1::RowIndexMismatch`] when the row index does
    /// not record exactly those rows.
    pub(crate) fn open(
        identity: GraphGenerationManifestIdentity,
        recovered_digest: String,
        (entities, relations): (usize, usize),
        row_sum: GraphRowDigestSum,
        files: SealedBaseFilesV1,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Result<Self, GraphSealedBaseAbsenceV1>, GraphDbError> {
        let physical_namespace = identity.physical_namespace()?;
        let index = match RowIndex::open(&files.row_index) {
            Ok(index) => index,
            Err(GraphDbError::Corrupt { .. }) => {
                return Ok(Err(GraphSealedBaseAbsenceV1::RowIndexMismatch));
            }
            Err(error) => return Err(error),
        };
        let entity_row_offsets =
            match EntityRowOffsets::open(&files.entity_row_offsets, &files.entity_rows) {
                Ok(offsets) => offsets,
                Err(GraphDbError::Corrupt { .. }) => {
                    return Ok(Err(GraphSealedBaseAbsenceV1::RowIndexMismatch));
                }
                Err(error) => return Err(error),
            };
        if index.row_counts() != (entities as u64, relations as u64)
            || entity_row_offsets.entities() != entities as u64
            || index.row_sum(check)? != row_sum
        {
            return Ok(Err(GraphSealedBaseAbsenceV1::RowIndexMismatch));
        }
        Ok(Ok(Self {
            inner: Arc::new(SealedBaseInner {
                identity,
                physical_namespace,
                recovered_digest,
                entities,
                relations,
                row_sum,
                container: files.container,
                attachment: files.attachment,
                index,
                entity_rows: files.entity_rows,
                entity_row_offsets,
            }),
        }))
    }

    /// The base's graph generation.
    #[must_use]
    pub fn generation(&self) -> &GraphGenerationId {
        &self.inner.identity.generation
    }

    /// `(entities, relations)` the base serves.
    #[must_use]
    pub fn row_counts(&self) -> (usize, usize) {
        (self.inner.entities, self.inner.relations)
    }
}

fn generation_relation(
    projection: &GraphProjectionIdentity,
    relation: GraphRelation,
) -> Result<GraphGenerationRelation, GraphDbError> {
    GraphGenerationRelation::new(
        relation.identity,
        GraphEntityRef::new(projection.clone(), relation.from),
        GraphEntityRef::new(projection.clone(), relation.to),
        relation.kind,
        relation.properties,
    )
}

/// Rows a refresh changes relative to a sealed base, spilled like a cold
/// generation's rows, plus the base identities it removes.
///
/// The spill hard-links the base container, producer attachment, row index,
/// and endpoint sidecars when it is created, so every byte the delta derives
/// from survives base-generation retirement until the delta seals.
pub struct GraphLayeredRowSpill {
    spill: GraphGenerationRowSpill,
    base: GraphSealedBaseV1,
    hidden_entities: BTreeSet<GraphEntityId>,
    hidden_relations: BTreeSet<GraphRelationId>,
}

impl std::fmt::Debug for GraphLayeredRowSpill {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GraphLayeredRowSpill")
            .field("base", &self.base)
            .field("hidden_entities", &self.hidden_entities.len())
            .field("hidden_relations", &self.hidden_relations.len())
            .finish_non_exhaustive()
    }
}

impl GraphLayeredRowSpill {
    pub(crate) fn create(
        directory: PathBuf,
        projection: GraphProjectionIdentity,
        base: GraphSealedBaseV1,
    ) -> Result<Self, GraphDbError> {
        if projection != base.inner.identity.projection {
            return Err(GraphDbError::invalid(
                "a layered generation names a projection its base does not serve",
            ));
        }
        let spill = GraphGenerationRowSpill::create_layered(directory, projection)?;
        let pinned_container = spill.directory().join(LAYERED_BASE_CONTAINER_FILE);
        std::fs::hard_link(&base.inner.container, &pinned_container)
            .map_err(|error| layered_io("base container pin", error))?;
        // The base's verify-once marker binds the container's bytes, not its
        // path, so it vouches for the pinned link too; without it the layer's
        // first open re-proves every base row.
        match std::fs::copy(
            marker_path(&base.inner.container),
            marker_path(&pinned_container),
        ) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(layered_io("base marker pin", error)),
        }
        for (source, name) in [
            (&base.inner.attachment, LAYERED_BASE_ATTACHMENT_FILE),
            (
                &base.inner.index.path().to_path_buf(),
                LAYERED_BASE_ROW_INDEX_FILE,
            ),
            (&base.inner.entity_rows, LAYERED_BASE_ENTITY_ROWS_FILE),
            (
                &base.inner.entity_row_offsets.path().to_path_buf(),
                LAYERED_BASE_ENTITY_ROW_OFFSETS_FILE,
            ),
        ] {
            std::fs::hard_link(source, spill.directory().join(name))
                .map_err(|error| layered_io("base file pin", error))?;
        }
        Ok(Self {
            spill,
            base,
            hidden_entities: BTreeSet::new(),
            hidden_relations: BTreeSet::new(),
        })
    }

    #[must_use]
    pub fn base(&self) -> &GraphSealedBaseV1 {
        &self.base
    }

    /// The base's producer attachment, pinned with the spill.
    #[must_use]
    pub fn base_attachment(&self) -> PathBuf {
        self.spill.directory().join(LAYERED_BASE_ATTACHMENT_FILE)
    }

    /// Adds changed or new rows. A row whose identity the base also serves
    /// replaces it. Every endpoint a relation names must be pushed too, the
    /// base's own row where nothing changed; see [`Self::missing_endpoints`].
    pub fn push_batch(
        &mut self,
        entities: Vec<GraphEntity>,
        relations: Vec<GraphGenerationRelation>,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        self.spill.push_batch(entities, relations, check)
    }

    /// Removes base rows. Identities the base does not serve are ignored,
    /// and a pushed row of the same identity still serves.
    pub fn hide(
        &mut self,
        entities: impl IntoIterator<Item = GraphEntityId>,
        relations: impl IntoIterator<Item = GraphRelationId>,
    ) {
        self.hidden_entities.extend(entities);
        self.hidden_relations.extend(relations);
    }

    /// The generation's entity count with the rows pushed and hidden so
    /// far: what a cold build of the same rows counts.
    pub fn entity_count(
        &mut self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<usize, GraphDbError> {
        let delta = self.spill.sorted_entity_identities().to_vec();
        let mut present = 0_usize;
        for id in delta
            .iter()
            .chain(self.hidden_entities.iter())
            .collect::<BTreeSet<_>>()
        {
            check()?;
            if self.base.inner.index.entity(id.as_str())?.is_some() {
                present += 1;
            }
        }
        (self.base.inner.entities + delta.len())
            .checked_sub(present)
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "a layered generation hides more entities than its base holds".to_owned(),
            })
    }

    /// Relation endpoints pushed so far that no pushed entity carries. The
    /// producer pushes each one's row before [`Self::finish`], which refuses
    /// a delta that still misses one.
    pub fn missing_endpoints(&mut self) -> Vec<GraphEntityId> {
        self.spill.missing_endpoints()
    }

    /// Copies exact unchanged endpoint rows from the pinned cold base without
    /// opening its graph engine or materializing the endpoint owners' pages.
    pub fn copy_base_endpoints(
        &mut self,
        endpoints: impl IntoIterator<Item = GraphEntityId>,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        for endpoint in endpoints {
            check()?;
            let indexed = self
                .base
                .inner
                .index
                .entity(endpoint.as_str())?
                .ok_or_else(|| {
                    GraphDbError::invalid(format!(
                        "layered relation endpoint `{endpoint}` is absent from its base"
                    ))
                })?;
            let (offset, length) = self.base.inner.entity_row_offsets.row(indexed.ordinal)?;
            let entity = read_spilled_entity(
                &self.spill.directory().join(LAYERED_BASE_ENTITY_ROWS_FILE),
                offset,
                length,
                &endpoint,
                indexed.lanes,
            )?;
            self.spill.push_batch(vec![entity], Vec::new(), check)?;
        }
        Ok(())
    }

    /// Merges the delta and derives the layered generation's row sum and
    /// digest from the base's, reading only the base row index.
    #[hotpath::measure(label = "graph_db.sealed_layer.finish")]
    pub fn finish(
        mut self,
        identity: GraphGenerationManifestIdentity,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<LayeredGraphGeneration, GraphDbError> {
        check()?;
        if identity.projection != self.base.inner.identity.projection {
            return Err(GraphDbError::invalid(
                "a layered generation names a projection its base does not serve",
            ));
        }
        if let Some(endpoint) = self.spill.missing_endpoints().first() {
            return Err(GraphDbError::invalid(format!(
                "layered relation endpoint `{endpoint}` is not carried by the delta"
            )));
        }
        let Self {
            spill,
            base,
            hidden_entities,
            hidden_relations,
        } = self;
        let delta = spill.finish(identity.clone(), check)?;
        let shadowed =
            shadowed_base_rows(&base, &hidden_entities, &hidden_relations, &delta, check)?;
        let (delta_entity_count, delta_relation_count) = delta.row_counts();
        let mut row_sum = base.inner.row_sum;
        row_sum.subtract(shadowed.sum)?;
        row_sum.merge(delta.row_sum());
        let expected_recovered_digest = recovered_digest_from_row_sum(&identity, row_sum, check)?;
        #[cfg(feature = "hotpath")]
        {
            hotpath::gauge!("graph_db.sealed_layer.delta_entities").inc(delta_entity_count as u64);
            hotpath::gauge!("graph_db.sealed_layer.delta_relations")
                .inc(delta_relation_count as u64);
            hotpath::gauge!("graph_db.sealed_layer.hidden_rows")
                .inc((shadowed.hidden.entities.len() + shadowed.hidden.relations.len()) as u64);
        }
        Ok(LayeredGraphGeneration {
            identity,
            delta,
            base,
            hidden: shadowed.hidden,
            hidden_sum: shadowed.sum,
            entity_count: shadowed.base_entities + delta_entity_count,
            relation_count: shadowed.base_relations + delta_relation_count,
            row_sum,
            expected_recovered_digest,
        })
    }
}

/// The base rows the finished `delta` shadows or the layer hides: their
/// row sum, the identities left hidden, and the base rows that stay.
fn shadowed_base_rows(
    base: &GraphSealedBaseV1,
    hidden_entities: &BTreeSet<GraphEntityId>,
    hidden_relations: &BTreeSet<GraphRelationId>,
    delta: &SpilledGraphGeneration,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<ShadowedBaseRows, GraphDbError> {
    let mut shadowed = ShadowedBaseRows {
        sum: GraphRowDigestSum::default(),
        hidden: HiddenRowsV1::default(),
        base_entities: base.inner.entities,
        base_relations: base.inner.relations,
    };
    let index = &base.inner.index;
    let mut removed = shadowed.shadow_entities(index, hidden_entities, delta, check)?;
    shadowed.shadow_relations(index, hidden_relations, delta, &mut removed, check)?;
    if let Some((id, _)) = removed.values().find(|(_, degree)| *degree != 0) {
        return Err(GraphDbError::Corrupt {
            message: format!(
                "layered generation removes entity `{id}` but keeps a base relation it anchors"
            ),
        });
    }
    Ok(shadowed)
}

/// What a delta takes away from its base.
struct ShadowedBaseRows {
    sum: GraphRowDigestSum,
    hidden: HiddenRowsV1,
    /// Base rows that still serve.
    base_entities: usize,
    base_relations: usize,
}

/// Removed entities by identity-order ordinal, with the count of base
/// relations they still anchor, which must all leave with them.
type RemovedEntities = BTreeMap<u32, (GraphEntityId, u32)>;

fn hides_more_than_base() -> GraphDbError {
    GraphDbError::Corrupt {
        message: "a layered generation hides more rows than its base holds".to_owned(),
    }
}

fn take_base_row(
    sum: &mut GraphRowDigestSum,
    remaining: &mut usize,
    lanes: RowLanes,
) -> Result<(), GraphDbError> {
    sum.add_row_lanes(lanes);
    *remaining = remaining.checked_sub(1).ok_or_else(hides_more_than_base)?;
    Ok(())
}

impl ShadowedBaseRows {
    fn shadow_entities(
        &mut self,
        index: &RowIndex,
        hidden: &BTreeSet<GraphEntityId>,
        delta: &SpilledGraphGeneration,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<RemovedEntities, GraphDbError> {
        let mut removed = RemovedEntities::new();
        let delta_ids = delta.entity_identities();
        for id in delta_ids.iter().chain(hidden).collect::<BTreeSet<_>>() {
            check()?;
            let indexed = index.entity(id.as_str())?;
            if let Some(indexed) = indexed {
                take_base_row(&mut self.sum, &mut self.base_entities, indexed.lanes)?;
            }
            if !hidden.contains(id) || delta_ids.binary_search(id).is_ok() {
                continue;
            }
            self.hidden.entities.push(id.clone());
            if let Some(indexed) = indexed {
                removed.insert(indexed.ordinal, (id.clone(), indexed.degree));
            }
        }
        Ok(removed)
    }

    fn shadow_relations(
        &mut self,
        index: &RowIndex,
        hidden: &BTreeSet<GraphRelationId>,
        delta: &SpilledGraphGeneration,
        removed: &mut RemovedEntities,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        let delta_ids = delta.relation_identities()?;
        for id in delta_ids.iter().chain(hidden).collect::<BTreeSet<_>>() {
            check()?;
            let indexed = index.relation(id.as_str())?;
            if let Some(indexed) = indexed {
                take_base_row(&mut self.sum, &mut self.base_relations, indexed.lanes)?;
            }
            if !hidden.contains(id) {
                continue;
            }
            // Only a hidden relation leaves its endpoints unanchored; a
            // shadowing delta relation must carry its endpoints itself.
            if let Some(indexed) = indexed {
                release_endpoints(removed, indexed.from, indexed.to)?;
            }
            if delta_ids.binary_search(id).is_err() {
                self.hidden.relations.push(id.clone());
            }
        }
        Ok(())
    }
}

fn release_endpoints(
    removed: &mut RemovedEntities,
    from: u32,
    to: u32,
) -> Result<(), GraphDbError> {
    let ends: &[u32] = if from == to { &[from] } else { &[from, to] };
    for end in ends {
        if let Some((_, degree)) = removed.get_mut(end) {
            *degree = degree.checked_sub(1).ok_or_else(hides_more_than_base)?;
        }
    }
    Ok(())
}

/// A layered generation's rows, ready to seal over its base.
pub struct LayeredGraphGeneration {
    identity: GraphGenerationManifestIdentity,
    delta: SpilledGraphGeneration,
    base: GraphSealedBaseV1,
    hidden: HiddenRowsV1,
    hidden_sum: GraphRowDigestSum,
    entity_count: usize,
    relation_count: usize,
    row_sum: GraphRowDigestSum,
    expected_recovered_digest: GraphRecoveredGenerationDigestV1,
}

impl std::fmt::Debug for LayeredGraphGeneration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LayeredGraphGeneration")
            .field("generation", &self.identity.generation)
            .field("base", &self.base)
            .field("delta", &self.delta.row_counts())
            .field("hidden_entities", &self.hidden.entities.len())
            .field("hidden_relations", &self.hidden.relations.len())
            .finish_non_exhaustive()
    }
}

impl LayeredGraphGeneration {
    #[must_use]
    pub fn identity(&self) -> GraphGenerationManifestIdentity {
        self.identity.clone()
    }

    /// Logical `(entities, relations)`: what a cold build of the same rows
    /// holds.
    #[must_use]
    pub fn row_counts(&self) -> (usize, usize) {
        (self.entity_count, self.relation_count)
    }

    /// Rows the delta container encodes, base endpoint copies included.
    #[must_use]
    pub fn delta_row_counts(&self) -> (usize, usize) {
        self.delta.row_counts()
    }

    #[must_use]
    pub fn expected_recovered_digest(&self) -> &GraphRecoveredGenerationDigestV1 {
        &self.expected_recovered_digest
    }

    pub(crate) fn delta(&self) -> &SpilledGraphGeneration {
        &self.delta
    }

    pub(crate) fn row_sum(&self) -> GraphRowDigestSum {
        self.row_sum
    }

    pub(crate) fn base_receipt(&self) -> SealedBaseReceiptV1 {
        let base = &self.base.inner;
        SealedBaseReceiptV1 {
            generation: base.identity.generation.as_str().to_owned(),
            source_generation: base.identity.source_generation.as_str().to_owned(),
            watermark: base.identity.watermark.as_str().to_owned(),
            physical_namespace: base.physical_namespace.as_str().to_owned(),
            recovered_digest: base.recovered_digest.clone(),
            entities: base.entities,
            relations: base.relations,
            row_sum: base.row_sum.to_hex(),
            hidden_row_sum: self.hidden_sum.to_hex(),
        }
    }

    /// Moves the pinned base links into `staging` and records the hidden
    /// rows, completing a layered artifact directory beside its container.
    pub(crate) fn install_base_files(&self, staging: &Path) -> Result<(), GraphDbError> {
        let pinned = self.delta.directory();
        for file in [
            LAYERED_BASE_CONTAINER_FILE,
            LAYERED_BASE_ATTACHMENT_FILE,
            LAYERED_BASE_ROW_INDEX_FILE,
            LAYERED_BASE_ENTITY_ROWS_FILE,
            LAYERED_BASE_ENTITY_ROW_OFFSETS_FILE,
        ] {
            std::fs::hard_link(pinned.join(file), staging.join(file))
                .map_err(|error| layered_io("base link", error))?;
        }
        let container = Path::new(LAYERED_BASE_CONTAINER_FILE);
        match std::fs::copy(
            pinned.join(marker_path(container)),
            staging.join(marker_path(container)),
        ) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(layered_io("base marker link", error)),
        }
        let encoded = serde_json::to_vec(&self.hidden)
            .map_err(|error| GraphDbError::unavailable(format!("hidden rows encode: {error}")))?;
        std::fs::write(staging.join(LAYERED_HIDDEN_FILE), encoded)
            .map_err(|error| layered_io("hidden rows write", error))
    }
}

/// The read side of an installed layered generation: the base engine, the
/// namespace its rows live under, and what the delta hides or shadows.
pub(crate) struct SealedLayer {
    base: Arc<GraphDb>,
    /// Set once the base container is proven against its receipt's digest,
    /// which happens before the first read of a base row.
    base_proven: OnceLock<()>,
    base_identity: GraphGenerationManifestIdentity,
    base_digest: GraphRecoveredGenerationDigestV1,
    pub(crate) base_receipt: SealedBaseReceiptV1,
    base_namespace: GraphNamespace,
    hidden_entities: HashSet<GraphEntityId>,
    hidden_relations: HashSet<GraphRelationId>,
    delta_entities: HashSet<GraphEntityId>,
    delta_relations: HashSet<GraphRelationId>,
}

impl SealedLayer {
    pub(crate) fn open(
        directory: &Path,
        base: Arc<GraphDb>,
        base_receipt: SealedBaseReceiptV1,
        delta: &GraphDb,
        identity: &GraphGenerationManifestIdentity,
        base_proven: bool,
    ) -> Result<Self, GraphDbError> {
        let base_identity = base_receipt.identity(&identity.projection)?;
        let base_digest =
            GraphRecoveredGenerationDigestV1::new(base_receipt.recovered_digest.clone())
                .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        let hidden: HiddenRowsV1 = serde_json::from_slice(
            &std::fs::read(directory.join(LAYERED_HIDDEN_FILE))
                .map_err(|error| layered_io("hidden rows read", error))?,
        )
        .map_err(|error| GraphDbError::Corrupt {
            message: format!("layered hidden rows are unreadable: {error}"),
        })?;
        let delta_namespace = identity.physical_namespace()?;
        let (delta_entities, delta_relations) = delta.read_intact(&NeverCancelled, |database| {
            let entities = crate::state::projection_entity_nodes_sorted_checked(
                database,
                &delta_namespace,
                &identity.projection.projection,
                &|| Ok(()),
            )?
            .into_iter()
            .map(|(identity, _)| GraphEntityId::new(identity.as_str()))
            .collect::<Result<HashSet<_>, _>>()?;
            let relations = crate::state::projection_relation_nodes_sorted_checked(
                database,
                &delta_namespace,
                &identity.projection.projection,
                &|| Ok(()),
            )?
            .into_iter()
            .map(|(identity, _)| GraphRelationId::new(identity.as_str()))
            .collect::<Result<HashSet<_>, _>>()?;
            Ok((entities, relations))
        })?;
        let proven = OnceLock::new();
        if base_proven {
            proven.get_or_init(|| ());
        }
        Ok(Self {
            base,
            base_proven: proven,
            base_identity,
            base_digest,
            base_namespace: GraphNamespace::new(base_receipt.physical_namespace.clone())?,
            base_receipt,
            hidden_entities: hidden.entities.into_iter().collect(),
            hidden_relations: hidden.relations.into_iter().collect(),
            delta_entities,
            delta_relations,
        })
    }

    /// The base engine for lifecycle work that reads no row: census,
    /// hibernation, close.
    pub(crate) fn base_engine(&self) -> &Arc<GraphDb> {
        &self.base
    }

    /// The base engine for a read, proven first: a layer installed right
    /// after its seal defers the proof to here, so the seal never loads the
    /// base graph, and the carried verify-once marker keeps it cheap.
    pub(crate) fn proven_base(&self) -> Result<&GraphDb, GraphDbError> {
        if self.base_proven.get().is_none() {
            crate::sealed_store::sealed_copy_proof(
                &self.base,
                &self.base_identity,
                &self.base_digest,
                &|| Ok(()),
            )?;
            self.base_proven.get_or_init(|| ());
        }
        Ok(&self.base)
    }

    /// Rows a base read may return that the layer then drops, the headroom a
    /// per-layer budget needs so filtering cannot starve an exact answer.
    fn filtered_relation_bound(&self) -> usize {
        self.hidden_relations
            .len()
            .saturating_add(self.delta_relations.len())
    }

    fn base_relation_visible(&self, identity: &GraphRelationId) -> bool {
        !self.hidden_relations.contains(identity) && !self.delta_relations.contains(identity)
    }

    fn base_entity_visible(&self, identity: &GraphEntityId) -> bool {
        !self.hidden_entities.contains(identity) && !self.delta_entities.contains(identity)
    }
}

/// One layered generation's reads: `delta` is its own container, served
/// under `namespace`.
pub(crate) struct LayeredReads<'a> {
    pub(crate) delta: &'a GraphDb,
    pub(crate) layer: &'a SealedLayer,
    pub(crate) namespace: GraphNamespace,
    pub(crate) projection: &'a GraphProjectionIdentity,
}

/// How a fan-out answers a batch whose rows exceed its budget.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum LayeredOverflow {
    Refuse,
    Truncate,
}

fn read_budget(max: usize) -> GraphDbError {
    GraphDbError::budget_exhausted_count(GraphBudgetKind::Read, max)
}

impl LayeredReads<'_> {
    pub(crate) fn entity(
        &self,
        identity: &GraphEntityId,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<GraphEntity>, GraphDbError> {
        if self.layer.delta_entities.contains(identity) {
            return self.delta.entity(&self.namespace, identity, cancellation);
        }
        if self.layer.hidden_entities.contains(identity) {
            return Ok(None);
        }
        self.layer
            .proven_base()?
            .entity(&self.layer.base_namespace, identity, cancellation)
    }

    pub(crate) fn relation(
        &self,
        identity: &GraphRelationId,
        cancellation: &dyn GraphCancellation,
    ) -> Result<Option<GraphGenerationRelation>, GraphDbError> {
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let (database, namespace) = if self.layer.delta_relations.contains(identity) {
            (self.delta, &self.namespace)
        } else if self.layer.hidden_relations.contains(identity) {
            return Ok(None);
        } else {
            (self.layer.proven_base()?, &self.layer.base_namespace)
        };
        database
            .read_intact(cancellation, |native| {
                load_relation(native, namespace, identity)
            })?
            .map(|stored| generation_relation(self.projection, stored.relation))
            .transpose()
    }

    fn entity_visible(&self, identity: &GraphEntityId) -> Result<bool, GraphDbError> {
        if self.layer.delta_entities.contains(identity) {
            return Ok(true);
        }
        if self.layer.hidden_entities.contains(identity) {
            return Ok(false);
        }
        self.layer
            .proven_base()?
            .read_intact(&NeverCancelled, |native| {
                Ok(load_entity(native, &self.layer.base_namespace, identity)?.is_some())
            })
    }

    fn relation_visible(&self, identity: &GraphRelationId) -> Result<bool, GraphDbError> {
        if self.layer.delta_relations.contains(identity) {
            return Ok(true);
        }
        if self.layer.hidden_relations.contains(identity) {
            return Ok(false);
        }
        self.layer
            .proven_base()?
            .read_intact(&NeverCancelled, |native| {
                Ok(load_relation(native, &self.layer.base_namespace, identity)?.is_some())
            })
    }

    /// The visible identities after `after` in one domain, ascending,
    /// at most `limit`: the delta's merged with the base's that survive.
    fn identity_page(
        &self,
        entities: bool,
        after: Option<&str>,
        limit: usize,
        cancellation: &dyn GraphCancellation,
    ) -> Result<Vec<String>, GraphDbError> {
        let (record_label, identity_property) = if entities {
            (ENTITY_LABEL, ENTITY_ID_PROPERTY)
        } else {
            (RELATION_LABEL, RELATION_ID_PROPERTY)
        };
        let owner = |namespace: &GraphNamespace| {
            if entities {
                entity_projection_label(namespace, &self.projection.projection)
            } else {
                relation_projection_label(namespace, &self.projection.projection)
            }
        };
        let page = |database: &GraphDb, namespace: &GraphNamespace, after: Option<&str>| {
            database.read_intact(cancellation, |native| {
                let owner_label = owner(namespace);
                query_identity_page(
                    database,
                    native,
                    IdentityScope {
                        owner_label: &owner_label,
                        record_label,
                        identity_property,
                    },
                    after,
                    limit,
                    cancellation,
                )
            })
        };
        let delta = page(self.delta, &self.namespace, after)?;
        let mut base = Vec::with_capacity(limit);
        let mut cursor = after.map(str::to_owned);
        loop {
            let fetched = page(
                self.layer.proven_base()?,
                &self.layer.base_namespace,
                cursor.as_deref(),
            )?;
            let exhausted = fetched.len() < limit;
            cursor = fetched.last().cloned();
            base.extend(fetched.into_iter().filter(|identity| {
                if entities {
                    GraphEntityId::new(identity.as_str())
                        .is_ok_and(|identity| self.layer.base_entity_visible(&identity))
                } else {
                    GraphRelationId::new(identity.as_str())
                        .is_ok_and(|identity| self.layer.base_relation_visible(&identity))
                }
            }));
            if exhausted || base.len() >= limit {
                break;
            }
        }
        let mut merged = delta.into_iter().chain(base).collect::<Vec<_>>();
        merged.sort_unstable();
        merged.dedup();
        merged.truncate(limit);
        Ok(merged)
    }

    pub(crate) fn read_projection(
        &self,
        request: GraphProjectionReadRequest,
    ) -> Result<GraphProjectionPage, GraphDbError> {
        crate::projection::check_cancelled(request.cancellation.as_ref())?;
        if request.max_entities == 0 && request.max_relations == 0 {
            return Err(read_budget(
                crate::projection_read::MAX_PROJECTION_PAGE_ITEMS,
            ));
        }
        for limit in [request.max_entities, request.max_relations] {
            if limit > crate::projection_read::MAX_PROJECTION_PAGE_ITEMS {
                return Err(read_budget(
                    crate::projection_read::MAX_PROJECTION_PAGE_ITEMS,
                ));
            }
        }
        let cancellation = request.cancellation.as_ref();
        let mut page = GraphProjectionPage {
            entities: Vec::new(),
            relations: Vec::new(),
            next_entity: None,
            next_relation: None,
        };
        if request.max_entities > 0 {
            if let Some(cursor) = &request.after_entity
                && !self.entity_visible(cursor)?
            {
                return Err(GraphDbError::invalid(
                    "entity cursor does not belong to the requested projection",
                ));
            }
            let identities = self.identity_page(
                true,
                request.after_entity.as_ref().map(GraphEntityId::as_str),
                request.max_entities.saturating_add(1),
                cancellation,
            )?;
            let has_more = identities.len() > request.max_entities;
            for identity in identities.into_iter().take(request.max_entities) {
                crate::projection::check_cancelled(cancellation)?;
                let identity = GraphEntityId::new(identity)?;
                let entity = self
                    .entity(&identity, Arc::clone(&request.cancellation))?
                    .ok_or_else(|| GraphDbError::Corrupt {
                        message: "layered projection page named an unreadable entity".to_owned(),
                    })?;
                page.entities.push(entity);
            }
            page.next_entity = has_more
                .then(|| page.entities.last().map(|entity| entity.identity.clone()))
                .flatten();
        }
        if request.max_relations > 0 {
            if let Some(cursor) = &request.after_relation
                && !self.relation_visible(cursor)?
            {
                return Err(GraphDbError::invalid(
                    "relation cursor does not belong to the requested projection",
                ));
            }
            let identities = self.identity_page(
                false,
                request.after_relation.as_ref().map(GraphRelationId::as_str),
                request.max_relations.saturating_add(1),
                cancellation,
            )?;
            let has_more = identities.len() > request.max_relations;
            for identity in identities.into_iter().take(request.max_relations) {
                crate::projection::check_cancelled(cancellation)?;
                let identity = GraphRelationId::new(identity)?;
                let relation = self.relation(&identity, cancellation)?.ok_or_else(|| {
                    GraphDbError::Corrupt {
                        message: "layered projection page named an unreadable relation".to_owned(),
                    }
                })?;
                page.relations.push(relation.storage_relation()?);
            }
            page.next_relation = has_more
                .then(|| {
                    page.relations
                        .last()
                        .map(|relation| relation.identity.clone())
                })
                .flatten();
        }
        Ok(page)
    }

    pub(crate) fn projection_telemetry(
        &self,
        request: GraphProjectionTelemetryRequest,
        (entities, relations): (usize, usize),
    ) -> Result<Option<GraphProjectionTelemetry>, GraphDbError> {
        crate::projection::check_cancelled(request.cancellation.as_ref())?;
        let Some(projection) = self
            .delta
            .read_intact(request.cancellation.as_ref(), |native| {
                latest_projection(native, &self.namespace, &request.projection)
            })?
        else {
            return Ok(None);
        };
        Ok(Some(GraphProjectionTelemetry {
            source_generation: projection.commit.source_generation,
            watermark: projection.commit.watermark,
            commit_sequence: projection.commit.sequence,
            entity_count: entities as u64,
            relation_count: relations as u64,
        }))
    }

    /// Every visible relation incident to each start in `direction`,
    /// ascending by identity, drawn from both layers with `headroom` rows of
    /// budget beyond what a cold store would read.
    fn directed(
        &self,
        starts: &[GraphEntityId],
        relation_kinds: &BTreeSet<GraphRelationKind>,
        outgoing: bool,
        budget: usize,
        overflow: LayeredOverflow,
        cancellation: &Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<(GraphRelation, bool)>>, GraphDbError> {
        let per_layer = budget.saturating_add(self.layer.filtered_relation_bound());
        let read = |database: &GraphDb, namespace: &GraphNamespace| match (outgoing, overflow) {
            (true, LayeredOverflow::Refuse) => database.outgoing_relations(
                namespace,
                starts,
                relation_kinds,
                per_layer,
                Arc::clone(cancellation),
            ),
            (true, LayeredOverflow::Truncate) => database.outgoing_relations_truncated(
                namespace,
                starts,
                relation_kinds,
                per_layer,
                Arc::clone(cancellation),
            ),
            (false, LayeredOverflow::Refuse) => database.incoming_relations(
                namespace,
                starts,
                relation_kinds,
                per_layer,
                Arc::clone(cancellation),
            ),
            (false, LayeredOverflow::Truncate) => database.incoming_relations_truncated(
                namespace,
                starts,
                relation_kinds,
                per_layer,
                Arc::clone(cancellation),
            ),
        };
        // A layer past its headroom holds more visible rows than the caller's
        // budget, so the refusal is the caller's.
        let refused = |error| match error {
            GraphDbError::BudgetExhausted { .. } => read_budget(budget),
            error => error,
        };
        let delta = read(self.delta, &self.namespace).map_err(refused)?;
        let base = read(self.layer.proven_base()?, &self.layer.base_namespace).map_err(refused)?;
        Ok(delta
            .into_iter()
            .zip(base)
            .map(|(delta, base)| {
                let mut merged = delta
                    .into_iter()
                    .map(|relation| (relation, true))
                    .chain(
                        base.into_iter()
                            .filter(|relation| self.layer.base_relation_visible(&relation.identity))
                            .map(|relation| (relation, false)),
                    )
                    .collect::<Vec<_>>();
                merged.sort_by(|left, right| left.0.identity.cmp(&right.0.identity));
                merged.dedup_by(|left, right| left.0.identity == right.0.identity);
                merged
            })
            .collect())
    }

    /// The batch budget a cold store charges: across every start, refusing
    /// or keeping the prefix and emptying every later start.
    fn apply_budget<T>(
        batches: Vec<Vec<T>>,
        max: usize,
        overflow: LayeredOverflow,
    ) -> Result<Vec<Vec<T>>, GraphDbError> {
        let mut admitted = 0_usize;
        let mut truncated = false;
        let mut results = Vec::with_capacity(batches.len());
        for mut batch in batches {
            if truncated {
                results.push(Vec::new());
                continue;
            }
            let remaining = max.saturating_sub(admitted);
            if batch.len() > remaining {
                match overflow {
                    LayeredOverflow::Refuse => return Err(read_budget(max)),
                    LayeredOverflow::Truncate => {
                        batch.truncate(remaining);
                        truncated = true;
                    }
                }
            }
            admitted += batch.len();
            results.push(batch);
        }
        Ok(results)
    }

    pub(crate) fn relations(
        &self,
        starts: &[GraphEntityId],
        relation_kinds: &BTreeSet<GraphRelationKind>,
        max_relations: usize,
        outgoing: bool,
        overflow: LayeredOverflow,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<GraphRelation>>, GraphDbError> {
        let merged = self.directed(
            starts,
            relation_kinds,
            outgoing,
            max_relations,
            overflow,
            &cancellation,
        )?;
        Self::apply_budget(
            merged
                .into_iter()
                .map(|batch| batch.into_iter().map(|(relation, _)| relation).collect())
                .collect(),
            max_relations,
            overflow,
        )
    }

    /// Relation identity pages: `after` is exclusive and `limit` bounds each
    /// start separately, like a cold store's paged fan-out.
    pub(crate) fn relation_ids_page(
        &self,
        starts: &[GraphEntityId],
        relation_kinds: &BTreeSet<GraphRelationKind>,
        after: Option<&GraphRelationId>,
        limit: usize,
        outgoing: bool,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<GraphRelationId>>, GraphDbError> {
        let per_layer = limit.saturating_add(self.layer.filtered_relation_bound());
        let read = |database: &GraphDb, namespace: &GraphNamespace| {
            if outgoing {
                database.outgoing_relation_ids_page(
                    namespace,
                    starts,
                    relation_kinds,
                    after,
                    per_layer,
                    Arc::clone(&cancellation),
                )
            } else {
                database.incoming_relation_ids_page(
                    namespace,
                    starts,
                    relation_kinds,
                    after,
                    per_layer,
                    Arc::clone(&cancellation),
                )
            }
        };
        let delta = read(self.delta, &self.namespace)?;
        let base = read(self.layer.proven_base()?, &self.layer.base_namespace)?;
        Ok(delta
            .into_iter()
            .zip(base)
            .map(|(delta, base)| {
                let mut merged = delta
                    .into_iter()
                    .chain(
                        base.into_iter()
                            .filter(|identity| self.layer.base_relation_visible(identity)),
                    )
                    .collect::<Vec<_>>();
                merged.sort_unstable();
                merged.dedup();
                merged.truncate(limit);
                merged
            })
            .collect())
    }

    pub(crate) fn relation_ids(
        &self,
        starts: &[GraphEntityId],
        relation_kinds: &BTreeSet<GraphRelationKind>,
        max_relations: usize,
        outgoing: bool,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<GraphRelationId>>, GraphDbError> {
        Ok(self
            .relations(
                starts,
                relation_kinds,
                max_relations,
                outgoing,
                LayeredOverflow::Refuse,
                cancellation,
            )?
            .into_iter()
            .map(|batch| {
                batch
                    .into_iter()
                    .map(|relation| relation.identity)
                    .collect()
            })
            .collect())
    }

    /// A relation's far endpoint as this generation serves it.
    fn target(
        &self,
        relation: &GraphRelation,
        cancellation: &Arc<dyn GraphCancellation>,
    ) -> Result<GraphEntity, GraphDbError> {
        self.entity(&relation.to, Arc::clone(cancellation))?
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "layered relation reaches an entity the generation does not serve"
                    .to_owned(),
            })
    }

    pub(crate) fn relation_targets(
        &self,
        starts: &[GraphEntityId],
        relation_kinds: &BTreeSet<GraphRelationKind>,
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<GraphRelationTarget>>, GraphDbError> {
        let relations = self.relations(
            starts,
            relation_kinds,
            max_relations,
            true,
            LayeredOverflow::Refuse,
            Arc::clone(&cancellation),
        )?;
        relations
            .into_iter()
            .map(|batch| {
                batch
                    .into_iter()
                    .map(|relation| {
                        Ok(GraphRelationTarget {
                            target: self.target(&relation, &cancellation)?,
                            relation,
                        })
                    })
                    .collect()
            })
            .collect()
    }

    pub(crate) fn visit_relation_targets(
        &self,
        start: &GraphEntityId,
        relation_kinds: &BTreeSet<GraphRelationKind>,
        cancellation: Arc<dyn GraphCancellation>,
        visitor: &mut dyn FnMut(GraphRelationTarget),
    ) -> Result<usize, GraphDbError> {
        let merged = self.directed(
            std::slice::from_ref(start),
            relation_kinds,
            true,
            usize::MAX,
            LayeredOverflow::Refuse,
            &cancellation,
        )?;
        let mut visited = 0_usize;
        for (relation, _) in merged.into_iter().flatten() {
            if cancellation.is_cancelled() {
                return Err(GraphDbError::Cancelled);
            }
            let target = self.target(&relation, &cancellation)?;
            visitor(GraphRelationTarget { relation, target });
            visited += 1;
        }
        Ok(visited)
    }

    /// Breadth-first traversal with the verified traversal's contract: each
    /// node's adjacency ordered by relation then neighbor identity, a start
    /// that must exist, and the same visit, result, and depth budgets.
    pub(crate) fn traverse(
        &self,
        request: TraversalRequest,
    ) -> Result<VerifiedTraversalResult, GraphDbError> {
        if request.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        if request.max_visits == 0 {
            return Err(read_budget(request.max_visits));
        }
        if request.max_results == 0 {
            return Ok(VerifiedTraversalResult { visits: Vec::new() });
        }
        if !self.entity_visible(&request.start)? {
            return Err(GraphDbError::invalid(
                "traversal start entity does not exist",
            ));
        }
        let directions: &[bool] = match request.direction {
            GraphTraversalDirection::Outgoing => &[true],
            GraphTraversalDirection::Incoming => &[false],
            GraphTraversalDirection::Both => &[true, false],
        };
        let mut queue = VecDeque::from([(request.start.clone(), 0_usize, None)]);
        let mut discovered = HashSet::from([request.start.clone()]);
        let mut visits = Vec::new();
        while let Some((entity, depth, via_relation)) = queue.pop_front() {
            if request.cancellation.is_cancelled() {
                return Err(GraphDbError::Cancelled);
            }
            if visits.len() >= request.max_visits {
                return Err(read_budget(request.max_visits));
            }
            visits.push(VerifiedTraversalVisit {
                entity: GraphEntityRef::new(self.projection.clone(), entity.clone()),
                depth,
                via_relation,
            });
            if visits.len() >= request.max_results || depth >= request.max_depth {
                continue;
            }
            let mut adjacent = Vec::new();
            for outgoing in directions {
                let batches = self.directed(
                    std::slice::from_ref(&entity),
                    &request.relation_kinds,
                    *outgoing,
                    usize::MAX,
                    LayeredOverflow::Refuse,
                    &request.cancellation,
                )?;
                for (relation, _) in batches.into_iter().flatten() {
                    let neighbor = if *outgoing {
                        relation.to
                    } else {
                        relation.from
                    };
                    adjacent.push((
                        GraphRelationRef::new(self.projection.clone(), relation.identity),
                        GraphEntityRef::new(self.projection.clone(), neighbor),
                    ));
                }
            }
            adjacent.sort();
            adjacent.dedup();
            for (relation, neighbor) in adjacent {
                if discovered.insert(neighbor.identity.clone()) {
                    let next_depth = depth
                        .checked_add(1)
                        .ok_or_else(|| read_budget(request.max_depth))?;
                    queue.push_back((neighbor.identity, next_depth, Some(relation)));
                }
            }
        }
        Ok(VerifiedTraversalResult { visits })
    }
}

/// A layered generation's hard-linked base container.
pub(crate) fn base_database_path(directory: &Path) -> PathBuf {
    directory.join(LAYERED_BASE_CONTAINER_FILE)
}
