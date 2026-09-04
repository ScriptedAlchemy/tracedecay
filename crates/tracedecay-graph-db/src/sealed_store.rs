//! Per-generation sealed compact stores.
//!
//! `GrafeoDB::compact()` freezes a whole database, while TraceDecay
//! immutability is per *generation*: one staging database holds every
//! generation in physical namespaces, so compacting it at a seal would freeze
//! the store the next generation stages into. This module aligns the two
//! scopes instead of fighting them: at seal time the just-verified
//! generation's rows are streamed into their own single-generation Grafeo
//! database, that database is compacted (its whole-store scope now covers
//! exactly one immutable generation), closed, reopened from its columnar
//! `CompactStore` section, and proven against the generation's recovered
//! digest before it serves a single read.
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
//! discards it. Nothing ever writes to a sealed store after compaction — the
//! handle is marked read-only and refuses writes with a typed error.
//!
//! On-disk layout, next to the staging database file:
//!
//! ```text
//! graph.grafeo                  <- mutable staging database
//! graph.sealed/
//!   <physical-namespace-hex>/
//!     generation.grafeo         <- compacted single-generation store
//!     sealed.json               <- receipt binding the recovered digest
//! ```
//!
//! # Sealed-read-bundle integration point
//!
//! Each `<physical-namespace-hex>/` directory is a self-describing,
//! digest-bound, immutable artifact — exactly the shape a sealed-read bundle
//! catalogs. The bundle manifest work owns the catalog; its integration
//! point here is the directory plus `sealed.json` (identity, physical
//! namespace, recovered digest, row counts, and the `form` the store was
//! sealed in). A bundle that ships this directory can be adopted on any host
//! through [`GraphDb::open_sealed_generation_store_if_present`], which
//! re-proves the digest before the store serves a read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracedecay_store::runtime::GraphRecoveredGenerationDigestV1;

use crate::generation::{
    physical_namespace_projection_map, recovered_entity_ref, verify_sealed_copy_generation,
};
use crate::lease::GenerationLocator;
use crate::location::PersistentGraphStoreState;
use crate::projection::graph_properties_live_bytes;
use crate::state::{
    EndpointIdentityCache, latest_projection, load_entity, load_relation_by_locator_cached,
    projection_entity_nodes_sorted_checked, projection_relation_nodes_sorted_checked,
};
use crate::{
    GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability, GraphEntity,
    GraphEntityId, GraphFormatVersion, GraphGenerationManifestIdentity, GraphGenerationRelation,
    GraphMutation, GraphNamespace, GraphProjectionIdentity, GraphWriteBatch, NeverCancelled,
    mutation,
};

/// Opens and verifies one dependency-free sealed generation without opening
/// the shared mutable staging database.
#[hotpath::measure(label = "graph_db.sealed_store.open_direct")]
pub(crate) fn open_direct_sealed_generation(
    database_path: &Path,
    projection: crate::GraphProjectionIdentity,
    generation: crate::GraphGenerationId,
    expected: &GraphRecoveredGenerationDigestV1,
    authority_lease: Arc<dyn tracedecay_store::RetainedGraphStoreLeaseV1>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Option<(crate::GraphDbLeaseV1, GraphGenerationManifestIdentity)>, GraphDbError> {
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
        sealed_database_options(sealed_path),
        PersistentGraphStoreState::Existing,
    )
    .map_err(|error| match error {
        error @ (GraphDbError::ProjectionMismatch { .. }
        | GraphDbError::GenerationMismatch { .. }) => error,
        error => sealed_store_failure("reopen failed", error),
    })?;
    let identity = {
        let guard = database.read_guard()?;
        let native = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let recovered = latest_projection(native, &physical_namespace, &projection.projection)?
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
    // Same marker-aware proof as registry adoption: a boot that reopens the
    // exact bytes an earlier open already proved resolves by stat, and a
    // fresh or moved container pays the full row proof and files the marker.
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
    Ok(Some((lease, identity)))
}

/// One mutation page applied while copying rows into a sealed store. The
/// bounds mirror native staging so a copy can never exceed the canonical
/// batch budget the staging path already proves out.
const MAX_SEALED_COPY_MUTATIONS: usize = crate::limits::MAX_NATIVE_GENERATION_STAGE_MUTATIONS;
const MAX_SEALED_COPY_LIVE_BYTES: usize = 96 * 1024 * 1024;
/// Rows loaded from the shared staging database per read-guard hold while a
/// sealed copy streams. Small enough that a queued writer (and the readers
/// that pile up behind it under `std::sync::RwLock`'s writer preference)
/// waits milliseconds, large enough that lock churn stays negligible against
/// row decode cost.
const SEALED_COPY_GUARD_CHUNK_ROWS: usize = 4096;

const SEALED_STORE_RECEIPT_VERSION: u32 = 1;
const SEALED_STORE_DATABASE_FILE: &str = "generation.grafeo";
const SEALED_STORE_RECEIPT_FILE: &str = "sealed.json";
const SEALED_STORE_DISABLE_ENV: &str = "TRACEDECAY_GRAPH_SEALED_STORE";

const SEALED_STORE_FORM_COMPACT: &str = "compact";
const SEALED_STORE_FORM_REPLAY: &str = "replay";

/// Whether Bytes-carrying generations may seal in compact columnar form on
/// the pinned grafeo revision.
///
/// The pinned rev (`0c4f93a584e9`) carries both compact Bytes dictionary
/// markers and stable preserved-edge ordering: equal-source edge IDs stay in
/// the same order as the native CSR topology. A 45,000-entity /
/// 51,428-relation Bytes generation passed the full post-reopen recovered
/// digest proof in compact form before this contract was enabled.
///
/// Vector-carrying generations never compact regardless of this constant
/// (`saw_vector_property` in [`seal_generation_store`]): the sealed lane
/// never serves vector search, so the columnar base buys them nothing, and
/// mixed-dimension vector columns fall back to the dictionary codec's
/// `Display` encoding, which does not round-trip.
const COMPACT_ROUND_TRIPS_BYTES: bool = true;

/// Minimum code-generation row count that may pay eager compact construction.
///
/// The first production-shaped comparison (45,000 entities + 51,428
/// relations) grew from 192.4 MiB in replay form to 204.5 MiB in compact form,
/// while eager construction added 42% to seal-to-activation time. The next
/// power of two keeps that measured no-win class on the replay path without
/// disabling compact Bytes correctness or large-generation eligibility.
const MIN_EAGER_COMPACT_BYTES_GENERATION_ROWS: usize = 128 * 1024;

/// Receipt binding a sealed store directory to the exact generation and
/// recovered digest it was built from. Written after the compacted database
/// is durably closed; an open that finds a receipt for a different digest
/// discards the artifact instead of serving it.
#[derive(Debug, Deserialize, Serialize)]
struct SealedStoreReceiptV1 {
    version: u32,
    /// `"compact"` when the store serves from a columnar `CompactStore`
    /// base, `"replay"` when it stayed in LPG replay form (see
    /// [`COMPACT_ROUND_TRIPS_BYTES`]).
    form: String,
    namespace: String,
    projection: String,
    generation: String,
    physical_namespace: String,
    recovered_digest: String,
    entities: usize,
    relations: usize,
}

/// How [`GraphDb::ensure_sealed_generation_store`] satisfied a publication's
/// request for a sealed per-generation store.
pub(crate) enum SealedStoreInstall {
    /// The sealed-store lane cannot serve this database (feature off,
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
    /// it. An adopted pre-existing artifact proves only itself — its rows
    /// were never read out of this container — so it carries `None` and the
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
    recovered_digest: String,
    entity_count: usize,
    relation_count: usize,
    /// Canonical bytes hashed by the post-reopen digest proof — the size of
    /// the exact row stream `recovered_digest` covers.
    canonical_bytes: u64,
    directory: PathBuf,
    database: Arc<GraphDb>,
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

    /// Canonical bytes the post-reopen digest proof covered — the served
    /// index size this generation is retained for.
    pub(crate) fn canonical_bytes(&self) -> u64 {
        self.canonical_bytes
    }

    /// Whether this reader's native engine is currently materialized.
    ///
    /// A retained sealed generation with no resident engine costs its
    /// identity and nothing else; this is the falsifiable form of that claim.
    pub(crate) fn engine_resident(&self) -> bool {
        self.database.native_engine_open().unwrap_or(false)
    }

    /// Best-effort teardown used only when the generation is quarantined or
    /// retired, or before staging rows become releasable. A sealed-only head
    /// is never discarded through this path while it remains serveable.
    fn discard(&self) {
        let _ = self.database.close();
        remove_sealed_directory(&self.directory);
    }
}

/// Whether the sealed-store lane is disabled through the environment.
///
/// Sealed stores are on by default: `TRACEDECAY_GRAPH_SEALED_STORE=off`
/// (or `0`/`false`/`disabled`) is the operational kill-switch.
fn sealed_store_disabled() -> bool {
    if cfg!(not(feature = "graph-sealed-store")) {
        return true;
    }
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

fn sealed_database_options(path: PathBuf) -> GraphDbOpenOptions {
    GraphDbOpenOptions {
        location: GraphDbLocation::Persistent(path),
        expected_format: GraphFormatVersion::current(),
        durability: GraphDurability::WalSync,
        cancellation: Arc::new(NeverCancelled),
    }
}

fn sealed_store_failure(context: &str, error: GraphDbError) -> GraphDbError {
    GraphDbError::unavailable(format!("sealed generation store {context}: {error}"))
}

fn sealed_store_io_failure(context: &str, error: std::io::Error) -> GraphDbError {
    GraphDbError::unavailable(format!("sealed generation store {context}: {error}"))
}

/// One copy page staged for application into the sealed store.
struct SealedCopyPager {
    namespace: GraphNamespace,
    projection: crate::GraphProjectionId,
    source_generation: crate::SourceGeneration,
    watermark: crate::GraphWatermark,
    mutations: Vec<GraphMutation>,
    endpoint_namespaces: mutation::RelationEndpointNamespaces,
    live_bytes: usize,
}

impl SealedCopyPager {
    fn new(
        namespace: GraphNamespace,
        projection: crate::GraphProjectionId,
        identity: &GraphGenerationManifestIdentity,
    ) -> Self {
        Self {
            namespace,
            projection,
            source_generation: identity.source_generation.clone(),
            watermark: identity.watermark.clone(),
            mutations: Vec::new(),
            endpoint_namespaces: mutation::RelationEndpointNamespaces::new(),
            live_bytes: 0,
        }
    }

    fn push(
        &mut self,
        sealed: &GraphDb,
        mutation_row: GraphMutation,
        endpoints: Option<(crate::GraphRelationId, (GraphNamespace, GraphNamespace))>,
        live_bytes: usize,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        let page_is_full = self.mutations.len() >= MAX_SEALED_COPY_MUTATIONS
            || (!self.mutations.is_empty()
                && self.live_bytes.saturating_add(live_bytes) > MAX_SEALED_COPY_LIVE_BYTES);
        if page_is_full {
            self.flush(sealed, check)?;
        }
        if let Some((relation, namespaces)) = endpoints {
            self.endpoint_namespaces.insert(relation, namespaces);
        }
        self.mutations.push(mutation_row);
        self.live_bytes = self.live_bytes.saturating_add(live_bytes);
        Ok(())
    }

    fn flush(
        &mut self,
        sealed: &GraphDb,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        if self.mutations.is_empty() {
            return Ok(());
        }
        let batch = GraphWriteBatch::new_canonical_checked(
            self.namespace.clone(),
            self.projection.clone(),
            self.source_generation.clone(),
            self.watermark.clone(),
            std::mem::take(&mut self.mutations),
            check,
        )?;
        let endpoint_namespaces = std::mem::take(&mut self.endpoint_namespaces);
        self.live_bytes = 0;
        sealed.apply_sealed_copy_batch(batch, &endpoint_namespaces, None, check)?;
        Ok(())
    }
}

fn properties_carry_bytes(
    properties: &std::collections::BTreeMap<crate::GraphPropertyName, crate::GraphProperty>,
) -> bool {
    properties
        .values()
        .any(|property| matches!(property, crate::GraphProperty::Bytes(_)))
}

fn properties_carry_vectors(
    properties: &std::collections::BTreeMap<crate::GraphPropertyName, crate::GraphProperty>,
) -> bool {
    properties
        .values()
        .any(|property| matches!(property, crate::GraphProperty::Vector(_)))
}

fn entity_copy_live_bytes(entity: &GraphEntity) -> usize {
    let labels: usize = entity.labels.iter().map(|label| label.as_str().len()).sum();
    entity.identity.as_str().len()
        + labels
        + graph_properties_live_bytes(&entity.properties).unwrap_or(usize::MAX / 4)
}

fn relation_copy_live_bytes(relation: &GraphGenerationRelation) -> usize {
    relation.identity.as_str().len()
        + relation.from.identity.as_str().len()
        + relation.to.identity.as_str().len()
        + relation.kind.as_str().len()
        + graph_properties_live_bytes(&relation.properties).unwrap_or(usize::MAX / 4)
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

    /// Whether a sealed store is installed for `locator` (test observability).
    #[cfg(any(test, feature = "test-helpers", feature = "eval-helpers"))]
    #[must_use]
    pub fn has_sealed_generation_store_for(
        &self,
        namespace: &str,
        projection: &str,
        generation: &str,
    ) -> bool {
        let Ok(sealed) = self.inner.sealed_generations.read() else {
            return false;
        };
        sealed.keys().any(|locator| {
            locator.projection.namespace.as_str() == namespace
                && locator.projection.projection.as_str() == projection
                && locator.generation.as_str() == generation
        })
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
            store.database().close()?;
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
    #[hotpath::measure(label = "graph_db.sealed_store.ensure", impl_type = "GraphDb")]
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
        let (store, staging_proof) =
            build_or_open_sealed_store(self, identity, expected, &database_path, check)?;
        self.install_sealed_generation_store(locator, store)?;
        Ok(SealedStoreInstall::Installed { staging_proof })
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
        match open_sealed_store(&directory, identity, expected) {
            Ok(Some(store)) => self.install_sealed_generation_store(locator, store),
            Ok(None) => Ok(()),
            Err(_) => {
                // A stale or corrupt artifact never outranks the verified
                // staging rows; discard it so a later seal can rebuild.
                remove_sealed_directory(&directory);
                Ok(())
            }
        }
    }

    pub(crate) fn open_installed_sealed_generation_store_if_present(
        &self,
        lease: &crate::lease::VerifiedGenerationLease,
    ) -> Result<(), GraphDbError> {
        if self.sealed_generation_reader(&lease.locator).is_some() {
            return Ok(());
        }
        let Some(commit) = self.generation_commit(&lease.locator)? else {
            return Ok(());
        };
        let identity = GraphGenerationManifestIdentity::new(
            lease.locator.projection.clone(),
            lease.locator.generation.clone(),
            commit.source_generation,
            commit.watermark,
            lease.dependency_identities.clone(),
        );
        self.open_sealed_generation_store_if_present(&identity, &lease.head.recovered_digest)
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
            let _ = previous.database.close();
        }
        // Newly installed generation aside, every other retained sealed
        // reader is now a non-serving owner. Step each idle one down to its
        // identity so the count of concurrently materialized graphs stays
        // bounded by what is actually being read, not by how many
        // generations this process has ever sealed.
        self.hibernate_idle_sealed_generation_engines(Some(&locator));
        self.publish_sealed_generation_census();
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
    pub(crate) fn hibernate_idle_sealed_generation_engines(
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
            match store.database.hibernate_if_lazy_when_idle() {
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
        hotpath::gauge!("graph_db.sealed_store.retained").set(census.retained as f64);
        hotpath::gauge!("graph_db.sealed_store.resident").set(census.resident as f64);
        hotpath::gauge!("graph_db.sealed_store.retained_canonical_bytes")
            .set(census.retained_canonical_bytes as f64);
        hotpath::gauge!("graph_db.sealed_store.resident_canonical_bytes")
            .set(census.resident_canonical_bytes as f64);
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
    #[hotpath::measure(label = "graph_db.sealed_store.retire", impl_type = "GraphDb")]
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

    /// Applies one copy page into a sealed store under construction.
    ///
    /// This is the staging `apply` path minus vector-index maintenance: a
    /// sealed store never serves vector search (HNSW indexes are not durable
    /// and are rebuilt against the staging database), so building one here
    /// would be dead weight in the artifact.
    pub(crate) fn apply_sealed_copy_batch(
        &self,
        mut batch: GraphWriteBatch,
        endpoint_namespaces: &mutation::RelationEndpointNamespaces,
        dependency_digest: Option<
            tracedecay_store::runtime::GraphDependencyGenerationClosureDigestV1,
        >,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        let digest = batch.validate_and_digest()?;
        let _snapshot_gate = self.wait_snapshot_gate_write();
        let guard = self.write_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let mut state = self.state_write_guard()?;
        let state = state
            .as_mut()
            .ok_or_else(|| GraphDbError::unavailable("graph format state is unavailable"))?;
        self.apply_locked_without_vector_index_maintenance(
            database,
            state,
            batch,
            mutation::CommitMetadata {
                digest,
                generation_dependency_digest: dependency_digest,
                publication_record: None,
            },
            endpoint_namespaces,
            check,
        )?;
        Ok(())
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
            sealed_database_options(directory.join(SEALED_STORE_DATABASE_FILE)),
            Some(PersistentGraphStoreState::Existing),
        )
    }
}

/// Builds (or adopts) the sealed store for `identity` and returns the
/// reopened, digest-verified reader.
#[hotpath::measure(label = "graph_db.sealed_store.build")]
fn build_or_open_sealed_store(
    source: &GraphDb,
    identity: &GraphGenerationManifestIdentity,
    expected: &GraphRecoveredGenerationDigestV1,
    database_path: &Path,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(Arc<SealedGenerationStore>, Option<u64>), GraphDbError> {
    let physical_namespace = identity.physical_namespace()?;
    let root = sealed_store_root(database_path);
    let directory = sealed_generation_directory(&root, &physical_namespace);
    // Idempotent replay: an artifact from an earlier seal of this exact
    // generation is adopted if its receipt binds the same digest. Adoption
    // never enumerates `source`'s rows, so it yields no staging proof.
    match open_sealed_store(&directory, identity, expected) {
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
    let built = copy_compact_and_close(source, identity, &staging, check)
        .inspect_err(|_| remove_sealed_directory(&staging));
    let (entities, relations, form) = built?;
    let receipt = SealedStoreReceiptV1 {
        version: SEALED_STORE_RECEIPT_VERSION,
        form: form.to_owned(),
        namespace: identity.projection.namespace.as_str().to_owned(),
        projection: identity.projection.projection.as_str().to_owned(),
        generation: identity.generation.as_str().to_owned(),
        physical_namespace: physical_namespace.as_str().to_owned(),
        recovered_digest: expected.as_str().to_owned(),
        entities,
        relations,
    };
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
    match open_sealed_store(&directory, identity, expected) {
        // This call enumerated the staging database's rows into the copy and
        // the reopen digest matched the authority's expectation: together
        // that is the staging container's own proof, sized by the canonical
        // bytes the reopen hashed.
        Ok(Some(store)) => {
            let canonical_bytes = store.canonical_bytes;
            Ok((store, Some(canonical_bytes)))
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

/// Streams the generation's verified rows into a fresh database under
/// `staging`, proves the recovered digest reproduces, compacts when the
/// pinned engine can round-trip every value in the row set, and closes.
/// Returns the copied `(entities, relations)` counts and the sealed form.
fn copy_compact_and_close(
    source: &GraphDb,
    identity: &GraphGenerationManifestIdentity,
    staging: &Path,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(usize, usize, &'static str), GraphDbError> {
    let physical_namespace = identity.physical_namespace()?;
    let sealed = GraphDb::open_with_store_state(
        sealed_database_options(staging.join(SEALED_STORE_DATABASE_FILE)),
        Some(PersistentGraphStoreState::Prospective),
    )
    .map_err(|error| sealed_store_failure("open for build failed", error))?;

    let dependency_namespaces: BTreeMap<GraphProjectionIdentity, GraphNamespace> = identity
        .dependencies
        .iter()
        .map(|dependency| {
            Ok((
                dependency.projection.clone(),
                crate::generation::physical_namespace(
                    &dependency.projection.namespace,
                    &dependency.projection.projection,
                    &dependency.generation,
                )?,
            ))
        })
        .collect::<Result<_, GraphDbError>>()?;
    let namespace_projection = physical_namespace_projection_map(identity)?;

    // Enumerate exactly the digest's row sets from the staging database.
    // Index scans only under this guard; the row loads below reacquire it in
    // bounded chunks.
    let (entity_nodes, relation_locators) = {
        let guard = source.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let entity_nodes = projection_entity_nodes_sorted_checked(
            database,
            &physical_namespace,
            &identity.projection.projection,
            check,
        )?;
        let relation_locators = projection_relation_nodes_sorted_checked(
            database,
            &physical_namespace,
            &identity.projection.projection,
            check,
        )?;
        (entity_nodes, relation_locators)
    };
    // The staged generation is immutable, so its node handles stay valid
    // across guard reacquisitions (the per-entity copy below has always
    // relied on this). Bounding each hold matters because the staging
    // database is shared: `std::sync::RwLock` blocks new readers once a
    // writer queues, so one corpus-length read guard here turned every
    // concurrent write *and every reader arriving behind it* — memory-graph
    // publication, fact and journey reads — into a build-length stall.
    let mut endpoint_cache = EndpointIdentityCache::default();
    let mut relation_rows = Vec::new();
    relation_rows
        .try_reserve_exact(relation_locators.len())
        .map_err(|_| GraphDbError::unavailable("sealed relation copy set is too large"))?;
    // Endpoint entities living in dependency generations, keyed by the
    // dependency projection so each copy batch stays namespace-exact.
    let mut dependency_endpoints: BTreeMap<
        GraphProjectionIdentity,
        BTreeMap<GraphEntityId, GraphEntity>,
    > = BTreeMap::new();
    for chunk in relation_locators.chunks(SEALED_COPY_GUARD_CHUNK_ROWS) {
        let guard = source.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let store = database.graph_store();
        for (_, locator) in chunk {
            check()?;
            let stored =
                load_relation_by_locator_cached(store.as_ref(), *locator, &mut endpoint_cache)?;
            let from = recovered_entity_ref(store.as_ref(), stored.source, &namespace_projection)?;
            let to = recovered_entity_ref(store.as_ref(), stored.target, &namespace_projection)?;
            for endpoint in [&from, &to] {
                if endpoint.projection == identity.projection {
                    continue;
                }
                let dependency_namespace = dependency_namespaces
                    .get(&endpoint.projection)
                    .ok_or_else(|| GraphDbError::Corrupt {
                        message: "sealed copy relation escapes its dependency closure".to_owned(),
                    })?;
                let copies = dependency_endpoints
                    .entry(endpoint.projection.clone())
                    .or_default();
                if !copies.contains_key(&endpoint.identity) {
                    let entity = load_entity(database, dependency_namespace, &endpoint.identity)?
                        .ok_or_else(|| GraphDbError::Corrupt {
                        message: "sealed copy dependency endpoint disappeared".to_owned(),
                    })?;
                    copies.insert(endpoint.identity.clone(), entity.entity);
                }
            }
            let relation = GraphGenerationRelation::new(
                stored.relation.identity,
                from,
                to,
                stored.relation.kind,
                stored.relation.properties,
            )?;
            relation_rows.push(relation);
        }
    }
    drop(endpoint_cache);
    let entity_count = entity_nodes.len();
    let relation_count = relation_rows.len();
    let mut saw_bytes_property = relation_rows
        .iter()
        .any(|relation| properties_carry_bytes(&relation.properties));
    let mut saw_vector_property = relation_rows
        .iter()
        .any(|relation| properties_carry_vectors(&relation.properties));

    // 1. Dependency endpoint copies, so cross-generation edges resolve.
    for (projection, copies) in dependency_endpoints {
        let namespace = dependency_namespaces
            .get(&projection)
            .cloned()
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "sealed copy dependency namespace disappeared".to_owned(),
            })?;
        let mut pager = SealedCopyPager::new(namespace, projection.projection.clone(), identity);
        for (_, entity) in copies {
            check()?;
            saw_bytes_property |= properties_carry_bytes(&entity.properties);
            saw_vector_property |= properties_carry_vectors(&entity.properties);
            let live_bytes = entity_copy_live_bytes(&entity);
            pager.push(
                &sealed,
                GraphMutation::UpsertEntity(entity),
                None,
                live_bytes,
                check,
            )?;
        }
        pager.flush(&sealed, check)?;
    }

    // 2. The generation's own entities, in recovered-digest order.
    let mut pager = SealedCopyPager::new(
        physical_namespace.clone(),
        identity.projection.projection.clone(),
        identity,
    );
    for chunk in entity_nodes.chunks(SEALED_COPY_GUARD_CHUNK_ROWS) {
        let mut loaded = Vec::with_capacity(chunk.len());
        {
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
                    message: "sealed copy entity disappeared during enumeration".to_owned(),
                })?;
                loaded.push(crate::schema::decode_entity(&record)?);
            }
        }
        for entity in loaded {
            saw_bytes_property |= properties_carry_bytes(&entity.properties);
            saw_vector_property |= properties_carry_vectors(&entity.properties);
            let live_bytes = entity_copy_live_bytes(&entity);
            pager.push(
                &sealed,
                GraphMutation::UpsertEntity(entity),
                None,
                live_bytes,
                check,
            )?;
        }
    }
    pager.flush(&sealed, check)?;

    // 3. The generation's relations, with exact endpoint namespaces.
    let mut pager = SealedCopyPager::new(
        physical_namespace.clone(),
        identity.projection.projection.clone(),
        identity,
    );
    for relation in relation_rows {
        check()?;
        let live_bytes = relation_copy_live_bytes(&relation);
        let from_namespace = if relation.from.projection == identity.projection {
            physical_namespace.clone()
        } else {
            dependency_namespaces
                .get(&relation.from.projection)
                .cloned()
                .ok_or_else(|| GraphDbError::Corrupt {
                    message: "sealed copy relation source escapes its closure".to_owned(),
                })?
        };
        let to_namespace = if relation.to.projection == identity.projection {
            physical_namespace.clone()
        } else {
            dependency_namespaces
                .get(&relation.to.projection)
                .cloned()
                .ok_or_else(|| GraphDbError::Corrupt {
                    message: "sealed copy relation target escapes its closure".to_owned(),
                })?
        };
        let identity_key = relation.identity.clone();
        let storage = relation.storage_relation()?;
        pager.push(
            &sealed,
            GraphMutation::UpsertRelation(storage),
            Some((identity_key, (from_namespace, to_namespace))),
            live_bytes,
            check,
        )?;
    }
    pager.flush(&sealed, check)?;

    // Finalization: exactly like native staging, an empty batch binds the
    // dependency-closure digest to the projection commit — the recovered
    // proof requires it, and it is what marks these rows as a *sealed*
    // generation rather than an unfinished stage.
    let finalization = GraphWriteBatch::new_canonical_checked(
        physical_namespace.clone(),
        identity.projection.projection.clone(),
        identity.source_generation.clone(),
        identity.watermark.clone(),
        Vec::new(),
        check,
    )?;
    sealed.apply_sealed_copy_batch(
        finalization,
        &mutation::RelationEndpointNamespaces::new(),
        Some(identity.dependency_closure_digest(check)?),
        check,
    )?;

    // Compact only when the pinned engine round-trips every scalar and the
    // measured form policy justifies paying eager construction. Small
    // Bytes-carrying code generations stay in replay form because compacting
    // the measured 96,428-row shape made the artifact larger and delayed
    // activation. Vector-carrying generations always stay in replay form:
    // mixed-dimension vectors still use a lossy display dictionary in compact
    // form. The post-reopen digest proof checks whichever durable form was
    // selected before installation.
    let generation_rows = entity_count.saturating_add(relation_count);
    let compact_bytes_generation = saw_bytes_property
        && COMPACT_ROUND_TRIPS_BYTES
        && generation_rows >= MIN_EAGER_COMPACT_BYTES_GENERATION_ROWS;
    let should_compact = !saw_vector_property && (!saw_bytes_property || compact_bytes_generation);
    let form = if !should_compact {
        SEALED_STORE_FORM_REPLAY
    } else {
        sealed
            .compact_for_seal()
            .map_err(|error| sealed_store_failure("compact failed", error))?;
        SEALED_STORE_FORM_COMPACT
    };
    sealed
        .close()
        .map_err(|error| sealed_store_failure("durable close failed", error))?;
    Ok((entity_count, relation_count, form))
}

/// Opens the artifact under `directory` and proves it against `expected`.
///
/// Returns `Ok(None)` when no artifact exists, `Err` when one exists but is
/// unreadable or bound to a different digest.
#[hotpath::measure(label = "graph_db.sealed_store.open")]
fn open_sealed_store(
    directory: &Path,
    identity: &GraphGenerationManifestIdentity,
    expected: &GraphRecoveredGenerationDigestV1,
) -> Result<Option<Arc<SealedGenerationStore>>, GraphDbError> {
    let receipt_path = directory.join(SEALED_STORE_RECEIPT_FILE);
    let database_path = directory.join(SEALED_STORE_DATABASE_FILE);
    let receipt_bytes = match std::fs::read(&receipt_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(sealed_store_io_failure("receipt read failed", error)),
    };
    let receipt: SealedStoreReceiptV1 = serde_json::from_slice(&receipt_bytes)
        .map_err(|error| GraphDbError::unavailable(format!("sealed receipt decode: {error}")))?;
    let physical_namespace = identity.physical_namespace()?;
    if receipt.version != SEALED_STORE_RECEIPT_VERSION
        || receipt.recovered_digest != expected.as_str()
        || receipt.physical_namespace != physical_namespace.as_str()
        || receipt.namespace != identity.projection.namespace.as_str()
        || receipt.projection != identity.projection.projection.as_str()
        || receipt.generation != identity.generation.as_str()
    {
        return Err(GraphDbError::unavailable(
            "sealed generation store receipt does not bind this generation".to_owned(),
        ));
    }
    // Lazily: installing a sealed reader must not cost a whole in-memory
    // graph. grafeo's store is heap resident, so an eager open here replayed
    // the artifact's entire block log into RAM for every generation this
    // process ever sealed — five retained generations meant five whole graphs
    // (#799), and one published worktree scope meant one more (#830). The
    // proof below resolves by stat whenever the verify-once marker covers
    // these exact container bytes, so a retained-but-unread generation now
    // materializes nothing at all; anything that does read it reopens the
    // same container through `ensure_opened` on first use.
    let database = GraphDb::open_lazy_with_store_state(
        sealed_database_options(database_path),
        PersistentGraphStoreState::Existing,
    )
    .map_err(|error| sealed_store_failure("reopen failed", error))?;
    // Prove the compacted, reopened store serves exactly the sealed rows
    // before it answers a single read. The artifact is immutable after its
    // build, so a proof established by an earlier open of these exact
    // container bytes stands: the marker beside the artifact resolves it by
    // stat, and anything else — a missing or foreign marker, or a container
    // whose file identity moved — falls back to the full row proof and files
    // the marker for the next open. `expected` still comes from the
    // relational authority, exactly as on the staging container.
    let canonical_bytes = match sealed_copy_proof(&database, identity, expected, &|| Ok(())) {
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
    // A full proof had to materialize the engine to stream the rows. The
    // proof is filed now, so the engine is pure resident cost until a read
    // actually arrives: release it and let the first read reopen. A marker
    // hit never opened it, and hibernation is then a no-op.
    if let Err(error) = database.hibernate_if_lazy() {
        let _ = database.close();
        return Err(sealed_store_failure("post-proof hibernation failed", error));
    }
    Ok(Some(Arc::new(SealedGenerationStore {
        locator: GenerationLocator::new(identity.projection.clone(), identity.generation.clone()),
        recovered_digest: expected.as_str().to_owned(),
        entity_count: receipt.entities,
        relation_count: receipt.relations,
        canonical_bytes,
        directory: directory.to_path_buf(),
        database,
    })))
}

/// Resolves the recovered-digest proof for a reopened sealed copy: by the
/// artifact's own verify-once marker when the container bytes are the ones an
/// earlier proof ran over, by the full row-streaming proof otherwise. A full
/// proof files the marker so the next open of unchanged bytes resolves by
/// stat. Returns the canonical byte count the proof covers.
fn sealed_copy_proof(
    database: &GraphDb,
    identity: &GraphGenerationManifestIdentity,
    expected: &GraphRecoveredGenerationDigestV1,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<u64, GraphDbError> {
    let locator = GenerationLocator::new(identity.projection.clone(), identity.generation.clone());
    if let Some(canonical_bytes) = database.inner.markers.lookup(&locator, expected.as_str()) {
        database.inner.markers.record_fresh(&locator);
        #[cfg(test)]
        crate::generation::record_sealed_copy_marker_hit();
        crate::hotpath_observe::record_sealed_copy_verification(
            crate::verified_marker::GenerationVerification::VerifiedFresh,
            canonical_bytes,
        );
        return Ok(canonical_bytes);
    }
    let canonical_bytes = {
        let guard = database.read_guard()?;
        let native = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let (_, canonical_bytes) =
            verify_sealed_copy_generation(native, identity, expected, check)?;
        canonical_bytes
    };
    database
        .inner
        .markers
        .record_proven(&locator, expected.as_str(), canonical_bytes);
    // Published now as well as at close, because both identities matter. An
    // open session writes only the sidecar WAL (index catalog entries), so
    // the container bytes stay exactly the ones this proof ran over for as
    // long as this process serves them: publishing here lets every further
    // open of the artifact in the same boot — the direct-sealed recover and
    // the registry adoption were each paying this proof — resolve by stat.
    // The close-time publish then re-records the checkpointed container for
    // the next boot. A marker is a cache of completed proofs; failing to
    // write one costs the next open a re-proof and nothing else.
    if let Err(error) = database.inner.markers.publish() {
        let _ = error;
    }
    crate::hotpath_observe::record_sealed_copy_verification(
        crate::verified_marker::GenerationVerification::Reverified,
        canonical_bytes,
    );
    Ok(canonical_bytes)
}

/// Measurement harness for the sealed-store verification path, phase by
/// phase, against production-shaped rows (a Bytes payload on every entity by
/// default, or a String payload when `TRACEDECAY_VERIFY_PROBE_FORM=compact`).
///
/// ```text
/// TRACEDECAY_VERIFY_PROBE_ROWS=50000 TRACEDECAY_VERIFY_PROBE_PAYLOAD=700 \
///   cargo test -p tracedecay-graph-db --features graph-sealed-store \
///   --profile perf --lib -- --ignored --nocapture \
///   sealed_store::cost_probe::sealed_verification_cost_probe
/// ```
#[cfg(test)]
mod cost_probe {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;
    use std::time::Instant;

    use super::{build_or_open_sealed_store, open_sealed_store};
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
        #[cfg(feature = "hotpath")]
        let _hotpath = hotpath::HotpathGuardBuilder::new("sealed_verification_cost_probe")
            .functions_limit(0)
            .report("functions-timing")
            .build();

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
        let (store, staging_proof) =
            build_or_open_sealed_store(&database, &identity, &expected, &database_path, check)
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
