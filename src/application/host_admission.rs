//! Root composition for host-admission test runtimes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use tracedecay_usecases::host_admission::*;

use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::global_db::RegisteredGlobalDb;
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};
use rusqlite::{Connection as RusqliteConnection, OpenFlags, types::ValueRef};
use sha2::{Digest as _, Sha256};
use tracedecay_domain::{BrainId, ProjectId, UserProfileId};
use tracedecay_runtime_core::db::DaemonDatabaseScope;
use tracedecay_runtime_core::errors::{Result, TraceDecayError};
use tracedecay_store::StoreShardScopeV1;

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
    profile_database: Arc<RegisteredGlobalDb>,
    profile_registered: Arc<RegisteredGlobalDb>,
    project_registered: Option<Arc<RegisteredGlobalDb>>,
    session_registry: Arc<DaemonSessionRuntimeRegistryV1>,
    _database_scope: DaemonDatabaseScope,
}

impl HostAdmissionTestRuntimeV1 {
    #[doc(hidden)]
    pub async fn profile(profile_root: impl AsRef<Path>) -> Result<Self> {
        Self::open(profile_root.as_ref().to_path_buf(), None).await
    }

    #[doc(hidden)]
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
    pub async fn project_scoped(
        profile_root: impl AsRef<Path>,
        project_root: impl AsRef<Path>,
        project_id: ProjectId,
    ) -> Result<ProjectScopedTestRuntimeV1> {
        ProjectScopedTestRuntimeV1::new(
            Self::project(profile_root, project_root, project_id).await?,
        )
    }

    async fn open(profile_root: PathBuf, project: Option<(PathBuf, ProjectId)>) -> Result<Self> {
        prepare_host_admission_test_profile_root(&profile_root)?;
        if let Some((project_root, project_id)) = project.as_ref() {
            prepare_host_admission_test_project_root(project_root, project_id)?;
        }

        let identity = crate::daemon::profile_identity::load_or_create(&profile_root)?;
        let database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
            identity.profile_root(),
            1,
            "host-admission-test-runtime",
        )?;
        let session_registry =
            Arc::new(DaemonSessionRuntimeRegistryV1::open(identity.clone()).await?);
        let profile_database = session_registry.profile_database().await?;
        let profile_registered = session_registry.profile_sessions().await?;
        let (project_id, project_registered) = if let Some((project_root, project_id)) = project {
            let registered = session_registry
                .project_sessions(project_id.clone(), [project_root])
                .await?;
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

    #[doc(hidden)]
    pub fn registered_database_arc(
        &self,
        scope: HostAdmissionScope,
    ) -> Option<Arc<RegisteredGlobalDb>> {
        match scope {
            HostAdmissionScope::Project => self.project_registered.clone(),
            HostAdmissionScope::Profile => Some(Arc::clone(&self.profile_registered)),
        }
    }

    #[doc(hidden)]
    pub async fn read_snapshot(
        &self,
        scope: HostAdmissionScope,
    ) -> tracedecay_runtime_core::db::engine::Result<
        tracedecay_runtime_core::db::engine::ReadSnapshot,
    > {
        self.registered_database(scope)
            .ok_or_else(|| {
                tracedecay_runtime_core::db::engine::Error::invalid_operation(
                    "registered session test runtime unavailable",
                )
            })?
            .read_snapshot()
            .await
    }

    #[doc(hidden)]
    pub async fn checkpoint_session_database_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> Result<()> {
        self.session_database_for_test(scope)?.checkpoint().await;
        Ok(())
    }

    #[doc(hidden)]
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
    pub async fn session_domain_sha256_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> Result<[u8; 32]> {
        self.checkpoint_session_database_for_test(scope).await?;
        canonical_session_domain_sha256(self.session_database_for_test(scope)?.db_path())
    }

    #[doc(hidden)]
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
                crate::global_db::ParseOffset::default(),
            )
            .await)
    }

    #[doc(hidden)]
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
    pub async fn session_message_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        message_id: &str,
    ) -> Result<Option<tracedecay_sessions::runtime::SessionMessageRecord>> {
        Ok(self
            .session_database_for_test(scope)?
            .get_session_message(provider, message_id)
            .await)
    }

    #[doc(hidden)]
    pub async fn upsert_transcript_batch_for_test(
        &self,
        scope: HostAdmissionScope,
        session: &tracedecay_sessions::runtime::SessionRecord,
        messages: &[tracedecay_sessions::runtime::SessionMessageRecord],
        source: &str,
        offset: crate::global_db::ParseOffset,
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
            let raw = database
                .lcm_load_raw_message(&message.provider, &message.message_id)
                .await
                .ok_or_else(|| TraceDecayError::Database {
                    operation: "read registered transcript fixture store id".to_owned(),
                    message: format!(
                        "LCM raw message {}/{} is unavailable after insert",
                        message.provider, message.message_id
                    ),
                })?;
            store_ids.push(raw.store_id);
        }
        Ok(store_ids)
    }

    #[doc(hidden)]
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
    pub async fn project_lcm_raw_message_exists_for_test(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Result<bool> {
        Ok(self
            .project_database_for_test()?
            .lcm_load_raw_message(provider, message_id)
            .await
            .is_some())
    }

    #[doc(hidden)]
    pub async fn git_sessions_for_for_test(
        &self,
        query: &crate::sessions::git_correlation::SessionsForQuery,
        relation: crate::sessions::git_correlation::CommitRelationFilter,
    ) -> std::result::Result<
        Vec<crate::sessions::git_correlation::SessionGitCorrelationHit>,
        crate::sessions::git_correlation::GitCorrelationError,
    > {
        let database = self.project_database_for_test().map_err(|error| {
            crate::sessions::git_correlation::GitCorrelationError::Db(error.to_string())
        })?;
        crate::store::GlobalDbGitCorrelationStore::new(database)
            .sessions_for_with_relation(query, relation)
            .await
    }

    #[doc(hidden)]
    pub async fn upsert(&self, project_path: &Path, tokens_saved: u64) {
        self.profile_database
            .upsert(project_path, tokens_saved)
            .await;
    }

    #[doc(hidden)]
    pub async fn upsert_code_project(
        &self,
        project_id: &str,
        project_root: &Path,
        git_common_dir: Option<&Path>,
        git_remote_url: Option<&str>,
        default_branch: Option<&str>,
    ) -> Option<crate::global_db::CodeProjectRecord> {
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
    pub async fn upsert_project_alias(
        &self,
        alias_path: &Path,
        project_id: &str,
    ) -> Option<crate::global_db::ProjectAliasRecord> {
        self.profile_database
            .upsert_project_alias(alias_path, project_id)
            .await
    }

    #[doc(hidden)]
    pub async fn upsert_store_instance(
        &self,
        upsert: crate::global_db::StoreInstanceUpsert,
    ) -> Option<crate::global_db::StoreInstanceRecord> {
        self.profile_database.upsert_store_instance(upsert).await
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
            true,
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
            Arc::clone(&self.profile_database),
            Arc::clone(&self.session_registry),
        )
        .await
    }

    /// Reopens an existing project graph through this retained runtime.
    #[doc(hidden)]
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
            Arc::clone(&self.profile_database),
            Arc::clone(&self.session_registry),
        )
        .await
    }

    /// Opens one tracked branch through this retained registered runtime.
    #[doc(hidden)]
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
            true,
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
            Arc::clone(&self.profile_database),
            Arc::clone(&self.session_registry),
        )
        .await
    }

    /// Reopens an existing graph read-only without inferring authority.
    #[doc(hidden)]
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
            Arc::clone(&self.profile_database),
            Arc::clone(&self.session_registry),
        )
        .await
    }

    async fn registered_project_open_inputs(
        &self,
        project_root: &Path,
        open_options: &TraceDecayOpenOptions,
    ) -> Result<(
        tracedecay_runtime_core::storage::StoreLayout,
        Arc<RegisteredGlobalDb>,
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
            true,
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
    let connection = RusqliteConnection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| session_domain_digest_error("open session database", error))?;
    let mut table_statement = connection
        .prepare(
            "SELECT name
             FROM sqlite_schema
             WHERE type = 'table'
               AND name NOT LIKE 'sqlite_%'
               AND name <> 'analytics_events'
             ORDER BY name",
        )
        .map_err(|error| session_domain_digest_error("prepare session table inventory", error))?;
    let tables = table_statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| session_domain_digest_error("query session table inventory", error))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| session_domain_digest_error("read session table inventory", error))?;
    drop(table_statement);

    let mut digest = Sha256::new();
    digest.update(b"tracedecay.session-domain-state.v1\0");
    for table in tables {
        digest_len_prefixed(&mut digest, table.as_bytes());
        let escaped = table.replace('"', "\"\"");
        let mut statement = connection
            .prepare(&format!("SELECT * FROM \"{escaped}\""))
            .map_err(|error| session_domain_digest_error("prepare session table read", error))?;
        let column_count = statement.column_count();
        let order = (1..=column_count)
            .map(|index| index.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("SELECT * FROM \"{escaped}\" ORDER BY {order}");
        drop(statement);
        statement = connection
            .prepare(&sql)
            .map_err(|error| session_domain_digest_error("prepare ordered session read", error))?;
        let mut rows = statement
            .query([])
            .map_err(|error| session_domain_digest_error("query session table", error))?;
        while let Some(row) = rows
            .next()
            .map_err(|error| session_domain_digest_error("read session table row", error))?
        {
            digest.update(b"row\0");
            for index in 0..column_count {
                match row.get_ref(index).map_err(|error| {
                    session_domain_digest_error("decode session table value", error)
                })? {
                    ValueRef::Null => digest.update([0]),
                    ValueRef::Integer(value) => {
                        digest.update([1]);
                        digest.update(value.to_le_bytes());
                    }
                    ValueRef::Real(value) => {
                        digest.update([2]);
                        digest.update(value.to_bits().to_le_bytes());
                    }
                    ValueRef::Text(value) => {
                        digest.update([3]);
                        digest_len_prefixed(&mut digest, value);
                    }
                    ValueRef::Blob(value) => {
                        digest.update([4]);
                        digest_len_prefixed(&mut digest, value);
                    }
                }
            }
        }
    }
    Ok(digest.finalize().into())
}

fn digest_len_prefixed(digest: &mut Sha256, value: &[u8]) {
    digest.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    digest.update(value);
}

fn session_domain_digest_error(operation: &str, error: rusqlite::Error) -> TraceDecayError {
    TraceDecayError::Database {
        operation: operation.to_owned(),
        message: error.to_string(),
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
    if tracedecay_runtime_core::storage::read_enrollment_marker(project_root)?.is_none() {
        tracedecay_runtime_core::storage::write_enrollment_marker(
            project_root,
            &tracedecay_runtime_core::storage::EnrollmentMarker {
                project_id: project_id.as_str().to_owned(),
                storage_mode: tracedecay_runtime_core::storage::StorageMode::ProfileSharded,
            },
        )?;
    }
    Ok(())
}
