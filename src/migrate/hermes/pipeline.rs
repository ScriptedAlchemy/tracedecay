//! Per-candidate migration pipeline: verify, resolve, snapshot, and merge.

use std::path::{Path, PathBuf};

use tracedecay_domain::ProjectId;

use super::candidates::{CandidateError, CandidateOutcome, LegacyStoreCandidate};
use super::copy::{quote_identifier, table_columns};
use super::fingerprint::logical_source_fingerprint;
use super::memory::merge_memory_snapshot;
use super::resolution::{ResolvedTargetProject, resolve_target_project, same_path};
use super::session_merge::{MergeSnapshotRequest, merge_snapshot};
use crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1;
use crate::db::engine::{QueryExecutor, params};
use crate::global_db::RegisteredGlobalDb;
use crate::migrate::hermes::{LegacyHermesMigration, LegacyHermesMigrationIssue};
use crate::sqlite_read_snapshot::{SnapshotConnection, SnapshotDatabase};

pub(crate) async fn verify_source<Q>(source: &Q) -> Result<(), String>
where
    Q: QueryExecutor + ?Sized,
{
    let mut rows = source
        .query("PRAGMA quick_check", ())
        .await
        .map_err(|error| format!("source quick_check failed: {error}"))?;
    let result = rows
        .next()
        .await
        .map_err(|error| format!("source quick_check could not be read: {error}"))?
        .and_then(|row| row.get::<String>(0).ok())
        .unwrap_or_default();
    if result == "ok" {
        Ok(())
    } else {
        Err(format!("source quick_check reported: {result}"))
    }
}

async fn source_lcm_schema_version<Q>(source: &Q) -> Result<i64, String>
where
    Q: QueryExecutor + ?Sized,
{
    if table_columns(source, "session_schema_migrations")
        .await?
        .is_empty()
    {
        return Ok(0);
    }
    let mut rows = source
        .query(
            "SELECT version FROM session_schema_migrations WHERE name = 'lcm'",
            (),
        )
        .await
        .map_err(|error| format!("could not inspect source schema: {error}"))?;
    match rows
        .next()
        .await
        .map_err(|error| format!("could not read source schema: {error}"))?
    {
        Some(row) => row
            .get(0)
            .map_err(|error| format!("invalid source schema version: {error}")),
        None => Ok(0),
    }
}

struct ResolvedTargetLayout {
    sessions_db_path: PathBuf,
    graph_db_path: Option<PathBuf>,
    project_id: String,
}

fn project_layout(
    layout: crate::storage::StoreLayout,
) -> crate::errors::Result<ResolvedTargetLayout> {
    let project_id = layout.identity.project_id.clone().ok_or_else(|| {
        crate::errors::TraceDecayError::Config {
            message: "target project shard has no durable project id".to_string(),
        }
    })?;
    Ok(ResolvedTargetLayout {
        sessions_db_path: layout.sessions_db_path,
        graph_db_path: Some(layout.graph_db_path),
        project_id,
    })
}

async fn resolve_target_layout(
    target_project: &ResolvedTargetProject,
    tracedecay_profile_root: &Path,
) -> crate::errors::Result<ResolvedTargetLayout> {
    if target_project.user_scope {
        return Ok(ResolvedTargetLayout {
            sessions_db_path: crate::sessions::user_sessions_db_path(tracedecay_profile_root),
            graph_db_path: None,
            project_id: "user".to_string(),
        });
    }
    if let Some(project_id) = target_project.registry_project_id.as_deref() {
        if let Some(layout) =
            crate::storage::resolve_persisted_layout(&target_project.root, tracedecay_profile_root)?
        {
            if layout.identity.project_id.as_deref() != Some(project_id) {
                return Err(crate::errors::TraceDecayError::Config {
                    message: format!(
                        "registered project identity collision for '{}': registry has '{project_id}', repository has '{}'",
                        target_project.root.display(),
                        layout.identity.project_id.as_deref().unwrap_or("none")
                    ),
                });
            }
            return project_layout(layout);
        }
        return project_layout(crate::storage::profile_sharded_layout(
            &target_project.root,
            tracedecay_profile_root,
            &crate::storage::EnrollmentMarker {
                project_id: project_id.to_string(),
                storage_mode: crate::storage::StorageMode::ProfileSharded,
            },
        )?);
    }

    let production_profile = crate::storage::default_profile_root()
        .is_ok_and(|default| same_path(&default, tracedecay_profile_root));
    let layout = if production_profile {
        crate::tracedecay::TraceDecay::resolve_store_layout_for_identity(&target_project.root).await
    } else {
        crate::storage::resolve_layout(&target_project.root, tracedecay_profile_root)
    }?;
    project_layout(layout)
}

async fn ensure_message_identity_matches<S, T>(
    source: &S,
    target: &T,
    table: &str,
    content_column: &str,
) -> Result<(), String>
where
    S: QueryExecutor + ?Sized,
    T: QueryExecutor + ?Sized,
{
    let columns = table_columns(source, table).await?;
    if !["provider", "message_id", content_column]
        .iter()
        .all(|required| columns.iter().any(|column| column == required))
    {
        return Ok(());
    }
    let table = quote_identifier(table);
    let content_column = quote_identifier(content_column);
    let mut rows = source
        .query(
            &format!(
                "SELECT provider, message_id, {content_column} FROM {table} ORDER BY provider, message_id"
            ),
            (),
        )
        .await
        .map_err(|error| format!("could not inspect legacy message identities: {error}"))?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| format!("could not read legacy message identity: {error}"))?
    {
        let provider = row
            .get::<String>(0)
            .map_err(|error| format!("invalid legacy message provider: {error}"))?;
        let message_id = row
            .get::<String>(1)
            .map_err(|error| format!("invalid legacy message id: {error}"))?;
        let source_content = row
            .get::<String>(2)
            .map_err(|error| format!("invalid legacy message content identity: {error}"))?;
        let mut target_rows = target
            .query(
                &format!(
                    "SELECT {content_column} FROM {table} WHERE provider = ?1 AND message_id = ?2"
                ),
                params![provider.as_str(), message_id.as_str()],
            )
            .await
            .map_err(|error| format!("could not inspect target message identity: {error}"))?;
        let Some(target_row) = target_rows
            .next()
            .await
            .map_err(|error| format!("could not read target message identity: {error}"))?
        else {
            continue;
        };
        let target_content = target_row
            .get::<String>(0)
            .map_err(|error| format!("invalid target message content identity: {error}"))?;
        if target_content != source_content {
            return Err(format!(
                "legacy {table} identity ({provider}, {message_id}) conflicts with target content"
            ));
        }
    }
    Ok(())
}

async fn migrate_candidate_snapshot(
    user_home: &Path,
    hermes_homes: &[PathBuf],
    candidate: &LegacyStoreCandidate,
    source: Option<&SnapshotConnection>,
    tracedecay_profile_root: &Path,
    session_registry: &DaemonSessionRuntimeRegistryV1,
    profile_registry: &RegisteredGlobalDb,
    fail_after_table: Option<&str>,
) -> Result<CandidateOutcome, CandidateError> {
    if let Some(source) = source {
        verify_source(source)
            .await
            .map_err(CandidateError::Failed)?;
    }
    let source_schema_version = match source {
        Some(source) => source_lcm_schema_version(source)
            .await
            .map_err(CandidateError::Failed)?,
        None => 0,
    };
    if source_schema_version > crate::sessions::lcm::LCM_SCHEMA_VERSION {
        return Err(CandidateError::Failed(format!(
            "source LCM schema {source_schema_version} is newer than supported schema {}",
            crate::sessions::lcm::LCM_SCHEMA_VERSION
        )));
    }

    let target_project = resolve_target_project(
        source,
        Some(profile_registry),
        &candidate.profile_dir.join("config.yaml"),
        user_home,
        hermes_homes,
    )
    .await
    .map_err(CandidateError::Unresolved)?;
    let preserved_memory = if target_project.user_scope {
        candidate
            .source_memory_db
            .as_ref()
            .map(|source_db| LegacyHermesMigrationIssue {
                source_db: source_db.clone(),
                reason: "unscoped legacy memory was preserved because no durable project attribution exists"
                    .to_string(),
            })
    } else {
        None
    };
    let target_layout = resolve_target_layout(&target_project, tracedecay_profile_root)
        .await
        .map_err(|error| {
            CandidateError::Failed(format!("could not resolve target profile shard: {error}"))
        })?;
    if candidate
        .source_sessions_db
        .as_deref()
        .is_some_and(|source_path| same_path(source_path, &target_layout.sessions_db_path))
    {
        return Err(CandidateError::Failed(
            "source and target session databases resolve to the same path".to_string(),
        ));
    }
    if candidate
        .source_memory_db
        .as_deref()
        .zip(target_layout.graph_db_path.as_deref())
        .is_some_and(|(source_path, target_path)| same_path(source_path, target_path))
    {
        return Err(CandidateError::Failed(
            "source and target memory databases resolve to the same path".to_string(),
        ));
    }
    // Projectless sessions are safe to retain in the profile user-session
    // store. Legacy memory facts are not: without a project pin or durable
    // session attribution their scope cannot be proven, so leave that source
    // database untouched for a later explicit recovery.
    let source_memory_db: Option<SnapshotDatabase> = match candidate
        .source_memory_db
        .as_deref()
        .filter(|_| !target_project.user_scope)
    {
        Some(path) => Some(
            crate::sqlite_read_snapshot::open_in(path, tracedecay_profile_root)
                .await
                .map_err(|error| {
                    CandidateError::Failed(format!(
                        "could not snapshot legacy memory store '{}': {error}",
                        path.display()
                    ))
                })?,
        ),
        None => None,
    };
    let source_memory = source_memory_db.as_ref().map(SnapshotDatabase::connection);
    let fingerprint = logical_source_fingerprint(
        source,
        candidate.primary_path(),
        source_memory.zip(candidate.source_memory_db.as_deref()),
    )
    .await
    .map_err(CandidateError::Failed)?;
    let target_project_id = ProjectId::new(target_layout.project_id.clone()).map_err(|error| {
        CandidateError::Failed(format!("invalid target project identity: {error}"))
    })?;
    let target_db = if target_project.user_scope {
        session_registry.profile_sessions().await
    } else {
        session_registry
            .project_sessions(target_project_id.clone(), [target_project.root.clone()])
            .await
    }
    .map_err(|error| {
        CandidateError::Failed(format!("could not mount target session store: {error}"))
    })?;
    if !same_path(target_db.db_path(), &target_layout.sessions_db_path) {
        return Err(CandidateError::Failed(
            "registered target session store does not match resolved migration target".to_string(),
        ));
    }
    if let Some(source) = source {
        let target_snapshot = target_db.read_snapshot().await.map_err(|error| {
            CandidateError::Failed(format!("could not snapshot target session store: {error}"))
        })?;
        ensure_message_identity_matches(source, &target_snapshot, "session_messages", "text")
            .await
            .map_err(CandidateError::Failed)?;
        ensure_message_identity_matches(
            source,
            &target_snapshot,
            "lcm_raw_messages",
            "content_hash",
        )
        .await
        .map_err(CandidateError::Failed)?;
    }
    let memory_rows = match source_memory {
        Some(source_memory) => {
            let graph_db_path = target_layout.graph_db_path.as_deref().ok_or_else(|| {
                CandidateError::Failed("project memory target disappeared".to_string())
            })?;
            let memory_db = session_registry
                .project_memory(target_project_id.clone(), [target_project.root.clone()])
                .await
                .map_err(|error| {
                    CandidateError::Failed(format!("could not mount target memory store: {error}"))
                })?;
            if !same_path(memory_db.database_path(), graph_db_path) {
                return Err(CandidateError::Failed(
                    "registered target memory store does not match resolved migration target"
                        .to_string(),
                ));
            }
            merge_memory_snapshot(source_memory, memory_db.as_ref())
                .await
                .map_err(CandidateError::Failed)?
        }
        None => 0,
    };

    let result = merge_snapshot(
        &target_db,
        MergeSnapshotRequest {
            source,
            source_path: candidate.primary_path(),
            target_path: target_db.db_path(),
            target_project: &target_project.root,
            target_project_id: &target_layout.project_id,
            fingerprint: &fingerprint,
            source_schema_version,
            initial_rows_copied: memory_rows,
            fail_after_table,
        },
    )
    .await
    .map_err(CandidateError::Failed)?;
    let migration = LegacyHermesMigration {
        source_db: candidate.primary_path().to_path_buf(),
        target_project: target_project.root,
        rows_copied: result.rows_copied,
    };
    Ok(if result.already_migrated {
        CandidateOutcome::AlreadyMigrated(migration, preserved_memory)
    } else {
        CandidateOutcome::Migrated(migration, preserved_memory)
    })
}

pub(crate) async fn migrate_legacy_state_store(
    user_home: &Path,
    hermes_homes: &[PathBuf],
    profile_dir: &Path,
    tracedecay_profile_root: &Path,
    session_registry: &DaemonSessionRuntimeRegistryV1,
    profile_registry: &RegisteredGlobalDb,
) -> Result<CandidateOutcome, CandidateError> {
    let state_db = profile_dir.join("state.db");
    let target_project = resolve_target_project::<SnapshotConnection>(
        None,
        Some(profile_registry),
        &profile_dir.join("config.yaml"),
        user_home,
        hermes_homes,
    )
    .await
    .map_err(CandidateError::Unresolved)?;
    let target_layout = resolve_target_layout(&target_project, tracedecay_profile_root)
        .await
        .map_err(|error| {
            CandidateError::Failed(format!("could not resolve target profile shard: {error}"))
        })?;
    let project_id = ProjectId::new(target_layout.project_id).map_err(|error| {
        CandidateError::Failed(format!("invalid target project identity: {error}"))
    })?;
    let target = session_registry
        .project_sessions(project_id.clone(), [target_project.root.clone()])
        .await
        .map_err(|error| {
            CandidateError::Failed(format!("could not mount target session store: {error}"))
        })?;
    if !same_path(target.db_path(), &target_layout.sessions_db_path) {
        return Err(CandidateError::Failed(
            "registered target session store does not match resolved migration target".to_string(),
        ));
    }
    let shard = &target.binding().shard_id;
    let admission = crate::application::host_admission::HostAdmissionFacade::new(
        crate::application::host_admission::HostAdmissionAuthorities::for_project(
            shard.brain_id.clone(),
            shard.profile_id.clone(),
            project_id.clone(),
            target.as_ref(),
        ),
    );
    let stats = crate::sessions::hermes::ingest_legacy_pinned_profile(
        &admission,
        profile_dir,
        &target_project.root,
        project_id,
    )
    .await
    .map_err(CandidateError::Failed)?;
    let rows_copied = stats
        .sessions_upserted
        .saturating_add(stats.messages_upserted);
    let migration = LegacyHermesMigration {
        source_db: state_db,
        target_project: target_project.root,
        rows_copied,
    };
    Ok(if rows_copied == 0 {
        CandidateOutcome::AlreadyMigrated(migration, None)
    } else {
        CandidateOutcome::Migrated(migration, None)
    })
}

pub(crate) async fn migrate_candidate(
    user_home: &Path,
    hermes_homes: &[PathBuf],
    candidate: &LegacyStoreCandidate,
    tracedecay_profile_root: &Path,
    session_registry: &DaemonSessionRuntimeRegistryV1,
    profile_registry: &RegisteredGlobalDb,
    fail_after_table: Option<&str>,
) -> Result<CandidateOutcome, CandidateError> {
    let source_db = match candidate.source_sessions_db.as_deref() {
        Some(path) => Some(
            crate::sqlite_read_snapshot::open_in(path, tracedecay_profile_root)
                .await
                .map_err(|error| {
                    CandidateError::Failed(format!(
                        "could not snapshot legacy session store '{}': {error}",
                        path.display()
                    ))
                })?,
        ),
        None => None,
    };
    let source = source_db.as_ref().map(SnapshotDatabase::connection);

    migrate_candidate_snapshot(
        user_home,
        hermes_homes,
        candidate,
        source,
        tracedecay_profile_root,
        session_registry,
        profile_registry,
        fail_after_table,
    )
    .await
}
