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
    ObservationScopeV1, PayloadReferenceV1, ProjectId, ProjectionGenerationId, RetentionClass,
    SanitizationReceiptId, SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1,
    SensitivityV1, SessionId, UtcMicros,
};
use tracedecay_global_db::tests::harness::HostAdmissionTestRuntimeV1;
use tracedecay_global_db::{GlobalDbObservationStore, RegisteredGlobalDb};
use tracedecay_sessions::admission::HostAdmissionScope;
use tracedecay_store::observation::ObservationCoverageV1;
use tracedecay_store::{
    AnchoredObservationWrite, ObservationPersistOutcome, ObservationProjectionStore,
    ObservationStore, ObservationWrite, SESSION_MESSAGE_PROJECTOR_VERSION, SessionMessageRecord,
    SessionRecord, build_observation_resolution_authorization_v1,
    build_observation_retrieval_anchor_v2,
};

use super::*;
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};
use tracedecay_runtime_core::db::{Database, DatabaseAuthority};
use tracedecay_runtime_core::memory::store::MemoryStore;
use tracedecay_runtime_core::memory::types::{
    AddFactRequest, FactRelationKind, FeedbackAction, FeedbackRequest, MemoryCategory,
};

mod configuration;
mod external_source;
mod lifecycle;
mod memory;
mod observation;
mod schema;
mod session_merge;
mod temporal;

async fn test_initialize(path: &Path) -> (Database, bool) {
    let authority = DatabaseAuthority::acquire_test(path, "consolidation test initialize").unwrap();
    Database::publish_test_runtime(
        path,
        &authority,
        tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Initialize,
    )
    .await
    .unwrap()
}

async fn test_open(path: &Path) -> (Database, bool) {
    let authority = DatabaseAuthority::acquire_test(path, "consolidation test open").unwrap();
    Database::publish_test_runtime(
        path,
        &authority,
        tracedecay_runtime_core::db::TestDatabaseRuntimeMode::Existing,
    )
    .await
    .unwrap()
}

async fn test_open_read_only(path: &Path) -> (Database, bool) {
    let authority = DatabaseAuthority::acquire_test(path, "consolidation test read").unwrap();
    Database::publish_test_runtime(
        path,
        &authority,
        tracedecay_runtime_core::db::TestDatabaseRuntimeMode::ReadOnly,
    )
    .await
    .unwrap()
}

fn test_observation_store(db: &RegisteredGlobalDb) -> GlobalDbObservationStore<'_> {
    GlobalDbObservationStore::with_runtime(db.runtime(), db.authority())
}

async fn clear_project_aliases_for_test(
    db: &RegisteredGlobalDb,
) -> tracedecay_runtime_core::errors::Result<u64> {
    let transaction = db.begin_write_transaction().await?;
    let deleted = transaction
        .execute("DELETE FROM project_aliases", ())
        .await
        .map_err(|error| TraceDecayError::Database {
            operation: "clear registered project aliases for test".to_owned(),
            message: error.to_string(),
        })?;
    transaction
        .commit()
        .await
        .map_err(|error| TraceDecayError::Database {
            operation: "commit registered project alias cleanup for test".to_owned(),
            message: error.to_string(),
        })?;
    Ok(deleted)
}

async fn open_historical_project_runtime(
    profile: &Path,
    project: &Path,
    project_id: &str,
) -> HostAdmissionTestRuntimeV1 {
    let previous_enrollment = storage::read_enrollment_marker(project).unwrap();
    let requested_enrollment = EnrollmentMarker {
        project_id: project_id.to_string(),
        storage_mode: StorageMode::ProfileSharded,
    };
    storage::write_enrollment_marker(project, &requested_enrollment).unwrap();
    let runtime = HostAdmissionTestRuntimeV1::project(
        profile,
        project,
        ProjectId::new(project_id.to_string()).unwrap(),
    )
    .await;
    match previous_enrollment {
        Some(marker) => storage::write_enrollment_marker(project, &marker).unwrap(),
        None => {
            storage::remove_enrollment_marker(project, project_id).unwrap();
        }
    }
    runtime.unwrap()
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
        let is_transient_sqlite_sidecar = relative.file_name().is_some_and(|name| {
            let name = name.to_string_lossy();
            name.ends_with("-shm") || (name.ends_with("-wal") && metadata.len() == 0)
        });
        if is_database_authority_artifact
            || is_coordination_lock(&relative)
            || is_transient_sqlite_sidecar
        {
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

fn migration_cursor_for_scope(
    session_id: &str,
    byte_offset: u64,
    scope: ObservationScopeV1,
) -> ClaudeSourceCursorV1 {
    migration_cursor_generation_for_scope(session_id, 17, byte_offset, scope)
}

fn migration_cursor_generation_for(
    session_id: &str,
    generation: u64,
    byte_offset: u64,
) -> ClaudeSourceCursorV1 {
    migration_cursor_generation_for_scope(
        session_id,
        generation,
        byte_offset,
        ObservationScopeV1::Profile,
    )
}

fn migration_cursor_generation_for_scope(
    session_id: &str,
    generation: u64,
    byte_offset: u64,
    scope: ObservationScopeV1,
) -> ClaudeSourceCursorV1 {
    ClaudeSourceCursorV1::new(
        ClaudeSourceIdentityV1::new(SessionId::new(session_id).unwrap()).unwrap(),
        scope,
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

fn migration_observation_for_scope(
    session_id: &str,
    scope: ObservationScopeV1,
    receipt_id: &str,
    message_id: &str,
    body: &str,
) -> DurableClaudeObservationV1 {
    migration_observation_range_generation_for_scope(
        session_id, scope, 17, 0, 10, receipt_id, message_id, body,
    )
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

fn migration_observation_range_for_scope(
    session_id: &str,
    scope: ObservationScopeV1,
    start: u64,
    end: u64,
    receipt_id: &str,
    message_id: &str,
    body: &str,
) -> DurableClaudeObservationV1 {
    migration_observation_range_generation_for_scope(
        session_id, scope, 17, start, end, receipt_id, message_id, body,
    )
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
    migration_observation_range_generation_for_scope(
        session_id,
        ObservationScopeV1::Profile,
        generation,
        start,
        end,
        receipt_id,
        message_id,
        body,
    )
}

fn migration_observation_range_generation_for_scope(
    session_id: &str,
    scope: ObservationScopeV1,
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
        scope,
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
    db: &RegisteredGlobalDb,
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
        test_observation_store(db)
            .persist_observation(write)
            .await
            .unwrap(),
        ObservationPersistOutcome::Committed(_)
    ));
}

async fn project_all_migration_observations(db: &RegisteredGlobalDb) -> usize {
    let store = test_observation_store(db);
    let mut projected = 0;
    while let Some(observation_id) = store.next_queued_observation().await.unwrap() {
        store.project_observation(&observation_id).await.unwrap();
        projected += 1;
    }
    projected
}

async fn registered_count_rows(db: &RegisteredGlobalDb, table: &str) -> i64 {
    let snapshot = db.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(&format!("SELECT COUNT(*) FROM {table}"), ())
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

async fn assert_observation_authority(db: &RegisteredGlobalDb) {
    for (table, expected) in [
        ("sanitization_receipts", 2),
        ("observations", 2),
        ("source_cursors", 1),
    ] {
        assert_eq!(registered_count_rows(db, table).await, expected);
    }
    let cursor = test_observation_store(db)
        .get_source_cursor(&migration_source(), &ObservationScopeV1::Profile)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cursor.byte_offset(), 20);
}

async fn assert_pending_projection_replay(db: &RegisteredGlobalDb) {
    assert_eq!(
        registered_count_rows(db, "observation_projection_checkpoints").await,
        0
    );
    assert_eq!(
        registered_count_rows(db, "observation_projection_dispositions").await,
        0
    );
    assert_eq!(
        registered_count_rows(db, "observation_projection_provenance").await,
        2
    );
    assert_eq!(registered_count_rows(db, "projection_queue").await, 2);
}

async fn assert_projection_output(
    db: &RegisteredGlobalDb,
    observation_id: &str,
    output_message_id: &str,
) {
    for table in [
        "observation_projection_aliases",
        "observation_projection_provenance",
    ] {
        let sql = format!(
            "SELECT output_message_id FROM {table}
             WHERE observation_id=?1"
        );
        let snapshot = db.read_snapshot().await.unwrap();
        let mut rows = snapshot.query(&sql, params![observation_id]).await.unwrap();
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
}

async fn assert_projection_alias(
    db: &RegisteredGlobalDb,
    observation_id: &str,
    output_message_id: &str,
) {
    let snapshot = db.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT output_message_id FROM observation_projection_aliases
             WHERE observation_id=?1",
            params![observation_id],
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
    drop(snapshot);
}

async fn assert_no_projection_alias(db: &RegisteredGlobalDb, observation_id: &str) {
    let snapshot = db.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT COUNT(*) FROM observation_projection_aliases WHERE observation_id=?1",
            params![observation_id],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    drop(snapshot);
}

async fn assert_projection_ownership(
    db: &RegisteredGlobalDb,
    output_message_id: &str,
    created: i64,
    retained: i64,
) {
    let snapshot = db.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT SUM(message_created), SUM(1-message_created)
             FROM observation_projection_provenance WHERE output_message_id=?1",
            params![output_message_id],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), created);
    assert_eq!(row.get::<i64>(1).unwrap(), retained);
    drop(snapshot);
}

async fn assert_shared_projection_predrain(
    db: &RegisteredGlobalDb,
    shared_observation_id: &str,
    newer_observation_id: &str,
    original_message_id: &str,
    remapped_message_id: &str,
) {
    assert_no_projection_alias(db, shared_observation_id).await;
    assert_projection_alias(db, newer_observation_id, remapped_message_id).await;
    assert_message_text(db, original_message_id, "older target body").await;
    assert_message_absent(db, remapped_message_id).await;
    assert_eq!(
        registered_count_rows(db, "observation_projection_provenance").await,
        1
    );
    assert_eq!(registered_count_rows(db, "projection_queue").await, 2);
    assert_no_orphaned_projection_provenance(db).await;
}

async fn assert_message_text(db: &RegisteredGlobalDb, message_id: &str, expected: &str) {
    let snapshot = db.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT text FROM session_messages WHERE provider='claude' AND message_id=?1",
            params![message_id],
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
    drop(snapshot);
}

async fn assert_message_absent(db: &RegisteredGlobalDb, message_id: &str) {
    let snapshot = db.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT COUNT(*) FROM session_messages WHERE provider='claude' AND message_id=?1",
            params![message_id],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    drop(snapshot);
}

async fn assert_no_orphaned_projection_provenance(db: &RegisteredGlobalDb) {
    let snapshot = db.read_snapshot().await.unwrap();
    let mut rows = snapshot
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
    drop(snapshot);
}

async fn set_migration_cursor(
    db: &RegisteredGlobalDb,
    session_id: &str,
    generation: u64,
    byte_offset: u64,
) {
    let cursor = migration_cursor_generation_for(session_id, generation, byte_offset);
    db.writer_connection()
        .unwrap()
        .execute(
            "UPDATE source_cursors SET cursor_json=?1",
            params![serde_json::to_string(&cursor).unwrap()],
        )
        .await
        .unwrap();
}

async fn insert_projection_alias(
    db: &RegisteredGlobalDb,
    observation_id: &str,
    output_message_id: &str,
) {
    db.writer_connection()
        .unwrap()
        .execute(
            "INSERT INTO observation_projection_aliases(
                 projector_version, observation_id, output_provider, output_message_id
             ) VALUES (?1, ?2, 'claude', ?3)",
            params![
                SESSION_MESSAGE_PROJECTOR_VERSION,
                observation_id,
                output_message_id
            ],
        )
        .await
        .unwrap();
    let rebuilt = test_observation_store(db)
        .rebuild_projection(0)
        .await
        .unwrap();
    assert!(rebuilt.is_complete());
}

const FIXTURE_SOURCE_ID: &str = "proj_legacy";
const FIXTURE_TARGET_ID: &str = "proj_current";

/// Cross-process on-disk fixture template. nextest runs every test in its own
/// process, so per-process caches cannot amortize the fixed cost of building
/// the consolidation fixture (a git repo, two graph shards, two session DBs,
/// and a global DB) that ~40 tests each pay. This builds the full tree once
/// behind an exclusive file lock, and every test copies the template into its
/// own isolated `TempDir` and re-runs only the cheap path-dependent fixups
/// (global-DB project rows/aliases, the two store manifests, and the
/// repository-identity marker). Bump the version suffix when the fixture
/// layout changes so stale templates are ignored. Every entry point falls back
/// to the real builder when the template cannot be built, copied, or fixed up,
/// so tests never depend on the template for correctness.
const FIXTURE_TEMPLATE_BASE: &str = "tracedecay-consolidate-fixture-template-v1";

static FIXTURE_TEMPLATE: tokio::sync::OnceCell<Option<PathBuf>> =
    tokio::sync::OnceCell::const_new();

/// Names the template directory, keyed by a fingerprint of the schema source
/// files whose shape the seeded stores embed. A schema change flips the key, so
/// a stale template built by an earlier revision (the dir persists under the
/// system temp dir across runs) is abandoned rather than served with the wrong
/// shape.
fn fixture_template_dir_name() -> String {
    let fingerprint = tracedecay_global_db::tests::registered_schema_fixture_fingerprint();
    format!("{FIXTURE_TEMPLATE_BASE}-{fingerprint}")
}

async fn fixture() -> Fixture {
    match fixture_from_template().await {
        Some(fixture) => fixture,
        None => build_fixture_real().await,
    }
}

/// Builds the fixture tree directly under a fresh `TempDir` via the real
/// builder — the correctness baseline and the fallback when templating fails.
async fn build_fixture_real() -> Fixture {
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("repo");
    let profile = temp.path().join("profile");
    build_fixture_tree(&project, &profile).await;
    make_fixture(temp)
}

fn make_fixture(temp: TempDir) -> Fixture {
    let project = temp.path().join("repo");
    let profile = temp.path().join("profile");
    Fixture {
        _temp: temp,
        project,
        profile,
        source_id: FIXTURE_SOURCE_ID.to_string(),
        target_id: FIXTURE_TARGET_ID.to_string(),
    }
}

/// Builds the complete fixture tree (git repo, two shards, global DB, and the
/// repository-identity marker) at the given `project`/`profile` roots. Shared
/// by the real builder and the template builder.
async fn build_fixture_tree(project: &Path, profile: &Path) {
    init_repo(project);
    create_shard(
        profile,
        project,
        FIXTURE_SOURCE_ID,
        "legacy durable fact",
        "legacy-session",
        true,
    )
    .await;
    storage::remove_enrollment_marker(project, FIXTURE_SOURCE_ID)
        .expect("remove historical source enrollment before target fixture");
    create_shard(
        profile,
        project,
        FIXTURE_TARGET_ID,
        "current durable fact",
        "current-session",
        false,
    )
    .await;
    let global = HostAdmissionTestRuntimeV1::profile(profile).await.unwrap();
    let git_common_dir = tracedecay_runtime_core::worktree::git_common_dir(project).unwrap();
    for project_id in [FIXTURE_SOURCE_ID, FIXTURE_TARGET_ID] {
        global
            .profile_registry()
            .upsert_code_project(
                project_id,
                project,
                Some(&git_common_dir),
                None,
                Some("main"),
            )
            .await
            .unwrap();
        global
            .profile_registry()
            .upsert_store_instance(StoreInstanceUpsert {
                store_id: format!("store:{project_id}:profile_sharded"),
                project_id: project_id.to_string(),
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
        .profile_registry()
        .upsert_project_alias(project, FIXTURE_TARGET_ID)
        .await
        .unwrap();
    global.profile_registry().checkpoint().await;
    storage::write_repository_identity_marker(project, FIXTURE_TARGET_ID).unwrap();
}

/// Seeds a fixture by copying the shared template and re-running the cheap
/// path-dependent fixups. Returns `None` on any failure so the caller falls
/// back to [`build_fixture_real`].
async fn fixture_from_template() -> Option<Fixture> {
    let template = fixture_template_root().await?;
    let temp = TempDir::new().ok()?;
    let project = temp.path().join("repo");
    let profile = temp.path().join("profile");
    copy_fixture_tree(&template.join("repo"), &project).ok()?;
    copy_fixture_tree(&template.join("profile"), &profile).ok()?;
    apply_fixture_fixups(&project, &profile).await?;
    Some(make_fixture(temp))
}

/// Re-points the copied store at its new location: global-DB project rows and
/// aliases (recomputed git common dir + fresh path aliases), the two store
/// manifests (`project_root/data_root`), and the repository-identity marker.
async fn apply_fixture_fixups(project: &Path, profile: &Path) -> Option<()> {
    let git_common_dir = tracedecay_runtime_core::worktree::git_common_dir(project)?;
    let global = HostAdmissionTestRuntimeV1::profile(profile).await.ok()?;
    // Drop the template build's path aliases so only fresh new-path aliases
    // remain after the re-upserts below.
    clear_project_aliases_for_test(global.profile_registry())
        .await
        .ok()?;
    for project_id in [FIXTURE_SOURCE_ID, FIXTURE_TARGET_ID] {
        global
            .profile_registry()
            .upsert_code_project(
                project_id,
                project,
                Some(&git_common_dir),
                None,
                Some("main"),
            )
            .await?;
    }
    global
        .profile_registry()
        .upsert_project_alias(project, FIXTURE_TARGET_ID)
        .await?;
    global.profile_registry().checkpoint().await;
    for project_id in [FIXTURE_SOURCE_ID, FIXTURE_TARGET_ID] {
        let layout = layout_for_id(project, profile, project_id).ok()?;
        storage::write_store_manifest(&layout).ok()?;
    }
    storage::write_repository_identity_marker(project, FIXTURE_TARGET_ID).ok()?;
    Some(())
}

async fn fixture_template_root() -> Option<&'static Path> {
    FIXTURE_TEMPLATE
        .get_or_init(ensure_fixture_template)
        .await
        .as_deref()
}

/// Returns the shared template dir, building it under an exclusive file lock if
/// this is the first process to need it. Exactly one process builds; concurrent
/// processes block on the lock, then find the `READY` marker.
async fn ensure_fixture_template() -> Option<PathBuf> {
    let base = std::env::temp_dir();
    let dir_name = fixture_template_dir_name();
    let shared = base.join(&dir_name);
    if shared.join("READY").is_file() {
        return Some(shared);
    }
    fs::create_dir_all(&base).ok()?;
    let lock_path = base.join(format!("{dir_name}.lock"));
    let lock_file = tokio::task::spawn_blocking(move || -> std::io::Result<fs::File> {
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)?;
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(file)
    })
    .await
    .ok()?
    .ok()?;
    // Another process may have finished the build while we waited on the lock.
    if shared.join("READY").is_file() {
        let _ = fs2::FileExt::unlock(&lock_file);
        return Some(shared);
    }
    let build = shared.with_file_name(format!("{dir_name}-build-{}", std::process::id()));
    let _ = fs::remove_dir_all(&build);
    let built = build_fixture_template(&build).await;
    let result = match built {
        Ok(()) => match fs::rename(&build, &shared) {
            Ok(()) => Some(shared),
            Err(_) if shared.join("READY").is_file() => {
                let _ = fs::remove_dir_all(&build);
                Some(shared)
            }
            // Rename failed but the private build tree is a valid template.
            Err(_) => Some(build),
        },
        Err(_) => {
            let _ = fs::remove_dir_all(&build);
            None
        }
    };
    let _ = fs2::FileExt::unlock(&lock_file);
    result
}

/// Builds the template into `dest`. The tree is built in a system `TempDir`
/// (never under the repository target/) so branch detection walking up from the
/// fixture repo cannot bootstrap from this repository's own `.git`.
async fn build_fixture_template(dest: &Path) -> std::io::Result<()> {
    let scratch = TempDir::new()?;
    let project = scratch.path().join("repo");
    let profile = scratch.path().join("profile");
    build_fixture_tree(&project, &profile).await;
    copy_fixture_tree(&project, &dest.join("repo"))?;
    copy_fixture_tree(&profile, &dest.join("profile"))?;
    fs::write(dest.join("READY"), b"ok")?;
    Ok(())
}

/// Recursively copies `src` into `dest`, skipping database-authority lock
/// artifacts (mirroring `full_tree_snapshot`'s ignore set) so the copied store
/// carries no stale locks and each test acquires a fresh authority.
fn copy_fixture_tree(src: &Path, dest: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if is_database_lock_artifact(&name.to_string_lossy()) {
            continue;
        }
        let from = entry.path();
        let to = dest.join(&name);
        if entry.file_type()?.is_dir() {
            copy_fixture_tree(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn is_database_lock_artifact(name: &str) -> bool {
    name == ".tracedecay-database-locks"
        || name == "lifecycle.lock"
        || name == "lifecycle.lock.owner"
        || name.ends_with(".access.lock")
        || name.ends_with(".writer.lock")
        || name.ends_with(".writer.owner")
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

    let project_id = ProjectId::new(project_id.to_string()).unwrap();
    let runtime = HostAdmissionTestRuntimeV1::project(profile, project, project_id.clone())
        .await
        .unwrap();
    assert_eq!(
        runtime.database_path(HostAdmissionScope::Project).unwrap(),
        layout.sessions_db_path
    );
    assert!(
        runtime
            .upsert_session_for_test(
                HostAdmissionScope::Project,
                &SessionRecord {
                    provider: "codex".to_string(),
                    session_id: session_id.to_string(),
                    project_key: project_id.as_str().to_string(),
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
                },
            )
            .await
            .unwrap()
    );
    assert!(
        runtime
            .upsert_session_message_for_test(
                HostAdmissionScope::Project,
                &SessionMessageRecord {
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
                },
            )
            .await
            .unwrap()
    );
    runtime
        .registered_database(HostAdmissionScope::Project)
        .unwrap()
        .checkpoint()
        .await;

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

async fn rewrite_page_size(path: &Path, page_size: i64) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(&format!(
        "PRAGMA journal_mode = DELETE; PRAGMA page_size = {page_size}; VACUUM;"
    ))
    .unwrap();
}

async fn database_page_size(path: &Path) -> i64 {
    let (db, _) = test_open_read_only(path).await;
    let mut rows = db.conn().query("PRAGMA page_size", ()).await.unwrap();
    let page_size = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
    db.close();
    page_size
}

async fn explain_query_plan(conn: &impl QueryExecutor, sql: &str) -> Vec<String> {
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
    let status = Command::new(tracedecay_runtime_core::git::git_program())
        .args(args)
        .current_dir(path)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}
