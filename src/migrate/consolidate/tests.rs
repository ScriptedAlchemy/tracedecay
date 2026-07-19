use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use serde_json::json;
use tempfile::TempDir;
use tracedecay_domain::{
    ClaudeByteRangeV1, ClaudeFileGenerationV1, ClaudeObservationIdentityMaterialV1,
    ClaudeSourceCursorV1, ClaudeSourceIdentityV1, ComponentVersion, DurableClaudeObservationV1,
    ObservationScopeV1, PayloadReferenceV1, ProjectionGenerationId, RetentionClass,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1,
    SensitivityV1, SessionId, UtcMicros,
};
use tracedecay_store::observation::ObservationCoverageV1;
use tracedecay_store::{
    AnchoredObservationWrite, ObservationPersistOutcome, ObservationProjectionStore,
    ObservationStore, ObservationWrite, SESSION_MESSAGE_PROJECTOR_VERSION,
    build_observation_resolution_authorization_v1, build_observation_retrieval_anchor_v2,
};

use super::*;
use crate::db::{Database, DatabaseAuthority};
use crate::memory::store::MemoryStore;
use crate::memory::types::{
    AddFactRequest, FactRelationKind, FeedbackAction, FeedbackRequest, MemoryCategory,
};
use crate::sessions::{SessionMessageRecord, SessionRecord};
use crate::store::GlobalDbObservationStore;
use crate::tracedecay::{TraceDecay, TraceDecayOpenOptions};

mod lifecycle;
mod memory;
mod observation;
mod schema;
mod session_merge;
mod temporal;

async fn test_initialize(path: &Path) -> (Database, bool) {
    let authority = DatabaseAuthority::acquire_test(path, "consolidation test initialize").unwrap();
    Database::initialize(path, &authority).await.unwrap()
}

async fn test_open(path: &Path) -> (Database, bool) {
    let authority = DatabaseAuthority::acquire_test(path, "consolidation test open").unwrap();
    Database::open(path, &authority).await.unwrap()
}

async fn test_open_read_only(path: &Path) -> (Database, bool) {
    let authority = DatabaseAuthority::acquire_test(path, "consolidation test read").unwrap();
    Database::open_read_only(path, &authority).await.unwrap()
}

struct Fixture {
    _temp: TempDir,
    project: PathBuf,
    profile: PathBuf,
    source_id: String,
    target_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SnapshotEntry {
    Missing,
    File {
        digest: [u8; 32],
        bytes: u64,
        modified: SystemTime,
        #[cfg(unix)]
        device: u64,
        #[cfg(unix)]
        inode: u64,
        #[cfg(unix)]
        changed_seconds: i64,
        #[cfg(unix)]
        changed_nanoseconds: i64,
        #[cfg(unix)]
        links: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TreeSnapshotEntry {
    // Directory timestamps are derived state: creating and removing ignored
    // authority-lock artifacts changes their parent directories' mtime/ctime.
    // Topology, identity, permissions, and every non-ignored child remain
    // snapshotted, so persistent input mutations are still detected.
    Directory {
        #[cfg(unix)]
        device: u64,
        #[cfg(unix)]
        inode: u64,
        #[cfg(unix)]
        mode: u32,
    },
    File(SnapshotEntry),
}

fn migration_surface_snapshot(fixture: &Fixture) -> BTreeMap<PathBuf, SnapshotEntry> {
    let mut snapshot = BTreeMap::new();
    for root in [
        fixture.profile.join("projects").join(&fixture.source_id),
        fixture.profile.join("projects").join(&fixture.target_id),
    ] {
        for path in relative_file_map(&root).unwrap().into_values() {
            snapshot_file(&path, &mut snapshot);
        }
    }
    let global = fixture.profile.join("global.db");
    for path in [
        storage::enrollment_marker_path(&fixture.project),
        storage::repository_identity_path(&fixture.project).unwrap(),
        global.clone(),
        sqlite_sidecar(&global, "-wal"),
        sqlite_sidecar(&global, "-shm"),
    ] {
        snapshot_file(&path, &mut snapshot);
    }
    snapshot
}

fn snapshot_file(path: &Path, snapshot: &mut BTreeMap<PathBuf, SnapshotEntry>) {
    let entry = if path.is_file() {
        let metadata = fs::metadata(path).unwrap();
        SnapshotEntry::File {
            digest: file_digest(path).unwrap(),
            bytes: metadata.len(),
            modified: metadata.modified().unwrap(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            changed_seconds: metadata.ctime(),
            #[cfg(unix)]
            changed_nanoseconds: metadata.ctime_nsec(),
            #[cfg(unix)]
            links: metadata.nlink(),
        }
    } else {
        SnapshotEntry::Missing
    };
    snapshot.insert(path.to_path_buf(), entry);
}

fn full_tree_snapshot(root: &Path) -> BTreeMap<PathBuf, TreeSnapshotEntry> {
    let mut snapshot = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path).unwrap();
        let relative = path.strip_prefix(root).unwrap().to_path_buf();
        let is_database_authority_artifact = relative.components().any(|component| {
            component.as_os_str() == std::ffi::OsStr::new(".tracedecay-database-locks")
        }) || relative.file_name().is_some_and(|name| {
            let name = name.to_string_lossy();
            name == "lifecycle.lock"
                || name == "lifecycle.lock.owner"
                || name.ends_with(".access.lock")
                || name.ends_with(".writer.lock")
                || name.ends_with(".writer.owner")
        });
        if is_database_authority_artifact {
            continue;
        }
        if metadata.is_dir() {
            #[cfg(unix)]
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            snapshot.insert(
                relative,
                TreeSnapshotEntry::Directory {
                    #[cfg(unix)]
                    device: metadata.dev(),
                    #[cfg(unix)]
                    inode: metadata.ino(),
                    #[cfg(unix)]
                    mode: metadata.permissions().mode(),
                },
            );
            let mut children = fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            children.sort();
            pending.extend(children);
        } else {
            let mut file = BTreeMap::new();
            snapshot_file(&path, &mut file);
            snapshot.insert(
                relative,
                TreeSnapshotEntry::File(file.remove(&path).unwrap()),
            );
        }
    }
    snapshot
}

impl Fixture {
    fn options(&self) -> ConsolidationOptions {
        ConsolidationOptions {
            project_root: self.project.clone(),
            profile_root: self.profile.clone(),
            source_project_id: self.source_id.clone(),
            target_project_id: self.target_id.clone(),
        }
    }
}

fn input_manifest_paths(
    fixture: &Fixture,
    project_id: &str,
    destination_project_id: &str,
) -> (PathBuf, PathBuf) {
    let root = fixture.profile.join("projects").join(project_id);
    (
        root.join(storage::STORE_MANIFEST_FILENAME),
        root.join(format!(
            "store_manifest.consolidated-into-{destination_project_id}.json"
        )),
    )
}

fn migration_source() -> ClaudeSourceIdentityV1 {
    ClaudeSourceIdentityV1::new(SessionId::new("session.migration").unwrap()).unwrap()
}

fn migration_coverage_json(start: u64, end: u64) -> String {
    serde_json::to_string(&ObservationCoverageV1::new(
        ClaudeFileGenerationV1::new(17).unwrap(),
        tracedecay_domain::ObservationOrderingDomainV1::FileBytes,
        ClaudeByteRangeV1::new(start, end).unwrap(),
    ))
    .unwrap()
}

fn migration_cursor(byte_offset: u64) -> ClaudeSourceCursorV1 {
    migration_cursor_for("session.migration", byte_offset)
}

fn migration_cursor_for(session_id: &str, byte_offset: u64) -> ClaudeSourceCursorV1 {
    migration_cursor_generation_for(session_id, 17, byte_offset)
}

fn migration_cursor_generation_for(
    session_id: &str,
    generation: u64,
    byte_offset: u64,
) -> ClaudeSourceCursorV1 {
    ClaudeSourceCursorV1::new(
        ClaudeSourceIdentityV1::new(SessionId::new(session_id).unwrap()).unwrap(),
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(generation).unwrap(),
        byte_offset,
    )
    .unwrap()
}

fn migration_observation(
    start: u64,
    end: u64,
    receipt_id: &str,
    message_id: &str,
) -> DurableClaudeObservationV1 {
    migration_observation_range(
        "session.migration",
        start,
        end,
        receipt_id,
        message_id,
        &format!("payload {message_id}"),
    )
}

fn migration_observation_generation(
    session_id: &str,
    generation: u64,
    start: u64,
    end: u64,
    receipt_id: &str,
    message_id: &str,
    body: &str,
) -> DurableClaudeObservationV1 {
    migration_observation_range_generation(
        session_id, generation, start, end, receipt_id, message_id, body,
    )
}

fn migration_observation_for(
    session_id: &str,
    receipt_id: &str,
    message_id: &str,
    body: &str,
) -> DurableClaudeObservationV1 {
    migration_observation_range_generation(session_id, 17, 0, 10, receipt_id, message_id, body)
}

fn migration_observation_range(
    session_id: &str,
    start: u64,
    end: u64,
    receipt_id: &str,
    message_id: &str,
    body: &str,
) -> DurableClaudeObservationV1 {
    migration_observation_range_generation(session_id, 17, start, end, receipt_id, message_id, body)
}

fn migration_observation_range_generation(
    session_id: &str,
    generation: u64,
    start: u64,
    end: u64,
    receipt_id: &str,
    message_id: &str,
    body: &str,
) -> DurableClaudeObservationV1 {
    let payload = json!({
        "type": "assistant",
        "uuid": format!("record-{message_id}"),
        "timestamp": "2025-06-15T15:06:40Z",
        "message": {
            "id": message_id,
            "role": "assistant",
            "content": [{"type": "text", "text": body}],
            "model": "claude-sonnet-4"
        }
    });
    let payload_reference = PayloadReferenceV1::for_payload(&payload).unwrap();
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(receipt_id).unwrap(),
            ComponentVersion::new("sanitizer.migration-test.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(payload_reference),
    )
    .unwrap();
    let identity = ClaudeObservationIdentityMaterialV1::new(
        ClaudeSourceIdentityV1::new(SessionId::new(session_id).unwrap()).unwrap(),
        ObservationScopeV1::Profile,
        ClaudeFileGenerationV1::new(generation).unwrap(),
        ClaudeByteRangeV1::new(start, end).unwrap(),
    )
    .unwrap();
    DurableClaudeObservationV1::new(
        identity,
        receipt,
        RetentionClass::new("retention.migration-test").unwrap(),
        payload,
    )
    .unwrap()
}

async fn persist_migration_observation(
    db: &GlobalDb,
    observation: DurableClaudeObservationV1,
    expected_cursor: Option<ClaudeSourceCursorV1>,
) {
    let next_cursor = ClaudeSourceCursorV1::new(
        observation.source().clone(),
        observation.scope().clone(),
        observation.identity().generation(),
        observation.identity().position().end(),
    )
    .unwrap();
    let write = ObservationWrite::new(observation, expected_cursor, next_cursor).unwrap();
    let projection_generation =
        ProjectionGenerationId::new(SESSION_MESSAGE_PROJECTOR_VERSION).unwrap();
    let authorization = build_observation_resolution_authorization_v1(
        write.observation(),
        "observation-migration-test.v1",
    )
    .unwrap();
    let retrieval_anchor = build_observation_retrieval_anchor_v2(
        write.observation(),
        projection_generation.clone(),
        UtcMicros(1),
        authorization,
    )
    .unwrap();
    let write =
        AnchoredObservationWrite::new(write, retrieval_anchor, projection_generation).unwrap();
    assert!(matches!(
        GlobalDbObservationStore::new(db)
            .persist_observation(write)
            .await
            .unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
}

async fn project_all_migration_observations(db: &GlobalDb) -> usize {
    let store = GlobalDbObservationStore::new(db);
    let mut projected = 0;
    while let Some(observation_id) = store.next_queued_observation().await.unwrap() {
        store.project_observation(&observation_id).await.unwrap();
        projected += 1;
    }
    projected
}

async fn assert_observation_authority(path: &Path) {
    for (table, expected) in [
        ("sanitization_receipts", 2),
        ("observations", 2),
        ("source_cursors", 1),
    ] {
        assert_eq!(sqlite::count_rows(path, table).await.unwrap(), expected);
    }
    let db = GlobalDb::open_at_without_structured_backfill(path)
        .await
        .unwrap();
    let cursor = GlobalDbObservationStore::new(&db)
        .get_source_cursor(&migration_source(), &ObservationScopeV1::Profile)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cursor.byte_offset(), 20);
    db.close();
}

async fn assert_pending_projection_replay(path: &Path) {
    assert_eq!(
        sqlite::count_rows(path, "observation_projection_checkpoints")
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlite::count_rows(path, "observation_projection_dispositions")
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlite::count_rows(path, "observation_projection_provenance")
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlite::count_rows(path, "projection_queue").await.unwrap(),
        2
    );
}

async fn assert_projection_output(path: &Path, observation_id: &str, output_message_id: &str) {
    let db = GlobalDb::open_at_without_structured_backfill(path)
        .await
        .unwrap();
    for table in [
        "observation_projection_aliases",
        "observation_projection_provenance",
    ] {
        let sql = format!(
            "SELECT output_message_id FROM {table}
             WHERE observation_id=?1"
        );
        let mut rows = db
            .conn()
            .query(&sql, libsql::params![observation_id])
            .await
            .unwrap();
        assert_eq!(
            rows.next()
                .await
                .unwrap()
                .unwrap()
                .get::<String>(0)
                .unwrap(),
            output_message_id
        );
    }
    db.close();
}

async fn assert_projection_alias(path: &Path, observation_id: &str, output_message_id: &str) {
    let db = GlobalDb::open_at_without_structured_backfill(path)
        .await
        .unwrap();
    let mut rows = db
        .conn()
        .query(
            "SELECT output_message_id FROM observation_projection_aliases
             WHERE observation_id=?1",
            libsql::params![observation_id],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        output_message_id
    );
    db.close();
}

async fn assert_no_projection_alias(path: &Path, observation_id: &str) {
    let db = GlobalDb::open_at_without_structured_backfill(path)
        .await
        .unwrap();
    let mut rows = db
        .conn()
        .query(
            "SELECT COUNT(*) FROM observation_projection_aliases WHERE observation_id=?1",
            libsql::params![observation_id],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    db.close();
}

async fn assert_projection_ownership(
    path: &Path,
    output_message_id: &str,
    created: i64,
    retained: i64,
) {
    let db = GlobalDb::open_at_without_structured_backfill(path)
        .await
        .unwrap();
    let mut rows = db
        .conn()
        .query(
            "SELECT SUM(message_created), SUM(1-message_created)
             FROM observation_projection_provenance WHERE output_message_id=?1",
            libsql::params![output_message_id],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), created);
    assert_eq!(row.get::<i64>(1).unwrap(), retained);
    db.close();
}

async fn assert_shared_projection_predrain(
    path: &Path,
    shared_observation_id: &str,
    newer_observation_id: &str,
    original_message_id: &str,
    remapped_message_id: &str,
) {
    assert_no_projection_alias(path, shared_observation_id).await;
    assert_projection_alias(path, newer_observation_id, remapped_message_id).await;
    assert_message_text(path, original_message_id, "older target body").await;
    assert_message_absent(path, remapped_message_id).await;
    assert_eq!(
        sqlite::count_rows(path, "observation_projection_provenance")
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlite::count_rows(path, "projection_queue").await.unwrap(),
        2
    );
    assert_no_orphaned_projection_provenance(path).await;
}

async fn assert_message_text(path: &Path, message_id: &str, expected: &str) {
    let db = GlobalDb::open_at_without_structured_backfill(path)
        .await
        .unwrap();
    let mut rows = db
        .conn()
        .query(
            "SELECT text FROM session_messages WHERE provider='claude' AND message_id=?1",
            libsql::params![message_id],
        )
        .await
        .unwrap();
    let actual = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    assert!(
        actual.contains(expected),
        "{actual:?} does not contain {expected:?}"
    );
    assert!(rows.next().await.unwrap().is_none());
    db.close();
}

async fn assert_message_absent(path: &Path, message_id: &str) {
    let db = GlobalDb::open_at_without_structured_backfill(path)
        .await
        .unwrap();
    let mut rows = db
        .conn()
        .query(
            "SELECT COUNT(*) FROM session_messages WHERE provider='claude' AND message_id=?1",
            libsql::params![message_id],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    db.close();
}

async fn assert_no_orphaned_projection_provenance(path: &Path) {
    let db = GlobalDb::open_at_without_structured_backfill(path)
        .await
        .unwrap();
    let mut rows = db
        .conn()
        .query(
            "SELECT COUNT(*)
             FROM observation_projection_provenance AS provenance
             LEFT JOIN session_messages AS message
               ON message.provider=provenance.output_provider
              AND message.message_id=provenance.output_message_id
             WHERE message.message_id IS NULL",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    db.close();
}

async fn set_migration_cursor(db: &GlobalDb, session_id: &str, generation: u64, byte_offset: u64) {
    let cursor = migration_cursor_generation_for(session_id, generation, byte_offset);
    db.writer_connection()
        .await
        .unwrap()
        .execute(
            "UPDATE source_cursors SET cursor_json=?1",
            libsql::params![serde_json::to_string(&cursor).unwrap()],
        )
        .await
        .unwrap();
}

async fn insert_projection_alias(db: &GlobalDb, observation_id: &str, output_message_id: &str) {
    db.writer_connection()
        .await
        .unwrap()
        .execute(
            "INSERT INTO observation_projection_aliases(
                 projector_version, observation_id, output_provider, output_message_id
             ) VALUES (?1, ?2, 'claude', ?3)",
            libsql::params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                observation_id,
                output_message_id
            ],
        )
        .await
        .unwrap();
    let rebuilt = GlobalDbObservationStore::new(db)
        .rebuild_projection(0)
        .await
        .unwrap();
    assert!(rebuilt.is_complete());
}

async fn fixture() -> Fixture {
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("repo");
    let profile = temp.path().join("profile");
    let source_id = "proj_legacy".to_string();
    let target_id = "proj_current".to_string();
    init_repo(&project);
    create_shard(
        &profile,
        &project,
        &source_id,
        "legacy durable fact",
        "legacy-session",
        true,
    )
    .await;
    create_shard(
        &profile,
        &project,
        &target_id,
        "current durable fact",
        "current-session",
        false,
    )
    .await;
    let global = GlobalDb::open_at_without_structured_backfill(&profile.join("global.db"))
        .await
        .unwrap();
    let git_common_dir = crate::worktree::git_common_dir(&project).unwrap();
    for project_id in [&source_id, &target_id] {
        global
            .upsert_code_project(
                project_id,
                &project,
                Some(&git_common_dir),
                None,
                Some("main"),
            )
            .await
            .unwrap();
        global
            .upsert_store_instance(StoreInstanceUpsert {
                store_id: format!("store:{project_id}:profile_sharded"),
                project_id: project_id.clone(),
                store_kind: "code_project".to_string(),
                storage_mode: "profile_sharded".to_string(),
                store_relpath: format!("projects/{project_id}"),
                manifest_relpath: Some(format!(
                    "projects/{project_id}/{}",
                    storage::STORE_MANIFEST_FILENAME
                )),
                last_verified_at: Some(1_800_000_000),
                last_write_at: Some(1_800_000_000),
            })
            .await
            .unwrap();
    }
    global
        .upsert_project_alias(&project, &target_id)
        .await
        .unwrap();
    global.checkpoint().await;
    global.close();
    storage::write_repository_identity_marker(&project, &target_id).unwrap();
    Fixture {
        _temp: temp,
        project,
        profile,
        source_id,
        target_id,
    }
}

async fn create_shard(
    profile: &Path,
    project: &Path,
    project_id: &str,
    fact_content: &str,
    session_id: &str,
    feedback: bool,
) {
    let layout = layout_for_id(project, profile, project_id).unwrap();
    fs::create_dir_all(&layout.data_root).unwrap();
    let (graph, _) = test_initialize(&layout.graph_db_path).await;
    {
        let writer = graph.memory_writer().await.unwrap();
        let memory = writer.store();
        let outcome = memory
            .add_fact(
                AddFactRequest {
                    content: fact_content.to_string(),
                    category: MemoryCategory::Project,
                    source: Some("consolidation-test".to_string()),
                    tags: vec![project_id.to_string()],
                    entities: vec!["TraceDecay".to_string()],
                    trust: Some(0.8),
                    metadata: json!({"project_id": project_id}),
                },
                0.5,
            )
            .await
            .unwrap();
        if feedback {
            memory
                .record_feedback_event(FeedbackRequest {
                    fact_id: outcome.fact.unwrap().fact_id,
                    action: FeedbackAction::Helpful,
                    source: Some("consolidation-test".to_string()),
                    note: Some("verified".to_string()),
                })
                .await
                .unwrap();
        }
    }
    graph.checkpoint().await.unwrap();
    graph.close();

    let sessions = GlobalDb::open_at_without_structured_backfill(&layout.sessions_db_path)
        .await
        .unwrap();
    assert!(
        sessions
            .upsert_session(&SessionRecord {
                provider: "codex".to_string(),
                session_id: session_id.to_string(),
                project_key: project_id.to_string(),
                project_path: project.to_string_lossy().to_string(),
                title: Some(session_id.to_string()),
                started_at: Some(1_800_000_000),
                ended_at: Some(1_800_000_001),
                transcript_path: None,
                metadata_json: None,
                parent_session_id: None,
                is_subagent: false,
                agent_id: None,
                parent_tool_use_id: None,
            })
            .await
    );
    assert!(
        sessions
            .upsert_session_message(&SessionMessageRecord {
                provider: "codex".to_string(),
                message_id: format!("message-{session_id}"),
                session_id: session_id.to_string(),
                role: "user".to_string(),
                timestamp: Some(1_800_000_000),
                ordinal: 0,
                text: format!("message from {session_id}"),
                kind: Some("message".to_string()),
                model: None,
                tool_names: None,
                source_path: None,
                source_offset: None,
                metadata_json: None,
            })
            .await
    );
    sessions.checkpoint().await;
    sessions.close();

    branch_meta::save_branch_meta(&layout.data_root, &BranchMeta::new("main")).unwrap();
    fs::create_dir_all(layout.data_root.join("lcm-payloads")).unwrap();
    let payload_name = if feedback { "source.txt" } else { "target.txt" };
    fs::write(
        layout.data_root.join("lcm-payloads").join(payload_name),
        session_id,
    )
    .unwrap();
    storage::write_store_manifest(&layout).unwrap();
}

async fn add_fact_to_shard(
    fixture: &Fixture,
    project_id: &str,
    content: &str,
    tag: &str,
    metadata: serde_json::Value,
    feedback: Option<FeedbackAction>,
) {
    let layout = layout_for_id(&fixture.project, &fixture.profile, project_id).unwrap();
    let (graph, _) = test_open(&layout.graph_db_path).await;
    {
        let writer = graph.memory_writer().await.unwrap();
        let memory = writer.store();
        let outcome = memory
            .add_fact(
                AddFactRequest {
                    content: content.to_string(),
                    category: MemoryCategory::Project,
                    source: Some(project_id.to_string()),
                    tags: vec![tag.to_string()],
                    entities: vec!["TraceDecay".to_string()],
                    trust: Some(0.8),
                    metadata,
                },
                0.5,
            )
            .await
            .unwrap();
        if let Some(action) = feedback {
            memory
                .record_feedback_event(FeedbackRequest {
                    fact_id: outcome.fact.unwrap().fact_id,
                    action,
                    source: Some(project_id.to_string()),
                    note: Some("overlap".to_string()),
                })
                .await
                .unwrap();
        }
    }
    graph.checkpoint().await.unwrap();
    graph.close();
}

async fn add_fact_relation_to_shard(fixture: &Fixture, project_id: &str) {
    let layout = layout_for_id(&fixture.project, &fixture.profile, project_id).unwrap();
    let (graph, _) = test_open(&layout.graph_db_path).await;
    {
        let writer = graph.memory_writer().await.unwrap();
        let memory = writer.store();
        let source_fact_id = memory
            .list_facts(None, Some(0.0), 10)
            .await
            .unwrap()
            .into_iter()
            .next()
            .expect("fixture source fact")
            .fact_id;
        let target_fact_id = memory
            .add_fact(
                AddFactRequest {
                    content: "relation target fact".to_string(),
                    category: MemoryCategory::Project,
                    source: Some("consolidation-test".to_string()),
                    tags: vec!["relation".to_string()],
                    entities: vec!["TraceDecay".to_string()],
                    trust: Some(0.75),
                    metadata: json!({"project_id": project_id}),
                },
                0.5,
            )
            .await
            .unwrap()
            .fact
            .expect("relation target fact should be stored")
            .fact_id;
        memory
            .upsert_fact_relation(
                source_fact_id,
                target_fact_id,
                FactRelationKind::Supports,
                0.9,
                "consolidation-test",
                json!({"evidence": "fixture"}),
            )
            .await
            .unwrap();
    }
    graph.checkpoint().await.unwrap();
    graph.close();
}

fn add_branch_links(fixture: &Fixture, project_id: &str, count: usize) {
    let layout = layout_for_id(&fixture.project, &fixture.profile, project_id).unwrap();
    let mut meta = branch_meta::load_branch_meta(&layout.data_root).unwrap();
    let branches = layout.data_root.join("branches");
    fs::create_dir_all(&branches).unwrap();
    for index in 0..count {
        let name = format!("load-{index:03}");
        let relative = format!("branches/load-{index:03}.db");
        fs::copy(&layout.graph_db_path, layout.data_root.join(&relative)).unwrap();
        meta.add_branch(&name, &relative, "main");
    }
    branch_meta::save_branch_meta(&layout.data_root, &meta).unwrap();
}

async fn add_untracked_branch(layout: &StoreLayout, name: &str, fact_content: &str) {
    let branches = layout.data_root.join("branches");
    fs::create_dir_all(&branches).unwrap();
    let path = branches.join(format!("{name}.db"));
    fs::copy(&layout.graph_db_path, &path).unwrap();
    let (db, _) = test_open(&path).await;
    {
        let writer = db.memory_writer().await.unwrap();
        writer
            .store()
            .add_fact(
                AddFactRequest {
                    content: fact_content.to_string(),
                    category: MemoryCategory::Project,
                    source: Some("untracked-branch-test".to_string()),
                    tags: vec![name.to_string()],
                    entities: vec!["TraceDecay".to_string()],
                    trust: Some(0.8),
                    metadata: json!({"branch": name}),
                },
                0.5,
            )
            .await
            .unwrap();
    }
    db.checkpoint().await.unwrap();
    db.close();
}

fn sqlite_family_bytes(path: &Path) -> u64 {
    [
        path.to_path_buf(),
        sqlite_sidecar(path, "-wal"),
        sqlite_sidecar(path, "-shm"),
    ]
    .into_iter()
    .filter_map(|member| fs::metadata(member).ok())
    .map(|metadata| metadata.len())
    .sum()
}

async fn execute_sql(path: &Path, sql: &str) {
    let (db, _) = test_open(path).await;
    db.execute_write_batch("execute consolidation fixture SQL", sql)
        .await
        .unwrap();
    db.checkpoint().await.unwrap();
    db.close();
}

async fn rewrite_page_size(path: &Path, page_size: i64) {
    let db = libsql::Builder::new_local(path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute_batch(&format!(
        "PRAGMA journal_mode = DELETE; PRAGMA page_size = {page_size}; VACUUM;"
    ))
    .await
    .unwrap();
}

async fn database_page_size(path: &Path) -> i64 {
    let (db, _) = test_open_read_only(path).await;
    let mut rows = db.conn().query("PRAGMA page_size", ()).await.unwrap();
    let page_size = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
    db.close();
    page_size
}

async fn explain_query_plan(conn: &libsql::Connection, sql: &str) -> Vec<String> {
    let mut rows = conn
        .query(&format!("EXPLAIN QUERY PLAN {sql}"), ())
        .await
        .unwrap();
    let mut details = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        details.push(row.get::<String>(3).unwrap());
    }
    details
}

fn init_repo(path: &Path) {
    fs::create_dir_all(path).unwrap();
    run_git(path, &["init"]);
    run_git(path, &["config", "user.email", "test@example.com"]);
    run_git(path, &["config", "user.name", "TraceDecay Test"]);
    fs::write(path.join("lib.rs"), "pub fn fixture() {}\n").unwrap();
    run_git(path, &["add", "."]);
    run_git(path, &["commit", "-m", "fixture"]);
}

fn run_git(path: &Path, args: &[&str]) {
    let status = Command::new(crate::git::git_program())
        .args(args)
        .current_dir(path)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}
