use super::*;
use serde_json::json;
use tracedecay_domain::{
    ClaudeByteRangeV1, ClaudeFileGenerationV1, ClaudeObservationIdentityMaterialV1,
    ClaudeSourceCursorV1, ClaudeSourceIdentityV1, ComponentVersion, DurableClaudeObservationV1,
    ObservationScopeV1, PayloadReferenceV1, ProjectId, RetentionClass, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
    SessionId,
};
use tracedecay_store::observation::{
    CursorAdvanceOutcome, NonDurableFrameReason, ObservationCursorAdvance,
};
use tracedecay_store::{
    CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION, ObservationStoreError, ProjectionSkipReason,
};

#[cfg(unix)]
fn colliding_non_unicode_project_paths(root: &Path) -> (PathBuf, PathBuf) {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    (
        root.join(OsString::from_vec(vec![b'p', 0x80])),
        root.join(OsString::from_vec(vec![b'p', 0x81])),
    )
}

#[cfg(any(unix, windows))]
#[test]
fn native_project_path_alias_uses_canonical_native_decoder() {
    let dir = tempfile::TempDir::new().unwrap();
    let (path, _) = colliding_non_unicode_project_paths(dir.path());
    let native_bytes = encode_native_project_path(&path);
    let alias = encode_native_project_path_alias(native_project_path_platform(), &native_bytes);

    assert_eq!(
        decode_native_project_path(native_project_path_platform(), native_bytes).unwrap(),
        path
    );
    assert_eq!(
        decode_native_project_path_alias(&alias).unwrap(),
        Some(path)
    );
    assert_eq!(
        decode_native_project_path_alias("ordinary-path").unwrap(),
        None
    );

    let other_platform = if cfg!(unix) {
        "windows-utf16le"
    } else {
        "unix-bytes"
    };
    let other_alias = encode_native_project_path_alias(other_platform, b"path");
    assert_eq!(
        decode_native_project_path_alias(&other_alias).unwrap_err(),
        "native project path alias belongs to another platform"
    );
}

#[cfg(any(unix, windows))]
#[test]
fn native_project_path_alias_preserves_hex_decode_errors() {
    let alias = format!(
        "{NATIVE_PROJECT_PATH_ALIAS_PREFIX}-{}-zz",
        native_project_path_platform()
    );
    assert_eq!(
        decode_native_project_path_alias(&alias).unwrap_err(),
        hex::decode("zz").unwrap_err().to_string()
    );
}

#[cfg(windows)]
#[test]
fn native_project_path_alias_preserves_windows_odd_length_error() {
    let alias = encode_native_project_path_alias(native_project_path_platform(), &[0]);
    assert_eq!(
        decode_native_project_path_alias(&alias).unwrap_err(),
        "native Windows project path alias has odd byte length"
    );
    assert_eq!(
        decode_native_project_path(native_project_path_platform(), vec![0]).unwrap_err(),
        "native Windows project path has odd byte length"
    );
}

#[cfg(windows)]
fn colliding_non_unicode_project_paths(root: &Path) -> (PathBuf, PathBuf) {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt as _;

    (
        root.join(OsString::from_wide(&[u16::from(b'p'), 0xd800])),
        root.join(OsString::from_wide(&[u16::from(b'p'), 0xd801])),
    )
}

#[cfg(any(unix, windows))]
async fn replace_native_alias_with_legacy(db: &GlobalDb, project_path: &Path, project_id: &str) {
    let native_alias = project_path_alias_key(project_path);
    let legacy_alias = GlobalDb::canonical_project_key(project_path);
    assert_ne!(native_alias, legacy_alias);
    db.conn
        .execute(
            "DELETE FROM project_aliases WHERE alias_path = ?1",
            params![native_alias],
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO project_aliases (alias_path, project_id, last_seen_at)
             VALUES (?1, ?2, 1)
             ON CONFLICT(alias_path) DO UPDATE SET project_id = excluded.project_id",
            params![legacy_alias, project_id],
        )
        .await
        .unwrap();
}

async fn create_conflicting_schema_view(db_path: &Path, view_name: &str) {
    let raw_db = Builder::new_local(db_path).build().await.unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute_batch(&format!(
            "CREATE VIEW {view_name} AS SELECT 1 AS incompatible"
        ))
        .await
        .unwrap();
}

async fn require_schema_reensure(db: &GlobalDb) {
    let slot = GLOBAL_DB_SLOTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(db.db_path())
        .and_then(Weak::upgrade)
        .expect("authoritative open has schema slot");
    slot.lock().await.ensured = false;
}

fn schema_authority_fixture(
    sequence: i64,
    label: &str,
) -> (DurableClaudeObservationV1, ClaudeSourceCursorV1) {
    schema_authority_fixture_with_scope(sequence, label, ObservationScopeV1::Profile)
}

fn schema_authority_fixture_with_scope(
    sequence: i64,
    label: &str,
    scope: ObservationScopeV1,
) -> (DurableClaudeObservationV1, ClaudeSourceCursorV1) {
    assert!(sequence > 0);
    let source =
        ClaudeSourceIdentityV1::new(SessionId::new("session.schema-contract").unwrap()).unwrap();
    let generation = ClaudeFileGenerationV1::new(7).unwrap();
    let start = u64::try_from(sequence - 1).unwrap() * 100;
    let end = start + 100;
    let identity = ClaudeObservationIdentityMaterialV1::new(
        source.clone(),
        scope.clone(),
        generation,
        ClaudeByteRangeV1::new(start, end).unwrap(),
    )
    .unwrap();
    let payload = json!({"kind": "schema_contract_fixture", "label": label});
    let payload_reference = PayloadReferenceV1::for_payload(&payload).unwrap();
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(format!("receipt.schema-contract.{sequence}")).unwrap(),
            ComponentVersion::new("sanitizer.schema-contract.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(payload_reference),
    )
    .unwrap();
    let observation = DurableClaudeObservationV1::new(
        identity,
        receipt,
        RetentionClass::new("retention.schema-contract").unwrap(),
        payload,
    )
    .unwrap();
    let cursor = ClaudeSourceCursorV1::new(source, scope, generation, end).unwrap();
    (observation, cursor)
}

async fn seed_observation(
    conn: &Connection,
    sequence: i64,
    label: &str,
) -> (DurableClaudeObservationV1, ClaudeSourceCursorV1) {
    seed_observation_with_scope(conn, sequence, label, ObservationScopeV1::Profile).await
}

async fn seed_observation_with_scope(
    conn: &Connection,
    sequence: i64,
    label: &str,
    scope: ObservationScopeV1,
) -> (DurableClaudeObservationV1, ClaudeSourceCursorV1) {
    let (observation, cursor) = schema_authority_fixture_with_scope(sequence, label, scope);
    let receipt = observation.receipt();
    let receipt_id = receipt.receipt().receipt_id().as_str();
    let payload_digest = observation.payload_reference().digest().as_str();
    let receipt_json = serde_json::to_string(receipt).unwrap();
    let observation_json = serde_json::to_string(&observation).unwrap();
    let cursor_json = serde_json::to_string(&cursor).unwrap();
    conn.execute(
        "INSERT INTO sanitization_receipts
         (receipt_id, sanitizer_version, payload_digest, receipt_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            receipt_id,
            receipt.receipt().sanitizer_version().as_str(),
            payload_digest,
            receipt_json
        ],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO observations
         (sequence, observation_id, payload_digest, receipt_id,
          observation_json, committed_cursor_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            sequence,
            observation.observation_id().as_str(),
            payload_digest,
            receipt_id,
            observation_json,
            cursor_json
        ],
    )
    .await
    .unwrap();
    (observation, cursor)
}

async fn seed_skip_projection(conn: &Connection, observation: &DurableClaudeObservationV1) {
    conn.execute(
        "INSERT INTO observation_projection_dispositions
         (projector_version, observation_id, receipt_id, reason)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
            observation.observation_id().as_str(),
            observation.receipt().receipt().receipt_id().as_str(),
            ProjectionSkipReason::NonConversationalRecord.as_str()
        ],
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn try_open_at_reports_observation_schema_failure() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    create_conflicting_schema_view(&db_path, "observations").await;

    let Err(error) = GlobalDb::try_open_at(&db_path).await else {
        panic!("observation schema conflict unexpectedly opened");
    };
    let TraceDecayError::Database { operation, message } = error else {
        panic!("unexpected error: {error}");
    };
    assert_eq!(operation, "migrate observation authority schema");
    assert!(message.contains("observations"), "{message}");
    assert!(GlobalDb::open_at(&db_path).await.is_none());
}

#[tokio::test]
async fn try_open_at_reports_observation_projection_schema_failure() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    create_conflicting_schema_view(&db_path, "observation_projection_provenance").await;

    let Err(error) = GlobalDb::try_open_at(&db_path).await else {
        panic!("observation projection schema conflict unexpectedly opened");
    };
    let TraceDecayError::Database { operation, message } = error else {
        panic!("unexpected error: {error}");
    };
    assert_eq!(operation, "initialize observation projection schema");
    assert!(message.contains("view"), "{message}");
    assert!(GlobalDb::open_at(&db_path).await.is_none());
}

#[tokio::test]
async fn try_open_at_rejects_observation_table_without_authority_constraints() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let raw_db = Builder::new_local(&db_path).build().await.unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute_batch(
            "CREATE TABLE observations (
                sequence INTEGER,
                observation_id TEXT NOT NULL,
                payload_digest TEXT NOT NULL,
                receipt_id TEXT NOT NULL,
                observation_json TEXT NOT NULL,
                committed_cursor_json TEXT NOT NULL
            )",
        )
        .await
        .unwrap();
    drop(raw_conn);
    drop(raw_db);

    let Err(error) = GlobalDb::try_open_at(&db_path).await else {
        panic!("constraintless observation table unexpectedly opened");
    };
    let TraceDecayError::Database { message, operation } = error else {
        panic!("unexpected error: {error}");
    };
    assert_eq!(operation, "validate global database authority schema");
    assert!(message.contains("observations"), "{message}");

    let raw_db = Builder::new_local(&db_path).build().await.unwrap();
    let raw_conn = raw_db.connect().unwrap();
    let mut rows = raw_conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE name IN (
                'sanitization_receipts', 'source_cursors', 'projection_queue',
                'observation_projection_provenance',
                'observation_projection_checkpoints',
                'observation_projection_aliases',
                'observation_projection_dispositions',
                'authority_audit_checkpoints',
                'observations_immutable_update', 'observations_immutable_delete'
             )",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0,
        "rejected authority schema changes must roll back atomically"
    );
}

#[tokio::test]
async fn legacy_idempotency_and_non_autoincrement_observations_migrate_canonically() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let (legacy_observation, legacy_cursor) = schema_authority_fixture(41, "legacy");
    let legacy_receipt = legacy_observation.receipt();
    let raw_db = Builder::new_local(&db_path).build().await.unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute_batch(
            "CREATE TABLE sanitization_receipts (
                receipt_id TEXT PRIMARY KEY,
                sanitizer_version TEXT NOT NULL,
                payload_digest TEXT NOT NULL,
                receipt_json TEXT NOT NULL
            );
             CREATE TABLE observations (
                sequence INTEGER PRIMARY KEY,
                observation_id TEXT NOT NULL UNIQUE,
                idempotency_key TEXT NOT NULL UNIQUE,
                payload_digest TEXT NOT NULL,
                receipt_id TEXT NOT NULL,
                observation_json TEXT NOT NULL,
                committed_cursor_json TEXT NOT NULL,
                FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
             );",
        )
        .await
        .unwrap();
    raw_conn
        .execute(
            "INSERT INTO sanitization_receipts VALUES (?1, ?2, ?3, ?4)",
            params![
                legacy_receipt.receipt().receipt_id().as_str(),
                legacy_receipt.receipt().sanitizer_version().as_str(),
                legacy_observation.payload_reference().digest().as_str(),
                serde_json::to_string(legacy_receipt).unwrap()
            ],
        )
        .await
        .unwrap();
    raw_conn
        .execute(
            "INSERT INTO observations VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                41_i64,
                legacy_observation.observation_id().as_str(),
                legacy_observation.idempotency_key().as_str(),
                legacy_observation.payload_reference().digest().as_str(),
                legacy_receipt.receipt().receipt_id().as_str(),
                serde_json::to_string(&legacy_observation).unwrap(),
                serde_json::to_string(&legacy_cursor).unwrap()
            ],
        )
        .await
        .unwrap();
    drop(raw_conn);
    drop(raw_db);

    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let mut columns = db
        .conn
        .query("SELECT name FROM pragma_table_xinfo('observations')", ())
        .await
        .unwrap();
    let mut names = Vec::new();
    while let Some(row) = columns.next().await.unwrap() {
        names.push(row.get::<String>(0).unwrap());
    }
    assert!(!names.iter().any(|name| name == "idempotency_key"));

    let mut rows = db
        .conn
        .query(
            "SELECT seq FROM sqlite_sequence WHERE name = 'observations'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        41
    );
    let mut rows = db
        .conn
        .query(
            "SELECT observation_id, observation_sequence FROM projection_queue",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(
        row.get::<String>(0).unwrap(),
        legacy_observation.observation_id().as_str()
    );
    assert_eq!(row.get::<i64>(1).unwrap(), 41);

    let (next_observation, next_cursor) = schema_authority_fixture(42, "next");
    let next_receipt = next_observation.receipt();
    db.conn
        .execute(
            "INSERT INTO sanitization_receipts VALUES (?1, ?2, ?3, ?4)",
            params![
                next_receipt.receipt().receipt_id().as_str(),
                next_receipt.receipt().sanitizer_version().as_str(),
                next_observation.payload_reference().digest().as_str(),
                serde_json::to_string(next_receipt).unwrap()
            ],
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO observations
                (observation_id, payload_digest, receipt_id, observation_json,
                 committed_cursor_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                next_observation.observation_id().as_str(),
                next_observation.payload_reference().digest().as_str(),
                next_receipt.receipt().receipt_id().as_str(),
                serde_json::to_string(&next_observation).unwrap(),
                serde_json::to_string(&next_cursor).unwrap()
            ],
        )
        .await
        .unwrap();
    let mut rows = db
        .conn
        .query(
            "SELECT sequence FROM observations WHERE observation_id = ?1",
            params![next_observation.observation_id().as_str()],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        42
    );
}

#[tokio::test]
async fn schema_validation_accepts_equivalent_table_level_primary_key() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let raw_db = Builder::new_local(&db_path).build().await.unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute_batch(
            "CREATE TABLE projects (
                path TEXT,
                tokens_saved INTEGER NOT NULL DEFAULT (0),
                PRIMARY KEY (path)
            )",
        )
        .await
        .unwrap();
    drop(raw_conn);
    drop(raw_db);

    GlobalDb::try_open_at(&db_path)
        .await
        .expect("structurally equivalent schema should open")
        .expect("global database");
}

#[tokio::test]
async fn schema_validation_rejects_incomplete_registry_table() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let raw_db = Builder::new_local(&db_path).build().await.unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute_batch(
            "CREATE TABLE code_projects (
                project_id TEXT PRIMARY KEY,
                canonical_root TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                last_seen_at INTEGER NOT NULL
            )",
        )
        .await
        .unwrap();
    drop(raw_conn);
    drop(raw_db);

    let Err(error) = GlobalDb::try_open_at(&db_path).await else {
        panic!("incomplete registry schema unexpectedly opened");
    };
    assert!(
        error.to_string().contains("incompatible number of columns"),
        "{error}"
    );
}

#[tokio::test]
async fn schema_validation_rejects_partial_required_index() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    db.conn
        .execute_batch(
            "DROP INDEX idx_project_aliases_project_id;
             CREATE INDEX idx_project_aliases_project_id
             ON project_aliases(project_id) WHERE last_seen_at > 0;",
        )
        .await
        .unwrap();
    require_schema_reensure(&db).await;
    drop(db);

    let Err(error) = GlobalDb::try_open_at(&db_path).await else {
        panic!("partial authority index unexpectedly opened");
    };
    assert!(
        error.to_string().contains("missing required index"),
        "{error}"
    );
}

#[tokio::test]
async fn schema_validation_rejects_hidden_generated_registry_column() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let raw_db = Builder::new_local(&db_path).build().await.unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute_batch(
            "CREATE TABLE projects (
                path TEXT PRIMARY KEY,
                tokens_saved INTEGER NOT NULL DEFAULT 0,
                derived INTEGER GENERATED ALWAYS AS (tokens_saved + 1) VIRTUAL
            )",
        )
        .await
        .unwrap();
    drop(raw_conn);
    drop(raw_db);

    let Err(error) = GlobalDb::try_open_at(&db_path).await else {
        panic!("hidden generated registry column unexpectedly opened");
    };
    assert!(error.to_string().contains("incompatible number of columns"));
}

#[tokio::test]
async fn cross_table_identity_constraints_reject_mismatched_rows() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("open global db");
    let first_root = dir.path().join("first");
    let second_root = dir.path().join("second");
    db.upsert_code_project("project_one", &first_root, None, None, None)
        .await
        .unwrap();
    db.upsert_code_project("project_two", &second_root, None, None, None)
        .await
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO store_instances
             (store_id, project_id, store_kind, storage_mode, store_relpath, created_at)
             VALUES ('store_one', 'project_one', 'sessions', 'central', 'sessions', 1)",
            (),
        )
        .await
        .unwrap();
    let graph_error = db
        .conn
        .execute(
            "INSERT INTO graph_scopes
             (graph_scope_id, project_id, store_id, branch_name, db_relpath)
             VALUES ('scope_bad', 'project_two', 'store_one', 'main', 'graph.db')",
            (),
        )
        .await
        .unwrap_err();
    assert!(graph_error.to_string().contains("store/project mismatch"));

    db.conn
        .execute_batch(
            "INSERT INTO sanitization_receipts
             (receipt_id, sanitizer_version, payload_digest, receipt_json)
             VALUES ('receipt_one', 'v1', 'digest_one', '{}');
             INSERT INTO sanitization_receipts
             (receipt_id, sanitizer_version, payload_digest, receipt_json)
             VALUES ('receipt_two', 'v1', 'digest_two', '{}');
             INSERT INTO observations
             (observation_id, payload_digest, receipt_id, observation_json, committed_cursor_json)
             VALUES ('observation_one', 'digest_one', 'receipt_one', '{}', '{}');",
        )
        .await
        .unwrap();
    let mut rows = db
        .conn
        .query(
            "SELECT sequence FROM observations WHERE observation_id = 'observation_one'",
            (),
        )
        .await
        .unwrap();
    let sequence = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap();
    drop(rows);

    let queue_error = db
        .conn
        .execute(
            "INSERT INTO projection_queue(observation_id, observation_sequence)
             VALUES ('observation_one', ?1)",
            params![sequence + 1],
        )
        .await
        .unwrap_err();
    assert!(
        queue_error
            .to_string()
            .contains("observation identity mismatch")
    );

    let provenance_error = db
        .conn
        .execute(
            "INSERT INTO observation_projection_provenance
             (projector_version, observation_id, receipt_id, output_provider,
              output_message_id, output_digest, message_created)
             VALUES ('v1', 'observation_one', 'receipt_two', 'claude', 'message', 'digest', 1)",
            (),
        )
        .await
        .unwrap_err();
    assert!(
        provenance_error
            .to_string()
            .contains("provenance receipt mismatch")
    );

    let disposition_error = db
        .conn
        .execute(
            "INSERT INTO observation_projection_dispositions
             (projector_version, observation_id, receipt_id, reason)
             VALUES ('v1', 'observation_one', 'receipt_two', 'invalid')",
            (),
        )
        .await
        .unwrap_err();
    assert!(
        disposition_error
            .to_string()
            .contains("disposition receipt mismatch")
    );
}

#[tokio::test]
async fn malformed_same_name_invariant_trigger_is_replaced() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    db.conn
        .execute_batch(
            "DROP TRIGGER projection_queue_identity_insert_v1;
             CREATE TRIGGER projection_queue_identity_insert_v1
             BEFORE INSERT ON projection_queue BEGIN SELECT 1; END;",
        )
        .await
        .unwrap();
    require_schema_reensure(&db).await;
    drop(db);

    let reopened = GlobalDb::open_at(&db_path).await.unwrap();
    let (observation, _) = seed_observation(&reopened.conn, 1, "observation_trigger").await;
    let error = reopened
        .conn
        .execute(
            "INSERT INTO projection_queue(observation_id, observation_sequence)
             VALUES (?1, 2)",
            params![observation.observation_id().as_str()],
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("observation identity mismatch"));
}

#[tokio::test]
async fn store_project_identity_cannot_be_reparented() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .unwrap();
    db.upsert_code_project("project_one", &dir.path().join("one"), None, None, None)
        .await
        .unwrap();
    db.upsert_code_project("project_two", &dir.path().join("two"), None, None, None)
        .await
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO store_instances
             (store_id, project_id, store_kind, storage_mode, store_relpath, created_at)
             VALUES ('store_one', 'project_one', 'sessions', 'central', 'sessions', 1)",
            (),
        )
        .await
        .unwrap();
    let error = db
        .conn
        .execute(
            "UPDATE store_instances SET project_id = 'project_two'
             WHERE store_id = 'store_one'",
            (),
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("store project identity is immutable")
    );
}

#[tokio::test]
async fn schema_reensure_repairs_projection_queue_to_checkpoint_frontier() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let (first, _) = seed_observation(&db.conn, 1, "observation_one").await;
    let (second, _) = seed_observation(&db.conn, 2, "observation_two").await;
    db.conn
        .execute(
            "INSERT INTO observation_projection_dispositions
             (projector_version, observation_id, receipt_id, reason)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
                first.observation_id().as_str(),
                first.receipt().receipt().receipt_id().as_str(),
                ProjectionSkipReason::NonConversationalRecord.as_str()
            ],
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO observation_projection_checkpoints(projector_version, last_sequence)
             VALUES (?1, 1)",
            params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO projection_queue(observation_id, observation_sequence)
             VALUES (?1, 1)",
            params![first.observation_id().as_str()],
        )
        .await
        .unwrap();
    require_schema_reensure(&db).await;
    drop(db);

    let reopened = GlobalDb::open_at(&db_path).await.unwrap();
    let mut rows = reopened
        .conn
        .query(
            "SELECT observation_id, observation_sequence FROM projection_queue",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(
        row.get::<String>(0).unwrap(),
        second.observation_id().as_str()
    );
    assert_eq!(row.get::<i64>(1).unwrap(), 2);
    assert!(rows.next().await.unwrap().is_none());
}

#[tokio::test]
async fn schema_reensure_lowers_checkpoint_without_contiguous_projection_evidence() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let (observation, _) = seed_observation(&db.conn, 1, "observation_one").await;
    db.conn
        .execute(
            "INSERT INTO observation_projection_checkpoints(projector_version, last_sequence)
             VALUES ('claude-session-message-v1', 2)",
            (),
        )
        .await
        .unwrap();
    require_schema_reensure(&db).await;
    drop(db);

    let reopened = GlobalDb::try_open_at(&db_path)
        .await
        .expect("invalid checkpoint should be repairable")
        .expect("global database");
    let mut rows = reopened
        .conn
        .query(
            "SELECT last_sequence FROM observation_projection_checkpoints
             WHERE projector_version = ?1",
            params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    drop(rows);
    let mut rows = reopened
        .conn
        .query(
            "SELECT observation_id FROM projection_queue WHERE observation_sequence = 1",
            (),
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
        observation.observation_id().as_str()
    );
}

#[tokio::test]
async fn schema_reensure_adds_receipt_id_to_legacy_cursor_advances() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at_without_structured_backfill(&db_path)
        .await
        .unwrap();
    db.conn
        .execute_batch(
            "DROP TABLE source_cursor_advances;
             CREATE TABLE source_cursor_advances (
                 source_json TEXT NOT NULL,
                 scope_json TEXT NOT NULL,
                 file_generation TEXT NOT NULL,
                 start_offset TEXT NOT NULL,
                 end_offset TEXT NOT NULL,
                 reason TEXT NOT NULL,
                 PRIMARY KEY(
                     source_json, scope_json, file_generation, start_offset, end_offset
                 )
             );",
        )
        .await
        .unwrap();
    db.close();

    let reopened = GlobalDb::open_at_without_structured_backfill(&db_path)
        .await
        .unwrap();
    assert!(
        table_column_exists(reopened.conn(), "source_cursor_advances", "receipt_id")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn schema_reensure_preserves_valid_nondurable_cursor_progress() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let (_, committed_cursor) = seed_observation(&db.conn, 1, "durable").await;
    let advanced_cursor = ClaudeSourceCursorV1::new(
        committed_cursor.source().clone(),
        committed_cursor.scope().clone(),
        committed_cursor.generation(),
        committed_cursor.byte_offset() + 50,
    )
    .unwrap();
    let source_json = serde_json::to_string(advanced_cursor.source()).unwrap();
    let scope_json = serde_json::to_string(advanced_cursor.scope()).unwrap();
    let advanced_json = serde_json::to_string(&advanced_cursor).unwrap();
    db.conn
        .execute(
            "INSERT INTO source_cursors(source_json, scope_json, cursor_json)
             VALUES (?1, ?2, ?3)",
            params![source_json.as_str(), scope_json.as_str(), advanced_json],
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO source_cursor_advances(
                source_json, scope_json, file_generation,
                start_offset, end_offset, reason
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'blank_frame')",
            params![
                source_json.as_str(),
                scope_json.as_str(),
                advanced_cursor.generation().file_id().to_string(),
                committed_cursor.byte_offset().to_string(),
                advanced_cursor.byte_offset().to_string()
            ],
        )
        .await
        .unwrap();
    require_schema_reensure(&db).await;
    drop(db);

    let reopened = GlobalDb::open_at(&db_path).await.unwrap();
    let mut rows = reopened
        .conn
        .query(
            "SELECT cursor_json FROM source_cursors
             WHERE source_json = ?1 AND scope_json = ?2",
            params![source_json, scope_json],
        )
        .await
        .unwrap();
    let cursor_json = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    assert_eq!(
        serde_json::from_str::<ClaudeSourceCursorV1>(&cursor_json).unwrap(),
        advanced_cursor
    );
}

#[tokio::test]
async fn schema_reensure_repairs_a_stale_present_committed_source_cursor() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let (_, committed_cursor) = seed_observation(&db.conn, 1, "stale-present-cursor").await;
    let stale_cursor = ClaudeSourceCursorV1::new(
        committed_cursor.source().clone(),
        committed_cursor.scope().clone(),
        committed_cursor.generation(),
        committed_cursor.byte_offset() - 1,
    )
    .unwrap();
    db.conn
        .execute(
            "INSERT INTO source_cursors(source_json, scope_json, cursor_json)
             VALUES (?1, ?2, ?3)",
            params![
                serde_json::to_string(stale_cursor.source()).unwrap(),
                serde_json::to_string(stale_cursor.scope()).unwrap(),
                serde_json::to_string(&stale_cursor).unwrap()
            ],
        )
        .await
        .unwrap();
    require_schema_reensure(&db).await;
    drop(db);

    let reopened = GlobalDb::open_at(&db_path).await.unwrap();
    let mut rows = reopened
        .conn
        .query("SELECT cursor_json FROM source_cursors", ())
        .await
        .unwrap();
    let cursor_json = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    assert_eq!(
        serde_json::from_str::<ClaudeSourceCursorV1>(&cursor_json).unwrap(),
        committed_cursor
    );
}

#[tokio::test]
async fn schema_reensure_reconstructs_a_missing_committed_source_cursor() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let (_, committed_cursor) = seed_observation(&db.conn, 1, "missing-cursor").await;
    require_schema_reensure(&db).await;
    drop(db);

    let reopened = GlobalDb::open_at(&db_path).await.unwrap();
    let mut rows = reopened
        .conn
        .query("SELECT cursor_json FROM source_cursors", ())
        .await
        .unwrap();
    let cursor_json = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<String>(0)
        .unwrap();
    assert_eq!(
        serde_json::from_str::<ClaudeSourceCursorV1>(&cursor_json).unwrap(),
        committed_cursor
    );
    assert!(rows.next().await.unwrap().is_none());
}

#[tokio::test]
async fn source_cursor_repair_canonicalizes_reordered_project_scope_json() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let scope = ObservationScopeV1::Project {
        project_id: ProjectId::new("project.schema-contract").unwrap(),
    };
    let (observation, cursor) =
        seed_observation_with_scope(&db.conn, 1, "reordered-scope", scope.clone()).await;
    let canonical_observation_json = serde_json::to_string(&observation).unwrap();
    let reordered_observation_json = canonical_observation_json.replace(
        "\"scope\":{\"kind\":\"project\",\"project_id\":\"project.schema-contract\"}",
        "\"scope\":{\"project_id\":\"project.schema-contract\",\"kind\":\"project\"}",
    );
    assert_ne!(reordered_observation_json, canonical_observation_json);
    db.conn
        .execute_batch(
            "DROP TRIGGER observations_immutable_update;
             DROP TRIGGER observations_immutable_delete;",
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "UPDATE observations SET observation_json = ?2 WHERE observation_id = ?1",
            params![
                observation.observation_id().as_str(),
                reordered_observation_json
            ],
        )
        .await
        .unwrap();
    require_schema_reensure(&db).await;
    drop(db);

    let reopened = GlobalDb::open_at(&db_path).await.unwrap();
    let mut rows = reopened
        .conn
        .query(
            "SELECT source_json, scope_json, cursor_json FROM source_cursors",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(
        row.get::<String>(0).unwrap(),
        serde_json::to_string(observation.source()).unwrap()
    );
    assert_eq!(
        row.get::<String>(1).unwrap(),
        serde_json::to_string(&scope).unwrap()
    );
    assert_eq!(
        row.get::<String>(2).unwrap(),
        serde_json::to_string(&cursor).unwrap()
    );
}

#[tokio::test]
async fn malformed_source_cursor_fails_before_missing_authority_is_repaired() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let (_, cursor) = seed_observation(&db.conn, 1, "malformed-cursor").await;
    db.conn
        .execute(
            "INSERT INTO source_cursors(source_json, scope_json, cursor_json)
             VALUES (?1, ?2, '{}')",
            params![
                serde_json::to_string(cursor.source()).unwrap(),
                serde_json::to_string(cursor.scope()).unwrap()
            ],
        )
        .await
        .unwrap();
    require_schema_reensure(&db).await;
    drop(db);

    let Err(error) = GlobalDb::try_open_at(&db_path).await else {
        panic!("malformed source cursor unexpectedly opened");
    };
    assert!(
        error
            .to_string()
            .contains("invalid source cursor authority JSON"),
        "{error}"
    );
    let raw_db = Builder::new_local(&db_path).build().await.unwrap();
    let raw_conn = raw_db.connect().unwrap();
    let mut rows = raw_conn
        .query(
            "SELECT (SELECT COUNT(*) FROM source_cursors),
                    (SELECT COUNT(*) FROM projection_queue)",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), 1);
    assert_eq!(row.get::<i64>(1).unwrap(), 0);
}

#[tokio::test]
async fn source_cursor_authority_keys_are_cross_checked() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let (_, cursor) = seed_observation(&db.conn, 1, "cursor-key-mismatch").await;
    let other_source =
        ClaudeSourceIdentityV1::new(SessionId::new("session.other").unwrap()).unwrap();
    db.conn
        .execute(
            "INSERT INTO source_cursors(source_json, scope_json, cursor_json)
             VALUES (?1, ?2, ?3)",
            params![
                serde_json::to_string(&other_source).unwrap(),
                serde_json::to_string(cursor.scope()).unwrap(),
                serde_json::to_string(&cursor).unwrap()
            ],
        )
        .await
        .unwrap();
    require_schema_reensure(&db).await;
    drop(db);

    let Err(error) = GlobalDb::try_open_at(&db_path).await else {
        panic!("mismatched source cursor authority unexpectedly opened");
    };
    assert!(
        error
            .to_string()
            .contains("source cursor authority keys disagree with cursor JSON"),
        "{error}"
    );
}

#[tokio::test]
async fn source_cursor_advance_authority_is_cross_checked() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let (_, cursor) = seed_observation(&db.conn, 1, "invalid-advance").await;
    db.conn
        .execute(
            "INSERT INTO source_cursor_advances(
                 source_json, scope_json, file_generation,
                 start_offset, end_offset, reason
             ) VALUES (?1, ?2, '7', '0', '10', 'unknown_reason')",
            params![
                serde_json::to_string(cursor.source()).unwrap(),
                serde_json::to_string(cursor.scope()).unwrap()
            ],
        )
        .await
        .unwrap();
    require_schema_reensure(&db).await;
    drop(db);

    let Err(error) = GlobalDb::try_open_at(&db_path).await else {
        panic!("invalid source cursor advance unexpectedly opened");
    };
    assert!(
        error
            .to_string()
            .contains("source cursor advance contains invalid authority evidence"),
        "{error}"
    );
}

#[tokio::test]
async fn source_cursor_advance_duplicate_requires_the_same_reason() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let source =
        ClaudeSourceIdentityV1::new(SessionId::new("session.advance-retry").unwrap()).unwrap();
    let scope = ObservationScopeV1::Profile;
    let generation = ClaudeFileGenerationV1::new(11).unwrap();
    let covered = ClaudeByteRangeV1::new(0, 10).unwrap();
    let advance = |reason| {
        ObservationCursorAdvance::new(
            source.clone(),
            scope.clone(),
            generation,
            None,
            covered,
            reason,
        )
        .unwrap()
    };

    assert_eq!(
        db.advance_observation_source_cursor_result(advance(NonDurableFrameReason::BlankFrame))
            .await
            .unwrap(),
        CursorAdvanceOutcome::Committed
    );
    assert_eq!(
        db.advance_observation_source_cursor_result(advance(NonDurableFrameReason::BlankFrame))
            .await
            .unwrap(),
        CursorAdvanceOutcome::ExactDuplicate
    );
    assert!(matches!(
        db.advance_observation_source_cursor_result(advance(NonDurableFrameReason::OutOfScope))
            .await,
        Err(ObservationStoreError::CursorAdvanceCollision)
    ));
    let update = db
        .conn
        .execute(
            "UPDATE source_cursor_advances SET reason = 'out_of_scope'",
            (),
        )
        .await
        .unwrap_err();
    assert!(
        update
            .to_string()
            .contains("source cursor advances are immutable")
    );
    let delete = db
        .conn
        .execute("DELETE FROM source_cursor_advances", ())
        .await
        .unwrap_err();
    assert!(
        delete
            .to_string()
            .contains("source cursor advances are immutable")
    );
}

#[tokio::test]
async fn malformed_receipt_authority_json_is_rejected_before_repair() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let (observation, _) = seed_observation(&db.conn, 1, "malformed-receipt").await;
    db.conn
        .execute_batch(
            "DROP TRIGGER sanitization_receipts_immutable_update_v1;
             DROP TRIGGER sanitization_receipts_immutable_delete_v1;",
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "UPDATE sanitization_receipts SET receipt_json = '{}'
             WHERE receipt_id = ?1",
            params![observation.receipt().receipt().receipt_id().as_str()],
        )
        .await
        .unwrap();
    require_schema_reensure(&db).await;
    drop(db);

    let Err(error) = GlobalDb::try_open_at(&db_path).await else {
        panic!("malformed receipt unexpectedly opened");
    };
    assert!(
        error
            .to_string()
            .contains("invalid sanitization receipt authority JSON"),
        "{error}"
    );
}

#[tokio::test]
async fn redundant_receipt_authority_columns_are_cross_checked() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let (observation, _) = seed_observation(&db.conn, 1, "receipt-column-mismatch").await;
    db.conn
        .execute_batch(
            "DROP TRIGGER sanitization_receipts_immutable_update_v1;
             DROP TRIGGER sanitization_receipts_immutable_delete_v1;",
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "UPDATE sanitization_receipts SET sanitizer_version = 'sanitizer.other.v1'
             WHERE receipt_id = ?1",
            params![observation.receipt().receipt().receipt_id().as_str()],
        )
        .await
        .unwrap();
    require_schema_reensure(&db).await;
    drop(db);

    let Err(error) = GlobalDb::try_open_at(&db_path).await else {
        panic!("mismatched receipt authority unexpectedly opened");
    };
    assert!(
        error
            .to_string()
            .contains("receipt authority columns disagree with receipt JSON"),
        "{error}"
    );
}

#[tokio::test]
async fn sanitization_receipts_are_immutable_after_commit() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .unwrap();
    let (observation, _) = seed_observation(&db.conn, 1, "immutable-receipt").await;
    let receipt_id = observation.receipt().receipt().receipt_id().as_str();
    for statement in [
        "UPDATE sanitization_receipts SET payload_digest = payload_digest WHERE receipt_id = ?1",
        "DELETE FROM sanitization_receipts WHERE receipt_id = ?1",
    ] {
        let error = db
            .conn
            .execute(statement, params![receipt_id])
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("sanitization receipts are immutable"),
            "{error}"
        );
    }
}

#[tokio::test]
async fn redundant_observation_authority_columns_are_cross_checked() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let (observation, _) = seed_observation(&db.conn, 1, "column-mismatch").await;
    db.conn
        .execute_batch(
            "DROP TRIGGER observations_immutable_update;
             DROP TRIGGER observations_immutable_delete;",
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "UPDATE observations SET payload_digest = ?2 WHERE observation_id = ?1",
            params![
                observation.observation_id().as_str(),
                format!("sha256:{}", "0".repeat(64))
            ],
        )
        .await
        .unwrap();
    require_schema_reensure(&db).await;
    drop(db);

    let Err(error) = GlobalDb::try_open_at(&db_path).await else {
        panic!("mismatched observation authority unexpectedly opened");
    };
    assert!(
        error
            .to_string()
            .contains("authority columns disagree with observation JSON"),
        "{error}"
    );
}

#[tokio::test]
async fn committed_cursor_is_cross_checked_against_observation_evidence() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let (observation, cursor) = seed_observation(&db.conn, 1, "cursor-mismatch").await;
    let mismatched = ClaudeSourceCursorV1::new(
        cursor.source().clone(),
        cursor.scope().clone(),
        ClaudeFileGenerationV1::new(cursor.generation().file_id() + 1).unwrap(),
        cursor.byte_offset(),
    )
    .unwrap();
    db.conn
        .execute_batch(
            "DROP TRIGGER observations_immutable_update;
             DROP TRIGGER observations_immutable_delete;",
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "UPDATE observations SET committed_cursor_json = ?2 WHERE observation_id = ?1",
            params![
                observation.observation_id().as_str(),
                serde_json::to_string(&mismatched).unwrap()
            ],
        )
        .await
        .unwrap();
    require_schema_reensure(&db).await;
    drop(db);

    let Err(error) = GlobalDb::try_open_at(&db_path).await else {
        panic!("mismatched committed cursor unexpectedly opened");
    };
    assert!(
        error
            .to_string()
            .contains("committed source cursor disagrees"),
        "{error}"
    );
}

#[tokio::test]
async fn checkpoint_with_a_missing_disposition_requeues_the_entire_suffix() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let (first, _) = seed_observation(&db.conn, 1, "first").await;
    let (second, _) = seed_observation(&db.conn, 2, "second").await;
    let (third, _) = seed_observation(&db.conn, 3, "third").await;
    for observation in [&first, &third] {
        db.conn
            .execute(
                "INSERT INTO observation_projection_dispositions
                 (projector_version, observation_id, receipt_id, reason)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
                    observation.observation_id().as_str(),
                    observation.receipt().receipt().receipt_id().as_str(),
                    ProjectionSkipReason::NonConversationalRecord.as_str()
                ],
            )
            .await
            .unwrap();
    }
    db.conn
        .execute(
            "INSERT INTO observation_projection_checkpoints(projector_version, last_sequence)
             VALUES (?1, 3)",
            params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .await
        .unwrap();
    require_schema_reensure(&db).await;
    drop(db);

    let reopened = GlobalDb::open_at(&db_path).await.unwrap();
    let mut rows = reopened
        .conn
        .query(
            "SELECT last_sequence FROM observation_projection_checkpoints
             WHERE projector_version = ?1",
            params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1
    );
    drop(rows);
    let mut rows = reopened
        .conn
        .query(
            "SELECT observation_id FROM projection_queue ORDER BY observation_sequence",
            (),
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
        second.observation_id().as_str()
    );
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        third.observation_id().as_str()
    );
    assert!(rows.next().await.unwrap().is_none());
}

#[tokio::test]
async fn conflicting_projection_outcomes_are_rejected_atomically() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let (observation, _) = seed_observation(&db.conn, 1, "conflicting-effects").await;
    seed_skip_projection(&db.conn, &observation).await;
    db.conn
        .execute(
            "INSERT INTO observation_projection_provenance
             (projector_version, observation_id, receipt_id, output_provider,
              output_message_id, output_digest, message_created)
             VALUES (?1, ?2, ?3, 'claude', 'invalid', 'sha256:invalid', 0)",
            params![
                CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
                observation.observation_id().as_str(),
                observation.receipt().receipt().receipt_id().as_str()
            ],
        )
        .await
        .unwrap();
    require_schema_reensure(&db).await;
    drop(db);

    let Err(error) = GlobalDb::try_open_at(&db_path).await else {
        panic!("conflicting projection outcomes unexpectedly opened");
    };
    assert!(
        error
            .to_string()
            .contains("exactly one skip outcome without an alias"),
        "{error}"
    );
    let raw_db = Builder::new_local(&db_path).build().await.unwrap();
    let raw_conn = raw_db.connect().unwrap();
    let mut rows = raw_conn
        .query("SELECT COUNT(*) FROM projection_queue", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
}

#[tokio::test]
async fn invalid_projection_skip_reason_is_rejected() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let (observation, _) = seed_observation(&db.conn, 1, "invalid-skip-reason").await;
    db.conn
        .execute(
            "INSERT INTO observation_projection_dispositions
             (projector_version, observation_id, receipt_id, reason)
             VALUES (?1, ?2, ?3, 'invented_reason')",
            params![
                CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
                observation.observation_id().as_str(),
                observation.receipt().receipt().receipt_id().as_str()
            ],
        )
        .await
        .unwrap();
    require_schema_reensure(&db).await;
    drop(db);

    let Err(error) = GlobalDb::try_open_at(&db_path).await else {
        panic!("invalid projection skip reason unexpectedly opened");
    };
    assert!(
        error
            .to_string()
            .contains("disposition disagrees with deterministic skip reason"),
        "{error}"
    );
}

#[tokio::test]
async fn projection_alias_on_skipped_observation_is_rejected() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let (observation, _) = seed_observation(&db.conn, 1, "alias-on-skip").await;
    seed_skip_projection(&db.conn, &observation).await;
    db.conn
        .execute(
            "INSERT INTO observation_projection_aliases
             (projector_version, observation_id, output_provider, output_message_id)
             VALUES (?1, ?2, 'claude', 'consolidated/source/invalid')",
            params![
                CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION,
                observation.observation_id().as_str()
            ],
        )
        .await
        .unwrap();
    require_schema_reensure(&db).await;
    drop(db);

    let Err(error) = GlobalDb::try_open_at(&db_path).await else {
        panic!("projection alias on skipped observation unexpectedly opened");
    };
    assert!(
        error.to_string().contains("invalid projection authority"),
        "{error}"
    );
}

#[tokio::test]
async fn authority_reensure_audits_only_new_append_suffixes() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    for sequence in 1..=64 {
        let (observation, _) =
            seed_observation(&db.conn, sequence, &format!("bounded-{sequence}")).await;
        seed_skip_projection(&db.conn, &observation).await;
    }
    require_schema_reensure(&db).await;
    drop(db);

    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let mut rows = db
        .conn
        .query(
            "SELECT last_receipts_audited, last_observations_audited,
                    last_dispositions_audited
             FROM authority_audit_checkpoints
             WHERE audit_name = 'observation-authority'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), 64);
    assert_eq!(row.get::<i64>(1).unwrap(), 64);
    assert_eq!(row.get::<i64>(2).unwrap(), 64);
    drop(row);
    drop(rows);
    require_schema_reensure(&db).await;
    drop(db);

    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let mut rows = db
        .conn
        .query(
            "SELECT last_receipts_audited, last_observations_audited,
                    last_dispositions_audited
             FROM authority_audit_checkpoints
             WHERE audit_name = 'observation-authority'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), 0);
    assert_eq!(row.get::<i64>(1).unwrap(), 0);
    assert_eq!(row.get::<i64>(2).unwrap(), 0);
}

#[tokio::test]
async fn authority_reensure_rejects_checkpoint_beyond_current_frontiers() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let (observation, _) = seed_observation(&db.conn, 1, "inflated-checkpoint").await;
    seed_skip_projection(&db.conn, &observation).await;
    require_schema_reensure(&db).await;
    drop(db);

    let db = GlobalDb::open_at(&db_path).await.unwrap();
    db.conn
        .execute(
            "UPDATE authority_audit_checkpoints SET
                receipt_rowid = 1000000,
                observation_sequence = 1000000,
                provenance_rowid = 1000000,
                disposition_rowid = 1000000,
                alias_rowid = 1000000,
                projection_checkpoint = 1000000
             WHERE audit_name = 'observation-authority'",
            (),
        )
        .await
        .unwrap();
    require_schema_reensure(&db).await;
    drop(db);

    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let mut rows = db
        .conn
        .query(
            "SELECT last_receipts_audited, last_observations_audited,
                    last_dispositions_audited
             FROM authority_audit_checkpoints
             WHERE audit_name = 'observation-authority'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<i64>(0).unwrap(), 1);
    assert_eq!(row.get::<i64>(1).unwrap(), 1);
    assert_eq!(row.get::<i64>(2).unwrap(), 1);
}

#[tokio::test]
async fn periodic_exhaustive_audit_rejects_old_row_corruption_at_equal_frontier() {
    let dir = tempfile::TempDir::new().unwrap();
    let db_path = dir.path().join("global.db");
    let db = GlobalDb::open_at(&db_path).await.unwrap();
    let (observation, _) = seed_observation(&db.conn, 1, "periodic-exhaustive").await;
    seed_skip_projection(&db.conn, &observation).await;
    require_schema_reensure(&db).await;
    drop(db);

    let db = GlobalDb::open_at(&db_path).await.unwrap();
    db.conn
        .execute_batch(
            "DROP TRIGGER sanitization_receipts_immutable_update_v1;
             UPDATE sanitization_receipts SET receipt_json = '{}';
             CREATE TRIGGER sanitization_receipts_immutable_update_v1
             BEFORE UPDATE ON sanitization_receipts BEGIN
                SELECT RAISE(ABORT, 'sanitization receipts are immutable');
             END;
             UPDATE authority_audit_checkpoints
             SET bounded_passes_since_exhaustive = 64
             WHERE audit_name = 'observation-authority';",
        )
        .await
        .unwrap();
    drop(db);

    let Err(error) = GlobalDb::try_open_at(&db_path).await else {
        panic!("old receipt corruption unexpectedly bypassed periodic exhaustive audit");
    };
    assert!(
        error
            .to_string()
            .contains("sanitization receipt authority JSON"),
        "{error}"
    );
}

#[tokio::test]
async fn try_open_at_prevalidates_projects_before_canonical_migration() {
    let dir = tempfile::TempDir::new().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let legacy = format!("{}/.", project.display());
    let db_path = dir.path().join("global.db");
    let raw_db = Builder::new_local(&db_path).build().await.unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute_batch(
            "CREATE TABLE projects (
                path TEXT PRIMARY KEY,
                tokens_saved INTEGER NOT NULL DEFAULT 0,
                unexpected TEXT
             )",
        )
        .await
        .unwrap();
    raw_conn
        .execute(
            "INSERT INTO projects(path, tokens_saved) VALUES (?1, 7)",
            params![legacy.as_str()],
        )
        .await
        .unwrap();
    drop(raw_conn);
    drop(raw_db);

    let Err(error) = GlobalDb::try_open_at(&db_path).await else {
        panic!("invalid projects table unexpectedly opened");
    };
    let TraceDecayError::Database { message, operation } = error else {
        panic!("unexpected error: {error}");
    };
    assert_eq!(operation, "validate global database authority schema");
    assert!(
        message.contains("incompatible number of columns"),
        "{message}"
    );

    let raw_db = Builder::new_local(&db_path).build().await.unwrap();
    let raw_conn = raw_db.connect().unwrap();
    let mut rows = raw_conn
        .query("SELECT path FROM projects", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next()
            .await
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        legacy
    );
    assert!(rows.next().await.unwrap().is_none());
}

#[tokio::test]
async fn canonical_project_migration_rolls_back_insert_when_delete_fails() {
    let dir = tempfile::TempDir::new().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let legacy = format!("{}/.", project.display());
    let canonical = project.display().to_string();
    let db_path = dir.path().join("global.db");
    let raw_db = Builder::new_local(&db_path).build().await.unwrap();
    let raw_conn = raw_db.connect().unwrap();
    raw_conn
        .execute_batch(
            "CREATE TABLE projects (
                path TEXT PRIMARY KEY,
                tokens_saved INTEGER NOT NULL DEFAULT 0
            );
             CREATE TRIGGER reject_project_delete
             BEFORE DELETE ON projects
             BEGIN SELECT RAISE(ABORT, 'delete rejected'); END;",
        )
        .await
        .unwrap();
    raw_conn
        .execute(
            "INSERT INTO projects(path, tokens_saved) VALUES (?1, 7)",
            params![legacy.as_str()],
        )
        .await
        .unwrap();
    drop(raw_conn);
    drop(raw_db);

    let Err(error) = GlobalDb::try_open_at(&db_path).await else {
        panic!("failing canonical migration unexpectedly opened");
    };
    assert!(error.to_string().contains("delete rejected"), "{error}");

    let raw_db = Builder::new_local(&db_path).build().await.unwrap();
    let raw_conn = raw_db.connect().unwrap();
    let mut rows = raw_conn
        .query(
            "SELECT path FROM projects WHERE path IN (?1, ?2) ORDER BY path",
            params![legacy.as_str(), canonical.as_str()],
        )
        .await
        .unwrap();
    let mut paths = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        paths.push(row.get::<String>(0).unwrap());
    }
    assert_eq!(paths, vec![legacy]);
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn unique_legacy_non_unicode_alias_migrates_to_native_key() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("open global db");
    let (project_path, _) = colliding_non_unicode_project_paths(dir.path());
    db.upsert_code_project("proj_legacy", &project_path, None, None, None)
        .await
        .expect("register legacy project");
    replace_native_alias_with_legacy(&db, &project_path, "proj_legacy").await;

    let context = db
        .project_registry_context_by_alias(&project_path)
        .await
        .expect("unique legacy owner should migrate");
    assert_eq!(context.project.project_id, "proj_legacy");
    assert_eq!(
        db.project_id_by_alias_key(&project_path_alias_key(&project_path))
            .await
            .as_deref(),
        Some("proj_legacy")
    );
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn colliding_legacy_non_unicode_alias_fails_closed() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("open global db");
    let (first, second) = colliding_non_unicode_project_paths(dir.path());
    db.upsert_code_project("proj_first", &first, None, None, None)
        .await
        .expect("register first project");
    db.upsert_code_project("proj_second", &second, None, None, None)
        .await
        .expect("register second project");
    replace_native_alias_with_legacy(&db, &first, "proj_first").await;
    db.conn
        .execute(
            "DELETE FROM project_aliases WHERE alias_path = ?1",
            params![project_path_alias_key(&second)],
        )
        .await
        .unwrap();

    assert!(db.project_registry_context_by_alias(&first).await.is_none());
    assert!(
        db.project_id_by_alias_key(&project_path_alias_key(&first))
            .await
            .is_none()
    );
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn bulk_project_deletion_preserves_native_non_unicode_aliases() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("open global db");
    let (first, second) = colliding_non_unicode_project_paths(dir.path());
    assert_ne!(
        project_path_alias_key(&first),
        project_path_alias_key(&second)
    );
    assert_eq!(
        GlobalDb::canonical_project_key(&first),
        GlobalDb::canonical_project_key(&second)
    );
    db.upsert(&first, 11).await;
    db.upsert(&second, 22).await;

    assert_eq!(
        db.delete_project_paths(std::slice::from_ref(&first)).await,
        1
    );
    assert_eq!(db.get_project_tokens(&first).await, 0);
    assert_eq!(db.get_project_tokens(&second).await, 22);
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn lossless_project_path_listing_decodes_native_aliases() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("open global db");
    let (project, _) = colliding_non_unicode_project_paths(dir.path());
    db.upsert(&project, 11).await;
    db.upsert_code_project("proj_lossless_listing", &project, None, None, None)
        .await
        .expect("register project");

    assert_eq!(
        db.try_list_project_paths().await.unwrap(),
        vec![project.clone()]
    );
    assert!(
        db.try_list_project_alias_paths()
            .await
            .unwrap()
            .contains(&project)
    );
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn code_project_listing_uses_latest_lossless_primary_root() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("open global db");
    let (first, second) = colliding_non_unicode_project_paths(dir.path());
    db.upsert_code_project("proj_moved", &first, None, None, None)
        .await
        .expect("register first root");
    db.upsert_code_project("proj_moved", &second, None, None, None)
        .await
        .expect("move primary root");

    assert_eq!(
        db.try_list_code_project_paths(usize::MAX).await.unwrap(),
        vec![second]
    );
    assert_eq!(
        db.project_id_by_alias_key(&project_path_alias_key(&first))
            .await
            .as_deref(),
        Some("proj_moved")
    );
}

#[tokio::test]
async fn code_project_listing_preserves_literal_unicode_replacement_character() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .unwrap();
    let project = dir.path().join("literal-\u{fffd}-project");
    db.upsert_code_project("proj_unicode", &project, None, None, None)
        .await
        .unwrap();

    assert_eq!(
        db.try_list_code_project_paths(usize::MAX).await.unwrap(),
        vec![project]
    );
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn code_project_listing_prefers_explicit_unicode_root_after_non_unicode_move() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .unwrap();
    let (old_non_unicode, _) = colliding_non_unicode_project_paths(dir.path());
    let current_unicode = dir.path().join("current-unicode");
    db.upsert_code_project("proj_moved_unicode", &old_non_unicode, None, None, None)
        .await
        .unwrap();
    db.upsert_code_project("proj_moved_unicode", &current_unicode, None, None, None)
        .await
        .unwrap();

    assert_eq!(
        db.try_list_code_project_paths(usize::MAX).await.unwrap(),
        vec![current_unicode]
    );
    assert_eq!(
        db.project_id_by_alias_key(&project_path_alias_key(&old_non_unicode))
            .await
            .as_deref(),
        Some("proj_moved_unicode")
    );
}

#[tokio::test]
async fn legacy_code_project_listing_rejects_display_without_lossless_alias_evidence() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .unwrap();
    let project = dir.path().join("literal-\u{fffd}-legacy");
    db.upsert_code_project("proj_no_evidence", &project, None, None, None)
        .await
        .unwrap();
    db.conn
        .execute(
            "DELETE FROM project_aliases WHERE project_id = 'proj_no_evidence'",
            (),
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "UPDATE code_projects SET primary_root_platform = NULL,
             primary_root_bytes = NULL, primary_root_last_seen_at = NULL
             WHERE project_id = 'proj_no_evidence'",
            (),
        )
        .await
        .unwrap();

    let error = db
        .try_list_code_project_paths(usize::MAX)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("no current lossless legacy root evidence")
    );
}

#[tokio::test]
async fn code_project_listing_rejects_incomplete_primary_root_tuple() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .unwrap();
    let project = dir.path().join("project");
    db.upsert_code_project("proj_incomplete", &project, None, None, None)
        .await
        .unwrap();
    db.conn
        .execute(
            "UPDATE code_projects SET primary_root_bytes = NULL
             WHERE project_id = 'proj_incomplete'",
            (),
        )
        .await
        .unwrap();

    let error = db
        .try_list_code_project_paths(usize::MAX)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("incomplete primary root"));
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn legacy_code_project_listing_fails_closed_on_ambiguous_native_roots() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("open global db");
    let (first, second) = colliding_non_unicode_project_paths(dir.path());
    db.upsert_code_project("proj_legacy", &first, None, None, None)
        .await
        .expect("register project");
    db.upsert_project_alias(&second, "proj_legacy")
        .await
        .expect("register historical alias");
    let current_evidence_at = 42;
    db.conn
        .execute(
            "UPDATE code_projects SET last_seen_at = ?2 WHERE project_id = ?1",
            params!["proj_legacy", current_evidence_at],
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "UPDATE project_aliases SET last_seen_at = ?2
             WHERE project_id = ?1 AND alias_path IN (?3, ?4)",
            params![
                "proj_legacy",
                current_evidence_at,
                project_path_alias_key(&first),
                project_path_alias_key(&second)
            ],
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "UPDATE code_projects SET primary_root_platform = NULL,
             primary_root_bytes = NULL, primary_root_last_seen_at = NULL
             WHERE project_id = 'proj_legacy'",
            (),
        )
        .await
        .unwrap();

    let error = db
        .try_list_code_project_paths(usize::MAX)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("ambiguous legacy"), "{error}");
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn legacy_code_project_listing_uses_unique_current_plain_alias_evidence() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .unwrap();
    let (old_non_unicode, _) = colliding_non_unicode_project_paths(dir.path());
    let current_unicode = dir.path().join("current-unicode");
    db.upsert_code_project("proj_legacy_move", &old_non_unicode, None, None, None)
        .await
        .unwrap();
    db.upsert_code_project("proj_legacy_move", &current_unicode, None, None, None)
        .await
        .unwrap();
    db.conn
        .execute(
            "UPDATE project_aliases SET last_seen_at = 10
             WHERE alias_path = ?1",
            params![project_path_alias_key(&old_non_unicode)],
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "UPDATE project_aliases SET last_seen_at = 20
             WHERE alias_path = ?1",
            params![project_path_alias_key(&current_unicode)],
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "UPDATE code_projects SET last_seen_at = 20,
             primary_root_platform = NULL, primary_root_bytes = NULL,
             primary_root_last_seen_at = NULL
             WHERE project_id = 'proj_legacy_move'",
            (),
        )
        .await
        .unwrap();

    assert_eq!(
        db.try_list_code_project_paths(usize::MAX).await.unwrap(),
        vec![current_unicode]
    );
}

#[tokio::test]
async fn bulk_project_deletion_accepts_string_paths() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("open global db");
    let project = dir.path().join("project");
    let project_text = project.to_string_lossy().into_owned();
    db.upsert(&project, 11).await;

    assert_eq!(db.delete_projects(&[]).await, 0);
    assert_eq!(db.delete_projects(&[project_text]).await, 1);
    assert_eq!(db.get_project_tokens(&project).await, 0);
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn unique_legacy_non_unicode_git_common_alias_migrates_to_native_key() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("open global db");
    let project_root = dir.path().join("project");
    let (git_common_dir, _) = colliding_non_unicode_project_paths(dir.path());
    db.upsert_code_project(
        "proj_common",
        &project_root,
        Some(&git_common_dir),
        None,
        None,
    )
    .await
    .expect("register project");
    let native_alias = format!("git-common-dir:{}", project_path_alias_key(&git_common_dir));
    let legacy_alias = format!(
        "git-common-dir:{}",
        GlobalDb::canonical_project_key(&git_common_dir)
    );
    db.conn
        .execute(
            "DELETE FROM project_aliases WHERE alias_path = ?1",
            params![native_alias.as_str()],
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO project_aliases (alias_path, project_id, last_seen_at)
             VALUES (?1, 'proj_common', 1)",
            params![legacy_alias],
        )
        .await
        .unwrap();

    assert_eq!(
        db.project_id_by_git_common_dir_alias(&git_common_dir)
            .await
            .as_deref(),
        Some("proj_common")
    );
    assert_eq!(
        db.project_id_by_alias_key(&native_alias).await.as_deref(),
        Some("proj_common")
    );
}

#[test]
fn global_db_disables_mmap_on_every_platform() {
    assert_eq!(global_db_mmap_size_guard(), 0);
}

#[tokio::test]
async fn concurrent_full_opens_singleflight_schema_but_use_independent_connections() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("global.db");
    let (first, second, third, fourth) = tokio::join!(
        GlobalDb::open_at(&path),
        GlobalDb::open_at(&path),
        GlobalDb::open_at(&path),
        GlobalDb::open_at(&path),
    );
    let first = first.expect("first open");
    let opened = [
        second.expect("second open"),
        third.expect("third open"),
        fourth.expect("fourth open"),
    ];
    for db in &opened {
        assert!(!Arc::ptr_eq(&first.inner, &db.inner));
    }

    first.conn().execute("BEGIN", ()).await.unwrap();
    for db in &opened {
        db.conn().execute("BEGIN", ()).await.unwrap();
    }
    first.conn().execute("ROLLBACK", ()).await.unwrap();
    for db in &opened {
        db.conn().execute("ROLLBACK", ()).await.unwrap();
    }
}

#[tokio::test]
async fn cancelled_authoritative_transaction_isolated_from_retained_connection_and_cleans_payload()
{
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("global.db");
    let db = Arc::new(GlobalDb::open_at(&path).await.expect("global DB open"));
    let session = SessionRecord {
        provider: "codex".to_string(),
        session_id: "cancelled-transaction".to_string(),
        project_key: "project".to_string(),
        project_path: dir.path().display().to_string(),
        title: None,
        started_at: None,
        ended_at: None,
        transcript_path: Some(dir.path().join("session.jsonl").display().to_string()),
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    };
    let (created_tx, created_rx) = tokio::sync::oneshot::channel();
    let task_db = Arc::clone(&db);
    let task_session = session.clone();
    let task = tokio::spawn(async move {
        let _writer = task_db.transaction.lock().await;
        let transaction = task_db.begin_authoritative_transaction().await.unwrap();
        assert!(GlobalDb::upsert_session_in_existing_tx(&transaction, &task_session).await);
        let mut payload_rollback =
            crate::sessions::lcm::payload::PayloadFileRollback::begin_cancellation_safe(
                &task_db.storage_root,
            );
        let payload = crate::sessions::lcm::payload::write_external_payload_tracked(
            &task_db.storage_root,
            crate::sessions::lcm::payload::ExternalPayloadWrite {
                provider: "codex",
                session_id: "cancelled-transaction",
                message_id: "cancelled-message",
                kind: "tool_output",
                content: "payload created inside a transaction that will be cancelled",
                metadata_json: None,
            },
            &mut payload_rollback,
        )
        .unwrap();
        created_tx.send(payload.payload_ref).unwrap();
        std::future::pending::<()>().await;
    });

    let payload_ref = created_rx.await.expect("payload creation signal");
    let payload_path =
        crate::sessions::lcm::payload::payload_dir(&db.storage_root).join(&payload_ref);
    assert!(payload_path.is_file());

    let mut rows = db
        .conn()
        .query(
            "SELECT 1 FROM sessions WHERE provider = ?1 AND session_id = ?2",
            params!["codex", "cancelled-transaction"],
        )
        .await
        .expect("retained read must not join the fresh transaction");
    assert!(rows.next().await.unwrap().is_none());
    drop(rows);

    db.conn()
        .execute_batch("PRAGMA busy_timeout = 0;")
        .await
        .unwrap();
    assert!(!GlobalDb::upsert_session_in_existing_tx(&db.conn, &session).await);

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(!payload_path.exists());
    db.conn()
        .execute_batch("PRAGMA busy_timeout = 5000;")
        .await
        .unwrap();
    assert!(
        db.get_session("codex", "cancelled-transaction")
            .await
            .is_none()
    );
    assert!(db.upsert_session(&session).await);
    assert!(
        db.get_session("codex", "cancelled-transaction")
            .await
            .is_some()
    );
}

#[tokio::test]
async fn cancelled_lcm_lifecycle_mutation_rolls_back_and_releases_writer() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Arc::new(
        GlobalDb::open_at(&dir.path().join("global.db"))
            .await
            .expect("global DB open"),
    );
    let update = crate::sessions::lcm::LcmLifecycleUpdate {
        provider: "cursor".to_string(),
        conversation_id: "cancelled-lifecycle".to_string(),
        current_session_id: "cancelled-lifecycle".to_string(),
        current_frontier_store_id: None,
        last_finalized_session_id: None,
        last_finalized_frontier_store_id: None,
        maintenance_debt: vec![crate::sessions::lcm::LcmMaintenanceDebt::RawBacklog {
            from_store_id: 1,
            to_store_id: 2,
        }],
    };
    let (written_tx, written_rx) = tokio::sync::oneshot::channel();
    let task_db = Arc::clone(&db);
    let task_update = update.clone();
    let task = tokio::spawn(async move {
        let _writer = task_db.transaction.lock().await;
        let transaction = task_db.begin_authoritative_transaction().await.unwrap();
        crate::sessions::lcm::compression::update_lifecycle(&transaction, task_update)
            .await
            .unwrap();
        written_tx.send(()).unwrap();
        std::future::pending::<()>().await;
    });

    written_rx.await.expect("lifecycle write signal");
    assert!(
        db.lcm_lifecycle_state("cursor", "cancelled-lifecycle")
            .await
            .is_err(),
        "retained reader must not observe the uncommitted lifecycle state"
    );

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(
        db.lcm_lifecycle_state("cursor", "cancelled-lifecycle")
            .await
            .is_err(),
        "cancellation must roll back lifecycle state and maintenance debt"
    );

    let state = db
        .lcm_update_lifecycle(update.clone())
        .await
        .expect("writer must be reusable after cancellation");
    assert_eq!(state.provider, update.provider);
    assert_eq!(state.conversation_id, update.conversation_id);
    assert_eq!(state.maintenance_debt, update.maintenance_debt);
}

#[tokio::test]
async fn analytics_batch_error_rolls_back_prior_rows_and_releases_writer() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("global DB open");
    db.conn()
        .execute_batch(
            "CREATE TRIGGER fail_analytics_batch
             BEFORE INSERT ON analytics_events
             WHEN NEW.event_kind = 'force_failure'
             BEGIN
                 SELECT RAISE(ABORT, 'forced analytics failure');
             END;",
        )
        .await
        .unwrap();

    let event = |event_kind: &str| AnalyticsEventInsert {
        provider: "codex".to_string(),
        project_id: "project".to_string(),
        session_id: Some("session".to_string()),
        timestamp: 1,
        event_kind: event_kind.to_string(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: None,
        outcome: None,
        metadata_json: None,
    };
    assert!(
        db.append_analytics_events(&[event("valid"), event("force_failure")])
            .await
            .is_err()
    );

    let mut rows = db
        .conn()
        .query("SELECT COUNT(*) FROM analytics_events", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    drop(rows);

    db.conn()
        .execute("DROP TRIGGER fail_analytics_batch", ())
        .await
        .unwrap();
    assert_eq!(
        db.append_analytics_events(&[event("after_failure")])
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn turn_batch_error_rolls_back_prior_rows_and_releases_writer() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("global DB open");
    db.conn()
        .execute_batch(
            "CREATE TRIGGER fail_turn_batch
             BEFORE INSERT ON turns
             WHEN NEW.message_id = 'force-failure'
             BEGIN
                 SELECT RAISE(ABORT, 'forced turn failure');
             END;",
        )
        .await
        .unwrap();

    let turn = |message_id: &str| crate::types::CostTurn {
        message_id: message_id.to_string(),
        project_hash: "project".to_string(),
        session_id: "session".to_string(),
        model: "test-model".to_string(),
        timestamp: 1,
        input_tokens: 1,
        output_tokens: 1,
        cache_write_tokens: 0,
        cache_read_tokens: 0,
        cost_usd: 0.01,
        category: "test".to_string(),
        tool_names: String::new(),
    };
    assert_eq!(
        db.insert_turns(&[turn("valid"), turn("force-failure")])
            .await,
        0
    );

    let mut rows = db
        .conn()
        .query("SELECT COUNT(*) FROM turns", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    drop(rows);

    db.conn()
        .execute("DROP TRIGGER fail_turn_batch", ())
        .await
        .unwrap();
    assert_eq!(db.insert_turns(&[turn("after-failure")]).await, 1);
}

#[tokio::test]
async fn global_db_slot_uses_database_authority_canonical_identity() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("nested")).unwrap();
    let direct_path = dir.path().join("global.db");
    let alias_path = dir.path().join("nested").join("..").join("global.db");
    let direct = DatabaseAuthority::for_runtime(&direct_path, "direct slot identity").unwrap();
    let alias = DatabaseAuthority::for_runtime(&alias_path, "alias slot identity").unwrap();

    assert_eq!(
        direct.canonical_database_path(),
        alias.canonical_database_path()
    );
    assert!(Arc::ptr_eq(
        &global_db_slot(&direct),
        &global_db_slot(&alias)
    ));
}

#[tokio::test]
async fn assuming_schema_open_cannot_poison_full_schema_ensure() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("global.db");
    std::fs::File::create(&path).unwrap();

    let raw = GlobalDb::open_at_assuming_schema(&path)
        .await
        .expect("raw assuming-schema open");
    let mut rows = raw
        .conn()
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'projects'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    drop(rows);
    let raw_inner = Arc::downgrade(&raw.inner);
    raw.close();

    let ensured = GlobalDb::open_at(&path).await.expect("full schema open");
    let mut rows = ensured
        .conn()
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'projects'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1
    );
    assert!(raw_inner.upgrade().is_none());
}

#[tokio::test]
async fn distinct_global_db_paths_do_not_share_an_initialization_lock() {
    let dir = tempfile::TempDir::new().unwrap();
    let first_path = dir.path().join("first.db");
    let second_path = dir.path().join("second.db");
    let first_authority =
        DatabaseAuthority::for_runtime(&first_path, "hold first global DB slot").unwrap();
    let first_slot = global_db_slot(&first_authority);
    let _first_guard = first_slot.lock().await;

    let second = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        GlobalDb::open_at_without_structured_backfill(&second_path),
    )
    .await
    .expect("unrelated global DB path waited on the first path's slot")
    .expect("open unrelated global DB path");
    let second_authority =
        DatabaseAuthority::for_runtime(&second_path, "verify second global DB path").unwrap();
    assert_eq!(second.db_path(), second_authority.canonical_database_path());
}

#[tokio::test]
async fn read_only_open_is_independent_and_cannot_poison_writer() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("global.db");
    let seed = GlobalDb::open_at(&path).await.expect("seed writable open");
    drop(seed);
    let reader = GlobalDb::open_read_only_at(&path)
        .await
        .expect("read-only open");
    let writable = GlobalDb::open_at(&path).await.expect("writable open");
    assert!(!Arc::ptr_eq(&writable.inner, &reader.inner));
    assert!(
        reader
            .conn()
            .execute("CREATE TABLE forbidden_reader_write (id INTEGER)", ())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn runtime_open_without_authority_scope_fails_closed() {
    let path = std::env::current_dir()
        .unwrap()
        .join("target/global-db-no-authority/global.db");
    if path.starts_with(std::env::temp_dir()) {
        return;
    }
    assert!(GlobalDb::open_at(&path).await.is_none());
}

#[tokio::test]
async fn try_open_at_preserves_authority_error() {
    let path = std::env::current_dir()
        .unwrap()
        .join("target/global-db-authority-error/global.db");
    if path.starts_with(std::env::temp_dir()) {
        return;
    }
    let Err(error) = GlobalDb::try_open_at(&path).await else {
        panic!("unauthorized global DB open unexpectedly succeeded");
    };
    let message = error.to_string();
    assert!(
        message
            .contains("database access requires managed-daemon or exclusive-maintenance authority"),
        "{message}"
    );
    assert!(message.contains("open global database"), "{message}");
    let displayed = path.display().to_string();
    #[cfg(windows)]
    assert!(
        message
            .replace('\\', "/")
            .contains(&displayed.replace('\\', "/")),
        "{message}"
    );
    #[cfg(not(windows))]
    assert!(message.contains(&displayed), "{message}");
}

#[tokio::test]
async fn isolated_temp_database_opens_without_ambient_daemon() {
    let dir = tempfile::TempDir::new().unwrap();
    let _db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("temp test open");
}

#[test]
fn explicit_project_path_selector_keeps_names_and_paths_separate() {
    assert!(!GlobalDb::is_explicit_project_path_selector("target"));
    assert!(!GlobalDb::is_explicit_project_path_selector(" proj_123 "));
    assert!(GlobalDb::is_explicit_project_path_selector("."));
    assert!(GlobalDb::is_explicit_project_path_selector(".."));
    assert!(GlobalDb::is_explicit_project_path_selector("./target"));
    assert!(GlobalDb::is_explicit_project_path_selector("../target"));
    assert!(GlobalDb::is_explicit_project_path_selector("/tmp/target"));
    assert!(GlobalDb::is_explicit_project_path_selector(r"..\target"));
}

#[tokio::test]
async fn session_column_migration_tolerates_duplicate_column_race() {
    // In-memory DB: the duplicate-column race only needs one connection,
    // so the on-disk sqlite file adds nothing but I/O.
    let db = Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                PRIMARY KEY(provider, session_id)
            );",
    )
    .await
    .unwrap();

    assert!(
        !table_column_exists(&conn, "sessions", "parent_session_id")
            .await
            .unwrap()
    );

    conn.execute("ALTER TABLE sessions ADD COLUMN parent_session_id TEXT", ())
        .await
        .unwrap();

    assert!(
        add_table_column_after_missing_check(
            &conn,
            "sessions",
            "parent_session_id",
            "ALTER TABLE sessions ADD COLUMN parent_session_id TEXT",
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn code_projects_seen_within_applies_window_and_limit() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("open global db");

    let now = crate::tracedecay::current_timestamp();
    // (project_id, last_seen_at)
    let rows = [
        ("proj_recent", now - 60),       // 1 min ago  -> in window
        ("proj_mid", now - 3 * 86_400),  // 3 days ago -> in window
        ("proj_old", now - 30 * 86_400), // 30 days ago-> outside 14d window
    ];
    for (project_id, last_seen) in rows {
        db.conn
            .execute(
                "INSERT INTO code_projects
                     (project_id, canonical_root, display_root, git_common_dir,
                      git_remote_url, default_branch, created_at, last_seen_at)
                     VALUES (?1, ?2, ?3, NULL, NULL, NULL, ?4, ?4)",
                params![
                    project_id,
                    format!("/root/{project_id}"),
                    project_id,
                    last_seen
                ],
            )
            .await
            .unwrap();
    }

    // 14-day window keeps the two recent projects, most-recent first.
    let within = db.code_projects_seen_within(14 * 86_400, 10).await;
    let ids: Vec<&str> = within.iter().map(|p| p.project_id.as_str()).collect();
    assert_eq!(ids, vec!["proj_recent", "proj_mid"]);

    // Limit caps the result even when more projects are in-window.
    let capped = db.code_projects_seen_within(14 * 86_400, 1).await;
    let capped_ids: Vec<&str> = capped.iter().map(|p| p.project_id.as_str()).collect();
    assert_eq!(capped_ids, vec!["proj_recent"]);
}

#[tokio::test]
async fn search_code_projects_matches_any_whitespace_term() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("open global db");

    for (project_id, root) in [
        ("proj_rsbuild", "/repos/rsbuild-plugin-react-router"),
        ("proj_rspack", "/repos/rspack"),
        ("proj_unrelated", "/repos/unrelated"),
    ] {
        db.upsert_code_project(project_id, Path::new(root), None, None, Some("main"))
            .await
            .expect("code project should upsert");
    }
    db.upsert_code_project(
        "proj_remote_only",
        Path::new("/repos/remote-only"),
        None,
        Some("https://token:secret@example.test/remote-only.git"),
        Some("main"),
    )
    .await
    .expect("code project with remote should upsert");

    let matches = db.search_code_projects("rsbuild rspack", 10).await;
    let ids: Vec<&str> = matches
        .iter()
        .map(|project| project.project_id.as_str())
        .collect();

    assert!(ids.contains(&"proj_rsbuild"), "ids: {ids:?}");
    assert!(ids.contains(&"proj_rspack"), "ids: {ids:?}");
    assert!(!ids.contains(&"proj_unrelated"), "ids: {ids:?}");

    let remote_name_matches = db.search_code_projects("remote-only.git", 10).await;
    let remote_name_ids: Vec<&str> = remote_name_matches
        .iter()
        .map(|project| project.project_id.as_str())
        .collect();
    assert!(
        remote_name_ids.contains(&"proj_remote_only"),
        "remote_name_ids: {remote_name_ids:?}"
    );

    let remote_matches = db.search_code_projects("secret", 10).await;
    assert!(
        remote_matches.is_empty(),
        "remote credential text must not be searchable: {remote_matches:?}"
    );
}
