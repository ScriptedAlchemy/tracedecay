use std::cell::Cell;

use tracedecay_runtime_core::db::engine::{
    Executor, IntoParams, QueryExecutor, Result as EngineResult, Rows, TestConnection, params,
};

use crate::{LCM_SCAN_PAGE_ROWS, schema, summary_convergence};

/// Executor adapter that counts the statements a code path issues and the rows
/// those statements change, so write amplification is measured rather than
/// inferred.
struct CountingExecutor<'a> {
    inner: &'a TestConnection,
    queries: Cell<usize>,
    executes: Cell<usize>,
    rows_changed: Cell<u64>,
}

impl<'a> CountingExecutor<'a> {
    fn new(inner: &'a TestConnection) -> Self {
        Self {
            inner,
            queries: Cell::new(0),
            executes: Cell::new(0),
            rows_changed: Cell::new(0),
        }
    }
}

impl QueryExecutor for CountingExecutor<'_> {
    async fn query<P>(&self, sql: &str, params: P) -> EngineResult<Rows>
    where
        P: IntoParams,
    {
        self.queries.set(self.queries.get() + 1);
        self.inner.query(sql, params).await
    }
}

impl Executor for CountingExecutor<'_> {
    async fn execute<P>(&self, sql: &str, params: P) -> EngineResult<u64>
    where
        P: IntoParams,
    {
        self.executes.set(self.executes.get() + 1);
        let changed = self.inner.execute(sql, params).await?;
        self.rows_changed.set(self.rows_changed.get() + changed);
        Ok(changed)
    }

    async fn execute_batch(&self, sql: &str) -> EngineResult<()> {
        self.executes.set(self.executes.get() + 1);
        self.inner.execute_batch(sql).await
    }
}

async fn fetch_i64(conn: &TestConnection, sql: &str, params: impl IntoParams) -> i64 {
    let mut rows = conn.query(sql, params).await.unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

#[tokio::test]
async fn backfill_page_upserts_each_session_once_and_idles_without_work() {
    const ROWS: i64 = 300;
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
         VALUES ('cursor', 'session-a', 'project', '/p'),
                ('cursor', 'session-b', 'project', '/p'),
                ('cursor', 'session-c', 'project', '/p');",
    )
    .await
    .unwrap();
    schema::ensure_lcm_schema(&conn).await.unwrap();
    // Interleave three sessions so first-seen order (b, a, c) differs from
    // both lexical order and the order of the sessions table.
    let session_for = |ordinal: i64| match ordinal % 3 {
        1 => "session-b",
        2 => "session-a",
        _ => "session-c",
    };
    for ordinal in 1..=ROWS {
        conn.execute(
            "INSERT INTO lcm_raw_messages (
                provider, message_id, session_id, role, ordinal, content,
                content_hash, storage_kind, snippet_text, index_text, metadata_json
             ) VALUES ('cursor', ?1, ?2, 'assistant', ?3, 'body',
                       ?1, 'inline', 'body', 'body', '{}')",
            params![format!("message-{ordinal}"), session_for(ordinal), ordinal],
        )
        .await
        .unwrap();
    }
    // The insert trigger already queued every session. Model a store whose
    // rows predate the queue: drop two queue rows so the backfill must create
    // them, and give the third retry evidence the backfill must not erase.
    conn.execute_batch(
        "DELETE FROM lcm_summary_convergence_queue
         WHERE session_id IN ('session-a', 'session-b');
         UPDATE lcm_summary_convergence_queue
         SET state = 'retryable', failure_code = 'storage_unavailable',
             failure_count = 2, next_attempt_at_ms = 999
         WHERE session_id = 'session-c';",
    )
    .await
    .unwrap();

    assert!(
        summary_convergence::backfill_queue_has_work(&*conn)
            .await
            .unwrap()
    );

    let counting = CountingExecutor::new(&conn);
    let page = summary_convergence::backfill_queue_page(&counting, LCM_SCAN_PAGE_ROWS as usize)
        .await
        .unwrap();
    assert_eq!(page.rows_scanned, ROWS as usize);
    assert!(!page.has_more);
    assert!(
        counting.executes.get() < page.rows_scanned,
        "a {}-row page issued {} write statements (one per row again?)",
        page.rows_scanned,
        counting.executes.get()
    );

    // Queue rows: one per session, newest store id per session, and queue_id
    // (fair-scheduling order) in first-seen order for the rows the page
    // created, after the pre-existing row.
    let mut rows = conn
        .query(
            "SELECT session_id, newest_raw_store_id, state, failure_code, failure_count,
                    next_attempt_at_ms
             FROM lcm_summary_convergence_queue
             ORDER BY queue_id",
            (),
        )
        .await
        .unwrap();
    let mut queue = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        queue.push((
            row.get::<String>(0).unwrap(),
            row.get::<i64>(1).unwrap(),
            row.get::<String>(2).unwrap(),
            row.get::<Option<String>>(3).unwrap(),
            row.get::<i64>(4).unwrap(),
            row.get::<i64>(5).unwrap(),
        ));
    }
    drop(rows);
    assert_eq!(
        queue,
        vec![
            (
                "session-c".to_string(),
                300,
                "retryable".to_string(),
                Some("storage_unavailable".to_string()),
                2,
                999,
            ),
            (
                "session-b".to_string(),
                298,
                "pending".to_string(),
                None,
                0,
                0
            ),
            (
                "session-a".to_string(),
                299,
                "pending".to_string(),
                None,
                0,
                0
            ),
        ]
    );

    // Predecessor ranges: every message except each session's first keeps
    // its own exact predecessor interval.
    assert_eq!(
        fetch_i64(&conn, "SELECT COUNT(*) FROM lcm_raw_predecessor_ranges", ()).await,
        ROWS - 3
    );
    let mut range = conn
        .query(
            "SELECT session_id, from_store_id, to_store_id
             FROM lcm_raw_predecessor_ranges
             WHERE provider = 'cursor' AND message_id = 'message-7'",
            (),
        )
        .await
        .unwrap();
    let row = range.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "session-b");
    assert_eq!(row.get::<i64>(1).unwrap(), 1);
    assert_eq!(row.get::<i64>(2).unwrap(), 4);
    drop(range);
    assert_eq!(
        fetch_i64(
            &conn,
            "SELECT COUNT(*) FROM lcm_raw_predecessor_ranges WHERE message_id = 'message-1'",
            (),
        )
        .await,
        0,
        "a session's first message has no predecessor range"
    );

    // Everything is behind the frontier now: the read probe reports idle and
    // a page under the writer changes nothing.
    assert!(
        !summary_convergence::backfill_queue_has_work(&*conn)
            .await
            .unwrap()
    );
    let idle = CountingExecutor::new(&conn);
    let empty = summary_convergence::backfill_queue_page(&idle, LCM_SCAN_PAGE_ROWS as usize)
        .await
        .unwrap();
    assert_eq!(
        empty,
        summary_convergence::LcmSummaryQueueBackfillPage::default()
    );
    assert_eq!(idle.executes.get(), 0);
    assert_eq!(idle.rows_changed.get(), 0);

    // A new raw row past the frontier makes the probe report work again.
    conn.execute(
        "INSERT INTO lcm_raw_messages (
            provider, message_id, session_id, role, ordinal, content,
            content_hash, storage_kind, snippet_text, index_text, metadata_json
         ) VALUES ('cursor', 'message-301', 'session-a', 'assistant', 301, 'body',
                   'message-301', 'inline', 'body', 'body', '{}')",
        (),
    )
    .await
    .unwrap();
    assert!(
        summary_convergence::backfill_queue_has_work(&*conn)
            .await
            .unwrap()
    );
}

/// Raw fixture whose persisted ranges predate the policy-anchor role filter.
///
/// `first-user` owns a range it must lose (its only earlier row is an anchor)
/// and `compact-summary` owns an interval widened past the compact boundary.
async fn seed_preserved_role_filter_store(conn: &TestConnection) {
    create_session_host_tables(conn).await;
    conn.execute_batch(
        "INSERT INTO sessions(provider, session_id, project_key, project_path)
         VALUES ('claude', 'preserved', 'project.preserved', '/preserved');",
    )
    .await
    .unwrap();
    schema::ensure_lcm_schema(conn).await.unwrap();
    for (store_id, message_id, role) in [
        (1_i64, "session-open-system", "system"),
        (2, "first-user", "user"),
        (3, "compact_boundary:marker", "system"),
        (4, "compact-summary", "user"),
        (5, "reply", "assistant"),
        (6, "follow-up", "user"),
    ] {
        conn.execute(
            "INSERT INTO lcm_raw_messages (
                 store_id, provider, message_id, session_id, role, ordinal,
                 content, content_hash, storage_kind, snippet_text, index_text,
                 metadata_json
             ) VALUES (?1, 'claude', ?2, 'preserved', ?3, ?1, 'body', ?2,
                       'inline', 'body', 'body', '{}')",
            params![store_id, message_id, role],
        )
        .await
        .unwrap();
    }
    seed_pre_role_filter_ranges(conn).await;
}

/// The session tables the LCM schema's raw-identity triggers read. Store open
/// installs LCM objects beside them, so a store without them is not a shape
/// any profile presents.
async fn create_session_host_tables(conn: &TestConnection) {
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
}

/// Ranges an ingest before the role filter would have written, and a journal
/// that has never recorded the rewrite: this is the store shape a preserved
/// profile presents on its first open under the role filter.
async fn seed_pre_role_filter_ranges(conn: &TestConnection) {
    conn.execute_batch(
        "INSERT INTO lcm_raw_predecessor_ranges (
             provider, message_id, session_id, from_store_id, to_store_id
         ) VALUES ('claude', 'first-user', 'preserved', 1, 1),
                  ('claude', 'compact-summary', 'preserved', 1, 3),
                  ('claude', 'follow-up', 'preserved', 1, 5);
         DELETE FROM lcm_gc_meta
         WHERE key = 'predecessor_range_role_filter_v1';",
    )
    .await
    .unwrap();
}

async fn predecessor_range(conn: &TestConnection, message_id: &str) -> Option<(i64, i64)> {
    let mut rows = conn
        .query(
            "SELECT from_store_id, to_store_id
             FROM lcm_raw_predecessor_ranges
             WHERE provider = 'claude' AND message_id = ?1",
            params![message_id],
        )
        .await
        .unwrap();
    rows.next()
        .await
        .unwrap()
        .map(|row| (row.get::<i64>(0).unwrap(), row.get::<i64>(1).unwrap()))
}

async fn journaled_rewrite_cursor(conn: &TestConnection) -> Option<String> {
    schema::get_gc_meta(
        &**conn,
        summary_convergence::PREDECESSOR_RANGE_ROLE_FILTER_KEY,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn role_filter_range_rewrite_pages_in_background_without_blocking_admission() {
    const PAGE_ROWS: usize = 2;
    let temp = tempfile::tempdir().unwrap();
    let conn = TestConnection::open(&temp.path().join("sessions.db"));
    seed_preserved_role_filter_store(&conn).await;

    // Admission: reopening an existing store must not perform the rewrite.
    schema::ensure_lcm_schema(&conn).await.unwrap();
    assert_eq!(
        predecessor_range(&conn, "compact-summary").await,
        Some((1, 3)),
        "store open must leave historical convergence to the background pass"
    );
    assert_eq!(
        journaled_rewrite_cursor(&conn).await,
        None,
        "store open must not journal rewrite progress"
    );
    assert!(
        summary_convergence::predecessor_range_rewrite_has_work(&*conn)
            .await
            .unwrap()
    );
    // Retrieval answers from the unrewritten store before the pass runs.
    assert_eq!(
        fetch_i64(
            &conn,
            "SELECT COUNT(*) FROM lcm_raw_messages WHERE session_id = 'preserved'",
            (),
        )
        .await,
        6
    );

    // First page covers store ids 1..=2 only: the range `first-user` must
    // lose is already gone while later stale rows are untouched.
    let first = summary_convergence::predecessor_range_rewrite_page(&*conn, PAGE_ROWS)
        .await
        .unwrap();
    assert_eq!(
        first,
        summary_convergence::LcmPredecessorRangeRewritePage {
            rows_rewritten: PAGE_ROWS,
            has_more: true,
        }
    );
    assert_eq!(
        predecessor_range(&conn, "first-user").await,
        None,
        "a row whose only predecessors are policy anchors must lose its range"
    );
    assert_eq!(
        predecessor_range(&conn, "compact-summary").await,
        Some((1, 3)),
        "a page must not rewrite rows above its keyset cursor"
    );
    assert_eq!(
        journaled_rewrite_cursor(&conn).await.as_deref(),
        Some("2"),
        "each page must journal its own keyset cursor"
    );

    // Restart mid-rewrite: a fresh pass resumes from the journaled cursor.
    drop(conn);
    let conn = TestConnection::open(&temp.path().join("sessions.db"));
    let resumed = summary_convergence::predecessor_range_rewrite_page(&*conn, PAGE_ROWS)
        .await
        .unwrap();
    assert_eq!(resumed.rows_rewritten, PAGE_ROWS);
    assert_eq!(
        predecessor_range(&conn, "compact-summary").await,
        Some((2, 2)),
        "the rewrite must narrow the interval to the conversational backlog"
    );

    let last = summary_convergence::predecessor_range_rewrite_page(&*conn, PAGE_ROWS)
        .await
        .unwrap();
    assert_eq!(last.rows_rewritten, PAGE_ROWS);
    assert_eq!(
        predecessor_range(&conn, "follow-up").await,
        Some((2, 5)),
        "the last page rewrites its own rows from the same authority"
    );
    assert!(
        summary_convergence::predecessor_range_rewrite_has_work(&*conn)
            .await
            .unwrap(),
        "the pass still owes its completion marker"
    );

    // The drained page retires the pass exactly once.
    let drained = summary_convergence::predecessor_range_rewrite_page(&*conn, PAGE_ROWS)
        .await
        .unwrap();
    assert_eq!(
        drained,
        summary_convergence::LcmPredecessorRangeRewritePage::default()
    );
    assert_eq!(
        journaled_rewrite_cursor(&conn).await.as_deref(),
        Some("applied")
    );
    assert!(
        !summary_convergence::predecessor_range_rewrite_has_work(&*conn)
            .await
            .unwrap()
    );
    conn.execute(
        "UPDATE lcm_raw_predecessor_ranges SET to_store_id = 3
         WHERE provider = 'claude' AND message_id = 'compact-summary'",
        (),
    )
    .await
    .unwrap();
    let after_marker = summary_convergence::predecessor_range_rewrite_page(&*conn, PAGE_ROWS)
        .await
        .unwrap();
    assert_eq!(
        after_marker,
        summary_convergence::LcmPredecessorRangeRewritePage::default()
    );
    assert_eq!(
        predecessor_range(&conn, "compact-summary").await,
        Some((2, 3)),
        "a completed rewrite must not run a second time"
    );
}

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
    let mut range_rows = conn
        .query(
            "SELECT COUNT(*) FROM lcm_raw_predecessor_ranges
             WHERE provider = 'cursor' AND session_id = 'large-corpus'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        range_rows
            .next()
            .await
            .unwrap()
            .unwrap()
            .get::<i64>(0)
            .unwrap(),
        LCM_SCAN_PAGE_ROWS - 1,
        "the bounded queue backfill must durably derive native predecessor ranges"
    );

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
         DROP TRIGGER lcm_summary_convergence_dirty_raw_seed;
         DROP TABLE lcm_summary_convergence_invalidation_work;
         DROP TABLE lcm_summary_convergence_dirty_raw;
         DROP TABLE lcm_summary_convergence_queue;
         CREATE TABLE lcm_summary_convergence_dirty_raw (
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            store_id INTEGER NOT NULL,
            PRIMARY KEY(provider, session_id, store_id),
            FOREIGN KEY(provider, session_id)
                REFERENCES sessions(provider, session_id) ON DELETE CASCADE
         );
         CREATE TABLE lcm_summary_convergence_invalidation_work (
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            raw_store_id INTEGER NOT NULL,
            source_kind TEXT NOT NULL,
            source_id TEXT NOT NULL,
            depth INTEGER NOT NULL,
            after_node_id TEXT NOT NULL DEFAULT '',
            PRIMARY KEY(provider, session_id, raw_store_id, source_kind, source_id),
            FOREIGN KEY(provider, session_id, raw_store_id)
                REFERENCES lcm_summary_convergence_dirty_raw(provider, session_id, store_id)
                ON DELETE CASCADE
         );
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
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM pragma_table_info('lcm_summary_convergence_dirty_raw')
             WHERE name = 'rewind_frontier_store_id'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1,
        "missing durable invalidation rewind frontier"
    );
    for object in [
        "lcm_summary_convergence_invalidation_work",
        "lcm_summary_convergence_dirty_raw_seed",
        "idx_lcm_summary_sources_source_node",
    ] {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name = ?1",
                params![object],
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            1,
            "missing in-place invalidation authority {object}"
        );
    }
    let mut rows = conn
        .query(
            "SELECT COUNT(*)
             FROM pragma_table_info('lcm_summary_convergence_invalidation_work')
             WHERE name = 'state'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1,
        "missing durable invalidation visited state"
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
async fn protection_progress_cannot_overwrite_a_concurrent_raw_rewind() {
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
         VALUES ('cursor', 'protection-cas', 'project.cas', '/cas');",
    )
    .await
    .unwrap();
    schema::ensure_lcm_schema(&conn).await.unwrap();
    conn.execute(
        "INSERT INTO lcm_raw_messages (
            provider, message_id, session_id, role, ordinal, content,
            content_hash, storage_kind, snippet_text, index_text, metadata_json
         ) VALUES ('cursor', 'message-1', 'protection-cas', 'assistant', 1,
                   'old', 'old-hash', 'inline', 'old', 'old',
                   '{\"ingest_protection\":{\"sanitization_receipt\":{}}}')",
        (),
    )
    .await
    .unwrap();
    let candidate = summary_convergence::next_candidate(&conn, i64::MAX)
        .await
        .unwrap()
        .unwrap();

    conn.execute(
        "UPDATE lcm_raw_messages SET content_hash = 'new-hash'
         WHERE provider = 'cursor' AND message_id = 'message-1'",
        (),
    )
    .await
    .unwrap();
    let error = summary_convergence::record_current_protection_progress(
        &conn,
        "cursor",
        "protection-cas",
        999,
        candidate.raw_revision_generation,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, crate::LcmError::StaleRawRevision { .. }));
    let refreshed = summary_convergence::candidate_for_session(&conn, "cursor", "protection-cas")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refreshed.protection_frontier_store_id, 0);
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

/// A store created after the role-aware filter owes no rewrite: ingest writes
/// every interval under the current filter, so paging the corpus to re-derive
/// them would be pure waste.
#[tokio::test]
async fn a_fresh_store_opens_with_the_range_rewrite_already_retired() {
    let temp = tempfile::tempdir().unwrap();
    let conn = TestConnection::open(&temp.path().join("sessions.db"));
    create_session_host_tables(&conn).await;

    schema::ensure_lcm_schema(&conn).await.unwrap();

    assert_eq!(
        journaled_rewrite_cursor(&conn).await.as_deref(),
        Some("applied"),
        "a fresh install must journal the rewrite as already retired"
    );
    assert!(
        !summary_convergence::predecessor_range_rewrite_has_work(&*conn)
            .await
            .unwrap(),
        "a fresh store must not hand the background worker a corpus-wide pass"
    );

    // Reopening preserves the retirement rather than re-arming the pass.
    schema::ensure_lcm_schema(&conn).await.unwrap();
    assert!(
        !summary_convergence::predecessor_range_rewrite_has_work(&*conn)
            .await
            .unwrap()
    );
}
