//! Root composition for host-admission test runtimes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use tokio::sync::Mutex as AsyncMutex;

use tracedecay_code_index::parallelism::install_worker_plan;
use tracedecay_domain::configuration::CodeIndexWorkerSelectionV1;
use tracedecay_private_fs::background_cpu::process_background_cpu;
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, detected_process_resident_memory_limit_v1,
};

use tracedecay_host_admission::{HostAdmissionAuthorities, HostAdmissionFacade};
#[cfg(test)]
use tracedecay_host_admission::{
    HostAdmissionBroker, HostAdmissionRuntime, SharedHostAdmissionBroker,
};
use tracedecay_sessions::admission::{
    HostAdmissionOutcome, HostAdmissionScope, HostAdmissionStatus,
};
use tracedecay_sessions::runtime::codex::CodexDiscoveryHub;

use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_domain::{BrainId, ProjectId, UserProfileId};
use tracedecay_global_db::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1};
use tracedecay_runtime_core::db::DaemonDatabaseScope;
#[cfg(test)]
use tracedecay_runtime_core::db::DatabaseEngineReadSnapshot;
use tracedecay_runtime_core::weak_registry::WeakRegistry;
use tracedecay_store::StoreShardScopeV1;
use tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1;

#[path = "host_admission/accounting_test_support.rs"]
mod accounting_test_support;
#[path = "host_admission/integration_test_support.rs"]
mod integration_test_support;
#[path = "host_admission/lcm_api_test_support.rs"]
mod lcm_api_test_support;
#[path = "host_admission/lcm_fixture_test_support.rs"]
mod lcm_fixture_test_support;
#[path = "host_admission/profile_registry_test_support.rs"]
mod profile_registry_test_support;
#[path = "host_admission/session_test_support.rs"]
mod session_test_support;
mod verified_graph_test_support;

#[cfg(any(test, feature = "test-transport"))]
#[doc(hidden)]
pub(crate) use verified_graph_test_support::await_bound_graph_runtime;

#[doc(hidden)]
pub use lcm_fixture_test_support::{
    LcmExternalPayloadManifestTestRecord, LcmLineageCountsForTest, LcmLineageFaultForTest,
};
#[doc(hidden)]
pub use profile_registry_test_support::HostAdmissionDatabaseIdentityV1;

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionTemporalFixtureCountV1 {
    ProjectionReceipts,
    Occurrences,
    Assertions,
    RefreshReceipts,
    RefreshProgress,
    RefreshOperations,
    RefreshBindings,
    RefreshBatchBindings,
    TemporalGenerations,
}

/// One daemon session runtime registry per profile root, process-wide.
///
/// Production runs exactly one daemon per profile, and the profile
/// session-relation graph store has a single writer (an exclusive Grafeo file
/// lock), so a second independent registry on the same profile cannot open
/// it. Every test-runtime construction path (project, profile, sibling, and
/// repeated opens of the same project) must therefore join the live registry
/// for its profile. Entries are weak: when the last runtime for a profile
/// drops, its registry drops with it and the next open starts fresh.
static SHARED_TEST_SESSION_REGISTRIES: LazyLock<
    AsyncMutex<WeakRegistry<PathBuf, DaemonSessionRuntimeRegistryV1>>,
> = LazyLock::new(|| AsyncMutex::new(WeakRegistry::new()));

/// Process-scoped scratch-memory authority shared by every host-admission
/// fixture in this test binary. Its capacity follows the same RAM-derived
/// production policy; it is not a repository-size limit.
static SESSION_CAPTURE_TEST_RESIDENT_MEMORY: LazyLock<Arc<ProcessResidentMemoryV1>> =
    LazyLock::new(|| {
        Arc::new(ProcessResidentMemoryV1::new(
            detected_process_resident_memory_limit_v1(),
        ))
    });

/// Installs the process worker plan (and with it the background CPU
/// authority) that host-admission capture requires. Production installs it
/// during daemon worker-plan admission, which these fixtures never run;
/// without it every observation capture is refused with
/// `background_cpu_unavailable`. Going through `install_worker_plan` — the
/// same authority production and the scheduler's test fallback use — keeps
/// the background CPU width consistent with any later worker-plan install in
/// the same test process instead of poisoning it with an ad-hoc width.
#[hotpath::measure(label = "daemon.host_admission.install_background_cpu")]
pub(crate) fn ensure_process_background_cpu_authority() -> Result<()> {
    if process_background_cpu().is_none() {
        let memory = SESSION_CAPTURE_TEST_RESIDENT_MEMORY.snapshot();
        if let Err(error) = install_worker_plan(
            CodeIndexWorkerSelectionV1::Automatic {},
            memory.limit_bytes.saturating_sub(memory.used_bytes),
        ) && process_background_cpu().is_none()
        {
            return Err(TraceDecayError::Config {
                message: format!(
                    "host-admission test runtime could not install the worker plan: {error}"
                ),
            });
        }
    }
    CodexDiscoveryHub::default()
        .configure_preparation_resources(Arc::clone(&SESSION_CAPTURE_TEST_RESIDENT_MEMORY))
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "host-admission test runtime could not install JSONL preparation resources: {error}"
            ),
        })
}

/// Registered host-admission fixture assembled by the composition root.
///
/// This retains the canonical daemon scope, registered databases, and
/// session-runtime registry needed by graph, daemon, MCP, and hook integration
/// tests.
#[doc(hidden)]
pub struct HostAdmissionTestRuntimeV1 {
    brain_id: BrainId,
    profile_id: UserProfileId,
    profile_root: PathBuf,
    project_id: Option<ProjectId>,
    profile_database: RegisteredGlobalDbLeaseV1,
    profile_registered: RegisteredGlobalDbLeaseV1,
    project_registered: Option<RegisteredGlobalDbLeaseV1>,
    session_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    _database_scope: DaemonDatabaseScope,
}

impl HostAdmissionTestRuntimeV1 {
    #[doc(hidden)]
    #[hotpath::skip]
    pub async fn profile(profile_root: impl AsRef<Path>) -> Result<Self> {
        Self::open(profile_root.as_ref().to_path_buf(), None).await
    }

    #[doc(hidden)]
    #[hotpath::skip]
    pub async fn project(
        profile_root: impl AsRef<Path>,
        project_root: impl AsRef<Path>,
        project_id: ProjectId,
    ) -> Result<Self> {
        Self::open(
            profile_root.as_ref().to_path_buf(),
            Some((project_root.as_ref().to_path_buf(), project_id)),
        )
        .await
    }

    /// [`Self::project`] returning proof that project authorities are mounted.
    #[doc(hidden)]
    #[hotpath::skip]
    pub async fn project_scoped(
        profile_root: impl AsRef<Path>,
        project_root: impl AsRef<Path>,
        project_id: ProjectId,
    ) -> Result<ProjectScopedTestRuntimeV1> {
        ProjectScopedTestRuntimeV1::new(
            Self::project(profile_root, project_root, project_id).await?,
        )
    }

    /// Mounts a second registered project through this runtime's daemon
    /// session registry, mirroring production multi-project composition: one
    /// daemon registry holds the single-writer profile authorities and many
    /// project mounts. A second independent runtime on the same profile
    /// cannot exist — the profile session-relation graph has exactly one
    /// writer.
    #[doc(hidden)]
    #[hotpath::measure(label = "daemon.host_admission.mount_sibling_project", future = true)]
    pub async fn sibling_project(
        &self,
        project_root: impl AsRef<Path>,
        project_id: ProjectId,
    ) -> Result<Self> {
        let project_root = project_root.as_ref();
        prepare_host_admission_test_project_root(project_root, &project_id)?;
        let registered = self
            .session_registry
            .project_sessions(project_id.clone(), [project_root.to_path_buf()])
            .await?;
        self.session_registry
            .settle_project_session_graph(&project_id)
            .await?;
        // The shared registry caches project mounts, so a project that was
        // mounted before (opened, dropped, reopened while a sibling keeps the
        // registry alive) already carries its weak graph proxy; only a first
        // mount binds one.
        if registered.project_graph_runtime().is_none() {
            let project_database = self
                .session_registry
                .project_memory(project_id.clone(), [project_root.to_path_buf()])
                .await?;
            let graph_proxy = verified_graph_test_support::await_bound_graph_runtime(
                &project_database,
                "bind sibling test runtime project graph",
            )
            .await?;
            registered
                .bind_project_graph_runtime(graph_proxy)
                .map_err(|_| TraceDecayError::Database {
                    operation: "bind sibling test runtime project graph".to_owned(),
                    message: "project graph runtime was already mounted for project sessions"
                        .to_owned(),
                })?;
        }
        validate_registered_authorities(
            &self.brain_id,
            &self.profile_id,
            Some(&project_id),
            self.profile_database.as_ref(),
            self.profile_registered.as_ref(),
            Some(registered.as_ref()),
        )?;
        let database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            &self.profile_root,
            1,
            "host-admission-test-runtime",
        )?;
        Ok(Self {
            brain_id: self.brain_id.clone(),
            profile_id: self.profile_id.clone(),
            profile_root: self.profile_root.clone(),
            project_id: Some(project_id),
            profile_database: self.profile_database.clone(),
            profile_registered: self.profile_registered.clone(),
            project_registered: Some(registered),
            session_registry: Arc::clone(&self.session_registry),
            _database_scope: database_scope,
        })
    }

    #[hotpath::measure(label = "daemon.host_admission.open_runtime", future = true)]
    async fn open(profile_root: PathBuf, project: Option<(PathBuf, ProjectId)>) -> Result<Self> {
        // Fixture compositions run in-process daemon code that reads the
        // registered product runtime (handshakes, initialize payloads);
        // test processes only ever register the canonical fixture.
        crate::product_runtime::register_fixture_product_runtime();
        ensure_process_background_cpu_authority()?;
        prepare_host_admission_test_profile_root(&profile_root)?;
        if let Some((project_root, project_id)) = project.as_ref() {
            prepare_host_admission_test_project_root(project_root, project_id)?;
        }

        let identity = tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)?;
        let database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            identity.profile_root(),
            1,
            "host-admission-test-runtime",
        )?;
        let session_registry = {
            let profile_key = tracedecay_runtime_core::lifecycle_lease::canonical_or_original(
                identity.profile_root(),
            );
            // Held across construction so two concurrent opens of one profile
            // cannot race into two registries (the loser would fail the
            // exclusive profile session store open).
            let registries = SHARED_TEST_SESSION_REGISTRIES.lock().await;
            match registries.get_live(&profile_key) {
                Some(registry) => registry,
                None => {
                    crate::register_runtime_ports()?;
                    let registry =
                        Arc::new(DaemonSessionRuntimeRegistryV1::open(identity.clone()).await?);
                    registries.insert(profile_key, &registry);
                    registry
                }
            }
        };
        let profile_database = session_registry.profile_database().await?;
        let profile_registered = session_registry.profile_sessions().await?;
        // The session relation graph opens as bounded background work behind
        // the mounted lease. Production tolerates the warming window through
        // typed retryable refusals; the deterministic fixture awaits
        // settlement so graph-dependent operations (LCM compression, relation
        // projection) do not race the open task.
        session_registry.settle_profile_session_graph().await?;
        let (project_id, project_registered) = if let Some((project_root, project_id)) = project {
            let registered = session_registry
                .project_sessions(project_id.clone(), [project_root.clone()])
                .await?;
            session_registry
                .settle_project_session_graph(&project_id)
                .await?;
            // Production project open binds a weak project graph proxy to
            // the registered project-sessions authority before
            // any ingest runs; persist-time git-evidence publication
            // requires that mount, so the canonical test runtime provides
            // the same composition. The shared registry caches project
            // mounts, so a project reopened while its profile registry stays
            // live already carries its weak graph proxy; only a first mount
            // binds one.
            if registered.project_graph_runtime().is_none() {
                let project_database = session_registry
                    .project_memory(project_id.clone(), [project_root])
                    .await?;
                let graph_proxy = verified_graph_test_support::await_bound_graph_runtime(
                    &project_database,
                    "bind test runtime project graph",
                )
                .await?;
                registered
                    .bind_project_graph_runtime(graph_proxy)
                    .map_err(|_| TraceDecayError::Database {
                        operation: "bind test runtime project graph".to_owned(),
                        message: "project graph runtime was already mounted for project sessions"
                            .to_owned(),
                    })?;
            }
            (Some(project_id), Some(registered))
        } else {
            (None, None)
        };
        validate_registered_authorities(
            identity.brain_id(),
            identity.profile_id(),
            project_id.as_ref(),
            profile_database.as_ref(),
            profile_registered.as_ref(),
            project_registered.as_deref(),
        )?;
        Ok(Self {
            brain_id: identity.brain_id().clone(),
            profile_id: identity.profile_id().clone(),
            profile_root,
            project_id,
            profile_database,
            profile_registered,
            project_registered,
            session_registry,
            _database_scope: database_scope,
        })
    }

    #[doc(hidden)]
    pub fn canonical_project_key(project_path: &Path) -> String {
        RegisteredGlobalDb::canonical_project_key(project_path)
    }

    #[doc(hidden)]
    pub fn profile_root_for_test(&self) -> &Path {
        &self.profile_root
    }

    #[doc(hidden)]
    pub fn registered_database(&self, scope: HostAdmissionScope) -> Option<&RegisteredGlobalDb> {
        match scope {
            HostAdmissionScope::Project => self.project_registered.as_deref(),
            HostAdmissionScope::Profile => Some(self.profile_registered.as_ref()),
        }
    }

    #[doc(hidden)]
    pub fn database_path(&self, scope: HostAdmissionScope) -> Option<&Path> {
        self.registered_database(scope)
            .map(RegisteredGlobalDb::db_path)
    }

    #[cfg(test)]
    pub(crate) fn registered_database_arc(
        &self,
        scope: HostAdmissionScope,
    ) -> Option<RegisteredGlobalDbLeaseV1> {
        match scope {
            HostAdmissionScope::Project => self.project_registered.clone(),
            HostAdmissionScope::Profile => Some(self.profile_registered.clone()),
        }
    }

    #[cfg(test)]
    pub(crate) fn session_registry_for_test(&self) -> Arc<DaemonSessionRuntimeRegistryV1> {
        Arc::clone(&self.session_registry)
    }

    #[cfg(test)]
    #[hotpath::skip]
    pub(crate) async fn read_snapshot(
        &self,
        scope: HostAdmissionScope,
    ) -> Result<DatabaseEngineReadSnapshot> {
        self.registered_database(scope)
            .ok_or_else(|| TraceDecayError::Database {
                operation: "open registered session test snapshot".to_owned(),
                message: "registered session test runtime unavailable".to_owned(),
            })?
            .read_snapshot()
            .await
    }

    #[doc(hidden)]
    #[hotpath::skip]
    pub async fn checkpoint_session_database_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> Result<()> {
        self.session_database_for_test(scope)?.checkpoint().await;
        Ok(())
    }

    #[doc(hidden)]
    #[hotpath::measure(label = "daemon.host_admission.scan_storage_bytes")]
    pub fn session_database_storage_bytes_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> Result<u64> {
        let database = self.session_database_for_test(scope)?;
        let mut total = 0u64;
        for suffix in ["", "-wal", "-shm"] {
            let mut path = database.db_path().as_os_str().to_os_string();
            path.push(suffix);
            match std::fs::metadata(PathBuf::from(path)) {
                Ok(metadata) => total = total.saturating_add(metadata.len()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(TraceDecayError::Database {
                        operation: "read retained session database storage bytes".to_owned(),
                        message: error.to_string(),
                    });
                }
            }
        }
        Ok(total)
    }

    #[doc(hidden)]
    #[hotpath::measure(label = "daemon.host_admission.digest_session_domain", future = true)]
    pub async fn session_domain_sha256_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> Result<[u8; 32]> {
        self.checkpoint_session_database_for_test(scope).await?;
        canonical_session_domain_sha256(self.session_database_for_test(scope)?.db_path())
    }

    #[doc(hidden)]
    pub fn observation_store(
        &self,
        scope: HostAdmissionScope,
    ) -> std::result::Result<tracedecay_global_db::GlobalDbObservationStore, HostAdmissionOutcome>
    {
        let database = self
            .registered_database(scope)
            .ok_or_else(registered_authority_unavailable_outcome)?;
        Ok(database.observation_store())
    }

    #[doc(hidden)]
    #[hotpath::measure(label = "daemon.host_admission.replay_observations", future = true)]
    pub async fn replay_observations(
        &self,
        scope: HostAdmissionScope,
        request: tracedecay_store::ObservationReplayRequest,
    ) -> tracedecay_store::ObservationStoreResult<Vec<tracedecay_store::StoredObservation>> {
        use tracedecay_store::ObservationStore as _;

        let store = self.observation_store(scope).map_err(|outcome| {
            tracedecay_store::ObservationStoreError::Storage {
                operation: "bind registered host admission replay",
                source: Box::new(std::io::Error::other(
                    outcome
                        .reason_code
                        .unwrap_or("registered_authority_unavailable"),
                )),
            }
        })?;
        store.replay_observations(request).await
    }

    #[doc(hidden)]
    pub fn session_temporal_store_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> Result<
        tracedecay_session_temporal_store::GlobalDbSessionTemporalStore<
            '_,
            tracedecay_global_db::RegisteredGlobalDb,
        >,
    > {
        Ok(
            tracedecay_session_temporal_store::GlobalDbSessionTemporalStore::new(
                self.session_database_for_test(scope)?,
            ),
        )
    }

    #[doc(hidden)]
    #[hotpath::measure(label = "daemon.host_admission.count_temporal_rows", future = true)]
    pub async fn session_temporal_fixture_count_for_test(
        &self,
        scope: HostAdmissionScope,
        kind: SessionTemporalFixtureCountV1,
    ) -> Result<i64> {
        let table = match kind {
            SessionTemporalFixtureCountV1::ProjectionReceipts => {
                "session_temporal_projection_receipts"
            }
            SessionTemporalFixtureCountV1::Occurrences => "session_occurrences",
            SessionTemporalFixtureCountV1::Assertions => "session_assertions",
            SessionTemporalFixtureCountV1::RefreshReceipts => "session_refresh_receipts",
            SessionTemporalFixtureCountV1::RefreshProgress => "session_refresh_progress",
            SessionTemporalFixtureCountV1::RefreshOperations => "session_refresh_operations",
            SessionTemporalFixtureCountV1::RefreshBindings => "session_refresh_bindings",
            SessionTemporalFixtureCountV1::RefreshBatchBindings => "session_refresh_batch_bindings",
            SessionTemporalFixtureCountV1::TemporalGenerations => "session_temporal_generations",
        };
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "open session-temporal fixture count snapshot".to_owned(),
                message: error.to_string(),
            })?;
        let mut rows = snapshot
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "query session-temporal fixture count".to_owned(),
                message: error.to_string(),
            })?;
        let row = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "read session-temporal fixture count".to_owned(),
                message: error.to_string(),
            })?
            .ok_or_else(|| TraceDecayError::Database {
                operation: "read session-temporal fixture count".to_owned(),
                message: "count query returned no row".to_owned(),
            })?;
        row.get(0).map_err(|error| TraceDecayError::Database {
            operation: "decode session-temporal fixture count".to_owned(),
            message: error.to_string(),
        })
    }

    #[cfg(test)]
    pub(crate) fn into_session_temporal_refresh_test_authority(
        self,
        scope: HostAdmissionScope,
    ) -> Result<crate::daemon::session_runtime_tests::SessionTemporalRefreshTestAuthority> {
        let database =
            self.registered_database_arc(scope)
                .ok_or_else(|| TraceDecayError::Database {
                    operation: "bind session temporal refresh test authority".to_owned(),
                    message: "registered session database mount is unavailable".to_owned(),
                })?;
        Ok(
            crate::daemon::session_runtime_tests::SessionTemporalRefreshTestAuthority::new(
                self, database,
            ),
        )
    }

    #[doc(hidden)]
    #[hotpath::skip]
    pub async fn upsert_session_for_test(
        &self,
        scope: HostAdmissionScope,
        session: &tracedecay_sessions::runtime::SessionRecord,
    ) -> Result<bool> {
        Ok(self
            .session_database_for_test(scope)?
            .upsert_session(session)
            .await)
    }

    #[doc(hidden)]
    #[hotpath::skip]
    pub async fn upsert_session_message_for_test(
        &self,
        scope: HostAdmissionScope,
        message: &tracedecay_sessions::runtime::SessionMessageRecord,
    ) -> Result<bool> {
        let database = self.session_database_for_test(scope)?;
        let session = database
            .get_session(&message.provider, &message.session_id)
            .await
            .ok_or_else(|| TraceDecayError::Database {
                operation: "seed registered session message fixture".to_owned(),
                message: format!(
                    "session {}/{} is unavailable",
                    message.provider, message.session_id
                ),
            })?;
        Ok(database
            .upsert_transcript_batch(
                &session,
                std::slice::from_ref(message),
                &format!(
                    "host-admission-test-message:{}:{}",
                    message.provider, message.message_id
                ),
                tracedecay_global_db::ParseOffset::default(),
            )
            .await)
    }

    #[doc(hidden)]
    #[hotpath::skip]
    pub async fn session_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        session_id: &str,
    ) -> Result<Option<tracedecay_sessions::runtime::SessionRecord>> {
        Ok(self
            .session_database_for_test(scope)?
            .get_session(provider, session_id)
            .await)
    }

    #[doc(hidden)]
    #[hotpath::skip]
    pub async fn session_message_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        message_id: &str,
    ) -> Result<Option<tracedecay_sessions::runtime::SessionMessageRecord>> {
        self.session_database_for_test(scope)?
            .get_session_message(provider, message_id)
            .await
    }

    #[doc(hidden)]
    #[hotpath::measure(label = "daemon.host_admission.upsert_transcript_batch", future = true)]
    pub async fn upsert_transcript_batch_for_test(
        &self,
        scope: HostAdmissionScope,
        session: &tracedecay_sessions::runtime::SessionRecord,
        messages: &[tracedecay_sessions::runtime::SessionMessageRecord],
        source: &str,
        offset: tracedecay_global_db::ParseOffset,
    ) -> Result<Vec<i64>> {
        let database = self.session_database_for_test(scope)?;
        if !database
            .upsert_transcript_batch(session, messages, source, offset)
            .await
        {
            return Err(TraceDecayError::Database {
                operation: "seed registered transcript batch fixture".to_owned(),
                message: "registered transcript batch write failed".to_owned(),
            });
        }
        let mut store_ids = Vec::with_capacity(messages.len());
        for message in messages {
            let store_id = database
                .lcm_raw_message_store_id(&message.provider, &message.message_id)
                .await
                .map_err(|error| TraceDecayError::Database {
                    operation: "read registered transcript fixture store id".to_owned(),
                    message: error.to_string(),
                })?
                .ok_or_else(|| TraceDecayError::Database {
                    operation: "read registered transcript fixture store id".to_owned(),
                    message: format!(
                        "LCM raw message {}/{} is unavailable after insert",
                        message.provider, message.message_id
                    ),
                })?;
            store_ids.push(store_id);
        }
        Ok(store_ids)
    }

    #[doc(hidden)]
    #[hotpath::measure(label = "daemon.host_admission.count_transcript_rows", future = true)]
    pub async fn transcript_store_counts_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        session_id: &str,
        transcript_path: &Path,
    ) -> Result<(i64, i64, i64, i64, i64, i64, i64)> {
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await?;
        let mut rows = snapshot
            .query(
                "SELECT
                    (SELECT COUNT(*) FROM sessions
                     WHERE provider = ?1 AND session_id = ?2),
                    (SELECT COUNT(*) FROM session_messages
                     WHERE provider = ?1 AND session_id = ?2),
                    (SELECT COUNT(*) FROM lcm_raw_messages
                     WHERE provider = ?1 AND session_id = ?2),
                    (SELECT COUNT(*) FROM lcm_raw_messages_fts
                     JOIN lcm_raw_messages raw
                       ON raw.store_id = lcm_raw_messages_fts.rowid
                     WHERE raw.provider = ?1 AND raw.session_id = ?2),
                    (SELECT COUNT(*) FROM lcm_raw_messages_fts),
                    (SELECT COUNT(*) FROM lcm_summary_nodes
                     WHERE provider = ?1 AND session_id = ?2),
                    (SELECT COUNT(*) FROM parse_offsets
                     WHERE file_path = ?3)",
                tracedecay_runtime_core::db::engine::params![
                    provider,
                    session_id,
                    transcript_path.to_string_lossy().as_ref()
                ],
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| TraceDecayError::Database {
                operation: "read registered transcript store counts".to_owned(),
                message: "count query returned no row".to_owned(),
            })?;
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
        ))
    }

    #[doc(hidden)]
    #[hotpath::skip]
    pub async fn project_session_message_count_for_test(&self) -> Result<i64> {
        self.session_database_for_test(HostAdmissionScope::Project)?
            .session_message_count()
            .await
            .map_err(|message| TraceDecayError::Database {
                operation: "count registered project session messages".to_owned(),
                message,
            })
    }

    #[doc(hidden)]
    #[hotpath::skip]
    pub async fn project_lcm_raw_message_exists_for_test(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Result<bool> {
        Ok(self
            .project_database_for_test()?
            .lcm_raw_message_store_id(provider, message_id)
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "check registered project LCM raw message".to_owned(),
                message: error.to_string(),
            })?
            .is_some())
    }

    #[doc(hidden)]
    #[hotpath::skip]
    pub async fn git_sessions_for_for_test(
        &self,
        query: &tracedecay_sessions::runtime::git_correlation::SessionsForQuery,
        relation: tracedecay_sessions::runtime::git_correlation::CommitRelationFilter,
    ) -> std::result::Result<
        Vec<tracedecay_sessions::runtime::git_correlation::SessionGitCorrelationHit>,
        tracedecay_sessions::runtime::git_correlation::GitCorrelationError,
    > {
        let database = self.project_database_for_test().map_err(|error| {
            tracedecay_sessions::runtime::git_correlation::GitCorrelationError::Db(
                error.to_string(),
            )
        })?;
        tracedecay_global_db::GlobalDbGitCorrelationStore::new(database)
            .sessions_for_with_relation(query, relation)
            .await
    }

    /// Fails the calling test loudly: a fixture whose accounting write is
    /// dropped would assert against totals that were never stored.
    #[doc(hidden)]
    #[hotpath::skip]
    pub async fn upsert(&self, project_path: &Path, tokens_saved: u64) {
        self.profile_database
            .try_upsert_project_tokens(project_path, tokens_saved)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "could not upsert project tokens for '{}': {error}",
                    project_path.display()
                )
            });
    }

    #[doc(hidden)]
    #[hotpath::skip]
    pub async fn upsert_code_project(
        &self,
        project_id: &str,
        project_root: &Path,
        git_common_dir: Option<&Path>,
        git_remote_url: Option<&str>,
        default_branch: Option<&str>,
    ) -> Result<tracedecay_global_db::CodeProjectRecord> {
        self.profile_database
            .upsert_code_project(
                project_id,
                project_root,
                git_common_dir,
                git_remote_url,
                default_branch,
            )
            .await
    }

    #[doc(hidden)]
    #[hotpath::skip]
    pub async fn upsert_project_alias(
        &self,
        alias_path: &Path,
        project_id: &str,
    ) -> Result<tracedecay_global_db::ProjectAliasRecord> {
        self.profile_database
            .upsert_project_alias(alias_path, project_id)
            .await
    }

    #[doc(hidden)]
    #[hotpath::skip]
    pub async fn upsert_store_instance(
        &self,
        upsert: tracedecay_global_db::StoreInstanceUpsert,
    ) -> Result<tracedecay_global_db::StoreInstanceRecord> {
        self.profile_database.upsert_store_instance(upsert).await
    }

    #[doc(hidden)]
    #[hotpath::skip]
    pub async fn append_profile_analytics_event_for_test(
        &self,
        event: &tracedecay_global_db::AnalyticsEventInsert,
    ) -> Result<i64> {
        self.profile_database
            .append_analytics_event(event)
            .await
            .map_err(|message| TraceDecayError::Database {
                operation: "append registered analytics event".to_owned(),
                message,
            })
    }

    #[doc(hidden)]
    #[hotpath::skip]
    pub async fn query_profile_analytics_events_for_test(
        &self,
        query: &tracedecay_global_db::AnalyticsEventQuery,
    ) -> Result<Vec<tracedecay_global_db::AnalyticsEventRecord>> {
        self.profile_database
            .query_analytics_events(query)
            .await
            .map_err(|message| TraceDecayError::Database {
                operation: "query registered profile analytics events".to_owned(),
                message,
            })
    }

    /// Returns the canonical profile/global authority used for analytics and
    /// other profile-scoped durable data in tests. The profile session
    /// authority returned by [`Self::registered_database`] is intentionally a
    /// different shard; callers that correlate analytics with sessions must
    /// bind both explicitly, just as production composition does.
    #[cfg(any(test, feature = "test-helpers"))]
    #[doc(hidden)]
    pub fn profile_database_for_test(&self) -> &RegisteredGlobalDb {
        self.profile_database.as_ref()
    }

    #[doc(hidden)]
    pub fn mcp_session_authorities(&self) -> crate::mcp::tools::SessionAuthorities<'_> {
        crate::mcp::tools::SessionAuthorities::new(
            self.project_registered.as_ref(),
            Some(&self.profile_registered),
        )
        .with_registered_databases(
            self.project_registered.as_ref(),
            Some(&self.profile_registered),
        )
    }

    #[cfg(test)]
    pub(crate) fn unregistered_mcp_session_authorities_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> crate::mcp::tools::SessionAuthorities<'_> {
        match scope {
            HostAdmissionScope::Project => {
                crate::mcp::tools::SessionAuthorities::new(self.project_registered.as_ref(), None)
            }
            HostAdmissionScope::Profile => {
                crate::mcp::tools::SessionAuthorities::new(None, Some(&self.profile_registered))
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn host_admission_broker_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> Result<SharedHostAdmissionBroker> {
        let database = self.session_database_for_test(scope)?;
        let (runtime, _) = HostAdmissionRuntime::open_for_database(database.db_path())?;
        Ok(Arc::new(HostAdmissionBroker::new(runtime)))
    }

    #[cfg(test)]
    pub(crate) fn into_mcp_server_context_for_test(
        self,
        cg: TraceDecay,
        scope_prefix: Option<String>,
    ) -> Result<crate::mcp::server::McpServerConstructionContext> {
        Arc::new(self).mcp_server_context_for_test(cg, scope_prefix)
    }

    #[cfg(any(test, feature = "test-transport"))]
    pub(crate) fn mcp_server_context_for_test(
        self: Arc<Self>,
        cg: TraceDecay,
        scope_prefix: Option<String>,
    ) -> Result<crate::mcp::server::McpServerConstructionContext> {
        let profile_root = self.profile_root.clone();
        let project_sessions =
            self.project_registered
                .clone()
                .ok_or_else(|| TraceDecayError::Database {
                    operation: "bind MCP test project sessions".to_owned(),
                    message: "registered ProjectSessions mount is unavailable".to_owned(),
                })?;
        let profile_database = self.profile_database.clone();
        let profile_sessions = self.profile_registered.clone();
        let profile_identity =
            tracedecay_daemon_identity::profile_identity::load_or_create(&profile_root)?;
        let mut context =
            crate::mcp::server::McpServerConstructionContext::direct(cg, scope_prefix)
                .with_direct_databases(
                    Some(profile_database.clone()),
                    Some(profile_database),
                    Some(project_sessions),
                    Some(profile_sessions),
                );
        context.profile_root = Some(profile_root);
        context.profile_identity = Some(std::sync::Arc::new(profile_identity));
        context.host_admission_test_runtime = Some(self);
        Ok(context)
    }

    #[cfg(test)]
    #[hotpath::skip]
    pub(crate) async fn ensure_runtime_configuration_for_test(
        &self,
        project_root: &Path,
        layout: &tracedecay_runtime_core::storage::StoreLayout,
    ) -> Result<crate::config::PinnedRuntimeConfiguration> {
        crate::config::ensure_runtime_configuration_for_registered_database(
            project_root,
            layout,
            self.project_configuration_database_for_test()?,
        )
        .await
    }

    #[cfg(test)]
    #[hotpath::skip]
    pub(crate) async fn resolve_runtime_configuration_for_test(
        &self,
        project_root: &Path,
        layout: &tracedecay_runtime_core::storage::StoreLayout,
    ) -> Result<crate::config::PinnedRuntimeConfiguration> {
        crate::config::resolve_runtime_configuration_for_registered_database(
            project_root,
            layout,
            self.project_configuration_database_for_test()?,
        )
        .await
    }

    #[cfg(test)]
    #[hotpath::skip]
    pub(crate) async fn load_runtime_configuration_read_only_for_test(
        &self,
        project_root: &Path,
        layout: &tracedecay_runtime_core::storage::StoreLayout,
    ) -> Result<crate::config::PinnedRuntimeConfiguration> {
        crate::config::load_runtime_configuration_for_registered_database_read_only(
            project_root,
            layout,
            self.project_configuration_database_for_test()?,
        )
        .await
    }

    #[cfg(test)]
    fn project_configuration_database_for_test(&self) -> Result<RegisteredGlobalDbLeaseV1> {
        self.project_registered
            .clone()
            .ok_or_else(|| TraceDecayError::Database {
                operation: "bind configuration test project sessions".to_owned(),
                message: "registered ProjectSessions mount is unavailable".to_owned(),
            })
    }

    fn project_database_for_test(&self) -> Result<&RegisteredGlobalDb> {
        self.project_registered
            .as_deref()
            .ok_or_else(|| TraceDecayError::Database {
                operation: "bind registered project session test runtime".to_owned(),
                message: "registered ProjectSessions mount is unavailable".to_owned(),
            })
    }

    fn session_database_for_test(&self, scope: HostAdmissionScope) -> Result<&RegisteredGlobalDb> {
        match scope {
            HostAdmissionScope::Project => self.project_database_for_test(),
            HostAdmissionScope::Profile => Ok(self.profile_registered.as_ref()),
        }
    }

    pub fn facade(&self) -> HostAdmissionFacade<'_> {
        match (self.project_id.as_ref(), self.project_registered.as_ref()) {
            (Some(project_id), Some(project_registered)) => HostAdmissionFacade::new(
                HostAdmissionAuthorities::registered_for_project(
                    self.brain_id.clone(),
                    self.profile_id.clone(),
                    project_id.clone(),
                    project_registered,
                )
                .with_profile_registered(self.profile_id.clone(), self.profile_registered.as_ref()),
            ),
            _ => HostAdmissionFacade::new(HostAdmissionAuthorities::for_profile(
                self.brain_id.clone(),
                self.profile_id.clone(),
                self.profile_registered.as_ref(),
            )),
        }
    }

    /// Initializes a project graph through this retained registered runtime.
    #[doc(hidden)]
    #[hotpath::measure(label = "daemon.host_admission.init_project_graph", future = true)]
    pub async fn initialize_project_graph_for_test(
        &self,
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<TraceDecay> {
        let project_id = self
            .project_id
            .as_ref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "project graph initialization requires project-scoped test authority"
                    .to_owned(),
            })?;
        let project_database =
            self.project_registered
                .clone()
                .ok_or_else(|| TraceDecayError::Config {
                    message: "project graph initialization requires a registered project session"
                        .to_owned(),
                })?;
        let store_layout = TraceDecay::resolve_registered_configuration_layout(
            project_root,
            &open_options,
            self.profile_database.as_ref(),
        )
        .await?;
        if store_layout.identity.project_id.as_deref() != Some(project_id.as_str()) {
            return Err(TraceDecayError::Config {
                message: "project graph identity differs from registered test authority".to_owned(),
            });
        }
        TraceDecay::init_with_registered_configuration(
            project_root,
            open_options,
            store_layout,
            project_database,
            self.profile_database.clone(),
            Arc::clone(&self.session_registry),
        )
        .await
    }

    /// Reopens an existing project graph through this retained runtime.
    #[doc(hidden)]
    #[hotpath::measure(label = "daemon.host_admission.open_project_graph", future = true)]
    pub async fn open_project_graph_for_test(
        &self,
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<TraceDecay> {
        let (store_layout, project_database) = self
            .registered_project_open_inputs(project_root, &open_options)
            .await?;
        TraceDecay::open_with_registered_configuration(
            project_root,
            open_options,
            store_layout,
            project_database,
            self.profile_database.clone(),
            Arc::clone(&self.session_registry),
        )
        .await
    }

    /// Opens one tracked branch through this retained registered runtime.
    #[doc(hidden)]
    #[hotpath::measure(label = "daemon.host_admission.open_project_branch", future = true)]
    pub async fn open_project_branch_for_test(
        &self,
        project_root: &Path,
        branch_name: &str,
        open_options: TraceDecayOpenOptions,
    ) -> Result<TraceDecay> {
        let project_id = self
            .project_id
            .as_ref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "project branch open requires project-scoped test authority".to_owned(),
            })?;
        let project_database =
            self.project_registered
                .clone()
                .ok_or_else(|| TraceDecayError::Config {
                    message: "project branch open requires a registered project session".to_owned(),
                })?;
        let store_layout = TraceDecay::resolve_registered_configuration_layout(
            project_root,
            &open_options,
            self.profile_database.as_ref(),
        )
        .await?;
        if store_layout.identity.project_id.as_deref() != Some(project_id.as_str()) {
            return Err(TraceDecayError::Config {
                message: "project branch identity differs from registered test authority"
                    .to_owned(),
            });
        }
        TraceDecay::open_branch_with_registered_configuration(
            project_root,
            branch_name,
            open_options,
            store_layout,
            project_database,
            self.profile_database.clone(),
            Arc::clone(&self.session_registry),
        )
        .await
    }

    /// Reopens an existing graph read-only without inferring authority.
    #[doc(hidden)]
    #[hotpath::measure(label = "daemon.host_admission.open_graph_read_only", future = true)]
    pub async fn open_project_graph_read_only_for_test(
        &self,
        project_root: &Path,
        open_options: TraceDecayOpenOptions,
    ) -> Result<TraceDecay> {
        let (store_layout, project_database) = self
            .registered_project_open_inputs(project_root, &open_options)
            .await?;
        TraceDecay::open_read_only_with_registered_configuration(
            project_root,
            open_options,
            store_layout,
            project_database,
            self.profile_database.clone(),
            Arc::clone(&self.session_registry),
        )
        .await
    }

    #[hotpath::skip]
    async fn registered_project_open_inputs(
        &self,
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Result<(
        tracedecay_runtime_core::storage::StoreLayout,
        RegisteredGlobalDbLeaseV1,
    )> {
        let project_id = self
            .project_id
            .as_ref()
            .ok_or_else(|| TraceDecayError::Config {
                message: "project graph open requires project-scoped test authority".to_owned(),
            })?;
        let project_database =
            self.project_registered
                .clone()
                .ok_or_else(|| TraceDecayError::Config {
                    message: "project graph open requires a registered project session".to_owned(),
                })?;
        let store_layout = TraceDecay::resolve_registered_configuration_layout(
            project_root,
            open_options,
            self.profile_database.as_ref(),
        )
        .await?;
        if store_layout.identity.project_id.as_deref() != Some(project_id.as_str()) {
            return Err(TraceDecayError::Config {
                message: "project graph identity differs from registered test authority".to_owned(),
            });
        }
        Ok((store_layout, project_database))
    }
}

fn canonical_session_domain_sha256(path: &Path) -> Result<[u8; 32]> {
    tracedecay_rusqlite_runtime::canonical_session_domain_content_sha256(path).map_err(|error| {
        TraceDecayError::Database {
            operation: error.operation.to_owned(),
            message: error.message,
        }
    })
}

const fn registered_authority_unavailable_outcome() -> HostAdmissionOutcome {
    HostAdmissionOutcome {
        status: HostAdmissionStatus::Unavailable,
        retryable: true,
        reason_code: Some("registered_authority_unavailable"),
        recovery: None,
        storage_cause: None,
    }
}

/// A root test runtime statically known to carry project authority.
#[doc(hidden)]
#[derive(Clone)]
pub struct ProjectScopedTestRuntimeV1(Arc<HostAdmissionTestRuntimeV1>);

impl ProjectScopedTestRuntimeV1 {
    #[doc(hidden)]
    pub fn new(runtime: impl Into<Arc<HostAdmissionTestRuntimeV1>>) -> Result<Self> {
        let runtime = runtime.into();
        if runtime.project_id.is_none() || runtime.project_registered.is_none() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "test runtime for profile '{}' is profile-scoped; project-scoped authority \
                     requires HostAdmissionTestRuntimeV1::project",
                    runtime.profile_root.display()
                ),
            });
        }
        Ok(Self(runtime))
    }

    #[doc(hidden)]
    pub fn into_runtime(self) -> Arc<HostAdmissionTestRuntimeV1> {
        self.0
    }
}

impl std::ops::Deref for ProjectScopedTestRuntimeV1 {
    type Target = HostAdmissionTestRuntimeV1;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[hotpath::measure(label = "daemon.host_admission.validate_authorities")]
fn validate_registered_authorities(
    brain_id: &BrainId,
    profile_id: &UserProfileId,
    project_id: Option<&ProjectId>,
    profile_database: &RegisteredGlobalDb,
    profile_registered: &RegisteredGlobalDb,
    project_registered: Option<&RegisteredGlobalDb>,
) -> Result<()> {
    let profile_shard = &profile_database.binding().shard_id;
    let profile_sessions_shard = &profile_registered.binding().shard_id;
    let profile_identity_matches = &profile_shard.brain_id == brain_id
        && &profile_shard.profile_id == profile_id
        && profile_shard.scope == StoreShardScopeV1::Profile;
    let profile_sessions_identity_matches = &profile_sessions_shard.brain_id == brain_id
        && &profile_sessions_shard.profile_id == profile_id
        && profile_sessions_shard.scope == StoreShardScopeV1::ProfileSessions;
    let project_identity_matches = match (project_id, project_registered) {
        (None, None) => true,
        (Some(project_id), Some(project_registered)) => {
            let shard = &project_registered.binding().shard_id;
            &shard.brain_id == brain_id
                && &shard.profile_id == profile_id
                && matches!(
                    &shard.scope,
                    StoreShardScopeV1::ProjectSessions {
                        project_id: shard_project_id
                    } if shard_project_id == project_id
                )
        }
        _ => false,
    };
    if profile_identity_matches && profile_sessions_identity_matches && project_identity_matches {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: "registered test databases differ from the retained profile/project authority"
            .to_owned(),
    })
}

#[cfg(unix)]
fn prepare_host_admission_test_profile_root(profile_root: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(profile_root).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to create host-admission test profile '{}': {error}",
            profile_root.display()
        ),
    })?;
    let metadata =
        std::fs::symlink_metadata(profile_root).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to inspect host-admission test profile '{}': {error}",
                profile_root.display()
            ),
        })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(TraceDecayError::Config {
            message: format!(
                "host-admission test profile '{}' must be a regular directory",
                profile_root.display()
            ),
        });
    }
    std::fs::set_permissions(profile_root, std::fs::Permissions::from_mode(0o700)).map_err(
        |error| TraceDecayError::Config {
            message: format!(
                "failed to restrict host-admission test profile '{}': {error}",
                profile_root.display()
            ),
        },
    )
}

#[cfg(not(unix))]
fn prepare_host_admission_test_profile_root(profile_root: &Path) -> Result<()> {
    std::fs::create_dir_all(profile_root).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to create host-admission test profile '{}': {error}",
            profile_root.display()
        ),
    })
}

/// Pins a fixture project's identity in the sanctioned `.git/` repository
/// identity marker (initializing a real git repository first when the fixture
/// root has none). Nothing is written into the working tree and the registry
/// fixture state stays exactly what each test arranged.
fn prepare_host_admission_test_project_root(
    project_root: &Path,
    project_id: &ProjectId,
) -> Result<()> {
    std::fs::create_dir_all(project_root).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to create host-admission test project '{}': {error}",
            project_root.display()
        ),
    })?;
    if tracedecay_runtime_core::worktree::git_common_dir(project_root).is_none() {
        let git = tracedecay_runtime_core::git::try_git_program().map_err(|error| {
            TraceDecayError::Config {
                message: format!(
                    "git executable unavailable for host-admission test project '{}': {error}",
                    project_root.display()
                ),
            }
        })?;
        let status = std::process::Command::new(git)
            .args(["init", "--quiet"])
            .current_dir(project_root)
            .status()
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to run git init in host-admission test project '{}': {error}",
                    project_root.display()
                ),
            })?;
        if !status.success() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "git init failed in host-admission test project '{}': {status}",
                    project_root.display()
                ),
            });
        }
    }
    if tracedecay_runtime_core::storage::read_repository_identity_marker(project_root)?.is_none() {
        tracedecay_runtime_core::storage::write_repository_identity_marker(
            project_root,
            project_id.as_str(),
        )?;
    }
    Ok(())
}
