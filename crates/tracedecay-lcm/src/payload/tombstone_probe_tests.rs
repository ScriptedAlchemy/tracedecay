//! Behavioural evidence that the tombstone existence probe stays scoped to the
//! requested payload.
//!
//! `expand_payload` asks `tombstoned_raw_ref_exists` exactly one question: was
//! *this* payload reference tombstoned in raw message text? Answering it must
//! not transfer or retain the store's whole tombstone history, so these tests
//! assert on rows visited — every row `SQLite` hands back to the scan — rather
//! than on query text or plan wording.

use std::cell::Cell;
use std::path::Path;

use tempfile::TempDir;
use tracedecay_runtime_core::db::engine::{
    Connection, IntoParams, QueryExecutor, Result as EngineResult, Row, Rows, TestConnection, Value,
};

use crate::{LCM_SCAN_PAGE_ROWS, LcmError, schema};

use super::{expand_payload, tombstoned_raw_ref_exists};

const PROVIDER: &str = "cursor";
const SESSION: &str = "session-probe";
const TARGET_REF: &str =
    "payload_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.payload";
const DECOY_REF: &str =
    "payload_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.payload";

/// Rows are seeded past two full scan pages so a probe that pages the whole
/// candidate set is distinguishable from one that stops at the first answer.
const SEEDED_ROWS: usize = 1_200;
/// Prefilter-matching rows that the text authority must reject, placed at the
/// lowest `store_id`s so they are visited before any real tombstone.
const DECOY_ROWS: usize = 3;

/// Counts every row `SQLite` returns to the scan under test.
///
/// The wrapper drains each result set, records the row count, and replays the
/// identical rows to the caller, so the measurement is the transfer the scan
/// actually paid for.
struct RowVisitCounter<'a> {
    inner: &'a TestConnection,
    queries: Cell<usize>,
    rows_visited: Cell<usize>,
}

impl<'a> RowVisitCounter<'a> {
    fn new(inner: &'a TestConnection) -> Self {
        Self {
            inner,
            queries: Cell::new(0),
            rows_visited: Cell::new(0),
        }
    }

    fn queries(&self) -> usize {
        self.queries.get()
    }

    fn rows_visited(&self) -> usize {
        self.rows_visited.get()
    }
}

impl QueryExecutor for RowVisitCounter<'_> {
    async fn query<P>(&self, sql: &str, params: P) -> EngineResult<Rows>
    where
        P: IntoParams,
    {
        let mut rows = self.inner.query(sql, params).await?;
        self.queries.set(self.queries.get() + 1);
        let columns = (0..rows.column_count())
            .map(|index| rows.column_name(index).unwrap_or_default().to_string())
            .collect::<Vec<_>>();
        let mut replay = Vec::new();
        while let Some(row) = rows.next().await? {
            let mut values = Vec::new();
            let mut column = 0_i32;
            while let Ok(value) = row.get::<Value>(column) {
                values.push(value);
                column += 1;
            }
            replay.push(Row::from_values(values));
        }
        self.rows_visited
            .set(self.rows_visited.get() + replay.len());
        Ok(Rows::from_parts(columns, replay))
    }
}

struct ProbeStore {
    _temp: TempDir,
    conn: TestConnection,
}

async fn probe_store() -> ProbeStore {
    let temp = TempDir::new().expect("create probe tempdir");
    let conn = TestConnection::open(&temp.path().join("sessions.db"));
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
            provider TEXT NOT NULL,
            session_id TEXT NOT NULL,
            project_key TEXT NOT NULL,
            project_path TEXT NOT NULL,
            title TEXT,
            started_at INTEGER,
            PRIMARY KEY(provider, session_id)
        );
        CREATE TABLE IF NOT EXISTS session_messages (
            provider TEXT NOT NULL,
            message_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            timestamp INTEGER,
            ordinal INTEGER NOT NULL,
            text TEXT NOT NULL,
            metadata_json TEXT,
            PRIMARY KEY(provider, message_id),
            FOREIGN KEY(provider, session_id)
                REFERENCES sessions(provider, session_id) ON DELETE CASCADE
        );",
    )
    .await
    .expect("create probe session tables");
    schema::ensure_lcm_schema(&conn)
        .await
        .expect("ensure lcm schema");
    conn.execute_batch(&format!(
        "INSERT INTO sessions (provider, session_id, project_key, project_path, title, started_at)
         VALUES ('{PROVIDER}', '{SESSION}', 'probe', 'probe', 'probe', 1);"
    ))
    .await
    .expect("insert probe session");
    ProbeStore { _temp: temp, conn }
}

fn sql_text(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Seeds one raw message per supplied text, in order, so `store_id` follows the
/// slice order.
async fn seed_raw_messages(conn: &Connection, texts: &[String]) {
    for (chunk_index, chunk) in texts.chunks(100).enumerate() {
        let mut batch = String::new();
        for (offset, text) in chunk.iter().enumerate() {
            let message_id = format!("message-{}", chunk_index * 100 + offset);
            let literal = sql_text(text);
            batch.push_str(&format!(
                "INSERT INTO lcm_raw_messages (
                    provider, message_id, session_id, role, ordinal, timestamp,
                    content, content_hash, storage_kind, payload_ref, snippet_text,
                    index_text, legacy_source, legacy_truncated, metadata_json
                 ) VALUES ('{PROVIDER}', '{message_id}', '{SESSION}', 'assistant', 1, 2,
                    {literal}, '{message_id}-hash', 'inline', NULL, {literal},
                    {literal}, 0, 0, NULL);\n"
            ));
        }
        conn.execute_batch(&batch)
            .await
            .expect("seed raw message batch");
    }
}

fn tombstone_text(payload_ref: &str) -> String {
    format!("[gc'd externalized payload: bytes=10 ref={payload_ref}; body]")
}

/// Matches the payload-scoped `LIKE` prefilter (a GC prefix followed later by
/// the requested reference) while the text authority rejects it: the GC
/// placeholder names a different payload and the requested reference appears in
/// a live placeholder.
fn decoy_text(target_ref: &str) -> String {
    format!(
        "{} [externalized payload: bytes=10 ref={target_ref}; live]",
        tombstone_text(DECOY_REF)
    )
}

fn unrelated_ref(index: usize) -> String {
    format!("payload_{index:064}.payload")
}

#[tokio::test]
async fn tombstone_probe_stops_at_the_first_confirmed_row() {
    let store = probe_store().await;
    let mut texts = Vec::with_capacity(SEEDED_ROWS);
    for _ in 0..DECOY_ROWS {
        texts.push(decoy_text(TARGET_REF));
    }
    for _ in DECOY_ROWS..SEEDED_ROWS {
        texts.push(tombstone_text(TARGET_REF));
    }
    seed_raw_messages(&store.conn, &texts).await;

    let counter = RowVisitCounter::new(&store.conn);
    let tombstoned = tombstoned_raw_ref_exists(&counter, TARGET_REF)
        .await
        .expect("probe tombstoned reference");

    assert!(tombstoned, "the requested payload is tombstoned");
    assert!(
        counter.rows_visited() <= LCM_SCAN_PAGE_ROWS as usize,
        "one payload lookup against {SEEDED_ROWS} tombstoned rows visited {} rows across {} queries",
        counter.rows_visited(),
        counter.queries()
    );
    assert_eq!(
        counter.queries(),
        1,
        "an answered probe must not page the rest of the tombstone history (visited {} rows)",
        counter.rows_visited()
    );
}

#[tokio::test]
async fn tombstone_probe_ignores_unrelated_payload_tombstones() {
    let store = probe_store().await;
    let texts = (0..SEEDED_ROWS)
        .map(|index| tombstone_text(&unrelated_ref(index)))
        .collect::<Vec<_>>();
    seed_raw_messages(&store.conn, &texts).await;

    let counter = RowVisitCounter::new(&store.conn);
    let tombstoned = tombstoned_raw_ref_exists(&counter, TARGET_REF)
        .await
        .expect("probe unrelated tombstones");

    assert!(!tombstoned, "the requested payload was never tombstoned");
    assert_eq!(
        counter.rows_visited(),
        0,
        "a lookup for one payload must not visit unrelated tombstones"
    );
    assert_eq!(counter.queries(), 1);
}

#[tokio::test]
async fn tombstone_probe_keeps_the_text_authority_over_the_like_prefilter() {
    let store = probe_store().await;
    let texts = (0..DECOY_ROWS)
        .map(|_| decoy_text(TARGET_REF))
        .collect::<Vec<_>>();
    seed_raw_messages(&store.conn, &texts).await;

    let counter = RowVisitCounter::new(&store.conn);
    let tombstoned = tombstoned_raw_ref_exists(&counter, TARGET_REF)
        .await
        .expect("probe decoy rows");

    assert!(
        !tombstoned,
        "a live placeholder trailing an unrelated tombstone is not a tombstone"
    );
    assert_eq!(
        counter.rows_visited(),
        DECOY_ROWS,
        "the LIKE pattern is only a prefilter, so every candidate reaches the text authority"
    );
}

#[tokio::test]
async fn expand_payload_still_reports_a_garbage_collected_payload() {
    let store = probe_store().await;
    seed_raw_messages(&store.conn, &[tombstone_text(TARGET_REF)]).await;

    let error = expand_payload(
        &store.conn,
        Path::new("/nonexistent-storage-root"),
        PROVIDER,
        SESSION,
        TARGET_REF,
        0,
        16,
    )
    .await
    .expect_err("a tombstoned payload cannot expand");

    assert_eq!(error, LcmError::PayloadGcd);
}
