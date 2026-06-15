use std::path::Path;

use tempfile::TempDir;
use tracedecay::global_db::GlobalDb;

async fn create_legacy_sessions_db(db_path: &Path) {
    create_legacy_sessions_db_with_text(db_path, "legacy text").await;
}

async fn create_legacy_sessions_db_with_text(db_path: &Path, legacy_text: &str) {
    std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();

    let old_db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = old_db.connect().unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            project_key TEXT NOT NULL,
            project_path TEXT NOT NULL,
            title TEXT,
            started_at INTEGER,
            ended_at INTEGER,
            transcript_path TEXT,
            metadata_json TEXT,
            PRIMARY KEY(provider, session_id)
        );
        CREATE TABLE session_messages (
            provider TEXT NOT NULL,
            message_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            timestamp INTEGER,
            ordinal INTEGER NOT NULL,
            text TEXT NOT NULL,
            kind TEXT,
            model TEXT,
            tool_names TEXT,
            source_path TEXT,
            source_offset INTEGER,
            metadata_json TEXT,
            PRIMARY KEY(provider, message_id)
        );
        INSERT INTO sessions(provider, session_id, project_key, project_path)
        VALUES ('cursor', 'legacy-session', '/tmp/project', '/tmp/project');",
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO session_messages(provider, message_id, session_id, role, ordinal, text)
         VALUES ('cursor', 'legacy-message', 'legacy-session', 'assistant', 1, ?1)",
        libsql::params![legacy_text],
    )
    .await
    .unwrap();
    drop(conn);
    drop(old_db);
}

async fn table_exists(db_path: &Path, table: &str) -> bool {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT 1 FROM sqlite_master WHERE name = ?1 AND type IN ('table', 'view')",
            libsql::params![table],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().is_some()
}

async fn row_count(db_path: &Path, table: &str) -> i64 {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let mut rows = conn.query(&sql, ()).await.unwrap();
    let row = rows.next().await.unwrap().unwrap();
    row.get(0).unwrap()
}

async fn fts_legacy_message_ids(db_path: &Path) -> Vec<String> {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT raw.message_id
             FROM lcm_raw_messages_fts
             JOIN lcm_raw_messages raw ON raw.store_id = lcm_raw_messages_fts.rowid
             WHERE lcm_raw_messages_fts MATCH 'legacy'
             ORDER BY raw.message_id",
            (),
        )
        .await
        .unwrap();
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        ids.push(row.get(0).unwrap());
    }
    ids
}

async fn schema_version(db_path: &Path) -> i64 {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT version FROM session_schema_migrations WHERE name = 'lcm'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    row.get(0).unwrap()
}

async fn migration_applied_at(db_path: &Path) -> i64 {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT applied_at FROM session_schema_migrations WHERE name = 'lcm'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    row.get(0).unwrap()
}

async fn set_migration_applied_at(db_path: &Path, applied_at: i64) {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute(
        "UPDATE session_schema_migrations
         SET applied_at = ?1
         WHERE name = 'lcm'",
        libsql::params![applied_at],
    )
    .await
    .unwrap();
}

async fn set_migration_version(db_path: &Path, version: i64) {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute(
        "UPDATE session_schema_migrations
         SET version = ?1
         WHERE name = 'lcm'",
        libsql::params![version],
    )
    .await
    .unwrap();
}

/// Rewrites the raw-message FTS objects into the pre-v3 shape (role +
/// metadata_json indexed alongside index_text) and stamps the requested
/// schema version, simulating a database written by an older tracedecay.
async fn downgrade_raw_fts_to_v2(db_path: &Path) {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS lcm_raw_messages_fts_insert;
         DROP TRIGGER IF EXISTS lcm_raw_messages_fts_delete;
         DROP TRIGGER IF EXISTS lcm_raw_messages_fts_update;
         DROP TABLE IF EXISTS lcm_raw_messages_fts;
         CREATE VIRTUAL TABLE lcm_raw_messages_fts USING fts5(
             index_text, role, metadata_json,
             content='lcm_raw_messages',
             content_rowid='store_id'
         );
         CREATE TRIGGER lcm_raw_messages_fts_insert
             AFTER INSERT ON lcm_raw_messages BEGIN
                 INSERT INTO lcm_raw_messages_fts(rowid, index_text, role, metadata_json)
                 VALUES (NEW.store_id, NEW.index_text, NEW.role, NEW.metadata_json);
             END;
         CREATE TRIGGER lcm_raw_messages_fts_delete
             AFTER DELETE ON lcm_raw_messages BEGIN
                 INSERT INTO lcm_raw_messages_fts(
                     lcm_raw_messages_fts, rowid, index_text, role, metadata_json
                 )
                 VALUES ('delete', OLD.store_id, OLD.index_text, OLD.role, OLD.metadata_json);
             END;
         CREATE TRIGGER lcm_raw_messages_fts_update
             AFTER UPDATE ON lcm_raw_messages BEGIN
                 INSERT INTO lcm_raw_messages_fts(
                     lcm_raw_messages_fts, rowid, index_text, role, metadata_json
                 )
                 VALUES ('delete', OLD.store_id, OLD.index_text, OLD.role, OLD.metadata_json);
                 INSERT INTO lcm_raw_messages_fts(rowid, index_text, role, metadata_json)
                 VALUES (NEW.store_id, NEW.index_text, NEW.role, NEW.metadata_json);
             END;
         INSERT INTO lcm_raw_messages_fts(lcm_raw_messages_fts) VALUES('rebuild');
         UPDATE session_schema_migrations SET version = 2 WHERE name = 'lcm';",
    )
    .await
    .unwrap();
}

async fn fts_message_ids_matching(db_path: &Path, query: &str) -> Vec<String> {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT raw.message_id
             FROM lcm_raw_messages_fts
             JOIN lcm_raw_messages raw ON raw.store_id = lcm_raw_messages_fts.rowid
             WHERE lcm_raw_messages_fts MATCH ?1
             ORDER BY raw.message_id",
            libsql::params![query],
        )
        .await
        .unwrap();
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        ids.push(row.get(0).unwrap());
    }
    ids
}

async fn raw_fts_object_sql(db_path: &Path) -> Vec<String> {
    let db = libsql::Builder::new_local(db_path).build().await.unwrap();
    let conn = db.connect().unwrap();
    let mut rows = conn
        .query(
            "SELECT sql FROM sqlite_master
             WHERE name IN ('lcm_raw_messages_fts',
                            'lcm_raw_messages_fts_insert',
                            'lcm_raw_messages_fts_delete',
                            'lcm_raw_messages_fts_update')",
            (),
        )
        .await
        .unwrap();
    let mut sqls = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        sqls.push(row.get(0).unwrap());
    }
    sqls
}

#[tokio::test]
async fn lcm_schema_migrates_legacy_sessions_db_in_place() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    create_legacy_sessions_db(&db_path).await;

    let db = GlobalDb::open_at(&db_path).await.expect("global db open");

    assert!(table_exists(&db_path, "session_schema_migrations").await);
    assert!(table_exists(&db_path, "lcm_raw_messages").await);
    assert!(table_exists(&db_path, "lcm_raw_messages_fts").await);
    assert_eq!(
        db.lcm_schema_version().await.unwrap(),
        tracedecay::sessions::lcm::LCM_SCHEMA_VERSION
    );

    let legacy = db
        .lcm_load_raw_message("cursor", "legacy-message")
        .await
        .expect("legacy message should be carried into raw store");
    assert_eq!(legacy.provider, "cursor");
    assert_eq!(legacy.message_id, "legacy-message");
    assert_eq!(legacy.session_id, "legacy-session");
    assert_eq!(legacy.role, "assistant");
    assert_eq!(legacy.ordinal, 1);
    assert_eq!(legacy.content, "legacy text");
    assert_eq!(
        legacy.storage_kind,
        tracedecay::sessions::lcm::LcmStorageKind::Inline
    );
    assert!(legacy.legacy_source);
    assert!(!legacy.legacy_truncated);
    assert_eq!(
        fts_legacy_message_ids(&db_path).await,
        vec!["legacy-message".to_string()]
    );
}

#[tokio::test]
async fn lcm_schema_marks_legacy_truncated_messages() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    let legacy_text = "legacy text\n[truncated by tracedecay]";
    create_legacy_sessions_db_with_text(&db_path, legacy_text).await;

    let db = GlobalDb::open_at(&db_path).await.expect("global db open");
    let legacy = db
        .lcm_load_raw_message("cursor", "legacy-message")
        .await
        .expect("legacy message should be carried into raw store");

    assert_eq!(legacy.content, legacy_text);
    assert!(legacy.legacy_source);
    assert!(legacy.legacy_truncated);
}

#[tokio::test]
async fn lcm_schema_migration_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    create_legacy_sessions_db(&db_path).await;

    let db = GlobalDb::open_at(&db_path).await.expect("global db open");
    assert_eq!(
        db.lcm_schema_version().await.unwrap(),
        tracedecay::sessions::lcm::LCM_SCHEMA_VERSION
    );
    drop(db);

    let reopened = GlobalDb::open_at(&db_path).await.expect("global db reopen");
    assert_eq!(
        reopened.lcm_schema_version().await.unwrap(),
        tracedecay::sessions::lcm::LCM_SCHEMA_VERSION
    );
    assert_eq!(
        schema_version(&db_path).await,
        tracedecay::sessions::lcm::LCM_SCHEMA_VERSION
    );
    assert_eq!(row_count(&db_path, "lcm_raw_messages").await, 1);
    assert_eq!(
        fts_legacy_message_ids(&db_path).await,
        vec!["legacy-message".to_string()]
    );
}

// Schema v3 narrows the raw-message FTS index to index_text only, matching
// hermes-lcm `build_message_fts_spec` (store.py:173-204) which indexes the
// content column alone. Migrating a v2 database must restructure the FTS
// objects, carry the searchable rows forward, and stop role/metadata text
// from satisfying unqualified MATCH queries.
#[tokio::test]
async fn lcm_schema_v3_migration_restructures_raw_fts_and_preserves_search() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    create_legacy_sessions_db(&db_path).await;

    // Establish the schema, then rewrite the FTS objects into the pre-v3
    // shape with the version marker set back to 2.
    let db = GlobalDb::open_at(&db_path).await.expect("global db open");
    drop(db);
    downgrade_raw_fts_to_v2(&db_path).await;
    assert_eq!(schema_version(&db_path).await, 2);
    assert_eq!(
        fts_message_ids_matching(&db_path, "assistant").await,
        vec!["legacy-message".to_string()],
        "v2 fixture must over-match via the indexed role column"
    );

    let migrated = GlobalDb::open_at(&db_path).await.expect("global db reopen");
    assert_eq!(
        migrated.lcm_schema_version().await.unwrap(),
        tracedecay::sessions::lcm::LCM_SCHEMA_VERSION
    );
    drop(migrated);

    // The restructured objects no longer index role/metadata_json.
    let sqls = raw_fts_object_sql(&db_path).await;
    assert_eq!(sqls.len(), 4, "FTS table and three triggers must exist");
    for sql in &sqls {
        assert!(
            !sql.contains("metadata_json"),
            "migrated FTS object still references metadata_json: {sql}"
        );
    }

    // Search results carried forward; role text no longer matches.
    assert_eq!(
        fts_message_ids_matching(&db_path, "legacy").await,
        vec!["legacy-message".to_string()],
        "content search results must survive the migration"
    );
    assert!(
        fts_message_ids_matching(&db_path, "assistant")
            .await
            .is_empty(),
        "role text must not match after the v3 restructure"
    );

    // Idempotent re-open: structure and results are stable.
    let reopened = GlobalDb::open_at(&db_path)
        .await
        .expect("idempotent reopen");
    assert_eq!(
        reopened.lcm_schema_version().await.unwrap(),
        tracedecay::sessions::lcm::LCM_SCHEMA_VERSION
    );
    drop(reopened);
    assert_eq!(
        fts_message_ids_matching(&db_path, "legacy").await,
        vec!["legacy-message".to_string()]
    );
    assert!(fts_message_ids_matching(&db_path, "assistant")
        .await
        .is_empty());
}

// Mirrors hermes-lcm `run_versioned_migrations` (db_bootstrap.py:580-601):
// version steps are monotonic and `set_schema_version(conn, current_version)`
// never lowers a marker written by a newer release. Opening a database whose
// LCM schema version is newer than this binary must not downgrade the marker
// or re-run the legacy carry-forward against data the newer schema owns.
#[tokio::test]
async fn lcm_schema_future_version_is_preserved_without_remigration() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    create_legacy_sessions_db(&db_path).await;

    let db = GlobalDb::open_at(&db_path).await.expect("global db open");
    assert_eq!(
        db.lcm_schema_version().await.unwrap(),
        tracedecay::sessions::lcm::LCM_SCHEMA_VERSION
    );
    drop(db);

    // Simulate a database last touched by a newer tracedecay: bump the version
    // marker past this binary and have the newer schema relocate carried rows
    // out of lcm_raw_messages.
    let future_version = tracedecay::sessions::lcm::LCM_SCHEMA_VERSION + 97;
    set_migration_version(&db_path, future_version).await;
    set_migration_applied_at(&db_path, 456).await;
    {
        let raw_db = libsql::Builder::new_local(&db_path).build().await.unwrap();
        let conn = raw_db.connect().unwrap();
        conn.execute("DELETE FROM lcm_raw_messages", ())
            .await
            .unwrap();
    }
    assert_eq!(row_count(&db_path, "lcm_raw_messages").await, 0);

    let reopened = GlobalDb::open_at(&db_path).await.expect("global db reopen");
    assert_eq!(
        reopened.lcm_schema_version().await.unwrap(),
        future_version,
        "future schema version marker must not be downgraded"
    );
    drop(reopened);
    assert_eq!(schema_version(&db_path).await, future_version);
    assert_eq!(migration_applied_at(&db_path).await, 456);
    assert_eq!(
        row_count(&db_path, "lcm_raw_messages").await,
        0,
        "legacy carry-forward must not re-run against a newer schema's data"
    );
}

#[tokio::test]
async fn lcm_schema_current_version_reopen_skips_migration_update() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tracedecay").join("sessions.db");
    create_legacy_sessions_db(&db_path).await;

    let db = GlobalDb::open_at(&db_path).await.expect("global db open");
    assert_eq!(
        db.lcm_schema_version().await.unwrap(),
        tracedecay::sessions::lcm::LCM_SCHEMA_VERSION
    );
    drop(db);

    set_migration_applied_at(&db_path, 123).await;
    assert_eq!(migration_applied_at(&db_path).await, 123);

    let reopened = GlobalDb::open_at(&db_path).await.expect("global db reopen");
    assert_eq!(
        reopened.lcm_schema_version().await.unwrap(),
        tracedecay::sessions::lcm::LCM_SCHEMA_VERSION
    );
    assert_eq!(migration_applied_at(&db_path).await, 123);
}
