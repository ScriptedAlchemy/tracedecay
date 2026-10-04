//! Per-generation sealed compact stores.
//!
//! Grafeo's columnar `CompactStore` is immutable per *store*, while TraceDecay
//! immutability is per *generation*: one staging database holds every
//! generation in physical namespaces, so compacting it at a seal would freeze
//! the store the next generation stages into. This module aligns the two
//! scopes instead of fighting them: at seal time the just-verified
//! generation's rows are streamed from the staging database straight into a
//! `CompactStore` of their own (`IncrementalCompactStoreBuilder`), written as
//! a single-generation Grafeo container in one durable pass
//! (`GrafeoDB::write_compact_container`), reopened read-only, and proven
//! against the generation's recovered digest before it serves a single read.
//! No live LPG, sidecar WAL, or second checkpoint stands between the verified
//! rows and the sealed bytes.
//!
//! Every sealed store is digest-verified after durable reopen before
//! installation. Once a dependency-free generation's relational head is
//! seated, that immutable store is its serving authority and the duplicate
//! staging rows may be released; losing the artifact then requires canonical
//! republishing instead of falling back to absent staging rows. Dependency-
//! bearing generations retain staging because their edges cross physical
//! namespaces. Before release, and for configurations without an installed
//! sealed store, the WAL-backed staging database remains authoritative.
//! Retirement deletes the artifact directory with the generation; quarantine
//! discards it. Nothing ever writes to a sealed store after compaction, the
//! handle is marked read-only and refuses writes with a typed error.
//!
//! On-disk layout, next to the staging database file:
//!
//! ```text
//! graph.grafeo                  <- mutable staging database
//! graph.sealed/
//!   <physical-namespace-hex>/
//!     generation.grafeo         <- compact single-generation store
//!     sealed.json               <- receipt binding the recovered digest
//!     sealed.checked            <- written only after post-reopen proof
//! ```
//!
//! Each `<physical-namespace-hex>/` directory is a self-describing,
//! digest-bound, immutable artifact: the directory plus `sealed.json`
//! (identity, physical namespace, recovered digest, row counts, and the
//! `form` the store was sealed in) can be adopted on any host through
//! [`GraphDb::open_sealed_generation_store_if_present`], which re-proves the
//! digest before the store serves a read.

use std::collections::{BTreeMap, HashMap};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use grafeo_common::types::{EdgeId, NodeId, PropertyKey, Value};
use grafeo_core::graph::compact::IncrementalCompactStoreBuilder;
use grafeo_engine::GrafeoDB;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tracedecay_store::runtime::GraphRecoveredGenerationDigestV1;

use crate::generation::{
    GraphRowDigestSum, physical_namespace_projection_map, recovered_digest_from_row_sum,
    verify_sealed_copy_generation,
};
use crate::lease::GenerationLocator;
use crate::location::PersistentGraphStoreState;
use crate::schema::{
    FINAL_SCHEMA, FORMAT_LABEL, FORMAT_VERSION_PROPERTY, INDEXED_PROPERTIES, PROJECTION_LABEL,
    SCHEMA_PROPERTY, SEQUENCE_PROPERTY, decode_entity, edge_properties, entity_labels,
    entity_properties, projection_properties, relation_locator_labels, relation_properties,
    relation_type_for_kind,
};
use crate::sealed_layer::{
    GraphLayeredRowSpill, GraphSealedBaseAbsenceV1, GraphSealedBaseV1, LayeredGraphGeneration,
    LayeredReads, SealedBaseFilesV1, SealedBaseReceiptV1, SealedLayer,
};
use crate::state::{
    EndpointIdentityCache, latest_projection, load_relation_by_locator_cached,
    projection_entity_nodes_sorted_checked, projection_relation_nodes_sorted_checked,
};
use crate::{
    GraphCommit, GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability,
    GraphEntity, GraphFormatVersion, GraphGenerationManifest, GraphGenerationManifestIdentity,
    GraphGenerationRowSpill, GraphGenerationRows, GraphNamespace, GraphProjectionId,
    GraphProjectionIdentity, GraphRelation, GraphWatermark, GraphWriteBatch, NeverCancelled,
    SourceGeneration, SpilledGraphGeneration,
};

/// A sealed generation opened without the shared staging database.
pub(crate) struct DirectSealedGenerationV1 {
    pub(crate) lease: crate::GraphDbLeaseV1,
    pub(crate) identity: GraphGenerationManifestIdentity,
    /// A layered generation's store, whose delta `lease` holds.
    pub(crate) layered: Option<Arc<SealedGenerationStore>>,
}

/// Opens and verifies one dependency-free sealed generation without opening
/// the shared mutable staging database.
#[tracing::instrument(name = "graph_db.sealed_store.open_direct", level = "trace", skip_all)]
pub(crate) fn open_direct_sealed_generation(
    database_path: &Path,
    projection: crate::GraphProjectionIdentity,
    generation: crate::GraphGenerationId,
    expected: &GraphRecoveredGenerationDigestV1,
    authority_lease: Arc<dyn tracedecay_store::RetainedGraphStoreLeaseV1>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Option<DirectSealedGenerationV1>, GraphDbError> {
    if sealed_store_disabled() {
        return Ok(None);
    }
    let locator = GenerationLocator::new(projection.clone(), generation.clone());
    let physical_namespace = locator.physical_namespace()?;
    let directory =
        sealed_generation_directory(&sealed_store_root(database_path), &physical_namespace);
    let receipt_path = directory.join(SEALED_STORE_RECEIPT_FILE);
    let sealed_path = directory.join(SEALED_STORE_DATABASE_FILE);
    let receipt_bytes = match std::fs::read(&receipt_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(sealed_store_io_failure("receipt read failed", error)),
    };
    let receipt: SealedStoreReceiptV1 = serde_json::from_slice(&receipt_bytes)
        .map_err(|error| GraphDbError::unavailable(format!("sealed receipt decode: {error}")))?;
    if receipt.version != SEALED_STORE_RECEIPT_VERSION
        || receipt.graph_format != GraphFormatVersion::current().get()
        || receipt.recovered_digest != expected.as_str()
        || receipt.physical_namespace != physical_namespace.as_str()
        || receipt.namespace != projection.namespace.as_str()
        || receipt.projection != projection.projection.as_str()
        || receipt.generation != generation.as_str()
    {
        return Err(GraphDbError::unavailable(
            "sealed generation store receipt does not bind this generation".to_owned(),
        ));
    }
    // Lazily, for the same reason as `open_sealed_store`, and additionally so
    // the owner's lease-drop hibernation applies: an eagerly opened handle
    // has no lazy store state, so `hibernate_if_lazy` was a no-op and this
    // direct sealed serving path retained its whole graph past its last
    // lease. The identity read below reopens it immediately; the release
    // happens when the last operation lease goes away.
    let database = GraphDb::open_lazy_with_store_state(
        sealed_artifact_database_options(sealed_path),
        PersistentGraphStoreState::Existing,
    )
    .map_err(|error| match error {
        error @ (GraphDbError::ProjectionMismatch { .. }
        | GraphDbError::GenerationMismatch { .. }) => error,
        error => sealed_store_failure("reopen failed", error),
    })?;
    let identity = {
        let recovered = database
            .read_intact(&NeverCancelled, |native| {
                latest_projection(native, &physical_namespace, &projection.projection)
            })?
            .ok_or_else(|| GraphDbError::GenerationMismatch {
                namespace: projection.namespace.to_string(),
                projection: projection.projection.to_string(),
                generation: generation.to_string(),
                message: "sealed generation is missing its projection commit".to_owned(),
            })?;
        GraphGenerationManifestIdentity::new(
            projection,
            generation,
            recovered.commit.source_generation,
            recovered.commit.watermark,
            Vec::new(),
        )
    };
    if receipt.base.is_some() {
        // A layered store serves through its two engines; the direct
        // snapshot routes its head reads through the proven store.
        let _ = database.close();
        let store = open_sealed_store_checked(&directory, &identity, expected, check, None)?
            .ok_or_else(|| GraphDbError::unavailable("layered sealed generation is absent"))?;
        let lease =
            crate::owner::issue_derived_read_lease(Arc::clone(&store.database), authority_lease)?;
        return Ok(Some(DirectSealedGenerationV1 {
            lease,
            identity,
            layered: Some(store),
        }));
    }
    // Same marker-aware proof as registry adoption: a boot that reopens the
    // exact bytes an earlier open already proved resolves by marker, and a
    // fresh or changed container pays the full row proof and files the marker.
    if let Err(error) = sealed_copy_proof(&database, &identity, expected, check) {
        let _ = database.close();
        return Err(match error {
            error @ (GraphDbError::ProjectionMismatch { .. }
            | GraphDbError::GenerationMismatch { .. }) => error,
            error => sealed_store_failure("post-reopen verification failed", error),
        });
    }
    database.mark_sealed_read_only();
    let lease = crate::owner::issue_derived_read_lease(database, authority_lease)?;
    Ok(Some(DirectSealedGenerationV1 {
        lease,
        identity,
        layered: None,
    }))
}

/// Rows loaded from the shared staging database per read-guard hold while a
/// sealed build streams. Small enough that a queued writer (and the readers
/// that pile up behind it under `std::sync::RwLock`'s writer preference)
/// waits milliseconds, large enough that lock churn stays negligible against
/// row decode cost.
const SEALED_COPY_GUARD_CHUNK_ROWS: usize = 4096;

const SEALED_STORE_RECEIPT_VERSION: u32 = 1;
pub(crate) const SEALED_STORE_DATABASE_FILE: &str = "generation.grafeo";
const SEALED_STORE_RECEIPT_FILE: &str = "sealed.json";
/// Digest recorded only after a successful post-reopen proof. A `sealed.json`
/// written before that proof is not release authority.
const SEALED_STORE_CHECKED_FILE: &str = "sealed.checked";
const SEALED_STORE_DISABLE_ENV: &str = "TRACEDECAY_GRAPH_SEALED_STORE";

/// The one form a sealed store is built in. Every value TraceDecay persists
/// round-trips through the columnar codecs: scalars natively and Bytes through
/// the dictionary's marked entries. The post-reopen digest proof re-checks
/// every row regardless.
const SEALED_STORE_FORM_COMPACT: &str = "compact";
/// A delta container over a hard-linked cold base; see [`crate::sealed_layer`].
const SEALED_STORE_FORM_LAYERED: &str = "layered";

/// Receipt binding a sealed store directory to the exact generation and
/// recovered digest it was built from. Written after the compact container
/// is durably on disk; an open that finds a receipt for a different digest
/// discards the artifact instead of serving it.
#[derive(Debug, Deserialize, Serialize)]
struct SealedStoreReceiptV1 {
    version: u32,
    /// Always [`SEALED_STORE_FORM_COMPACT`] for stores this revision builds;
    /// receipts written by earlier revisions may name a replay-form store,
    /// which reopens and proves exactly like a compact one.
    form: String,
    namespace: String,
    projection: String,
    generation: String,
    physical_namespace: String,
    recovered_digest: String,
    entities: usize,
    relations: usize,
    /// The graph format the container was written in. A superseded-format
    /// rebuild discards every sealed generation that does not name the
    /// current format and keeps the ones this build already sealed.
    graph_format: u32,
    /// The generation's row sum. Only a spilled flat build records one, and
    /// only a store that records one can serve as a layered base.
    row_sum: Option<String>,
    /// The cold base a [`SEALED_STORE_FORM_LAYERED`] store's delta shadows.
    base: Option<SealedBaseReceiptV1>,
    /// The generation's source generation and watermark, which a refresh
    /// layering over it names without opening its container.
    source_generation: Option<String>,
    watermark: Option<String>,
}

impl SealedStoreReceiptV1 {
    fn binds(
        &self,
        locator: &GenerationLocator,
        physical_namespace: &str,
        expected_digest: &str,
    ) -> bool {
        self.version == SEALED_STORE_RECEIPT_VERSION
            && self.graph_format == GraphFormatVersion::current().get()
            && self.recovered_digest == expected_digest
            && self.physical_namespace == physical_namespace
            && self.namespace == locator.projection.namespace.as_str()
            && self.projection == locator.projection.projection.as_str()
            && self.generation == locator.generation.as_str()
    }
}

/// Digest and row counts that authorize a staging-row release without opening
/// the sealed engine.
pub(crate) struct SealedReleaseEvidence {
    pub recovered_digest: String,
    pub entities: usize,
    pub relations: usize,
}

/// How [`GraphDb::ensure_sealed_generation_store`] satisfied a publication's
/// request for a sealed per-generation store.
pub(crate) enum SealedStoreInstall {
    /// The sealed-store lane cannot serve this database (kill-switch set,
    /// memory-backed, or no reopen configuration); publication keeps the
    /// staging close/reopen proof.
    Unavailable,
    /// A digest-verified sealed store is installed for reads.
    ///
    /// `staging_proof` carries the canonical byte count of the digest proof
    /// when this call *built* the artifact by enumerating the staging
    /// database's rows: that enumeration plus the matching post-reopen digest
    /// is the evidence that the staging container serves exactly the
    /// authority's rows, so the caller may file a verify-once marker against
    /// it. An adopted pre-existing artifact proves only itself, its rows
    /// were never read out of this container, so it carries `None` and the
    /// staging container earns its marker the next time a full proof runs.
    Installed { staging_proof: Option<u64> },
}

/// Retained sealed generation readers and how much of that retention is
/// currently materialized as a native engine.
///
/// `retained` counts identities this database can serve without touching the
/// staging container; `resident` counts the subset that is actually holding a
/// grafeo store in RAM right now. The gap between them is the point of
/// hibernation, and the pair is what makes a pressure decision falsifiable.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SealedGenerationCensusV1 {
    pub(crate) retained: usize,
    pub(crate) resident: usize,
    pub(crate) retained_canonical_bytes: u64,
    pub(crate) resident_canonical_bytes: u64,
}

/// A reopened, digest-verified, compacted single-generation store.
pub(crate) struct SealedGenerationStore {
    locator: GenerationLocator,
    identity: GraphGenerationManifestIdentity,
    recovered_digest: String,
    entity_count: usize,
    relation_count: usize,
    /// Canonical bytes hashed by the post-reopen digest proof, the size of
    /// the exact row stream `recovered_digest` covers.
    canonical_bytes: u64,
    directory: PathBuf,
    database: Arc<GraphDb>,
    row_sum: Option<GraphRowDigestSum>,
    /// Set for a layered store: `database` holds the delta, this the base.
    layer: Option<SealedLayer>,
}

impl std::fmt::Debug for SealedGenerationStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SealedGenerationStore")
            .field("locator", &self.locator)
            .field("directory", &self.directory)
            .finish_non_exhaustive()
    }
}

impl SealedGenerationStore {
    /// The read-only compacted database serving this generation's reads.
    pub(crate) fn database(&self) -> &Arc<GraphDb> {
        &self.database
    }

    pub(crate) fn recovered_digest(&self) -> &str {
        &self.recovered_digest
    }

    pub(crate) fn row_counts(&self) -> (usize, usize) {
        (self.entity_count, self.relation_count)
    }

    /// Canonical bytes the post-reopen digest proof covered, the served
    /// index size this generation is retained for.
    pub(crate) fn canonical_bytes(&self) -> u64 {
        self.canonical_bytes
    }

    /// Whether this reader's native engine is currently materialized.
    ///
    /// A retained sealed generation with no resident engine costs its
    /// identity and nothing else; this is the falsifiable form of that claim.
    pub(crate) fn engine_resident(&self) -> bool {
        self.engines()
            .any(|database| database.native_engine_open().unwrap_or(false))
    }

    /// The engines this store serves from: its container, and a layered
    /// store's base.
    fn engines(&self) -> impl Iterator<Item = &Arc<GraphDb>> {
        std::iter::once(&self.database).chain(self.layer.as_ref().map(SealedLayer::base_engine))
    }

    pub(crate) fn close(&self) -> Result<(), GraphDbError> {
        let mut result = Ok(());
        for database in self.engines() {
            if let Err(error) = database.close() {
                result = Err(error);
            }
        }
        result
    }

    pub(crate) fn hibernate_if_lazy(&self) -> Result<(), GraphDbError> {
        for database in self.engines() {
            database.hibernate_if_lazy()?;
        }
        Ok(())
    }

    pub(crate) fn hibernate_if_lazy_when_idle(&self) -> Result<bool, GraphDbError> {
        let mut released = true;
        for database in self.engines() {
            released &= database.hibernate_if_lazy_when_idle()?;
        }
        Ok(released)
    }

    pub(crate) fn native_engine_open(&self) -> Result<bool, GraphDbError> {
        self.database.native_engine_open()
    }

    pub(crate) fn resident_engine_bytes(&self) -> Result<Option<u64>, GraphDbError> {
        let mut total = None;
        for database in self.engines() {
            if let Some(bytes) = database.resident_engine_bytes()? {
                total = Some(total.unwrap_or(0_u64).saturating_add(bytes));
            }
        }
        Ok(total)
    }

    pub(crate) fn pin_serving_engine(&self) -> Result<crate::GraphServingEnginePin, GraphDbError> {
        let pin = self.database.pin_serving_engine()?;
        match &self.layer {
            Some(layer) => Ok(pin.with_companion(layer.proven_base()?.pin_serving_engine()?)),
            None => Ok(pin),
        }
    }

    /// This store's layered reads, when it is a layered store.
    pub(crate) fn layered_reads(&self) -> Option<LayeredReads<'_>> {
        let layer = self.layer.as_ref()?;
        Some(LayeredReads {
            delta: &self.database,
            layer,
            namespace: self.locator.physical_namespace().ok()?,
            projection: &self.locator.projection,
        })
    }

    /// The cold base a refresh of this generation layers over: this store
    /// itself when it is a spilled cold build, or its own base when layered.
    fn sealed_base(
        &self,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Result<GraphSealedBaseV1, GraphSealedBaseAbsenceV1>, GraphDbError> {
        if let Some(layer) = &self.layer {
            let receipt = &layer.base_receipt;
            let files = match SealedBaseFilesV1::layered(&self.directory) {
                Ok(files) => files,
                Err(absence) => return Ok(Err(absence)),
            };
            return GraphSealedBaseV1::open(
                receipt.identity(&self.locator.projection)?,
                receipt.recovered_digest.clone(),
                (receipt.entities, receipt.relations),
                GraphRowDigestSum::from_hex(&receipt.row_sum)?,
                files,
                check,
            );
        }
        let Some(row_sum) = self.row_sum else {
            return Ok(Err(GraphSealedBaseAbsenceV1::NoRowSum));
        };
        let files = match SealedBaseFilesV1::flat(&self.directory) {
            Ok(files) => files,
            Err(absence) => return Ok(Err(absence)),
        };
        GraphSealedBaseV1::open(
            self.identity.clone(),
            self.recovered_digest.clone(),
            (self.entity_count, self.relation_count),
            row_sum,
            files,
            check,
        )
    }

    /// Best-effort teardown used only when the generation is quarantined or
    /// retired, or before staging rows become releasable. A sealed-only head
    /// is never discarded through this path while it remains serveable.
    fn discard(&self) {
        let _ = self.close();
        remove_sealed_directory(&self.directory);
    }
}

/// Whether the sealed-store lane is disabled through the environment.
///
/// Sealed stores are on by default: `TRACEDECAY_GRAPH_SEALED_STORE=off`
/// (or `0`/`false`/`disabled`) is the operational kill-switch.
fn sealed_store_disabled() -> bool {
    match std::env::var(SEALED_STORE_DISABLE_ENV) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false" | "disabled"
        ),
        Err(_) => false,
    }
}

/// `/var/db/graph.grafeo` -> `/var/db/graph.sealed`.
fn sealed_store_root(database_path: &Path) -> PathBuf {
    database_path.with_extension("sealed")
}

fn sealed_generation_directory(root: &Path, physical_namespace: &GraphNamespace) -> PathBuf {
    // `generation:<64 hex>` -> `<64 hex>`: the digest is filesystem-safe.
    let name = physical_namespace
        .as_str()
        .strip_prefix("generation:")
        .unwrap_or(physical_namespace.as_str());
    root.join(name)
}

/// Byte census of a store's sealed root, split by what serves and what does
/// not. The Doctor storage finding reports the two dead classes; a count in
/// either means bytes nothing reads are sitting next to the live store.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SealedStoreCensusV1 {
    /// Sealed generations whose `generation` is a verified head.
    pub head_count: u64,
    pub head_bytes: u64,
    /// Sealed generations no verified head names: superseded artifacts whose
    /// retirement has not run since the last publication.
    pub superseded_count: u64,
    pub superseded_bytes: u64,
    /// `.staging-*` directories left by seals that never installed.
    pub abandoned_staging_count: u64,
    pub abandoned_staging_bytes: u64,
    /// Entries that are neither a readable sealed receipt nor staging: an
    /// unrecognized layout is reported, never silently classed as dead.
    pub unrecognized_count: u64,
}

/// Measure the sealed root beside `database_path` against the generation ids
/// of the store's current verified heads (as journaled: `<projection>:<digest>`).
///
/// Metadata only: every entry is one receipt read plus a directory size walk,
/// never a container open. An unreadable receipt counts as unrecognized so the
/// census stays truthful when a receipt is mid-write or corrupt.
pub fn census_sealed_store(
    database_path: &Path,
    head_generations: &std::collections::BTreeSet<String>,
) -> std::io::Result<SealedStoreCensusV1> {
    let root = sealed_store_root(database_path);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SealedStoreCensusV1::default());
        }
        Err(error) => return Err(error),
    };
    let mut census = SealedStoreCensusV1::default();
    let mut counted = std::collections::HashSet::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            census.unrecognized_count += 1;
            continue;
        }
        let path = entry.path();
        let bytes = directory_bytes(&path, &mut counted)?;
        let name = entry.file_name();
        if name
            .to_str()
            .is_some_and(|name| name.starts_with(".staging-"))
        {
            census.abandoned_staging_count += 1;
            census.abandoned_staging_bytes += bytes;
            continue;
        }
        let receipt = std::fs::read(path.join(SEALED_STORE_RECEIPT_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<SealedStoreReceiptV1>(&bytes).ok());
        let Some(receipt) = receipt else {
            census.unrecognized_count += 1;
            continue;
        };
        if head_generations.contains(&receipt.generation) {
            census.head_count += 1;
            census.head_bytes += bytes;
        } else {
            census.superseded_count += 1;
            census.superseded_bytes += bytes;
        }
    }
    Ok(census)
}

/// Bytes under `directory` not already counted in `counted`: a layered
/// generation's base container is a hard link to bytes another directory
/// already holds, so each file is charged once per census.
fn directory_bytes(
    directory: &Path,
    counted: &mut std::collections::HashSet<(u64, u64)>,
) -> std::io::Result<u64> {
    let mut total = 0u64;
    let mut pending = vec![directory.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                pending.push(entry.path());
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() > 1 && !counted.insert((metadata.dev(), metadata.ino())) {
                    continue;
                }
            }
            #[cfg(not(unix))]
            let _ = &counted;
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
}

/// Removes every sealed generation beside `database_path` whose receipt does
/// not name the current graph format: after a superseded-format rebuild those
/// containers can never serve, while a generation this build already sealed
/// keeps serving. In-flight `.staging-*` seals are left to their builder.
pub(crate) fn discard_superseded_sealed_generations(
    database_path: &Path,
) -> Result<(), GraphDbError> {
    let root = sealed_store_root(database_path);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(sealed_store_io_failure("sealed root read failed", error)),
    };
    for entry in entries {
        let entry =
            entry.map_err(|error| sealed_store_io_failure("sealed root read failed", error))?;
        let path = entry.path();
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(".staging-"))
        {
            continue;
        }
        let current = load_sealed_store_receipt(&path)
            .ok()
            .flatten()
            .is_some_and(|receipt| receipt.graph_format == GraphFormatVersion::current().get());
        if !current {
            match std::fs::remove_dir_all(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(sealed_store_io_failure(
                        "superseded sealed generation removal failed",
                        error,
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Removes every `.staging-*` directory a seal left behind under the store's
/// sealed root.
///
/// A seal builds into `.staging-<digest>` and installs by rename, so a staging
/// directory that survives to the next open of the same store belongs to a
/// build the process never finished, a crash, a kill, or an OOM between
/// container write and rename. Nothing reads it and the next seal of that
/// generation starts over, so on the live profile these accumulated to 7.4 GB
/// under one store. Run at open: the exclusive store lock means no seal of
/// this store is in flight.
pub(crate) fn sweep_abandoned_sealed_staging(database_path: &Path) {
    let root = sealed_store_root(database_path);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            tracing::warn!(
                event = "sealed_staging_sweep_unreadable",
                root = %root.display(),
                error = %error,
                "sealed store root could not be enumerated; abandoned staging stays"
            );
            return;
        }
    };
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(".staging-") {
            continue;
        }
        let path = entry.path();
        match std::fs::remove_dir_all(&path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(
                event = "sealed_staging_sweep_failed",
                path = %path.display(),
                error = %error,
                "abandoned sealed staging directory could not be removed"
            ),
        }
    }
    if removed > 0 {
        tracing::info!(
            event = "sealed_staging_swept",
            root = %root.display(),
            removed,
            "removed abandoned sealed staging directories left by interrupted seals"
        );
    }
}

/// Prefix of a batch producer's row spill under the sealed root.
const ROW_SPILL_PREFIX: &str = ".rows-";

/// Removes every row spill another process left under the store's sealed
/// root. Run at the eager open, whose exclusive store lock means no other
/// process is producing rows for this store. Spills named for this process
/// are skipped: a remount inside a live daemon may overlap its own publisher,
/// whose spill removes itself when it drops.
pub(crate) fn sweep_abandoned_row_spills(database_path: &Path) {
    let root = sealed_store_root(database_path);
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    let own = format!("{ROW_SPILL_PREFIX}{}-", std::process::id());
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(ROW_SPILL_PREFIX) && !name.starts_with(&own))
        {
            let path = entry.path();
            if let Err(error) = std::fs::remove_dir_all(&path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(
                    event = "graph_row_spill_sweep_failed",
                    path = %path.display(),
                    error = %error,
                    "abandoned graph row spill could not be removed"
                );
            }
        }
    }
}

fn remove_sealed_directory(directory: &Path) {
    match std::fs::remove_dir_all(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            // Retirement re-runs are idempotent; a leaked directory is
            // re-collected on the next retirement or rebuild of this
            // generation and never serves reads without a matching receipt.
        }
    }
}

/// Open options for a sealed artifact: read-only, so a reopen for proof,
/// adoption, or serving never re-serializes the immutable container on close
/// and never moves the identity its marker binds. A sealed container is only
/// ever opened this way, its bytes are written once, by
/// [`GrafeoDB::write_compact_container`], before any engine opens them.
fn sealed_artifact_database_options(path: PathBuf) -> GraphDbOpenOptions {
    GraphDbOpenOptions {
        location: GraphDbLocation::Persistent(path),
        expected_format: GraphFormatVersion::current(),
        durability: GraphDurability::SealedReadOnly,
        cancellation: Arc::new(NeverCancelled),
    }
}

fn sealed_store_failure(context: &str, error: GraphDbError) -> GraphDbError {
    GraphDbError::unavailable(format!("sealed generation store {context}: {error}"))
}

fn sealed_store_io_failure(context: &str, error: std::io::Error) -> GraphDbError {
    GraphDbError::unavailable(format!("sealed generation store {context}: {error}"))
}

/// Rows of one generation streamed into a columnar [`IncrementalCompactStoreBuilder`]
/// in exactly the node and edge shape `mutation::apply` gives a live store:
/// entity nodes, one native edge plus one locator node per relation, a
/// projection-state node per written namespace, and the format marker.
///
/// Node and edge ids are dense and assigned in push order, so identical row
/// streams build byte-identical containers whichever [`SealedRowSource`]
/// supplied them. The caller keeps its own map from source row to the sealed
/// [`NodeId`] `push_entity` returns, which is how edges find their endpoints
/// without re-resolving identities through an index.
struct SealedCompactRows {
    builder: IncrementalCompactStoreBuilder,
    next_node: u64,
    next_edge: u64,
}

struct PreparedSealedNode {
    labels: Vec<String>,
    properties: Vec<(PropertyKey, Value)>,
}

struct PreparedSealedRelation {
    edge: EdgeId,
    edge_type: String,
    edge_properties: Vec<(PropertyKey, Value)>,
    locator: PreparedSealedNode,
}

impl SealedCompactRows {
    /// Spools pushed column values to an anonymous file in `staging`, so
    /// the build holds the row topology rather than every value.
    fn new(staging: &Path) -> Result<Self, GraphDbError> {
        let spool = tempfile::tempfile_in(staging)
            .map_err(|error| sealed_store_io_failure("column spool", error))?;
        Ok(Self {
            builder: IncrementalCompactStoreBuilder::spooling_to(spool),
            next_node: 0,
            next_edge: 0,
        })
    }

    fn push_node(
        &mut self,
        labels: &[String],
        properties: Vec<(String, Value)>,
    ) -> Result<NodeId, GraphDbError> {
        self.push_prepared_node(Self::prepare_node(labels.to_vec(), properties))
    }

    fn prepare_node(labels: Vec<String>, properties: Vec<(String, Value)>) -> PreparedSealedNode {
        PreparedSealedNode {
            labels,
            properties: properties
                .into_iter()
                .map(|(key, value)| (PropertyKey::new(&key), value))
                .collect(),
        }
    }

    fn push_prepared_node(&mut self, prepared: PreparedSealedNode) -> Result<NodeId, GraphDbError> {
        let id = NodeId::new(self.next_node);
        self.builder
            .push_node(
                id,
                prepared.labels.iter().map(String::as_str),
                prepared.properties.iter().map(|(key, value)| (key, value)),
            )
            .map_err(|error| sealed_build_failure("node", error))?;
        self.next_node += 1;
        Ok(id)
    }

    /// Writes one entity as it is stored under `namespace`/`projection` and
    /// returns its sealed node so relations can reach it.
    fn push_entity(
        &mut self,
        namespace: &GraphNamespace,
        projection: &GraphProjectionId,
        entity: &GraphEntity,
    ) -> Result<NodeId, GraphDbError> {
        self.push_prepared_node(Self::prepare_entity(namespace, projection, entity))
    }

    fn prepare_entity(
        namespace: &GraphNamespace,
        projection: &GraphProjectionId,
        entity: &GraphEntity,
    ) -> PreparedSealedNode {
        Self::prepare_node(
            entity_labels(namespace, projection, &entity.labels),
            entity_properties(namespace, projection, entity),
        )
    }

    /// Writes one relation: the native edge between two already-written
    /// endpoints, then the locator node that names that edge.
    fn push_relation(
        &mut self,
        namespace: &GraphNamespace,
        projection: &GraphProjectionId,
        relation: &GraphRelation,
        from: NodeId,
        to: NodeId,
    ) -> Result<(), GraphDbError> {
        let edge = EdgeId::new(self.next_edge);
        let prepared = Self::prepare_relation(namespace, projection, relation, edge)?;
        self.push_prepared_relation(prepared, from, to)
    }

    fn prepare_relation(
        namespace: &GraphNamespace,
        projection: &GraphProjectionId,
        relation: &GraphRelation,
        edge: EdgeId,
    ) -> Result<PreparedSealedRelation, GraphDbError> {
        Ok(PreparedSealedRelation {
            edge,
            edge_type: relation_type_for_kind(&relation.kind),
            edge_properties: edge_properties(namespace, projection, relation)
                .into_iter()
                .map(|(key, value)| (PropertyKey::new(&key), value))
                .collect(),
            locator: Self::prepare_node(
                relation_locator_labels(namespace, projection),
                relation_properties(namespace, projection, relation, edge)?,
            ),
        })
    }

    fn push_prepared_relation(
        &mut self,
        prepared: PreparedSealedRelation,
        from: NodeId,
        to: NodeId,
    ) -> Result<(), GraphDbError> {
        if prepared.edge.as_u64() != self.next_edge {
            return Err(GraphDbError::Corrupt {
                message: "sealed relation preparation diverged from canonical order".to_owned(),
            });
        }
        self.builder
            .push_edge(
                prepared.edge,
                &prepared.edge_type,
                from,
                to,
                prepared
                    .edge_properties
                    .iter()
                    .map(|(key, value)| (key, value)),
            )
            .map_err(|error| sealed_build_failure("edge", error))?;
        self.next_edge += 1;
        self.push_prepared_node(prepared.locator)?;
        Ok(())
    }

    fn push_projection_commit(
        &mut self,
        namespace: &GraphNamespace,
        projection: &GraphProjectionId,
        commit: &GraphCommit,
    ) -> Result<(), GraphDbError> {
        let properties = projection_properties(namespace, projection, commit)?;
        self.push_node(&[PROJECTION_LABEL.to_owned()], properties)?;
        Ok(())
    }

    /// The format marker every open validates, carrying the store's final
    /// commit sequence exactly as the last `mutation::apply` would leave it.
    fn push_format_marker(&mut self, sequence: u64) -> Result<(), GraphDbError> {
        let sequence = i64::try_from(sequence)
            .map_err(|_| GraphDbError::unavailable("graph commit sequence exceeds i64"))?;
        let properties = vec![
            (
                FORMAT_VERSION_PROPERTY.to_owned(),
                Value::from(i64::from(GraphFormatVersion::current().get())),
            ),
            (SCHEMA_PROPERTY.to_owned(), Value::from(FINAL_SCHEMA)),
            (SEQUENCE_PROPERTY.to_owned(), Value::from(sequence)),
        ];
        self.push_node(&[FORMAT_LABEL.to_owned()], properties)?;
        Ok(())
    }

    /// Encodes every pushed row and writes the sealed container at `path`
    /// in one durable pass, one column at a time. The container holds the
    /// compact store, an empty LPG overlay whose id allocators start past
    /// the written ids, and a catalog naming the unique-key property
    /// indexes.
    fn write_container(self, path: &Path) -> Result<(), GraphDbError> {
        {
            let _span = tracing::trace_span!("code_index.seal.write.container").entered();
            GrafeoDB::write_compact_container(
                path,
                self.builder,
                INDEXED_PROPERTIES
                    .iter()
                    .map(|property| (*property).to_owned()),
            )
        }
        .map_err(|error| GraphDbError::unavailable(format!("sealed container write: {error}")))
    }
}

fn sealed_build_failure(
    what: &str,
    error: grafeo_core::graph::compact::builder::CompactStoreError,
) -> GraphDbError {
    GraphDbError::Corrupt {
        message: format!("sealed compact build refused a {what}: {error}"),
    }
}

fn collect_prepared_rows_ordered<T, R>(
    items: &[T],
    operation: impl Fn(usize, &T) -> Result<R, GraphDbError> + Send + Sync,
) -> Result<Vec<R>, GraphDbError>
where
    T: Sync,
    R: Send,
{
    const ROWS_PER_WORK_UNIT: usize = 512;
    if items.len() < 2 || rayon::current_thread_index().is_none() {
        return items
            .iter()
            .enumerate()
            .map(|(index, item)| operation(index, item))
            .collect();
    }
    let chunks = items
        .par_chunks(ROWS_PER_WORK_UNIT)
        .enumerate()
        .map(|(chunk_index, chunk)| {
            catch_unwind(AssertUnwindSafe(|| {
                chunk
                    .iter()
                    .enumerate()
                    .map(|(offset, item)| {
                        operation(
                            chunk_index
                                .saturating_mul(ROWS_PER_WORK_UNIT)
                                .saturating_add(offset),
                            item,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()
            }))
            .unwrap_or_else(|_| {
                Err(GraphDbError::unavailable(
                    "sealed row preparation worker panicked",
                ))
            })
        })
        .collect::<Vec<_>>();
    let mut collected = Vec::with_capacity(items.len());
    for chunk in chunks {
        collected.extend(chunk?);
    }
    Ok(collected)
}

/// The commit a sealed namespace's projection-state node records: the
/// generation's source and watermark, the canonical digest of the empty
/// finalization batch for that namespace (the same batch native staging
/// commits last), and, for the physical namespace only, the dependency
/// closure digest the recovered proof requires.
fn sealed_namespace_commit(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    identity: &GraphGenerationManifestIdentity,
    sequence: u64,
    dependency_digest: Option<tracedecay_store::runtime::GraphDependencyGenerationClosureDigestV1>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphCommit, GraphDbError> {
    let mut finalization = GraphWriteBatch::new_canonical_checked(
        namespace.clone(),
        projection.clone(),
        identity.source_generation.clone(),
        identity.watermark.clone(),
        Vec::new(),
        check,
    )?;
    Ok(GraphCommit {
        sequence,
        source_generation: identity.source_generation.clone(),
        watermark: identity.watermark.clone(),
        digest: finalization.validate_and_digest()?,
        generation_dependency_digest: dependency_digest,
    })
}

impl GraphDb {
    /// The typed refusal every post-compact write against a sealed
    /// generation receives, when this handle holds a sealed store for it.
    pub(crate) fn sealed_write_refusal(&self, locator: &GenerationLocator) -> Option<GraphDbError> {
        let sealed = self.inner.sealed_generations.read().ok()?;
        if sealed.contains_key(locator) {
            Some(GraphDbError::SealedStoreImmutable {
                message: format!(
                    "generation `{}/{}/{}` is sealed and compacted; its rows accept no further writes",
                    locator.projection.namespace, locator.projection.projection, locator.generation
                ),
            })
        } else {
            None
        }
    }

    /// The sealed compacted store for `locator`, when one is installed.
    pub(crate) fn sealed_generation_reader(
        &self,
        locator: &GenerationLocator,
    ) -> Option<Arc<SealedGenerationStore>> {
        let sealed = self.inner.sealed_generations.read().ok()?;
        sealed.get(locator).cloned()
    }

    /// Receipt evidence that `locator`'s on-disk sealed artifact matches
    /// `expected` digest, without opening the sealed engine.
    ///
    /// Staging-row release is cleanup, not serving. The relational head plus
    /// a post-reopen `sealed.checked` digest already name a proven artifact.
    /// A `sealed.json` written before that proof is not enough: a crash after
    /// the install rename and before proof would otherwise delete the only
    /// reconstructable staging rows. Serving and activation still prove
    /// before they read, and a successful proof persists the check.
    pub(crate) fn matching_sealed_release_receipt(
        &self,
        locator: &GenerationLocator,
        expected: &str,
    ) -> Result<Option<SealedReleaseEvidence>, GraphDbError> {
        if sealed_store_disabled() {
            return Ok(None);
        }
        let Some(reopen) = self.inner.reopen.as_ref() else {
            return Ok(None);
        };
        let Some(database_path) = reopen.config.path.clone() else {
            return Ok(None);
        };
        let physical_namespace = locator.physical_namespace()?;
        let directory =
            sealed_generation_directory(&sealed_store_root(&database_path), &physical_namespace);
        let Some(receipt) = load_sealed_store_receipt(&directory)? else {
            return Ok(None);
        };
        if !receipt.binds(locator, physical_namespace.as_str(), expected) {
            return Ok(None);
        }
        if !sealed_store_check_matches(&directory, expected)? {
            return Ok(None);
        }
        Ok(Some(SealedReleaseEvidence {
            recovered_digest: receipt.recovered_digest,
            entities: receipt.entities,
            relations: receipt.relations,
        }))
    }

    #[cfg(any(test, feature = "test-helpers", feature = "eval-helpers"))]
    pub fn discard_sealed_generation_reader(
        &self,
        identity: &GraphGenerationManifestIdentity,
    ) -> Result<(), GraphDbError> {
        let locator =
            GenerationLocator::new(identity.projection.clone(), identity.generation.clone());
        let removed = self
            .inner
            .sealed_generations
            .write()
            .map_err(|_| GraphDbError::unavailable("sealed generation store lock is poisoned"))?
            .remove(&locator);
        if let Some(store) = removed {
            store.close()?;
        }
        Ok(())
    }

    /// Ensures the sealed compacted store for `identity` exists, is
    /// digest-verified, and is installed for reads. Builds it from this
    /// staging database's verified rows when missing.
    ///
    /// Returns [`SealedStoreInstall::Installed`] when the exact
    /// post-reopen-verified artifact is installed. A memory-backed database
    /// and a disabled lane return [`SealedStoreInstall::Unavailable`]; their
    /// publication path retains the staging proof.
    #[tracing::instrument(name = "graph_db.sealed_store.ensure", level = "trace", skip_all)]
    pub(crate) fn ensure_sealed_generation_store(
        &self,
        identity: &GraphGenerationManifestIdentity,
        expected: &GraphRecoveredGenerationDigestV1,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<SealedStoreInstall, GraphDbError> {
        if sealed_store_disabled() {
            return Ok(SealedStoreInstall::Unavailable);
        }
        let Some(reopen) = self.inner.reopen.as_ref() else {
            return Ok(SealedStoreInstall::Unavailable);
        };
        let Some(database_path) = reopen.config.path.clone() else {
            return Ok(SealedStoreInstall::Unavailable);
        };
        let locator =
            GenerationLocator::new(identity.projection.clone(), identity.generation.clone());
        {
            let sealed = self.inner.sealed_generations.read().map_err(|_| {
                GraphDbError::unavailable("sealed generation store lock is poisoned")
            })?;
            if let Some(existing) = sealed.get(&locator)
                && existing.recovered_digest() == expected.as_str()
            {
                // An earlier ensure in this open installed it; if that call
                // built from staging rows it filed the marker then.
                return Ok(SealedStoreInstall::Installed {
                    staging_proof: None,
                });
            }
        }
        let (store, staging_proof) = build_or_open_sealed_store(
            SealedRowSource::Staging(self),
            identity,
            expected,
            &database_path,
            check,
        )?;
        self.install_sealed_generation_store(locator, store)?;
        Ok(SealedStoreInstall::Installed { staging_proof })
    }

    /// Seals a dependency-free generation straight from its verified
    /// manifest, without staging a row: the compact container is built from
    /// the manifest, written once, reopened read-only, proven against
    /// `expected`, and installed for reads. Returns the sealed generation's
    /// commit, read back from the proven artifact.
    ///
    /// Every failure boundary recovers from durable state that already
    /// exists. Cancellation or a crash before the artifact directory is
    /// renamed into place leaves no container (the next attempt clears the
    /// build directory and rebuilds from the replay journal's manifest); a
    /// crash after it leaves a complete, receipted artifact the next attempt
    /// adopts by digest. Nothing about this build is recoverable *only* from
    /// process memory. An artifact from an earlier seal of this exact
    /// generation is adopted without a build.
    ///
    /// `Ok(None)` means the sealed-store lane cannot serve this database
    /// (kill-switch set, memory-backed, or no reopen configuration), so the
    /// caller must stage and prove the generation the ordinary way.
    #[tracing::instrument(name = "graph_db.sealed_store.seal_direct", level = "trace", skip_all)]
    pub(crate) fn seal_generation_directly(
        &self,
        rows: &GraphGenerationRows,
        expected: &GraphRecoveredGenerationDigestV1,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Option<GraphCommit>, GraphDbError> {
        if sealed_store_disabled() {
            return Ok(None);
        }
        let Some(reopen) = self.inner.reopen.as_ref() else {
            return Ok(None);
        };
        let Some(database_path) = reopen.config.path.clone() else {
            return Ok(None);
        };
        check()?;
        let source = match rows {
            GraphGenerationRows::Manifest(manifest) => {
                manifest.validate_checked(check)?;
                SealedRowSource::Manifest(manifest)
            }
            GraphGenerationRows::Spilled(spilled) => SealedRowSource::Spilled(spilled),
            GraphGenerationRows::Layered(layered) => SealedRowSource::Layered(layered),
        };
        let identity = rows.identity();
        if !identity.dependencies.is_empty() {
            return Err(GraphDbError::invalid(
                "a direct sealed build requires a dependency-free generation",
            ));
        }
        let locator =
            GenerationLocator::new(identity.projection.clone(), identity.generation.clone());
        if let Some(existing) = self.sealed_generation_reader(&locator)
            && existing.recovered_digest() != expected.as_str()
            && let Some(refusal) = self.sealed_write_refusal(&locator)
        {
            // Different content is already sealed and serving under this
            // exact locator: the same immutability refusal a conflicting
            // restage gets, not a silent rebuild underneath its readers.
            return Err(refusal);
        }
        let (store, _) =
            build_or_open_sealed_store(source, &identity, expected, &database_path, check)?;
        self.install_sealed_generation_store(locator.clone(), store)?;
        // The generation normally exists only as this sealed artifact: it is
        // sealed-only from its first instant, and no lease remembered for it
        // may claim staging rows in the durable-row ledger. A database written
        // before generations sealed directly may still hold this generation's
        // rows beside the artifact; those stay in the ledger so the ordinary
        // release deletes them instead of leaking them under a sealed-only
        // claim. A hibernated staging engine is not opened to find out: the
        // generation stays in the ledger and the next release sweep, which
        // opens the engine once anyway, settles it from the durable rows.
        let staging_rows = match self.try_read_open_engine()? {
            Some(guard) => {
                let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
                Some(crate::state::projection_node_counts(
                    database,
                    &locator.physical_namespace()?,
                    &locator.projection.projection,
                )?)
            }
            None => None,
        };
        if staging_rows == Some((0, 0)) {
            let mut state = self.wait_verified_generations_write()?;
            state.stored.remove(&locator);
            state.sealed_only.insert(locator.clone());
        }
        check()?;
        let commit = self
            .generation_commit(&locator)?
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "sealed generation is missing its projection commit".to_owned(),
            })?;
        Ok(Some(commit))
    }

    /// A row spill for one generation of `projection`, scratch space under
    /// this store's sealed root. Registered stores are always persistent; a
    /// memory-backed database has no disk to spill to and refuses typed.
    pub(crate) fn generation_row_spill(
        &self,
        projection: GraphProjectionIdentity,
    ) -> Result<GraphGenerationRowSpill, GraphDbError> {
        GraphGenerationRowSpill::create(self.row_spill_directory()?, projection)
    }

    /// A fresh spill directory name under this store's sealed root.
    fn row_spill_directory(&self) -> Result<PathBuf, GraphDbError> {
        static NEXT_SPILL: AtomicU64 = AtomicU64::new(0);
        let database_path = self
            .inner
            .reopen
            .as_ref()
            .and_then(|reopen| reopen.config.path.as_deref())
            .ok_or_else(|| {
                GraphDbError::unavailable("a memory-backed graph store has no row spill root")
            })?;
        let root = sealed_store_root(database_path);
        std::fs::create_dir_all(&root)
            .map_err(|error| sealed_store_io_failure("row spill root create failed", error))?;
        Ok(root.join(format!(
            "{ROW_SPILL_PREFIX}{}-{}",
            std::process::id(),
            NEXT_SPILL.fetch_add(1, AtomicOrdering::Relaxed)
        )))
    }

    /// The cold base a refresh replacing `locator` may layer over: from its
    /// installed sealed store, or, when this process has not installed it
    /// (a head recovered straight from its artifact), from its proven
    /// artifact on disk.
    pub(crate) fn sealed_generation_base(
        &self,
        locator: &GenerationLocator,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Result<GraphSealedBaseV1, GraphSealedBaseAbsenceV1>, GraphDbError> {
        match self.sealed_generation_reader(locator) {
            Some(store) => store.sealed_base(check),
            None => self.sealed_base_from_disk(locator, check),
        }
    }

    /// A proven sealed artifact of `locator` on disk as a layered base. Its
    /// container is not opened: the refresh reads only its row index, bound
    /// here to the receipt's row sum, and a layer proves the container
    /// before its first base read.
    fn sealed_base_from_disk(
        &self,
        locator: &GenerationLocator,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Result<GraphSealedBaseV1, GraphSealedBaseAbsenceV1>, GraphDbError> {
        if sealed_store_disabled() {
            return Ok(Err(GraphSealedBaseAbsenceV1::SealedStoreUnavailable));
        }
        let Some(database_path) = self
            .inner
            .reopen
            .as_ref()
            .and_then(|reopen| reopen.config.path.clone())
        else {
            return Ok(Err(GraphSealedBaseAbsenceV1::SealedStoreUnavailable));
        };
        let physical_namespace = locator.physical_namespace()?;
        let directory =
            sealed_generation_directory(&sealed_store_root(&database_path), &physical_namespace);
        let Some(receipt) = load_sealed_store_receipt(&directory)? else {
            return Ok(Err(GraphSealedBaseAbsenceV1::NoArtifact));
        };
        if receipt.version != SEALED_STORE_RECEIPT_VERSION
            || receipt.graph_format != GraphFormatVersion::current().get()
            || receipt.physical_namespace != physical_namespace.as_str()
        {
            return Ok(Err(GraphSealedBaseAbsenceV1::SupersededArtifact));
        }
        if !sealed_store_check_matches(&directory, &receipt.recovered_digest)? {
            return Ok(Err(GraphSealedBaseAbsenceV1::UnprovenArtifact));
        }
        let projection = &locator.projection;
        let files = match &receipt.base {
            Some(_) => SealedBaseFilesV1::layered(&directory),
            None => SealedBaseFilesV1::flat(&directory),
        };
        let files = match files {
            Ok(files) => files,
            Err(absence) => return Ok(Err(absence)),
        };
        let (digest, counts, row_sum, identity) = match &receipt.base {
            Some(base) => (
                base.recovered_digest.clone(),
                (base.entities, base.relations),
                GraphRowDigestSum::from_hex(&base.row_sum)?,
                base.identity(projection)?,
            ),
            None => {
                let Some(row_sum) = receipt.row_sum.as_deref() else {
                    return Ok(Err(GraphSealedBaseAbsenceV1::NoRowSum));
                };
                let (Some(source_generation), Some(watermark)) =
                    (&receipt.source_generation, &receipt.watermark)
                else {
                    return Ok(Err(GraphSealedBaseAbsenceV1::SupersededArtifact));
                };
                (
                    receipt.recovered_digest.clone(),
                    (receipt.entities, receipt.relations),
                    GraphRowDigestSum::from_hex(row_sum)?,
                    GraphGenerationManifestIdentity::new(
                        projection.clone(),
                        locator.generation.clone(),
                        SourceGeneration::new(source_generation.clone())?,
                        GraphWatermark::new(watermark.clone())?,
                        Vec::new(),
                    ),
                )
            }
        };
        let expected = GraphRecoveredGenerationDigestV1::new(digest.clone())
            .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
        if recovered_digest_from_row_sum(&identity, row_sum, check)? != expected {
            return Ok(Err(GraphSealedBaseAbsenceV1::RowSumMismatch));
        }
        GraphSealedBaseV1::open(identity, digest, counts, row_sum, files, check)
    }

    /// A row spill for a delta of `projection` over `base`, pinning the
    /// base's bytes until the delta seals.
    pub(crate) fn layered_row_spill(
        &self,
        projection: GraphProjectionIdentity,
        base: GraphSealedBaseV1,
    ) -> Result<GraphLayeredRowSpill, GraphDbError> {
        GraphLayeredRowSpill::create(self.row_spill_directory()?, projection, base)
    }

    /// Opens an existing sealed store for `identity` without building one.
    ///
    /// Used on the recovery path: a matching artifact on disk is installed
    /// for reads, anything else (absent, foreign digest, unreadable) is
    /// discarded and reads stay on the staging database.
    pub(crate) fn open_sealed_generation_store_if_present(
        &self,
        identity: &GraphGenerationManifestIdentity,
        expected: &GraphRecoveredGenerationDigestV1,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        if sealed_store_disabled() {
            return Ok(());
        }
        let Some(reopen) = self.inner.reopen.as_ref() else {
            return Ok(());
        };
        let Some(database_path) = reopen.config.path.clone() else {
            return Ok(());
        };
        let locator =
            GenerationLocator::new(identity.projection.clone(), identity.generation.clone());
        {
            let sealed = self.inner.sealed_generations.read().map_err(|_| {
                GraphDbError::unavailable("sealed generation store lock is poisoned")
            })?;
            if sealed
                .get(&locator)
                .is_some_and(|existing| existing.recovered_digest() == expected.as_str())
            {
                return Ok(());
            }
        }
        let physical_namespace = identity.physical_namespace()?;
        let root = sealed_store_root(&database_path);
        let directory = sealed_generation_directory(&root, &physical_namespace);
        match open_sealed_store_checked(&directory, identity, expected, check, None) {
            Ok(Some(store)) => self.install_sealed_generation_store(locator, store),
            Ok(None) => Ok(()),
            Err(error @ (GraphDbError::Cancelled | GraphDbError::DeadlineExceeded)) => Err(error),
            Err(_) => {
                // A stale or corrupt artifact never outranks the verified
                // staging rows; discard it so a later seal can rebuild.
                remove_sealed_directory(&directory);
                Ok(())
            }
        }
    }

    fn install_sealed_generation_store(
        &self,
        locator: GenerationLocator,
        store: Arc<SealedGenerationStore>,
    ) -> Result<(), GraphDbError> {
        let superseded = {
            let mut sealed = self.inner.sealed_generations.write().map_err(|_| {
                GraphDbError::unavailable("sealed generation store lock is poisoned")
            })?;
            sealed.insert(locator.clone(), store)
        };
        if let Some(previous) = superseded {
            // The replacement shares the artifact directory, so only the
            // superseded handle is closed; the files stay for the new reader.
            let _ = previous.close();
        }
        // Newly installed generation aside, every other retained sealed
        // reader is now a non-serving owner. Step each idle one down to its
        // identity so the count of concurrently materialized graphs stays
        // bounded by what is actually being read, not by how many
        // generations this process has ever sealed.
        self.reap_idle_sealed_generation_engines(Some(&locator));
        Ok(())
    }

    /// Releases the native engine of every retained sealed reader except
    /// `serving`, skipping any a reader currently holds. Returns how many
    /// engines were released.
    ///
    /// Never loses truth and never evicts a leased serving generation: the
    /// artifact and its receipt stay on disk, the reader keeps its exact
    /// locator, digest, row counts and canonical byte census, and the next
    /// read reopens the same container. A generation whose snapshot gate is
    /// busy is left resident.
    pub(crate) fn reap_idle_sealed_generation_engines(
        &self,
        serving: Option<&GenerationLocator>,
    ) -> usize {
        let Ok(sealed) = self.inner.sealed_generations.read() else {
            return 0;
        };
        let candidates = sealed
            .iter()
            .filter(|(locator, _)| Some(*locator) != serving)
            .map(|(_, store)| Arc::clone(store))
            .collect::<Vec<_>>();
        drop(sealed);
        let mut released = 0usize;
        for store in candidates {
            match store.hibernate_if_lazy_when_idle() {
                Ok(true) => released += 1,
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "sealed generation engine could not hibernate; it stays resident"
                    );
                }
            }
        }
        self.publish_sealed_generation_census();
        released
    }

    /// Per-generation retained census: how many sealed readers this database
    /// holds, how many of them have a materialized engine right now, and the
    /// canonical bytes those readers were proven over.
    pub(crate) fn sealed_generation_census(&self) -> SealedGenerationCensusV1 {
        let Ok(sealed) = self.inner.sealed_generations.read() else {
            return SealedGenerationCensusV1::default();
        };
        let mut census = SealedGenerationCensusV1::default();
        for store in sealed.values() {
            census.retained += 1;
            census.retained_canonical_bytes = census
                .retained_canonical_bytes
                .saturating_add(store.canonical_bytes());
            if store.engine_resident() {
                census.resident += 1;
                census.resident_canonical_bytes = census
                    .resident_canonical_bytes
                    .saturating_add(store.canonical_bytes());
            }
        }
        census
    }

    fn publish_sealed_generation_census(&self) {
        let census = self.sealed_generation_census();
        tracing::debug!(
            event = "graph_sealed_generation_census",
            retained = census.retained,
            resident = census.resident,
            retained_canonical_bytes = census.retained_canonical_bytes,
            resident_canonical_bytes = census.resident_canonical_bytes,
            "retained sealed generation readers and their materialized engines"
        );
    }

    /// Retained sealed readers and how many hold a materialized engine.
    #[cfg(any(test, feature = "test-helpers", feature = "eval-helpers"))]
    #[must_use]
    pub fn sealed_generation_engine_census(&self) -> (usize, usize) {
        let census = self.sealed_generation_census();
        (census.retained, census.resident)
    }

    /// Retires the sealed artifact for `locator`: uninstalls the reader and
    /// deletes its directory. Idempotent, and never touches staging rows.
    #[tracing::instrument(name = "graph_db.sealed_store.retire", level = "trace", skip_all)]
    pub(crate) fn retire_sealed_generation_store(&self, locator: &GenerationLocator) {
        let removed = self
            .inner
            .sealed_generations
            .write()
            .ok()
            .and_then(|mut sealed| sealed.remove(locator));
        if let Some(store) = removed {
            store.discard();
            return;
        }
        // No installed reader: still delete any on-disk artifact so a
        // retired generation leaves nothing behind.
        let Some(reopen) = self.inner.reopen.as_ref() else {
            return;
        };
        let Some(database_path) = reopen.config.path.as_ref() else {
            return;
        };
        let Ok(physical_namespace) = locator.physical_namespace() else {
            return;
        };
        let root = sealed_store_root(database_path);
        remove_sealed_directory(&sealed_generation_directory(&root, &physical_namespace));
    }
}

impl GraphDb {
    /// Bench/test-only: open a sealed artifact database directly by its
    /// directory, exactly as production adoption opens it (mmap-backed
    /// compact base when the artifact is compact-form), without the digest
    /// proof. The at-rest probes time the open and then prove the reads
    /// themselves.
    #[cfg(any(test, feature = "test-helpers", feature = "eval-helpers"))]
    pub fn open_sealed_artifact_for_bench(directory: &Path) -> Result<Arc<GraphDb>, GraphDbError> {
        GraphDb::open_with_store_state(
            sealed_artifact_database_options(directory.join(SEALED_STORE_DATABASE_FILE)),
            Some(PersistentGraphStoreState::Existing),
        )
    }
}

/// Where a sealed build reads the generation's rows from.
///
/// Both sources yield the recovered digest's row stream in its canonical
/// order, sorted, unique entities, then sorted, unique relations, so the two
/// build byte-identical containers for the same generation, and the reopen
/// proof against the relational authority's digest is the same proof.
pub(crate) enum SealedRowSource<'a> {
    /// The generation is staged in this shared staging database, which also
    /// resolves the dependency-generation endpoints its relations reach.
    Staging(&'a GraphDb),
    /// The verified manifest hydrated from the durable replay journal. Only a
    /// dependency-free generation may seal this way: every relation endpoint
    /// is one of its own entities, so no staging row is ever needed, written,
    /// or read. The journal and the code generation it names remain the
    /// recovery source for every failure boundary of the build.
    Manifest(&'a GraphGenerationManifest),
    /// The same rows, merged on disk by a batch producer. Always
    /// dependency-free, and recoverable from the same journal.
    Spilled(&'a SpilledGraphGeneration),
    /// A delta over a sealed cold base, also recoverable from the journal:
    /// a cold replay of the same code generation records the same digest.
    Layered(&'a LayeredGraphGeneration),
}

/// Builds (or adopts) the sealed store for `identity` and returns the
/// reopened, digest-verified reader.
///
/// The returned `Option<u64>` is the staging proof: `Some(canonical_bytes)`
/// only when this call enumerated the *staging* database's rows and the
/// reopened artifact reproduced the authority's digest. A manifest-sourced
/// build never read the staging container, so it proves nothing about it.
#[tracing::instrument(name = "graph_db.sealed_store.build", level = "trace", skip_all)]
fn build_or_open_sealed_store(
    rows: SealedRowSource<'_>,
    identity: &GraphGenerationManifestIdentity,
    expected: &GraphRecoveredGenerationDigestV1,
    database_path: &Path,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(Arc<SealedGenerationStore>, Option<u64>), GraphDbError> {
    let staging_sourced = matches!(rows, SealedRowSource::Staging(_));
    let physical_namespace = identity.physical_namespace()?;
    let root = sealed_store_root(database_path);
    let directory = sealed_generation_directory(&root, &physical_namespace);
    // A layered build names the base it derived its digest from; its reopen
    // proves the delta now and the base before the layer's first base read.
    let trusted_base = match rows {
        SealedRowSource::Layered(layered) => Some(layered.base_receipt()),
        _ => None,
    };
    // Idempotent replay: an artifact from an earlier seal of this exact
    // generation is adopted if its receipt binds the same digest. Adoption
    // never enumerates `source`'s rows, so it yields no staging proof.
    match open_sealed_store_checked(
        &directory,
        identity,
        expected,
        &|| Ok(()),
        trusted_base.as_ref(),
    ) {
        Ok(Some(store)) => return Ok((store, None)),
        Ok(None) => {}
        Err(_) => remove_sealed_directory(&directory),
    }
    if directory.exists() {
        remove_sealed_directory(&directory);
    }
    let staging = root.join(format!(
        ".staging-{}",
        directory
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("sealed")
    ));
    remove_sealed_directory(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|error| sealed_store_io_failure("staging directory create failed", error))?;
    let receipt = stage_sealed_artifact(rows, identity, expected, &staging, check)
        .inspect_err(|_| remove_sealed_directory(&staging))?;
    let encoded = serde_json::to_vec_pretty(&receipt)
        .map_err(|error| GraphDbError::unavailable(format!("sealed receipt encode: {error}")))?;
    std::fs::write(staging.join(SEALED_STORE_RECEIPT_FILE), encoded)
        .inspect_err(|_| remove_sealed_directory(&staging))
        .map_err(|error| sealed_store_io_failure("receipt write failed", error))?;
    if let Err(error) = std::fs::rename(&staging, &directory) {
        remove_sealed_directory(&staging);
        // A concurrent seal of the same generation may have installed the
        // directory first; adopting it below keeps this path idempotent.
        if !directory.exists() {
            return Err(sealed_store_io_failure("artifact install failed", error));
        }
    }
    let match_result = {
        let _span = tracing::trace_span!("code_index.seal.verify").entered();
        open_sealed_store_checked(
            &directory,
            identity,
            expected,
            &|| Ok(()),
            trusted_base.as_ref(),
        )
    };
    match match_result {
        // When this call enumerated the staging database's rows into the
        // copy and the reopen digest matched the authority's expectation,
        // together that is the staging container's own proof, sized by the
        // canonical bytes the reopen hashed.
        Ok(Some(store)) => {
            let staging_proof = staging_sourced.then_some(store.canonical_bytes);
            Ok((store, staging_proof))
        }
        Ok(None) => {
            remove_sealed_directory(&directory);
            Err(GraphDbError::unavailable(
                "sealed generation store disappeared between install and reopen".to_owned(),
            ))
        }
        Err(error) => {
            remove_sealed_directory(&directory);
            Err(error)
        }
    }
}

/// Builds the artifact directory `staging` holds for `rows`: the container,
/// a layered store's base links and hidden rows or a flat store's producer
/// attachment, and the receipt that binds them, unwritten.
fn stage_sealed_artifact(
    rows: SealedRowSource<'_>,
    identity: &GraphGenerationManifestIdentity,
    expected: &GraphRecoveredGenerationDigestV1,
    staging: &Path,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<SealedStoreReceiptV1, GraphDbError> {
    let (row_sum, base_files, layered) = match rows {
        SealedRowSource::Spilled(spilled) => (
            Some(spilled.row_sum()),
            spilled
                .attachment()
                .zip(spilled.row_index())
                .zip(spilled.entity_rows())
                .map(
                    |((attachment, row_index), (entity_rows, entity_row_offsets))| {
                        (attachment, row_index, entity_rows, entity_row_offsets)
                    },
                ),
            None,
        ),
        SealedRowSource::Layered(layered) => (Some(layered.row_sum()), None, Some(layered)),
        SealedRowSource::Manifest(manifest) => (Some(manifest.row_sum(check)?), None, None),
        SealedRowSource::Staging(_) => (None, None, None),
    };
    let container_rows = match rows {
        SealedRowSource::Layered(layered) => SealedRowSource::Spilled(layered.delta()),
        rows => rows,
    };
    let (mut entities, mut relations) =
        build_sealed_container(container_rows, identity, staging, check)?;
    if let Some(layered) = layered {
        layered.install_base_files(staging)?;
        (entities, relations) = layered.row_counts();
    }
    if let Some((attachment, row_index, entity_rows, entity_row_offsets)) = &base_files {
        for (source, name) in [
            (attachment, crate::sealed_layer::GENERATION_ATTACHMENT_FILE),
            (row_index, crate::row_index::ROW_INDEX_FILE),
            (entity_rows, crate::generation::ENTITIES_FILE),
            (
                entity_row_offsets,
                crate::row_index::ENTITY_ROW_OFFSETS_FILE,
            ),
        ] {
            std::fs::hard_link(source, staging.join(name))
                .map_err(|error| sealed_store_io_failure("base file link failed", error))?;
        }
    }
    Ok(SealedStoreReceiptV1 {
        version: SEALED_STORE_RECEIPT_VERSION,
        form: if layered.is_some() {
            SEALED_STORE_FORM_LAYERED
        } else {
            SEALED_STORE_FORM_COMPACT
        }
        .to_owned(),
        namespace: identity.projection.namespace.as_str().to_owned(),
        projection: identity.projection.projection.as_str().to_owned(),
        generation: identity.generation.as_str().to_owned(),
        physical_namespace: identity.physical_namespace()?.as_str().to_owned(),
        recovered_digest: expected.as_str().to_owned(),
        entities,
        relations,
        graph_format: GraphFormatVersion::current().get(),
        row_sum: row_sum.map(GraphRowDigestSum::to_hex),
        base: layered.map(LayeredGraphGeneration::base_receipt),
        source_generation: Some(identity.source_generation.as_str().to_owned()),
        watermark: Some(identity.watermark.as_str().to_owned()),
    })
}

/// Streams the generation's verified rows into a compact store and writes it
/// as the sealed container under `staging` in one durable pass. Returns the
/// written `(entities, relations)` counts.
///
/// The row set is exactly the recovered digest's: the sorted, unique entity
/// and relation enumerations of the physical namespace, plus every
/// dependency-generation endpoint those relations reach, each written once
/// in its own namespace. `check` runs per row; a cancelled or failed build
/// has written nothing under `staging` that the caller keeps, the container
/// appears complete or not at all, and the next attempt rebuilds from the
/// same source.
fn build_sealed_container(
    rows: SealedRowSource<'_>,
    identity: &GraphGenerationManifestIdentity,
    staging: &Path,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(usize, usize), GraphDbError> {
    let physical_namespace = identity.physical_namespace()?;
    let mut sealed = SealedCompactRows::new(staging)?;
    let (entity_count, relation_count, dependency_namespaces_written) = {
        let _span = tracing::trace_span!("code_index.seal.encode").entered();
        {
            match rows {
                SealedRowSource::Staging(source) => {
                    push_staged_rows(source, identity, &physical_namespace, &mut sealed, check)
                }
                SealedRowSource::Manifest(manifest) => {
                    let counts = push_manifest_rows(
                        manifest,
                        identity,
                        &physical_namespace,
                        &mut sealed,
                        check,
                    )?;
                    Ok((counts.0, counts.1, BTreeMap::new()))
                }
                SealedRowSource::Spilled(spilled) => {
                    let counts = push_spilled_rows(
                        spilled,
                        identity,
                        &physical_namespace,
                        &mut sealed,
                        check,
                    )?;
                    Ok((counts.0, counts.1, BTreeMap::new()))
                }
                SealedRowSource::Layered(layered) => {
                    let counts = push_spilled_rows(
                        layered.delta(),
                        identity,
                        &physical_namespace,
                        &mut sealed,
                        check,
                    )?;
                    Ok((counts.0, counts.1, BTreeMap::new()))
                }
            }
        }
    }?;

    // Finalization: one projection commit per written namespace, in
    // namespace order, then the format marker at the final sequence. The
    // physical namespace's commit binds the dependency-closure digest, the
    // recovered proof requires it, and it is what marks these rows as a
    // *sealed* generation rather than an unfinished stage.
    let mut sequence = 0_u64;
    for (namespace, projection) in &dependency_namespaces_written {
        check()?;
        sequence += 1;
        let commit =
            sealed_namespace_commit(namespace, projection, identity, sequence, None, check)?;
        sealed.push_projection_commit(namespace, projection, &commit)?;
    }
    sequence += 1;
    let commit = sealed_namespace_commit(
        &physical_namespace,
        &identity.projection.projection,
        identity,
        sequence,
        Some(identity.dependency_closure_digest(check)?),
        check,
    )?;
    sealed.push_projection_commit(
        &physical_namespace,
        &identity.projection.projection,
        &commit,
    )?;
    sealed.push_format_marker(sequence)?;
    check()?;
    {
        let _span = tracing::trace_span!("code_index.seal.write").entered();
        sealed.write_container(&staging.join(SEALED_STORE_DATABASE_FILE))
    }?;
    Ok((entity_count, relation_count))
}

/// Pushes a dependency-free generation's rows straight from its verified
/// manifest: the entities in identity order, then the relations in identity
/// order, each endpoint resolved by binary search over the entity identities.
/// Entity nodes are the first `entities.len()` sealed ids, so the search
/// index *is* the endpoint's node.
///
/// Refuses, typed, a manifest that carries dependencies or whose rows are
/// not in canonical order: the recovered digest is defined over the sorted,
/// unique row set, and the manifest constructor sorts and deduplicates, so
/// anything else here is a corrupted manifest, not a different build.
fn push_manifest_rows(
    manifest: &GraphGenerationManifest,
    identity: &GraphGenerationManifestIdentity,
    physical_namespace: &GraphNamespace,
    sealed: &mut SealedCompactRows,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(usize, usize), GraphDbError> {
    if !identity.dependencies.is_empty() {
        return Err(GraphDbError::invalid(
            "a direct sealed build requires a dependency-free generation",
        ));
    }
    let projection = &identity.projection.projection;
    let entities = &manifest.entities;
    let workers = rayon::current_thread_index()
        .map(|_| rayon::current_num_threads())
        .unwrap_or(1);
    let row_window = workers.max(1).saturating_mul(512);
    {
        let _span = tracing::trace_span!("code_index.seal.encode.entities").entered();
        {
            for (window_index, window) in entities.chunks(row_window).enumerate() {
                let start = window_index.saturating_mul(row_window);
                for (offset, entity) in window.iter().enumerate() {
                    check()?;
                    let index = start.saturating_add(offset);
                    if index
                        .checked_sub(1)
                        .is_some_and(|prior| entities[prior].identity >= entity.identity)
                    {
                        return Err(GraphDbError::Corrupt {
                            message:
                                "graph generation manifest entities are not in canonical order"
                                    .to_owned(),
                        });
                    }
                }
                let prepared = collect_prepared_rows_ordered(window, |_, entity| {
                    Ok(SealedCompactRows::prepare_entity(
                        physical_namespace,
                        projection,
                        entity,
                    ))
                })?;
                for (offset, prepared) in prepared.into_iter().enumerate() {
                    let node = sealed.push_prepared_node(prepared)?;
                    if usize::try_from(node.as_u64()).ok() != Some(start.saturating_add(offset)) {
                        return Err(GraphDbError::Corrupt {
                            message: "sealed build entity ids diverged from manifest order"
                                .to_owned(),
                        });
                    }
                }
            }
            Ok::<(), GraphDbError>(())
        }
    }?;
    {
        let _span = tracing::trace_span!("code_index.seal.encode.relations").entered();
        {
            let relations = &manifest.relations;
            for (window_index, window) in relations.chunks(row_window).enumerate() {
                let start = window_index.saturating_mul(row_window);
                for (offset, relation) in window.iter().enumerate() {
                    check()?;
                    let index = start.saturating_add(offset);
                    if index
                        .checked_sub(1)
                        .is_some_and(|prior| relations[prior].identity >= relation.identity)
                    {
                        return Err(GraphDbError::Corrupt {
                            message:
                                "graph generation manifest relations are not in canonical order"
                                    .to_owned(),
                        });
                    }
                }
                let prepared = collect_prepared_rows_ordered(window, |offset, relation| {
                    let mut endpoints = [NodeId::new(0); 2];
                    for (slot, endpoint) in endpoints.iter_mut().zip([&relation.from, &relation.to])
                    {
                        if endpoint.projection != identity.projection {
                            return Err(GraphDbError::Corrupt {
                                message: "sealed build relation escapes its dependency closure"
                                    .to_owned(),
                            });
                        }
                        let index = entities
                        .binary_search_by(|entity| entity.identity.cmp(&endpoint.identity))
                        .map_err(|_| GraphDbError::Corrupt {
                            message: format!(
                                "local relation endpoint `{}` is absent from the candidate generation",
                                endpoint.identity
                            ),
                        })?;
                        *slot = NodeId::new(u64::try_from(index).map_err(|_| {
                            GraphDbError::unavailable("sealed entity count exceeds u64")
                        })?);
                    }
                    let edge_index = u64::try_from(start.saturating_add(offset)).map_err(|_| {
                        GraphDbError::unavailable("sealed relation count exceeds u64")
                    })?;
                    let stored = relation.storage_relation()?;
                    let prepared = SealedCompactRows::prepare_relation(
                        physical_namespace,
                        projection,
                        &stored,
                        EdgeId::new(edge_index),
                    )?;
                    Ok((prepared, endpoints))
                })?;
                for (prepared, endpoints) in prepared {
                    sealed.push_prepared_relation(prepared, endpoints[0], endpoints[1])?;
                }
            }
            Ok::<(), GraphDbError>(())
        }
    }?;
    Ok((entities.len(), manifest.relations.len()))
}

/// Pushes a spilled generation's merged rows in the same order and shape as
/// [`push_manifest_rows`]: entities stream in identity order and take the
/// first sealed node ids, then relations stream in identity order with each
/// endpoint resolved to its entity's position in the resident identity list.
/// Only one window of decoded rows is held at a time.
fn push_spilled_rows(
    spilled: &SpilledGraphGeneration,
    identity: &GraphGenerationManifestIdentity,
    physical_namespace: &GraphNamespace,
    sealed: &mut SealedCompactRows,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(usize, usize), GraphDbError> {
    let projection = &identity.projection.projection;
    let workers = rayon::current_thread_index()
        .map(|_| rayon::current_num_threads())
        .unwrap_or(1);
    let row_window = workers.max(1).saturating_mul(512);
    let (entity_count, relation_count) = spilled.row_counts();
    {
        let _span = tracing::trace_span!("code_index.seal.encode.entities").entered();
        {
            let mut rows = spilled.entities()?;
            let mut pushed = 0usize;
            loop {
                let window = rows
                    .by_ref()
                    .take(row_window)
                    .map(|row| row.map(|(entity, _)| entity))
                    .collect::<Result<Vec<_>, _>>()?;
                if window.is_empty() {
                    break;
                }
                check()?;
                let prepared = collect_prepared_rows_ordered(&window, |_, entity| {
                    Ok(SealedCompactRows::prepare_entity(
                        physical_namespace,
                        projection,
                        entity,
                    ))
                })?;
                for prepared in prepared {
                    let node = sealed.push_prepared_node(prepared)?;
                    if usize::try_from(node.as_u64()).ok() != Some(pushed) {
                        return Err(GraphDbError::Corrupt {
                            message: "sealed build entity ids diverged from spilled order"
                                .to_owned(),
                        });
                    }
                    pushed += 1;
                }
            }
            if pushed != entity_count {
                return Err(GraphDbError::Corrupt {
                    message: "sealed build read a different entity count than was spilled"
                        .to_owned(),
                });
            }
            Ok::<(), GraphDbError>(())
        }
    }?;
    {
        let _span = tracing::trace_span!("code_index.seal.encode.relations").entered();
        {
            let mut rows = spilled.relations()?;
            let mut pushed = 0usize;
            loop {
                let window = rows
                    .by_ref()
                    .take(row_window)
                    .collect::<Result<Vec<_>, _>>()?;
                if window.is_empty() {
                    break;
                }
                check()?;
                let start = pushed;
                let prepared = collect_prepared_rows_ordered(
                    &window,
                    |offset, (relation, endpoints)| {
                        let mut nodes = [NodeId::new(0); 2];
                        for (slot, endpoint) in nodes.iter_mut().zip(endpoints) {
                            let index = spilled.entity_index(endpoint).ok_or_else(|| {
                            GraphDbError::Corrupt {
                                message: format!(
                                    "local relation endpoint `{endpoint}` is absent from the candidate generation"
                                ),
                            }
                        })?;
                            *slot = NodeId::new(u64::try_from(index).map_err(|_| {
                                GraphDbError::unavailable("sealed entity count exceeds u64")
                            })?);
                        }
                        let edge_index =
                            u64::try_from(start.saturating_add(offset)).map_err(|_| {
                                GraphDbError::unavailable("sealed relation count exceeds u64")
                            })?;
                        let stored = relation.storage_relation()?;
                        let prepared = SealedCompactRows::prepare_relation(
                            physical_namespace,
                            projection,
                            &stored,
                            EdgeId::new(edge_index),
                        )?;
                        Ok((prepared, nodes))
                    },
                )?;
                for (prepared, nodes) in prepared {
                    sealed.push_prepared_relation(prepared, nodes[0], nodes[1])?;
                    pushed += 1;
                }
            }
            if pushed != relation_count {
                return Err(GraphDbError::Corrupt {
                    message: "sealed build read a different relation count than was spilled"
                        .to_owned(),
                });
            }
            Ok::<(), GraphDbError>(())
        }
    }?;
    Ok((entity_count, relation_count))
}

/// Pushes a staged generation's rows out of the shared staging database.
///
/// Each staging read guard is held for one bounded chunk so concurrent
/// writers and readers of the shared staging database wait milliseconds, not
/// a build. Returns the row counts and the dependency namespaces whose
/// endpoints were copied, each with its projection.
fn push_staged_rows(
    source: &GraphDb,
    identity: &GraphGenerationManifestIdentity,
    physical_namespace: &GraphNamespace,
    rows: &mut SealedCompactRows,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(usize, usize, BTreeMap<GraphNamespace, GraphProjectionId>), GraphDbError> {
    // Physical namespace -> projection identity for the generation and its
    // dependency closure; an endpoint outside this map escapes the closure.
    let namespace_projection = physical_namespace_projection_map(identity)?;

    // Enumerate exactly the digest's row sets from the staging database.
    // Index scans only under this guard; the row loads below reacquire it in
    // bounded chunks.
    let (entity_nodes, relation_locators) = {
        let _span = tracing::trace_span!("graph_db.sealed_store.copy.enumerate").entered();
        {
            let guard = source.read_guard()?;
            let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
            let entity_nodes = projection_entity_nodes_sorted_checked(
                database,
                physical_namespace,
                &identity.projection.projection,
                check,
            )?;
            let relation_locators = projection_relation_nodes_sorted_checked(
                database,
                physical_namespace,
                &identity.projection.projection,
                check,
            )?;
            (entity_nodes, relation_locators)
        }
    };
    // The staged generation is immutable, so its node handles stay valid
    // across guard reacquisitions.
    let entity_count = entity_nodes.len();
    let relation_count = relation_locators.len();
    // Staging entity handle -> sealed node, for endpoint resolution.
    let mut sealed_endpoints: HashMap<NodeId, NodeId> = HashMap::new();

    // 1. The generation's own entities, in recovered-digest order.
    {
        let _span = tracing::trace_span!("graph_db.sealed_store.copy.entities").entered();
        {
            for chunk in entity_nodes.chunks(SEALED_COPY_GUARD_CHUNK_ROWS) {
                let guard = source.read_guard()?;
                let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
                let store = database.graph_store();
                for (_, node) in chunk {
                    check()?;
                    // Decode straight from the enumerated node: the sorted
                    // enumeration already proved identity uniqueness, so the
                    // unique-key index round-trip `load_entity_by_node` pays
                    // per row contributes nothing here.
                    let record = store.get_node(*node).ok_or_else(|| GraphDbError::Corrupt {
                        message: "sealed build entity disappeared during enumeration".to_owned(),
                    })?;
                    let entity = decode_entity(&record)?;
                    let sealed = rows.push_entity(
                        physical_namespace,
                        &identity.projection.projection,
                        &entity,
                    )?;
                    if sealed_endpoints.insert(*node, sealed).is_some() {
                        return Err(GraphDbError::Corrupt {
                            message: "sealed build enumerated the same entity twice".to_owned(),
                        });
                    }
                }
            }
            Ok::<(), GraphDbError>(())
        }
    }?;
    drop(entity_nodes);

    // 2. The generation's relations, in recovered-digest order. An endpoint
    // that is not yet written lives in a dependency generation: it is copied
    // into its own namespace the first time a relation reaches it, so every
    // edge's endpoints exist before the edge is pushed.
    let mut endpoint_cache = EndpointIdentityCache::default();
    let mut dependency_namespaces_written: BTreeMap<GraphNamespace, GraphProjectionId> =
        BTreeMap::new();
    {
        let _span = tracing::trace_span!("graph_db.sealed_store.copy.relations").entered();
        {
            for chunk in relation_locators.chunks(SEALED_COPY_GUARD_CHUNK_ROWS) {
                let guard = source.read_guard()?;
                let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
                let store = database.graph_store();
                for (_, locator) in chunk {
                    check()?;
                    let stored = load_relation_by_locator_cached(
                        store.as_ref(),
                        *locator,
                        &mut endpoint_cache,
                    )?;
                    let mut endpoints = [NodeId::new(0); 2];
                    for (slot, staging_node) in
                        endpoints.iter_mut().zip([stored.source, stored.target])
                    {
                        *slot = match sealed_endpoints.get(&staging_node) {
                            Some(node) => *node,
                            None => {
                                let (namespace, _) =
                                    endpoint_cache.identity(store.as_ref(), staging_node)?;
                                let projection = namespace_projection
                                    .get(&namespace)
                                    .filter(|_| namespace != *physical_namespace)
                                    .ok_or_else(|| GraphDbError::Corrupt {
                                        message:
                                            "sealed build relation escapes its dependency closure"
                                                .to_owned(),
                                    })?;
                                let record = store.get_node(staging_node).ok_or_else(|| {
                                    GraphDbError::Corrupt {
                                        message: "sealed build dependency endpoint disappeared"
                                            .to_owned(),
                                    }
                                })?;
                                let entity = decode_entity(&record)?;
                                let sealed =
                                    rows.push_entity(&namespace, &projection.projection, &entity)?;
                                dependency_namespaces_written
                                    .entry(namespace)
                                    .or_insert_with(|| projection.projection.clone());
                                sealed_endpoints.insert(staging_node, sealed);
                                sealed
                            }
                        };
                    }
                    rows.push_relation(
                        physical_namespace,
                        &identity.projection.projection,
                        &stored.relation,
                        endpoints[0],
                        endpoints[1],
                    )?;
                }
            }
            Ok::<(), GraphDbError>(())
        }
    }?;
    Ok((entity_count, relation_count, dependency_namespaces_written))
}

/// Opens the artifact under `directory` and proves it against `expected`.
///
/// Returns `Ok(None)` when no artifact exists, `Err` when one exists but is
/// unreadable or bound to a different digest.
#[cfg(test)]
#[tracing::instrument(name = "graph_db.sealed_store.open", level = "trace", skip_all)]
fn open_sealed_store(
    directory: &Path,
    identity: &GraphGenerationManifestIdentity,
    expected: &GraphRecoveredGenerationDigestV1,
) -> Result<Option<Arc<SealedGenerationStore>>, GraphDbError> {
    open_sealed_store_checked(directory, identity, expected, &|| Ok(()), None)
}

fn load_sealed_store_receipt(
    directory: &Path,
) -> Result<Option<SealedStoreReceiptV1>, GraphDbError> {
    let receipt_path = directory.join(SEALED_STORE_RECEIPT_FILE);
    let receipt_bytes = match std::fs::read(&receipt_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(sealed_store_io_failure("receipt read failed", error)),
    };
    serde_json::from_slice(&receipt_bytes)
        .map(Some)
        .map_err(|error| GraphDbError::unavailable(format!("sealed receipt decode: {error}")))
}

fn sealed_store_check_matches(
    directory: &Path,
    expected_digest: &str,
) -> Result<bool, GraphDbError> {
    let path = directory.join(SEALED_STORE_CHECKED_FILE);
    match std::fs::read_to_string(&path) {
        Ok(digest) => Ok(digest == expected_digest),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(sealed_store_io_failure("sealed check read failed", error)),
    }
}

fn persist_sealed_store_check(directory: &Path, expected_digest: &str) -> Result<(), GraphDbError> {
    if sealed_store_check_matches(directory, expected_digest)? {
        return Ok(());
    }
    std::fs::write(directory.join(SEALED_STORE_CHECKED_FILE), expected_digest)
        .map_err(|error| sealed_store_io_failure("sealed check persist failed", error))
}

fn open_sealed_store_checked(
    directory: &Path,
    identity: &GraphGenerationManifestIdentity,
    expected: &GraphRecoveredGenerationDigestV1,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    trusted_base: Option<&SealedBaseReceiptV1>,
) -> Result<Option<Arc<SealedGenerationStore>>, GraphDbError> {
    let database_path = directory.join(SEALED_STORE_DATABASE_FILE);
    let Some(receipt) = load_sealed_store_receipt(directory)? else {
        return Ok(None);
    };
    let locator = GenerationLocator::new(identity.projection.clone(), identity.generation.clone());
    let physical_namespace = identity.physical_namespace()?;
    if !receipt.binds(&locator, physical_namespace.as_str(), expected.as_str()) {
        return Err(GraphDbError::unavailable(
            "sealed generation store receipt does not bind this generation".to_owned(),
        ));
    }
    let row_sum = receipt
        .row_sum
        .as_deref()
        .map(GraphRowDigestSum::from_hex)
        .transpose()?;
    let container_expected =
        container_proof_digest(&receipt, row_sum, identity, expected, trusted_base, check)?;
    // Lazily: installing a sealed reader must not retain a whole in-memory
    // graph. grafeo's store is heap resident, so an eager open here kept the
    // artifact's entire block log in RAM for every generation this process
    // ever sealed, five retained generations meant five whole graphs (#799),
    // and one published worktree scope meant one more (#830). The proof below
    // opens the engine once, resolves by marker whenever the verify-once
    // marker covers the exact container the engine loaded, and the engine is
    // released again immediately after; anything that reads the generation
    // later reopens the same container through `ensure_opened`.
    let database = GraphDb::open_lazy_with_store_state(
        sealed_artifact_database_options(database_path),
        PersistentGraphStoreState::Existing,
    )
    .map_err(|error| sealed_store_failure("reopen failed", error))?;
    // Prove the compacted, reopened store serves exactly the sealed rows
    // before it answers a single read. The artifact is immutable after its
    // build, so a proof established by an earlier open of these exact
    // container bytes stands: the marker beside the artifact resolves it
    // against the container the engine opened, and anything else, a missing
    // or foreign marker, or a container whose identity moved, falls back to
    // the full row proof and files the marker for the next open. `expected`
    // still comes from the relational authority, exactly as on the staging
    // container.
    let canonical_bytes = match sealed_copy_proof(&database, identity, &container_expected, check) {
        Ok(canonical_bytes) => canonical_bytes,
        Err(error) => {
            let _ = database.close();
            return Err(sealed_store_failure(
                "post-reopen verification failed",
                error,
            ));
        }
    };
    database.mark_sealed_read_only();
    let layer = match &receipt.base {
        Some(base) => {
            match open_layer_base(
                directory,
                identity,
                base,
                trusted_base.is_none(),
                &database,
                check,
            ) {
                Ok(layer) => Some(layer),
                Err(error) => {
                    let _ = database.close();
                    return Err(error);
                }
            }
        }
        None => None,
    };
    // The proof materialized the engine, whether it streamed the rows or
    // resolved by marker against the container that engine opened. The proof
    // is filed now, so the engine is pure resident cost until a read actually
    // arrives: release it and let the first read reopen.
    if let Err(error) = {
        let _span = tracing::trace_span!("code_index.seal.verify.hibernate").entered();
        database.hibernate_if_lazy()
    } {
        let _ = database.close();
        return Err(sealed_store_failure("post-proof hibernation failed", error));
    }
    if let Err(error) = persist_sealed_store_check(directory, expected.as_str()) {
        let _ = database.close();
        return Err(error);
    }
    Ok(Some(Arc::new(SealedGenerationStore {
        locator: GenerationLocator::new(identity.projection.clone(), identity.generation.clone()),
        identity: identity.clone(),
        recovered_digest: expected.as_str().to_owned(),
        entity_count: receipt.entities,
        relation_count: receipt.relations,
        canonical_bytes,
        directory: directory.to_path_buf(),
        database,
        row_sum: if receipt.base.is_some() {
            None
        } else {
            row_sum
        },
        layer,
    })))
}

/// The digest the artifact's own container proves against: the head's for a
/// flat store; for a layered store, the digest of its delta rows, derived
/// from the receipt's row sums after the head's digest binds them.
fn container_proof_digest(
    receipt: &SealedStoreReceiptV1,
    row_sum: Option<GraphRowDigestSum>,
    identity: &GraphGenerationManifestIdentity,
    expected: &GraphRecoveredGenerationDigestV1,
    trusted_base: Option<&SealedBaseReceiptV1>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<GraphRecoveredGenerationDigestV1, GraphDbError> {
    match (&receipt.base, receipt.form.as_str()) {
        (None, SEALED_STORE_FORM_COMPACT) => Ok(expected.clone()),
        (Some(base), SEALED_STORE_FORM_LAYERED) => {
            let row_sum = row_sum.ok_or_else(|| {
                GraphDbError::unavailable("layered sealed receipt records no row sum")
            })?;
            if recovered_digest_from_row_sum(identity, row_sum, check)? != *expected {
                return Err(GraphDbError::unavailable(
                    "layered sealed receipt row sum does not digest to its verified head",
                ));
            }
            if trusted_base.is_some_and(|trusted| trusted != base) {
                return Err(GraphDbError::unavailable(
                    "layered sealed receipt names a different base",
                ));
            }
            let mut delta = row_sum;
            delta.merge(GraphRowDigestSum::from_hex(&base.hidden_row_sum)?);
            delta.subtract(GraphRowDigestSum::from_hex(&base.row_sum)?)?;
            recovered_digest_from_row_sum(identity, delta, check)
        }
        _ => Err(GraphDbError::unavailable(
            "sealed generation receipt names an unknown form",
        )),
    }
}

/// Opens a layered store's hard-linked base. `prove_now` proves it against
/// the digest its receipt records before returning, by marker or by
/// streaming its rows; otherwise the layer proves it before its first base
/// read, which lets a freshly sealed layer install without loading its base.
fn open_layer_base(
    directory: &Path,
    identity: &GraphGenerationManifestIdentity,
    base: &SealedBaseReceiptV1,
    prove_now: bool,
    delta: &GraphDb,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<SealedLayer, GraphDbError> {
    let base_identity = base.identity(&identity.projection)?;
    let base_digest = GraphRecoveredGenerationDigestV1::new(base.recovered_digest.clone())
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    if recovered_digest_from_row_sum(
        &base_identity,
        GraphRowDigestSum::from_hex(&base.row_sum)?,
        check,
    )? != base_digest
    {
        return Err(GraphDbError::unavailable(
            "layered base row sum does not digest to the base it names",
        ));
    }
    let database = Arc::new(
        GraphDb::open_lazy_with_store_state(
            sealed_artifact_database_options(crate::sealed_layer::base_database_path(directory)),
            PersistentGraphStoreState::Existing,
        )
        .map_err(|error| sealed_store_failure("layered base reopen failed", error))?,
    );
    if prove_now
        && let Err(error) = sealed_copy_proof(&database, &base_identity, &base_digest, check)
    {
        let _ = database.close();
        return Err(sealed_store_failure(
            "layered base verification failed",
            error,
        ));
    }
    database.mark_sealed_read_only();
    let layer = SealedLayer::open(
        directory,
        Arc::clone(&database),
        base.clone(),
        delta,
        identity,
        prove_now,
    );
    if let Err(error) = database.hibernate_if_lazy() {
        let _ = database.close();
        return Err(sealed_store_failure(
            "layered base hibernation failed",
            error,
        ));
    }
    layer
}

/// Resolves the recovered-digest proof for a reopened sealed copy: by the
/// artifact's own verify-once marker when the container the engine opened is
/// the one an earlier proof ran over, by the full row-streaming proof
/// otherwise. A full proof files the marker so the next open of unchanged
/// bytes resolves by marker. Returns the canonical byte count the proof
/// covers.
pub(crate) fn sealed_copy_proof(
    database: &GraphDb,
    identity: &GraphGenerationManifestIdentity,
    expected: &GraphRecoveredGenerationDigestV1,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<u64, GraphDbError> {
    let locator = GenerationLocator::new(identity.projection.clone(), identity.generation.clone());
    // The marker is consulted only against the container the resident engine
    // loaded, so the engine has to be open before the lookup can answer.
    {
        let _span = tracing::trace_span!("code_index.seal.verify.open").entered();
        database.ensure_opened()
    }?;
    if let Some(canonical_bytes) = {
        let _span = tracing::trace_span!("code_index.seal.verify.marker_lookup").entered();
        database.inner.markers.lookup(&locator, expected.as_str())
    } {
        database.inner.markers.record_fresh(&locator);
        #[cfg(test)]
        crate::generation::record_sealed_copy_marker_hit();
        crate::observe::record_sealed_copy_verification(
            crate::verified_marker::GenerationVerification::VerifiedFresh,
            canonical_bytes,
        );
        return Ok(canonical_bytes);
    }
    let canonical_bytes = {
        let _span = tracing::trace_span!("code_index.seal.verify.rows").entered();
        database.read_intact(&NeverCancelled, |native| {
            verify_sealed_copy_generation(native, identity, expected, check)
                .map(|(_, canonical_bytes)| canonical_bytes)
        })
    }?;
    database
        .inner
        .markers
        .record_proven(&locator, expected.as_str(), canonical_bytes);
    // Published now as well as at close, because both identities matter. A
    // read-only engine never checkpoints, so the container stays exactly the
    // one this engine opened for as long as this process serves it:
    // publishing under that identity lets every further open of the artifact
    // in the same boot, the direct-sealed recover and the registry adoption
    // were each paying this proof, resolve by marker. The close-time publish
    // then re-records the container as the closed handle reports it for the
    // next boot. A marker is a cache of completed proofs; failing to write one
    // costs the next open a re-proof and nothing else.
    if let Err(error) = {
        let _span = tracing::trace_span!("code_index.seal.verify.marker_publish").entered();
        database.inner.markers.publish_resident()
    } {
        let _ = error;
    }
    crate::observe::record_sealed_copy_verification(
        crate::verified_marker::GenerationVerification::Reverified,
        canonical_bytes,
    );
    Ok(canonical_bytes)
}

#[cfg(test)]
mod build_tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use rayon::ThreadPoolBuilder;

    use super::{
        SEALED_STORE_DATABASE_FILE, SealedRowSource, build_or_open_sealed_store,
        build_sealed_container, sealed_artifact_database_options, sealed_copy_proof,
        sealed_generation_directory, sealed_store_root,
    };
    use crate::location::PersistentGraphStoreState;
    use crate::{
        GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDbOwner, GraphDurability,
        GraphEntity, GraphEntityId, GraphEntityRef, GraphFormatVersion, GraphGenerationId,
        GraphGenerationManifest, GraphGenerationRelation, GraphLabel, GraphNamespace,
        GraphProjectionId, GraphProjectionIdentity, GraphProperty, GraphPropertyName,
        GraphRelationId, GraphRelationKind, GraphWatermark, NeverCancelled, SourceGeneration,
    };

    fn entity_identity(index: usize) -> GraphEntityId {
        GraphEntityId::new(format!("symbol:{index:05}")).unwrap()
    }

    /// A generation large enough that its copy spans several guard chunks
    /// and pager pages, with a Bytes payload so it takes the production
    /// (compact-eligible) row shape.
    fn manifest(entities: usize, relations: usize) -> GraphGenerationManifest {
        let projection = GraphProjectionIdentity::new(
            GraphNamespace::new("sealed-build").unwrap(),
            GraphProjectionId::new("code").unwrap(),
        );
        let entity_rows = (0..entities)
            .map(|index| {
                GraphEntity::new(
                    entity_identity(index),
                    BTreeSet::from([GraphLabel::new("function").unwrap()]),
                    BTreeMap::from([
                        (
                            GraphPropertyName::new("name").unwrap(),
                            GraphProperty::String(format!("fn_{index:05}")),
                        ),
                        (
                            GraphPropertyName::new("payload").unwrap(),
                            GraphProperty::Bytes(vec![(index % 251) as u8; 64]),
                        ),
                    ]),
                )
                .unwrap()
            })
            .collect();
        let entity_ref =
            |index: usize| GraphEntityRef::new(projection.clone(), entity_identity(index));
        let relation_rows = (0..relations)
            .map(|index| {
                GraphGenerationRelation::new(
                    GraphRelationId::new(format!("call:{index:05}")).unwrap(),
                    entity_ref(index % entities),
                    entity_ref((index + 1) % entities),
                    GraphRelationKind::new("calls").unwrap(),
                    BTreeMap::new(),
                )
                .unwrap()
            })
            .collect();
        GraphGenerationManifest::new(
            projection,
            GraphGenerationId::new("generation:build").unwrap(),
            SourceGeneration::new("source:build").unwrap(),
            GraphWatermark::new("watermark:build").unwrap(),
            Vec::new(),
            entity_rows,
            relation_rows,
        )
        .unwrap()
    }

    fn open_source(database_path: &std::path::Path) -> crate::GraphDbLeaseV1 {
        let owner = GraphDbOwner::open(GraphDbOpenOptions {
            location: GraphDbLocation::Persistent(database_path.to_path_buf()),
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::WalSync,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap();
        owner.issue_lease().unwrap()
    }

    /// The direct build holds the rows' topology and one column at a time,
    /// not every pushed value: its peak above the resident manifest stays
    /// within a fixed budget for a 30,000-entity, 45,000-relation generation,
    /// and the column spool it used is gone once the container is written.
    /// Holding every value until the columnar store was encoded peaked at
    /// 89,678,855 bytes on this generation.
    #[test]
    fn direct_build_holds_one_column_not_every_value() {
        const PEAK_BUDGET_BYTES: usize = 30_000_000;
        let check: &dyn Fn() -> Result<(), GraphDbError> = &|| Ok(());
        let manifest = manifest(30_000, 45_000);
        let identity = manifest.identity();
        let staging = tempfile::tempdir().unwrap();

        let (built, peak) = crate::thread_allocation::peak_above_start(|| {
            build_sealed_container(
                SealedRowSource::Manifest(&manifest),
                &identity,
                staging.path(),
                check,
            )
        });
        eprintln!("SEALED BUILD peak {peak}");

        assert_eq!(built.unwrap(), (30_000, 45_000));
        assert!(
            peak <= PEAK_BUDGET_BYTES,
            "the sealed build held {peak} bytes at peak, over its {PEAK_BUDGET_BYTES}-byte budget"
        );
        let left: Vec<_> = std::fs::read_dir(staging.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(
            left,
            vec![std::ffi::OsString::from(SEALED_STORE_DATABASE_FILE)]
        );
    }

    /// A reopened sealed generation serves its rows in place: opening it
    /// leaves a small fraction of the container resident, a point read
    /// answers exactly, and a page that fails verification refuses the read
    /// that touched it as corrupt instead of answering without the page.
    /// Opening one used to decode every dictionary string and rebuild every
    /// id map and property index, leaving more than the container resident.
    #[test]
    fn a_reopened_sealed_generation_serves_its_rows_in_place() {
        let check: &dyn Fn() -> Result<(), GraphDbError> = &|| Ok(());
        let manifest = manifest(30_000, 45_000);
        let identity = manifest.identity();
        let namespace = identity.physical_namespace().unwrap();
        let staging = tempfile::tempdir().unwrap();
        build_sealed_container(
            SealedRowSource::Manifest(&manifest),
            &identity,
            staging.path(),
            check,
        )
        .unwrap();
        let path = staging.path().join(SEALED_STORE_DATABASE_FILE);
        let container = std::fs::metadata(&path).unwrap().len();
        let open = || {
            GraphDb::open_lazy_with_store_state(
                sealed_artifact_database_options(path.clone()),
                PersistentGraphStoreState::Existing,
            )
            .unwrap()
        };
        let name = GraphPropertyName::new("name").unwrap();

        let database = open();
        let entity = database
            .entity(
                &namespace,
                &entity_identity(12_345),
                Arc::new(NeverCancelled),
            )
            .unwrap()
            .expect("the sealed entity reads back");
        assert_eq!(
            entity.properties.get(&name),
            Some(&GraphProperty::String("fn_12345".to_owned()))
        );
        let resident = database.resident_engine_bytes().unwrap().unwrap();
        assert!(
            resident * 4 < container,
            "opening a {container}-byte sealed container left {resident} bytes resident"
        );
        database.close().unwrap();
        drop(database);

        corrupt_stored_name(&path);

        let database = open();
        let refused = database.entity(
            &namespace,
            &entity_identity(12_345),
            Arc::new(NeverCancelled),
        );
        assert!(
            matches!(&refused, Err(GraphDbError::Corrupt { message }) if message.contains("CRC mismatch")),
            "a read over a corrupt sealed page must be refused as corrupt: {refused:?}"
        );
        let later = database.entity(&namespace, &entity_identity(7), Arc::new(NeverCancelled));
        assert!(
            matches!(later, Err(GraphDbError::Corrupt { .. })),
            "once a page fails, the open serves no further reads: {later:?}"
        );
    }

    fn corrupt_stored_name(path: &std::path::Path) {
        let mut bytes = std::fs::read(path).unwrap();
        let at: Vec<usize> = bytes
            .windows(8)
            .enumerate()
            .filter(|(_, window)| *window == b"fn_12345")
            .map(|(at, _)| at)
            .collect();
        assert_eq!(at.len(), 1, "the name is stored once, in its dictionary");
        bytes[at[0] + 3] ^= 0x01;
        std::fs::write(path, &bytes).unwrap();
    }

    /// The read that first touches a corrupt sealed page is refused, not
    /// answered without that page: a fresh open's target visit and its copy
    /// proof each fail as corrupt on their first read.
    #[test]
    fn the_first_read_over_a_corrupt_sealed_page_is_refused() {
        let check: &dyn Fn() -> Result<(), GraphDbError> = &|| Ok(());
        let manifest = manifest(30_000, 45_000);
        let identity = manifest.identity();
        let expected = manifest.expected_recovered_digest(check).unwrap();
        let namespace = identity.physical_namespace().unwrap();
        let staging = tempfile::tempdir().unwrap();
        build_sealed_container(
            SealedRowSource::Manifest(&manifest),
            &identity,
            staging.path(),
            check,
        )
        .unwrap();
        let path = staging.path().join(SEALED_STORE_DATABASE_FILE);
        corrupt_stored_name(&path);
        let open = || {
            GraphDb::open_lazy_with_store_state(
                sealed_artifact_database_options(path.clone()),
                PersistentGraphStoreState::Existing,
            )
            .unwrap()
        };
        let calls = BTreeSet::from([GraphRelationKind::new("calls").unwrap()]);

        let mut targets = Vec::new();
        let visited = open().visit_outgoing_relation_targets(
            &namespace,
            &entity_identity(12_344),
            &calls,
            Arc::new(NeverCancelled),
            &mut |target| targets.push(target),
        );
        let proof = sealed_copy_proof(&open(), &identity, &expected, check);
        assert!(
            matches!(visited, Err(GraphDbError::Corrupt { .. })),
            "a target visit over a corrupt sealed page must be refused: {visited:?} {targets:?} (copy proof: {proof:?})"
        );
        assert!(
            targets.is_empty(),
            "a refused visit must not hand any target to the visitor: {targets:?}"
        );
        assert!(
            matches!(proof, Err(GraphDbError::Corrupt { .. })),
            "a copy proof over a corrupt sealed page must be refused as corrupt: {proof:?}"
        );
    }

    /// The sealed container is written once, complete, after every row has
    /// been encoded: no engine, WAL, or partial container exists under the
    /// staging directory while rows stream. A build cancelled mid-copy leaves
    /// neither an artifact nor a staging directory behind, and the next
    /// attempt rebuilds from the source rows and proves the reopened artifact
    /// against the same digest.
    #[test]
    fn interrupted_build_leaves_nothing_and_a_completed_build_is_written_once() {
        let check: &dyn Fn() -> Result<(), GraphDbError> = &|| Ok(());
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("source.grafeo");
        let database = open_source(&database_path);
        let manifest = manifest(9_000, 9_000);
        let identity = manifest.identity();
        let expected = manifest.expected_recovered_digest(check).unwrap();
        database
            .apply_generation_unverified_with_digest(Arc::new(manifest), &expected, check)
            .unwrap();

        let root = sealed_store_root(&database_path);
        let directory = sealed_generation_directory(&root, &identity.physical_namespace().unwrap());
        let staging = root.join(format!(
            ".staging-{}",
            directory.file_name().unwrap().to_str().unwrap()
        ));
        let container = staging.join(SEALED_STORE_DATABASE_FILE);

        // Cancel once the copy is well inside the row stream. Until the final
        // write, the staging directory holds no container at all.
        let checks = AtomicUsize::new(0);
        let cancel_mid_copy = || {
            let count = checks.fetch_add(1, Ordering::Relaxed);
            assert!(
                !container.exists(),
                "no container may exist before every row is encoded"
            );
            if count >= 12_000 {
                return Err(GraphDbError::Cancelled);
            }
            Ok(())
        };
        let interrupted = build_or_open_sealed_store(
            SealedRowSource::Staging(&database),
            &identity,
            &expected,
            &database_path,
            &cancel_mid_copy,
        );
        assert!(
            matches!(interrupted, Err(GraphDbError::Cancelled)),
            "mid-copy cancellation must surface typed: {interrupted:?}"
        );
        assert!(
            checks.load(Ordering::Relaxed) > 12_000,
            "the cancellation must have fired inside the row stream"
        );
        assert!(
            !staging.exists() && !directory.exists(),
            "an interrupted build must leave neither its staging directory nor an artifact"
        );

        let (store, staging_proof) = build_or_open_sealed_store(
            SealedRowSource::Staging(&database),
            &identity,
            &expected,
            &database_path,
            check,
        )
        .unwrap();
        assert!(staging_proof.is_some(), "a fresh build carries its proof");
        assert_eq!(store.recovered_digest(), expected.as_str());
        assert_eq!((store.entity_count, store.relation_count), (9_000, 9_000));
        let artifact = directory.join(SEALED_STORE_DATABASE_FILE);
        assert!(artifact.is_file());
        assert!(
            !staging.exists(),
            "a completed build renames its staging directory into place"
        );
        let receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.join("sealed.json")).unwrap()).unwrap();
        assert_eq!(receipt["form"], "compact");
        let _ = store.database().close();

        // Exact container identity across reopen: adoption, proof, serving
        // reads, and close never rewrite a byte of the sealed file.
        let written = std::fs::read(&artifact).unwrap();
        let (adopted, adopted_proof) = build_or_open_sealed_store(
            SealedRowSource::Staging(&database),
            &identity,
            &expected,
            &database_path,
            check,
        )
        .unwrap();
        assert!(
            adopted_proof.is_none(),
            "adoption never re-enumerates staging rows"
        );
        let served = adopted.database().read_guard().unwrap();
        let native = served.as_ref().unwrap();
        let (entities, relations) = crate::state::projection_node_counts(
            native,
            &identity.physical_namespace().unwrap(),
            &identity.projection.projection,
        )
        .unwrap();
        assert_eq!((entities, relations), (9_000, 9_000));
        drop(served);
        let _ = adopted.database().close();
        assert_eq!(
            std::fs::read(&artifact).unwrap(),
            written,
            "reopen, proof, reads, and close must leave the sealed bytes exactly as written"
        );
    }

    /// Two builds of the same staged rows write byte-identical section
    /// payloads: the build order is the recovered-digest order, and the
    /// compact serializer is deterministic, so every byte past the Grafeo
    /// file/DB headers (which carry the creation and checkpoint timestamps by
    /// format design) is a function of the generation alone.
    #[test]
    fn rebuilding_the_same_generation_writes_identical_bytes() {
        fn payload(bytes: &[u8]) -> &[u8] {
            let data_offset = usize::try_from(grafeo_storage::file::format::DATA_OFFSET).unwrap();
            assert!(
                bytes.len() > data_offset,
                "container holds no section payload"
            );
            &bytes[data_offset..]
        }
        let check: &dyn Fn() -> Result<(), GraphDbError> = &|| Ok(());
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("source.grafeo");
        let database = open_source(&database_path);
        let manifest = manifest(2_000, 3_000);
        let identity = manifest.identity();
        let expected = manifest.expected_recovered_digest(check).unwrap();
        database
            .apply_generation_unverified_with_digest(Arc::new(manifest), &expected, check)
            .unwrap();
        let directory = sealed_generation_directory(
            &sealed_store_root(&database_path),
            &identity.physical_namespace().unwrap(),
        );
        let artifact = directory.join(SEALED_STORE_DATABASE_FILE);

        let (first, _) = build_or_open_sealed_store(
            SealedRowSource::Staging(&database),
            &identity,
            &expected,
            &database_path,
            check,
        )
        .unwrap();
        let _ = first.database().close();
        let first_bytes = std::fs::read(&artifact).unwrap();
        std::fs::remove_dir_all(&directory).unwrap();
        let (second, _) = build_or_open_sealed_store(
            SealedRowSource::Staging(&database),
            &identity,
            &expected,
            &database_path,
            check,
        )
        .unwrap();
        let _ = second.database().close();
        let second_bytes = std::fs::read(&artifact).unwrap();
        assert_eq!(second_bytes.len(), first_bytes.len());
        assert!(
            payload(&second_bytes) == payload(&first_bytes),
            "sealed section payloads differ between two builds of one generation"
        );
    }

    /// The direct build's failure boundary and its identity with the staged
    /// build. A build cancelled inside the manifest's row stream leaves
    /// neither a staging directory nor an artifact; a crash image (a leftover
    /// staging directory holding a partial container) is cleared by the next
    /// attempt, which rebuilds from the manifest alone and proves the
    /// reopened artifact against the same digest; and the container it
    /// writes carries the same section payload the staged copy of the same
    /// rows writes, so the sealed identity does not depend on which source
    /// built it.
    #[test]
    fn direct_build_recovers_from_interruption_and_matches_the_staged_build() {
        fn payload(bytes: &[u8]) -> &[u8] {
            let data_offset = usize::try_from(grafeo_storage::file::format::DATA_OFFSET).unwrap();
            assert!(
                bytes.len() > data_offset,
                "container holds no section payload"
            );
            &bytes[data_offset..]
        }
        let check: &dyn Fn() -> Result<(), GraphDbError> = &|| Ok(());
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("source.grafeo");
        let manifest = manifest(2_000, 3_000);
        let identity = manifest.identity();
        let expected = manifest.expected_recovered_digest(check).unwrap();
        let root = sealed_store_root(&database_path);
        let directory = sealed_generation_directory(&root, &identity.physical_namespace().unwrap());
        let staging = root.join(format!(
            ".staging-{}",
            directory.file_name().unwrap().to_str().unwrap()
        ));
        let artifact = directory.join(SEALED_STORE_DATABASE_FILE);

        // Cancellation inside the row stream: nothing is left behind.
        let checks = AtomicUsize::new(0);
        let cancel_mid_build = || {
            if checks.fetch_add(1, Ordering::Relaxed) >= 2_500 {
                return Err(GraphDbError::Cancelled);
            }
            Ok(())
        };
        let interrupted = build_or_open_sealed_store(
            SealedRowSource::Manifest(&manifest),
            &identity,
            &expected,
            &database_path,
            &cancel_mid_build,
        );
        assert!(
            matches!(interrupted, Err(GraphDbError::Cancelled)),
            "mid-build cancellation must surface typed: {interrupted:?}"
        );
        assert!(
            !staging.exists() && !directory.exists(),
            "an interrupted direct build must leave neither its staging directory nor an artifact"
        );

        // A crash image: the process died after creating the staging
        // directory and part of a container. The next attempt owns that
        // directory and rebuilds from the manifest.
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join(SEALED_STORE_DATABASE_FILE), b"torn container").unwrap();
        let (direct, staging_proof) = build_or_open_sealed_store(
            SealedRowSource::Manifest(&manifest),
            &identity,
            &expected,
            &database_path,
            check,
        )
        .unwrap();
        assert!(
            staging_proof.is_none(),
            "a manifest-sourced build proves nothing about the staging database"
        );
        assert_eq!(direct.recovered_digest(), expected.as_str());
        assert_eq!((direct.entity_count, direct.relation_count), (2_000, 3_000));
        assert!(
            !staging.exists(),
            "the crash image must be replaced, not kept"
        );
        let _ = direct.database().close();
        let direct_bytes = std::fs::read(&artifact).unwrap();
        std::fs::remove_dir_all(&directory).unwrap();

        // The same rows, staged and copied out of the staging database.
        let database = open_source(&database_path);
        database
            .apply_generation_unverified_with_digest(Arc::new(manifest.clone()), &expected, check)
            .unwrap();
        let (staged, staged_proof) = build_or_open_sealed_store(
            SealedRowSource::Staging(&database),
            &identity,
            &expected,
            &database_path,
            check,
        )
        .unwrap();
        assert!(staged_proof.is_some());
        let _ = staged.database().close();
        let staged_bytes = std::fs::read(&artifact).unwrap();
        assert_eq!(staged_bytes.len(), direct_bytes.len());
        assert!(
            payload(&staged_bytes) == payload(&direct_bytes),
            "the direct and staged builds of one generation must write the same sealed payload"
        );
        std::fs::remove_dir_all(&directory).unwrap();

        // Preparation may fan out, but the canonical push order and compact
        // section bytes remain identical to the serial direct build.
        let pool = ThreadPoolBuilder::new().num_threads(4).build().unwrap();
        let (parallel, _) = pool
            .install(|| {
                build_or_open_sealed_store(
                    SealedRowSource::Manifest(&manifest),
                    &identity,
                    &expected,
                    &database_path,
                    &|| Ok(()),
                )
            })
            .unwrap();
        let _ = parallel.database().close();
        let parallel_bytes = std::fs::read(&artifact).unwrap();
        assert_eq!(parallel_bytes.len(), direct_bytes.len());
        assert!(
            payload(&parallel_bytes) == payload(&direct_bytes),
            "parallel and serial direct builds must write the same sealed payload"
        );
    }
}

#[cfg(test)]
mod hibernation_tests {
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use super::SealedGenerationStore;
    use crate::lease::GenerationLocator;
    use crate::location::PersistentGraphStoreState;
    use crate::{
        GraphDb, GraphDbLocation, GraphDbOpenOptions, GraphDurability, GraphFormatVersion,
        GraphGenerationId, GraphNamespace, GraphProjectionId, GraphProjectionIdentity,
        NeverCancelled,
    };

    #[test]
    fn bounded_reaper_retries_a_sealed_engine_skipped_while_its_reader_was_busy() {
        let temp = tempfile::tempdir().unwrap();
        let child_path = temp.path().join("sealed-reader.grafeo");
        let child_options = || GraphDbOpenOptions {
            location: GraphDbLocation::Persistent(child_path.clone()),
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::WalSync,
            cancellation: Arc::new(NeverCancelled),
        };
        let created = GraphDb::open(child_options()).unwrap();
        created.close().unwrap();
        drop(created);
        let child = GraphDb::open_lazy_with_store_state(
            child_options(),
            PersistentGraphStoreState::Existing,
        )
        .unwrap();
        child.ensure_opened().unwrap();

        let parent = GraphDb::open(GraphDbOpenOptions {
            location: GraphDbLocation::Memory,
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::Memory,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap();
        let locator = GenerationLocator::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new("sealed-reader-retry").unwrap(),
                GraphProjectionId::new("code").unwrap(),
            ),
            GraphGenerationId::new("g1").unwrap(),
        );
        parent.inner.sealed_generations.write().unwrap().insert(
            locator.clone(),
            Arc::new(SealedGenerationStore {
                identity: crate::GraphGenerationManifestIdentity::new(
                    locator.projection.clone(),
                    locator.generation.clone(),
                    crate::SourceGeneration::new("source-g1").unwrap(),
                    crate::GraphWatermark::new("watermark-g1").unwrap(),
                    Vec::new(),
                ),
                locator,
                recovered_digest: format!("sha256:{}", "a".repeat(64)),
                entity_count: 0,
                relation_count: 0,
                canonical_bytes: 0,
                directory: temp.path().join("sealed-reader"),
                database: Arc::clone(&child),
                row_sum: None,
                layer: None,
            }),
        );

        let active_reader = child.read_guard().unwrap();
        let retry_parent = Arc::clone(&parent);
        let (result_tx, result_rx) = mpsc::channel();
        let attempt = std::thread::spawn(move || {
            result_tx
                .send(retry_parent.reap_idle_sealed_generation_engines(None))
                .unwrap();
        });
        let busy_result = result_rx.recv_timeout(Duration::from_millis(100));
        drop(active_reader);
        attempt.join().unwrap();
        assert_eq!(
            busy_result,
            Ok(0),
            "the bounded pass must return without waiting for an active database reader"
        );
        assert!(child.native_engine_open().unwrap());

        assert_eq!(
            parent.reap_idle_sealed_generation_engines(None),
            1,
            "the next maintenance pass must retry the reader after it becomes idle"
        );
        assert!(!child.native_engine_open().unwrap());
    }
}

/// Measurement harness for the sealed-store verification path, phase by
/// phase, against production-shaped rows (a Bytes payload on every entity by
/// default, or a String payload when `TRACEDECAY_VERIFY_PROBE_FORM=compact`).
///
/// ```text
/// TRACEDECAY_VERIFY_PROBE_ROWS=50000 TRACEDECAY_VERIFY_PROBE_PAYLOAD=700 \
///   cargo test -p tracedecay-graph-db --profile perf --lib -- --ignored --nocapture \
///   sealed_store::cost_probe::sealed_verification_cost_probe
/// ```
#[cfg(test)]
mod cost_probe {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;
    use std::time::Instant;

    use super::{SealedRowSource, build_or_open_sealed_store, open_sealed_store};
    use crate::generation::verify_recovered_generation;
    use crate::{
        GraphDbLocation, GraphDbOpenOptions, GraphDbOwner, GraphDurability, GraphEntity,
        GraphEntityId, GraphEntityRef, GraphFormatVersion, GraphGenerationId,
        GraphGenerationManifest, GraphGenerationRelation, GraphLabel, GraphNamespace,
        GraphProjectionId, GraphProjectionIdentity, GraphProperty, GraphPropertyName,
        GraphRelationId, GraphRelationKind, GraphWatermark, NeverCancelled, SourceGeneration,
    };

    fn env_usize(name: &str, default: usize) -> usize {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    }

    fn property_name(name: &str) -> GraphPropertyName {
        GraphPropertyName::new(name).unwrap()
    }

    fn entity_identity(index: usize) -> GraphEntityId {
        GraphEntityId::new(format!("symbol:{index:07}")).unwrap()
    }

    /// Deterministic pseudo-random payload so runs are reproducible and the
    /// JSON number-array encoding sees realistic digit-length dispersion.
    fn payload_bytes(seed: usize, len: usize) -> Vec<u8> {
        let mut state = seed as u64 ^ 0x9e37_79b9_7f4a_7c15;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                (state >> 33) as u8
            })
            .collect()
    }

    fn probe_manifest(
        entities: usize,
        relations: usize,
        payload: usize,
        bytes_payload: bool,
    ) -> GraphGenerationManifest {
        let projection = GraphProjectionIdentity::new(
            GraphNamespace::new("sealed-cost-probe").unwrap(),
            GraphProjectionId::new("code").unwrap(),
        );
        let entity_rows = (0..entities)
            .map(|index| {
                let payload_property = if bytes_payload {
                    GraphProperty::Bytes(payload_bytes(index, payload))
                } else {
                    // Sized so the canonical JSON frame roughly matches the
                    // Bytes number-array encoding (~3.7 chars per byte).
                    GraphProperty::String("x".repeat(payload.saturating_mul(37) / 10))
                };
                GraphEntity::new(
                    entity_identity(index),
                    BTreeSet::from([
                        GraphLabel::new("function").unwrap(),
                        GraphLabel::new(format!("bucket-{}", index % 7)).unwrap(),
                    ]),
                    BTreeMap::from([
                        (
                            property_name("name"),
                            GraphProperty::String(format!("fn_symbol_{index:07}")),
                        ),
                        (
                            property_name("path"),
                            GraphProperty::String(format!(
                                "crates/probe/src/module_{:03}/file_{:04}.rs",
                                index % 251,
                                index % 4093
                            )),
                        ),
                        (
                            property_name("arity"),
                            GraphProperty::I64((index % 9) as i64),
                        ),
                        (
                            property_name("exported"),
                            GraphProperty::Bool(index % 3 == 0),
                        ),
                        (property_name("payload"), payload_property),
                    ]),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let entity_ref =
            |index: usize| GraphEntityRef::new(projection.clone(), entity_identity(index));
        let relation_rows = (0..relations)
            .map(|index| {
                let from = index % entities.max(1);
                // Hub-heavy mix: every eighth edge points at entity 0, the
                // rest chain, mirroring call-graph endpoint reuse.
                let to = if index % 8 == 0 {
                    0
                } else {
                    (from + 1) % entities.max(1)
                };
                GraphGenerationRelation::new(
                    GraphRelationId::new(format!("call:{index:07}")).unwrap(),
                    entity_ref(from),
                    entity_ref(to),
                    GraphRelationKind::new("calls").unwrap(),
                    BTreeMap::from([(property_name("weight"), GraphProperty::I64(index as i64))]),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        GraphGenerationManifest::new(
            projection,
            GraphGenerationId::new("generation:cost-probe").unwrap(),
            SourceGeneration::new("source:cost-probe").unwrap(),
            GraphWatermark::new("watermark:cost-probe").unwrap(),
            Vec::new(),
            entity_rows,
            relation_rows,
        )
        .unwrap()
    }

    fn directory_bytes(path: &std::path::Path) -> u64 {
        let Ok(entries) = std::fs::read_dir(path) else {
            return std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
        };
        entries
            .filter_map(Result::ok)
            .map(|entry| {
                let path = entry.path();
                if path.is_dir() {
                    directory_bytes(&path)
                } else {
                    std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0)
                }
            })
            .sum()
    }

    fn gib_per_second(bytes: u64, seconds: f64) -> f64 {
        if seconds <= 0.0 {
            return 0.0;
        }
        bytes as f64 / (1024.0 * 1024.0 * 1024.0) / seconds
    }

    fn seconds_per_gib(bytes: u64, seconds: f64) -> f64 {
        let gib = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        if gib <= 0.0 {
            return 0.0;
        }
        seconds / gib
    }

    #[test]
    #[ignore = "measurement harness; see module doc"]
    fn sealed_verification_cost_probe() {
        let entities = env_usize("TRACEDECAY_VERIFY_PROBE_ROWS", 50_000);
        let relations = entities.saturating_mul(8) / 7;
        let payload = env_usize("TRACEDECAY_VERIFY_PROBE_PAYLOAD", 700);
        let bytes_payload = std::env::var("TRACEDECAY_VERIFY_PROBE_FORM")
            .map(|form| form != "compact")
            .unwrap_or(true);
        let check: &dyn Fn() -> Result<(), crate::GraphDbError> = &|| Ok(());

        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("probe.grafeo");
        let owner = GraphDbOwner::open(GraphDbOpenOptions {
            location: GraphDbLocation::Persistent(database_path.clone()),
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::WalSync,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap();
        let database = owner.issue_lease().unwrap();

        let built = Instant::now();
        let manifest = probe_manifest(entities, relations, payload, bytes_payload);
        let identity = manifest.identity();
        let manifest_build_s = built.elapsed().as_secs_f64();

        let started = Instant::now();
        let expected = manifest.expected_recovered_digest(check).unwrap();
        let manifest_digest_s = started.elapsed().as_secs_f64();

        let started = Instant::now();
        database
            .apply_generation_unverified_with_digest(Arc::new(manifest), &expected, check)
            .unwrap();
        let stage_s = started.elapsed().as_secs_f64();
        // The serial full proof, exactly as every open before the parallel
        // pipeline streamed it.
        let started = Instant::now();
        let serial_bytes = {
            let guard = database.read_guard().unwrap();
            let native = guard.as_ref().unwrap();
            crate::generation::recovered_generation_digest_chunked(
                native,
                &identity,
                check,
                usize::MAX,
            )
            .unwrap()
            .1
        };
        let serial_proof_s = started.elapsed().as_secs_f64();

        // The pure full proof through the production entry, exactly as the
        // publication path streams it over the staging rows.
        let started = Instant::now();
        let canonical_bytes = {
            let guard = database.read_guard().unwrap();
            let native = guard.as_ref().unwrap();
            verify_recovered_generation(native, &identity, &expected, check)
                .unwrap()
                .1
        };
        let staging_proof_s = started.elapsed().as_secs_f64();
        assert_eq!(serial_bytes, canonical_bytes);

        // The sealed build: copy + (compact | replay) + durable close +
        // reopen + full post-reopen proof.
        let started = Instant::now();
        let (store, staging_proof) = build_or_open_sealed_store(
            SealedRowSource::Staging(&database),
            &identity,
            &expected,
            &database_path,
            check,
        )
        .unwrap();
        let build_s = started.elapsed().as_secs_f64();
        assert!(staging_proof.is_some(), "fresh build must carry its proof");
        let directory = store.directory.clone();
        let _ = store.database().close();
        drop(store);

        // Reopen + full proof in isolation: drop the verify-once marker so
        // the open pays the entire row proof, as a foreign host adopting the
        // artifact (or any container whose identity moved) would.
        std::fs::remove_file(directory.join("generation.verified")).unwrap();
        let started = Instant::now();
        let reopened = open_sealed_store(&directory, &identity, &expected)
            .unwrap()
            .unwrap();
        let reopen_full_proof_s = started.elapsed().as_secs_f64();
        let _ = reopened.database().close();
        drop(reopened);

        // Reopen resolved by marker: what every later boot of unchanged
        // bytes pays.
        let started = Instant::now();
        let marker_hit = open_sealed_store(&directory, &identity, &expected)
            .unwrap()
            .unwrap();
        let marker_reopen_s = started.elapsed().as_secs_f64();
        let _ = marker_hit.database().close();
        drop(marker_hit);

        let staging_bytes = directory_bytes(&database_path)
            + directory_bytes(&database_path.with_extension("grafeo.wal"));
        let artifact_bytes = directory_bytes(&directory);
        let receipt = std::fs::read_to_string(directory.join("sealed.json")).unwrap();
        let form = receipt
            .split("\"form\": \"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or("unknown");

        println!("=== sealed verification cost probe ===");
        println!(
            "rows                    : {entities} entities + {relations} relations \
             (payload {payload}B, form {form})"
        );
        println!(
            "canonical proof stream  : {canonical_bytes} bytes ({:.2} GiB)",
            canonical_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
        );
        println!(
            "staging store           : {:.1} MiB; sealed artifact: {:.1} MiB",
            staging_bytes as f64 / (1024.0 * 1024.0),
            artifact_bytes as f64 / (1024.0 * 1024.0)
        );
        println!("manifest build          : {manifest_build_s:.2}s");
        println!(
            "manifest digest (seal)  : {manifest_digest_s:.2}s ({:.3} GiB/s canonical)",
            gib_per_second(canonical_bytes, manifest_digest_s)
        );
        println!("stage (durable pages)   : {stage_s:.2}s");
        println!(
            "staging proof (serial)  : {serial_proof_s:.2}s ({:.3} GiB/s canonical, \
             {:.0} rows/s)",
            gib_per_second(canonical_bytes, serial_proof_s),
            (entities + relations) as f64 / serial_proof_s
        );
        println!(
            "staging full proof      : {staging_proof_s:.2}s ({:.3} GiB/s canonical, \
             {:.0} rows/s)",
            gib_per_second(canonical_bytes, staging_proof_s),
            (entities + relations) as f64 / staging_proof_s
        );
        println!("sealed build (copy+close+reopen+proof): {build_s:.2}s");
        println!(
            "reopen + full proof     : {reopen_full_proof_s:.2}s ({:.3} GiB/s canonical, \
             {:.0} rows/s)",
            gib_per_second(canonical_bytes, reopen_full_proof_s),
            (entities + relations) as f64 / reopen_full_proof_s
        );
        println!("reopen via marker       : {marker_reopen_s:.3}s");
        println!(
            "seconds/canonical GiB (parallel staging proof): {:.1}",
            staging_proof_s / (canonical_bytes as f64 / (1024.0 * 1024.0 * 1024.0))
        );
        println!(
            "seconds/physical GiB  (serial staging proof): {:.1}",
            seconds_per_gib(staging_bytes, serial_proof_s)
        );
        println!(
            "seconds/physical GiB  (parallel staging proof): {:.1}",
            seconds_per_gib(staging_bytes, staging_proof_s)
        );
        println!(
            "seconds/physical GiB  (sealed reopen proof): {:.1}",
            seconds_per_gib(artifact_bytes, reopen_full_proof_s)
        );
    }
}
