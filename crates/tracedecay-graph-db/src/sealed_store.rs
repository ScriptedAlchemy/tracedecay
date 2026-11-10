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
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use grafeo_common::types::{EdgeId, NodeId, PropertyKey, Value};
use grafeo_core::graph::compact::IncrementalCompactStoreBuilder;
use grafeo_engine::GrafeoDB;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tracedecay_private_fs::capability_dir::install_child_directory_noreplace_nofollow;
use tracedecay_store::runtime::GraphRecoveredGenerationDigestV1;

use crate::generation::{
    AbandonedRowSpill, GraphRowDigestSum, MAX_ROW_SPILL_OWNER_RECEIPT_BYTES,
    ROW_SPILL_OWNER_RECEIPT_FILE, ROW_SPILL_PREFIX, physical_namespace_projection_map,
    recovered_digest_from_row_sum, row_spill_owner, verify_sealed_copy_generation,
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
    GraphLayeredRowSpill, GraphSealedBaseAbsenceV1, GraphSealedBaseV1, GraphSiblingSealedBaseV1,
    LayeredGraphGeneration, LayeredReads, SealedBaseFilesV1, SealedBaseReceiptV1, SealedLayer,
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
    let receipt_bytes = match read_sealed_receipt_bytes(&receipt_path) {
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
    // Lazily, for the same reason as `open_sealed_store`, and also so
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
/// A proof marker contains one canonical recovered digest, never a payload.
const MAX_SEALED_CHECK_BYTES: usize = 256;
/// Identity metadata, never an unbounded content read.
const MAX_SEALED_RECEIPT_BYTES: usize = 64 * 1024;

fn read_sealed_receipt_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
    // The shared reader opens without following a final symlink, rejects FIFOs
    // and other nonregular files, and checks size against the same held handle.
    match tracedecay_private_fs::framed_log::read_bounded(path, MAX_SEALED_RECEIPT_BYTES)? {
        Some(bytes) => Ok(bytes),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "sealed generation receipt is missing",
        )),
    }
}

pub(crate) const SEALED_STORE_DATABASE_FILE: &str = "generation.grafeo";
const SEALED_STORE_RECEIPT_FILE: &str = "sealed.json";
/// Small, generation-bound deletion authority persisted before any staged rows.
/// Unlike sealed.json, this is never evidence that a container is complete.
const SEALED_STAGING_OWNER_FILE: &str = ".sealed-staging-owner-v1";
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
    /// [`SEALED_STORE_FORM_COMPACT`] or [`SEALED_STORE_FORM_LAYERED`]; any
    /// other form is refused.
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

/// An incomplete sealed container belongs to one producer incarnation.
/// A remount in that process must not reclaim its in-flight row stream.
#[derive(Debug, Deserialize, Serialize)]
struct SealedStagingOwnerV1 {
    producer: String,
    receipt: SealedStoreReceiptV1,
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
        // Closing the same derived reader twice is harmless. A durability
        // failure is not: preserve the first real close failure even when a
        // later layered-base reader was already closed.
        let mut failure = None;
        for database in self.engines() {
            match database.close() {
                Ok(()) | Err(GraphDbError::Closed) => {}
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
            // An earlier failed engine.close() may have consumed its handle
            // and set Closed on the next attempt. Closed is idempotent only
            // when the underlying reader is not poisoned.
            if database.inner.poisoned.load(AtomicOrdering::Acquire) {
                failure.get_or_insert_with(|| GraphDbError::DurabilityUncertain {
                    message: "sealed reader physical close remains uncertain".to_owned(),
                });
            }
        }
        failure.map_or(Ok(()), Err)
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

    /// Retirement tears down the reader and requires a successful artifact
    /// unlink. A failed unlink remains a typed error for the durable cleanup
    /// owner to retry; a sealed-only live head never enters this path.
    fn discard(&self) -> Result<(), GraphDbError> {
        // An unconfirmed physical close cannot authorize unlinking the only
        // durable sealed copy. Leave its receipt for a later retirement retry.
        self.close()?;
        retire_sealed_directory_checked(
            &self.directory,
            &self.locator,
            Some(&self.recovered_digest),
        )
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
    /// Receipt-verified `.staging-*` directories belonging to earlier producer
    /// incarnations; current or unproven staging cannot be called abandoned.
    pub abandoned_staging_count: u64,
    pub abandoned_staging_bytes: u64,
    /// Unrecognized or unproven entries (including live staging) are reported
    /// without walking their contents or claiming that they can be reclaimed.
    pub unrecognized_count: u64,
}

/// Measure the sealed root beside `database_path` against the generation ids
/// of the store's current verified heads (as journaled: `<projection>:<digest>`).
///
/// Metadata only: every entry is one receipt read plus a directory size walk,
/// never a container open. An unreadable receipt counts as unrecognized so the
/// census stays truthful when a receipt is mid-write or corrupt.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum SealedCensusEntry {
    Head,
    Superseded,
    AbandonedStaging,
    Unrecognized,
}

pub fn census_sealed_store(
    database_path: &Path,
    head_generations: &std::collections::BTreeSet<String>,
) -> std::io::Result<SealedStoreCensusV1> {
    let root = sealed_store_root(database_path);
    let current_producer = row_spill_owner().to_string();
    let totals = tracedecay_private_fs::capability_dir::census_receipted_directory_bytes(
        &root,
        SealedCensusEntry::Unrecognized,
        |name| {
            if name.to_str().is_some_and(sealed_staging_name) {
                (SEALED_STAGING_OWNER_FILE, MAX_SEALED_RECEIPT_BYTES)
            } else {
                (SEALED_STORE_RECEIPT_FILE, MAX_SEALED_RECEIPT_BYTES)
            }
        },
        |name, bytes| {
            let Some(bytes) = bytes else {
                return SealedCensusEntry::Unrecognized;
            };
            if name.to_str().is_some_and(sealed_staging_name) {
                return if verified_staging_receipt(name, bytes)
                    .is_some_and(|owner| owner.producer != current_producer)
                {
                    SealedCensusEntry::AbandonedStaging
                } else {
                    SealedCensusEntry::Unrecognized
                };
            }
            let Some(name) = name.to_str() else {
                return SealedCensusEntry::Unrecognized;
            };
            let Ok(receipt) = serde_json::from_slice::<SealedStoreReceiptV1>(bytes) else {
                return SealedCensusEntry::Unrecognized;
            };
            let form_valid = match receipt.form.as_str() {
                SEALED_STORE_FORM_COMPACT => receipt.base.is_none(),
                SEALED_STORE_FORM_LAYERED => receipt.base.is_some(),
                _ => false,
            };
            if !form_valid
                || receipt.version != SEALED_STORE_RECEIPT_VERSION
                || receipt.graph_format == 0
                || receipt.graph_format > GraphFormatVersion::current().get()
                || receipt.recovered_digest.is_empty()
                || !sealed_receipt_matches_generation_entry(&receipt, name)
            {
                return SealedCensusEntry::Unrecognized;
            }
            if head_generations.contains(&receipt.generation) {
                SealedCensusEntry::Head
            } else {
                SealedCensusEntry::Superseded
            }
        },
    )?;
    let mut census = SealedStoreCensusV1::default();
    for (kind, (count, bytes)) in totals {
        match kind {
            SealedCensusEntry::Head => {
                census.head_count = count;
                census.head_bytes = bytes;
            }
            SealedCensusEntry::Superseded => {
                census.superseded_count = count;
                census.superseded_bytes = bytes;
            }
            SealedCensusEntry::AbandonedStaging => {
                census.abandoned_staging_count = count;
                census.abandoned_staging_bytes = bytes;
            }
            SealedCensusEntry::Unrecognized => census.unrecognized_count = count,
        }
    }
    Ok(census)
}

/// Removes every sealed generation beside `database_path` whose receipt does
/// not name the current graph format: after a superseded-format rebuild those
/// containers can never serve, while a generation this build already sealed
/// keeps serving. In-flight `.staging-*` seals are left to their builder.
pub(crate) fn discard_superseded_sealed_generations(
    database_path: &Path,
) -> Result<(), GraphDbError> {
    let root = sealed_store_root(database_path);
    let reclaimed = tracedecay_private_fs::capability_dir::remove_receipted_directories_nofollow(
        &root,
        std::ffi::OsStr::new(SEALED_STORE_RECEIPT_FILE),
        MAX_SEALED_RECEIPT_BYTES,
        |name, bytes| {
            let Some(name) = name.to_str() else {
                return false;
            };
            if name.len() != 64 || !name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return false;
            }
            let Ok(receipt) = serde_json::from_slice::<SealedStoreReceiptV1>(bytes) else {
                return false;
            };
            let recognized_form = match receipt.form.as_str() {
                SEALED_STORE_FORM_COMPACT => receipt.base.is_none(),
                SEALED_STORE_FORM_LAYERED => receipt.base.is_some(),
                _ => false,
            };
            recognized_form
                && receipt.version == SEALED_STORE_RECEIPT_VERSION
                && sealed_receipt_matches_generation_entry(&receipt, name)
                && !receipt.recovered_digest.is_empty()
                && receipt.graph_format > 0
                && receipt.graph_format < GraphFormatVersion::current().get()
        },
    );
    match reclaimed {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(sealed_store_io_failure(
            "superseded sealed generation removal failed",
            error,
        )),
    }
}

/// A physical generation entry is deletable only if the receipt's logical
/// locator derives that exact entry name. A copied receipt with a fabricated
/// physical namespace is not evidence of ownership.
fn sealed_receipt_matches_generation_entry(
    receipt: &SealedStoreReceiptV1,
    generation_hex: &str,
) -> bool {
    let (Ok(namespace), Ok(projection), Ok(generation)) = (
        GraphNamespace::new(receipt.namespace.clone()),
        GraphProjectionId::new(receipt.projection.clone()),
        crate::GraphGenerationId::new(receipt.generation.clone()),
    ) else {
        return false;
    };
    let locator = GenerationLocator::new(
        GraphProjectionIdentity::new(namespace, projection),
        generation,
    );
    locator.physical_namespace().is_ok_and(|physical| {
        physical.as_str() == receipt.physical_namespace
            && receipt.physical_namespace == format!("generation:{generation_hex}")
    })
}

fn sealed_staging_name(name: &str) -> bool {
    name.strip_prefix(".staging-").is_some_and(|suffix| {
        suffix.len() == 64
            && suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn initial_sealed_staging_owner(
    identity: &GraphGenerationManifestIdentity,
    expected: &GraphRecoveredGenerationDigestV1,
    physical_namespace: &str,
) -> SealedStoreReceiptV1 {
    // This receipt authorizes only abandoned-staging cleanup, never serving.
    // The completed container receives a separate sealed.json after writing.
    SealedStoreReceiptV1 {
        version: SEALED_STORE_RECEIPT_VERSION,
        form: SEALED_STORE_FORM_COMPACT.to_owned(),
        namespace: identity.projection.namespace.as_str().to_owned(),
        projection: identity.projection.projection.as_str().to_owned(),
        generation: identity.generation.as_str().to_owned(),
        physical_namespace: physical_namespace.to_owned(),
        recovered_digest: expected.as_str().to_owned(),
        entities: 0,
        relations: 0,
        graph_format: GraphFormatVersion::current().get(),
        row_sum: None,
        base: None,
        source_generation: None,
        watermark: None,
    }
}

fn verified_staging_receipt(name: &std::ffi::OsStr, bytes: &[u8]) -> Option<SealedStagingOwnerV1> {
    let name = name.to_str()?;
    if !sealed_staging_name(name) {
        return None;
    }
    let generation_hex = name.strip_prefix(".staging-")?;
    let owner = serde_json::from_slice::<SealedStagingOwnerV1>(bytes).ok()?;
    let instance = owner.producer.parse::<u128>().ok()?;
    if instance.to_string() != owner.producer {
        return None;
    }
    let receipt = &owner.receipt;
    let recognized_form = match receipt.form.as_str() {
        SEALED_STORE_FORM_COMPACT => receipt.base.is_none(),
        SEALED_STORE_FORM_LAYERED => receipt.base.is_some(),
        _ => false,
    };
    (recognized_form
        && receipt.version == SEALED_STORE_RECEIPT_VERSION
        && receipt.graph_format > 0
        && receipt.graph_format <= GraphFormatVersion::current().get()
        && sealed_receipt_matches_generation_entry(receipt, generation_hex)
        && !receipt.recovered_digest.is_empty())
    .then_some(owner)
}

/// A failed builder must never recursively delete a substituted directory
/// merely because its pathname is the deterministic staging name. Only the
/// producer-bound early marker authorizes cleanup, including after sealed.json
/// publication and before installation.
fn discard_failed_sealed_staging(
    staging: &Path,
    locator: &GenerationLocator,
    expected_digest: &str,
) {
    let Some((root, name)) = staging.parent().zip(staging.file_name()) else {
        return;
    };
    let result = tracedecay_private_fs::capability_dir::remove_receipted_child_directory_nofollow(
        root,
        name,
        std::ffi::OsStr::new(SEALED_STAGING_OWNER_FILE),
        MAX_SEALED_RECEIPT_BYTES,
        |bytes| {
            verified_staging_receipt(name, bytes).is_some_and(|owner| {
                let receipt = &owner.receipt;
                owner.producer == row_spill_owner().to_string()
                    && receipt.namespace == locator.projection.namespace.as_str()
                    && receipt.projection == locator.projection.projection.as_str()
                    && receipt.generation == locator.generation.as_str()
                    && receipt.recovered_digest == expected_digest
            })
        },
    );
    if !matches!(&result, Ok(true)) {
        tracing::warn!(
            event = "sealed_staging_cleanup_deferred",
            path = %staging.display(),
            ?result,
            "unverified staging preserved instead of deleting an unknown directory"
        );
    }
}

/// Reclaims interrupted seals only when the bounded receipt from the same
/// opened staging directory proves its exact generation identity. The staging
/// filename alone is not deletion authority: a missing, malformed, oversized,
/// or foreign receipt leaves the directory intact for inspection.
///
/// Store open owns exclusive maintenance admission. Capability-relative
/// traversal holds the root and child across verification and recursive removal,
/// so a pathname replacement cannot redirect deletion into a different tree.
pub(crate) fn sweep_abandoned_sealed_staging(database_path: &Path) {
    let root = sealed_store_root(database_path);
    // Every new sealed build writes this marker before rows and retains it
    // through installation. The final sealed.json is not a separate cleanup
    // authority: it cannot identify whether a concurrent build is still live.
    let reclaimed =
        tracedecay_private_fs::capability_dir::remove_matching_receipted_directories_nofollow(
            &root,
            std::ffi::OsStr::new(SEALED_STAGING_OWNER_FILE),
            MAX_SEALED_RECEIPT_BYTES,
            |name| name.to_str().is_some_and(sealed_staging_name),
            |name, bytes| {
                verified_staging_receipt(name, bytes)
                    .is_some_and(|owner| owner.producer != row_spill_owner().to_string())
            },
        );
    match reclaimed {
        Ok(removed) if removed > 0 => tracing::info!(
            event = "sealed_staging_swept",
            root = %root.display(),
            removed,
            "removed receipt-verified staging from exited producers"
        ),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            event = "sealed_staging_sweep_failed",
            root = %root.display(),
            %error,
            "unverified abandoned staging stays for inspection"
        ),
    }
}

/// Removes every row spill another process left under the store's sealed
/// root. Run at the eager open, whose exclusive store lock means no other
/// process is producing rows for this store. Spills named for this process
/// are skipped: a remount inside a live daemon may overlap its own publisher,
/// whose spill removes itself when it drops.
pub(crate) fn sweep_abandoned_row_spills(database_path: &Path) {
    let root = sealed_store_root(database_path);
    let current_owner = row_spill_owner();
    // Enumerate, verify and unlink through one held sealed-root capability:
    // a root rename between discovery and cleanup must not redirect deletion.
    let result =
        tracedecay_private_fs::capability_dir::remove_matching_receipted_directories_nofollow(
            &root,
            std::ffi::OsStr::new(ROW_SPILL_OWNER_RECEIPT_FILE),
            MAX_ROW_SPILL_OWNER_RECEIPT_BYTES,
            |name| AbandonedRowSpill::from_name(name, current_owner).is_some(),
            |name, receipt| {
                AbandonedRowSpill::from_name(name, current_owner)
                    .is_some_and(|proof| proof.owns(receipt))
            },
        );
    match result {
        Ok(removed) if removed > 0 => tracing::info!(
            event = "graph_row_spill_swept",
            root = %root.display(),
            removed,
            "reclaimed receipt-verified abandoned graph row spills"
        ),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => tracing::warn!(
            event = "graph_row_spill_sweep_failed",
            root = %root.display(),
            %error,
            "unverified abandoned graph row spills preserved for inspection"
        ),
    }
}

/// Accepted replay retirement is not permission to recursively delete a
/// substituted or unrecognizable artifact. Require the exact durable identity
/// under a held child capability; preserve an unverified directory for retry.
fn retire_sealed_directory_checked(
    directory: &Path,
    locator: &GenerationLocator,
    expected_digest: Option<&str>,
) -> Result<(), GraphDbError> {
    let physical = locator.physical_namespace()?;
    let root = directory
        .parent()
        .ok_or_else(|| GraphDbError::unavailable("sealed retirement directory has no parent"))?;
    if sealed_generation_directory(root, &physical) != directory {
        return Err(GraphDbError::unavailable(
            "sealed retirement directory does not match generation identity",
        ));
    }
    let name = directory.file_name().ok_or_else(|| {
        GraphDbError::unavailable("sealed retirement directory has no entry name")
    })?;
    let result = tracedecay_private_fs::capability_dir::remove_receipted_child_directory_nofollow(
        root,
        name,
        std::ffi::OsStr::new(SEALED_STORE_RECEIPT_FILE),
        MAX_SEALED_RECEIPT_BYTES,
        |bytes| {
            let Ok(receipt) = serde_json::from_slice::<SealedStoreReceiptV1>(bytes) else {
                return false;
            };
            let form_valid = match receipt.form.as_str() {
                SEALED_STORE_FORM_COMPACT => receipt.base.is_none(),
                SEALED_STORE_FORM_LAYERED => receipt.base.is_some(),
                _ => false,
            };
            form_valid
                && receipt.version == SEALED_STORE_RECEIPT_VERSION
                && receipt.graph_format > 0
                && receipt.graph_format <= GraphFormatVersion::current().get()
                && receipt.physical_namespace == physical.as_str()
                && receipt.namespace == locator.projection.namespace.as_str()
                && receipt.projection == locator.projection.projection.as_str()
                && receipt.generation == locator.generation.as_str()
                && expected_digest.is_none_or(|digest| receipt.recovered_digest == digest)
        },
    );
    let removed =
        result.map_err(|error| sealed_store_io_failure("sealed generation retirement refused", error))?;
    // A resident reader carries exact proof that this artifact was installed.
    // If its root or child disappeared before we acquired a deletion capability,
    // physical retirement was not confirmed: the directory may have been moved
    // aside and still contain the sole durable generation. Keep the reader
    // registered so maintenance can retry rather than mark it collected.
    if !removed && expected_digest.is_some() {
        return Err(GraphDbError::unavailable(
            "registered sealed generation retirement found no owned artifact",
        ));
    }
    Ok(())
}

/// Staging and invalid-artifact cleanup are best effort. Retiring an accepted
/// replay instead uses the checked path, so a failed unlink cannot be reported
/// as a completed retirement and lose its durable retry ownership.
/// Best-effort disposal after failed adoption/reopen. Only an exact durable
/// generation receipt authorizes removal; foreign bytes remain for diagnosis.
fn discard_failed_sealed_adoption(
    directory: &Path,
    locator: &GenerationLocator,
    expected_digest: &str,
) {
    if let Err(error) = retire_sealed_directory_checked(directory, locator, Some(expected_digest)) {
        tracing::warn!(
            %error,
            path = %directory.display(),
            "failed sealed adoption left an unverified artifact for inspection"
        );
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
            row_spill_owner(),
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

    /// The cold bases a first generation of `projection` may layer over:
    /// every flat sealed generation of the same projector this store has
    /// installed for another scope. Linked worktrees share one store, so a
    /// worktree at its sibling's tree finds that sibling's graph here.
    pub(crate) fn sibling_sealed_bases(
        &self,
        projection: &GraphProjectionIdentity,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Vec<GraphSiblingSealedBaseV1>, GraphDbError> {
        let siblings = self
            .inner
            .sealed_generations
            .read()
            .map_err(|_| GraphDbError::unavailable("sealed generation map lock is poisoned"))?
            .iter()
            .filter(|(locator, store)| {
                locator.projection.projection == projection.projection
                    && locator.projection.namespace != projection.namespace
                    && store.layer.is_none()
            })
            .map(|(_, store)| Arc::clone(store))
            .collect::<Vec<_>>();
        let mut bases = Vec::new();
        for store in siblings {
            if let Ok(base) = store.sealed_base(check)? {
                bases.push(GraphSiblingSealedBaseV1 {
                    base,
                    engine: Arc::clone(&store.database),
                });
            }
        }
        Ok(bases)
    }

    /// A row spill for a delta of `projection` over a sibling scope's base.
    pub(crate) fn sibling_layered_row_spill(
        &self,
        projection: GraphProjectionIdentity,
        sibling: GraphSiblingSealedBaseV1,
    ) -> Result<GraphLayeredRowSpill, GraphDbError> {
        GraphLayeredRowSpill::create_over_sibling(
            self.row_spill_directory()?,
            projection,
            sibling.base,
            sibling.engine,
        )
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
            Err(error) => {
                // A failed adoption does not prove this directory belongs to
                // the generation. Preserve unknown bytes rather than removing
                // an occupied path without a matching durable receipt.
                discard_failed_sealed_adoption(&directory, &locator, expected.as_str());
                tracing::warn!(
                    %error,
                    path = %directory.display(),
                    "sealed adoption failed; serving falls back to staging rows"
                );
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
    pub(crate) fn retire_sealed_generation_store(
        &self,
        locator: &GenerationLocator,
    ) -> Result<(), GraphDbError> {
        let registered = self
            .inner
            .sealed_generations
            .read()
            .map_err(|_| GraphDbError::unavailable("sealed generation registry lock poisoned"))?
            .get(locator)
            .cloned();
        if let Some(store) = registered {
            // Keep the registered reader until its physical close AND sealed
            // directory removal both succeed. A failed close may consume the
            // native handle; dropping the registry entry would let the next
            // tick take the disk-only path and unlink uncertain bytes.
            store.discard()?;
            let mut sealed = self.inner.sealed_generations.write().map_err(|_| {
                GraphDbError::unavailable("sealed generation registry lock poisoned")
            })?;
            if sealed
                .get(locator)
                .is_some_and(|current| Arc::ptr_eq(current, &store))
            {
                sealed.remove(locator);
            }
            return Ok(());
        }
        // The artifact may exist on disk without a resident reader, for
        // example after reopening a store before generation activation.
        // Only receipt-verified, exact-locator disk cleanup is authorized.
        let Some(reopen) = self.inner.reopen.as_ref() else {
            return Ok(());
        };
        let Some(database_path) = reopen.config.path.as_ref() else {
            return Ok(());
        };
        let physical_namespace = locator.physical_namespace()?;
        let root = sealed_store_root(database_path);
        retire_sealed_directory_checked(
            &sealed_generation_directory(&root, &physical_namespace),
            locator,
            None,
        )
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
    let locator = GenerationLocator::new(identity.projection.clone(), identity.generation.clone());
    match open_sealed_store_checked(
        &directory,
        identity,
        expected,
        &|| Ok(()),
        trusted_base.as_ref(),
    ) {
        Ok(Some(store)) => return Ok((store, None)),
        Ok(None) => match std::fs::symlink_metadata(&directory) {
            Ok(_) => {
                return Err(GraphDbError::unavailable(
                    "unverified sealed generation occupies the artifact directory",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(sealed_store_io_failure(
                    "sealed artifact metadata read failed",
                    error,
                ));
            }
        },
        Err(GraphDbError::Cancelled) => return Err(GraphDbError::Cancelled),
        Err(_) => {
            // A corrupt owned artifact may be rebuilt, but an unknown receipt
            // (or a replacement directory) never grants deletion authority.
            retire_sealed_directory_checked(&directory, &locator, Some(expected.as_str()))?;
        }
    }
    let staging = root.join(format!(
        ".staging-{}",
        directory
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("sealed")
    ));
    // create_dir_all may accept a symlinked sealed root, so verify the actual
    // root before creating the staging entry or writing a container.
    std::fs::create_dir_all(&root)
        .map_err(|error| sealed_store_io_failure("sealed root create failed", error))?;
    // create_dir_all accepts symlinked directory components. Refuse a
    // preexisting substituted sealed root before writing staging bytes.
    let root_metadata = std::fs::symlink_metadata(&root)
        .map_err(|error| sealed_store_io_failure("sealed root metadata failed", error))?;
    if !root_metadata.is_dir() {
        return Err(GraphDbError::unavailable(
            "sealed root must be a real directory",
        ));
    }
    let staging_name = staging
        .file_name()
        .ok_or_else(|| GraphDbError::unavailable("sealed staging name missing"))?;
    // A deterministic staging name is not proof of ownership. An earlier
    // interrupted build or a foreign directory must not be deleted on retry.
    // create_dir is exclusive, so an occupied name fails closed.
    std::fs::create_dir(&staging)
        .map_err(|error| sealed_store_io_failure("staging directory create failed", error))?;
    let owner = SealedStagingOwnerV1 {
        producer: row_spill_owner().to_string(),
        receipt: initial_sealed_staging_owner(identity, expected, physical_namespace.as_str()),
    };
    let owner_bytes = serde_json::to_vec(&owner)
        .map_err(|error| GraphDbError::unavailable(format!("staging owner encode: {error}")))?;
    // Exclusive creation refuses a pre-existing marker. Flush before writing
    // rows so a killed producer leaves verifiable deletion authority.
    let marker_path = staging.join(SEALED_STAGING_OWNER_FILE);
    let mut marker = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker_path)
        .map_err(|error| sealed_store_io_failure("staging owner create failed", error))?;
    marker
        .write_all(&owner_bytes)
        .and_then(|()| marker.sync_all())
        .map_err(|error| sealed_store_io_failure("staging owner persist failed", error))?;
    drop(marker);
    let receipt = stage_sealed_artifact(rows, identity, expected, &staging, check)
        .inspect_err(|_| discard_failed_sealed_staging(&staging, &locator, expected.as_str()))?;
    let encoded = serde_json::to_vec_pretty(&receipt)
        .map_err(|error| GraphDbError::unavailable(format!("sealed receipt encode: {error}")))
        .inspect_err(|_| discard_failed_sealed_staging(&staging, &locator, expected.as_str()))?;
    let mut receipt_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(staging.join(SEALED_STORE_RECEIPT_FILE))
        .map_err(|error| sealed_store_io_failure("receipt create failed", error))
        .inspect_err(|_| discard_failed_sealed_staging(&staging, &locator, expected.as_str()))?;
    let receipt_write = receipt_file
        .write_all(&encoded)
        .and_then(|()| receipt_file.sync_all());
    drop(receipt_file);
    receipt_write
        .map_err(|error| sealed_store_io_failure("receipt persist failed", error))
        .inspect_err(|_| discard_failed_sealed_staging(&staging, &locator, expected.as_str()))?;
    let artifact_name = directory
        .file_name()
        .ok_or_else(|| GraphDbError::unavailable("sealed artifact name missing"))?;
    let install = install_child_directory_noreplace_nofollow(&root, staging_name, artifact_name);
    if let Err(error) = install {
        discard_failed_sealed_staging(&staging, &locator, expected.as_str());
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
            discard_failed_sealed_adoption(&directory, &locator, expected.as_str());
            Err(GraphDbError::unavailable(
                "sealed generation store disappeared between install and reopen".to_owned(),
            ))
        }
        Err(error) => {
            discard_failed_sealed_adoption(&directory, &locator, expected.as_str());
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
    let workers = rayon::current_thread_index().map_or(1, |_| rayon::current_num_threads());
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
    let workers = rayon::current_thread_index().map_or(1, |_| rayon::current_num_threads());
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
    let receipt_bytes = match read_sealed_receipt_bytes(&receipt_path) {
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
    // This is a rebuildable proof hint. Invalid or overlarge bytes cause a
    // fresh proof, never an unbounded allocation or a trusted cache hit.
    match tracedecay_private_fs::framed_log::read_bounded(&path, MAX_SEALED_CHECK_BYTES) {
        Ok(Some(bytes)) => Ok(bytes == expected_digest.as_bytes()),
        Ok(None) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => Ok(false),
        Err(error) => Err(sealed_store_io_failure("sealed check read failed", error)),
    }
}

fn persist_sealed_store_check(directory: &Path, expected_digest: &str) -> Result<(), GraphDbError> {
    if sealed_store_check_matches(directory, expected_digest)? {
        return Ok(());
    }
    // Never truncate an existing symlink target: the marker is a rebuildable
    // cache, so stage a regular file and publish it by atomic replacement.
    tracedecay_private_fs::capability_dir::atomic_replace_child_file_nofollow(
        directory,
        std::ffi::OsStr::new(SEALED_STORE_CHECKED_FILE),
        expected_digest.as_bytes(),
    )
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
mod receipt_budget_tests {
    #[cfg(unix)]
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::*;

    fn receipt(format: u32, name: &str, generation: &str) -> Vec<u8> {
        serde_json::to_vec(&SealedStoreReceiptV1 {
            version: SEALED_STORE_RECEIPT_VERSION,
            form: SEALED_STORE_FORM_COMPACT.to_owned(),
            namespace: "test".to_owned(),
            projection: "code".to_owned(),
            generation: generation.to_owned(),
            physical_namespace: format!("generation:{name}"),
            recovered_digest: "digest".to_owned(),
            entities: 0,
            relations: 0,
            graph_format: format,
            row_sum: None,
            base: None,
            source_generation: None,
            watermark: None,
        })
        .unwrap()
    }

    fn generation(root: &Path, digit: char, contents: &[u8]) -> PathBuf {
        let directory = root.join(digit.to_string().repeat(64));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join(SEALED_STORE_RECEIPT_FILE), contents).unwrap();
        directory
    }

    #[test]
    fn sealed_receipt_reads_bound_memory_even_for_json_padded_beyond_the_limit() {
        let temp = tempfile::tempdir().unwrap();
        let receipt_path = temp.path().join(SEALED_STORE_RECEIPT_FILE);
        let normal = receipt(GraphFormatVersion::current().get(), &"a".repeat(64), "head");
        std::fs::write(&receipt_path, &normal).unwrap();
        assert_eq!(read_sealed_receipt_bytes(&receipt_path).unwrap(), normal);

        let oversized = [normal, vec![b' '; MAX_SEALED_RECEIPT_BYTES]].concat();
        assert!(serde_json::from_slice::<SealedStoreReceiptV1>(&oversized).is_ok());
        std::fs::write(&receipt_path, oversized).unwrap();
        assert_eq!(
            read_sealed_receipt_bytes(&receipt_path).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        // On Unix this is sparse, so the multi-gigabyte bound is exercised
        // without allocating a multi-gigabyte test artifact.
        #[cfg(unix)]
        {
            std::fs::File::create(&receipt_path)
                .unwrap()
                .set_len(15 * 1024 * 1024 * 1024)
                .unwrap();
            assert_eq!(
                read_sealed_receipt_bytes(&receipt_path).unwrap_err().kind(),
                std::io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn sealed_check_marker_is_bounded_and_never_overwrites_a_foreign_target() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("sealed");
        std::fs::create_dir(&directory).unwrap();
        let marker = directory.join(SEALED_STORE_CHECKED_FILE);
        let digest = format!("sha256:{}", "a".repeat(64));

        assert!(!sealed_store_check_matches(&directory, &digest).unwrap());
        persist_sealed_store_check(&directory, &digest).unwrap();
        assert!(sealed_store_check_matches(&directory, &digest).unwrap());
        assert_eq!(std::fs::read(&marker).unwrap(), digest.as_bytes());

        let mut oversized = digest.as_bytes().to_vec();
        oversized.extend(vec![b' '; MAX_SEALED_CHECK_BYTES]);
        std::fs::write(&marker, oversized).unwrap();
        assert!(!sealed_store_check_matches(&directory, &digest).unwrap());
        persist_sealed_store_check(&directory, &digest).unwrap();
        assert!(sealed_store_check_matches(&directory, &digest).unwrap());

        #[cfg(unix)]
        {
            // Sparse bytes establish the large-file refusal without writing
            // or allocating a multi-gigabyte test payload.
            std::fs::File::create(&marker)
                .unwrap()
                .set_len(15 * 1024 * 1024 * 1024)
                .unwrap();
            assert!(!sealed_store_check_matches(&directory, &digest).unwrap());
            persist_sealed_store_check(&directory, &digest).unwrap();
            assert!(sealed_store_check_matches(&directory, &digest).unwrap());

            let foreign = temp.path().join("foreign");
            std::fs::remove_file(&marker).unwrap();
            std::fs::write(&foreign, digest.as_bytes()).unwrap();
            std::os::unix::fs::symlink(&foreign, &marker).unwrap();
            assert!(
                sealed_store_check_matches(&directory, &digest).is_err(),
                "a symlink cannot supply proof evidence"
            );

            std::fs::remove_file(&marker).unwrap();
            std::fs::write(&foreign, b"foreign sentinel").unwrap();
            std::os::unix::fs::symlink(&foreign, &marker).unwrap();
            assert!(
                persist_sealed_store_check(&directory, &digest).is_err(),
                "publishing a proof marker must refuse a foreign symlink"
            );
            assert_eq!(std::fs::read(&foreign).unwrap(), b"foreign sentinel");
            assert!(
                std::fs::symlink_metadata(&marker)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "the foreign marker entry itself must survive"
            );
        }
    }

    #[test]
    fn obsolete_format_cleanup_keeps_unknown_receipts_and_other_files() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("graph.grafeo");
        let root = sealed_store_root(&database);
        std::fs::create_dir_all(&root).unwrap();
        let current = GraphFormatVersion::current().get();
        assert!(current > 0);
        let old_locator = GenerationLocator::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new("test").unwrap(),
                GraphProjectionId::new("code").unwrap(),
            ),
            crate::GraphGenerationId::new("generation:obsolete").unwrap(),
        );
        let old_physical = old_locator.physical_namespace().unwrap();
        let old_name = old_physical.as_str().strip_prefix("generation:").unwrap();
        let old = root.join(old_name);
        std::fs::create_dir(&old).unwrap();
        std::fs::write(
            old.join(SEALED_STORE_RECEIPT_FILE),
            receipt(current - 1, old_name, "generation:obsolete"),
        )
        .unwrap();
        // A syntactically plausible old receipt with a different logical
        // generation cannot authorize deletion of this occupied directory.
        let forged = generation(
            &root,
            'a',
            &receipt(current - 1, &"a".repeat(64), "generation:foreign"),
        );
        let live = generation(&root, 'b', &receipt(current, &"b".repeat(64), "live"));
        let invalid = generation(&root, 'c', b"not a receipt");
        // Zero is not a valid GraphFormatVersion, even when the JSON is well formed.
        let zero_format = generation(&root, '0', &receipt(0, &"0".repeat(64), "zero"));
        let future = generation(
            &root,
            'f',
            &receipt(current.saturating_add(1), &"f".repeat(64), "future"),
        );
        let padded = [
            receipt(current - 1, &"d".repeat(64), "padded"),
            vec![b' '; MAX_SEALED_RECEIPT_BYTES],
        ]
        .concat();
        let oversized = generation(&root, 'd', &padded);
        let missing = root.join("e".repeat(64));
        std::fs::create_dir_all(&missing).unwrap();
        let unrelated = root.join("unrelated.txt");
        std::fs::write(&unrelated, b"foreign").unwrap();
        // A symlink to a syntactically valid old receipt must never confer
        // deletion authority over the directory that holds the link.
        #[cfg(unix)]
        let linked_receipt = {
            let directory = root.join("1".repeat(64));
            std::fs::create_dir_all(&directory).unwrap();
            let foreign = temp.path().join("foreign-receipt.json");
            std::fs::write(&foreign, receipt(current - 1, &"1".repeat(64), "foreign")).unwrap();
            std::os::unix::fs::symlink(&foreign, directory.join(SEALED_STORE_RECEIPT_FILE))
                .unwrap();
            std::fs::write(directory.join("sentinel"), b"unowned").unwrap();
            (directory, foreign)
        };
        for dir in [
            &old,
            &forged,
            &live,
            &invalid,
            &zero_format,
            &oversized,
            &missing,
            &future,
        ] {
            std::fs::write(dir.join("sentinel"), b"owned").unwrap();
        }

        discard_superseded_sealed_generations(&database).unwrap();
        assert!(!old.exists(), "verified old-format receipt is reclaimable");
        for dir in [
            &forged,
            &live,
            &invalid,
            &zero_format,
            &oversized,
            &missing,
            &future,
        ] {
            assert_eq!(std::fs::read(dir.join("sentinel")).unwrap(), b"owned");
        }
        assert_eq!(std::fs::read(&unrelated).unwrap(), b"foreign");
        #[cfg(unix)]
        {
            assert_eq!(
                std::fs::read(linked_receipt.0.join("sentinel")).unwrap(),
                b"unowned"
            );
            assert!(linked_receipt.1.is_file());
            assert_eq!(
                read_sealed_receipt_bytes(&linked_receipt.0.join(SEALED_STORE_RECEIPT_FILE))
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn failed_adoption_cannot_discard_unverified_or_foreign_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("graph.grafeo");
        let root = sealed_store_root(&database);
        let locator = GenerationLocator::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new("test").unwrap(),
                GraphProjectionId::new("code").unwrap(),
            ),
            crate::GraphGenerationId::new("generation:retirement").unwrap(),
        );
        let directory = sealed_generation_directory(&root, &locator.physical_namespace().unwrap());
        std::fs::create_dir_all(&directory).unwrap();
        let name = directory.file_name().unwrap().to_str().unwrap();
        let sentinel = directory.join("foreign-sentinel");
        std::fs::write(&sentinel, b"preserve").unwrap();
        let sibling = root.join("unrelated");
        std::fs::create_dir(&sibling).unwrap();
        std::fs::write(sibling.join("sentinel"), b"other").unwrap();

        // Neither a missing nor a malformed receipt proves ownership.
        discard_failed_sealed_adoption(&directory, &locator, "digest");
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"preserve");
        let receipt_path = directory.join(SEALED_STORE_RECEIPT_FILE);
        std::fs::write(&receipt_path, b"invalid receipt").unwrap();
        discard_failed_sealed_adoption(&directory, &locator, "digest");
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"preserve");

        // An otherwise recognizable receipt still cannot retire a different
        // expected digest. The exact matching receipt allows cleanup.
        std::fs::write(
            &receipt_path,
            receipt(
                GraphFormatVersion::current().get(),
                name,
                "generation:retirement",
            ),
        )
        .unwrap();
        discard_failed_sealed_adoption(&directory, &locator, "other-digest");
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"preserve");
        discard_failed_sealed_adoption(&directory, &locator, "digest");
        assert!(
            !directory.exists(),
            "owned invalid artifacts can be reclaimed"
        );
        assert_eq!(std::fs::read(sibling.join("sentinel")).unwrap(), b"other");
    }

    #[test]
    fn retired_sealed_generation_requires_its_exact_receipt_identity() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("graph.grafeo");
        let root = sealed_store_root(&database);
        let locator = GenerationLocator::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new("test").unwrap(),
                GraphProjectionId::new("code").unwrap(),
            ),
            crate::GraphGenerationId::new("generation:retirement").unwrap(),
        );
        let physical = locator.physical_namespace().unwrap();
        let directory = sealed_generation_directory(&root, &physical);
        std::fs::create_dir_all(&directory).unwrap();
        let name = directory.file_name().unwrap().to_str().unwrap();
        let receipt_path = directory.join(SEALED_STORE_RECEIPT_FILE);
        std::fs::write(
            &receipt_path,
            receipt(
                GraphFormatVersion::current().get(),
                name,
                "generation:other",
            ),
        )
        .unwrap();
        std::fs::write(directory.join("sentinel"), b"preserve").unwrap();
        assert!(retire_sealed_directory_checked(&directory, &locator, None).is_err());
        assert_eq!(
            std::fs::read(directory.join("sentinel")).unwrap(),
            b"preserve"
        );
        std::fs::write(
            &receipt_path,
            receipt(
                GraphFormatVersion::current().get(),
                name,
                locator.generation.as_str(),
            ),
        )
        .unwrap();
        assert!(retire_sealed_directory_checked(&directory, &locator, Some("wrong")).is_err());
        assert_eq!(
            std::fs::read(directory.join("sentinel")).unwrap(),
            b"preserve"
        );
        retire_sealed_directory_checked(&directory, &locator, Some("digest")).unwrap();
        assert!(
            !directory.exists(),
            "verified retired generation is physically removed"
        );
        retire_sealed_directory_checked(&directory, &locator, Some("digest")).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn census_handles_seventy_large_sparse_generations_without_loading_containers() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("graph.grafeo");
        let root = sealed_store_root(&database);
        std::fs::create_dir_all(&root).unwrap();
        const FILE_BYTES: u64 = 220 * 1024 * 1024;
        let mut oldest = None;
        for index in 0..70 {
            let generation = format!("generation:g{index}");
            let locator = GenerationLocator::new(
                GraphProjectionIdentity::new(
                    GraphNamespace::new("test").unwrap(),
                    GraphProjectionId::new("code").unwrap(),
                ),
                crate::GraphGenerationId::new(generation.clone()).unwrap(),
            );
            let physical = locator.physical_namespace().unwrap();
            let name = physical.as_str().strip_prefix("generation:").unwrap();
            let dir = root.join(name);
            if index == 0 {
                oldest = Some(dir.clone());
            }
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(SEALED_STORE_RECEIPT_FILE),
                receipt(GraphFormatVersion::current().get(), name, &generation),
            )
            .unwrap();
            std::fs::File::create(dir.join(SEALED_STORE_DATABASE_FILE))
                .unwrap()
                .set_len(FILE_BYTES)
                .unwrap();
        }
        // A sealed directory must never account bytes outside its own tree.
        // A missing target must also remain a harmless link, not abort census.
        let foreign = temp.path().join("foreign");
        std::fs::create_dir(&foreign).unwrap();
        std::fs::File::create(foreign.join("payload"))
            .unwrap()
            .set_len(20 * 1024 * 1024 * 1024)
            .unwrap();
        let oldest = oldest.unwrap();
        std::os::unix::fs::symlink(&foreign, oldest.join("foreign")).unwrap();
        std::os::unix::fs::symlink(temp.path().join("missing"), oldest.join("dangling")).unwrap();
        let census =
            census_sealed_store(&database, &BTreeSet::from(["generation:g69".to_owned()])).unwrap();
        assert_eq!(census.head_count, 1);
        assert_eq!(census.superseded_count, 69);
        assert_eq!(census.unrecognized_count, 0);
        assert!(census.head_bytes >= FILE_BYTES);
        assert!(census.superseded_bytes >= 69 * FILE_BYTES);
        assert!(
            census.superseded_bytes < 70 * FILE_BYTES,
            "foreign link targets must not contribute to sealed generation bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn census_does_not_classify_copied_or_foreign_receipts_as_live_heads() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("graph.grafeo");
        let root = sealed_store_root(&database);
        std::fs::create_dir_all(&root).unwrap();
        let generation = "generation:head";
        let locator = GenerationLocator::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new("test").unwrap(),
                GraphProjectionId::new("code").unwrap(),
            ),
            crate::GraphGenerationId::new(generation).unwrap(),
        );
        let physical = locator.physical_namespace().unwrap();
        let owned_name = physical.as_str().strip_prefix("generation:").unwrap();
        let owned = root.join(owned_name);
        std::fs::create_dir(&owned).unwrap();
        let owned_receipt = receipt(GraphFormatVersion::current().get(), owned_name, generation);
        std::fs::write(owned.join(SEALED_STORE_RECEIPT_FILE), &owned_receipt).unwrap();
        std::fs::write(owned.join(SEALED_STORE_DATABASE_FILE), b"owned payload").unwrap();

        // The foreign directory carries the *exact* valid head receipt, but
        // its physical entry is not the one derived from the logical identity.
        let foreign_name = if owned_name == "f".repeat(64) {
            "e".repeat(64)
        } else {
            "f".repeat(64)
        };
        let foreign = root.join(foreign_name);
        std::fs::create_dir(&foreign).unwrap();
        std::fs::write(foreign.join(SEALED_STORE_RECEIPT_FILE), &owned_receipt).unwrap();
        std::fs::File::create(foreign.join(SEALED_STORE_DATABASE_FILE))
            .unwrap()
            .set_len(5 * 1024 * 1024 * 1024)
            .unwrap();

        let census =
            census_sealed_store(&database, &BTreeSet::from([generation.to_owned()])).unwrap();
        assert_eq!(census.head_count, 1);
        assert_eq!(census.superseded_count, 0);
        assert_eq!(census.unrecognized_count, 1);
        assert_eq!(census.head_bytes, owned_receipt.len() as u64 + 13);
        assert!(foreign.join(SEALED_STORE_DATABASE_FILE).exists());
    }
}

#[cfg(test)]
mod build_tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use rayon::ThreadPoolBuilder;

    use super::{
        MAX_SEALED_RECEIPT_BYTES, SEALED_STAGING_OWNER_FILE, SEALED_STORE_DATABASE_FILE,
        SEALED_STORE_RECEIPT_FILE, SealedRowSource, SealedStagingOwnerV1,
        build_or_open_sealed_store, build_sealed_container, census_sealed_store,
        initial_sealed_staging_owner, persist_sealed_store_check, sealed_artifact_database_options,
        sealed_copy_proof, sealed_generation_directory, sealed_store_root,
        sweep_abandoned_sealed_staging,
    };
    use crate::location::PersistentGraphStoreState;
    use crate::{
        GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDbOwner, GraphDurability,
        GraphEntity, GraphEntityId, GraphEntityRef, GraphFormatVersion, GraphGenerationId,
        GraphGenerationManifest, GraphGenerationRelation, GraphLabel, GraphNamespace,
        GraphProjectionId, GraphProjectionIdentity, GraphProperty, GraphPropertyName,
        GraphRelationId, GraphRelationKind, GraphWatermark, NeverCancelled, SourceGeneration,
    };

    #[cfg(unix)]
    #[test]
    fn sealed_check_persistence_refuses_a_substituted_generation_directory() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let owned = temp.path().join("owned-generation");
        let foreign = temp.path().join("foreign-generation");
        std::fs::create_dir(&owned).unwrap();
        std::fs::create_dir(&foreign).unwrap();
        std::fs::write(foreign.join("sealed.checked"), b"foreign marker").unwrap();
        std::fs::remove_dir(&owned).unwrap();
        symlink(&foreign, &owned).unwrap();

        assert!(persist_sealed_store_check(&owned, "accepted-digest").is_err());
        assert_eq!(
            std::fs::read(foreign.join("sealed.checked")).unwrap(),
            b"foreign marker",
        );
        std::fs::remove_file(&owned).unwrap();
        std::fs::create_dir(&owned).unwrap();
        persist_sealed_store_check(&owned, "accepted-digest").unwrap();
        assert_eq!(
            std::fs::read(owned.join("sealed.checked")).unwrap(),
            b"accepted-digest",
        );
        assert_eq!(
            std::fs::read(foreign.join("sealed.checked")).unwrap(),
            b"foreign marker",
        );
    }

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

    #[cfg(unix)]
    #[test]
    fn sealed_census_only_attributes_verified_abandoned_staging() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("source.grafeo");
        let root = sealed_store_root(&database);
        let manifest = manifest(2, 1);
        let identity = manifest.identity();
        let expected = manifest.expected_recovered_digest(&|| Ok(())).unwrap();
        let physical = identity.physical_namespace().unwrap();
        let name = physical.as_str().strip_prefix("generation:").unwrap();
        let staging = root.join(format!(".staging-{name}"));
        std::fs::create_dir_all(&staging).unwrap();

        let exited = SealedStagingOwnerV1 {
            producer: (crate::generation::row_spill_owner() ^ 1).to_string(),
            receipt: initial_sealed_staging_owner(&identity, &expected, physical.as_str()),
        };
        let bytes = serde_json::to_vec(&exited).unwrap();
        let owner_path = staging.join(SEALED_STAGING_OWNER_FILE);
        std::fs::write(&owner_path, &bytes).unwrap();
        std::fs::write(staging.join("payload"), vec![7; 4096]).unwrap();

        let census = census_sealed_store(&database, &BTreeSet::new()).unwrap();
        assert_eq!(census.abandoned_staging_count, 1);
        assert_eq!(census.abandoned_staging_bytes, bytes.len() as u64 + 4096);
        assert_eq!(census.unrecognized_count, 0);

        // This same completed marker may belong to a seal currently running
        // in this process. A remount must not report its bytes as garbage.
        let live = SealedStagingOwnerV1 {
            producer: crate::generation::row_spill_owner().to_string(),
            receipt: initial_sealed_staging_owner(&identity, &expected, physical.as_str()),
        };
        std::fs::write(&owner_path, serde_json::to_vec(&live).unwrap()).unwrap();
        let census = census_sealed_store(&database, &BTreeSet::new()).unwrap();
        assert_eq!(census.abandoned_staging_count, 0);
        assert_eq!(census.abandoned_staging_bytes, 0);
        assert_eq!(census.unrecognized_count, 1);

        // A copied authentic receipt in a different physical staging name
        // does not authorize either reclamation or an unbounded size walk.
        let foreign_name = if name == "f".repeat(64) {
            "e".repeat(64)
        } else {
            "f".repeat(64)
        };
        let foreign = root.join(format!(".staging-{foreign_name}"));
        std::fs::create_dir(&foreign).unwrap();
        std::fs::write(foreign.join(SEALED_STAGING_OWNER_FILE), &bytes).unwrap();
        std::fs::File::create(foreign.join("sparse-foreign-payload"))
            .unwrap()
            .set_len(5 * 1024 * 1024 * 1024)
            .unwrap();
        let census = census_sealed_store(&database, &BTreeSet::new()).unwrap();
        assert_eq!(census.abandoned_staging_count, 0);
        assert_eq!(census.abandoned_staging_bytes, 0);
        assert_eq!(census.unrecognized_count, 2);
        assert!(foreign.join("sparse-foreign-payload").is_file());

        // Even an owned directory whose marker exceeds the fixed bound must
        // not cause its contents to be traversed or counted as abandoned.
        let oversized = vec![b'x'; MAX_SEALED_RECEIPT_BYTES + 1];
        std::fs::write(&owner_path, &oversized).unwrap();
        let census = census_sealed_store(&database, &BTreeSet::new()).unwrap();
        assert_eq!(census.abandoned_staging_count, 0);
        assert_eq!(census.unrecognized_count, 2);
        assert_eq!(std::fs::read(&owner_path).unwrap(), oversized);
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

    /// An unverified existing generation directory cannot be treated as
    /// disposable scratch space. Rebuilding is allowed only after its owner
    /// removes the unknown data.
    #[test]
    fn sealed_build_preserves_an_unverified_occupied_generation() {
        let check: &dyn Fn() -> Result<(), GraphDbError> = &|| Ok(());
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("source.grafeo");
        let manifest = manifest(2, 1);
        let identity = manifest.identity();
        let expected = manifest.expected_recovered_digest(check).unwrap();
        let root = sealed_store_root(&database_path);
        let directory = sealed_generation_directory(&root, &identity.physical_namespace().unwrap());
        std::fs::create_dir_all(&directory).unwrap();
        let sentinel = directory.join("foreign-sentinel");
        std::fs::write(&sentinel, b"not owned").unwrap();

        for foreign_receipt in [false, true] {
            if foreign_receipt {
                std::fs::write(
                    directory.join(SEALED_STORE_RECEIPT_FILE),
                    b"invalid receipt",
                )
                .unwrap();
            }
            let result = build_or_open_sealed_store(
                SealedRowSource::Manifest(&manifest),
                &identity,
                &expected,
                &database_path,
                check,
            );
            assert!(
                result.is_err(),
                "unreceipted or malformed foreign data must refuse rebuilding"
            );
            assert_eq!(std::fs::read(&sentinel).unwrap(), b"not owned");
            assert!(
                !directory.join(SEALED_STORE_DATABASE_FILE).exists(),
                "refused rebuilding must never write a container into foreign data"
            );
        }

        std::fs::remove_dir_all(&directory).unwrap();
        let (store, _) = build_or_open_sealed_store(
            SealedRowSource::Manifest(&manifest),
            &identity,
            &expected,
            &database_path,
            check,
        )
        .expect("clearing the unknown artifact allows a verified sealed build");
        assert_eq!(store.recovered_digest(), expected.as_str());
        assert!(directory.join(SEALED_STORE_DATABASE_FILE).is_file());
    }

    #[cfg(unix)]
    #[test]
    fn retirement_keeps_sealed_bytes_when_reader_close_is_uncertain() {
        let check: &dyn Fn() -> Result<(), GraphDbError> = &|| Ok(());
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("source.grafeo");
        let manifest = manifest(2, 1);
        let identity = manifest.identity();
        let expected = manifest.expected_recovered_digest(check).unwrap();
        let (store, _) = build_or_open_sealed_store(
            SealedRowSource::Manifest(&manifest),
            &identity,
            &expected,
            &database_path,
            check,
        )
        .unwrap();
        let directory = store.directory.clone();
        let container = directory.join(SEALED_STORE_DATABASE_FILE);
        let receipt = directory.join(SEALED_STORE_RECEIPT_FILE);
        assert!(container.is_file() && receipt.is_file());

        // Poison the real native reader's engine lock. Its physical close
        // now returns DurabilityUncertain; the old discard ignored that error
        // and removed the verified directory anyway on Unix.
        let reader = Arc::clone(&store.database);
        let poisoned = std::thread::spawn(move || {
            let _guard = reader.inner.database.write().unwrap();
            panic!("exercise uncertain sealed reader close");
        });
        assert!(poisoned.join().is_err());
        let refused = store.discard();
        assert!(
            matches!(refused, Err(GraphDbError::DurabilityUncertain { .. })),
            "retirement must surface the failed native close: {refused:?}"
        );
        assert!(
            container.is_file(),
            "uncertain close must preserve sealed bytes"
        );
        assert!(receipt.is_file(), "retry must retain its ownership receipt");
    }

    #[test]
    fn retirement_accepts_an_already_closed_derived_reader() {
        let check: &dyn Fn() -> Result<(), GraphDbError> = &|| Ok(());
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("source.grafeo");
        let manifest = manifest(2, 1);
        let identity = manifest.identity();
        let expected = manifest.expected_recovered_digest(check).unwrap();
        let (store, _) = build_or_open_sealed_store(
            SealedRowSource::Manifest(&manifest),
            &identity,
            &expected,
            &database_path,
            check,
        )
        .unwrap();
        let directory = store.directory.clone();
        store.database.close().unwrap();
        store.discard().unwrap();
        assert!(
            !directory.exists(),
            "a cleanly closed reader is reclaimable"
        );
    }

    #[test]
    fn retirement_does_not_erase_a_prior_uncertain_close() {
        let check: &dyn Fn() -> Result<(), GraphDbError> = &|| Ok(());
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("source.grafeo");
        let manifest = manifest(2, 1);
        let identity = manifest.identity();
        let expected = manifest.expected_recovered_digest(check).unwrap();
        let (store, _) = build_or_open_sealed_store(
            SealedRowSource::Manifest(&manifest),
            &identity,
            &expected,
            &database_path,
            check,
        )
        .unwrap();
        let directory = store.directory.clone();
        let container = directory.join(SEALED_STORE_DATABASE_FILE);
        let receipt = directory.join(SEALED_STORE_RECEIPT_FILE);

        // Reproduce the post-failure state of native engine.close(): its
        // handle is gone, Closed will be returned on retry, but durability
        // remains uncertain. Retirement must never mistake that state for a
        // previously successful close and destroy its sealed receipt.
        store.database.inner.poisoned.store(true, Ordering::Release);
        let first = store.database.close();
        assert!(matches!(
            first,
            Err(GraphDbError::DurabilityUncertain { .. })
        ));
        let retry = store.discard();
        assert!(
            matches!(retry, Err(GraphDbError::DurabilityUncertain { .. })),
            "a prior uncertain close must not become an unlink: {retry:?}"
        );
        assert!(container.is_file() && receipt.is_file());
    }

    #[test]
    fn retirement_retries_must_keep_a_reader_with_uncertain_close_registered() {
        let check: &dyn Fn() -> Result<(), GraphDbError> = &|| Ok(());
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("source.grafeo");
        let manifest = manifest(2, 1);
        let identity = manifest.identity();
        let expected = manifest.expected_recovered_digest(check).unwrap();
        let (store, _) = build_or_open_sealed_store(
            SealedRowSource::Manifest(&manifest),
            &identity,
            &expected,
            &database_path,
            check,
        )
        .unwrap();
        let locator = crate::lease::GenerationLocator::new(
            identity.projection.clone(),
            identity.generation.clone(),
        );
        let parent = GraphDb::open(GraphDbOpenOptions {
            location: GraphDbLocation::Persistent(database_path.clone()),
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::WalSync,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap();
        parent
            .inner
            .sealed_generations
            .write()
            .unwrap()
            .insert(locator.clone(), Arc::clone(&store));

        let container = store.directory.join(SEALED_STORE_DATABASE_FILE);
        let receipt = store.directory.join(SEALED_STORE_RECEIPT_FILE);
        let held_reader = Arc::clone(store.database());
        store.database.inner.poisoned.store(true, Ordering::Release);
        for attempt in 1..=2 {
            let retirement = parent.retire_sealed_generation_store(&locator);
            assert!(
                matches!(&retirement, Err(GraphDbError::DurabilityUncertain { .. })),
                "retirement attempt {attempt} must refuse uncertain close: {retirement:?}"
            );
            let registered = parent
                .sealed_generation_reader(&locator)
                .expect("failed close must preserve the registered reader");
            assert!(Arc::ptr_eq(&registered, &store));
            assert!(container.is_file() && receipt.is_file());
        }
        assert!(Arc::ptr_eq(&held_reader, store.database()));
    }

    #[test]
    fn successful_retirement_uninstalls_the_reader_after_removing_artifact() {
        let check: &dyn Fn() -> Result<(), GraphDbError> = &|| Ok(());
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("source.grafeo");
        let manifest = manifest(2, 1);
        let identity = manifest.identity();
        let expected = manifest.expected_recovered_digest(check).unwrap();
        let (store, _) = build_or_open_sealed_store(
            SealedRowSource::Manifest(&manifest),
            &identity,
            &expected,
            &database_path,
            check,
        )
        .unwrap();
        let locator = crate::lease::GenerationLocator::new(
            identity.projection.clone(),
            identity.generation.clone(),
        );
        let parent = GraphDb::open(GraphDbOpenOptions {
            location: GraphDbLocation::Persistent(database_path.clone()),
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::WalSync,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap();
        parent
            .inner
            .sealed_generations
            .write()
            .unwrap()
            .insert(locator.clone(), Arc::clone(&store));

        parent.retire_sealed_generation_store(&locator).unwrap();
        assert!(parent.sealed_generation_reader(&locator).is_none());
        assert!(!store.directory.exists());
    }

    /// A missing pathname is not evidence that an installed generation's
    /// durable bytes were removed. It may have been relocated between
    /// registration and the retirement attempt.
    #[test]
    fn registered_retirement_refuses_disappeared_root_or_child_until_restored() {
        let check: &dyn Fn() -> Result<(), GraphDbError> = &|| Ok(());
        for move_root in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let database_path = temp.path().join("source.grafeo");
            let manifest = manifest(2, 1);
            let identity = manifest.identity();
            let expected = manifest.expected_recovered_digest(check).unwrap();
            let (store, _) = build_or_open_sealed_store(
                SealedRowSource::Manifest(&manifest),
                &identity,
                &expected,
                &database_path,
                check,
            )
            .unwrap();
            let locator = crate::lease::GenerationLocator::new(
                identity.projection.clone(),
                identity.generation.clone(),
            );
            let parent = GraphDb::open(GraphDbOpenOptions {
                location: GraphDbLocation::Persistent(database_path.clone()),
                expected_format: GraphFormatVersion::current(),
                durability: GraphDurability::WalSync,
                cancellation: Arc::new(NeverCancelled),
            })
            .unwrap();
            parent
                .inner
                .sealed_generations
                .write()
                .unwrap()
                .insert(locator.clone(), Arc::clone(&store));

            let root = sealed_store_root(&database_path);
            let (original, relocated) = if move_root {
                (root, temp.path().join("relocated-sealed-root"))
            } else {
                (store.directory.clone(), root.join("relocated-sealed-child"))
            };
            std::fs::rename(&original, &relocated).unwrap();
            let surviving_receipt = if move_root {
                relocated
                    .join(store.directory.file_name().unwrap())
                    .join(SEALED_STORE_RECEIPT_FILE)
            } else {
                relocated.join(SEALED_STORE_RECEIPT_FILE)
            };
            let receipt = std::fs::read(&surviving_receipt).unwrap();

            let refused = parent.retire_sealed_generation_store(&locator);
            assert!(
                matches!(&refused, Err(GraphDbError::Unavailable { .. })),
                "missing owned artifact must remain pending (root={move_root}): {refused:?}"
            );
            assert_eq!(
                std::fs::read(&surviving_receipt).unwrap(),
                receipt,
                "retirement must preserve the relocated sealed receipt"
            );
            let retained = parent
                .sealed_generation_reader(&locator)
                .expect("failed retirement must retain its registered reader");
            assert!(Arc::ptr_eq(&retained, &store));
            drop(retained);

            // Retry against the restored owner must really delete the bytes
            // and remove the reader. A permanent refusal cannot pass.
            std::fs::rename(&relocated, &original).unwrap();
            parent.retire_sealed_generation_store(&locator).unwrap();
            assert!(parent.sealed_generation_reader(&locator).is_none());
            assert!(!store.directory.exists());
            // Disk-only duplicate notifications remain idempotent.
            retire_sealed_directory_checked(&store.directory, &locator, None).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn sealed_build_refuses_a_foreign_root_symlink() {
        let check: &dyn Fn() -> Result<(), GraphDbError> = &|| Ok(());
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("source.grafeo");
        let manifest = manifest(2, 1);
        let identity = manifest.identity();
        let expected = manifest.expected_recovered_digest(check).unwrap();
        let root = sealed_store_root(&database_path);
        let foreign = temp.path().join("foreign");
        std::fs::create_dir(&foreign).unwrap();
        std::fs::write(foreign.join("sentinel"), b"not owned").unwrap();
        std::os::unix::fs::symlink(&foreign, &root).unwrap();

        let result = build_or_open_sealed_store(
            SealedRowSource::Manifest(&manifest),
            &identity,
            &expected,
            &database_path,
            check,
        );
        assert!(
            result.is_err(),
            "a symlinked sealed root must refuse writes"
        );
        assert_eq!(
            std::fs::read(foreign.join("sentinel")).unwrap(),
            b"not owned"
        );
        assert_eq!(
            std::fs::read_dir(&foreign).unwrap().count(),
            1,
            "no staged container or receipt may be written into the foreign root"
        );

        std::fs::remove_file(&root).unwrap();
        let (store, _) = build_or_open_sealed_store(
            SealedRowSource::Manifest(&manifest),
            &identity,
            &expected,
            &database_path,
            check,
        )
        .expect("a genuine sealed root still supports building");
        assert_eq!(store.recovered_digest(), expected.as_str());
    }

    /// A pre-existing staging symlink must not become a writable artifact
    /// root or be unlinked as disposable scratch space.
    #[cfg(unix)]
    #[test]
    fn sealed_build_refuses_a_foreign_staging_symlink() {
        let check: &dyn Fn() -> Result<(), GraphDbError> = &|| Ok(());
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("source.grafeo");
        let manifest = manifest(2, 1);
        let identity = manifest.identity();
        let expected = manifest.expected_recovered_digest(check).unwrap();
        let root = sealed_store_root(&database_path);
        let directory = sealed_generation_directory(&root, &identity.physical_namespace().unwrap());
        let staging = root.join(format!(
            ".staging-{}",
            directory.file_name().unwrap().to_str().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let foreign = temp.path().join("foreign");
        std::fs::create_dir(&foreign).unwrap();
        std::fs::write(foreign.join("sentinel"), b"unchanged").unwrap();
        std::os::unix::fs::symlink(&foreign, &staging).unwrap();

        let result = build_or_open_sealed_store(
            SealedRowSource::Manifest(&manifest),
            &identity,
            &expected,
            &database_path,
            check,
        );
        assert!(result.is_err(), "a symlink must refuse the build");
        assert!(
            std::fs::symlink_metadata(&staging)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read(foreign.join("sentinel")).unwrap(),
            b"unchanged"
        );
        assert!(
            !foreign.join(SEALED_STORE_DATABASE_FILE).exists()
                && !foreign.join("sealed.json").exists(),
            "the foreign directory must not receive a container or receipt"
        );
        assert!(!directory.exists(), "no generation was installed");

        // Removing the foreign link must restore a successful, verified seal.
        // A fixture that always refuses to build would not prove the fix.
        std::fs::remove_file(&staging).unwrap();
        let (store, _) = build_or_open_sealed_store(
            SealedRowSource::Manifest(&manifest),
            &identity,
            &expected,
            &database_path,
            check,
        )
        .expect("a normal staging directory must still produce a sealed generation");
        assert_eq!(store.recovered_digest(), expected.as_str());
        assert!(
            directory.join(SEALED_STORE_DATABASE_FILE).is_file(),
            "valid staging must install a verified sealed container"
        );
    }

    /// A pre-existing real staging directory is not disposable merely
    /// because its name matches the generation being built.
    #[test]
    fn sealed_build_preserves_a_foreign_real_staging_directory() {
        let check: &dyn Fn() -> Result<(), GraphDbError> = &|| Ok(());
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("source.grafeo");
        let manifest = manifest(2, 1);
        let identity = manifest.identity();
        let expected = manifest.expected_recovered_digest(check).unwrap();
        let root = sealed_store_root(&database_path);
        let directory = sealed_generation_directory(&root, &identity.physical_namespace().unwrap());
        let staging = root.join(format!(
            ".staging-{}",
            directory.file_name().unwrap().to_str().unwrap()
        ));
        std::fs::create_dir_all(&staging).unwrap();
        let sentinel = staging.join("foreign-sentinel");
        std::fs::write(&sentinel, b"must survive").unwrap();

        let result = build_or_open_sealed_store(
            SealedRowSource::Manifest(&manifest),
            &identity,
            &expected,
            &database_path,
            check,
        );
        assert!(
            result.is_err(),
            "an occupied staging entry must refuse sealing"
        );
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"must survive");
        assert!(
            !staging.join(SEALED_STORE_DATABASE_FILE).exists()
                && !staging.join(SEALED_STORE_RECEIPT_FILE).exists(),
            "refused sealing must not modify foreign staging data"
        );
        assert!(!directory.exists(), "no generation may be installed");

        std::fs::remove_dir_all(&staging).unwrap();
        let (store, _) = build_or_open_sealed_store(
            SealedRowSource::Manifest(&manifest),
            &identity,
            &expected,
            &database_path,
            check,
        )
        .expect("clearing the unknown stage allows a verified seal");
        assert_eq!(store.recovered_digest(), expected.as_str());
        assert!(directory.join(SEALED_STORE_DATABASE_FILE).is_file());
    }

    /// A store remount invokes the real startup sweep while a producer is
    /// still streaming rows. The sweep must preserve its owner-bound staging
    /// directory, or the builder can no longer install the verified artifact.
    #[test]
    fn remount_preserves_live_sealed_staging_and_seal_completes() {
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("source.grafeo");
        let manifest = manifest(48, 24);
        let identity = manifest.identity();
        let expected = manifest.expected_recovered_digest(&|| Ok(())).unwrap();
        let root = sealed_store_root(&database_path);
        let directory = sealed_generation_directory(&root, &identity.physical_namespace().unwrap());
        let staging = root.join(format!(
            ".staging-{}",
            directory.file_name().unwrap().to_str().unwrap()
        ));
        let remounted = std::cell::Cell::new(false);
        let check = || {
            if !remounted.replace(true) {
                assert!(staging.join(SEALED_STAGING_OWNER_FILE).is_file());
                let remount = open_source(&database_path);
                drop(remount);
                assert!(
                    staging.join(SEALED_STAGING_OWNER_FILE).is_file(),
                    "remount must not reclaim a live producer's staged container"
                );
            }
            Ok(())
        };
        let (store, _) = build_or_open_sealed_store(
            SealedRowSource::Manifest(&manifest),
            &identity,
            &expected,
            &database_path,
            &check,
        )
        .expect("live seal must finish after a store remount");
        assert!(
            remounted.get(),
            "remount must run inside the row-copy callback"
        );
        assert!(directory.join(SEALED_STORE_DATABASE_FILE).is_file());
        assert_eq!(store.recovered_digest(), expected.as_str());
        assert!(!staging.exists());
    }

    /// Kill the real builder during row preparation, before sealed.json is
    /// published. Its early receipt must let a later store-open sweep reclaim
    /// the crashed stage without touching foreign staging data.
    #[test]
    fn crashed_seal_before_final_receipt_is_reclaimed_and_can_retry() {
        const CHILD_ROOT: &str = "TRACEDECAY_TEST_CRASHED_SEAL_ROOT";
        const CRASHED: i32 = 43;
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let database_path = std::path::PathBuf::from(root).join("source.grafeo");
            let manifest = manifest(24, 12);
            let identity = manifest.identity();
            let expected = manifest.expected_recovered_digest(&|| Ok(())).unwrap();
            let exit_during_copy = || -> Result<(), GraphDbError> {
                std::process::exit(CRASHED);
            };
            let _ = build_or_open_sealed_store(
                SealedRowSource::Manifest(&manifest),
                &identity,
                &expected,
                &database_path,
                &exit_during_copy,
            );
            panic!("the row stream did not invoke its cancellation callback");
        }

        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("source.grafeo");
        let manifest = manifest(24, 12);
        let identity = manifest.identity();
        let expected = manifest.expected_recovered_digest(&|| Ok(())).unwrap();
        let root = sealed_store_root(&database_path);
        let directory = sealed_generation_directory(&root, &identity.physical_namespace().unwrap());
        let staging = root.join(format!(
            ".staging-{}",
            directory.file_name().unwrap().to_str().unwrap()
        ));
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(std::thread::current().name().unwrap())
            .env(CHILD_ROOT, temp.path())
            .output()
            .unwrap();
        assert_eq!(child.status.code(), Some(CRASHED), "{child:?}");
        assert!(staging.join(SEALED_STAGING_OWNER_FILE).is_file());
        assert!(!staging.join(SEALED_STORE_RECEIPT_FILE).exists());

        let foreign = root.join(format!(".staging-{}", "0".repeat(64)));
        std::fs::create_dir(&foreign).unwrap();
        std::fs::write(foreign.join("sentinel"), b"foreign").unwrap();
        sweep_abandoned_sealed_staging(&database_path);
        assert!(
            !staging.exists(),
            "the crashed but proven stage must be reclaimed"
        );
        assert_eq!(std::fs::read(foreign.join("sentinel")).unwrap(), b"foreign");

        let (store, _) = build_or_open_sealed_store(
            SealedRowSource::Manifest(&manifest),
            &identity,
            &expected,
            &database_path,
            &|| Ok(()),
        )
        .unwrap();
        assert_eq!(store.recovered_digest(), expected.as_str());
        assert!(directory.join(SEALED_STORE_DATABASE_FILE).is_file());
    }

    /// The failed-builder cleanup must not delete a foreign replacement of a
    /// staging pathname after the owner marker was written.
    #[cfg(unix)]
    #[test]
    fn interrupted_seal_preserves_a_substituted_staging_directory() {
        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("source.grafeo");
        let manifest = manifest(24, 12);
        let identity = manifest.identity();
        let expected = manifest.expected_recovered_digest(&|| Ok(())).unwrap();
        let root = sealed_store_root(&database_path);
        let directory = sealed_generation_directory(&root, &identity.physical_namespace().unwrap());
        let staging = root.join(format!(
            ".staging-{}",
            directory.file_name().unwrap().to_str().unwrap()
        ));
        let moved = root.join("original-stage");
        let swapped = std::cell::Cell::new(false);
        let interrupt = || {
            if !swapped.replace(true) {
                std::fs::rename(&staging, &moved).unwrap();
                std::fs::create_dir(&staging).unwrap();
                std::fs::write(staging.join("sentinel"), b"foreign replacement").unwrap();
            }
            Err(GraphDbError::Cancelled)
        };
        let result = build_or_open_sealed_store(
            SealedRowSource::Manifest(&manifest),
            &identity,
            &expected,
            &database_path,
            &interrupt,
        );
        assert!(
            matches!(&result, Err(GraphDbError::Cancelled)),
            "{result:?}"
        );
        assert_eq!(
            std::fs::read(staging.join("sentinel")).unwrap(),
            b"foreign replacement",
            "cleanup must not erase the substituted directory"
        );
        assert!(moved.join(SEALED_STAGING_OWNER_FILE).is_file());
        assert!(!directory.exists());

        std::fs::remove_dir_all(&staging).unwrap();
        std::fs::remove_dir_all(&moved).unwrap();
        let (store, _) = build_or_open_sealed_store(
            SealedRowSource::Manifest(&manifest),
            &identity,
            &expected,
            &database_path,
            &|| Ok(()),
        )
        .unwrap();
        assert_eq!(store.recovered_digest(), expected.as_str());
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

        // A crash before the receipt exists leaves no deletion authority.
        // A retry must preserve the unverified staging bytes, not silently
        // treat a name collision as proof that the directory belongs to us.
        std::fs::create_dir_all(&staging).unwrap();
        let torn_container = staging.join(SEALED_STORE_DATABASE_FILE);
        std::fs::write(&torn_container, b"torn container").unwrap();
        let refused = build_or_open_sealed_store(
            SealedRowSource::Manifest(&manifest),
            &identity,
            &expected,
            &database_path,
            check,
        );
        assert!(
            matches!(&refused, Err(GraphDbError::Unavailable { .. })),
            "an unreceipted staging collision must fail closed: {refused:?}"
        );
        assert_eq!(std::fs::read(&torn_container).unwrap(), b"torn container");
        assert!(
            !directory.exists(),
            "refused build must not publish a generation"
        );

        // The test fixture explicitly removes its unowned crash image.
        // Only then may the same build succeed and prove its normal payload.
        std::fs::remove_dir_all(&staging).unwrap();
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
            "successful build must consume its staging directory"
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
            return std::fs::metadata(path).map_or(0, |meta| meta.len());
        };
        entries
            .filter_map(Result::ok)
            .map(|entry| {
                let path = entry.path();
                if path.is_dir() {
                    directory_bytes(&path)
                } else {
                    std::fs::metadata(&path).map_or(0, |meta| meta.len())
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
        let bytes_payload =
            std::env::var("TRACEDECAY_VERIFY_PROBE_FORM").map_or(true, |form| form != "compact");
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
