//! Durable bounded work queue for retained-session summary convergence.

use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use crate::{LcmCompressionResponse, LcmError, schema};

const BACKFILL_FRONTIER_KEY: &str = "summary_convergence_queue_backfill_store_id_v1";

pub const SUMMARY_CONVERGENCE_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS lcm_summary_convergence_queue (
    queue_id INTEGER PRIMARY KEY AUTOINCREMENT,
    provider TEXT NOT NULL,
    session_id TEXT NOT NULL,
    newest_raw_store_id INTEGER NOT NULL,
    protection_frontier_store_id INTEGER NOT NULL DEFAULT 0,
    attempted_raw_store_id INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK(state IN ('pending', 'retryable', 'current', 'unavailable', 'permanent')),
    failure_code TEXT,
    failure_count INTEGER NOT NULL DEFAULT 0,
    next_attempt_at_ms INTEGER NOT NULL DEFAULT 0,
    attempt_generation INTEGER NOT NULL DEFAULT 0,
    UNIQUE(provider, session_id),
    FOREIGN KEY(provider, session_id)
        REFERENCES sessions(provider, session_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_lcm_summary_convergence_due
    ON lcm_summary_convergence_queue(
        next_attempt_at_ms, attempt_generation, queue_id
    )
    WHERE state IN ('pending', 'retryable');
CREATE TRIGGER IF NOT EXISTS lcm_summary_convergence_raw_insert
    AFTER INSERT ON lcm_raw_messages BEGIN
        INSERT INTO lcm_summary_convergence_queue (
            provider, session_id, newest_raw_store_id
        ) VALUES (NEW.provider, NEW.session_id, NEW.store_id)
        ON CONFLICT(provider, session_id) DO UPDATE SET
            newest_raw_store_id = MAX(
                lcm_summary_convergence_queue.newest_raw_store_id,
                excluded.newest_raw_store_id
            ),
            state = CASE
                WHEN excluded.newest_raw_store_id
                     > lcm_summary_convergence_queue.attempted_raw_store_id
                THEN 'pending'
                ELSE lcm_summary_convergence_queue.state
            END,
            failure_code = CASE
                WHEN excluded.newest_raw_store_id
                     > lcm_summary_convergence_queue.attempted_raw_store_id
                THEN NULL
                ELSE lcm_summary_convergence_queue.failure_code
            END,
            failure_count = CASE
                WHEN excluded.newest_raw_store_id
                     > lcm_summary_convergence_queue.attempted_raw_store_id
                THEN 0
                ELSE lcm_summary_convergence_queue.failure_count
            END,
            next_attempt_at_ms = CASE
                WHEN excluded.newest_raw_store_id
                     > lcm_summary_convergence_queue.attempted_raw_store_id
                THEN 0
                ELSE lcm_summary_convergence_queue.next_attempt_at_ms
            END;
    END;
CREATE TRIGGER IF NOT EXISTS lcm_summary_convergence_raw_unprotected_update
    AFTER UPDATE OF provider, session_id, metadata_json ON lcm_raw_messages
    WHEN CASE
        WHEN json_valid(NEW.metadata_json)
        THEN json_extract(
            NEW.metadata_json,
            '$.ingest_protection.sanitization_receipt'
        ) IS NULL
        ELSE 1
    END BEGIN
        INSERT INTO lcm_summary_convergence_queue (
            provider, session_id, newest_raw_store_id,
            protection_frontier_store_id
        ) VALUES (
            NEW.provider, NEW.session_id, NEW.store_id,
            MAX(0, NEW.store_id - 1)
        )
        ON CONFLICT(provider, session_id) DO UPDATE SET
            newest_raw_store_id = MAX(
                lcm_summary_convergence_queue.newest_raw_store_id,
                excluded.newest_raw_store_id
            ),
            protection_frontier_store_id = MIN(
                lcm_summary_convergence_queue.protection_frontier_store_id,
                excluded.protection_frontier_store_id
            ),
            state = 'pending',
            failure_code = NULL,
            failure_count = 0,
            next_attempt_at_ms = 0;
    END;
"#;

pub const NEXT_CANDIDATE_SQL: &str = "SELECT provider, session_id, newest_raw_store_id,
            protection_frontier_store_id, attempted_raw_store_id,
            failure_count
     FROM lcm_summary_convergence_queue
     WHERE state IN ('pending', 'retryable')
       AND next_attempt_at_ms <= ?1
     ORDER BY next_attempt_at_ms, attempt_generation, queue_id
     LIMIT 1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LcmSummaryConvergenceCandidate {
    pub provider: String,
    pub session_id: String,
    pub newest_raw_store_id: i64,
    pub protection_frontier_store_id: i64,
    pub attempted_raw_store_id: i64,
    pub failure_count: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LcmSummaryQueueBackfillPage {
    pub rows_scanned: usize,
    pub has_more: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LcmRawProtectionPage {
    pub rows_scanned: usize,
    pub rows_protected: usize,
    pub bytes_scanned: u64,
    pub frontier_store_id: i64,
    pub has_more: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LcmBoundedCompressionResponse {
    pub response: LcmCompressionResponse,
    pub rows_scanned: usize,
    pub bytes_scanned: u64,
    pub has_more: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LcmSummaryConvergenceQueueState {
    Pending,
    Retryable,
    Current,
    Unavailable,
    Permanent,
}

impl LcmSummaryConvergenceQueueState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Retryable => "retryable",
            Self::Current => "current",
            Self::Unavailable => "unavailable",
            Self::Permanent => "permanent",
        }
    }
}

pub async fn ensure_schema(conn: &(impl Executor + ?Sized)) -> Result<(), LcmError> {
    conn.execute_batch(SUMMARY_CONVERGENCE_SCHEMA_SQL).await?;
    Ok(())
}

pub async fn backfill_queue_page(
    conn: &(impl Executor + ?Sized),
    page_limit: usize,
) -> Result<LcmSummaryQueueBackfillPage, LcmError> {
    let page_limit_usize = page_limit.max(1);
    let page_limit = i64::try_from(page_limit_usize)
        .map_err(|_| LcmError::Db("LCM summary queue page limit overflow".to_string()))?;
    let frontier = schema::get_gc_meta(conn, BACKFILL_FRONTIER_KEY)
        .await?
        .map(|value| {
            value.parse::<i64>().map_err(|error| {
                LcmError::Db(format!(
                    "decode LCM summary queue backfill frontier: {error}"
                ))
            })
        })
        .transpose()?
        .unwrap_or(0);
    let mut rows = conn
        .query(
            "SELECT provider, session_id, store_id
             FROM lcm_raw_messages
             WHERE store_id > ?1
             ORDER BY store_id
             LIMIT ?2",
            params![frontier, page_limit],
        )
        .await?;
    let mut queued = Vec::new();
    while let Some(row) = rows.next().await? {
        queued.push((
            row.get::<String>(0)?,
            row.get::<String>(1)?,
            row.get::<i64>(2)?,
        ));
    }
    drop(rows);
    for (provider, session_id, store_id) in &queued {
        queue_raw_session(conn, provider, session_id, *store_id).await?;
    }
    if let Some((_, _, store_id)) = queued.last() {
        schema::set_gc_meta(conn, BACKFILL_FRONTIER_KEY, &store_id.to_string()).await?;
    }
    Ok(LcmSummaryQueueBackfillPage {
        rows_scanned: queued.len(),
        has_more: queued.len() == page_limit_usize,
    })
}

async fn queue_raw_session(
    conn: &(impl Executor + ?Sized),
    provider: &str,
    session_id: &str,
    store_id: i64,
) -> Result<(), LcmError> {
    conn.execute(
        "INSERT INTO lcm_summary_convergence_queue (
            provider, session_id, newest_raw_store_id
         ) VALUES (?1, ?2, ?3)
         ON CONFLICT(provider, session_id) DO UPDATE SET
            newest_raw_store_id = MAX(
                lcm_summary_convergence_queue.newest_raw_store_id,
                excluded.newest_raw_store_id
            ),
            state = CASE
                WHEN excluded.newest_raw_store_id
                     > lcm_summary_convergence_queue.newest_raw_store_id
                THEN 'pending'
                ELSE lcm_summary_convergence_queue.state
            END,
            failure_code = CASE
                WHEN excluded.newest_raw_store_id
                     > lcm_summary_convergence_queue.newest_raw_store_id
                THEN NULL
                ELSE lcm_summary_convergence_queue.failure_code
            END,
            failure_count = CASE
                WHEN excluded.newest_raw_store_id
                     > lcm_summary_convergence_queue.newest_raw_store_id
                THEN 0
                ELSE lcm_summary_convergence_queue.failure_count
            END,
            next_attempt_at_ms = CASE
                WHEN excluded.newest_raw_store_id
                     > lcm_summary_convergence_queue.newest_raw_store_id
                THEN 0
                ELSE lcm_summary_convergence_queue.next_attempt_at_ms
            END",
        params![provider, session_id, store_id],
    )
    .await?;
    Ok(())
}

pub async fn next_candidate(
    conn: &(impl QueryExecutor + ?Sized),
    now_unix_ms: i64,
) -> Result<Option<LcmSummaryConvergenceCandidate>, LcmError> {
    let mut rows = conn.query(NEXT_CANDIDATE_SQL, params![now_unix_ms]).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let failure_count = u32::try_from(row.get::<i64>(5)?).map_err(|error| {
        LcmError::Db(format!(
            "invalid LCM summary convergence failure count: {error}"
        ))
    })?;
    Ok(Some(LcmSummaryConvergenceCandidate {
        provider: row.get(0)?,
        session_id: row.get(1)?,
        newest_raw_store_id: row.get(2)?,
        protection_frontier_store_id: row.get(3)?,
        attempted_raw_store_id: row.get(4)?,
        failure_count,
    }))
}

pub async fn record_protection_progress(
    conn: &(impl Executor + ?Sized),
    candidate: &LcmSummaryConvergenceCandidate,
    protection_frontier_store_id: i64,
) -> Result<(), LcmError> {
    conn.execute(
        "UPDATE lcm_summary_convergence_queue
         SET protection_frontier_store_id = MAX(
                 protection_frontier_store_id, ?3
             ),
             attempt_generation = attempt_generation + 1
         WHERE provider = ?1 AND session_id = ?2",
        params![
            candidate.provider.as_str(),
            candidate.session_id.as_str(),
            protection_frontier_store_id,
        ],
    )
    .await?;
    Ok(())
}

pub async fn record_outcome(
    conn: &(impl Executor + ?Sized),
    candidate: &LcmSummaryConvergenceCandidate,
    state: LcmSummaryConvergenceQueueState,
    failure_code: Option<&str>,
    failure_count: u32,
    next_attempt_at_ms: i64,
) -> Result<(), LcmError> {
    conn.execute(
        "UPDATE lcm_summary_convergence_queue
         SET attempted_raw_store_id = ?3,
             state = CASE WHEN newest_raw_store_id > ?3 THEN 'pending' ELSE ?4 END,
             failure_code = CASE WHEN newest_raw_store_id > ?3 THEN NULL ELSE ?5 END,
             failure_count = CASE WHEN newest_raw_store_id > ?3 THEN 0 ELSE ?6 END,
             next_attempt_at_ms = CASE WHEN newest_raw_store_id > ?3 THEN 0 ELSE ?7 END,
             attempt_generation = attempt_generation + 1
         WHERE provider = ?1 AND session_id = ?2",
        params![
            candidate.provider.as_str(),
            candidate.session_id.as_str(),
            candidate.newest_raw_store_id,
            state.as_str(),
            failure_code,
            i64::from(failure_count),
            next_attempt_at_ms,
        ],
    )
    .await?;
    Ok(())
}

pub async fn next_retry_at_ms(
    conn: &(impl QueryExecutor + ?Sized),
) -> Result<Option<i64>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT MIN(next_attempt_at_ms)
             FROM lcm_summary_convergence_queue
             WHERE state = 'retryable'",
            (),
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| LcmError::Db("summary retry query returned no row".to_string()))?;
    row.get(0).map_err(Into::into)
}
