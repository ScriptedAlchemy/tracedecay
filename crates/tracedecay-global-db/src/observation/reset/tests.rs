use tempfile::TempDir;

use crate::tests::harness::open_registered_test_database_fixture;
use tracedecay_runtime_core::db::TestDatabaseRuntimeScope;
use tracedecay_runtime_core::errors::TraceDecayError;

use super::reset_refused_observation_authority;

async fn install_registered_store(path: &std::path::Path) {
    let admitted =
        open_registered_test_database_fixture(path, TestDatabaseRuntimeScope::ProfileSessions)
            .await
            .expect("install the registered sessions schema");
    drop(admitted);
}

/// Replaces the canonical `observations` table with the pre-release
/// `idempotency_key` shape that admission refuses.
fn install_legacy_observation_shape(conn: &rusqlite::Connection) {
    conn.pragma_update(None, "foreign_keys", false)
        .expect("disable foreign keys for fixture seeding");
    conn.execute_batch(
        "DROP TABLE observations;
         CREATE TABLE observations (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            observation_id TEXT NOT NULL UNIQUE,
            idempotency_key TEXT NOT NULL UNIQUE,
            payload_digest TEXT NOT NULL,
            receipt_id TEXT NOT NULL,
            observation_json TEXT NOT NULL,
            committed_cursor_json TEXT NOT NULL,
            FOREIGN KEY(receipt_id) REFERENCES sanitization_receipts(receipt_id)
         );
         INSERT INTO observations
            (observation_id, idempotency_key, payload_digest, receipt_id,
             observation_json, committed_cursor_json)
         VALUES ('observation.legacy', 'idempotency.legacy', 'digest.legacy',
                 'receipt.legacy', '{}', '{}');",
    )
    .expect("install the refused pre-release observation shape");
}

fn seed_preserved_transcript_rows(conn: &rusqlite::Connection) {
    conn.execute_batch(
        "INSERT INTO sessions(provider, session_id, project_key, project_path)
         VALUES ('claude', 'session.fixture', 'project.fixture', '/project/fixture');
         INSERT INTO session_messages(provider, message_id, session_id, role, ordinal, text)
         VALUES ('claude', 'message.fixture', 'session.fixture', 'user', 0, 'projected output');",
    )
    .expect("seed transcript metadata and one projector-output message");
}

fn table_exists(conn: &rusqlite::Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get::<_, bool>(0),
    )
    .unwrap()
}

fn count(conn: &rusqlite::Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |row| {
        row.get::<_, i64>(0)
    })
    .unwrap()
}

#[tokio::test]
async fn refused_observation_shape_resets_scoped_and_readmits() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("sessions.db");
    install_registered_store(&database_path).await;
    {
        let raw = rusqlite::Connection::open(&database_path).unwrap();
        seed_preserved_transcript_rows(&raw);
        install_legacy_observation_shape(&raw);
    }

    let refusal = match open_registered_test_database_fixture(
        &database_path,
        TestDatabaseRuntimeScope::ProfileSessions,
    )
    .await
    {
        Ok(_) => panic!("the pre-release observation shape must refuse admission"),
        Err(error) => error,
    };
    let (authority, reason) = refusal
        .reset_required_context()
        .unwrap_or_else(|| panic!("expected the typed ResetRequired state, got: {refusal}"));
    assert_eq!(authority, "observations");
    assert!(
        reason.contains("no sanctioned migration") || reason.contains("branch-local"),
        "the refusal must say why no migration exists: {reason}"
    );

    let report = {
        let mut raw = rusqlite::Connection::open(&database_path).unwrap();
        reset_refused_observation_authority(&mut raw)
            .expect("scoped reset of the refused authority")
    };
    assert!(
        report
            .reset_tables
            .iter()
            .any(|table| table == "observations"),
        "the refused table must be part of the reset: {report:?}"
    );
    assert_eq!(report.cleared_session_message_rows, 1);

    let readmitted = open_registered_test_database_fixture(
        &database_path,
        TestDatabaseRuntimeScope::ProfileSessions,
    )
    .await
    .expect("the reset store must readmit at the canonical schema");
    drop(readmitted);

    let raw = rusqlite::Connection::open(&database_path).unwrap();
    assert_eq!(
        count(&raw, "observations"),
        0,
        "the refused authority must be recreated empty"
    );
    let has_idempotency_column = raw
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM pragma_table_xinfo('observations')
                WHERE name = 'idempotency_key'
             )",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap();
    assert!(
        !has_idempotency_column,
        "the recreated table must carry the canonical shape"
    );
    assert_eq!(
        count(&raw, "sessions"),
        1,
        "transcript metadata outside the refused authority must be preserved"
    );
    assert_eq!(
        count(&raw, "session_messages"),
        0,
        "recoverable projector output must be cleared with its provenance"
    );
    assert_eq!(
        count(&raw, "remote_deletion_tombstones"),
        0,
        "unrelated authorities must survive the scoped reset with their schema intact"
    );
}

#[tokio::test]
async fn healthy_observation_authority_refuses_the_scoped_reset() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("sessions.db");
    install_registered_store(&database_path).await;

    let mut raw = rusqlite::Connection::open(&database_path).unwrap();
    let error = reset_refused_observation_authority(&mut raw)
        .expect_err("a healthy authority must never be reset");
    assert!(
        matches!(
            &error,
            TraceDecayError::Config { message } if message.contains("not in a refused state")
        ),
        "unexpected error resetting a healthy authority: {error}"
    );
    assert!(
        table_exists(&raw, "observations"),
        "a refused reset must mutate nothing"
    );
}

#[tokio::test]
async fn durable_temporal_dependents_fail_the_scoped_reset_closed() {
    let directory = TempDir::new().unwrap();
    let database_path = directory.path().join("sessions.db");
    install_registered_store(&database_path).await;
    {
        let raw = rusqlite::Connection::open(&database_path).unwrap();
        install_legacy_observation_shape(&raw);
        raw.execute_batch(
            "INSERT INTO session_temporal_observation_effects
                (observation_id, observation_sequence, session_id, receipt_id,
                 effect_digest, output_count, recorded_at)
             VALUES ('observation.legacy', 1, 'session.fixture', 'receipt.legacy',
                     'digest.effect', 0, 1)",
        )
        .expect("seed one durable temporal dependent row");
    }

    let mut raw = rusqlite::Connection::open(&database_path).unwrap();
    let error = reset_refused_observation_authority(&mut raw)
        .expect_err("durable temporal dependents must fail the scoped reset closed");
    assert!(
        matches!(
            &error,
            TraceDecayError::Config { message }
                if message.contains("would orphan")
                    && message.contains("session_temporal_observation_effects")
        ),
        "unexpected error for durable dependents: {error}"
    );
    assert!(
        table_exists(&raw, "observations"),
        "a failed-closed reset must mutate nothing"
    );
    assert_eq!(
        count(&raw, "session_temporal_observation_effects"),
        1,
        "durable dependent rows must be preserved"
    );
}
