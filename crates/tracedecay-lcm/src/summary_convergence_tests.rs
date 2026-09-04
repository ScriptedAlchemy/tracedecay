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
         DROP TABLE lcm_summary_convergence_queue;
         CREATE TABLE lcm_summary_convergence_queue (
            queue_id INTEGER PRIMARY KEY AUTOINCREMENT,
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            newest_raw_store_id INTEGER NOT NULL,
            protection_frontier_store_id INTEGER NOT NULL DEFAULT 0,
            attempted_raw_store_id INTEGER NOT NULL DEFAULT 0,
            state TEXT NOT NULL DEFAULT 'pending',
            failure_code TEXT,
            failure_count INTEGER NOT NULL DEFAULT 0,
            next_attempt_at_ms INTEGER NOT NULL DEFAULT 0,
            attempt_generation INTEGER NOT NULL DEFAULT 0,
            UNIQUE(provider, session_id),
            FOREIGN KEY(provider, session_id)
                REFERENCES sessions(provider, session_id) ON DELETE CASCADE
         );",
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
    for column in ["raw_revision_generation", "stale_from_store_id"] {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM pragma_table_info('lcm_summary_convergence_queue')
                 WHERE name = ?1",
                params![column],
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            1,
            "missing in-place queue column {column}"
        );
    }
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE type = 'table' AND name = 'lcm_summary_convergence_dirty_raw'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1
    );
}

#[tokio::test]
async fn protected_content_revision_requeues_a_current_session() {
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
         VALUES ('cursor', 'revised-session', 'project.revised', '/revised');",
    )
    .await
    .unwrap();
    schema::ensure_lcm_schema(&conn).await.unwrap();
    conn.execute(
        "INSERT INTO lcm_raw_messages (
            provider, message_id, session_id, role, ordinal, content,
            content_hash, storage_kind, snippet_text, index_text, metadata_json
         ) VALUES ('cursor', 'message-1', 'revised-session', 'assistant', 1,
                   'old content', 'old-hash', 'inline', 'old content', 'old content',
                   '{\"ingest_protection\":{\"sanitization_receipt\":{}}}')",
        (),
    )
    .await
    .unwrap();
    let candidate = summary_convergence::next_candidate(&conn, i64::MAX)
        .await
        .unwrap()
        .unwrap();
    assert!(
        summary_convergence::record_outcome(
            &conn,
            &candidate,
            summary_convergence::LcmSummaryConvergenceQueueState::Current,
            None,
            0,
            0,
        )
        .await
        .unwrap()
    );
    assert!(
        summary_convergence::next_candidate(&conn, i64::MAX)
            .await
            .unwrap()
            .is_none()
    );

    conn.execute(
        "UPDATE lcm_raw_messages
         SET content = 'revised content', content_hash = 'revised-hash',
             snippet_text = 'revised content', index_text = 'revised content'
         WHERE provider = 'cursor' AND message_id = 'message-1'",
        (),
    )
    .await
    .unwrap();

    let revised = summary_convergence::next_candidate(&conn, i64::MAX)
        .await
        .unwrap()
        .expect("same-store content revisions must become due work");
    assert_eq!(revised.session_id, "revised-session");
    assert!(revised.attempted_raw_store_id < revised.newest_raw_store_id);
    assert!(
        !summary_convergence::record_outcome(
            &conn,
            &candidate,
            summary_convergence::LcmSummaryConvergenceQueueState::Current,
            None,
            0,
            0,
        )
        .await
        .unwrap(),
        "an outcome from the superseded raw generation must lose its CAS"
    );
}

#[tokio::test]
async fn disjoint_raw_revisions_drain_as_distinct_restart_safe_work_items() {
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
         VALUES ('cursor', 'disjoint-revisions', 'project.revised', '/revised');",
    )
    .await
    .unwrap();
    schema::ensure_lcm_schema(&conn).await.unwrap();
    for ordinal in 1..=2 {
        conn.execute(
            "INSERT INTO lcm_raw_messages (
                provider, message_id, session_id, role, ordinal, content,
                content_hash, storage_kind, snippet_text, index_text, metadata_json
             ) VALUES ('cursor', ?1, 'disjoint-revisions', 'assistant', ?2,
                       ?1, ?1, 'inline', ?1, ?1,
                       '{\"ingest_protection\":{\"sanitization_receipt\":{}}}')",
            params![format!("message-{ordinal}"), ordinal],
        )
        .await
        .unwrap();
    }
    let current = summary_convergence::next_candidate(&conn, i64::MAX)
        .await
        .unwrap()
        .unwrap();
    assert!(
        summary_convergence::record_outcome(
            &conn,
            &current,
            summary_convergence::LcmSummaryConvergenceQueueState::Current,
            None,
            0,
            0,
        )
        .await
        .unwrap()
    );
    conn.execute(
        "UPDATE lcm_raw_messages SET ordinal = 101
         WHERE provider = 'cursor' AND message_id = 'message-1'",
        (),
    )
    .await
    .unwrap();
    conn.execute(
        "UPDATE lcm_raw_messages SET content_hash = 'revised-2'
         WHERE provider = 'cursor' AND message_id = 'message-2'",
        (),
    )
    .await
    .unwrap();
    let first = summary_convergence::next_candidate(&conn, i64::MAX)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.stale_from_store_id, Some(1));
    assert!(
        summary_convergence::complete_stale_raw_revision(&conn, &first)
            .await
            .unwrap()
    );
    let after_restart =
        summary_convergence::candidate_for_session(&conn, "cursor", "disjoint-revisions")
            .await
            .unwrap()
            .unwrap();
    assert_eq!(after_restart.stale_from_store_id, Some(2));
    assert!(
        summary_convergence::complete_stale_raw_revision(&conn, &after_restart)
            .await
            .unwrap()
    );
    let drained = summary_convergence::candidate_for_session(&conn, "cursor", "disjoint-revisions")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(drained.stale_from_store_id, None);
    assert!(
        summary_convergence::record_outcome(
            &conn,
            &drained,
            summary_convergence::LcmSummaryConvergenceQueueState::Current,
            None,
            0,
            0,
        )
        .await
        .unwrap()
    );
    assert!(
        summary_convergence::next_candidate(&conn, i64::MAX)
            .await
            .unwrap()
            .is_none()
    );
}
