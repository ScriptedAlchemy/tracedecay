use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tempfile::TempDir;

use crate::{RegisteredGlobalDb, RegisteredGlobalDbLeaseV1, RegisteredGlobalDbOwnerV1};
use tracedecay_runtime_core::db::DaemonDatabaseScope;
#[cfg(test)]
use tracedecay_runtime_core::db::engine::{Executor, IntoParams, QueryExecutor, Rows};

static TEST_RUNTIME_NONCE: AtomicU64 = AtomicU64::new(1);

pub struct RegisteredGlobalDbHarness {
    pub registered: RegisteredGlobalDbLeaseV1,
    /// Retains the shared database runtime slot so concurrent remounts
    /// singleflight to the one runtime the daemon would hold in production.
    _database: RegisteredGlobalDbOwnerV1,
    _directory: TempDir,
    _scope: Option<DaemonDatabaseScope>,
}

#[cfg(test)]
pub(crate) struct RegisteredGlobalDbRetirementHarnessV1 {
    registered: RegisteredGlobalDbLeaseV1,
    database: RegisteredGlobalDbOwnerV1,
    retirement: tracedecay_runtime_core::db::RegisteredTestRuntimeRetirementControlV1,
    directory: TempDir,
    scope: DaemonDatabaseScope,
}

#[cfg(test)]
impl RegisteredGlobalDbRetirementHarnessV1 {
    pub(crate) async fn open(label: &str) -> Self {
        crate::register_test_schema_installer();
        let directory = tempfile::tempdir().expect("temporary registered global database");
        let profile_root = directory.path().join("profile");
        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(&profile_root)
            .expect("create registered global-db profile root");
        let nonce = TEST_RUNTIME_NONCE.fetch_add(1, Ordering::Relaxed);
        let scope =
            tracedecay_runtime_core::db::enter_daemon_database_scope(&profile_root, nonce, label)
                .expect("daemon database scope");
        let path = tracedecay_sessions::runtime::user_sessions_db_path(&profile_root);
        let source_authority = tracedecay_runtime_core::db::DatabaseAuthority::acquire_test(
            &path,
            "open registered global-db retirement test runtime",
        )
        .expect("registered retirement test authority");
        let fixture = tracedecay_runtime_core::db::Database::publish_registered_test_runtime_with_retirement_control(
            &path,
            &source_authority,
            tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
            tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
        .expect("publish registered retirement test runtime");
        let (database, _runtime, retirement) = fixture.into_parts();
        let database = RegisteredGlobalDbOwnerV1::admit_and_attach(database)
            .await
            .expect("attach registered retirement test runtime");
        let registered = database
            .issue_lease()
            .expect("issue registered retirement test client");
        Self {
            registered,
            database,
            retirement,
            directory,
            scope,
        }
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        RegisteredGlobalDbLeaseV1,
        RegisteredGlobalDbOwnerV1,
        tracedecay_runtime_core::db::RegisteredTestRuntimeRetirementControlV1,
        TempDir,
        DaemonDatabaseScope,
    ) {
        let Self {
            registered,
            database,
            retirement,
            directory,
            scope,
        } = self;
        (registered, database, retirement, directory, scope)
    }
}

/// Which write authority a registered test fixture attaches to the database.
///
/// `Fixture` keeps the unconditional Test-role escape hatch for fixtures whose
/// daemon scope outlives every write. `DaemonScoped` acquires real daemon-role
/// authority under the fixture's entered daemon database scope, so dropping
/// that scope revokes retained writers exactly as a lost daemon election does
/// in production.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Clone, Copy)]
enum RegisteredTestWriteAuthority {
    Fixture,
    DaemonScoped,
}

/// Standalone registered-database fixture for downstream use-case tests.
///
/// This owns only storage registration. Composition-root daemon, transport,
/// migration, and host-admission adapters deliberately stay outside it.
#[cfg(any(test, feature = "test-helpers"))]
pub struct RegisteredGlobalDbTestRuntime {
    profile_registered: RegisteredGlobalDbLeaseV1,
    _profile_owner: RegisteredGlobalDbOwnerV1,
    project_registered: Option<RegisteredGlobalDbLeaseV1>,
    _project_owner: Option<RegisteredGlobalDbOwnerV1>,
    graph_registry: tracedecay_graph_db::GraphDbRegistry,
    _scope: DaemonDatabaseScope,
}

#[cfg(any(test, feature = "test-helpers"))]
impl RegisteredGlobalDbTestRuntime {
    pub async fn profile(
        profile_root: impl AsRef<std::path::Path>,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        Self::open(profile_root.as_ref(), None).await
    }

    pub async fn project(
        profile_root: impl AsRef<std::path::Path>,
        project_root: impl AsRef<std::path::Path>,
        project_id: tracedecay_domain::ProjectId,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        let project_root = project_root.as_ref();
        std::fs::create_dir_all(project_root)?;
        Self::open(profile_root.as_ref(), Some((project_root, project_id))).await
    }

    async fn open(
        profile_root: &std::path::Path,
        project: Option<(&std::path::Path, tracedecay_domain::ProjectId)>,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        crate::register_test_schema_installer();
        // A profile root is a profile-identity root in production and must be
        // mode 0700; creating it with the ambient umask (0775 under umask 0002)
        // makes identity validation fail, so use the private-store helper
        // production itself uses.
        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(profile_root)?;
        let nonce = TEST_RUNTIME_NONCE.fetch_add(1, Ordering::Relaxed);
        let scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            profile_root,
            nonce,
            "registered-global-db-test-runtime",
        )?;
        let graph_registry =
            tracedecay_graph_db::GraphDbRegistry::new(tracedecay_graph_db::GraphDbRegistryConfig {
                max_open: 2,
            })
            .map_err(|error| {
                tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "create registered global-db test graph registry".to_owned(),
                    message: error.to_string(),
                }
            })?;
        let (profile_registered, profile_owner) = open_registered_test_database_with(
            &tracedecay_sessions::runtime::user_sessions_db_path(profile_root),
            tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProfileSessions,
            RegisteredTestWriteAuthority::DaemonScoped,
        )
        .await?;
        bind_test_session_relation_graph_with_registry(&profile_registered, &graph_registry)?;
        let (project_registered, project_owner) = match project {
            Some((project_root, project_id)) => {
                let marker = tracedecay_runtime_core::storage::EnrollmentMarker {
                    project_id: project_id.to_string(),
                    storage_mode: tracedecay_runtime_core::storage::StorageMode::ProfileSharded,
                };
                let layout = tracedecay_runtime_core::storage::profile_sharded_layout(
                    project_root,
                    profile_root,
                    &marker,
                )?;
                let (registered, owner) = open_registered_test_database_with(
                    &layout.sessions_db_path,
                    tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProjectSessions {
                        project_id,
                    },
                    RegisteredTestWriteAuthority::DaemonScoped,
                )
                .await?;
                bind_test_session_relation_graph_with_registry(&registered, &graph_registry)?;
                (Some(registered), Some(owner))
            }
            None => (None, None),
        };
        Ok(Self {
            profile_registered,
            _profile_owner: profile_owner,
            project_registered,
            _project_owner: project_owner,
            graph_registry,
            _scope: scope,
        })
    }

    pub fn profile_database(&self) -> &RegisteredGlobalDb {
        self.profile_registered.as_ref()
    }

    pub fn profile_database_arc(&self) -> RegisteredGlobalDbLeaseV1 {
        self.profile_registered.clone()
    }

    pub async fn remount_profile_database_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<RegisteredGlobalDbLeaseV1> {
        open_registered_test_database(
            self.profile_registered.db_path(),
            tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProfileSessions,
        )
        .await
    }

    pub async fn reopen_profile_database_for_test(
        self,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        let Self {
            profile_registered,
            _profile_owner,
            project_registered,
            _project_owner,
            graph_registry,
            _scope,
        } = self;
        if project_registered.is_some() {
            return Err(tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "reopen registered profile test database".to_owned(),
                message: "profile reopen fixture cannot retain a project sessions shard".to_owned(),
            });
        }
        let path = profile_registered.db_path().to_path_buf();
        let (graph_binding, graph_locator) =
            profile_registered
                .session_relation_graph_identity()
                .map(|(binding, locator)| (binding.clone(), locator.clone()))?;
        drop(profile_registered);
        drop(project_registered);
        drop(_profile_owner);
        drop(_project_owner);
        graph_registry
            .close_retained(&graph_binding, &graph_locator)
            .map_err(
                |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "close registered global-db test graph before reopen".to_owned(),
                    message: error.to_string(),
                },
            )?;
        drop(graph_registry);

        let (profile_registered, profile_owner) = open_registered_test_database_with(
            &path,
            tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProfileSessions,
            RegisteredTestWriteAuthority::DaemonScoped,
        )
        .await?;
        let graph_registry =
            tracedecay_graph_db::GraphDbRegistry::new(tracedecay_graph_db::GraphDbRegistryConfig {
                max_open: 1,
            })
            .map_err(|error| {
                tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "recreate registered global-db test graph registry".to_owned(),
                    message: error.to_string(),
                }
            })?;
        bind_test_session_relation_graph_with_registry(&profile_registered, &graph_registry)?;
        Ok(Self {
            profile_registered,
            _profile_owner: profile_owner,
            project_registered: None,
            _project_owner: None,
            graph_registry,
            _scope,
        })
    }

    pub fn project_database(&self) -> tracedecay_runtime_core::errors::Result<&RegisteredGlobalDb> {
        self.project_registered.as_deref().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "bind registered project test database".to_owned(),
                message: "registered project database is unavailable".to_owned(),
            }
        })
    }

    pub fn project_database_arc(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<RegisteredGlobalDbLeaseV1> {
        self.project_registered.clone().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "bind registered project test database".to_owned(),
                message: "registered project database is unavailable".to_owned(),
            }
        })
    }
}

impl RegisteredGlobalDbHarness {
    pub async fn open(label: &str) -> Self {
        let harness = Self::open_without_relation_graph(label).await;
        bind_test_session_relation_graph(&harness.registered)
            .expect("bind registered profile-sessions relation graph");
        harness
    }

    /// Opens the registered store without binding the session relation graph,
    /// staging the unbound state doctor health must report as partial.
    pub async fn open_without_relation_graph(label: &str) -> Self {
        crate::register_test_schema_installer();
        let directory = tempfile::tempdir().expect("temporary registered global database");
        let profile_root = directory.path().join("profile");
        // The profile root must exist on disk before it is canonicalized into
        // a daemon scope key. `canonical_profile_root` falls back to the raw,
        // pre-canonicalization path when the directory is missing, but by the
        // time `DatabaseIdentity::for_path` resolves the opened database the
        // directory has been created and canonicalizes to a different path on
        // any platform where the temp root involves a symlink or an extended
        // path prefix (macOS `/var` -> `/private/var`, Windows `\\?\`). That
        // mismatch makes the daemon-scope lookup miss and the authority
        // acquisition below fail closed. Creating the directory first keeps
        // both canonicalizations identical, matching the sibling runtimes
        // (`RegisteredGlobalDbTestRuntime::open`, `HostAdmissionTestRuntimeV1::open`)
        // that already do this.
        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(&profile_root)
            .expect("create registered global-db profile root");
        let nonce = TEST_RUNTIME_NONCE.fetch_add(1, Ordering::Relaxed);
        let scope =
            tracedecay_runtime_core::db::enter_daemon_database_scope(&profile_root, nonce, label)
                .expect("daemon database scope");
        let (registered, database) = open_registered_test_database_with(
            &tracedecay_sessions::runtime::user_sessions_db_path(&profile_root),
            tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProfileSessions,
            RegisteredTestWriteAuthority::DaemonScoped,
        )
        .await
        .expect("open registered profile-sessions runtime");
        Self {
            registered,
            _database: database,
            _directory: directory,
            _scope: Some(scope),
        }
    }

    #[cfg(test)]
    pub(super) async fn remount_profile_database_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<RegisteredGlobalDbLeaseV1> {
        Ok(open_registered_test_database_with(
            self.registered.db_path(),
            tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProfileSessions,
            RegisteredTestWriteAuthority::DaemonScoped,
        )
        .await?
        .0)
    }

    #[cfg(test)]
    pub(super) fn storage_root(&self) -> &std::path::Path {
        self.registered
            .db_path()
            .parent()
            .expect("registered database storage root")
    }

    pub async fn mount(&self) -> RegisteredGlobalDbLeaseV1 {
        open_registered_test_database_with(
            self.registered.db_path(),
            tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProfileSessions,
            RegisteredTestWriteAuthority::DaemonScoped,
        )
        .await
        .expect("remount registered profile-sessions runtime")
        .0
    }

    pub async fn restart(self) -> Self {
        let Self {
            registered,
            _database,
            _directory,
            _scope,
        } = self;
        let path = registered.db_path().to_path_buf();
        drop(registered);
        drop(_database);
        let (registered, database) = open_registered_test_database_with(
            &path,
            tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProfileSessions,
            RegisteredTestWriteAuthority::DaemonScoped,
        )
        .await
        .expect("restart registered profile-sessions runtime");
        bind_test_session_relation_graph(&registered)
            .expect("bind restarted profile-sessions relation graph");
        Self {
            registered,
            _database: database,
            _directory,
            _scope,
        }
    }

    #[cfg(test)]
    pub(crate) fn revoke(&mut self) {
        drop(self._scope.take());
    }
}

#[doc(hidden)]
pub use tracedecay_sessions::admission::HostAdmissionScope;

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionTemporalFixtureCountV1 {
    ProjectionReceipts,
    Occurrences,
    Assertions,
    RefreshReceipts,
    RefreshProgress,
}

/// Test-only registered database fixture retained below the use-case layer.
#[doc(hidden)]
pub struct HostAdmissionTestRuntimeV1 {
    profile_registry: RegisteredGlobalDbLeaseV1,
    _profile_registry_owner: Option<RegisteredGlobalDbOwnerV1>,
    profile_registered: RegisteredGlobalDbLeaseV1,
    _profile_registered_owner: Option<RegisteredGlobalDbOwnerV1>,
    project_registered: Option<RegisteredGlobalDbLeaseV1>,
    _project_registered_owner: Option<RegisteredGlobalDbOwnerV1>,
    _scope: Option<DaemonDatabaseScope>,
}

impl HostAdmissionTestRuntimeV1 {
    /// Wraps databases whose authority is retained by a higher composition
    /// runtime. This constructor grants no authority and owns no runtime scope.
    #[doc(hidden)]
    pub fn from_registered_databases_for_test(
        profile_registry: RegisteredGlobalDbLeaseV1,
        profile_registered: RegisteredGlobalDbLeaseV1,
        project_registered: Option<RegisteredGlobalDbLeaseV1>,
    ) -> Self {
        Self {
            profile_registry,
            _profile_registry_owner: None,
            profile_registered,
            _profile_registered_owner: None,
            project_registered,
            _project_registered_owner: None,
            _scope: None,
        }
    }

    pub fn registered_database(&self, scope: HostAdmissionScope) -> Option<&RegisteredGlobalDb> {
        match scope {
            HostAdmissionScope::Project => self.project_registered.as_deref(),
            HostAdmissionScope::Profile => Some(self.profile_registered.as_ref()),
        }
    }

    pub fn database_path(&self, scope: HostAdmissionScope) -> Option<&std::path::Path> {
        self.registered_database(scope)
            .map(RegisteredGlobalDb::db_path)
    }

    pub fn profile_registry(&self) -> &RegisteredGlobalDb {
        self.profile_registry.as_ref()
    }

    pub fn canonical_project_key(project_path: &std::path::Path) -> String {
        RegisteredGlobalDb::canonical_project_key(project_path)
    }
}

#[cfg(any(test, feature = "test-helpers"))]
impl HostAdmissionTestRuntimeV1 {
    pub async fn profile(
        profile_root: impl AsRef<std::path::Path>,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        Self::open(profile_root.as_ref(), None).await
    }

    pub async fn project(
        profile_root: impl AsRef<std::path::Path>,
        project_root: impl AsRef<std::path::Path>,
        project_id: tracedecay_domain::ProjectId,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        Self::open(
            profile_root.as_ref(),
            Some((project_root.as_ref(), project_id)),
        )
        .await
    }

    async fn open(
        profile_root: &std::path::Path,
        project: Option<(&std::path::Path, tracedecay_domain::ProjectId)>,
    ) -> tracedecay_runtime_core::errors::Result<Self> {
        crate::register_test_schema_installer();
        // See the note in `RegisteredGlobalDbTestRuntimeV1::open`: profile
        // identity roots must be 0700 regardless of the ambient umask.
        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(profile_root)?;
        if let Some((project_root, _)) = project.as_ref() {
            std::fs::create_dir_all(project_root)?;
        }
        let nonce = TEST_RUNTIME_NONCE.fetch_add(1, Ordering::Relaxed);
        let scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            profile_root,
            nonce,
            "global-db-test-runtime",
        )?;
        let (profile_registry, profile_registry_owner) = open_registered_test_database_with(
            &profile_root.join("global.db"),
            tracedecay_runtime_core::db::TestDatabaseRuntimeScope::Profile,
            RegisteredTestWriteAuthority::DaemonScoped,
        )
        .await?;
        let (profile_registered, profile_registered_owner) = open_registered_test_database_with(
            &tracedecay_sessions::runtime::user_sessions_db_path(profile_root),
            tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProfileSessions,
            RegisteredTestWriteAuthority::DaemonScoped,
        )
        .await?;
        bind_test_session_relation_graph(&profile_registered)?;
        let (project_registered, project_registered_owner) = match project {
            Some((project_root, project_id)) => {
                let marker = tracedecay_runtime_core::storage::EnrollmentMarker {
                    project_id: project_id.to_string(),
                    storage_mode: tracedecay_runtime_core::storage::StorageMode::ProfileSharded,
                };
                let layout = tracedecay_runtime_core::storage::profile_sharded_layout(
                    project_root,
                    profile_root,
                    &marker,
                )?;
                let (registered, owner) = open_registered_test_database_with(
                    &layout.sessions_db_path,
                    tracedecay_runtime_core::db::TestDatabaseRuntimeScope::ProjectSessions {
                        project_id,
                    },
                    RegisteredTestWriteAuthority::DaemonScoped,
                )
                .await?;
                bind_test_session_relation_graph(&registered)?;
                (Some(registered), Some(owner))
            }
            None => (None, None),
        };
        Ok(Self {
            profile_registry,
            _profile_registry_owner: Some(profile_registry_owner),
            profile_registered,
            _profile_registered_owner: Some(profile_registered_owner),
            project_registered,
            _project_registered_owner: project_registered_owner,
            _scope: Some(scope),
        })
    }

    fn session_database_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<&RegisteredGlobalDb> {
        self.registered_database(scope).ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "bind registered global-db test runtime".to_owned(),
                message: "requested registered database scope is unavailable".to_owned(),
            }
        })
    }

    #[cfg(test)]
    pub(crate) fn observation_store(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<crate::GlobalDbObservationStore> {
        let database = self.session_database_for_test(scope)?;
        Ok(database.observation_store())
    }

    pub async fn upsert_session_for_test(
        &self,
        scope: HostAdmissionScope,
        session: &tracedecay_sessions::runtime::SessionRecord,
    ) -> tracedecay_runtime_core::errors::Result<bool> {
        Ok(self
            .session_database_for_test(scope)?
            .upsert_session(session)
            .await)
    }

    pub async fn upsert_session_message_for_test(
        &self,
        scope: HostAdmissionScope,
        message: &tracedecay_sessions::runtime::SessionMessageRecord,
    ) -> tracedecay_runtime_core::errors::Result<bool> {
        let database = self.session_database_for_test(scope)?;
        let session = database
            .get_session(&message.provider, &message.session_id)
            .await
            .ok_or_else(
                || tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "seed registered session message fixture".to_owned(),
                    message: format!(
                        "session {}/{} is unavailable",
                        message.provider, message.session_id
                    ),
                },
            )?;
        Ok(database
            .upsert_transcript_batch(
                &session,
                std::slice::from_ref(message),
                &format!(
                    "global-db-test-message:{}:{}",
                    message.provider, message.message_id
                ),
                crate::ParseOffset::default(),
            )
            .await)
    }

    pub async fn session_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        session_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<Option<tracedecay_sessions::runtime::SessionRecord>>
    {
        Ok(self
            .session_database_for_test(scope)?
            .get_session(provider, session_id)
            .await)
    }

    pub async fn transcript_store_counts_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        session_id: &str,
        transcript_path: &std::path::Path,
    ) -> tracedecay_runtime_core::errors::Result<(i64, i64, i64, i64, i64, i64, i64)> {
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
        let row = rows.next().await?.ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "read registered transcript store counts".to_owned(),
                message: "count query returned no row".to_owned(),
            }
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

    pub async fn delete_session_message_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        message_id: &str,
    ) -> tracedecay_runtime_core::errors::Result<u64> {
        let transaction = self
            .session_database_for_test(scope)?
            .begin_write_transaction()
            .await?;
        let deleted = transaction
            .execute(
                "DELETE FROM session_messages WHERE provider = ?1 AND message_id = ?2",
                tracedecay_runtime_core::db::engine::params![provider, message_id],
            )
            .await?;
        transaction.commit().await?;
        Ok(deleted)
    }

    pub async fn project_session_message_count_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<i64> {
        self.session_database_for_test(HostAdmissionScope::Project)?
            .session_message_count()
            .await
            .map_err(
                |message| tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "count registered project session messages".to_owned(),
                    message,
                },
            )
    }

    #[cfg(test)]
    pub(crate) fn session_temporal_store_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<
        crate::session_temporal::GlobalDbSessionTemporalStore<'_>,
    > {
        Ok(crate::session_temporal::GlobalDbSessionTemporalStore::new(
            self.session_database_for_test(scope)?,
        ))
    }

    #[cfg(test)]
    pub(crate) fn project_configuration_control_store_for_test(
        &self,
    ) -> tracedecay_runtime_core::errors::Result<
        crate::configuration::OwnedGlobalDbConfigurationControlStore,
    > {
        let database = self.project_registered.clone().ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "bind configuration control test project sessions".to_owned(),
                message: "registered project database is unavailable".to_owned(),
            }
        })?;
        Ok(
            crate::configuration::OwnedGlobalDbConfigurationControlStore::from_registered_project_runtime_db(
                database,
            ),
        )
    }

    #[cfg(test)]
    pub(crate) async fn session_temporal_fixture_count_for_test(
        &self,
        scope: HostAdmissionScope,
        kind: SessionTemporalFixtureCountV1,
    ) -> tracedecay_runtime_core::errors::Result<i64> {
        let table = match kind {
            SessionTemporalFixtureCountV1::ProjectionReceipts => {
                "session_temporal_projection_receipts"
            }
            SessionTemporalFixtureCountV1::Occurrences => "session_occurrences",
            SessionTemporalFixtureCountV1::Assertions => "session_assertions",
            SessionTemporalFixtureCountV1::RefreshReceipts => "session_refresh_receipts",
            SessionTemporalFixtureCountV1::RefreshProgress => "session_refresh_progress",
        };
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await?;
        let mut rows = snapshot
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await?;
        let row = rows.next().await?.ok_or_else(|| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "read session-temporal fixture count".to_owned(),
                message: "count query returned no row".to_owned(),
            }
        })?;
        row.get(0).map_err(Into::into)
    }

    #[cfg(test)]
    pub(crate) async fn lcm_describe_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmDescribeRequest,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmDescribeResponse,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.profile_registered.lcm_describe(request).await
    }

    #[cfg(test)]
    pub(crate) async fn lcm_expand_for_test(
        &self,
        request: tracedecay_sessions::runtime::lcm::LcmExpandRequest,
    ) -> Result<
        tracedecay_sessions::runtime::lcm::LcmExpandResponse,
        tracedecay_sessions::runtime::lcm::LcmError,
    > {
        self.profile_registered.lcm_expand(request).await
    }

    #[cfg(test)]
    pub(crate) async fn seed_lcm_render_fixture_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::errors::Result<()> {
        let database = self.session_database_for_test(scope)?;
        let session = tracedecay_sessions::runtime::SessionRecord {
            provider: "codex".to_owned(),
            session_id: "session-a".to_owned(),
            project_key: "project-a".to_owned(),
            project_path: "/project-a".to_owned(),
            title: Some("Canonical render fixture".to_owned()),
            started_at: Some(10),
            ended_at: None,
            transcript_path: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        };
        if !database.upsert_session(&session).await {
            return Err(tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "seed canonical lcm render session".to_owned(),
                message: "session upsert failed".to_owned(),
            });
        }

        let external_content = "canonical external payload";
        let external_hash =
            tracedecay_sessions::runtime::lcm::util::sha256_hex(external_content.as_bytes());
        let raw_hash =
            tracedecay_sessions::runtime::lcm::util::sha256_hex(b"canonical raw message");
        let child_summary_hash =
            tracedecay_sessions::runtime::lcm::util::sha256_hex(b"canonical child summary");
        let parent_summary_hash =
            tracedecay_sessions::runtime::lcm::util::sha256_hex(b"canonical parent summary");
        let payload_dir = database
            .db_path()
            .parent()
            .ok_or_else(
                || tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "seed canonical lcm render payload".to_owned(),
                    message: "registered session database has no storage root".to_owned(),
                },
            )?
            .join("lcm-payloads");
        std::fs::create_dir_all(&payload_dir)?;
        std::fs::write(payload_dir.join("payload-a"), external_content)?;

        // Durable raw rows only render once they carry the canonical
        // `ingest_protection.sanitization_receipt` the payload sanitizer binds
        // at ingest; a receipt-free row is refused by `verify_raw_message_receipt`.
        let raw_message_metadata =
            lcm_render_fixture_sanitization_metadata("canonical raw message")?;
        let external_message_metadata = lcm_render_fixture_sanitization_metadata(external_content)?;

        database
            .writer_connection()?
            .execute_batch(&format!(
                "INSERT INTO lcm_external_payloads(
                    payload_ref, provider, session_id, message_id, kind, content_hash,
                    byte_count, char_count, created_at, metadata_json
                 ) VALUES (
                    'payload-a', 'codex', 'session-a', 'message-b', 'tool_result',
                    '{external_hash}', {byte_count}, {char_count}, 12, NULL
                 );
                 INSERT INTO lcm_raw_messages(
                    provider, message_id, session_id, store_id, role, ordinal, timestamp,
                    content, content_hash, storage_kind, payload_ref, snippet_text,
                    index_text, legacy_source, legacy_truncated, metadata_json
                 ) VALUES (
                    'codex', 'message-a', 'session-a', 11, 'assistant', 0, 11,
                    'canonical raw message', '{raw_hash}', 'inline', NULL,
                    'canonical raw message', 'canonical raw message', 0, 0,
                    '{raw_message_metadata}'
                 );
                 INSERT INTO lcm_raw_messages(
                    provider, message_id, session_id, store_id, role, ordinal, timestamp,
                    content, content_hash, storage_kind, payload_ref, snippet_text,
                    index_text, legacy_source, legacy_truncated, metadata_json
                 ) VALUES (
                    'codex', 'message-b', 'session-a', 12, 'tool', 1, 12,
                    NULL, '{external_hash}', 'external', 'payload-a',
                    'canonical external payload', 'canonical external payload', 0, 0,
                    '{external_message_metadata}'
                 );
                 INSERT INTO lcm_summary_nodes(
                    node_id, provider, conversation_id, session_id, depth, summary_text,
                    summary_hash, summary_token_count, source_token_count,
                    source_time_start, source_time_end, expand_hint, metadata_json, created_at
                 ) VALUES (
                    'summary-child', 'codex', 'session-a', 'session-a', 0,
                    'canonical child summary', '{child_summary_hash}', 3, 3,
                    11, 11, NULL, NULL, 13
                 );
                 INSERT INTO lcm_summary_nodes(
                    node_id, provider, conversation_id, session_id, depth, summary_text,
                    summary_hash, summary_token_count, source_token_count,
                    source_time_start, source_time_end, expand_hint, metadata_json, created_at
                 ) VALUES (
                    'summary-parent', 'codex', 'session-a', 'session-a', 1,
                    'canonical parent summary', '{parent_summary_hash}', 3, 6,
                    11, 12, NULL, NULL, 14
                 );
                 INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
                 VALUES ('summary-child', 'raw_message', '11', 0);
                 INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
                 VALUES ('summary-parent', 'summary_node', 'summary-child', 0);
                 INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
                 VALUES ('summary-parent', 'raw_message', '12', 1);",
                byte_count = external_content.len(),
                char_count = external_content.chars().count(),
            ))
            .await?;
        Ok(())
    }
}

/// Builds the canonical `ingest_protection.sanitization_receipt` metadata that
/// the payload sanitizer binds at ingest, so a seeded raw row satisfies
/// `verify_raw_message_receipt`. Returns a SQL-escaped literal ready to embed
/// directly in a fixture `INSERT`.
#[cfg(test)]
fn lcm_render_fixture_sanitization_metadata(
    content: &str,
) -> tracedecay_runtime_core::errors::Result<String> {
    let sanitization = tracedecay_runtime_core::privacy::sanitize_lcm_payload_text(content)
        .map_err(
            |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "seed canonical lcm render sanitization receipt".to_owned(),
                message: format!("payload sanitizer rejected fixture content: {error:?}"),
            },
        )?;
    let receipt = serde_json::to_value(sanitization.receipt()).map_err(|error| {
        tracedecay_runtime_core::errors::TraceDecayError::Database {
            operation: "seed canonical lcm render sanitization receipt".to_owned(),
            message: format!("sanitization receipt encoding failed: {error}"),
        }
    })?;
    let metadata = serde_json::json!({ "ingest_protection": { "sanitization_receipt": receipt } });
    Ok(metadata.to_string().replace('\'', "''"))
}

/// Reconstructs and publishes the final session relation projection for a
/// fixture generation through the durable receipt and graph-apply sequence
/// used by production. Fixtures that seed `session_temporal_generations`
/// directly must call this only after all projection rows are present.
#[cfg(any(test, feature = "test-helpers"))]
pub async fn publish_test_session_relation_projection(
    database: &RegisteredGlobalDb,
    session_id: &str,
    generation: u64,
) -> tracedecay_runtime_core::errors::Result<()> {
    use tracedecay_domain::SessionId;
    use tracedecay_graph_db::NeverCancelled;

    let session_id = SessionId::new(session_id).map_err(|error| {
        tracedecay_runtime_core::errors::TraceDecayError::Database {
            operation: "publish test session relation projection".to_owned(),
            message: error.to_string(),
        }
    })?;
    let snapshot = database.read_snapshot().await?;
    let projection = crate::session_temporal::seed_session_relation_projection(
        database,
        &snapshot,
        &session_id,
        Arc::new(NeverCancelled),
    )
    .await
    .map_err(
        |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
            operation: "reconstruct test session relation projection".to_owned(),
            message: format!("{error:?}"),
        },
    )?;
    drop(snapshot);
    if projection.generation != generation {
        return Err(tracedecay_runtime_core::errors::TraceDecayError::Database {
            operation: "reconstruct test session relation projection".to_owned(),
            message: format!(
                "fixture generation {generation} resolved projection generation {}",
                projection.generation
            ),
        });
    }
    let transaction = database.begin_write_transaction().await?;
    crate::session_temporal::record_relation_receipt(&transaction, &projection, 1)
        .await
        .map_err(
            |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "record test session relation receipt".to_owned(),
                message: format!("{error:?}"),
            },
        )?;
    transaction.commit().await?;
    crate::session_temporal::apply_relation_projection(
        database,
        &projection,
        Arc::new(NeverCancelled),
    )
    .await
    .map(|_| ())
    .map_err(
        |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
            operation: "apply test session relation projection".to_owned(),
            message: format!("{error:?}"),
        },
    )
}

#[cfg(any(test, feature = "test-helpers"))]
fn bind_test_session_relation_graph(
    database: &RegisteredGlobalDb,
) -> tracedecay_runtime_core::errors::Result<()> {
    let registry =
        tracedecay_graph_db::GraphDbRegistry::new(tracedecay_graph_db::GraphDbRegistryConfig {
            max_open: 1,
        })
        .map_err(
            |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "create test session relation graph registry".to_owned(),
                message: error.to_string(),
            },
        )?;
    bind_test_session_relation_graph_with_registry(database, &registry)
}

#[cfg(any(test, feature = "test-helpers"))]
fn bind_test_session_relation_graph_with_registry(
    database: &RegisteredGlobalDb,
    registry: &tracedecay_graph_db::GraphDbRegistry,
) -> tracedecay_runtime_core::errors::Result<()> {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use crate::session_temporal::relations::SessionRelationScope;
    use tracedecay_graph_db::{GraphDbOwnerRegistrationV1, GraphDbRegistration, NeverCancelled};
    use tracedecay_store::{
        RetainedGraphStoreLeaseV1, RetainedGraphStoreOwnerAttachmentV1,
        RetainedGraphStoreOwnerOperationLeaseErrorV1, StoreRuntimeBindingV1, StoreShardScopeV1,
        VerifiedStoreLocatorV1, canonical_store_locator_digest, graph_store_locator_path,
    };

    #[derive(Clone, Debug)]
    struct TestSessionGraphLease {
        binding: StoreRuntimeBindingV1,
        verified_locator: VerifiedStoreLocatorV1,
        canonical_path: PathBuf,
    }

    impl RetainedGraphStoreLeaseV1 for TestSessionGraphLease {
        fn binding(&self) -> &StoreRuntimeBindingV1 {
            &self.binding
        }

        fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
            &self.verified_locator
        }

        fn canonical_path(&self) -> &std::path::Path {
            &self.canonical_path
        }
    }

    impl RetainedGraphStoreOwnerAttachmentV1 for TestSessionGraphLease {
        fn binding(&self) -> &StoreRuntimeBindingV1 {
            &self.binding
        }

        fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
            &self.verified_locator
        }

        fn canonical_path(&self) -> &std::path::Path {
            &self.canonical_path
        }

        fn issue_operation_lease(
            &self,
        ) -> Result<Arc<dyn RetainedGraphStoreLeaseV1>, RetainedGraphStoreOwnerOperationLeaseErrorV1>
        {
            Ok(Arc::new(self.clone()))
        }
    }

    let binding = database.binding();
    let scope = match &binding.shard_id.scope {
        StoreShardScopeV1::ProjectSessions { project_id } => {
            SessionRelationScope::project_sessions(project_id.clone())
        }
        StoreShardScopeV1::ProfileSessions => {
            SessionRelationScope::profile_sessions(binding.shard_id.profile_id.clone())
        }
        _ => {
            return Err(tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "bind test session relation graph".to_owned(),
                message: "registered test database is not a session shard".to_owned(),
            });
        }
    };
    let store_root = database.db_path().parent().ok_or_else(|| {
        tracedecay_runtime_core::errors::TraceDecayError::Database {
            operation: "bind test session relation graph".to_owned(),
            message: "registered session database has no storage root".to_owned(),
        }
    })?;
    let canonical_path =
        graph_store_locator_path(store_root, database.db_path()).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "derive test session relation graph path".to_owned(),
                message: format!("{error:?}"),
            }
        })?;
    let verified_locator = VerifiedStoreLocatorV1::new(
        binding.shard_id.clone(),
        binding.incarnation,
        canonical_store_locator_digest(&canonical_path).map_err(|error| {
            tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "digest test session relation graph path".to_owned(),
                message: format!("{error:?}"),
            }
        })?,
    );
    let lease = TestSessionGraphLease {
        binding: binding.clone(),
        verified_locator: verified_locator.clone(),
        canonical_path,
    };
    let operation = GraphDbRegistration {
        authority_lease: Arc::new(lease.clone()),
        cancellation: Arc::new(NeverCancelled),
        lifecycle_cancellation: Arc::new(NeverCancelled),
        deadline: Instant::now() + Duration::from_secs(30),
    };
    // Owner attachment is the only registry entry-creation path: an ordinary
    // `resolve` can reuse a mounted runtime but never mounts one. Mount through
    // the exact map-owner attachment first (matching the daemon's
    // `open_session_relation_owner`), then resolve the ordinary lease the
    // database retains; the transient attachment may drop afterwards without
    // unmounting the Ready entry.
    let owner_attachment = registry
        .resolve_owner_attachment(GraphDbOwnerRegistrationV1 {
            operation: operation.clone(),
            authority_attachment: Box::new(lease),
        })
        .map_err(
            |error| tracedecay_runtime_core::errors::TraceDecayError::Database {
                operation: "mount test session relation graph owner".to_owned(),
                message: error.to_string(),
            },
        )?;
    let graph = registry.resolve(operation).map_err(|error| {
        tracedecay_runtime_core::errors::TraceDecayError::Database {
            operation: "resolve test session relation graph".to_owned(),
            message: error.to_string(),
        }
    })?;
    drop(owner_attachment);
    database.bind_session_relation_graph(scope, graph, binding.clone(), verified_locator)
}

#[cfg(any(test, feature = "test-helpers"))]
async fn open_registered_test_database(
    path: &std::path::Path,
    scope: tracedecay_runtime_core::db::TestDatabaseRuntimeScope,
) -> tracedecay_runtime_core::errors::Result<RegisteredGlobalDbLeaseV1> {
    Ok(
        open_registered_test_database_with(path, scope, RegisteredTestWriteAuthority::Fixture)
            .await?
            .0,
    )
}

#[cfg(any(test, feature = "test-helpers"))]
async fn open_registered_test_database_with(
    path: &std::path::Path,
    scope: tracedecay_runtime_core::db::TestDatabaseRuntimeScope,
    write_authority: RegisteredTestWriteAuthority,
) -> tracedecay_runtime_core::errors::Result<(RegisteredGlobalDbLeaseV1, RegisteredGlobalDbOwnerV1)>
{
    crate::register_test_schema_installer();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let authority = tracedecay_runtime_core::db::DatabaseAuthority::acquire_test(
        path,
        "open registered global-db test runtime",
    )?;
    // The exact test-runtime resolver refuses `Initialize` for a store that is
    // already on disk (and `Existing` for one that is not). Fixtures reach this
    // helper both ways — a fresh profile root, and a shard some earlier stage of
    // the same test already materialised — so pick the mode from the file.
    let mode = if path.try_exists()? {
        tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Existing
    } else {
        tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize
    };
    let fixture = tracedecay_runtime_core::db::Database::publish_registered_test_runtime_with_retirement_control(
        path, &authority, mode, scope,
    )
    .await?;
    let (database_owner, _runtime, _retirement) = fixture.into_parts();
    let database = RegisteredGlobalDbOwnerV1::admit_and_attach(database_owner).await?;
    // The physical fixture is already opened in the mode requested above.
    // Issuance preserves that capability; neither test branch manufactures a
    // second raw authority after publication.
    let registered = match write_authority {
        RegisteredTestWriteAuthority::Fixture | RegisteredTestWriteAuthority::DaemonScoped => {
            database.issue_lease().map_err(|failure| {
                tracedecay_runtime_core::errors::TraceDecayError::Database {
                    operation: "issue registered global-db test database lease".to_owned(),
                    message: format!("{failure:?}"),
                }
            })?
        }
    };
    Ok((registered, database))
}

/// Opens a registered-store fixture through the same physical publication,
/// sealed schema installation, owner migration, and client issuance route as
/// production admission. Tests may use engine fixtures for post-admission
/// corruption setup, but never install the registered schema directly.
#[cfg(test)]
pub(crate) async fn open_registered_test_database_fixture(
    path: &std::path::Path,
    scope: tracedecay_runtime_core::db::TestDatabaseRuntimeScope,
) -> tracedecay_runtime_core::errors::Result<(RegisteredGlobalDbLeaseV1, RegisteredGlobalDbOwnerV1)>
{
    open_registered_test_database_with(path, scope, RegisteredTestWriteAuthority::Fixture).await
}

/// Canonically published registered fixture that owns both the map owner and
/// one issued client. Its test-only query/write trait adapters retain the
/// guarded client; they cannot expose a raw runtime or connection.
#[cfg(test)]
pub(crate) struct RegisteredGlobalDbTestFixture {
    database: RegisteredGlobalDbLeaseV1,
    _owner: RegisteredGlobalDbOwnerV1,
}

#[cfg(test)]
impl RegisteredGlobalDbTestFixture {
    pub(crate) fn database(&self) -> &RegisteredGlobalDb {
        &self.database
    }
}

#[cfg(test)]
impl QueryExecutor for RegisteredGlobalDbTestFixture {
    async fn query<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<Rows>
    where
        P: IntoParams,
    {
        self.database.read_connection().query(sql, params).await
    }
}

#[cfg(test)]
impl Executor for RegisteredGlobalDbTestFixture {
    async fn execute<P>(
        &self,
        sql: &str,
        params: P,
    ) -> tracedecay_runtime_core::db::engine::Result<u64>
    where
        P: IntoParams,
    {
        self.database
            .writer_connection()
            .map_err(|error| {
                tracedecay_runtime_core::db::engine::Error::invalid_operation(error.to_string())
            })?
            .execute(sql, params)
            .await
    }

    async fn execute_batch(&self, sql: &str) -> tracedecay_runtime_core::db::engine::Result<()> {
        self.database
            .writer_connection()
            .map_err(|error| {
                tracedecay_runtime_core::db::engine::Error::invalid_operation(error.to_string())
            })?
            .execute_batch(sql)
            .await
    }
}

#[cfg(test)]
pub(crate) async fn open_registered_test_fixture(
    path: &std::path::Path,
    scope: tracedecay_runtime_core::db::TestDatabaseRuntimeScope,
) -> tracedecay_runtime_core::errors::Result<RegisteredGlobalDbTestFixture> {
    let (database, owner) = open_registered_test_database_fixture(path, scope).await?;
    Ok(RegisteredGlobalDbTestFixture {
        database,
        _owner: owner,
    })
}

#[cfg(test)]
mod tests {
    use super::RegisteredGlobalDbTestRuntime;
    use tracedecay_store::StoreShardScopeV1;

    #[tokio::test]
    async fn profile_runtime_publishes_a_profile_sessions_shard() {
        let temporary = tempfile::tempdir().expect("temporary profile root");
        let runtime = RegisteredGlobalDbTestRuntime::profile(temporary.path())
            .await
            .expect("registered profile runtime");

        assert!(matches!(
            runtime.profile_database().binding().shard_id.scope,
            StoreShardScopeV1::ProfileSessions
        ));
    }
}
