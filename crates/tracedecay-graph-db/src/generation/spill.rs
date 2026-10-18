//! Generation rows produced in batches and published without holding the
//! whole row set in memory.
//!
//! A producer pushes entities and relations in any order, one bounded batch
//! at a time. Each batch is canonicalized, sorted by identity, and written as
//! a run under a private spill directory; only the entity identities stay
//! resident, because every relation endpoint resolves to its entity's position
//! in the global identity order. [`GraphGenerationRowSpill::finish`] merges the
//! runs into one sorted entity file and one sorted relation file and hashes the
//! merged stream through the same frames as the in-memory manifest proof, so a
//! spilled generation's recovered digest, sealed container, and replay binding
//! are byte-identical to the manifest built from the same rows.
//!
//! The spill directory is scratch space: it is deleted when the spill or the
//! finished generation drops, and a directory left behind by a killed process
//! is swept the next time its graph store opens.

use std::cmp::{Ordering as CmpOrdering, Reverse};
use std::collections::BinaryHeap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_text::encode_lowercase_hex;
use tracedecay_store::runtime::{
    GraphDependencyGenerationClosureDigestV1, GraphGenerationIdV1, GraphNamespaceV1,
    GraphProjectionIdV1, GraphProjectionIdentityV1, GraphPublicationIdempotencyKeyV1,
    GraphPublicationInputDigestV1, GraphPublicationKeyV1, GraphPublicationReplayV1,
    GraphRecoveredGenerationDigestV1, GraphVerifiedHeadV1, MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
    StoreShardIdV1,
};

use crate::limits::{MAX_VERIFIED_GENERATION_ENTITIES, MAX_VERIFIED_GENERATION_RELATIONS};
use crate::row_index::{
    ENTITY_ROW_OFFSETS_FILE, EntityRowOffsetsWriter, ROW_INDEX_FILE, RowIndexBuilder,
};
use crate::{GraphBudgetKind, GraphDbError, GraphEntity, GraphEntityId, GraphIdempotencyKey};

use super::{
    CheckedDigestWriter, CheckedVecWriter, GraphGenerationManifest,
    GraphGenerationManifestIdentity, GraphGenerationRelation, GraphGenerationReplaySource,
    GraphProjectionIdentity, GraphRowDigestSum, SealedCodeGenerationReplay,
    checked_canonical_bytes, relational_dependency_generations, row_frame_lanes,
    validate_sealed_replay, write_generation_identity_frames, write_row_frame,
};

/// Canonical bytes a spill buffers before it sorts and writes one run.
///
/// Bounds the producer-side working set independently of the corpus: a run
/// is written as soon as a batch pushes the buffer past it.
pub const GRAPH_ROW_SPILL_RUN_BYTES: usize = 32 * 1024 * 1024;
/// Read and write buffer per open run or merged file.
const SPILL_IO_BUFFER_BYTES: usize = 256 * 1024;
pub(crate) const ENTITIES_FILE: &str = "entities.rows";
const RELATIONS_FILE: &str = "relations.rows";
/// A producer-owned file that travels with the sealed generation, see
/// [`GraphGenerationRowSpill::attachment_path`].
const ATTACHMENT_FILE: &str = "attachment";

/// Scratch directory owned by one spill; removed with its owner.
struct SpillDirectory(PathBuf);

impl SpillDirectory {
    fn create(path: PathBuf) -> Result<Self, GraphDbError> {
        std::fs::create_dir(&path).map_err(|error| spill_io("directory create", error))?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for SpillDirectory {
    fn drop(&mut self) {
        match std::fs::remove_dir_all(&self.0) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(
                event = "graph_row_spill_remove_failed",
                path = %self.0.display(),
                error = %error,
                "graph row spill directory could not be removed; the next store open sweeps it"
            ),
        }
    }
}

fn spill_io(context: &str, error: std::io::Error) -> GraphDbError {
    GraphDbError::unavailable(format!("graph row spill {context} failed: {error}"))
}

/// One canonical row as it sits in a run: its identity (the sort key), the
/// entity identities a relation's endpoints name (empty for entities), and
/// the canonical JSON the recovered digest hashes.
struct SpillRow {
    identity: String,
    endpoints: [String; 2],
    canonical: Vec<u8>,
}

impl SpillRow {
    fn bytes(&self) -> usize {
        self.identity.len()
            + self.endpoints[0].len()
            + self.endpoints[1].len()
            + self.canonical.len()
    }

    fn encoded_len(&self) -> u64 {
        [
            self.identity.len(),
            self.endpoints[0].len(),
            self.endpoints[1].len(),
            self.canonical.len(),
        ]
        .into_iter()
        .fold(0_u64, |total, length| {
            total.saturating_add(4).saturating_add(length as u64)
        })
    }

    fn write(&self, writer: &mut impl Write) -> Result<(), GraphDbError> {
        for field in [
            self.identity.as_bytes(),
            self.endpoints[0].as_bytes(),
            self.endpoints[1].as_bytes(),
            self.canonical.as_slice(),
        ] {
            let length = u32::try_from(field.len())
                .map_err(|_| GraphDbError::invalid("graph row spill field exceeds u32 bytes"))?;
            writer
                .write_all(&length.to_be_bytes())
                .and_then(|()| writer.write_all(field))
                .map_err(|error| spill_io("run write", error))?;
        }
        Ok(())
    }

    /// Reads the next row, or `None` at a clean end of file.
    fn read(reader: &mut impl Read) -> Result<Option<Self>, GraphDbError> {
        let mut fields: [Vec<u8>; 4] = Default::default();
        for (index, field) in fields.iter_mut().enumerate() {
            let mut length = [0_u8; 4];
            match reader.read_exact(&mut length) {
                Ok(()) => {}
                Err(error) if index == 0 && error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(None);
                }
                Err(error) => return Err(spill_io("run read", error)),
            }
            field.resize(u32::from_be_bytes(length) as usize, 0);
            reader
                .read_exact(field)
                .map_err(|error| spill_io("run read", error))?;
        }
        let [identity, from, to, canonical] = fields;
        let text = |bytes: Vec<u8>| {
            String::from_utf8(bytes).map_err(|_| GraphDbError::Corrupt {
                message: "graph row spill identity is not UTF-8".to_owned(),
            })
        };
        Ok(Some(Self {
            identity: text(identity)?,
            endpoints: [text(from)?, text(to)?],
            canonical,
        }))
    }
}

/// Buffered rows of one kind plus the runs already written for it.
struct RowRuns {
    kind: &'static str,
    buffer: Vec<SpillRow>,
    buffered_bytes: usize,
    runs: Vec<PathBuf>,
    pushed: usize,
}

impl RowRuns {
    fn new(kind: &'static str) -> Self {
        Self {
            kind,
            buffer: Vec::new(),
            buffered_bytes: 0,
            runs: Vec::new(),
            pushed: 0,
        }
    }

    fn push(&mut self, row: SpillRow, directory: &Path) -> Result<(), GraphDbError> {
        self.buffered_bytes = self.buffered_bytes.saturating_add(row.bytes());
        self.buffer.push(row);
        self.pushed += 1;
        if self.buffered_bytes >= GRAPH_ROW_SPILL_RUN_BYTES {
            self.flush(directory)?;
        }
        Ok(())
    }

    /// Sorts the buffered rows and writes them as one run.
    fn flush(&mut self, directory: &Path) -> Result<(), GraphDbError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let mut rows = std::mem::take(&mut self.buffer);
        self.buffered_bytes = 0;
        rows.sort_unstable_by(|left, right| {
            left.identity
                .cmp(&right.identity)
                .then_with(|| left.canonical.cmp(&right.canonical))
        });
        let path = directory.join(format!("{}-{}.run", self.kind, self.runs.len()));
        let file = File::create(&path).map_err(|error| spill_io("run create", error))?;
        let mut writer = BufWriter::with_capacity(SPILL_IO_BUFFER_BYTES, file);
        for row in &rows {
            row.write(&mut writer)?;
        }
        writer
            .flush()
            .map_err(|error| spill_io("run flush", error))?;
        self.runs.push(path);
        Ok(())
    }
}

/// Accepts one generation's rows in batches and spills them to sorted runs.
///
/// Every row is validated as it arrives exactly as the manifest constructor
/// validates it; duplicate identities, dangling endpoints, and escaping
/// endpoints are refused when [`Self::finish`] merges the runs. A spilled
/// generation is dependency-free: every relation endpoint must be one of its
/// own entities.
pub struct GraphGenerationRowSpill {
    directory: SpillDirectory,
    projection: GraphProjectionIdentity,
    entities: RowRuns,
    relations: RowRuns,
    entity_identities: Vec<GraphEntityId>,
    /// Relation endpoints, tracked only for a layered delta whose relations
    /// may reach rows its base serves.
    endpoints: Option<std::collections::HashSet<GraphEntityId>>,
}

/// Bytes one row adds to a spill buffer: `buffered` counts toward
/// [`GRAPH_ROW_SPILL_RUN_BYTES`], `resident` adds the row slot the buffer
/// holds it in.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GraphSpillRowFootprint {
    pub buffered: usize,
    pub resident: usize,
}

impl GraphSpillRowFootprint {
    pub fn of_entity(entity: &GraphEntity) -> Result<Self, GraphDbError> {
        let canonical = canonical_row(entity, "recovered generation entity", &|| Ok(()))?;
        Ok(Self::of_row(
            entity.identity.as_str().len() + canonical.len(),
        ))
    }

    pub fn of_relation(relation: &GraphGenerationRelation) -> Result<Self, GraphDbError> {
        let canonical = canonical_row(relation, "recovered generation relation", &|| Ok(()))?;
        Ok(Self::of_row(
            relation.identity.as_str().len()
                + relation.from.identity.as_str().len()
                + relation.to.identity.as_str().len()
                + canonical.len(),
        ))
    }

    fn of_row(buffered: usize) -> Self {
        Self {
            buffered,
            resident: buffered.saturating_add(size_of::<SpillRow>()),
        }
    }
}

impl GraphGenerationRowSpill {
    /// Creates a spill whose scratch runs live in `directory`, which must not
    /// exist yet and is removed with the spill.
    pub fn create(
        directory: PathBuf,
        projection: GraphProjectionIdentity,
    ) -> Result<Self, GraphDbError> {
        Ok(Self {
            directory: SpillDirectory::create(directory)?,
            projection,
            entities: RowRuns::new("entities"),
            relations: RowRuns::new("relations"),
            entity_identities: Vec::new(),
            endpoints: None,
        })
    }

    /// A spill for a layered delta: relations may name endpoints the delta
    /// does not carry, reported by [`Self::missing_endpoints`] so the layered
    /// builder copies them from the base before [`Self::finish`].
    pub(crate) fn create_layered(
        directory: PathBuf,
        projection: GraphProjectionIdentity,
    ) -> Result<Self, GraphDbError> {
        let mut spill = Self::create(directory, projection)?;
        spill.endpoints = Some(std::collections::HashSet::new());
        Ok(spill)
    }

    /// Scratch directory the spill owns; removed with the spill.
    pub(crate) fn directory(&self) -> &Path {
        self.directory.path()
    }

    /// Where the producer may write one file that is sealed beside the
    /// generation's container and reaches every layered generation built on
    /// it. Absent unless the producer writes it.
    #[must_use]
    pub fn attachment_path(&self) -> PathBuf {
        self.directory.path().join(ATTACHMENT_FILE)
    }

    /// The distinct entity identities pushed so far, ascending.
    pub(crate) fn sorted_entity_identities(&mut self) -> &[GraphEntityId] {
        self.distinct_entities();
        &self.entity_identities
    }

    /// Relation endpoints pushed so far that no pushed entity carries.
    pub(crate) fn missing_endpoints(&mut self) -> Vec<GraphEntityId> {
        self.distinct_entities();
        let Some(endpoints) = self.endpoints.as_ref() else {
            return Vec::new();
        };
        let mut missing = endpoints
            .iter()
            .filter(|endpoint| self.entity_identities.binary_search(endpoint).is_err())
            .cloned()
            .collect::<Vec<_>>();
        missing.sort_unstable();
        missing
    }

    /// Distinct entity identities pushed so far: the entity count of the
    /// generation if no further entity is pushed.
    pub fn distinct_entities(&mut self) -> usize {
        self.entity_identities.sort_unstable();
        self.entity_identities.dedup();
        self.entity_identities.len()
    }

    /// Adds one batch of rows. The batch may arrive in any order and may
    /// repeat a row another batch already pushed; an identical repeat is one
    /// row of the generation, a differing one fails the merge.
    pub fn push_batch(
        &mut self,
        entities: Vec<GraphEntity>,
        relations: Vec<GraphGenerationRelation>,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        check()?;
        self.entity_identities.reserve(entities.len());
        for entity in entities {
            check()?;
            entity.validate()?;
            let canonical = canonical_row(&entity, "recovered generation entity", check)?;
            let GraphEntity { identity, .. } = entity;
            let row = SpillRow {
                identity: identity.as_str().to_owned(),
                endpoints: Default::default(),
                canonical,
            };
            self.entity_identities.push(identity);
            self.entities.push(row, self.directory.path())?;
        }
        for relation in relations {
            check()?;
            relation.validate()?;
            for endpoint in [&relation.from, &relation.to] {
                if endpoint.projection != self.projection {
                    return Err(GraphDbError::invalid(format!(
                        "relation endpoint projection `{}` is not the candidate or an exact dependency",
                        endpoint.projection
                    )));
                }
                if let Some(endpoints) = self.endpoints.as_mut() {
                    endpoints.insert(endpoint.identity.clone());
                }
            }
            let canonical = canonical_row(&relation, "recovered generation relation", check)?;
            let row = SpillRow {
                identity: relation.identity.as_str().to_owned(),
                endpoints: [
                    relation.from.identity.as_str().to_owned(),
                    relation.to.identity.as_str().to_owned(),
                ],
                canonical,
            };
            self.relations.push(row, self.directory.path())?;
        }
        if self.entity_identities.len() > MAX_VERIFIED_GENERATION_ENTITIES.saturating_mul(2) {
            // Identities are deduplicated at finish; this bounds the resident
            // list against a producer that repeats rows without end.
            self.distinct_entities();
        }
        Ok(())
    }

    /// Merges every run into the generation's canonical row order and hashes
    /// it into the recovered digest.
    pub fn finish(
        mut self,
        identity: GraphGenerationManifestIdentity,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<SpilledGraphGeneration, GraphDbError> {
        check()?;
        if identity.projection != self.projection {
            return Err(GraphDbError::invalid(
                "a spilled graph generation names a foreign projection",
            ));
        }
        if !identity.dependencies.is_empty() {
            return Err(GraphDbError::invalid(
                "a spilled graph generation must be dependency-free",
            ));
        }
        if self.distinct_entities() > MAX_VERIFIED_GENERATION_ENTITIES {
            return Err(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Capacity,
                MAX_VERIFIED_GENERATION_ENTITIES,
            ));
        }
        self.entities.flush(self.directory.path())?;
        self.relations.flush(self.directory.path())?;

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
        drop(canonical);
        let directory = self.directory.path().to_path_buf();
        let entity_identities = self.entity_identities;
        // A generation a later refresh may layer over records every row's
        // digest beside its container; see `row_index`.
        let mut row_index = directory
            .join(ATTACHMENT_FILE)
            .is_file()
            .then(RowIndexBuilder::new);
        let mut entity_offsets = row_index
            .is_some()
            .then(|| {
                EntityRowOffsetsWriter::create(
                    &directory.join(ENTITY_ROW_OFFSETS_FILE),
                    entity_identities.len(),
                )
            })
            .transpose()?;
        let mut entity_row_offset = 0_u64;
        let entity_count = merge_runs(
            &self.entities.runs,
            &directory.join(ENTITIES_FILE),
            check,
            |row| {
                let lanes = write_row_frame(&mut writer, "entity", &row.canonical)?;
                if let Some(index) = row_index.as_mut() {
                    index.entity(&row.identity, lanes)?;
                }
                if let Some(offsets) = entity_offsets.as_mut() {
                    let length = row.encoded_len();
                    offsets.push(entity_row_offset, length)?;
                    entity_row_offset = entity_row_offset.checked_add(length).ok_or_else(|| {
                        GraphDbError::unavailable("graph entity row offsets exceed u64")
                    })?;
                }
                Ok(())
            },
        )?;
        if entity_count != entity_identities.len() {
            return Err(GraphDbError::Corrupt {
                message: "graph row spill merged a different entity set than it was pushed"
                    .to_owned(),
            });
        }
        let relation_count = merge_runs(
            &self.relations.runs,
            &directory.join(RELATIONS_FILE),
            check,
            |row| {
                let mut positions = [0_usize; 2];
                for (position, endpoint) in positions.iter_mut().zip(&row.endpoints) {
                    *position = entity_identities
                        .binary_search_by(|entity| entity.as_str().cmp(endpoint))
                        .map_err(|_| {
                            GraphDbError::invalid(format!(
                                "local relation endpoint `{endpoint}` is absent from the candidate generation"
                            ))
                        })?;
                }
                let lanes = write_row_frame(&mut writer, "relation", &row.canonical)?;
                if let Some(index) = row_index.as_mut() {
                    index.relation(&row.identity, lanes, positions[0], positions[1])?;
                }
                Ok(())
            },
        )?;
        if relation_count > MAX_VERIFIED_GENERATION_RELATIONS {
            return Err(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Capacity,
                MAX_VERIFIED_GENERATION_RELATIONS,
            ));
        }
        let row_sum = writer.row_sum();
        writer.finish()?;
        if let Some(offsets) = entity_offsets {
            offsets.finish()?;
        }
        if let Some(index) = row_index {
            index.write(&directory.join(ROW_INDEX_FILE))?;
        }
        let expected_recovered_digest = GraphRecoveredGenerationDigestV1::new(format!(
            "sha256:{}",
            encode_lowercase_hex(&digest.finalize())
        ))
        .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        for run in self.entities.runs.iter().chain(&self.relations.runs) {
            std::fs::remove_file(run).map_err(|error| spill_io("run remove", error))?;
        }
        crate::observe::record_counts(entity_count, relation_count, 0, 0);
        Ok(SpilledGraphGeneration {
            identity,
            directory: self.directory,
            entity_identities,
            relation_count,
            expected_recovered_digest,
            row_sum,
        })
    }
}

/// A row's canonical bytes, trimmed to their length: a buffered row holds
/// its allocation until its run is written, and the encoder grows by
/// doubling.
fn canonical_row<T: serde::Serialize>(
    value: &T,
    subject: &str,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Vec<u8>, GraphDbError> {
    let mut canonical =
        checked_canonical_bytes(value, check, subject, MAX_GRAPH_REPLAY_SOURCE_BYTES_V1)?;
    canonical.shrink_to_fit();
    Ok(canonical)
}

/// K-way merges sorted runs into `output`, visiting each distinct row once in
/// identity order. An identical repeat collapses into one row; two rows that
/// share an identity with different bytes refuse the generation exactly as
/// the manifest constructor refuses a repeated identity. Returns the number
/// of rows written.
fn merge_runs(
    runs: &[PathBuf],
    output: &Path,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    mut visit: impl FnMut(&SpillRow) -> Result<(), GraphDbError>,
) -> Result<usize, GraphDbError> {
    struct Head {
        row: SpillRow,
        run: usize,
    }
    impl PartialEq for Head {
        fn eq(&self, other: &Self) -> bool {
            self.cmp(other) == CmpOrdering::Equal
        }
    }
    impl Eq for Head {}
    impl PartialOrd for Head {
        fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
            Some(self.cmp(other))
        }
    }
    impl Ord for Head {
        fn cmp(&self, other: &Self) -> CmpOrdering {
            self.row
                .identity
                .cmp(&other.row.identity)
                .then_with(|| self.row.canonical.cmp(&other.row.canonical))
                .then_with(|| self.run.cmp(&other.run))
        }
    }

    let mut readers = runs
        .iter()
        .map(|run| {
            File::open(run)
                .map(|file| BufReader::with_capacity(SPILL_IO_BUFFER_BYTES, file))
                .map_err(|error| spill_io("run open", error))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut heap = BinaryHeap::with_capacity(readers.len());
    for (run, reader) in readers.iter_mut().enumerate() {
        if let Some(row) = SpillRow::read(reader)? {
            heap.push(Reverse(Head { row, run }));
        }
    }
    let file = File::create(output).map_err(|error| spill_io("merge create", error))?;
    let mut writer = BufWriter::with_capacity(SPILL_IO_BUFFER_BYTES, file);
    let mut previous: Option<SpillRow> = None;
    let mut written = 0usize;
    while let Some(Reverse(Head { row, run })) = heap.pop() {
        check()?;
        if let Some(next) = SpillRow::read(&mut readers[run])? {
            heap.push(Reverse(Head { row: next, run }));
        }
        if let Some(previous) = previous.as_ref()
            && previous.identity == row.identity
        {
            if previous.canonical == row.canonical {
                continue;
            }
            return Err(GraphDbError::invalid(
                "a graph generation repeats an entity or relation identity",
            ));
        }
        visit(&row)?;
        row.write(&mut writer)?;
        written += 1;
        previous = Some(row);
    }
    writer
        .flush()
        .map_err(|error| spill_io("merge flush", error))?;
    Ok(written)
}

/// A generation whose canonical rows sit merged on disk, sorted and unique,
/// with the recovered digest their stream hashes to.
///
/// Only the sorted entity identities stay resident; readers stream the rows
/// back in canonical order.
pub struct SpilledGraphGeneration {
    identity: GraphGenerationManifestIdentity,
    directory: SpillDirectory,
    entity_identities: Vec<GraphEntityId>,
    relation_count: usize,
    expected_recovered_digest: GraphRecoveredGenerationDigestV1,
    row_sum: GraphRowDigestSum,
}

impl std::fmt::Debug for SpilledGraphGeneration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SpilledGraphGeneration")
            .field("generation", &self.identity.generation)
            .field("entities", &self.entity_identities.len())
            .field("relations", &self.relation_count)
            .field("expected_recovered_digest", &self.expected_recovered_digest)
            .finish_non_exhaustive()
    }
}

impl SpilledGraphGeneration {
    #[must_use]
    pub fn identity(&self) -> GraphGenerationManifestIdentity {
        self.identity.clone()
    }

    /// `(entities, relations)` row counts.
    #[must_use]
    pub fn row_counts(&self) -> (usize, usize) {
        (self.entity_identities.len(), self.relation_count)
    }

    #[must_use]
    pub fn expected_recovered_digest(&self) -> &GraphRecoveredGenerationDigestV1 {
        &self.expected_recovered_digest
    }

    /// The order-independent row half of [`Self::expected_recovered_digest`].
    #[must_use]
    pub fn row_sum(&self) -> GraphRowDigestSum {
        self.row_sum
    }

    /// The producer's attachment, when it wrote one before the spill finished.
    pub(crate) fn attachment(&self) -> Option<PathBuf> {
        let path = self.directory.path().join(ATTACHMENT_FILE);
        path.is_file().then_some(path)
    }

    /// The row index sealed beside an attachment; see `row_index`.
    pub(crate) fn row_index(&self) -> Option<PathBuf> {
        let path = self.directory.path().join(ROW_INDEX_FILE);
        path.is_file().then_some(path)
    }

    /// Canonical entity rows and their identity-order offsets, retained only
    /// for a generation carrying a producer attachment.
    pub(crate) fn entity_rows(&self) -> Option<(PathBuf, PathBuf)> {
        let rows = self.directory.path().join(ENTITIES_FILE);
        let offsets = self.directory.path().join(ENTITY_ROW_OFFSETS_FILE);
        (rows.is_file() && offsets.is_file()).then_some((rows, offsets))
    }

    pub(crate) fn directory(&self) -> &Path {
        self.directory.path()
    }

    /// The canonical relation rows' identities, in canonical order.
    pub(crate) fn relation_identities(&self) -> Result<Vec<crate::GraphRelationId>, GraphDbError> {
        self.relations()?
            .map(|row| row.map(|(relation, _)| relation.identity))
            .collect()
    }

    pub(crate) fn entity_identities(&self) -> &[GraphEntityId] {
        &self.entity_identities
    }

    /// The position of `identity` in the canonical entity order.
    pub(crate) fn entity_index(&self, identity: &str) -> Option<usize> {
        self.entity_identities
            .binary_search_by(|entity| entity.as_str().cmp(identity))
            .ok()
    }

    pub(crate) fn entities(&self) -> Result<SpilledRows<GraphEntity>, GraphDbError> {
        SpilledRows::open(&self.directory.path().join(ENTITIES_FILE))
    }

    pub(crate) fn relations(&self) -> Result<SpilledRows<GraphGenerationRelation>, GraphDbError> {
        SpilledRows::open(&self.directory.path().join(RELATIONS_FILE))
    }

    /// The whole generation as an in-memory manifest, for the staging lane
    /// that has no sealed store to stream into.
    pub fn materialize(
        &self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphGenerationManifest, GraphDbError> {
        let entities = self
            .entities()?
            .map(|row| row.map(|(value, _)| value))
            .collect::<Result<Vec<_>, _>>()?;
        let relations = self
            .relations()?
            .map(|row| row.map(|(value, _)| value))
            .collect::<Result<Vec<_>, _>>()?;
        GraphGenerationManifest::new_checked(
            self.identity.projection.clone(),
            self.identity.generation.clone(),
            self.identity.source_generation.clone(),
            self.identity.watermark.clone(),
            Vec::new(),
            entities,
            relations,
            check,
        )
    }
}

pub(crate) fn read_spilled_entity(
    rows: &Path,
    offset: u64,
    length: u64,
    expected_identity: &GraphEntityId,
    expected_lanes: super::RowLanes,
) -> Result<GraphEntity, GraphDbError> {
    let mut file = File::open(rows).map_err(|error| spill_io("entity rows open", error))?;
    let rows_bytes = file
        .metadata()
        .map_err(|error| spill_io("entity rows metadata", error))?
        .len();
    if offset
        .checked_add(length)
        .is_none_or(|end| end > rows_bytes)
    {
        return Err(GraphDbError::Corrupt {
            message: "graph entity row offset exceeds its row file".to_owned(),
        });
    }
    let length = usize::try_from(length)
        .map_err(|_| GraphDbError::unavailable("graph entity row length exceeds address space"))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| spill_io("entity rows seek", error))?;
    let mut encoded = Vec::new();
    encoded.try_reserve_exact(length).map_err(|error| {
        GraphDbError::unavailable(format!("graph entity row cannot be allocated: {error}"))
    })?;
    encoded.resize(length, 0);
    file.read_exact(&mut encoded)
        .map_err(|error| spill_io("entity rows read", error))?;
    let mut cursor = encoded.as_slice();
    let row = SpillRow::read(&mut cursor)?.ok_or_else(|| GraphDbError::Corrupt {
        message: "graph entity row offset points at end of file".to_owned(),
    })?;
    if !cursor.is_empty() || !row.endpoints.iter().all(String::is_empty) {
        return Err(GraphDbError::Corrupt {
            message: "graph entity row offset does not cover one entity row".to_owned(),
        });
    }
    if row.identity != expected_identity.as_str()
        || row_frame_lanes("entity", &row.canonical)? != expected_lanes
    {
        return Err(GraphDbError::Corrupt {
            message: "graph entity row does not match its row index".to_owned(),
        });
    }
    let entity: GraphEntity =
        serde_json::from_slice(&row.canonical).map_err(|error| GraphDbError::Corrupt {
            message: format!("graph entity row is not canonical entity JSON: {error}"),
        })?;
    entity.validate()?;
    if entity.identity != *expected_identity
        || canonical_row(&entity, "recovered generation entity", &|| Ok(()))? != row.canonical
    {
        return Err(GraphDbError::Corrupt {
            message: "graph entity row canonical payload does not match its identity".to_owned(),
        });
    }
    Ok(entity)
}

/// Decoded rows of one merged spill file, in canonical order, each with the
/// endpoint identities it was spilled with.
pub(crate) struct SpilledRows<T> {
    reader: BufReader<File>,
    decoded: std::marker::PhantomData<T>,
}

impl<T> SpilledRows<T> {
    fn open(path: &Path) -> Result<Self, GraphDbError> {
        let file = File::open(path).map_err(|error| spill_io("merged open", error))?;
        Ok(Self {
            reader: BufReader::with_capacity(SPILL_IO_BUFFER_BYTES, file),
            decoded: std::marker::PhantomData,
        })
    }
}

impl<T: DeserializeOwned> Iterator for SpilledRows<T> {
    type Item = Result<(T, [String; 2]), GraphDbError>;

    fn next(&mut self) -> Option<Self::Item> {
        match SpillRow::read(&mut self.reader) {
            Ok(Some(row)) => Some(
                serde_json::from_slice(&row.canonical)
                    .map(|value| (value, row.endpoints))
                    .map_err(|error| GraphDbError::Corrupt {
                        message: format!("graph row spill holds an undecodable row: {error}"),
                    }),
            ),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        }
    }
}

/// A generation's rows as a publisher hands them to the registry: an
/// in-memory manifest, rows spilled to disk by a batch producer, or a delta
/// over a sealed base.
#[derive(Clone, Debug)]
pub enum GraphGenerationRows {
    Manifest(Arc<GraphGenerationManifest>),
    Spilled(Arc<SpilledGraphGeneration>),
    Layered(Arc<crate::LayeredGraphGeneration>),
}

impl From<Arc<GraphGenerationManifest>> for GraphGenerationRows {
    fn from(manifest: Arc<GraphGenerationManifest>) -> Self {
        Self::Manifest(manifest)
    }
}

impl From<GraphGenerationManifest> for GraphGenerationRows {
    fn from(manifest: GraphGenerationManifest) -> Self {
        Self::Manifest(Arc::new(manifest))
    }
}

impl From<SpilledGraphGeneration> for GraphGenerationRows {
    fn from(spilled: SpilledGraphGeneration) -> Self {
        Self::Spilled(Arc::new(spilled))
    }
}

impl From<crate::LayeredGraphGeneration> for GraphGenerationRows {
    fn from(layered: crate::LayeredGraphGeneration) -> Self {
        Self::Layered(Arc::new(layered))
    }
}

impl GraphGenerationRows {
    #[must_use]
    pub fn identity(&self) -> GraphGenerationManifestIdentity {
        match self {
            Self::Manifest(manifest) => manifest.identity(),
            Self::Spilled(spilled) => spilled.identity(),
            Self::Layered(layered) => layered.identity(),
        }
    }

    #[must_use]
    pub fn row_counts(&self) -> (usize, usize) {
        match self {
            Self::Manifest(manifest) => manifest.row_counts(),
            Self::Spilled(spilled) => spilled.row_counts(),
            Self::Layered(layered) => layered.row_counts(),
        }
    }

    pub fn expected_recovered_digest(
        &self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphRecoveredGenerationDigestV1, GraphDbError> {
        match self {
            Self::Manifest(manifest) => manifest.expected_recovered_digest(check),
            Self::Spilled(spilled) => Ok(spilled.expected_recovered_digest.clone()),
            Self::Layered(layered) => Ok(layered.expected_recovered_digest().clone()),
        }
    }

    /// The dependency-closure digest, memoized on the rows' own instance.
    pub fn dependency_closure_digest(
        &self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphDependencyGenerationClosureDigestV1, GraphDbError> {
        match self {
            Self::Manifest(manifest) => manifest.dependency_closure_digest(check),
            Self::Spilled(spilled) => spilled.identity.dependency_closure_digest(check),
            Self::Layered(layered) => layered.identity().dependency_closure_digest(check),
        }
    }

    /// The in-memory manifest, materializing spilled rows. Only the staging
    /// lane, which has no sealed store to stream into, pays this.
    pub(crate) fn into_manifest(
        self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Arc<GraphGenerationManifest>, GraphDbError> {
        match self {
            Self::Manifest(manifest) => Ok(manifest),
            Self::Spilled(spilled) => spilled.materialize(check).map(Arc::new),
            Self::Layered(_) => Err(GraphDbError::invalid(
                "a layered generation seals only over its sealed base",
            )),
        }
    }

    /// The journaled replay of a sealed code generation publishing these rows.
    pub fn relational_sealed_replay(
        &self,
        shard_id: StoreShardIdV1,
        idempotency_key: GraphIdempotencyKey,
        input_digest: GraphPublicationInputDigestV1,
        expected_prior_head: Option<GraphVerifiedHeadV1>,
        source: SealedCodeGenerationReplay,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphPublicationReplayV1, GraphDbError> {
        match self {
            Self::Manifest(manifest) => manifest.relational_sealed_replay(
                shard_id,
                idempotency_key,
                input_digest,
                expected_prior_head,
                source,
                check,
            ),
            Self::Spilled(_) | Self::Layered(_) => {
                validate_sealed_replay(&source)?;
                let payload = checked_canonical_bytes(
                    &GraphGenerationReplaySource::SealedCodeGeneration(source),
                    check,
                    "canonical graph generation replay",
                    MAX_GRAPH_REPLAY_SOURCE_BYTES_V1,
                )?;
                self.identity().relational_replay_with_payload(
                    shard_id,
                    idempotency_key,
                    input_digest,
                    expected_prior_head,
                    self.expected_recovered_digest(check)?,
                    payload,
                    check,
                )
            }
        }
    }
}

impl GraphGenerationManifestIdentity {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn relational_replay_with_payload(
        &self,
        shard_id: StoreShardIdV1,
        idempotency_key: GraphIdempotencyKey,
        input_digest: GraphPublicationInputDigestV1,
        expected_prior_head: Option<GraphVerifiedHeadV1>,
        expected_recovered_digest: GraphRecoveredGenerationDigestV1,
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
        let direct_dependencies =
            relational_dependency_generations(&self.dependencies, &projection.shard_id)?;
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
            expected_recovered_digest,
            payload,
        )
        .map_err(|error| GraphDbError::invalid(error.to_string()))
    }
}
