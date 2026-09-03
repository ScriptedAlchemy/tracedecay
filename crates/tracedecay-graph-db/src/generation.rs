use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::io::{self, Write};
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

use grafeo_engine::GrafeoDB;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_text::{encode_lowercase_hex, encode_tagged_lowercase_hex};
use tracedecay_store::runtime::{
    GraphDependencyGenerationClosureDigestV1, GraphDependencyGenerationIdentityV1,
    GraphGenerationIdV1, GraphNamespaceV1, GraphProjectionIdV1, GraphProjectionIdentityV1,
    GraphPublicationIdempotencyKeyV1, GraphPublicationInputDigestV1, GraphPublicationKeyV1,
    GraphPublicationReplayV1, GraphRecoveredGenerationDigestV1, GraphVerifiedHeadV1,
    MAX_GRAPH_REPLAY_SOURCE_BYTES_V1, StoreShardIdV1,
};

use crate::limits::{MAX_VERIFIED_GENERATION_ENTITIES, MAX_VERIFIED_GENERATION_RELATIONS};
use crate::schema::{NAMESPACE_PROPERTY, required_string};
use crate::state::latest_projection;
use crate::{
    GraphBudgetKind, GraphDbError, GraphEntity, GraphEntityId, GraphGenerationId,
    GraphIdempotencyKey, GraphNamespace, GraphProjectionId, GraphProperty, GraphPropertyName,
    GraphRelation, GraphRelationId, GraphRelationKind, GraphWatermark, SourceGeneration,
};

pub(super) const DIGEST_CHECK_INTERVAL_BYTES: u64 = 64 * 1024;
const CHECKED_VEC_INITIAL_CAPACITY_BYTES: usize = 1_024;
const MANIFEST_DIGEST_CHUNK_ROWS: usize = 512;
const MANIFEST_DIGEST_WORKER_BYTES: usize = 8 * 1024 * 1024;
const MANIFEST_DIGEST_MAX_IN_FLIGHT_BYTES: usize = 64 * 1024 * 1024;
const MANIFEST_DIGEST_MAX_WORKERS: usize = 8;

#[path = "generation/identity.rs"]
mod identity;
#[path = "generation/recovered.rs"]
mod recovered;
#[path = "generation/replay.rs"]
mod replay;
pub use identity::{
    GraphEntityRef, GraphGenerationDependency, GraphProjectionIdentity, GraphRelationRef,
};
#[cfg(test)]
pub(crate) use recovered::recovered_generation_digest_chunked;
pub(crate) use recovered::recovered_generation_digest_from_database;
pub(crate) use replay::InlineOnlyGraphGenerationManifestProvider;
use replay::validate_sealed_replay;
pub use replay::{
    GraphGenerationManifestProvider, GraphGenerationReplayMetadata, GraphGenerationReplaySource,
    GraphProjectorRevision, SealedCodeGenerationReplay, SealedGraphStateDigest,
    SemanticVectorGenerationReplay,
};
pub(crate) use replay::{
    checked_decode_replay_source, metadata_manifest_from_replay, validate_metadata_binding,
    validate_supplied_manifest_binding,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphGenerationRelation {
    pub identity: GraphRelationId,
    pub from: GraphEntityRef,
    pub to: GraphEntityRef,
    pub kind: GraphRelationKind,
    pub properties: BTreeMap<GraphPropertyName, GraphProperty>,
}

impl GraphGenerationRelation {
    pub fn new(
        identity: GraphRelationId,
        from: GraphEntityRef,
        to: GraphEntityRef,
        kind: GraphRelationKind,
        properties: BTreeMap<GraphPropertyName, GraphProperty>,
    ) -> Result<Self, GraphDbError> {
        let relation = Self {
            identity,
            from,
            to,
            kind,
            properties,
        };
        relation.validate()?;
        Ok(relation)
    }

    fn validate(&self) -> Result<(), GraphDbError> {
        GraphRelation::new(
            self.identity.clone(),
            self.from.identity.clone(),
            self.to.identity.clone(),
            self.kind.clone(),
            self.properties.clone(),
        )
        .map(|_| ())
    }

    pub(crate) fn storage_relation(&self) -> Result<GraphRelation, GraphDbError> {
        GraphRelation::new(
            self.identity.clone(),
            self.from.identity.clone(),
            self.to.identity.clone(),
            self.kind.clone(),
            self.properties.clone(),
        )
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphGenerationManifest {
    pub projection: GraphProjectionIdentity,
    pub generation: GraphGenerationId,
    pub source_generation: SourceGeneration,
    pub watermark: GraphWatermark,
    pub dependencies: Vec<GraphGenerationDependency>,
    pub entities: Vec<GraphEntity>,
    pub relations: Vec<GraphGenerationRelation>,
    /// Memoized canonical digests of this instance. Never serialized — the
    /// canonical replay payload and every digest byte are unchanged — and
    /// invisible to equality; re-validated against the fields on every read.
    #[serde(skip)]
    digest_memo: ManifestDigestMemo,
}

/// The small, cheaply cloned metadata half of a generation manifest: exactly
/// the fields that name a generation and bind it to its dependency closure.
///
/// Every stage after the staged rows are durable — the close/reopen
/// recovered-digest proof, quarantine, lease seating, and the finalization
/// receipt — reads only these fields. Carrying them separately lets the bulk
/// `entities`/`relations` vectors (multiple gigabytes on a first index) be
/// released the moment the last staging page commits, instead of staying live
/// through reopen and verification alongside the rebuilt in-RAM store.
///
/// This type is deliberately not serialized: the canonical replay payload and
/// every pinned digest are still produced from the flat
/// [`GraphGenerationManifest`] shape, so its byte layout is unchanged.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphGenerationManifestIdentity {
    pub projection: GraphProjectionIdentity,
    pub generation: GraphGenerationId,
    pub source_generation: SourceGeneration,
    pub watermark: GraphWatermark,
    pub dependencies: Vec<GraphGenerationDependency>,
    /// Memoized dependency-closure digest, seeded by
    /// [`GraphGenerationManifest::identity`] when the manifest already proved
    /// it. Re-validated against `dependencies` on every read and invisible to
    /// equality and clones.
    digest_memo: DependencyClosureDigestMemo,
}

impl GraphGenerationManifestIdentity {
    /// An identity reconstructed from its metadata parts, with a cold
    /// dependency-closure memo: the digest is recomputed on first use exactly
    /// as a cloned identity would.
    pub fn new(
        projection: GraphProjectionIdentity,
        generation: GraphGenerationId,
        source_generation: SourceGeneration,
        watermark: GraphWatermark,
        dependencies: Vec<GraphGenerationDependency>,
    ) -> Self {
        Self {
            projection,
            generation,
            source_generation,
            watermark,
            dependencies,
            digest_memo: DependencyClosureDigestMemo::default(),
        }
    }

    pub fn dependency_closure_digest(
        &self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphDependencyGenerationClosureDigestV1, GraphDbError> {
        self.digest_memo.digest(&self.dependencies, check)
    }

    pub(crate) fn physical_namespace(&self) -> Result<GraphNamespace, GraphDbError> {
        physical_namespace(
            &self.projection.namespace,
            &self.projection.projection,
            &self.generation,
        )
    }
}

/// Memoized dependency-closure digest, re-validated on every read.
///
/// The digest is a pure function of a `dependencies` vector, so the memo
/// stores the exact vector it hashed and serves the memoized digest only
/// while the owner's dependencies still compare equal to it. A caller that
/// mutates the owning collection after a read therefore always observes a
/// freshly computed digest; the pinned key merely stops memoizing from that
/// point on. Clones start cold, so memoized digests never cross instances
/// except through [`Self::propagated`], which re-validates the binding first.
#[derive(Default)]
struct DependencyClosureDigestMemo {
    slot: OnceLock<(
        Vec<GraphGenerationDependency>,
        GraphDependencyGenerationClosureDigestV1,
    )>,
}

impl DependencyClosureDigestMemo {
    fn digest(
        &self,
        dependencies: &[GraphGenerationDependency],
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphDependencyGenerationClosureDigestV1, GraphDbError> {
        if let Some((memoized_for, digest)) = self.slot.get() {
            if memoized_for.as_slice() == dependencies {
                return Ok(digest.clone());
            }
            return dependency_closure_digest(dependencies, check);
        }
        let digest = dependency_closure_digest(dependencies, check)?;
        // A lost seeding race means another reader memoized the same value.
        let _ = self.slot.set((dependencies.to_vec(), digest.clone()));
        Ok(digest)
    }

    /// A memo for a collection cloned from `dependencies`, carrying the
    /// already-computed digest forward only when it still binds.
    fn propagated(&self, dependencies: &[GraphGenerationDependency]) -> Self {
        let memo = Self::default();
        if let Some((memoized_for, digest)) = self.slot.get()
            && memoized_for.as_slice() == dependencies
        {
            let _ = memo.slot.set((memoized_for.clone(), digest.clone()));
        }
        memo
    }
}

impl Clone for DependencyClosureDigestMemo {
    // A clone starts cold: the source instance can be mutated independently
    // of the clone afterwards, so memoized digests only cross instances
    // through validated propagation.
    fn clone(&self) -> Self {
        Self::default()
    }
}

// Memoization state is invisible to value equality: two identical manifests
// or identities compare equal whether or not their digests were computed yet.
impl PartialEq for DependencyClosureDigestMemo {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for DependencyClosureDigestMemo {}

impl fmt::Debug for DependencyClosureDigestMemo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DependencyClosureDigestMemo")
    }
}

/// Memoized canonical digests of one manifest instance.
///
/// `expected_recovered` pins the metadata half and the row counts it hashed
/// and is served only while those still bind the manifest, which observes
/// every mutation pattern the repository exercises (dependency, generation,
/// source, watermark, and row-set size changes). The bulk rows are validated
/// by count only: replacing a row in place on the same instance after a
/// digest read would go unobserved, and no flow does that — production
/// manifests are constructed, proven, and then held behind `Arc`, while
/// fixtures mutate freshly constructed or freshly cloned (cold) instances
/// before their first digest read.
#[derive(Default)]
struct ManifestDigestMemo {
    dependency_closure: DependencyClosureDigestMemo,
    expected_recovered: OnceLock<RecoveredGenerationDigestMemo>,
}

impl Clone for ManifestDigestMemo {
    // Cold for the same reason as `DependencyClosureDigestMemo::clone`.
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for ManifestDigestMemo {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl fmt::Debug for ManifestDigestMemo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ManifestDigestMemo")
    }
}

struct RecoveredGenerationDigestMemo {
    identity: GraphGenerationManifestIdentity,
    entity_count: usize,
    relation_count: usize,
    digest: GraphRecoveredGenerationDigestV1,
}

impl RecoveredGenerationDigestMemo {
    /// Whether the memoized digest still binds `manifest` exactly.
    fn binds(&self, manifest: &GraphGenerationManifest) -> bool {
        self.identity.projection == manifest.projection
            && self.identity.generation == manifest.generation
            && self.identity.source_generation == manifest.source_generation
            && self.identity.watermark == manifest.watermark
            && self.identity.dependencies == manifest.dependencies
            && self.entity_count == manifest.entities.len()
            && self.relation_count == manifest.relations.len()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum GraphReplayCollectionOutcome {
    Retired(Box<GraphGenerationReplaySource>),
    Retained,
    Absent,
}

impl GraphGenerationManifest {
    pub fn new(
        projection: GraphProjectionIdentity,
        generation: GraphGenerationId,
        source_generation: SourceGeneration,
        watermark: GraphWatermark,
        dependencies: Vec<GraphGenerationDependency>,
        entities: Vec<GraphEntity>,
        relations: Vec<GraphGenerationRelation>,
    ) -> Result<Self, GraphDbError> {
        Self::new_checked(
            projection,
            generation,
            source_generation,
            watermark,
            dependencies,
            entities,
            relations,
            &|| Ok(()),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_checked(
        projection: GraphProjectionIdentity,
        generation: GraphGenerationId,
        source_generation: SourceGeneration,
        watermark: GraphWatermark,
        dependencies: Vec<GraphGenerationDependency>,
        entities: Vec<GraphEntity>,
        relations: Vec<GraphGenerationRelation>,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Self, GraphDbError> {
        check()?;
        if entities.len() > MAX_VERIFIED_GENERATION_ENTITIES {
            return Err(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Capacity,
                MAX_VERIFIED_GENERATION_ENTITIES,
            ));
        }
        if relations.len() > MAX_VERIFIED_GENERATION_RELATIONS {
            return Err(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Capacity,
                MAX_VERIFIED_GENERATION_RELATIONS,
            ));
        }
        let dependencies = checked_sorted_dependencies(dependencies, check)?;
        let entities = checked_sorted_entities(entities, check)?;
        let relations = checked_sorted_relations(relations, check)?;
        let manifest = Self {
            projection,
            generation,
            source_generation,
            watermark,
            dependencies,
            entities,
            relations,
            digest_memo: ManifestDigestMemo::default(),
        };
        manifest.validate_checked(check)?;
        Ok(manifest)
    }

    #[hotpath::measure(
        label = "graph_db.generation.replay.hydrate",
        impl_type = "GraphGenerationManifest"
    )]
    pub fn from_replay(
        publication: &GraphPublicationReplayV1,
        provider: &dyn GraphGenerationManifestProvider,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Self, GraphDbError> {
        check()?;
        publication
            .validate()
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let source = checked_decode_replay_source(&publication.canonical_replay_source, check)?;
        let manifest = match source {
            GraphGenerationReplaySource::InlineManifest(manifest) => *manifest,
            GraphGenerationReplaySource::MetadataOnlyManifest(_)
            | GraphGenerationReplaySource::SemanticVectorGeneration(_) => {
                return Err(GraphDbError::unavailable(
                    "metadata-only replay requires verified native generation rows",
                ));
            }
            GraphGenerationReplaySource::SealedCodeGeneration(source) => {
                validate_sealed_replay(&source)?;
                provider.hydrate_sealed_code_generation(
                    &publication.key.projection,
                    &source,
                    check,
                )?
            }
        };
        manifest.validate_checked(check)?;
        let projection = &publication.key.projection;
        if projection.namespace.as_str() != manifest.projection.namespace.as_str()
            || projection.projection.as_str() != manifest.projection.projection.as_str()
            || publication.key.generation.as_str() != manifest.generation.as_str()
        {
            return Err(GraphDbError::invalid(
                "canonical graph replay identity does not match its relational key",
            ));
        }
        if publication.direct_dependency_generations
            != manifest.relational_dependency_generations(&projection.shard_id)?
        {
            return Err(GraphDbError::conflict("generation.from_replay"));
        }
        if publication.dependency_generation_closure_digest.as_str()
            != manifest.dependency_closure_digest(check)?.as_str()
            || publication.expected_recovered_digest.as_str()
                != manifest.expected_recovered_digest(check)?.as_str()
        {
            return Err(GraphDbError::conflict("generation.from_replay"));
        }
        check()?;
        crate::hotpath_observe::record_counts(
            manifest.entities.len(),
            manifest.relations.len(),
            1,
            0,
        );
        crate::hotpath_observe::record_hydration_source(
            crate::hotpath_observe::HydrationSource::Replay,
        );
        Ok(manifest)
    }

    pub fn from_inline_replay(
        publication: &GraphPublicationReplayV1,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Self, GraphDbError> {
        Self::from_replay(
            publication,
            &InlineOnlyGraphGenerationManifestProvider,
            check,
        )
    }

    pub fn canonical_replay_source(
        &self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Vec<u8>, GraphDbError> {
        self.validate_checked(check)?;
        // Serialize the same externally-tagged `inline_manifest` shape as
        // `GraphGenerationReplaySource::InlineManifest` without cloning the
        // entity/relation payload into an owned enum just to write it.
        #[derive(Serialize)]
        #[serde(rename_all = "snake_case")]
        enum InlineReplaySourceView<'a> {
            InlineManifest(&'a GraphGenerationManifest),
        }
        checked_canonical_bytes(
            &InlineReplaySourceView::InlineManifest(self),
            check,
            "canonical graph generation replay",
            MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
        )
    }

    pub fn sealed_replay_payload(
        &self,
        source: SealedCodeGenerationReplay,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Vec<u8>, GraphDbError> {
        self.validate_checked(check)?;
        validate_sealed_replay(&source)?;
        self.replay_source_payload(
            GraphGenerationReplaySource::SealedCodeGeneration(source),
            check,
        )
    }

    fn replay_source_payload(
        &self,
        source: GraphGenerationReplaySource,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Vec<u8>, GraphDbError> {
        checked_canonical_bytes(
            &source,
            check,
            "canonical graph generation replay",
            MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
        )
    }

    pub fn dependency_closure_digest(
        &self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphDependencyGenerationClosureDigestV1, GraphDbError> {
        self.digest_memo
            .dependency_closure
            .digest(&self.dependencies, check)
    }

    /// The metadata half of this manifest, cloned away from its bulk rows.
    /// Carries the memoized dependency-closure digest along when it still
    /// binds, so later phases that hold only the identity — staging, the
    /// close/reopen recovered-digest proof, recovery — do not recompute a
    /// digest this manifest already proved.
    #[must_use]
    pub fn identity(&self) -> GraphGenerationManifestIdentity {
        GraphGenerationManifestIdentity {
            projection: self.projection.clone(),
            generation: self.generation.clone(),
            source_generation: self.source_generation.clone(),
            watermark: self.watermark.clone(),
            dependencies: self.dependencies.clone(),
            digest_memo: self
                .digest_memo
                .dependency_closure
                .propagated(&self.dependencies),
        }
    }

    /// `(entities, relations)` row counts, for observability that must not
    /// keep the rows themselves alive.
    #[must_use]
    pub fn row_counts(&self) -> (usize, usize) {
        (self.entities.len(), self.relations.len())
    }

    pub fn expected_recovered_digest(
        &self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphRecoveredGenerationDigestV1, GraphDbError> {
        if let Some(memo) = self.digest_memo.expected_recovered.get() {
            if memo.binds(self) {
                return Ok(memo.digest.clone());
            }
            // Mutated after seeding: the pinned key is stale, so every read
            // recomputes from the current fields.
            return self.compute_expected_recovered_digest(check);
        }
        let digest = self.compute_expected_recovered_digest(check)?;
        // A lost seeding race means another reader memoized the same value.
        let _ = self
            .digest_memo
            .expected_recovered
            .set(RecoveredGenerationDigestMemo {
                identity: self.identity(),
                entity_count: self.entities.len(),
                relation_count: self.relations.len(),
                digest: digest.clone(),
            });
        Ok(digest)
    }

    fn compute_expected_recovered_digest(
        &self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphRecoveredGenerationDigestV1, GraphDbError> {
        let digest = recovered_generation_digest(self, check)?;
        GraphRecoveredGenerationDigestV1::new(format!("sha256:{digest}"))
            .map_err(|error| GraphDbError::invalid(error.to_string()))
    }

    pub fn relational_replay(
        &self,
        shard_id: StoreShardIdV1,
        idempotency_key: GraphIdempotencyKey,
        input_digest: GraphPublicationInputDigestV1,
        expected_prior_head: Option<GraphVerifiedHeadV1>,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphPublicationReplayV1, GraphDbError> {
        let payload = self.canonical_replay_source(check)?;
        self.relational_replay_with_payload(
            shard_id,
            idempotency_key,
            input_digest,
            expected_prior_head,
            payload,
            check,
        )
    }

    pub fn relational_sealed_replay(
        &self,
        shard_id: StoreShardIdV1,
        idempotency_key: GraphIdempotencyKey,
        input_digest: GraphPublicationInputDigestV1,
        expected_prior_head: Option<GraphVerifiedHeadV1>,
        source: SealedCodeGenerationReplay,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphPublicationReplayV1, GraphDbError> {
        let payload = self.sealed_replay_payload(source, check)?;
        self.relational_replay_with_payload(
            shard_id,
            idempotency_key,
            input_digest,
            expected_prior_head,
            payload,
            check,
        )
    }

    fn relational_replay_with_payload(
        &self,
        shard_id: StoreShardIdV1,
        idempotency_key: GraphIdempotencyKey,
        input_digest: GraphPublicationInputDigestV1,
        expected_prior_head: Option<GraphVerifiedHeadV1>,
        payload: Vec<u8>,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphPublicationReplayV1, GraphDbError> {
        check()?;
        let projection = GraphProjectionIdentityV1 {
            shard_id,
            namespace: GraphNamespaceV1::new(self.projection.namespace.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            projection: GraphProjectionIdV1::new(self.projection.projection.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        };
        let direct_dependencies = self.relational_dependency_generations(&projection.shard_id)?;
        let key = GraphPublicationKeyV1::new(
            projection,
            GraphGenerationIdV1::new(self.generation.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            GraphPublicationIdempotencyKeyV1::new(idempotency_key.as_str())
                .map_err(|error| GraphDbError::invalid(error.to_string()))?,
        );
        GraphPublicationReplayV1::new(
            key,
            input_digest,
            self.dependency_closure_digest(check)?,
            direct_dependencies,
            expected_prior_head,
            self.expected_recovered_digest(check)?,
            payload,
        )
        .map_err(|error| GraphDbError::invalid(error.to_string()))
    }

    fn relational_dependency_generations(
        &self,
        shard_id: &StoreShardIdV1,
    ) -> Result<Vec<GraphDependencyGenerationIdentityV1>, GraphDbError> {
        relational_dependency_generations(&self.dependencies, shard_id)
    }

    pub(crate) fn validate_checked(
        &self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        check()?;
        if self.entities.len() > MAX_VERIFIED_GENERATION_ENTITIES {
            return Err(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Capacity,
                MAX_VERIFIED_GENERATION_ENTITIES,
            ));
        }
        if self.relations.len() > MAX_VERIFIED_GENERATION_RELATIONS {
            return Err(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Capacity,
                MAX_VERIFIED_GENERATION_RELATIONS,
            ));
        }
        let mut dependency_projections = BTreeSet::new();
        for dependency in &self.dependencies {
            check()?;
            if dependency.projection == self.projection {
                return Err(GraphDbError::invalid(
                    "a graph generation cannot depend on its own projection",
                ));
            }
            if !dependency_projections.insert(dependency.projection.clone()) {
                return Err(GraphDbError::invalid(
                    "a graph generation repeats a dependency projection",
                ));
            }
        }
        // The manifest owns every identity already. Keep only borrowed keys
        // while validating uniqueness and local relation endpoints instead
        // of cloning millions of identifier strings into a second owner.
        let mut entity_ids = HashSet::with_capacity(self.entities.len());
        for entity in &self.entities {
            check()?;
            entity.validate()?;
            if !entity_ids.insert(&entity.identity) {
                return Err(GraphDbError::invalid(
                    "a graph generation repeats an entity identity",
                ));
            }
        }
        let allowed_projections = self
            .dependencies
            .iter()
            .map(|dependency| dependency.projection.clone())
            .chain(std::iter::once(self.projection.clone()))
            .collect::<BTreeSet<_>>();
        let mut relation_ids = HashSet::with_capacity(self.relations.len());
        for relation in &self.relations {
            check()?;
            relation.validate()?;
            if !relation_ids.insert(&relation.identity) {
                return Err(GraphDbError::invalid(
                    "a graph generation repeats a relation identity",
                ));
            }
            for endpoint in [&relation.from, &relation.to] {
                if !allowed_projections.contains(&endpoint.projection) {
                    return Err(GraphDbError::invalid(format!(
                        "relation endpoint projection `{}` is not the candidate or an exact dependency",
                        endpoint.projection
                    )));
                }
                if endpoint.projection == self.projection
                    && !entity_ids.contains(&endpoint.identity)
                {
                    return Err(GraphDbError::invalid(format!(
                        "local relation endpoint `{}` is absent from the candidate generation",
                        endpoint.identity
                    )));
                }
            }
        }
        check()?;
        Ok(())
    }
}

fn checked_sorted_dependencies(
    mut dependencies: Vec<GraphGenerationDependency>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<GraphGenerationDependency>, GraphDbError> {
    check()?;
    dependencies.sort_unstable();
    for pair in dependencies.windows(2) {
        check()?;
        if pair[0] == pair[1] {
            return Err(GraphDbError::invalid(
                "a graph generation repeats a dependency",
            ));
        }
    }
    check()?;
    Ok(dependencies)
}

fn checked_sorted_entities(
    mut entities: Vec<GraphEntity>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<GraphEntity>, GraphDbError> {
    check()?;
    entities.sort_unstable_by(|left, right| left.identity.cmp(&right.identity));
    for pair in entities.windows(2) {
        check()?;
        if pair[0].identity == pair[1].identity {
            return Err(GraphDbError::invalid(
                "a graph generation repeats an entity identity",
            ));
        }
    }
    check()?;
    Ok(entities)
}

fn checked_sorted_relations(
    mut relations: Vec<GraphGenerationRelation>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<GraphGenerationRelation>, GraphDbError> {
    check()?;
    relations.sort_unstable_by(|left, right| left.identity.cmp(&right.identity));
    for pair in relations.windows(2) {
        check()?;
        if pair[0].identity == pair[1].identity {
            return Err(GraphDbError::invalid(
                "a graph generation repeats a relation identity",
            ));
        }
    }
    check()?;
    Ok(relations)
}

/// The dependency-closure digest, shared by the full manifest and its
/// identity so both hash the exact same `dependencies` encoding.
fn dependency_closure_digest(
    dependencies: &[GraphGenerationDependency],
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphDependencyGenerationClosureDigestV1, GraphDbError> {
    #[cfg(test)]
    DEPENDENCY_CLOSURE_CANONICALIZATIONS.with(|count| count.set(count.get() + 1));
    let mut digest = Sha256::new();
    let mut writer = CheckedDigestWriter::new(&mut digest, check);
    let encoded = serde_json::to_writer(&mut writer, dependencies);
    writer.finish()?;
    encoded.map_err(|error| {
        GraphDbError::invalid(format!(
            "failed to encode graph dependency generation closure: {error}"
        ))
    })?;
    GraphDependencyGenerationClosureDigestV1::new(encode_tagged_lowercase_hex(
        "sha256:",
        &digest.finalize(),
    ))
    .map_err(|error| GraphDbError::invalid(error.to_string()))
}

fn relational_dependency_generations(
    dependencies: &[GraphGenerationDependency],
    shard_id: &StoreShardIdV1,
) -> Result<Vec<GraphDependencyGenerationIdentityV1>, GraphDbError> {
    dependencies
        .iter()
        .map(|dependency| {
            Ok(GraphDependencyGenerationIdentityV1::new(
                GraphProjectionIdentityV1 {
                    shard_id: shard_id.clone(),
                    namespace: GraphNamespaceV1::new(dependency.projection.namespace.as_str())
                        .map_err(|error| GraphDbError::invalid(error.to_string()))?,
                    projection: GraphProjectionIdV1::new(dependency.projection.projection.as_str())
                        .map_err(|error| GraphDbError::invalid(error.to_string()))?,
                },
                GraphGenerationIdV1::new(dependency.generation.as_str())
                    .map_err(|error| GraphDbError::invalid(error.to_string()))?,
            ))
        })
        .collect()
}

pub(crate) fn physical_namespace(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    generation: &GraphGenerationId,
) -> Result<GraphNamespace, GraphDbError> {
    let encoded = serde_json::to_vec(&(namespace, projection, generation)).map_err(|error| {
        GraphDbError::invalid(format!(
            "failed to encode physical graph generation identity: {error}"
        ))
    })?;
    GraphNamespace::new(format!(
        "generation:{}",
        hex::encode(Sha256::digest(encoded))
    ))
}

pub(crate) fn is_physical_generation_namespace(namespace: &GraphNamespace) -> bool {
    let Some(digest) = namespace.as_str().strip_prefix("generation:") else {
        return false;
    };
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Streams the stored rows of `identity`'s generation through the
/// recovered-digest proof and compares against `expected`, the digest bound
/// to this exact manifest (a journaled publication's
/// `expected_recovered_digest`, a verified head's digest, or a digest the
/// caller computed from the manifest once). Verification never
/// re-canonicalizes the manifest itself, and reads no manifest row: the
/// expected rows come from the database, so the caller may already have
/// released the bulk `entities`/`relations` vectors.
///
/// This is the *full* proof, and it costs a whole generation's row decode and
/// canonicalization every time it runs. Callers on an activation path should
/// reach it through [`crate::runtime::GraphDb::verify_activated_generation`],
/// which consults a verified-generation marker first and only falls through to
/// here when the container's bytes are not the ones already proven. See
/// `crate::verified_marker` for what that marker does and does not assert.
///
/// Returns the proven digest and the number of canonical bytes it hashed.
#[hotpath::measure(label = "graph_db.generation.recover.verify")]
pub(crate) fn verify_recovered_generation(
    database: &GrafeoDB,
    identity: &GraphGenerationManifestIdentity,
    expected: &GraphRecoveredGenerationDigestV1,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(GraphRecoveredGenerationDigestV1, u64), GraphDbError> {
    #[cfg(test)]
    RECOVERED_GENERATION_ENUMERATIONS.with(|count| count.set(count.get() + 1));
    verify_recovered_rows(database, identity, expected, check)
}

/// The same proof run over a **sealed per-generation copy** rather than the
/// staging database (`crate::sealed_store`). Kept apart so the staging
/// enumeration count the publication tests pin ("stream the proof exactly
/// once") stays a statement about the authority's rows; the sealed copy pays
/// its own post-reopen proof, counted separately.
#[hotpath::measure(label = "graph_db.sealed_store.verify")]
pub(crate) fn verify_sealed_copy_generation(
    database: &GrafeoDB,
    identity: &GraphGenerationManifestIdentity,
    expected: &GraphRecoveredGenerationDigestV1,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(GraphRecoveredGenerationDigestV1, u64), GraphDbError> {
    #[cfg(test)]
    SEALED_COPY_PROOFS.with(|count| count.set(count.get() + 1));
    // The canonical byte count is the same stream the staging proof would
    // have hashed (the digests match byte for byte), so a sealed *build* —
    // which enumerated the staging database's rows to produce this copy —
    // may file it with the publication's verify-once marker.
    verify_recovered_rows(database, identity, expected, check)
}

fn verify_recovered_rows(
    database: &GrafeoDB,
    identity: &GraphGenerationManifestIdentity,
    expected: &GraphRecoveredGenerationDigestV1,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(GraphRecoveredGenerationDigestV1, u64), GraphDbError> {
    check()?;
    let physical_namespace = identity.physical_namespace()?;
    let recovered_commit = latest_projection(
        database,
        &physical_namespace,
        &identity.projection.projection,
    )?
    .ok_or_else(|| GraphDbError::GenerationMismatch {
        namespace: identity.projection.namespace.to_string(),
        projection: identity.projection.projection.to_string(),
        generation: identity.generation.to_string(),
        message: "recovered generation is missing".to_owned(),
    })?
    .commit;
    let expected_dependency_digest = identity.dependency_closure_digest(check)?;
    if recovered_commit.source_generation != identity.source_generation
        || recovered_commit.watermark != identity.watermark
        || recovered_commit.generation_dependency_digest.as_ref()
            != Some(&expected_dependency_digest)
    {
        return Err(GraphDbError::GenerationMismatch {
            namespace: identity.projection.namespace.to_string(),
            projection: identity.projection.projection.to_string(),
            generation: identity.generation.to_string(),
            message:
                "persisted generation source, watermark, or dependency metadata does not match its manifest"
                    .to_owned(),
        });
    }
    let (digest, canonical_bytes) =
        recovered_generation_digest_from_database(database, identity, check)?;
    let actual =
        GraphRecoveredGenerationDigestV1::new(format!("sha256:{digest}")).map_err(|error| {
            GraphDbError::Corrupt {
                message: error.to_string(),
            }
        })?;
    if &actual != expected {
        return Err(GraphDbError::GenerationMismatch {
            namespace: identity.projection.namespace.to_string(),
            projection: identity.projection.projection.to_string(),
            generation: identity.generation.to_string(),
            message: format!(
                "expected recovered digest `{}`, observed `{}`",
                expected.as_str(),
                actual.as_str()
            ),
        });
    }
    Ok((actual, canonical_bytes))
}

#[cfg(test)]
thread_local! {
    static RECOVERED_GENERATION_ENUMERATIONS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    static SEALED_COPY_PROOFS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    static SEALED_COPY_MARKER_HITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    static MANIFEST_CANONICALIZATIONS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    static CANONICAL_BUFFER_ALLOCATION_GROWTHS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
    static DEPENDENCY_CLOSURE_CANONICALIZATIONS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_recovered_generation_enumerations() {
    RECOVERED_GENERATION_ENUMERATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn recovered_generation_enumerations() -> usize {
    RECOVERED_GENERATION_ENUMERATIONS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_sealed_copy_proofs() {
    SEALED_COPY_PROOFS.with(|count| count.set(0));
}

/// Recovered-digest proofs run over sealed per-generation copies on this
/// thread after durable reopen, before installation.
#[cfg(test)]
pub(crate) fn sealed_copy_proofs() -> usize {
    SEALED_COPY_PROOFS.with(std::cell::Cell::get)
}

#[cfg(all(test, feature = "graph-sealed-store"))]
pub(crate) fn reset_sealed_copy_marker_hits() {
    SEALED_COPY_MARKER_HITS.with(|count| count.set(0));
}

/// Sealed-copy opens on this thread that resolved their recovered-digest
/// proof from a verified-generation marker over byte-identical container
/// bytes instead of re-streaming the rows.
#[cfg(all(test, feature = "graph-sealed-store"))]
pub(crate) fn sealed_copy_marker_hits() -> usize {
    SEALED_COPY_MARKER_HITS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn record_sealed_copy_marker_hit() {
    SEALED_COPY_MARKER_HITS.with(|count| count.set(count.get() + 1));
}

#[cfg(test)]
pub(crate) fn reset_manifest_canonicalizations() {
    MANIFEST_CANONICALIZATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn manifest_canonicalizations() -> usize {
    MANIFEST_CANONICALIZATIONS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_canonical_buffer_allocation_growths() {
    CANONICAL_BUFFER_ALLOCATION_GROWTHS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn canonical_buffer_allocation_growths() -> usize {
    CANONICAL_BUFFER_ALLOCATION_GROWTHS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_dependency_closure_canonicalizations() {
    DEPENDENCY_CLOSURE_CANONICALIZATIONS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn dependency_closure_canonicalizations() -> usize {
    DEPENDENCY_CLOSURE_CANONICALIZATIONS.with(std::cell::Cell::get)
}

/// Digest of the generation identity frames alone, in the exact byte layout
/// the recovered-generation proof hashes before its entity and relation
/// frames. This is the binding digest for derived read artifacts (the sealed
/// read bundle): it reuses [`write_generation_identity_frames`] so there is
/// exactly one canonical encoding of a generation's identity, never a
/// parallel authority.
pub(crate) fn generation_identity_frames_digest(
    identity: &GraphGenerationManifestIdentity,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<String, GraphDbError> {
    let mut digest = Sha256::new();
    let mut writer = CheckedDigestWriter::new(&mut digest, check);
    let mut canonical = CheckedVecWriter::new(check, MAX_GRAPH_REPLAY_SOURCE_BYTES_V1)?;
    write_generation_identity_frames(
        &mut writer,
        &mut canonical,
        &identity.projection,
        &identity.generation,
        &identity.source_generation,
        &identity.watermark,
        &identity.dependencies,
    )?;
    writer.finish()?;
    Ok(encode_lowercase_hex(&digest.finalize()))
}

pub(crate) fn physical_namespace_projection_map(
    identity: &GraphGenerationManifestIdentity,
) -> Result<BTreeMap<GraphNamespace, GraphProjectionIdentity>, GraphDbError> {
    let mut map = BTreeMap::from([(identity.physical_namespace()?, identity.projection.clone())]);
    for dependency in &identity.dependencies {
        map.insert(
            physical_namespace(
                &dependency.projection.namespace,
                &dependency.projection.projection,
                &dependency.generation,
            )?,
            dependency.projection.clone(),
        );
    }
    Ok(map)
}

pub(crate) fn recovered_entity_ref(
    store: &dyn grafeo_core::graph::GraphStore,
    node: grafeo_common::types::NodeId,
    namespace_projection: &BTreeMap<GraphNamespace, GraphProjectionIdentity>,
) -> Result<GraphEntityRef, GraphDbError> {
    let entity = store.get_node(node).ok_or_else(|| GraphDbError::Corrupt {
        message: "recovered generation relation endpoint is missing".to_owned(),
    })?;
    let namespace = GraphNamespace::new(required_string(
        entity.get_property(NAMESPACE_PROPERTY),
        "recovered generation endpoint namespace",
    )?)
    .map_err(|error| GraphDbError::Corrupt {
        message: format!("recovered generation endpoint namespace is invalid: {error}"),
    })?;
    let projection = namespace_projection
        .get(&namespace)
        .cloned()
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "recovered generation relation escapes its dependency closure".to_owned(),
        })?;
    let identity = GraphEntityId::new(required_string(
        entity.get_property(crate::schema::ENTITY_ID_PROPERTY),
        "recovered generation endpoint identity",
    )?)
    .map_err(|error| GraphDbError::Corrupt {
        message: format!("recovered generation endpoint identity is invalid: {error}"),
    })?;
    Ok(GraphEntityRef::new(projection, identity))
}

fn recovered_generation_digest(
    manifest: &GraphGenerationManifest,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<String, GraphDbError> {
    #[cfg(test)]
    MANIFEST_CANONICALIZATIONS.with(|count| count.set(count.get() + 1));
    recovered_generation_digest_with_config(
        manifest,
        check,
        ManifestDigestPipelineConfig::production(),
    )
}

struct ManifestDigestPipelineConfig {
    max_workers: usize,
    chunk_rows: usize,
    worker_bytes: usize,
    max_in_flight_bytes: usize,
    metrics: Arc<ManifestDigestPipelineMetrics>,
}

impl ManifestDigestPipelineConfig {
    fn production() -> Self {
        Self {
            max_workers: MANIFEST_DIGEST_MAX_WORKERS,
            chunk_rows: MANIFEST_DIGEST_CHUNK_ROWS,
            worker_bytes: MANIFEST_DIGEST_WORKER_BYTES,
            max_in_flight_bytes: MANIFEST_DIGEST_MAX_IN_FLIGHT_BYTES,
            metrics: Arc::new(ManifestDigestPipelineMetrics::default()),
        }
    }

    #[cfg(test)]
    fn serial() -> Self {
        Self {
            max_workers: 1,
            chunk_rows: usize::MAX,
            worker_bytes: 1,
            max_in_flight_bytes: 1,
            metrics: Arc::new(ManifestDigestPipelineMetrics::default()),
        }
    }

    #[cfg(test)]
    fn testing(
        max_workers: usize,
        chunk_rows: usize,
        worker_bytes: usize,
        max_in_flight_bytes: usize,
        metrics: Arc<ManifestDigestPipelineMetrics>,
    ) -> Self {
        Self {
            max_workers,
            chunk_rows,
            worker_bytes,
            max_in_flight_bytes,
            metrics,
        }
    }

    fn effective_workers(&self, chunk_count: usize) -> Result<usize, GraphDbError> {
        if self.max_workers == 0
            || self.chunk_rows == 0
            || self.worker_bytes == 0
            || self.max_in_flight_bytes < self.worker_bytes
        {
            return Err(GraphDbError::invalid(
                "manifest digest pipeline limits must admit at least one worker",
            ));
        }
        let byte_slots = self.max_in_flight_bytes / self.worker_bytes;
        Ok(self.max_workers.min(byte_slots).min(chunk_count).max(1))
    }
}

#[derive(Default)]
struct ManifestDigestPipelineMetrics {
    current_bytes: AtomicUsize,
    peak_bytes: AtomicUsize,
    reservation_gate: Mutex<()>,
    reservation_released: Condvar,
}

impl ManifestDigestPipelineMetrics {
    fn reserve(
        self: &Arc<Self>,
        bytes: usize,
        maximum: usize,
        abort: &AtomicBool,
    ) -> Result<ManifestDigestReservation, GraphDbError> {
        let mut gate = self.reservation_gate.lock().map_err(|_| {
            GraphDbError::unavailable("manifest digest byte reservation lock is poisoned")
        })?;
        while self
            .current_bytes
            .load(Ordering::Acquire)
            .checked_add(bytes)
            .is_none_or(|current| current > maximum)
        {
            if abort.load(Ordering::Acquire) {
                return Err(GraphDbError::Cancelled);
            }
            gate = self.reservation_released.wait(gate).map_err(|_| {
                GraphDbError::unavailable("manifest digest byte reservation lock is poisoned")
            })?;
        }
        let current = self.current_bytes.fetch_add(bytes, Ordering::AcqRel) + bytes;
        self.peak_bytes.fetch_max(current, Ordering::AcqRel);
        hotpath::gauge!("graph_db.generation.manifest_digest.in_flight_bytes").set(current as f64);
        hotpath::gauge!("graph_db.generation.manifest_digest.peak_in_flight_bytes")
            .set(self.peak_bytes.load(Ordering::Acquire) as f64);
        drop(gate);
        Ok(ManifestDigestReservation {
            bytes,
            metrics: Arc::clone(self),
        })
    }

    fn cancel_waiters(&self) {
        let _gate = match self.reservation_gate.lock() {
            Ok(gate) => gate,
            Err(poisoned) => poisoned.into_inner(),
        };
        self.reservation_released.notify_all();
    }

    #[cfg(test)]
    fn current_bytes(&self) -> usize {
        self.current_bytes.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn peak_bytes(&self) -> usize {
        self.peak_bytes.load(Ordering::Acquire)
    }
}

struct ManifestDigestReservation {
    bytes: usize,
    metrics: Arc<ManifestDigestPipelineMetrics>,
}

impl Drop for ManifestDigestReservation {
    fn drop(&mut self) {
        let _gate = match self.metrics.reservation_gate.lock() {
            Ok(gate) => gate,
            Err(poisoned) => poisoned.into_inner(),
        };
        let prior = self
            .metrics
            .current_bytes
            .fetch_sub(self.bytes, Ordering::AcqRel);
        let remaining = prior.saturating_sub(self.bytes);
        hotpath::gauge!("graph_db.generation.manifest_digest.in_flight_bytes")
            .set(remaining as f64);
        self.metrics.reservation_released.notify_all();
    }
}

#[derive(Clone, Copy)]
enum ManifestDigestChunk<'a> {
    Entities(&'a [GraphEntity]),
    Relations(&'a [GraphGenerationRelation]),
}

impl ManifestDigestChunk<'_> {
    fn row_count(self) -> usize {
        match self {
            Self::Entities(rows) => rows.len(),
            Self::Relations(rows) => rows.len(),
        }
    }
}

struct EncodedManifestDigestChunk {
    bytes: Vec<u8>,
    frame_ends: Vec<usize>,
}

enum ManifestDigestChunkEncoding {
    Encoded(EncodedManifestDigestChunk),
    SerialFallback,
}

struct ReservedManifestDigestChunk {
    encoding: ManifestDigestChunkEncoding,
    _reservation: ManifestDigestReservation,
}

#[hotpath::measure(label = "graph_db.generation.manifest_digest")]
fn recovered_generation_digest_with_config(
    manifest: &GraphGenerationManifest,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    config: ManifestDigestPipelineConfig,
) -> Result<String, GraphDbError> {
    let GraphGenerationManifest {
        projection,
        generation,
        source_generation,
        watermark,
        dependencies,
        entities,
        relations,
        digest_memo: _,
    } = manifest;
    let mut digest = Sha256::new();
    let mut writer = CheckedDigestWriter::new(&mut digest, check);
    let mut canonical = CheckedVecWriter::new(check, MAX_GRAPH_REPLAY_SOURCE_BYTES_V1)?;
    write_generation_identity_frames(
        &mut writer,
        &mut canonical,
        projection,
        generation,
        source_generation,
        watermark,
        dependencies,
    )?;

    let chunks = entities
        .chunks(config.chunk_rows)
        .map(ManifestDigestChunk::Entities)
        .chain(
            relations
                .chunks(config.chunk_rows)
                .map(ManifestDigestChunk::Relations),
        )
        .collect::<Vec<_>>();
    let workers = config.effective_workers(chunks.len())?;
    hotpath::gauge!("graph_db.generation.manifest_digest.effective_workers").set(workers as f64);
    hotpath::gauge!("graph_db.generation.manifest_digest.chunks").set(chunks.len() as f64);
    if workers == 1 {
        for chunk in chunks {
            digest_manifest_chunk_serial(chunk, &mut writer, &mut canonical, check)?;
        }
    } else {
        digest_manifest_chunks_parallel(
            &chunks,
            &mut writer,
            &mut canonical,
            check,
            &config,
            workers,
        )?;
    }
    writer.finish()?;
    Ok(encode_lowercase_hex(&digest.finalize()))
}

#[hotpath::measure(label = "graph_db.generation.manifest_digest.parallel")]
fn digest_manifest_chunks_parallel(
    chunks: &[ManifestDigestChunk<'_>],
    writer: &mut CheckedDigestWriter<'_>,
    canonical: &mut CheckedVecWriter<'_>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    config: &ManifestDigestPipelineConfig,
    workers: usize,
) -> Result<(), GraphDbError> {
    let abort = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let next_chunk = Arc::new(AtomicUsize::new(0));
        let dispatch_gate = Arc::new(Mutex::new(()));
        let (sender, receiver) =
            mpsc::channel::<(usize, Result<ReservedManifestDigestChunk, GraphDbError>)>();
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let sender = sender.clone();
            let abort = &abort;
            let next_chunk = Arc::clone(&next_chunk);
            let dispatch_gate = Arc::clone(&dispatch_gate);
            let metrics = Arc::clone(&config.metrics);
            handles.push(scope.spawn(move || {
                loop {
                    if abort.load(Ordering::Acquire) {
                        break;
                    }
                    let (chunk_index, chunk, reservation) = {
                        // Reserve in canonical chunk order. Otherwise later chunks can
                        // consume every byte slot while an earlier, descheduled worker
                        // waits to reserve, leaving the ordered collector deadlocked.
                        let _dispatch = match dispatch_gate.lock() {
                            Ok(gate) => gate,
                            Err(_) => {
                                let _ = sender.send((
                                    0,
                                    Err(GraphDbError::unavailable(
                                        "manifest digest dispatch lock is poisoned",
                                    )),
                                ));
                                break;
                            }
                        };
                        let chunk_index = next_chunk.fetch_add(1, Ordering::AcqRel);
                        let Some(&chunk) = chunks.get(chunk_index) else {
                            break;
                        };
                        let reservation =
                            metrics.reserve(config.worker_bytes, config.max_in_flight_bytes, abort);
                        (chunk_index, chunk, reservation)
                    };
                    let result = reservation.and_then(|reservation| {
                        match panic::catch_unwind(AssertUnwindSafe(|| {
                            encode_manifest_digest_chunk(chunk, config.worker_bytes, abort)
                        })) {
                            Ok(encoded) => encoded.map(|encoding| ReservedManifestDigestChunk {
                                encoding,
                                _reservation: reservation,
                            }),
                            Err(_) => {
                                Err(GraphDbError::unavailable("manifest digest worker panicked"))
                            }
                        }
                    });
                    let failed = result.is_err();
                    if sender.send((chunk_index, result)).is_err() || failed {
                        break;
                    }
                }
            }));
        }
        drop(sender);

        let result = (|| -> Result<(), GraphDbError> {
            let mut pending = BTreeMap::new();
            let mut expected_chunk = 0usize;
            while expected_chunk < chunks.len() {
                let (chunk_index, encoded) = receiver.recv().map_err(|_| {
                    GraphDbError::unavailable("manifest digest worker exited before its chunk")
                })?;
                if pending.insert(chunk_index, encoded).is_some() {
                    return Err(GraphDbError::unavailable(
                        "manifest digest worker repeated a chunk",
                    ));
                }
                while let Some(reserved) = pending.remove(&expected_chunk) {
                    let reserved = reserved?;
                    let ReservedManifestDigestChunk {
                        encoding,
                        _reservation: reservation,
                    } = reserved;
                    match encoding {
                        ManifestDigestChunkEncoding::Encoded(encoded) => {
                            let mut start = 0usize;
                            for &end in &encoded.frame_ends {
                                check()?;
                                write_digest_bytes(writer, &encoded.bytes[start..end])?;
                                start = end;
                            }
                            drop(encoded);
                            drop(reservation);
                        }
                        ManifestDigestChunkEncoding::SerialFallback => {
                            hotpath::gauge!(
                                "graph_db.generation.manifest_digest.serial_fallback_chunks"
                            )
                            .inc(1_u64);
                            // The worker buffer is already gone; release its
                            // reservation before the one-row serial buffer grows.
                            drop(reservation);
                            let chunk = chunks[expected_chunk];
                            digest_manifest_chunk_serial(chunk, writer, canonical, check)?;
                        }
                    }
                    expected_chunk += 1;
                }
            }
            Ok(())
        })();
        if result.is_err() {
            abort.store(true, Ordering::Release);
            config.metrics.cancel_waiters();
        }
        for (_, encoded) in receiver {
            drop(encoded);
        }
        let worker_panicked = handles.into_iter().any(|handle| handle.join().is_err());
        if result.is_ok() && worker_panicked {
            Err(GraphDbError::unavailable("manifest digest worker panicked"))
        } else {
            result
        }
    })
}

#[hotpath::measure(label = "graph_db.generation.manifest_digest.serial_chunk")]
fn digest_manifest_chunk_serial(
    chunk: ManifestDigestChunk<'_>,
    writer: &mut CheckedDigestWriter<'_>,
    canonical: &mut CheckedVecWriter<'_>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(), GraphDbError> {
    match chunk {
        ManifestDigestChunk::Entities(rows) => {
            for entity in rows {
                check()?;
                write_canonical_frame(
                    writer,
                    canonical,
                    "entity",
                    entity,
                    "recovered generation entity",
                )?;
            }
        }
        ManifestDigestChunk::Relations(rows) => {
            for relation in rows {
                check()?;
                write_canonical_frame(
                    writer,
                    canonical,
                    "relation",
                    relation,
                    "recovered generation relation",
                )?;
            }
        }
    }
    Ok(())
}

#[hotpath::measure(label = "graph_db.generation.manifest_digest.encode_chunk")]
fn encode_manifest_digest_chunk(
    chunk: ManifestDigestChunk<'_>,
    worker_bytes: usize,
    abort: &AtomicBool,
) -> Result<ManifestDigestChunkEncoding, GraphDbError> {
    let row_buffer_bytes = (worker_bytes / 4).max(1);
    let encoded_buffer_bytes = worker_bytes.saturating_sub(row_buffer_bytes);
    let worker_check = || {
        if abort.load(Ordering::Acquire) {
            Err(GraphDbError::Cancelled)
        } else {
            Ok(())
        }
    };
    let mut canonical = CheckedVecWriter::new(&worker_check, row_buffer_bytes)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(encoded_buffer_bytes)
        .map_err(|_| GraphDbError::budget_exhausted_count(GraphBudgetKind::Write, worker_bytes))?;
    if bytes.capacity() > encoded_buffer_bytes {
        return Err(GraphDbError::budget_exhausted_count(
            GraphBudgetKind::Write,
            worker_bytes,
        ));
    }
    let mut encoded = EncodedManifestDigestChunk {
        bytes,
        frame_ends: Vec::with_capacity(chunk.row_count()),
    };
    match chunk {
        ManifestDigestChunk::Entities(rows) => {
            for entity in rows {
                if !append_encoded_manifest_frame(
                    &mut canonical,
                    &mut encoded,
                    encoded_buffer_bytes,
                    "entity",
                    entity,
                    "recovered generation entity",
                    &worker_check,
                )? {
                    return Ok(ManifestDigestChunkEncoding::SerialFallback);
                }
            }
        }
        ManifestDigestChunk::Relations(rows) => {
            for relation in rows {
                if !append_encoded_manifest_frame(
                    &mut canonical,
                    &mut encoded,
                    encoded_buffer_bytes,
                    "relation",
                    relation,
                    "recovered generation relation",
                    &worker_check,
                )? {
                    return Ok(ManifestDigestChunkEncoding::SerialFallback);
                }
            }
        }
    }
    Ok(ManifestDigestChunkEncoding::Encoded(encoded))
}

fn append_encoded_manifest_frame<T: Serialize + ?Sized>(
    canonical: &mut CheckedVecWriter<'_>,
    encoded: &mut EncodedManifestDigestChunk,
    encoded_buffer_bytes: usize,
    tag: &str,
    value: &T,
    subject: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<bool, GraphDbError> {
    check()?;
    let bytes = match canonical.encode(value, subject) {
        Ok(bytes) => bytes,
        Err(GraphDbError::InvalidRequest { message })
            if message.contains("canonical graph replay exceeds its payload bound") =>
        {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    let (tag_len, byte_len) = frame_length_headers(tag, bytes)?;
    let frame_bytes = tag_len
        .len()
        .checked_add(tag.len())
        .and_then(|length| length.checked_add(byte_len.len()))
        .and_then(|length| length.checked_add(bytes.len()))
        .ok_or_else(|| GraphDbError::invalid("manifest digest frame size overflow"))?;
    if encoded
        .bytes
        .len()
        .checked_add(frame_bytes)
        .is_none_or(|length| length > encoded_buffer_bytes)
    {
        return Ok(false);
    }
    encoded.bytes.extend_from_slice(&tag_len);
    encoded.bytes.extend_from_slice(tag.as_bytes());
    encoded.bytes.extend_from_slice(&byte_len);
    encoded.bytes.extend_from_slice(bytes);
    encoded.frame_ends.push(encoded.bytes.len());
    Ok(true)
}

/// Writes the leading identity frames of the recovered-generation digest.
///
/// The in-memory manifest proof and the streamed database proof must agree
/// byte for byte, so both go through this one writer: the frame order
/// (`format`, `projection`, `generation`, `source_generation`, `watermark`,
/// `dependencies`) and each frame's canonical encoding live here only.
fn write_generation_identity_frames(
    writer: &mut CheckedDigestWriter<'_>,
    canonical: &mut CheckedVecWriter<'_>,
    projection: &GraphProjectionIdentity,
    generation: &GraphGenerationId,
    source_generation: &SourceGeneration,
    watermark: &GraphWatermark,
    dependencies: &[GraphGenerationDependency],
) -> Result<(), GraphDbError> {
    write_canonical_frame(
        writer,
        canonical,
        "format",
        "tracedecay.graph-generation.v1",
        "recovered generation format",
    )?;
    write_canonical_frame(
        writer,
        canonical,
        "projection",
        projection,
        "recovered generation projection",
    )?;
    write_canonical_frame(
        writer,
        canonical,
        "generation",
        generation,
        "recovered generation identity",
    )?;
    write_canonical_frame(
        writer,
        canonical,
        "source_generation",
        source_generation,
        "recovered source generation",
    )?;
    write_canonical_frame(
        writer,
        canonical,
        "watermark",
        watermark,
        "recovered generation watermark",
    )?;
    write_canonical_frame(
        writer,
        canonical,
        "dependencies",
        dependencies,
        "recovered generation dependencies",
    )
}

/// Big-endian `(tag length, payload length)` headers of one digest frame.
///
/// The streaming writer ([`write_frame`]) and the parallel proof's chunk
/// encoder (`generation::recovered`) emit the identical frame layout —
/// `tag_len | tag | byte_len | bytes` — so the length encoding lives here
/// once and the two emitters cannot drift.
fn frame_length_headers(tag: &str, bytes: &[u8]) -> Result<([u8; 8], [u8; 8]), GraphDbError> {
    let tag_len =
        u64::try_from(tag.len()).map_err(|_| GraphDbError::invalid("digest tag is too large"))?;
    let byte_len = u64::try_from(bytes.len())
        .map_err(|_| GraphDbError::invalid("digest frame is too large"))?;
    Ok((tag_len.to_be_bytes(), byte_len.to_be_bytes()))
}

fn write_frame(
    writer: &mut CheckedDigestWriter<'_>,
    tag: &str,
    bytes: &[u8],
) -> Result<(), GraphDbError> {
    let (tag_len, byte_len) = frame_length_headers(tag, bytes)?;
    write_digest_bytes(writer, &tag_len)?;
    write_digest_bytes(writer, tag.as_bytes())?;
    write_digest_bytes(writer, &byte_len)?;
    write_digest_bytes(writer, bytes)
}

fn write_canonical_frame<T: Serialize + ?Sized>(
    writer: &mut CheckedDigestWriter<'_>,
    canonical: &mut CheckedVecWriter<'_>,
    tag: &str,
    value: &T,
    subject: &str,
) -> Result<(), GraphDbError> {
    let bytes = canonical.encode(value, subject)?;
    write_frame(writer, tag, bytes)
}

fn write_digest_bytes(
    writer: &mut CheckedDigestWriter<'_>,
    bytes: &[u8],
) -> Result<(), GraphDbError> {
    match writer.write_all(bytes) {
        Ok(()) => Ok(()),
        Err(error) => Err(writer.take_failure().unwrap_or_else(|| {
            GraphDbError::unavailable(format!("failed to hash recovered generation: {error}"))
        })),
    }
}

struct CheckedDigestWriter<'a> {
    digest: &'a mut Sha256,
    bytes_since_check: u64,
    /// Every byte fed to the digest, for the verify byte gauge. Counted here
    /// rather than derived from row counts so the gauge reports the work the
    /// proof actually did.
    total_bytes: u64,
    check: &'a dyn Fn() -> Result<(), GraphDbError>,
    failure: Option<GraphDbError>,
}

impl<'a> CheckedDigestWriter<'a> {
    fn new(digest: &'a mut Sha256, check: &'a dyn Fn() -> Result<(), GraphDbError>) -> Self {
        Self {
            digest,
            bytes_since_check: 0,
            total_bytes: 0,
            check,
            failure: None,
        }
    }

    fn finish(mut self) -> Result<(), GraphDbError> {
        if let Some(error) = self.failure.take() {
            return Err(error);
        }
        (self.check)()
    }

    /// The byte count so far. Read before `finish` consumes the writer.
    fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    fn take_failure(&mut self) -> Option<GraphDbError> {
        self.failure.take()
    }
}

impl Write for CheckedDigestWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = u64::try_from(bytes.len())
            .map_err(|_| io::Error::other("digest frame is too large"))?;
        self.bytes_since_check = self
            .bytes_since_check
            .checked_add(length)
            .ok_or_else(|| io::Error::other("digest check interval overflow"))?;
        self.total_bytes = self.total_bytes.saturating_add(length);
        if self.bytes_since_check >= DIGEST_CHECK_INTERVAL_BYTES {
            self.bytes_since_check = 0;
            if let Err(error) = (self.check)() {
                self.failure = Some(error);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "generation digest interrupted",
                ));
            }
        }
        self.digest.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct CheckedVecWriter<'a> {
    bytes: Vec<u8>,
    bytes_since_check: u64,
    max_bytes: usize,
    check: &'a dyn Fn() -> Result<(), GraphDbError>,
    failure: Option<GraphDbError>,
    #[cfg(test)]
    allocation_growths: usize,
}

impl<'a> CheckedVecWriter<'a> {
    fn new(
        check: &'a dyn Fn() -> Result<(), GraphDbError>,
        max_bytes: usize,
    ) -> Result<Self, GraphDbError> {
        check()?;
        Ok(Self {
            bytes: Vec::new(),
            bytes_since_check: 0,
            max_bytes,
            check,
            failure: None,
            #[cfg(test)]
            allocation_growths: 0,
        })
    }

    fn finish(mut self) -> Result<Vec<u8>, GraphDbError> {
        if let Some(error) = self.failure.take() {
            return Err(error);
        }
        (self.check)()?;
        Ok(self.bytes)
    }

    fn encode<T: Serialize + ?Sized>(
        &mut self,
        value: &T,
        subject: &str,
    ) -> Result<&[u8], GraphDbError> {
        self.bytes.clear();
        self.bytes_since_check = 0;
        self.failure = None;
        (self.check)()?;
        let encoded = serde_json::to_writer(&mut *self, value);
        if let Some(error) = self.failure.take() {
            return Err(error);
        }
        (self.check)()?;
        encoded.map_err(|error| {
            GraphDbError::invalid(format!("failed to encode {subject}: {error}"))
        })?;
        Ok(&self.bytes)
    }

    #[cfg(test)]
    fn allocation_growths(&self) -> usize {
        self.allocation_growths
    }
}

impl Write for CheckedVecWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next_len = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("canonical graph replay size overflow"))?;
        if next_len > self.max_bytes {
            return Err(io::Error::other(
                "canonical graph replay exceeds its payload bound",
            ));
        }
        if next_len > self.bytes.capacity() {
            let growth_target = if self.bytes.capacity() == 0 {
                CHECKED_VEC_INITIAL_CAPACITY_BYTES
            } else {
                self.bytes
                    .capacity()
                    .checked_mul(2)
                    .unwrap_or(self.max_bytes)
            };
            let target_capacity = growth_target.max(next_len).min(self.max_bytes);
            let additional = target_capacity
                .checked_sub(self.bytes.len())
                .ok_or_else(|| {
                    io::Error::other("canonical graph replay capacity is below its encoded length")
                })?;
            #[cfg(test)]
            let capacity_before_reserve = self.bytes.capacity();
            if self.bytes.try_reserve_exact(additional).is_err()
                || self.bytes.capacity() > self.max_bytes
            {
                self.failure = Some(GraphDbError::budget_exhausted_count(
                    GraphBudgetKind::Write,
                    self.max_bytes,
                ));
                return Err(io::Error::other(
                    "canonical graph replay allocation exceeds its product budget",
                ));
            }
            #[cfg(test)]
            if self.bytes.capacity() != capacity_before_reserve {
                self.allocation_growths += 1;
                CANONICAL_BUFFER_ALLOCATION_GROWTHS.with(|count| {
                    count.set(count.get().saturating_add(1));
                });
            }
        }
        let length = u64::try_from(bytes.len())
            .map_err(|_| io::Error::other("canonical graph replay chunk is too large"))?;
        self.bytes_since_check = self
            .bytes_since_check
            .checked_add(length)
            .ok_or_else(|| io::Error::other("canonical replay check interval overflow"))?;
        if self.bytes_since_check >= DIGEST_CHECK_INTERVAL_BYTES {
            self.bytes_since_check = 0;
            if let Err(error) = (self.check)() {
                self.failure = Some(error);
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "canonical graph replay interrupted",
                ));
            }
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn checked_canonical_bytes<T: Serialize + ?Sized>(
    value: &T,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    subject: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, GraphDbError> {
    let mut writer = CheckedVecWriter::new(check, max_bytes)?;
    let encoded = serde_json::to_writer(&mut writer, value);
    let bytes = writer.finish()?;
    encoded
        .map_err(|error| GraphDbError::invalid(format!("failed to encode {subject}: {error}")))?;
    Ok(bytes)
}

#[cfg(test)]
mod checked_vec_writer_tests {
    use std::cell::Cell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Instant;

    use sha2::{Digest, Sha256};
    use tracedecay_domain::canonical_text::encode_lowercase_hex;

    use super::{
        CheckedVecWriter, GraphDbError, GraphGenerationManifest, MANIFEST_DIGEST_CHUNK_ROWS,
        MANIFEST_DIGEST_MAX_IN_FLIGHT_BYTES, MANIFEST_DIGEST_WORKER_BYTES, ManifestDigestChunk,
        ManifestDigestChunkEncoding, ManifestDigestPipelineConfig, ManifestDigestPipelineMetrics,
        canonical_buffer_allocation_growths, checked_canonical_bytes, checked_sorted_entities,
        encode_manifest_digest_chunk, frame_length_headers, recovered_generation_digest,
        recovered_generation_digest_with_config, reset_canonical_buffer_allocation_growths,
    };
    use crate::{
        GraphEntity, GraphEntityId, GraphEntityRef, GraphGenerationId, GraphGenerationRelation,
        GraphLabel, GraphNamespace, GraphProjectionId, GraphProjectionIdentity, GraphProperty,
        GraphPropertyName, GraphRelationId, GraphRelationKind, GraphWatermark, SourceGeneration,
    };

    #[test]
    fn many_tiny_serde_writes_use_bounded_amortized_growth() {
        let value = vec![0_u8; 4_096];
        let mut writer =
            CheckedVecWriter::new(&|| Ok(()), 16 * 1_024).expect("bounded writer initializes");

        serde_json::to_writer(&mut writer, &value).expect("fixture fits the writer bound");
        let allocation_growths = writer.allocation_growths();
        let actual = writer.finish().expect("bounded writer finishes");

        assert_eq!(
            actual,
            serde_json::to_vec(&value).expect("fixture serializes")
        );
        assert_eq!(actual.len(), 8_193);
        assert_eq!(
            encode_lowercase_hex(&Sha256::digest(&actual)),
            "cb113f74dc19a08fcacd246b84ca69e1dff17209792ea3bd8d1b34397f5eca92"
        );
        assert!(
            allocation_growths <= 16,
            "4,096 tiny values caused {allocation_growths} allocation growths"
        );
    }

    #[test]
    fn canonical_bytes_refuse_the_first_byte_past_the_bound() {
        let value = vec![0_u8; 4_096];

        let error = checked_canonical_bytes(&value, &|| Ok(()), "bounded fixture", 8_192)
            .expect_err("8,193 encoded bytes must exceed the bound");

        assert!(matches!(
            error,
            GraphDbError::InvalidRequest { message }
                if message.contains("canonical graph replay exceeds its payload bound")
        ));
    }

    #[test]
    fn writer_capacity_never_exceeds_its_payload_bound() {
        let value = vec![0_u8; 4_096];
        let max_bytes = 8_192;
        let mut writer =
            CheckedVecWriter::new(&|| Ok(()), max_bytes).expect("bounded writer initializes");

        let error = serde_json::to_writer(&mut writer, &value)
            .expect_err("the final encoded byte must be refused");

        assert!(error.to_string().contains("canonical graph replay exceeds"));
        assert!(writer.bytes.len() <= max_bytes);
        assert!(writer.bytes.capacity() <= max_bytes);
    }

    #[test]
    fn recovered_digest_reuses_canonical_buffer_across_manifest_rows() {
        let manifest = manifest_with_entities(4_096);

        reset_canonical_buffer_allocation_growths();
        let digest = recovered_generation_digest(&manifest, &|| Ok(())).unwrap();
        let allocation_growths = canonical_buffer_allocation_growths();
        assert_eq!(
            digest,
            "786f46a4a0f263e5c67927f2a196ce95bd7071733478fb94560c5736dce44f9f"
        );

        assert!(
            allocation_growths <= 8,
            "4,096 manifest rows caused {allocation_growths} canonical-buffer allocation growths"
        );
    }

    #[test]
    fn manifest_digest_is_deterministic_across_worker_widths() {
        let manifest = manifest_with_entities(16_384);
        let serial = recovered_generation_digest_with_config(
            &manifest,
            &|| Ok(()),
            ManifestDigestPipelineConfig::serial(),
        )
        .unwrap();

        for workers in [2, 4, 8] {
            let metrics = Arc::new(ManifestDigestPipelineMetrics::default());
            let parallel = recovered_generation_digest_with_config(
                &manifest,
                &|| Ok(()),
                ManifestDigestPipelineConfig::testing(
                    workers,
                    128,
                    32 * 1024,
                    128 * 1024,
                    Arc::clone(&metrics),
                ),
            )
            .unwrap();
            assert_eq!(parallel, serial, "digest diverged at {workers} workers");
            assert_eq!(metrics.current_bytes(), 0);
        }
    }

    #[test]
    fn parallel_manifest_frames_match_serial_canonical_bytes() {
        let manifest = manifest_with_entities(16_384);
        let encoded = encode_manifest_digest_chunk(
            ManifestDigestChunk::Entities(&manifest.entities),
            16 * 1024 * 1024,
            &AtomicBool::new(false),
        )
        .unwrap();
        let ManifestDigestChunkEncoding::Encoded(encoded) = encoded else {
            panic!("fixture must fit one parallel encode chunk");
        };
        let mut serial = Vec::new();
        for entity in &manifest.entities {
            let bytes = serde_json::to_vec(entity).unwrap();
            let (tag_len, byte_len) = frame_length_headers("entity", &bytes).unwrap();
            serial.extend_from_slice(&tag_len);
            serial.extend_from_slice(b"entity");
            serial.extend_from_slice(&byte_len);
            serial.extend_from_slice(&bytes);
        }

        assert_eq!(encoded.bytes, serial);
        assert_eq!(encoded.frame_ends.last(), Some(&serial.len()));
    }

    #[test]
    fn manifest_digest_caps_reserved_parallel_bytes() {
        let manifest = manifest_with_entities(32_768);
        let metrics = Arc::new(ManifestDigestPipelineMetrics::default());
        let maximum_in_flight = 64 * 1024;
        recovered_generation_digest_with_config(
            &manifest,
            &|| Ok(()),
            ManifestDigestPipelineConfig::testing(
                8,
                128,
                32 * 1024,
                maximum_in_flight,
                Arc::clone(&metrics),
            ),
        )
        .unwrap();

        assert!(metrics.peak_bytes() > 0);
        assert!(
            metrics.peak_bytes() <= maximum_in_flight,
            "peak reservation {} exceeded {maximum_in_flight}",
            metrics.peak_bytes()
        );
        assert_eq!(
            metrics.current_bytes(),
            0,
            "all reservations must be released after collection"
        );
    }

    #[test]
    fn manifest_digest_releases_reservations_on_cancellation_and_error() {
        let manifest = manifest_with_entities(32_768);
        for failure in [
            GraphDbError::Cancelled,
            GraphDbError::unavailable("injected"),
        ] {
            let metrics = Arc::new(ManifestDigestPipelineMetrics::default());
            let polls = Cell::new(0usize);
            let check = || {
                let next = polls.get() + 1;
                polls.set(next);
                if next >= 1_024 {
                    Err(failure.clone())
                } else {
                    Ok(())
                }
            };
            let result = recovered_generation_digest_with_config(
                &manifest,
                &check,
                ManifestDigestPipelineConfig::testing(
                    8,
                    128,
                    32 * 1024,
                    128 * 1024,
                    Arc::clone(&metrics),
                ),
            );

            assert_eq!(result, Err(failure));
            assert_eq!(
                metrics.current_bytes(),
                0,
                "failed pipelines must release every byte reservation"
            );
        }
    }

    #[test]
    fn canonical_entity_sort_reuses_manifest_row_storage() {
        let mut entities = (0..4_096)
            .rev()
            .map(|index| {
                GraphEntity::new(
                    GraphEntityId::new(format!("entity:{index:05}")).unwrap(),
                    BTreeSet::new(),
                    BTreeMap::new(),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        entities.shrink_to_fit();
        let allocation = entities.as_ptr();

        let sorted = checked_sorted_entities(entities, &|| Ok(())).unwrap();

        assert_eq!(
            sorted.as_ptr(),
            allocation,
            "canonical sorting must not materialize a second full row vector"
        );
        assert!(
            sorted
                .windows(2)
                .all(|rows| rows[0].identity < rows[1].identity)
        );
    }

    #[test]
    #[ignore = "large synthetic manifest timing/RSS harness; run explicitly in a fresh process"]
    fn manifest_digest_sandbox_probe() {
        let rows = std::env::var("TRACEDECAY_MANIFEST_BENCH_ROWS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(250_000usize);
        let mode = std::env::var("TRACEDECAY_MANIFEST_BENCH_MODE")
            .unwrap_or_else(|_| "parallel".to_owned());
        let workers = std::env::var("TRACEDECAY_MANIFEST_BENCH_WORKERS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(8usize);
        let projection = GraphProjectionIdentity::new(
            GraphNamespace::new("manifest-sandbox").unwrap(),
            GraphProjectionId::new("code").unwrap(),
        );
        let payload = "manifest-payload-".repeat(16);
        let entities = (0..rows)
            .map(|index| {
                GraphEntity::new(
                    GraphEntityId::new(format!("entity:{index:08}")).unwrap(),
                    BTreeSet::from([GraphLabel::new("symbol").unwrap()]),
                    BTreeMap::from([
                        (
                            GraphPropertyName::new("name").unwrap(),
                            GraphProperty::String(format!("symbol_{index}")),
                        ),
                        (
                            GraphPropertyName::new("payload").unwrap(),
                            GraphProperty::String(payload.clone()),
                        ),
                    ]),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let relations = (0..rows.saturating_sub(1))
            .map(|index| {
                GraphGenerationRelation::new(
                    GraphRelationId::new(format!("relation:{index:08}")).unwrap(),
                    GraphEntityRef::new(
                        projection.clone(),
                        GraphEntityId::new(format!("entity:{index:08}")).unwrap(),
                    ),
                    GraphEntityRef::new(
                        projection.clone(),
                        GraphEntityId::new(format!("entity:{:08}", index + 1)).unwrap(),
                    ),
                    GraphRelationKind::new("calls").unwrap(),
                    BTreeMap::new(),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let manifest = GraphGenerationManifest::new(
            projection,
            GraphGenerationId::new("manifest-sandbox-generation").unwrap(),
            SourceGeneration::new("manifest-sandbox-source").unwrap(),
            GraphWatermark::new("manifest-sandbox-watermark").unwrap(),
            vec![],
            entities,
            relations,
        )
        .unwrap();
        let rss_before = proc_status_kib("VmRSS");
        let hwm_before = proc_status_kib("VmHWM");
        let metrics = Arc::new(ManifestDigestPipelineMetrics::default());
        let config = match mode.as_str() {
            "serial" => ManifestDigestPipelineConfig::serial(),
            "parallel" => ManifestDigestPipelineConfig::testing(
                workers,
                MANIFEST_DIGEST_CHUNK_ROWS,
                MANIFEST_DIGEST_WORKER_BYTES,
                MANIFEST_DIGEST_MAX_IN_FLIGHT_BYTES,
                Arc::clone(&metrics),
            ),
            other => panic!("unknown TRACEDECAY_MANIFEST_BENCH_MODE `{other}`"),
        };
        let started = Instant::now();
        let digest =
            recovered_generation_digest_with_config(&manifest, &|| Ok(()), config).unwrap();
        let elapsed = started.elapsed();
        println!(
            "manifest_digest mode={mode} workers={workers} entities={} relations={} \
             elapsed_ms={} rss_before_kib={} rss_after_kib={} hwm_before_kib={} \
             hwm_after_kib={} peak_in_flight_bytes={} digest={digest}",
            manifest.entities.len(),
            manifest.relations.len(),
            elapsed.as_millis(),
            rss_before,
            proc_status_kib("VmRSS"),
            hwm_before,
            proc_status_kib("VmHWM"),
            metrics.peak_bytes(),
        );
    }

    fn proc_status_kib(field: &str) -> u64 {
        let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
            return 0;
        };
        let prefix = format!("{field}:");
        status
            .lines()
            .find_map(|line| line.strip_prefix(&prefix))
            .and_then(|value| value.split_whitespace().next())
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    }

    fn manifest_with_entities(entity_count: usize) -> GraphGenerationManifest {
        GraphGenerationManifest::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new("allocation-probe").unwrap(),
                GraphProjectionId::new("manifest").unwrap(),
            ),
            GraphGenerationId::new("generation-allocation-probe").unwrap(),
            SourceGeneration::new("source-allocation-probe").unwrap(),
            GraphWatermark::new("watermark-allocation-probe").unwrap(),
            vec![],
            (0..entity_count)
                .map(|index| {
                    GraphEntity::new(
                        GraphEntityId::new(format!("entity:{index:05}")).unwrap(),
                        BTreeSet::new(),
                        BTreeMap::new(),
                    )
                    .unwrap()
                })
                .collect(),
            vec![],
        )
        .unwrap()
    }
}

#[cfg(test)]
mod manifest_digest_memo_tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;

    use tracedecay_store::runtime::{
        BrainId, GraphPublicationInputDigestV1, ProjectId, StoreShardIdV1, UserProfileId,
    };

    use super::{
        GraphGenerationManifest, dependency_closure_canonicalizations, manifest_canonicalizations,
        reset_dependency_closure_canonicalizations, reset_manifest_canonicalizations,
    };
    use crate::{
        GraphDbLocation, GraphDbOpenOptions, GraphDbOwner, GraphDurability, GraphEntity,
        GraphEntityId, GraphFormatVersion, GraphGenerationDependency, GraphGenerationId,
        GraphIdempotencyKey, GraphNamespace, GraphProjectionId, GraphProjectionIdentity,
        GraphWatermark, NeverCancelled, SourceGeneration,
    };

    fn dependency(index: usize) -> GraphGenerationDependency {
        GraphGenerationDependency::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new(format!("memo-dependency-{index}")).unwrap(),
                GraphProjectionId::new("code").unwrap(),
            ),
            GraphGenerationId::new(format!("dependency-generation-{index}")).unwrap(),
            GraphIdempotencyKey::new(format!("dependency-publication-{index}")).unwrap(),
        )
    }

    fn entity(index: usize) -> GraphEntity {
        GraphEntity::new(
            GraphEntityId::new(format!("entity:{index:03}")).unwrap(),
            BTreeSet::new(),
            BTreeMap::new(),
        )
        .unwrap()
    }

    fn manifest_fixture(
        namespace: &str,
        dependencies: Vec<GraphGenerationDependency>,
    ) -> GraphGenerationManifest {
        GraphGenerationManifest::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new(namespace).unwrap(),
                GraphProjectionId::new("manifest").unwrap(),
            ),
            GraphGenerationId::new("generation-digest-memo").unwrap(),
            SourceGeneration::new("source-digest-memo").unwrap(),
            GraphWatermark::new("watermark-digest-memo").unwrap(),
            dependencies,
            (0..16).map(entity).collect(),
            vec![],
        )
        .unwrap()
    }

    fn shard() -> StoreShardIdV1 {
        StoreShardIdV1::project(
            BrainId::new("brain.digest-memo").unwrap(),
            UserProfileId::new("profile.digest-memo").unwrap(),
            ProjectId::new("project.digest-memo").unwrap(),
        )
    }

    fn input_digest() -> GraphPublicationInputDigestV1 {
        GraphPublicationInputDigestV1::new(format!("sha256:{}", "a".repeat(64))).unwrap()
    }

    #[test]
    fn produce_and_hydrate_flow_canonicalizes_each_digest_once_per_instance() {
        let produced = manifest_fixture("digest-memo-flow", vec![dependency(1), dependency(2)]);
        reset_manifest_canonicalizations();
        reset_dependency_closure_canonicalizations();

        let replay = produced
            .relational_replay(
                shard(),
                GraphIdempotencyKey::new("publish:digest-memo").unwrap(),
                input_digest(),
                None,
                &|| Ok(()),
            )
            .unwrap();
        assert_eq!(manifest_canonicalizations(), 1);
        assert_eq!(dependency_closure_canonicalizations(), 1);

        let sealed = produced.expected_recovered_digest(&|| Ok(())).unwrap();
        let dependency_digest = produced.dependency_closure_digest(&|| Ok(())).unwrap();
        assert_eq!(sealed, replay.expected_recovered_digest);
        assert_eq!(
            dependency_digest,
            replay.dependency_generation_closure_digest
        );
        assert_eq!(
            manifest_canonicalizations(),
            1,
            "re-reading a produced manifest's digest must not re-canonicalize it"
        );
        assert_eq!(
            dependency_closure_canonicalizations(),
            1,
            "re-reading a produced manifest's dependency digest must not re-encode the closure"
        );

        let hydrated = GraphGenerationManifest::from_inline_replay(&replay, &|| Ok(())).unwrap();
        assert_eq!(
            manifest_canonicalizations(),
            2,
            "hydration proves the journaled digest against the fresh instance exactly once"
        );
        assert_eq!(dependency_closure_canonicalizations(), 2);

        let identity = hydrated.identity();
        assert_eq!(
            identity.dependency_closure_digest(&|| Ok(())).unwrap(),
            dependency_digest
        );
        assert_eq!(
            hydrated.expected_recovered_digest(&|| Ok(())).unwrap(),
            sealed
        );
        assert_eq!(
            manifest_canonicalizations(),
            2,
            "this produce/reread/hydrate/reread sequence canonicalized the manifest 4 times before memoization"
        );
        assert_eq!(
            dependency_closure_canonicalizations(),
            2,
            "this sequence canonicalized the dependency closure 4 times before memoization"
        );

        // Byte identity: a cold instance built from the same inputs computes
        // the exact digests the memoized reads served.
        let control = manifest_fixture("digest-memo-flow", vec![dependency(1), dependency(2)]);
        assert_eq!(
            control.expected_recovered_digest(&|| Ok(())).unwrap(),
            sealed
        );
        assert_eq!(
            control.dependency_closure_digest(&|| Ok(())).unwrap(),
            dependency_digest
        );
    }

    #[test]
    fn verification_reuses_the_dependency_digest_that_rode_along() {
        let owner = GraphDbOwner::open(GraphDbOpenOptions {
            location: GraphDbLocation::Memory,
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::Memory,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap();
        let database = owner.issue_lease().unwrap();
        let manifest = Arc::new(manifest_fixture("digest-memo-verify", vec![]));
        database
            .apply_generation_unverified(Arc::clone(&manifest), &|| Ok(()))
            .unwrap();
        let sealed = manifest.expected_recovered_digest(&|| Ok(())).unwrap();
        let dependency_digest = manifest.dependency_closure_digest(&|| Ok(())).unwrap();
        let identity = manifest.identity();

        reset_manifest_canonicalizations();
        reset_dependency_closure_canonicalizations();
        let (_, recovered) = database
            .verify_existing_generation(&identity, &sealed, &|| Ok(()))
            .unwrap();

        assert_eq!(recovered, sealed);
        assert_eq!(
            identity.dependency_closure_digest(&|| Ok(())).unwrap(),
            dependency_digest
        );
        assert_eq!(
            dependency_closure_canonicalizations(),
            0,
            "the verify proof reuses the digest that rode along on the identity; it re-encoded the closure once per verify before memoization"
        );
        assert_eq!(
            manifest_canonicalizations(),
            0,
            "verification must never re-canonicalize the manifest"
        );
        owner.close().unwrap();
    }

    #[test]
    fn mutation_after_a_digest_read_recomputes_instead_of_serving_the_memo() {
        let mut manifest =
            manifest_fixture("digest-memo-mutation", vec![dependency(1), dependency(2)]);
        let dependency_before = manifest.dependency_closure_digest(&|| Ok(())).unwrap();
        let recovered_before = manifest.expected_recovered_digest(&|| Ok(())).unwrap();

        manifest.dependencies.push(dependency(3));
        let dependency_after = manifest.dependency_closure_digest(&|| Ok(())).unwrap();
        let recovered_after = manifest.expected_recovered_digest(&|| Ok(())).unwrap();
        assert_ne!(dependency_after, dependency_before);
        assert_ne!(recovered_after, recovered_before);
        assert_eq!(
            manifest
                .identity()
                .dependency_closure_digest(&|| Ok(()))
                .unwrap(),
            dependency_after,
            "an identity taken after the mutation must not inherit the stale memo"
        );

        manifest.entities.push(entity(999));
        let recovered_rows = manifest.expected_recovered_digest(&|| Ok(())).unwrap();
        assert_ne!(recovered_rows, recovered_after);

        manifest.watermark = GraphWatermark::new("watermark-digest-memo-mutated").unwrap();
        let recovered_watermark = manifest.expected_recovered_digest(&|| Ok(())).unwrap();
        assert_ne!(recovered_watermark, recovered_rows);

        // The recomputed digests are the true digests of the mutated fields:
        // a cold instance rebuilt from them computes the same values.
        let control = GraphGenerationManifest::new(
            manifest.projection.clone(),
            manifest.generation.clone(),
            manifest.source_generation.clone(),
            manifest.watermark.clone(),
            manifest.dependencies.clone(),
            manifest.entities.clone(),
            manifest.relations.clone(),
        )
        .unwrap();
        assert_eq!(
            control.expected_recovered_digest(&|| Ok(())).unwrap(),
            recovered_watermark
        );
        assert_eq!(
            control.dependency_closure_digest(&|| Ok(())).unwrap(),
            dependency_after
        );
    }

    #[test]
    fn identity_mutation_recomputes_the_propagated_digest() {
        let manifest = manifest_fixture("digest-memo-identity", vec![dependency(1), dependency(2)]);
        let seeded = manifest.dependency_closure_digest(&|| Ok(())).unwrap();
        let mut identity = manifest.identity();
        identity.dependencies.pop();

        let recomputed = identity.dependency_closure_digest(&|| Ok(())).unwrap();
        assert_ne!(recomputed, seeded);
        let control = super::dependency_closure_digest(&identity.dependencies, &|| Ok(())).unwrap();
        assert_eq!(recomputed, control);
    }

    #[test]
    fn a_clone_starts_with_a_cold_memo() {
        let manifest = manifest_fixture("digest-memo-clone", vec![dependency(1)]);
        let dependency_digest = manifest.dependency_closure_digest(&|| Ok(())).unwrap();
        let sealed = manifest.expected_recovered_digest(&|| Ok(())).unwrap();

        let clone = manifest.clone();
        reset_manifest_canonicalizations();
        reset_dependency_closure_canonicalizations();
        assert_eq!(
            clone.dependency_closure_digest(&|| Ok(())).unwrap(),
            dependency_digest
        );
        assert_eq!(clone.expected_recovered_digest(&|| Ok(())).unwrap(), sealed);
        assert_eq!(
            dependency_closure_canonicalizations(),
            1,
            "a clone must recompute rather than inherit its source's memo"
        );
        assert_eq!(
            manifest_canonicalizations(),
            1,
            "a clone must recompute rather than inherit its source's memo"
        );
    }
}
