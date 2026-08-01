//! One-time migration of historical Hermes-local `TraceDecay` session stores.
//!
//! Runtime storage never resolves through Hermes. This module only scans the
//! historical, bounded locations older installers could use and copies a
//! provably project-owned store into that project's user-profile shard.
//! Sources are opened read-only and are never deleted.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::root_seam::global_db::RegisteredGlobalDb;

mod candidates;
mod copy;
mod fingerprint;
mod memory;
mod pipeline;
mod resolution;
mod session_merge;

pub use candidates::{
    CandidateError, CandidateOutcome, legacy_profile_dirs_for_homes, legacy_store_candidates,
};
pub use copy::{
    copy_external_payload_files, copy_raw_messages, copy_table, remap_store_id_columns,
    remap_summary_source, remove_created_payloads,
};
use pipeline::{migrate_candidate, migrate_legacy_state_store};
pub use resolution::same_path;

const LEDGER_DIR: &str = "migration-ledger/hermes-legacy";
const COPIED_TABLES: &[&str] = &[
    "sessions",
    "session_messages",
    "lcm_external_payloads",
    "lcm_raw_messages",
    "lcm_summary_nodes",
    "lcm_summary_sources",
    "lcm_lifecycle_state",
    "lcm_maintenance_debt",
];
const COPIED_MEMORY_TABLES: &[&str] = &[
    "memory_facts",
    "memory_entities",
    "memory_fact_entities",
    "memory_feedback_events",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyHermesMigration {
    pub source_db: PathBuf,
    pub target_project: PathBuf,
    pub rows_copied: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyHermesMigrationIssue {
    pub source_db: PathBuf,
    pub reason: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LegacyHermesMigrationReport {
    pub migrated: Vec<LegacyHermesMigration>,
    pub already_migrated: Vec<LegacyHermesMigration>,
    pub unresolved: Vec<LegacyHermesMigrationIssue>,
    pub failed: Vec<LegacyHermesMigrationIssue>,
}

/// Migrates historical stores below the standard user Hermes integration into
/// the normal `TraceDecay` user profile. No environment or working-directory
/// override can redirect discovery.
pub async fn migrate_legacy_hermes_stores(user_home: &Path) -> LegacyHermesMigrationReport {
    let Ok(profile_root) = tracedecay_runtime_core::storage::default_profile_root() else {
        return LegacyHermesMigrationReport {
            failed: vec![LegacyHermesMigrationIssue {
                source_db: user_home.join(".hermes/.tracedecay/sessions.db"),
                reason: "could not resolve the TraceDecay user-profile store".to_string(),
            }],
            ..LegacyHermesMigrationReport::default()
        };
    };
    let hermes_homes = [user_home.join(".hermes")];
    if !has_legacy_hermes_sources(&hermes_homes, &profile_root) {
        return LegacyHermesMigrationReport::default();
    }
    let lifecycle = match crate::root_seam::daemon::QuiescedDaemonLifecycle::acquire(
        "legacy Hermes store migration",
    ) {
        Ok(lifecycle) => lifecycle,
        Err(error) => return migration_authority_failure(&profile_root, error.to_string()),
    };
    let lifecycle_lease = match lifecycle.lifecycle_lease() {
        Ok(lifecycle_lease) => lifecycle_lease,
        Err(error) => return migration_authority_failure(&profile_root, error.to_string()),
    };
    let mut report = migrate_legacy_hermes_stores_with_lease(
        user_home,
        &profile_root,
        &hermes_homes,
        lifecycle_lease,
        None,
    )
    .await;
    if let Err(error) = lifecycle.finish() {
        report.failed.push(LegacyHermesMigrationIssue {
            source_db: user_home.join(".hermes/.tracedecay/sessions.db"),
            reason: format!(
                "failed to restore TraceDecay daemon state after legacy Hermes store migration: {error}"
            ),
        });
    }
    report
}

/// Migrates historical Hermes stores while the caller retains exclusive
/// lifecycle authority for the destination profile.
///
/// Post-update already owns this lease while agents are refreshed. Reusing it
/// avoids trying to acquire a second exclusive lock from the same process.
pub async fn migrate_legacy_hermes_stores_under_lease(
    user_home: &Path,
    lifecycle: &tracedecay_runtime_core::lifecycle_lease::LifecycleLease,
) -> LegacyHermesMigrationReport {
    let Ok(profile_root) = tracedecay_runtime_core::storage::default_profile_root() else {
        return LegacyHermesMigrationReport {
            failed: vec![LegacyHermesMigrationIssue {
                source_db: user_home.join(".hermes/.tracedecay/sessions.db"),
                reason: "could not resolve the TraceDecay user-profile store".to_string(),
            }],
            ..LegacyHermesMigrationReport::default()
        };
    };
    let hermes_homes = [user_home.join(".hermes")];
    if !has_legacy_hermes_sources(&hermes_homes, &profile_root) {
        return LegacyHermesMigrationReport::default();
    }
    migrate_legacy_hermes_stores_with_lease(
        user_home,
        &profile_root,
        &hermes_homes,
        lifecycle,
        None,
    )
    .await
}

fn has_legacy_hermes_sources(hermes_homes: &[PathBuf], profile_root: &Path) -> bool {
    let profile_dirs = legacy_profile_dirs_for_homes(hermes_homes);
    !legacy_store_candidates(&profile_dirs, profile_root).is_empty()
        || profile_dirs.iter().any(|profile_dir| {
            profile_dir.join("state.db").is_file()
                && crate::root_seam::agents::hermes::read_config_pinned_project_root(
                    &profile_dir.join("config.yaml"),
                )
                .is_some()
        })
}

/// Explicit `TraceDecay` profile-root seam used by migration tests. The source
/// root remains the user's standard home; the second argument controls only
/// the destination `TraceDecay` profile.
pub async fn migrate_legacy_hermes_stores_to(
    user_home: &Path,
    tracedecay_profile_root: &Path,
) -> LegacyHermesMigrationReport {
    let hermes_homes = [user_home.join(".hermes")];
    if !has_legacy_hermes_sources(&hermes_homes, tracedecay_profile_root) {
        return LegacyHermesMigrationReport::default();
    }
    migrate_legacy_hermes_stores_inner(user_home, tracedecay_profile_root, &hermes_homes, None)
        .await
}

async fn migrate_legacy_hermes_stores_inner(
    user_home: &Path,
    tracedecay_profile_root: &Path,
    hermes_homes: &[PathBuf],
    fail_after_table: Option<&str>,
) -> LegacyHermesMigrationReport {
    let lifecycle = match tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
        tracedecay_profile_root,
        "legacy Hermes store migration",
    ) {
        Ok(lifecycle) => lifecycle,
        Err(error) => {
            return migration_authority_failure(tracedecay_profile_root, error.to_string());
        }
    };
    migrate_legacy_hermes_stores_with_lease(
        user_home,
        tracedecay_profile_root,
        hermes_homes,
        &lifecycle,
        fail_after_table,
    )
    .await
}

async fn migrate_legacy_hermes_stores_with_lease(
    user_home: &Path,
    tracedecay_profile_root: &Path,
    hermes_homes: &[PathBuf],
    lifecycle: &tracedecay_runtime_core::lifecycle_lease::LifecycleLease,
    fail_after_table: Option<&str>,
) -> LegacyHermesMigrationReport {
    let _database_scope = match tracedecay_runtime_core::db::enter_maintenance_database_scope(
        lifecycle,
        tracedecay_profile_root,
        "legacy Hermes store migration",
    ) {
        Ok(scope) => scope,
        Err(error) => {
            return migration_authority_failure(tracedecay_profile_root, error.to_string());
        }
    };
    let profile_identity =
        match crate::root_seam::daemon::profile_identity::load_or_create(tracedecay_profile_root) {
            Ok(identity) => identity,
            Err(error) => {
                return migration_authority_failure(
                    tracedecay_profile_root,
                    format!("could not load migration profile identity: {error}"),
                );
            }
        };
    let session_registry =
        match crate::root_seam::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1::open(
            profile_identity,
        )
        .await
        {
            Ok(registry) => registry,
            Err(error) => {
                return migration_authority_failure(
                    tracedecay_profile_root,
                    format!("could not open migration runtime registry: {error}"),
                );
            }
        };
    let profile_registry = match session_registry.profile_database().await {
        Ok(database) => database,
        Err(error) => {
            return migration_authority_failure(
                tracedecay_profile_root,
                format!("could not mount migration profile registry: {error}"),
            );
        }
    };
    let profile_dirs = legacy_profile_dirs_for_homes(hermes_homes);
    let mut report = LegacyHermesMigrationReport::default();
    for candidate in legacy_store_candidates(&profile_dirs, tracedecay_profile_root) {
        let source_db = candidate.primary_path().to_path_buf();
        match migrate_candidate(
            user_home,
            hermes_homes,
            &candidate,
            tracedecay_profile_root,
            &session_registry,
            profile_registry.as_ref(),
            fail_after_table,
        )
        .await
        {
            Ok(CandidateOutcome::Migrated(migration, preserved_memory)) => {
                if let Err(reason) = remove_legacy_registry_metadata(
                    profile_registry.as_ref(),
                    candidate.legacy_registry_project_id.as_deref(),
                    &candidate.profile_dir,
                )
                .await
                {
                    report.failed.push(LegacyHermesMigrationIssue {
                        source_db: source_db.clone(),
                        reason,
                    });
                }
                report.migrated.push(migration);
                report.unresolved.extend(preserved_memory);
            }
            Ok(CandidateOutcome::AlreadyMigrated(migration, preserved_memory)) => {
                if let Err(reason) = remove_legacy_registry_metadata(
                    profile_registry.as_ref(),
                    candidate.legacy_registry_project_id.as_deref(),
                    &candidate.profile_dir,
                )
                .await
                {
                    report.failed.push(LegacyHermesMigrationIssue {
                        source_db: source_db.clone(),
                        reason,
                    });
                }
                report.already_migrated.push(migration);
                report.unresolved.extend(preserved_memory);
            }
            Err(CandidateError::Unresolved(reason)) => {
                report
                    .unresolved
                    .push(LegacyHermesMigrationIssue { source_db, reason });
            }
            Err(CandidateError::Failed(reason)) => {
                report
                    .failed
                    .push(LegacyHermesMigrationIssue { source_db, reason });
            }
        }
    }
    for profile_dir in profile_dirs {
        let state_db = profile_dir.join("state.db");
        if !state_db.is_file()
            || crate::root_seam::agents::hermes::read_config_pinned_project_root(
                &profile_dir.join("config.yaml"),
            )
            .is_none()
        {
            continue;
        }
        match migrate_legacy_state_store(
            user_home,
            hermes_homes,
            &profile_dir,
            tracedecay_profile_root,
            &session_registry,
            profile_registry.as_ref(),
        )
        .await
        {
            Ok(CandidateOutcome::Migrated(migration, preserved_memory)) => {
                report.migrated.push(migration);
                report.unresolved.extend(preserved_memory);
            }
            Ok(CandidateOutcome::AlreadyMigrated(migration, preserved_memory)) => {
                report.already_migrated.push(migration);
                report.unresolved.extend(preserved_memory);
            }
            Err(CandidateError::Unresolved(reason)) => {
                report.unresolved.push(LegacyHermesMigrationIssue {
                    source_db: state_db,
                    reason,
                });
            }
            Err(CandidateError::Failed(reason)) => report.failed.push(LegacyHermesMigrationIssue {
                source_db: state_db,
                reason,
            }),
        }
    }
    report
}

fn migration_authority_failure(
    tracedecay_profile_root: &Path,
    reason: String,
) -> LegacyHermesMigrationReport {
    LegacyHermesMigrationReport {
        failed: vec![LegacyHermesMigrationIssue {
            source_db: tracedecay_profile_root.to_path_buf(),
            reason,
        }],
        ..LegacyHermesMigrationReport::default()
    }
}

async fn remove_legacy_registry_metadata(
    registry: &RegisteredGlobalDb,
    project_id: Option<&str>,
    expected_legacy_root: &Path,
) -> Result<(), String> {
    let Some(project_id) = project_id else {
        return Ok(());
    };
    let Some(project) = registry.get_code_project(project_id).await else {
        return Ok(());
    };
    if !same_path(Path::new(&project.canonical_root), expected_legacy_root)
        && !same_path(Path::new(&project.display_root), expected_legacy_root)
    {
        return Ok(());
    }
    registry
        .delete_code_projects(&[project_id.to_string()])
        .await;
    if registry.get_code_project(project_id).await.is_some() {
        return Err(format!(
            "migrated legacy sessions, but could not remove legacy Hermes registry metadata for '{project_id}'; source stores were preserved"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::candidates::legacy_profile_dirs;
    use super::*;
    use sha2::{Digest, Sha256};
    use tracedecay_domain::ProjectId;
    use tracedecay_global_db::tests::harness::HostAdmissionTestRuntimeV1;
    use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, TestConnection, params};
    use tracedecay_runtime_core::memory::store::MemoryStore;
    use tracedecay_runtime_core::memory::types::{
        AddFactRequest, FeedbackAction, FeedbackRequest, MemoryCategory,
    };
    use tracedecay_rusqlite_runtime::migration_sql::{
        MigrationSqlError, MigrationSqlWriteAuthority, MigrationSqlWriteIntent,
    };
    use tracedecay_sessions::admission::HostAdmissionScope;
    use tracedecay_store::{SessionMessageRecord, SessionRecord};

    static USER_DATA_DIR_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct PinnedMigrationEnvironment {
        _lock: std::sync::MutexGuard<'static, ()>,
        _root: tempfile::TempDir,
        previous_data_dir: Option<std::ffi::OsString>,
        previous_home: Option<std::ffi::OsString>,
        previous_userprofile: Option<std::ffi::OsString>,
        previous_xdg_config: Option<std::ffi::OsString>,
    }

    impl PinnedMigrationEnvironment {
        fn new() -> Self {
            let lock = USER_DATA_DIR_TEST_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let root = tempfile::tempdir().unwrap();
            let profile = root
                .path()
                .join(tracedecay_runtime_core::config::TRACEDECAY_DIR);
            tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(&profile).unwrap();
            let previous_data_dir =
                std::env::var_os(tracedecay_runtime_core::config::USER_DATA_DIR_ENV);
            let previous_home = std::env::var_os("HOME");
            let previous_userprofile = std::env::var_os("USERPROFILE");
            let previous_xdg_config = std::env::var_os("XDG_CONFIG_HOME");
            // SAFETY: this crate's environment-mutating tests hold this lock
            // for the complete lifetime of the isolated environment.
            unsafe {
                std::env::set_var(tracedecay_runtime_core::config::USER_DATA_DIR_ENV, &profile);
                std::env::set_var("HOME", root.path());
                std::env::set_var("USERPROFILE", root.path());
                std::env::set_var("XDG_CONFIG_HOME", root.path().join("config"));
            }
            Self {
                _lock: lock,
                _root: root,
                previous_data_dir,
                previous_home,
                previous_userprofile,
                previous_xdg_config,
            }
        }
    }

    impl Default for PinnedMigrationEnvironment {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Drop for PinnedMigrationEnvironment {
        fn drop(&mut self) {
            // SAFETY: the environment lock remains held while values are
            // restored.
            unsafe {
                match self.previous_data_dir.take() {
                    Some(value) => {
                        std::env::set_var(tracedecay_runtime_core::config::USER_DATA_DIR_ENV, value)
                    }
                    None => {
                        std::env::remove_var(tracedecay_runtime_core::config::USER_DATA_DIR_ENV)
                    }
                }
                match self.previous_home.take() {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
                match self.previous_userprofile.take() {
                    Some(value) => std::env::set_var("USERPROFILE", value),
                    None => std::env::remove_var("USERPROFILE"),
                }
                match self.previous_xdg_config.take() {
                    Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                    None => std::env::remove_var("XDG_CONFIG_HOME"),
                }
            }
        }
    }

    struct ForeignFixtureWriteAuthority;

    impl MigrationSqlWriteAuthority for ForeignFixtureWriteAuthority {
        fn verify(&self, _intent: MigrationSqlWriteIntent) -> Result<(), MigrationSqlError> {
            Ok(())
        }
    }

    #[derive(Clone, Copy)]
    enum HermesFixtureTable {
        Sessions,
        SessionMessages,
        MemoryFeedbackEvents,
    }

    impl HermesFixtureTable {
        const fn name(self) -> &'static str {
            match self {
                Self::Sessions => "sessions",
                Self::SessionMessages => "session_messages",
                Self::MemoryFeedbackEvents => "memory_feedback_events",
            }
        }
    }

    /// Opaque writable builder for foreign legacy-source fixtures.
    ///
    /// The migration never receives this handle. Tests drop it before invoking
    /// migration, which then reads the source through the production immutable
    /// snapshot path.
    struct HermesMigrationTestRuntime {
        connection: TestConnection,
    }

    impl HermesMigrationTestRuntime {
        async fn create(path: &Path) -> Self {
            if let Some(parent) = path.parent() {
                tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(parent).unwrap();
            }
            let connection = TestConnection::open_with_write_authority(
                path,
                Arc::new(ForeignFixtureWriteAuthority),
            );
            tracedecay_runtime_core::db::migrations::migrate_connection(&connection)
                .await
                .unwrap();
            crate::root_seam::global_db::ensure_registered_schema(&connection)
                .await
                .unwrap();
            Self { connection }
        }

        async fn seed_sessions(&self, sessions: &[(&str, &Path)]) {
            for (ordinal, (session_id, project)) in sessions.iter().enumerate() {
                let project = project.to_string_lossy().to_string();
                let session = SessionRecord {
                    provider: "hermes".into(),
                    session_id: (*session_id).into(),
                    project_key: project.clone(),
                    project_path: project,
                    title: Some("legacy".into()),
                    started_at: Some(ordinal as i64 + 1),
                    ended_at: None,
                    transcript_path: None,
                    metadata_json: None,
                    parent_session_id: None,
                    is_subagent: false,
                    agent_id: None,
                    parent_tool_use_id: None,
                };
                let message = SessionMessageRecord {
                    provider: "hermes".into(),
                    message_id: format!("message-{session_id}"),
                    session_id: (*session_id).into(),
                    role: "user".into(),
                    timestamp: Some(ordinal as i64 + 1),
                    ordinal: 0,
                    text: "keep this".into(),
                    kind: None,
                    model: None,
                    tool_names: None,
                    source_path: None,
                    source_offset: None,
                    metadata_json: None,
                };
                assert!(
                    self.connection
                        .execute(
                            "INSERT OR REPLACE INTO sessions (
                                provider, session_id, project_key, project_path, title,
                                started_at, ended_at, transcript_path, metadata_json,
                                parent_session_id, is_subagent, agent_id, parent_tool_use_id
                             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, NULL, NULL, 0, NULL, NULL)",
                            params![
                                session.provider.as_str(),
                                session.session_id.as_str(),
                                session.project_key.as_str(),
                                session.project_path.as_str(),
                                session.title.as_deref(),
                                session.started_at
                            ],
                        )
                        .await
                        .unwrap()
                        > 0
                );
                assert!(
                    self.connection
                        .execute(
                            "INSERT OR REPLACE INTO session_messages (
                                provider, message_id, session_id, role, timestamp, ordinal,
                                text, kind, model, tool_names, source_path, source_offset,
                                metadata_json
                             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, NULL, NULL, NULL, NULL)",
                            params![
                                message.provider.as_str(),
                                message.message_id.as_str(),
                                message.session_id.as_str(),
                                message.role.as_str(),
                                message.timestamp,
                                message.ordinal,
                                message.text.as_str()
                            ],
                        )
                        .await
                        .unwrap()
                        > 0
                );
                let content_hash = hex::encode(Sha256::digest(message.text.as_bytes()));
                assert!(
                    self.connection
                        .execute(
                            "INSERT OR REPLACE INTO lcm_raw_messages (
                                provider, message_id, session_id, role, ordinal, timestamp,
                                content, content_hash, storage_kind, payload_ref, snippet_text,
                                index_text, legacy_source, legacy_truncated, metadata_json
                             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'inline', NULL, ?7, ?7, 0, 0, NULL)",
                            params![
                                message.provider.as_str(),
                                message.message_id.as_str(),
                                message.session_id.as_str(),
                                message.role.as_str(),
                                message.ordinal,
                                message.timestamp,
                                message.text.as_str(),
                                content_hash
                            ],
                        )
                        .await
                        .unwrap()
                        > 0
                );
            }
        }

        async fn add_memory_fact(&self, content: &str) -> i64 {
            self.add_memory_fact_request(AddFactRequest {
                content: content.to_string(),
                category: MemoryCategory::Decision,
                source: Some("hermes".to_string()),
                tags: vec!["legacy".to_string()],
                entities: vec!["TraceDecay".to_string()],
                trust: Some(0.9),
                metadata: serde_json::json!({"migration_test": true}),
            })
            .await
        }

        async fn add_memory_fact_request(&self, request: AddFactRequest) -> i64 {
            MemoryStore::new_runtime(&self.connection)
                .add_fact(request, 0.5)
                .await
                .unwrap()
                .fact
                .unwrap()
                .fact_id
        }

        async fn set_session_project_path(&self, session_id: &str, project_path: &Path) {
            self.connection
                .execute(
                    "UPDATE sessions SET project_path = ?1 WHERE session_id = ?2",
                    params![project_path.to_string_lossy().as_ref(), session_id],
                )
                .await
                .unwrap();
        }

        async fn set_session_metadata_without_project(
            &self,
            session_id: &str,
            metadata_json: &str,
        ) {
            self.connection
                .execute(
                    "UPDATE sessions
                     SET project_key = '', project_path = '', metadata_json = ?1
                     WHERE session_id = ?2",
                    params![metadata_json, session_id],
                )
                .await
                .unwrap();
        }

        async fn set_lcm_schema_version(&self, version: i64) {
            self.connection
                .execute(
                    "UPDATE session_schema_migrations SET version = ?1 WHERE name = 'lcm'",
                    params![version],
                )
                .await
                .unwrap();
        }

        async fn insert_external_payload(&self, payload_ref: &str, payload: &[u8]) {
            self.connection
                .execute(
                    "INSERT INTO lcm_external_payloads (
                        payload_ref, provider, session_id, message_id, kind, content_hash,
                        byte_count, char_count, created_at
                     ) VALUES (?1, 'hermes', 'session', 'message-session', 'text', ?2, ?3, ?3, 1)",
                    params![
                        payload_ref,
                        hex::encode(Sha256::digest(payload)),
                        payload.len() as i64
                    ],
                )
                .await
                .unwrap();
        }

        async fn record_memory_feedback(&self, request: FeedbackRequest) {
            MemoryStore::new_runtime(&self.connection)
                .record_feedback_event(request)
                .await
                .unwrap();
        }

        async fn assert_memory_merge_waits_for_writer(source_path: &Path, target_path: &Path) {
            let source = tracedecay_runtime_core::sqlite_read_snapshot::open(source_path)
                .await
                .unwrap();
            let target = Self::create(target_path).await;
            let writer = target
                .connection
                .transaction_with_behavior(
                    tracedecay_runtime_core::db::engine::TransactionBehavior::Immediate,
                )
                .await
                .unwrap();
            let mut merge = Box::pin(super::memory::merge_memory_snapshot_for_test(
                source.connection(),
                &target.connection,
            ));

            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(25), &mut merge)
                    .await
                    .is_err()
            );
            writer.rollback().await.unwrap();
            assert!(merge.await.unwrap() > 0);
            source.validate_source().unwrap();
        }

        fn create_legacy_state_without_cwd(path: &Path) {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let connection = rusqlite::Connection::open(path).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE sessions (
                        id TEXT PRIMARY KEY,
                        source TEXT NOT NULL,
                        model TEXT,
                        parent_session_id TEXT,
                        started_at REAL NOT NULL,
                        ended_at REAL,
                        title TEXT,
                        input_tokens INTEGER DEFAULT 0,
                        output_tokens INTEGER DEFAULT 0,
                        cache_read_tokens INTEGER DEFAULT 0,
                        cache_write_tokens INTEGER DEFAULT 0,
                        reasoning_tokens INTEGER DEFAULT 0
                     );
                     CREATE TABLE messages (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        session_id TEXT NOT NULL,
                        role TEXT NOT NULL,
                        content TEXT,
                        tool_calls TEXT,
                        tool_name TEXT,
                        timestamp REAL NOT NULL,
                        reasoning TEXT,
                        active INTEGER NOT NULL DEFAULT 1
                     );
                     INSERT INTO sessions (
                        id, source, model, started_at, ended_at, title
                     ) VALUES (
                        'legacy-state-session', 'tui', 'legacy-model', 1.0, 2.0, 'legacy state'
                     );
                     INSERT INTO messages (
                        session_id, role, content, timestamp
                     ) VALUES (
                        'legacy-state-session', 'user', 'state row without cwd', 1.0
                     );",
                )
                .unwrap();
        }

        fn create_older_session_source(path: &Path, project: &Path) {
            let connection = rusqlite::Connection::open(path).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE sessions (
                        provider TEXT NOT NULL,
                        session_id TEXT NOT NULL,
                        project_key TEXT NOT NULL,
                        project_path TEXT NOT NULL,
                        title TEXT,
                        PRIMARY KEY(provider, session_id)
                     );
                     CREATE TABLE session_messages (
                        provider TEXT NOT NULL,
                        message_id TEXT NOT NULL,
                        session_id TEXT NOT NULL,
                        role TEXT NOT NULL,
                        ordinal INTEGER NOT NULL,
                        text TEXT NOT NULL,
                        PRIMARY KEY(provider, message_id)
                     );",
                )
                .unwrap();
            let project = project.to_string_lossy().into_owned();
            connection
                .execute(
                    "INSERT INTO sessions(provider, session_id, project_key, project_path, title)
                     VALUES ('hermes', 'old-session', ?1, ?1, 'old')",
                    rusqlite::params![project],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO session_messages(provider, message_id, session_id, role, ordinal, text)
                     VALUES ('hermes', 'old-message', 'old-session', 'user', 0, 'old text')",
                    (),
                )
                .unwrap();
        }
    }

    async fn query_count(
        connection: &(impl QueryExecutor + ?Sized),
        table: HermesFixtureTable,
    ) -> i64 {
        let mut rows = connection
            .query(&format!("SELECT COUNT(*) FROM {}", table.name()), ())
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    async fn immutable_source_count(path: &Path, table: HermesFixtureTable) -> i64 {
        let snapshot = tracedecay_runtime_core::sqlite_read_snapshot::open(path)
            .await
            .unwrap();
        let count = query_count(snapshot.connection(), table).await;
        snapshot.validate_source().unwrap();
        count
    }

    async fn immutable_memory_facts(path: &Path) -> Vec<(String, String, Vec<String>, i64, i64)> {
        let snapshot = tracedecay_runtime_core::sqlite_read_snapshot::open(path)
            .await
            .unwrap();
        let mut rows = snapshot
            .connection()
            .query(
                "SELECT f.content, f.tags, COALESCE(group_concat(e.name, char(31)), ''),
                        f.helpful_count, f.unhelpful_count
                 FROM memory_facts f
                 LEFT JOIN memory_fact_entities fe ON fe.fact_id = f.fact_id
                 LEFT JOIN memory_entities e ON e.entity_id = fe.entity_id
                 GROUP BY f.fact_id
                 ORDER BY f.fact_id",
                (),
            )
            .await
            .unwrap();
        let mut facts = Vec::new();
        while let Some(row) = rows.next().await.unwrap() {
            let entities = row
                .get::<String>(2)
                .unwrap()
                .split('\u{1f}')
                .filter(|entity| !entity.is_empty())
                .map(str::to_owned)
                .collect();
            facts.push((
                row.get(0).unwrap(),
                row.get(1).unwrap(),
                entities,
                row.get(3).unwrap(),
                row.get(4).unwrap(),
            ));
        }
        snapshot.validate_source().unwrap();
        facts
    }

    async fn registered_project_target(
        profile_root: &Path,
        project_root: &Path,
    ) -> HostAdmissionTestRuntimeV1 {
        let layout =
            tracedecay_runtime_core::storage::resolve_layout(project_root, profile_root).unwrap();
        HostAdmissionTestRuntimeV1::project(
            profile_root,
            project_root,
            ProjectId::new(layout.identity.project_id.unwrap()).unwrap(),
        )
        .await
        .unwrap()
    }

    async fn registered_project_target_with_id(
        profile_root: &Path,
        project_root: &Path,
        project_id: &str,
    ) -> HostAdmissionTestRuntimeV1 {
        HostAdmissionTestRuntimeV1::project(
            profile_root,
            project_root,
            ProjectId::new(project_id.to_string()).unwrap(),
        )
        .await
        .unwrap()
    }

    async fn registered_profile_target(profile_root: &Path) -> HostAdmissionTestRuntimeV1 {
        HostAdmissionTestRuntimeV1::profile(profile_root)
            .await
            .unwrap()
    }

    async fn seed_registered_session(
        runtime: &HostAdmissionTestRuntimeV1,
        scope: HostAdmissionScope,
        project: &Path,
        session_id: &str,
        title: &str,
        message_text: &str,
    ) {
        let project = project.to_string_lossy().into_owned();
        let session = SessionRecord {
            provider: "hermes".into(),
            session_id: session_id.into(),
            project_key: project.clone(),
            project_path: project,
            title: Some(title.into()),
            started_at: Some(1),
            ended_at: None,
            transcript_path: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        };
        let message = SessionMessageRecord {
            provider: "hermes".into(),
            message_id: format!("message-{session_id}"),
            session_id: session_id.into(),
            role: "user".into(),
            timestamp: Some(1),
            ordinal: 0,
            text: message_text.into(),
            kind: None,
            model: None,
            tool_names: None,
            source_path: None,
            source_offset: None,
            metadata_json: None,
        };
        assert!(
            runtime
                .upsert_session_for_test(scope, &session)
                .await
                .unwrap()
        );
        assert!(
            runtime
                .upsert_session_message_for_test(scope, &message)
                .await
                .unwrap()
        );
    }

    fn mark_real_project(project: &Path) {
        fs::create_dir_all(project.join(".tracedecay")).unwrap();
        fs::write(project.join(".tracedecay/tracedecay.db"), []).unwrap();
        write_project_enrollment(
            project,
            &tracedecay_runtime_core::storage::default_profile_project_id(project),
        );
    }

    fn write_project_enrollment(project: &Path, project_id: &str) {
        tracedecay_runtime_core::storage::write_enrollment_marker(
            project,
            &tracedecay_runtime_core::storage::EnrollmentMarker {
                project_id: project_id.to_string(),
                storage_mode: tracedecay_runtime_core::storage::StorageMode::ProfileSharded,
            },
        )
        .unwrap();
    }

    #[tokio::test]
    async fn caller_owned_lifecycle_lease_is_reused_without_self_conflict() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("profile");
        let hermes_homes = [user_home.join(".hermes")];
        let lifecycle = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
            &profile_root,
            "post-update test",
        )
        .unwrap();

        let report = migrate_legacy_hermes_stores_with_lease(
            &user_home,
            &profile_root,
            &hermes_homes,
            &lifecycle,
            None,
        )
        .await;

        assert!(report.failed.is_empty(), "{report:?}");
        assert!(report.unresolved.is_empty(), "{report:?}");
    }

    #[tokio::test]
    async fn default_migration_releases_lifecycle_authority_after_restore() {
        let _environment = PinnedMigrationEnvironment::new();
        let user_home = tempfile::tempdir().unwrap();

        let report = migrate_legacy_hermes_stores(user_home.path()).await;

        assert!(report.failed.is_empty(), "{report:?}");
        assert!(report.unresolved.is_empty(), "{report:?}");
        let profile_root = tracedecay_runtime_core::storage::default_profile_root().unwrap();
        let reacquired = tracedecay_runtime_core::lifecycle_lease::acquire_exclusive_for_profile(
            &profile_root,
            "post-migration test",
        );
        assert!(reacquired.is_ok(), "lifecycle lease was not released");
    }

    #[tokio::test]
    async fn registry_cleanup_preserves_reassigned_project_identity() {
        let temp = tempfile::tempdir().unwrap();
        let profile_root = temp.path().join("profile");
        let legacy_root = temp.path().join("home/.hermes");
        let corrected_root = temp.path().join("projects/hermes-agent");
        fs::create_dir_all(&legacy_root).unwrap();
        fs::create_dir_all(&corrected_root).unwrap();
        let registry = registered_profile_target(&profile_root).await;
        registry
            .profile_registry()
            .upsert_code_project("reassigned", &corrected_root, None, None, None)
            .await
            .unwrap();

        let reassigned = registry
            .profile_registry()
            .get_code_project("reassigned")
            .await
            .expect("registered reassigned project");
        assert!(!same_path(
            Path::new(&reassigned.canonical_root),
            &legacy_root
        ));
        assert!(!same_path(
            Path::new(&reassigned.display_root),
            &legacy_root
        ));
    }

    async fn seed_source(path: &Path, sessions: &[(&str, &Path)]) {
        HermesMigrationTestRuntime::create(path)
            .await
            .seed_sessions(sessions)
            .await;
    }

    async fn seed_memory_fact(path: &Path, content: &str) -> i64 {
        HermesMigrationTestRuntime::create(path)
            .await
            .add_memory_fact(content)
            .await
    }

    async fn seed_legacy_state_db_without_cwd(path: &Path) {
        HermesMigrationTestRuntime::create_legacy_state_without_cwd(path);
    }

    fn marker_count(target_db_path: &Path) -> usize {
        target_db_path
            .parent()
            .and_then(|root| fs::read_dir(root.join(LEDGER_DIR)).ok())
            .map_or(0, |entries| entries.flatten().count())
    }

    #[tokio::test]
    async fn migrates_standard_profile_store_once() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let hermes = user_home.join(".hermes");
        let source = hermes.join(".tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(
            hermes.join("config.yaml"),
            format!(
                "plugins:\n  tracedecay:\n    project_root: {}\n",
                project.display()
            ),
        )
        .unwrap();
        seed_source(&source, &[("session-1", &project)]).await;
        seed_memory_fact(
            &source.with_file_name("tracedecay.db"),
            "legacy Hermes fact",
        )
        .await;

        let first = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(first.migrated.len(), 1, "{first:?}");
        assert!(first.migrated[0].rows_copied >= 3);
        let layout =
            tracedecay_runtime_core::storage::resolve_layout(&project, &profile_root).unwrap();
        let target = registered_project_target(&profile_root, &project).await;
        let counts = target
            .transcript_store_counts_for_test(
                HostAdmissionScope::Project,
                "hermes",
                "session-1",
                Path::new("unused"),
            )
            .await
            .unwrap();
        assert_eq!((counts.0, counts.1, counts.2), (1, 1, 1));
        assert_eq!(marker_count(&layout.sessions_db_path), 1);
        let facts = immutable_memory_facts(&layout.graph_db_path).await;
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].0, "legacy Hermes fact");
        assert!(facts[0].2.contains(&"TraceDecay".to_string()));
        assert_eq!(
            target
                .session_for_test(HostAdmissionScope::Project, "hermes", "session-1")
                .await
                .unwrap()
                .unwrap()
                .project_path,
            HostAdmissionTestRuntimeV1::canonical_project_key(&project)
        );
        drop(target);

        let second = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(second.already_migrated.len(), 1, "{second:?}");
        let target = registered_project_target(&profile_root, &project).await;
        assert!(
            target
                .session_for_test(HostAdmissionScope::Project, "hermes", "session-1")
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(marker_count(&layout.sessions_db_path), 1);
        assert_eq!(immutable_memory_facts(&layout.graph_db_path).await.len(), 1);
        assert_eq!(
            immutable_source_count(&source, HermesFixtureTable::Sessions).await,
            1
        );
    }

    #[tokio::test]
    async fn migration_marker_remerges_when_a_target_row_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let hermes = user_home.join(".hermes");
        let source = hermes.join(".tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(
            hermes.join("config.yaml"),
            format!(
                "plugins:\n  tracedecay:\n    project_root: {}\n",
                project.display()
            ),
        )
        .unwrap();
        seed_source(&source, &[("session-1", &project)]).await;

        let first = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(first.migrated.len(), 1, "{first:?}");
        let initial_rows_copied = first.migrated[0].rows_copied;
        let layout =
            tracedecay_runtime_core::storage::resolve_layout(&project, &profile_root).unwrap();
        let target = registered_project_target(&profile_root, &project).await;
        assert_eq!(
            target
                .delete_session_message_for_test(
                    HostAdmissionScope::Project,
                    "hermes",
                    "message-session-1",
                )
                .await
                .unwrap(),
            1
        );
        drop(target);

        let repaired = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(repaired.migrated.len(), 1, "{repaired:?}");
        assert!(repaired.already_migrated.is_empty(), "{repaired:?}");
        assert_eq!(repaired.migrated[0].rows_copied, 1, "{repaired:?}");
        let target = registered_project_target(&profile_root, &project).await;
        assert_eq!(
            target
                .project_session_message_count_for_test()
                .await
                .unwrap(),
            1
        );
        drop(target);

        let verified = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(verified.already_migrated.len(), 1, "{verified:?}");
        assert_eq!(
            verified.already_migrated[0].rows_copied,
            initial_rows_copied + 1,
            "{verified:?}"
        );
        assert_eq!(marker_count(&layout.sessions_db_path), 1);

        let marker_path = fs::read_dir(layout.sessions_db_path.parent().unwrap().join(LEDGER_DIR))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let mut marker: serde_json::Value =
            serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
        marker["schema_version"] = serde_json::json!(1);
        marker.as_object_mut().unwrap().remove("target_project_id");
        marker.as_object_mut().unwrap().remove("target_db_path");
        fs::write(&marker_path, serde_json::to_vec_pretty(&marker).unwrap()).unwrap();

        let upgraded = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(upgraded.already_migrated.len(), 1, "{upgraded:?}");
        let mut marker: serde_json::Value =
            serde_json::from_slice(&fs::read(&marker_path).unwrap()).unwrap();
        assert_eq!(marker["schema_version"], 2);
        assert!(marker["target_project_id"].as_str().is_some());
        assert!(marker["target_db_path"].as_str().is_some());

        marker["target_project_id"] = serde_json::json!("proj_wrong_target");
        fs::write(&marker_path, serde_json::to_vec_pretty(&marker).unwrap()).unwrap();

        let mismatched = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(mismatched.failed.len(), 1, "{mismatched:?}");
        assert!(
            mismatched.failed[0]
                .reason
                .contains("different project store")
        );
    }

    #[tokio::test]
    async fn migrates_pinned_memory_store_without_session_store() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let hermes = user_home.join(".hermes");
        fs::create_dir_all(hermes.join(".tracedecay")).unwrap();
        fs::write(
            hermes.join("config.yaml"),
            format!(
                "plugins:\n  tracedecay:\n    project_root: {}\n",
                project.display()
            ),
        )
        .unwrap();
        seed_memory_fact(
            &hermes.join(".tracedecay/tracedecay.db"),
            "facts survive without sessions",
        )
        .await;

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.migrated.len(), 1, "{report:?}");
        let layout =
            tracedecay_runtime_core::storage::resolve_layout(&project, &profile_root).unwrap();
        let facts = immutable_memory_facts(&layout.graph_db_path).await;
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].0, "facts survive without sessions");
    }

    #[tokio::test]
    async fn migrates_pinned_state_db_rows_without_cwd_before_unpin() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let hermes = user_home.join(".hermes");
        fs::create_dir_all(&hermes).unwrap();
        fs::write(
            hermes.join("config.yaml"),
            format!(
                "plugins:\n  tracedecay:\n    project_root: {}\n",
                project.display()
            ),
        )
        .unwrap();
        let state_db = hermes.join("state.db");
        seed_legacy_state_db_without_cwd(&state_db).await;

        let first = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(first.migrated.len(), 1, "{first:?}");
        assert_eq!(first.migrated[0].source_db, state_db);
        let target = registered_project_target(&profile_root, &project).await;
        assert!(
            target
                .session_for_test(
                    HostAdmissionScope::Project,
                    "hermes",
                    "legacy-state-session",
                )
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            target
                .project_session_message_count_for_test()
                .await
                .unwrap(),
            1
        );
        assert!(
            fs::read_to_string(hermes.join("config.yaml"))
                .unwrap()
                .contains("project_root"),
            "the migration layer must leave the pin for lifecycle cutover"
        );

        drop(target);
        let second = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(second.already_migrated.len(), 1, "{second:?}");
        let target = registered_project_target(&profile_root, &project).await;
        assert_eq!(
            target
                .project_session_message_count_for_test()
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn failed_state_db_import_preserves_project_pin() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let hermes = user_home.join(".hermes");
        fs::create_dir_all(&hermes).unwrap();
        let config = format!(
            "plugins:\n  tracedecay:\n    project_root: {}\n",
            project.display()
        );
        fs::write(hermes.join("config.yaml"), &config).unwrap();
        fs::write(hermes.join("state.db"), b"not sqlite").unwrap();

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert_eq!(
            fs::read_to_string(hermes.join("config.yaml")).unwrap(),
            config
        );
    }

    #[tokio::test]
    async fn named_profile_upgrade_retries_in_place_without_default_cutover() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let legacy_profile = user_home.join(".hermes/profiles/work");
        let legacy_plugin = legacy_profile.join("plugins/tracedecay");
        fs::create_dir_all(&legacy_plugin).unwrap();
        fs::write(legacy_plugin.join("plugin.yaml"), "name: tracedecay\n").unwrap();
        let legacy_config = format!(
            "plugins:\n  enabled:\n    - tracedecay\n  tracedecay:\n    project_root: {}\n",
            project.display()
        );
        fs::write(legacy_profile.join("config.yaml"), &legacy_config).unwrap();
        seed_legacy_state_db_without_cwd(&legacy_profile.join("state.db")).await;

        let first = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(first.migrated.len(), 1, "{first:?}");

        let default_config = user_home.join(".hermes/config.yaml");
        fs::write(&default_config, "memory:\n  provider: other\n").unwrap();
        assert!(legacy_plugin.join("plugin.yaml").is_file());
        assert_eq!(
            fs::read_to_string(legacy_profile.join("config.yaml")).unwrap(),
            legacy_config
        );
        assert!(
            !user_home
                .join(".hermes/plugins/tracedecay/plugin.yaml")
                .exists()
        );

        let retry_migration = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(
            retry_migration.already_migrated.len(),
            1,
            "{retry_migration:?}"
        );
        assert!(legacy_plugin.join("plugin.yaml").is_file());
        assert!(
            !user_home
                .join(".hermes/plugins/tracedecay/plugin.yaml")
                .exists()
        );

        let target = registered_project_target(&profile_root, &project).await;
        assert_eq!(
            target
                .project_session_message_count_for_test()
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn same_content_memory_fact_merges_trust_and_feedback_once() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let hermes = user_home.join(".hermes");
        let source_sessions = hermes.join(".tracedecay/sessions.db");
        let source_memory = hermes.join(".tracedecay/tracedecay.db");
        fs::create_dir_all(source_sessions.parent().unwrap()).unwrap();
        fs::write(
            hermes.join("config.yaml"),
            format!(
                "plugins:\n  tracedecay:\n    project_root: {}\n",
                project.display()
            ),
        )
        .unwrap();
        seed_source(&source_sessions, &[("session", &project)]).await;
        let source_fact_id = seed_memory_fact(&source_memory, "shared durable fact").await;
        let source_runtime = HermesMigrationTestRuntime::create(&source_memory).await;
        source_runtime
            .record_memory_feedback(FeedbackRequest {
                fact_id: source_fact_id,
                action: FeedbackAction::Helpful,
                source: Some("legacy-hermes".to_string()),
                note: Some("source evidence".to_string()),
            })
            .await;
        drop(source_runtime);

        tracedecay_runtime_core::storage::PrivateStoreIo::create_dir_all(&profile_root).unwrap();
        let layout =
            tracedecay_runtime_core::storage::resolve_layout(&project, &profile_root).unwrap();
        let target_runtime = HermesMigrationTestRuntime::create(&layout.graph_db_path).await;
        let target_fact_id = target_runtime
            .add_memory_fact_request(AddFactRequest {
                content: "shared durable fact".to_string(),
                category: MemoryCategory::Project,
                source: Some("target".to_string()),
                tags: vec!["target".to_string()],
                entities: vec!["Target".to_string()],
                trust: Some(0.2),
                metadata: serde_json::json!({"target": true}),
            })
            .await;
        target_runtime
            .record_memory_feedback(FeedbackRequest {
                fact_id: target_fact_id,
                action: FeedbackAction::Unhelpful,
                source: Some("target".to_string()),
                note: Some("target evidence".to_string()),
            })
            .await;
        drop(target_runtime);

        let first = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(first.migrated.len(), 1, "{first:?}");
        let facts = immutable_memory_facts(&layout.graph_db_path).await;
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].3, 1);
        assert_eq!(facts[0].4, 1);
        assert!(facts[0].1.contains("legacy"));
        assert!(facts[0].1.contains("target"));
        assert_eq!(
            immutable_source_count(
                &layout.graph_db_path,
                HermesFixtureTable::MemoryFeedbackEvents
            )
            .await,
            2
        );

        let second = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(second.already_migrated.len(), 1, "{second:?}");
        assert_eq!(
            immutable_source_count(
                &layout.graph_db_path,
                HermesFixtureTable::MemoryFeedbackEvents
            )
            .await,
            2
        );
        let facts = immutable_memory_facts(&layout.graph_db_path).await;
        assert_eq!(facts[0].3, 1);
        assert_eq!(facts[0].4, 1);
    }

    #[tokio::test]
    async fn conflicting_existing_message_blocks_migration() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let hermes = user_home.join(".hermes");
        let source = hermes.join(".tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(
            hermes.join("config.yaml"),
            format!(
                "plugins:\n  tracedecay:\n    project_root: {}\n",
                project.display()
            ),
        )
        .unwrap();
        seed_source(&source, &[("session-1", &project)]).await;

        let target = registered_project_target(&profile_root, &project).await;
        seed_registered_session(
            &target,
            HostAdmissionScope::Project,
            &project,
            "session-1",
            "legacy",
            "conflicting target content",
        )
        .await;
        drop(target);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert!(report.failed[0].reason.contains("conflicts"));
    }

    #[tokio::test]
    async fn nonidentical_session_identity_collision_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let hermes = user_home.join(".hermes");
        let source = hermes.join(".tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(
            hermes.join("config.yaml"),
            format!(
                "plugins:\n  tracedecay:\n    project_root: {}\n",
                project.display()
            ),
        )
        .unwrap();
        seed_source(&source, &[("session-1", &project)]).await;

        let target = registered_project_target(&profile_root, &project).await;
        seed_registered_session(
            &target,
            HostAdmissionScope::Project,
            &project,
            "session-1",
            "different target title",
            "keep this",
        )
        .await;
        drop(target);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert!(report.failed[0].reason.contains("collides"));
        assert!(report.failed[0].reason.contains("sessions"));
    }

    #[tokio::test]
    async fn ambiguous_metadata_is_preserved_and_reported() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        mark_real_project(&first);
        mark_real_project(&second);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("first", &first), ("second", &second)]).await;

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.unresolved.len(), 1, "{report:?}");
        assert!(report.unresolved[0].reason.contains("ambiguous"));
        assert_eq!(
            immutable_source_count(&source, HermesFixtureTable::Sessions).await,
            2
        );
        assert!(
            !tracedecay_runtime_core::storage::resolve_layout(&first, &profile_root)
                .unwrap()
                .sessions_db_path
                .exists()
        );
    }

    #[tokio::test]
    async fn one_unpinned_metadata_project_is_migrated() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &project)]).await;

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.migrated.len(), 1, "{report:?}");
        assert_eq!(
            report.migrated[0].target_project,
            project.canonicalize().unwrap()
        );
    }

    #[tokio::test]
    async fn moved_pinned_project_resolves_through_registered_alias() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let legacy_project = temp.path().join("project-before-move");
        let current_project = temp.path().join("project-after-move");
        mark_real_project(&legacy_project);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(
            user_home.join(".hermes/config.yaml"),
            format!(
                "plugins:\n  tracedecay:\n    project_root: {}\n",
                legacy_project.display()
            ),
        )
        .unwrap();
        seed_source(&source, &[("session", &legacy_project)]).await;

        fs::create_dir_all(&profile_root).unwrap();
        let registry = registered_profile_target(&profile_root).await;
        registry
            .profile_registry()
            .upsert_code_project("stable-project", &legacy_project, None, None, None)
            .await
            .unwrap();
        fs::rename(&legacy_project, &current_project).unwrap();
        registry
            .profile_registry()
            .upsert_code_project("stable-project", &current_project, None, None, None)
            .await
            .unwrap();
        write_project_enrollment(&current_project, "stable-project");
        drop(registry);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

        assert_eq!(report.migrated.len(), 1, "{report:?}");
        assert_eq!(
            report.migrated[0].target_project,
            current_project.canonicalize().unwrap()
        );
        let target =
            registered_project_target_with_id(&profile_root, &current_project, "stable-project")
                .await;
        assert!(
            target
                .session_for_test(HostAdmissionScope::Project, "hermes", "session")
                .await
                .unwrap()
                .is_some()
        );
        let path_hash_layout = tracedecay_runtime_core::storage::default_profile_sharded_layout(
            &current_project,
            &profile_root,
        )
        .unwrap();
        assert!(
            !path_hash_layout.sessions_db_path.exists(),
            "migration must not create a second path-hash shard"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn moved_project_resolves_through_canonicalized_missing_parent_alias() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let physical_parent = temp.path().join("physical");
        let alias_parent = temp.path().join("alias");
        fs::create_dir_all(&physical_parent).unwrap();
        std::os::unix::fs::symlink(&physical_parent, &alias_parent).unwrap();
        let legacy_alias = alias_parent.join("project-before-move");
        let legacy_physical = physical_parent.join("project-before-move");
        let current_project = physical_parent.join("project-after-move");
        mark_real_project(&legacy_physical);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(
            user_home.join(".hermes/config.yaml"),
            format!(
                "plugins:\n  tracedecay:\n    project_root: {}\n",
                legacy_alias.display()
            ),
        )
        .unwrap();
        seed_source(&source, &[("session", &legacy_alias)]).await;

        fs::create_dir_all(&profile_root).unwrap();
        let registry = registered_profile_target(&profile_root).await;
        registry
            .profile_registry()
            .upsert_code_project("stable-project", &legacy_physical, None, None, None)
            .await
            .unwrap();
        fs::rename(&legacy_physical, &current_project).unwrap();
        registry
            .profile_registry()
            .upsert_code_project("stable-project", &current_project, None, None, None)
            .await
            .unwrap();
        write_project_enrollment(&current_project, "stable-project");
        drop(registry);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

        assert_eq!(report.migrated.len(), 1, "{report:?}");
        assert_eq!(
            report.migrated[0].target_project,
            current_project.canonicalize().unwrap()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn removed_unprovable_symlink_metadata_is_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let legacy_project = temp.path().join("project-before-move");
        let project_alias = temp.path().join("project-link");
        let current_project = temp.path().join("project-after-move");
        mark_real_project(&legacy_project);
        std::os::unix::fs::symlink(&legacy_project, &project_alias).unwrap();
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &legacy_project)]).await;
        let source_runtime = HermesMigrationTestRuntime::create(&source).await;
        source_runtime
            .set_session_project_path("session", &project_alias)
            .await;
        drop(source_runtime);

        fs::create_dir_all(&profile_root).unwrap();
        let registry = registered_profile_target(&profile_root).await;
        registry
            .profile_registry()
            .upsert_code_project("stable-project", &project_alias, None, None, None)
            .await
            .unwrap();
        fs::remove_file(&project_alias).unwrap();
        fs::rename(&legacy_project, &current_project).unwrap();
        registry
            .profile_registry()
            .upsert_code_project("stable-project", &current_project, None, None, None)
            .await
            .unwrap();
        write_project_enrollment(&current_project, "stable-project");
        drop(registry);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

        assert!(report.migrated.is_empty(), "{report:?}");
        assert_eq!(report.unresolved.len(), 1, "{report:?}");
        assert!(
            !profile_root
                .join("projects/stable-project/sessions.db")
                .is_file()
        );
    }

    #[tokio::test]
    async fn migrates_profile_shard_misidentified_as_hermes_project() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let hermes = user_home.join(".hermes");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let legacy_shard = profile_root.join("projects/legacy-hermes-identity");
        let source = legacy_shard.join(tracedecay_runtime_core::storage::SESSIONS_DB_FILENAME);
        fs::create_dir_all(&legacy_shard).unwrap();
        let manifest = tracedecay_runtime_core::storage::StoreManifest {
            schema_version: tracedecay_runtime_core::storage::STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some("legacy-hermes-identity".into()),
            store_kind: tracedecay_runtime_core::storage::StoreKind::CodeProject,
            storage_mode: tracedecay_runtime_core::storage::StorageMode::ProfileSharded,
            project_root: hermes.clone(),
            data_root: legacy_shard.clone(),
            graph_db_relpath: PathBuf::from("tracedecay.db"),
            sessions_db_relpath: PathBuf::from(
                tracedecay_runtime_core::storage::SESSIONS_DB_FILENAME,
            ),
            branch_meta_relpath: PathBuf::from(
                tracedecay_runtime_core::storage::BRANCH_META_FILENAME,
            ),
        };
        fs::write(
            legacy_shard.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let registry = registered_profile_target(&profile_root).await;
        registry
            .profile_registry()
            .upsert_code_project("legacy-hermes-identity", &hermes, None, None, None)
            .await
            .unwrap();
        seed_source(&source, &[("session", &project)]).await;
        drop(registry);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.migrated.len(), 1, "{report:?}");
        assert_eq!(report.migrated[0].source_db, source);
        assert_eq!(
            immutable_source_count(&source, HermesFixtureTable::Sessions).await,
            1
        );
        let target_layout =
            tracedecay_runtime_core::storage::resolve_layout(&project, &profile_root).unwrap();
        assert_ne!(target_layout.sessions_db_path, source);
        let target = registered_project_target(&profile_root, &project).await;
        assert!(
            target
                .session_for_test(HostAdmissionScope::Project, "hermes", "session")
                .await
                .unwrap()
                .is_some()
        );
        assert!(source.is_file());
        drop(target);
        let registry = registered_profile_target(&profile_root).await;
        assert!(
            registry
                .profile_registry()
                .get_code_project("legacy-hermes-identity")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn migrates_hermes_owned_profile_shard_sessions_to_user_and_cleans_registry() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let hermes = user_home.join(".hermes");
        let legacy_shard = profile_root.join("projects/legacy-hermes-projectless");
        let source = legacy_shard.join(tracedecay_runtime_core::storage::SESSIONS_DB_FILENAME);
        fs::create_dir_all(&legacy_shard).unwrap();
        let manifest = tracedecay_runtime_core::storage::StoreManifest {
            schema_version: tracedecay_runtime_core::storage::STORE_MANIFEST_SCHEMA_VERSION,
            project_id: Some("legacy-hermes-projectless".into()),
            store_kind: tracedecay_runtime_core::storage::StoreKind::CodeProject,
            storage_mode: tracedecay_runtime_core::storage::StorageMode::ProfileSharded,
            project_root: hermes.clone(),
            data_root: legacy_shard.clone(),
            graph_db_relpath: PathBuf::from("tracedecay.db"),
            sessions_db_relpath: PathBuf::from(
                tracedecay_runtime_core::storage::SESSIONS_DB_FILENAME,
            ),
            branch_meta_relpath: PathBuf::from(
                tracedecay_runtime_core::storage::BRANCH_META_FILENAME,
            ),
        };
        fs::write(
            legacy_shard.join(tracedecay_runtime_core::storage::STORE_MANIFEST_FILENAME),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let registry = registered_profile_target(&profile_root).await;
        registry
            .profile_registry()
            .upsert_code_project("legacy-hermes-projectless", &hermes, None, None, None)
            .await
            .unwrap();
        seed_source(&source, &[("session", &hermes)]).await;
        drop(registry);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.migrated.len(), 1, "{report:?}");
        assert!(report.failed.is_empty(), "{report:?}");
        assert_eq!(report.migrated[0].source_db, source);
        assert_eq!(report.migrated[0].target_project, Path::new("user"));
        let target_path =
            tracedecay_runtime_core::store_runtime::profile_paths::user_sessions_db_path(
                &profile_root,
            );
        let target = registered_profile_target(&profile_root).await;
        let session = target
            .session_for_test(HostAdmissionScope::Profile, "hermes", "session")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.project_key, "user");
        assert_eq!(session.project_path, "user");
        assert_eq!(
            immutable_source_count(&source, HermesFixtureTable::Sessions).await,
            1
        );
        assert_eq!(
            immutable_source_count(&source, HermesFixtureTable::SessionMessages).await,
            1
        );
        assert_eq!(marker_count(&target_path), 1);
        assert!(source.is_file());
        drop(target);
        let registry = registered_profile_target(&profile_root).await;
        assert!(
            registry
                .profile_registry()
                .get_code_project("legacy-hermes-projectless")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn migrates_older_source_with_missing_current_columns_and_lcm_tables() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        HermesMigrationTestRuntime::create_older_session_source(&source, &project);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.migrated.len(), 1, "{report:?}");
        let target = registered_project_target(&profile_root, &project).await;
        let counts = target
            .transcript_store_counts_for_test(
                HostAdmissionScope::Project,
                "hermes",
                "old-session",
                Path::new("unused"),
            )
            .await
            .unwrap();
        assert_eq!((counts.0, counts.1, counts.2), (1, 1, 0));
    }

    #[tokio::test]
    async fn projectless_profile_sessions_migrate_to_user_store_idempotently() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let hermes = user_home.join(".hermes");
        let source = hermes.join(".tracedecay/sessions.db");
        let source_memory = hermes.join(".tracedecay/tracedecay.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        let source_fact_id = seed_memory_fact(&source_memory, "unscoped legacy fact").await;
        seed_source(&source, &[("session", &hermes)]).await;

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.migrated.len(), 1, "{report:?}");
        assert_eq!(report.unresolved.len(), 1, "{report:?}");
        assert_eq!(report.unresolved[0].source_db, source_memory);
        assert!(report.unresolved[0].reason.contains("preserved"));
        let target = registered_profile_target(&profile_root).await;
        let session = target
            .session_for_test(HostAdmissionScope::Profile, "hermes", "session")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.project_path, "user");
        assert!(
            !tracedecay_runtime_core::memory::user::user_memory_db_path(&profile_root).exists()
        );
        assert_eq!(
            immutable_source_count(&source, HermesFixtureTable::Sessions).await,
            1
        );
        let source_facts = immutable_memory_facts(&source_memory).await;
        assert_eq!(source_facts.len(), 1);
        assert_eq!(source_facts[0].0, "unscoped legacy fact");
        assert!(source_fact_id > 0);

        drop(target);
        let retry = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(retry.already_migrated.len(), 1, "{retry:?}");
        assert_eq!(retry.unresolved.len(), 1, "{retry:?}");
        let target = registered_profile_target(&profile_root).await;
        assert!(
            target
                .session_for_test(HostAdmissionScope::Profile, "hermes", "session")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn malformed_metadata_is_preserved_not_misrouted_to_user() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &user_home)]).await;
        let source_runtime = HermesMigrationTestRuntime::create(&source).await;
        source_runtime
            .set_session_metadata_without_project("session", "{invalid")
            .await;
        drop(source_runtime);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

        assert_eq!(report.unresolved.len(), 1, "{report:?}");
        assert!(report.migrated.is_empty());
        assert!(
            !tracedecay_runtime_core::store_runtime::profile_paths::user_sessions_db_path(
                &profile_root
            )
            .exists()
        );
    }

    #[tokio::test]
    async fn structurally_invalid_metadata_is_preserved_not_misrouted_to_user() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &user_home)]).await;
        let source_runtime = HermesMigrationTestRuntime::create(&source).await;
        source_runtime
            .set_session_metadata_without_project("session", "{\"project_root\":42}")
            .await;
        drop(source_runtime);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

        assert_eq!(report.unresolved.len(), 1, "{report:?}");
        assert!(report.migrated.is_empty());
        assert!(
            !tracedecay_runtime_core::store_runtime::profile_paths::user_sessions_db_path(
                &profile_root
            )
            .exists()
        );
    }

    #[tokio::test]
    async fn vanished_hermes_owned_path_is_preserved_as_unresolved() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let vanished_project = user_home.join(".hermes/plugins/vanished-project");
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &vanished_project)]).await;

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert!(report.migrated.is_empty(), "{report:?}");
        assert_eq!(report.unresolved.len(), 1, "{report:?}");
        assert!(
            !tracedecay_runtime_core::store_runtime::profile_paths::user_sessions_db_path(
                &profile_root
            )
            .exists()
        );
        assert_eq!(
            immutable_source_count(&source, HermesFixtureTable::Sessions).await,
            1
        );
    }

    #[tokio::test]
    async fn durable_project_under_hermes_home_remains_project_scoped() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = user_home.join(".hermes/workspaces/real-project");
        mark_real_project(&project);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &project)]).await;

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.migrated.len(), 1, "{report:?}");
        assert!(report.unresolved.is_empty(), "{report:?}");
        assert_eq!(
            report.migrated[0].target_project,
            project.canonicalize().unwrap()
        );
        assert!(
            !tracedecay_runtime_core::store_runtime::profile_paths::user_sessions_db_path(
                &profile_root
            )
            .exists()
        );
    }

    #[tokio::test]
    async fn existing_unregistered_directory_is_not_assumed_projectless() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let unregistered = temp.path().join("unregistered-project");
        fs::create_dir_all(&unregistered).unwrap();
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &unregistered)]).await;

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.unresolved.len(), 1, "{report:?}");
        assert!(report.migrated.is_empty());
        assert!(
            !tracedecay_runtime_core::store_runtime::profile_paths::user_sessions_db_path(
                &profile_root
            )
            .exists()
        );
    }

    #[tokio::test]
    async fn same_session_resolved_and_unresolved_projects_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        let vanished = temp.path().join("vanished-project");
        mark_real_project(&project);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &project)]).await;
        let source_runtime = HermesMigrationTestRuntime::create(&source).await;
        source_runtime
            .set_session_project_path("session", &vanished)
            .await;
        drop(source_runtime);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;

        assert_eq!(report.unresolved.len(), 1, "{report:?}");
        assert!(report.migrated.is_empty());
        let layout =
            tracedecay_runtime_core::storage::resolve_layout(&project, &profile_root).unwrap();
        assert!(!layout.sessions_db_path.exists());
    }

    #[tokio::test]
    async fn mixed_user_and_project_sessions_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(
            &source,
            &[("user-session", &user_home), ("project-session", &project)],
        )
        .await;

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.unresolved.len(), 1, "{report:?}");
        assert!(report.unresolved[0].reason.contains("ambiguous"));
        assert!(
            !tracedecay_runtime_core::store_runtime::profile_paths::user_sessions_db_path(
                &profile_root
            )
            .exists()
        );
        let project_layout =
            tracedecay_runtime_core::storage::resolve_layout(&project, &profile_root).unwrap();
        assert!(!project_layout.sessions_db_path.exists());
    }

    #[tokio::test]
    async fn future_source_schema_is_rejected_without_target_changes() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &project)]).await;
        let source_runtime = HermesMigrationTestRuntime::create(&source).await;
        source_runtime
            .set_lcm_schema_version(crate::root_seam::sessions::lcm::LCM_SCHEMA_VERSION + 1)
            .await;
        drop(source_runtime);

        let report = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(report.failed.len(), 1, "{report:?}");
        assert!(report.failed[0].reason.contains("newer"));
        assert!(
            !tracedecay_runtime_core::storage::resolve_layout(&project, &profile_root)
                .unwrap()
                .sessions_db_path
                .exists()
        );
    }

    #[tokio::test]
    async fn injected_failure_rolls_back_and_retry_converges() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("home");
        let profile_root = temp.path().join("tracedecay-profile");
        let project = temp.path().join("project");
        mark_real_project(&project);
        let source = user_home.join(".hermes/.tracedecay/sessions.db");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        seed_source(&source, &[("session", &project)]).await;
        let payload_ref = "migration-payload";
        let payload = b"legacy payload";
        let source_payload_dir = source.parent().unwrap().join("lcm-payloads");
        fs::create_dir_all(&source_payload_dir).unwrap();
        fs::write(source_payload_dir.join(payload_ref), payload).unwrap();
        let source_runtime = HermesMigrationTestRuntime::create(&source).await;
        source_runtime
            .insert_external_payload(payload_ref, payload)
            .await;
        drop(source_runtime);
        let layout =
            tracedecay_runtime_core::storage::resolve_layout(&project, &profile_root).unwrap();
        let target = registered_project_target(&profile_root, &project).await;
        assert!(
            target
                .session_for_test(HostAdmissionScope::Project, "hermes", "session")
                .await
                .unwrap()
                .is_none()
        );
        drop(target);

        let failed = migrate_legacy_hermes_stores_inner(
            &user_home,
            &profile_root,
            &[user_home.join(".hermes")],
            Some("sessions"),
        )
        .await;
        assert_eq!(failed.failed.len(), 1, "{failed:?}");
        let target = registered_project_target(&profile_root, &project).await;
        assert!(
            target
                .session_for_test(HostAdmissionScope::Project, "hermes", "session")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(marker_count(&layout.sessions_db_path), 0);
        let target_payload = layout
            .sessions_db_path
            .parent()
            .unwrap()
            .join("lcm-payloads")
            .join(payload_ref);
        assert!(!target_payload.exists());
        drop(target);
        assert_eq!(
            immutable_source_count(&source, HermesFixtureTable::Sessions).await,
            1
        );

        let retry = migrate_legacy_hermes_stores_to(&user_home, &profile_root).await;
        assert_eq!(retry.migrated.len(), 1, "{retry:?}");
        assert_eq!(fs::read(target_payload).unwrap(), payload);
    }

    #[tokio::test]
    async fn memory_merge_waits_for_the_shared_writer_lane() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source-memory.db");
        let target_path = temp.path().join("target-memory.db");
        seed_memory_fact(&source_path, "legacy fact").await;
        HermesMigrationTestRuntime::assert_memory_merge_waits_for_writer(
            &source_path,
            &target_path,
        )
        .await;
    }

    #[test]
    fn single_legacy_home_profile_scan_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let standard = temp.path().join("home/.hermes");
        fs::create_dir_all(standard.join("profiles/alpha")).unwrap();
        let profiles = legacy_profile_dirs(&standard);
        assert_eq!(
            profiles,
            vec![standard.clone(), standard.join("profiles/alpha")]
        );
    }
}
