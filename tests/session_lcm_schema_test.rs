use std::path::Path;

use tempfile::TempDir;
use tokensave::global_db::GlobalDb;

async fn create_legacy_sessions_db(db_path: &Path) {
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
        VALUES ('cursor', 'legacy-session', '/tmp/project', '/tmp/project');
        INSERT INTO session_messages(provider, message_id, session_id, role, ordinal, text)
        VALUES ('cursor', 'legacy-message', 'legacy-session', 'assistant', 1, 'legacy text');",
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

#[tokio::test]
async fn lcm_schema_migrates_legacy_sessions_db_in_place() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tokensave").join("sessions.db");
    create_legacy_sessions_db(&db_path).await;

    let db = GlobalDb::open_at(&db_path).await.expect("global db open");

    assert!(table_exists(&db_path, "session_schema_migrations").await);
    assert!(table_exists(&db_path, "lcm_raw_messages").await);
    assert!(table_exists(&db_path, "lcm_raw_messages_fts").await);
    assert_eq!(
        db.lcm_schema_version().await.unwrap(),
        tokensave::sessions::lcm::LCM_SCHEMA_VERSION
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
        tokensave::sessions::lcm::LcmStorageKind::Inline
    );
    assert!(legacy.legacy_source);
    assert!(!legacy.legacy_truncated);
}

#[tokio::test]
async fn lcm_schema_migration_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join(".tokensave").join("sessions.db");
    create_legacy_sessions_db(&db_path).await;

    let db = GlobalDb::open_at(&db_path).await.expect("global db open");
    assert_eq!(
        db.lcm_schema_version().await.unwrap(),
        tokensave::sessions::lcm::LCM_SCHEMA_VERSION
    );
    drop(db);

    let reopened = GlobalDb::open_at(&db_path).await.expect("global db reopen");
    assert_eq!(
        reopened.lcm_schema_version().await.unwrap(),
        tokensave::sessions::lcm::LCM_SCHEMA_VERSION
    );
    assert_eq!(
        schema_version(&db_path).await,
        tokensave::sessions::lcm::LCM_SCHEMA_VERSION
    );
    assert_eq!(row_count(&db_path, "lcm_raw_messages").await, 1);
}
