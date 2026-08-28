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
//! The sealed store is a **derived artifact**: the staging database remains
//! the authority, every sealed store is digest-verified before installation,
//! and any failure to build or open one falls back to the staging database.
//! Retirement deletes the artifact directory with the generation; quarantine
//! discards it. Nothing ever writes to a sealed store after compaction — the
//! handle is marked read-only and refuses writes with a typed error.
//!
//! On-disk layout, next to the staging database file:
//!
//! ```text
//! graph.grafeo                  <- staging database (authority)
//! graph.sealed/
//!   <physical-namespace-hex>/
//!     generation.grafeo         <- compacted single-generation store
//!     sealed.json               <- receipt binding the recovered digest
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracedecay_store::runtime::GraphRecoveredGenerationDigestV1;

use crate::generation::{
    physical_namespace_projection_map, recovered_entity_ref, verify_recovered_generation,
};
use crate::lease::GenerationLocator;
use crate::location::PersistentGraphStoreState;
use crate::projection::graph_properties_live_bytes;
use crate::state::{
    load_entity, load_entity_by_node, load_relation_by_locator_cached,
    projection_entity_nodes_sorted_checked, projection_relation_nodes_sorted_checked,
    EndpointIdentityCache,
};
use crate::{
    GraphDb, GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDurability, GraphEntity,
    GraphEntityId, GraphFormatVersion, GraphGenerationManifestIdentity, GraphGenerationRelation,
    GraphMutation, GraphNamespace, GraphProjectionIdentity, GraphWriteBatch, NeverCancelled,
    mutation,
};

/// One mutation page applied while copying rows into a sealed store. The
/// bounds mirror native staging so a copy can never exceed the canonical
/// batch budget the staging path already proves out.
const MAX_SEALED_COPY_MUTATIONS: usize = crate::limits::MAX_NATIVE_GENERATION_STAGE_MUTATIONS;
const MAX_SEALED_COPY_LIVE_BYTES: usize = 96 * 1024 * 1024;

const SEALED_STORE_RECEIPT_VERSION: u32 = 1;
const SEALED_STORE_DATABASE_FILE: &str = "generation.grafeo";
const SEALED_STORE_RECEIPT_FILE: &str = "sealed.json";
const SEALED_STORE_DISABLE_ENV: &str = "TRACEDECAY_GRAPH_SEALED_STORE";

const SEALED_STORE_FORM_COMPACT: &str = "compact";
const SEALED_STORE_FORM_REPLAY: &str = "replay";

/// Whether the pinned grafeo revision round-trips `Value::Bytes` through the
/// columnar `CompactStore` codecs.
///
/// At rev `019d353b14` it does not: a Bytes column falls back to the
/// dictionary codec (`compact/builder.rs`, `infer_type_from_values`), and
/// `Column::value()` restores every dictionary entry as `Value::String`
/// (`compact/column.rs`). TraceDecay serializes a Bytes payload onto nearly
/// every code-graph entity, so compacting such a generation would fail its
/// post-reopen recovered-digest proof. The fork fix exists: branch
/// `tracedecay/0.5.42-compact-bytes-roundtrip` (rev `0bc27542`, stacked on
/// the pinned `tracedecay/0.5.42-close-and-overlay`) encodes Bytes entries
/// losslessly inside the string dictionary, and the full sealed-store
/// contract passes against it with this constant flipped. Until that branch
/// is picked up by the workspace pin, a generation whose rows carry any
/// Bytes property is sealed in **replay form**: still its own isolated
/// single-generation store — generation-scoped open, retirement by directory
/// delete, read routing — just without the columnar base. Flip this with the
/// pin move (and the `bytes_rows_seal_in_replay_form_and_read_exactly`
/// expectation with it); the post-reopen digest proof will refuse any store
/// the flip mis-declares.
const COMPACT_ROUND_TRIPS_BYTES: bool = false;

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

/// A reopened, digest-verified, compacted single-generation store.
pub(crate) struct SealedGenerationStore {
    locator: GenerationLocator,
    recovered_digest: String,
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

    /// Best-effort teardown of the artifact: close the handle and remove the
    /// directory. The staging database remains the authority, so failures are
    /// swallowed after marking the handle closed.
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

fn entity_copy_live_bytes(entity: &GraphEntity) -> usize {
    let labels: usize = entity
        .labels
        .iter()
        .map(|label| label.as_str().len())
        .sum();
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
                    locator.projection.namespace,
                    locator.projection.projection,
                    locator.generation
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

    /// Ensures the sealed compacted store for `identity` exists, is
    /// digest-verified, and is installed for reads. Builds it from this
    /// staging database's verified rows when missing.
    ///
    /// A memory-backed database and a disabled lane both return `Ok(())`
    /// without an artifact: sealed stores are a derived read-path artifact
    /// and never a publication precondition in those configurations.
    #[hotpath::measure(label = "graph_db.sealed_store.ensure", impl_type = "GraphDb")]
    pub(crate) fn ensure_sealed_generation_store(
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
            if let Some(existing) = sealed.get(&locator) {
                if existing.recovered_digest() == expected.as_str() {
                    return Ok(());
                }
            }
        }
        let store = build_or_open_sealed_store(self, identity, expected, &database_path, check)?;
        self.install_sealed_generation_store(locator, store)
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

    fn install_sealed_generation_store(
        &self,
        locator: GenerationLocator,
        store: Arc<SealedGenerationStore>,
    ) -> Result<(), GraphDbError> {
        let mut sealed = self
            .inner
            .sealed_generations
            .write()
            .map_err(|_| GraphDbError::unavailable("sealed generation store lock is poisoned"))?;
        if let Some(previous) = sealed.insert(locator, store) {
            // The replacement shares the artifact directory, so only the
            // superseded handle is closed; the files stay for the new reader.
            let _ = previous.database.close();
        }
        Ok(())
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
        mutation::apply(
            database,
            &mut state,
            batch,
            mutation::CommitMetadata {
                digest,
                generation_dependency_digest: dependency_digest,
                publication_record: None,
            },
            endpoint_namespaces,
            &self.inner.poisoned,
            check,
        )?;
        if self.inner.durability == GraphDurability::WalSync {
            crate::runtime::sync_wal(database)?;
        }
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
    pub fn open_sealed_artifact_for_bench(
        directory: &Path,
    ) -> Result<Arc<GraphDb>, GraphDbError> {
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
) -> Result<Arc<SealedGenerationStore>, GraphDbError> {
    let physical_namespace = identity.physical_namespace()?;
    let root = sealed_store_root(database_path);
    let directory = sealed_generation_directory(&root, &physical_namespace);
    // Idempotent replay: an artifact from an earlier seal of this exact
    // generation is adopted if its receipt binds the same digest.
    match open_sealed_store(&directory, identity, expected) {
        Ok(Some(store)) => return Ok(store),
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
    let built = copy_compact_and_close(source, identity, expected, &staging, check)
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
    open_sealed_store(&directory, identity, expected)?.ok_or_else(|| {
        GraphDbError::unavailable(
            "sealed generation store disappeared between install and reopen".to_owned(),
        )
    })
}

/// Streams the generation's verified rows into a fresh database under
/// `staging`, proves the recovered digest reproduces, compacts when the
/// pinned engine can round-trip every value in the row set, and closes.
/// Returns the copied `(entities, relations)` counts and the sealed form.
fn copy_compact_and_close(
    source: &GraphDb,
    identity: &GraphGenerationManifestIdentity,
    expected: &GraphRecoveredGenerationDigestV1,
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
    let (entity_nodes, relation_rows, dependency_endpoints) = {
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
        let store = database.graph_store();
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
        for (_, locator) in relation_locators {
            check()?;
            let stored = load_relation_by_locator_cached(database, locator, &mut endpoint_cache)?;
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
        (entity_nodes, relation_rows, dependency_endpoints)
    };
    let entity_count = entity_nodes.len();
    let relation_count = relation_rows.len();
    let mut saw_bytes_property = relation_rows
        .iter()
        .any(|relation| properties_carry_bytes(&relation.properties));

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
    for (_, node) in entity_nodes {
        check()?;
        let entity = {
            let guard = source.read_guard()?;
            let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
            load_entity_by_node(database, node)?.entity
        };
        saw_bytes_property |= properties_carry_bytes(&entity.properties);
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

    // Prove the copy: the sealed rows must reproduce the recovered digest
    // before anything is compacted or served.
    {
        let guard = sealed.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        verify_recovered_generation(database, identity, expected, check)
            .map_err(|error| sealed_store_failure("pre-compact verification failed", error))?;
    }

    // Compact only when the pinned engine round-trips every scalar the rows
    // carry; otherwise the artifact stays in replay form, still isolated per
    // generation. The post-reopen digest proof re-checks whichever form was
    // written, so a wrong choice here surfaces as a typed refusal, never as
    // silently wrong reads.
    let form = if saw_bytes_property && !COMPACT_ROUND_TRIPS_BYTES {
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
    let database = GraphDb::open_with_store_state(
        sealed_database_options(database_path),
        Some(PersistentGraphStoreState::Existing),
    )
    .map_err(|error| sealed_store_failure("reopen failed", error))?;
    // Prove the compacted, reopened store serves exactly the sealed rows.
    // This runs once per install (build or recovery adoption), so a corrupt
    // or truncated artifact is discarded before it answers a single read.
    {
        let guard = database.read_guard()?;
        let native = guard.as_ref().ok_or(GraphDbError::Closed)?;
        if let Err(error) = verify_recovered_generation(native, identity, expected, &|| Ok(())) {
            drop(guard);
            let _ = database.close();
            return Err(sealed_store_failure("post-reopen verification failed", error));
        }
    }
    database.mark_sealed_read_only();
    Ok(Some(Arc::new(SealedGenerationStore {
        locator: GenerationLocator::new(identity.projection.clone(), identity.generation.clone()),
        recovered_digest: expected.as_str().to_owned(),
        directory: directory.to_path_buf(),
        database,
    })))
}
