use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, TestConnection, params};

use crate::{LCM_SCAN_PAGE_ROWS, schema, summary_convergence};

#[tokio::test]
async fn retained_queue_page_is_keyset_bounded_and_candidate_read_avoids_raw_corpus() {
    let temp = tempfile::tempdir().unwrap();
    let conn = TestConnection::open(&temp.path().join("sessions.db"));
    conn.execute_batch(
        "CREATE TABLE sessions (
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            project_key TEXT NOT NULL,
            project_path TEXT NOT NULL,
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
            metadata_json TEXT,
            PRIMARY KEY(provider, message_id)
         );
         INSERT INTO sessions(provider, session_id, project_key, project_path)
         VALUES ('cursor', 'large-corpus', 'project.large', '/large');",
    )
    .await
    .unwrap();
    schema::ensure_lcm_schema(&conn).await.unwrap();
    let transaction = conn
        .transaction_with_behavior(
            tracedecay_runtime_core::db::engine::TransactionBehavior::Immediate,
        )
        .await
        .unwrap();
    for ordinal in 1..=4_096_i64 {
        transaction
            .execute(
                "INSERT INTO lcm_raw_messages (
                    provider, message_id, session_id, role, ordinal, content,
                    content_hash, storage_kind, snippet_text, index_text, metadata_json
                 ) VALUES ('cursor', ?1, 'large-corpus', 'assistant', ?2, 'body',
                           ?1, 'inline', 'body', 'body', '{}')",
                params![format!("message-{ordinal}"), ordinal],
            )
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();

    let page = summary_convergence::backfill_queue_page(&*conn, LCM_SCAN_PAGE_ROWS as usize)
        .await
        .unwrap();
    assert_eq!(page.rows_scanned, LCM_SCAN_PAGE_ROWS as usize);
    assert!(page.has_more);

    let mut plan = conn
        .query(
            &format!(
                "EXPLAIN QUERY PLAN {}",
                summary_convergence::NEXT_CANDIDATE_SQL
            ),
            params![i64::MAX],
        )
        .await
        .unwrap();
    let mut details = Vec::new();
    while let Some(row) = plan.next().await.unwrap() {
        details.push(row.get::<String>(3).unwrap());
    }
    assert!(
        details
            .iter()
            .any(|detail| detail.contains("idx_lcm_summary_convergence_due")),
        "candidate query did not use the due-work index: {details:?}"
    );
    assert!(
        details
            .iter()
            .all(|detail| !detail.contains("lcm_raw_messages")),
        "candidate query reached the raw corpus: {details:?}"
    );
    assert_eq!(
        summary_convergence::next_candidate(&*conn, i64::MAX)
            .await
            .unwrap()
            .unwrap()
            .session_id,
        "large-corpus"
    );
}

#[tokio::test]
async fn current_profiles_install_the_unreleased_queue_shape_in_place() {
    let temp = tempfile::tempdir().unwrap();
    let conn = TestConnection::open(&temp.path().join("sessions.db"));
    conn.execute_batch(
        "CREATE TABLE sessions (
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            project_key TEXT NOT NULL,
            project_path TEXT NOT NULL,
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
            metadata_json TEXT,
            PRIMARY KEY(provider, message_id)
         );",
    )
    .await
    .unwrap();
    schema::ensure_lcm_schema(&conn).await.unwrap();
    let version = schema::schema_version(&*conn).await.unwrap();
    conn.execute_batch(
        "DROP TRIGGER lcm_summary_convergence_raw_insert;
         DROP TRIGGER lcm_summary_convergence_raw_unprotected_update;
         DROP TABLE lcm_summary_convergence_queue;",
    )
    .await
    .unwrap();

    schema::ensure_lcm_schema(&conn).await.unwrap();

    assert_eq!(schema::schema_version(&*conn).await.unwrap(), version);
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE type = 'table' AND name = 'lcm_summary_convergence_queue'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1
    );
}
