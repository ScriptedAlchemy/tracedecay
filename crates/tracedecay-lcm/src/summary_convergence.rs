//! Durable bounded work queue for retained-session summary convergence.

use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use crate::{LcmCompressionResponse, LcmError, schema};

const BACKFILL_FRONTIER_KEY: &str = "summary_convergence_queue_backfill_store_id_v1";

const SUMMARY_CONVERGENCE_TABLE_SQL: &str = r#"
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
    raw_revision_generation INTEGER NOT NULL DEFAULT 0,
    stale_from_store_id INTEGER,
    UNIQUE(provider, session_id),
    FOREIGN KEY(provider, session_id)
        REFERENCES sessions(provider, session_id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS lcm_summary_convergence_dirty_raw (
    provider TEXT NOT NULL,
    session_id TEXT NOT NULL,
    store_id INTEGER NOT NULL,
    PRIMARY KEY(provider, session_id, store_id),
    FOREIGN KEY(provider, session_id)
        REFERENCES sessions(provider, session_id) ON DELETE CASCADE
);
"#;

const SUMMARY_CONVERGENCE_WORK_SQL: &str = r#"
CREATE INDEX IF NOT EXISTS idx_lcm_summary_convergence_due
    ON lcm_summary_convergence_queue(
        next_attempt_at_ms, attempt_generation, queue_id
    )
    WHERE state IN ('pending', 'retryable');
CREATE TRIGGER IF NOT EXISTS lcm_summary_convergence_raw_insert
    AFTER INSERT ON lcm_raw_messages BEGIN
        INSERT INTO lcm_summary_convergence_queue (
            provider, session_id, newest_raw_store_id, raw_revision_generation
        ) VALUES (NEW.provider, NEW.session_id, NEW.store_id, 1)
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
            END,
            raw_revision_generation =
                lcm_summary_convergence_queue.raw_revision_generation + 1;
    END;
CREATE TRIGGER IF NOT EXISTS lcm_summary_convergence_raw_unprotected_update
    AFTER UPDATE OF provider, session_id, role, ordinal, timestamp,
                    content_hash, storage_kind, payload_ref, metadata_json
                    ON lcm_raw_messages
    WHEN OLD.provider IS NOT NEW.provider
      OR OLD.session_id IS NOT NEW.session_id
      OR OLD.role IS NOT NEW.role
      OR OLD.ordinal IS NOT NEW.ordinal
      OR OLD.timestamp IS NOT NEW.timestamp
      OR OLD.content_hash IS NOT NEW.content_hash
      OR OLD.storage_kind IS NOT NEW.storage_kind
      OR OLD.payload_ref IS NOT NEW.payload_ref
      OR OLD.metadata_json IS NOT NEW.metadata_json
    BEGIN
        INSERT INTO lcm_summary_convergence_dirty_raw (
            provider, session_id, store_id
        ) VALUES (NEW.provider, NEW.session_id, NEW.store_id)
        ON CONFLICT(provider, session_id, store_id) DO NOTHING;
        INSERT INTO lcm_summary_convergence_queue (
            provider, session_id, newest_raw_store_id,
            protection_frontier_store_id, raw_revision_generation,
            stale_from_store_id
        ) VALUES (
            NEW.provider, NEW.session_id, NEW.store_id,
            CASE
              WHEN json_valid(NEW.metadata_json)
               AND json_extract(
                   NEW.metadata_json,
                   '$.ingest_protection.sanitization_receipt'
               ) IS NOT NULL
              THEN NEW.store_id
              ELSE MAX(0, NEW.store_id - 1)
            END,
            1,
            CASE
              WHEN OLD.role IS NOT NEW.role
                OR OLD.ordinal IS NOT NEW.ordinal
                OR OLD.timestamp IS NOT NEW.timestamp
                OR OLD.content_hash IS NOT NEW.content_hash
                OR OLD.storage_kind IS NOT NEW.storage_kind
                OR OLD.payload_ref IS NOT NEW.payload_ref
                OR OLD.metadata_json IS NOT NEW.metadata_json
              THEN NEW.store_id
              ELSE NULL
            END
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
            attempted_raw_store_id = CASE
              WHEN excluded.stale_from_store_id IS NOT NULL
              THEN MIN(
                  lcm_summary_convergence_queue.attempted_raw_store_id,
                  MAX(0, excluded.stale_from_store_id - 1)
              )
              ELSE lcm_summary_convergence_queue.attempted_raw_store_id
            END,
            stale_from_store_id = CASE
              WHEN excluded.stale_from_store_id IS NULL
              THEN lcm_summary_convergence_queue.stale_from_store_id
              WHEN lcm_summary_convergence_queue.stale_from_store_id IS NULL
              THEN excluded.stale_from_store_id
              ELSE MIN(
                  lcm_summary_convergence_queue.stale_from_store_id,
                  excluded.stale_from_store_id
              )
            END,
            state = 'pending',
            failure_code = NULL,
            failure_count = 0,
            next_attempt_at_ms = 0,
            raw_revision_generation =
                lcm_summary_convergence_queue.raw_revision_generation + 1;
    END;
"#;

pub const NEXT_CANDIDATE_SQL: &str = "SELECT provider, session_id, newest_raw_store_id,
            protection_frontier_store_id, attempted_raw_store_id,
            failure_count, raw_revision_generation, stale_from_store_id
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
    pub raw_revision_generation: i64,
    pub stale_from_store_id: Option<i64>,
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
    conn.execute_batch(SUMMARY_CONVERGENCE_TABLE_SQL).await?;
    for (column, definition) in [
        ("raw_revision_generation", "INTEGER NOT NULL DEFAULT 0"),
        ("stale_from_store_id", "INTEGER"),
    ] {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM pragma_table_info('lcm_summary_convergence_queue')
                 WHERE name = ?1",
                params![column],
            )
            .await?;
        let exists = rows
            .next()
            .await?
            .ok_or_else(|| LcmError::Db("summary queue column query returned no row".into()))?
            .get::<i64>(0)?
            != 0;
        drop(rows);
        if !exists {
            conn.execute_batch(&format!(
                "ALTER TABLE lcm_summary_convergence_queue ADD COLUMN {column} {definition}"
            ))
            .await?;
        }
    }
    for (trigger, required_fragment) in [
        (
            "lcm_summary_convergence_raw_insert",
            "raw_revision_generation",
        ),
        (
            "lcm_summary_convergence_raw_unprotected_update",
            "lcm_summary_convergence_dirty_raw",
        ),
    ] {
        let mut rows = conn
            .query(
                "SELECT sql FROM sqlite_schema WHERE type = 'trigger' AND name = ?1",
                params![trigger],
            )
            .await?;
        let existing = rows
            .next()
            .await?
            .map(|row| row.get::<Option<String>>(0))
            .transpose()?
            .flatten();
        drop(rows);
        if existing.is_some_and(|sql| !sql.contains(required_fragment)) {
            conn.execute_batch(&format!("DROP TRIGGER {trigger}"))
                .await?;
        }
    }
    conn.execute_batch(SUMMARY_CONVERGENCE_WORK_SQL).await?;
    conn.execute_batch(
        "INSERT INTO lcm_summary_convergence_dirty_raw (provider, session_id, store_id)
         SELECT provider, session_id, stale_from_store_id
         FROM lcm_summary_convergence_queue
         WHERE stale_from_store_id IS NOT NULL
         ON CONFLICT(provider, session_id, store_id) DO NOTHING",
    )
    .await?;
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
        raw_revision_generation: row.get(6)?,
        stale_from_store_id: row.get(7)?,
    }))
}

pub async fn candidate_for_session(
    conn: &(impl QueryExecutor + ?Sized),
    provider: &str,
    session_id: &str,
) -> Result<Option<LcmSummaryConvergenceCandidate>, LcmError> {
    let mut rows = conn
        .query(
            "SELECT provider, session_id, newest_raw_store_id,
                    protection_frontier_store_id, attempted_raw_store_id,
                    failure_count, raw_revision_generation, stale_from_store_id
             FROM lcm_summary_convergence_queue
             WHERE provider = ?1 AND session_id = ?2
               AND state IN ('pending', 'retryable')
             LIMIT 1",
            params![provider, session_id],
        )
        .await?;
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
        raw_revision_generation: row.get(6)?,
        stale_from_store_id: row.get(7)?,
    }))
}

pub async fn record_current_protection_progress(
    conn: &(impl Executor + ?Sized),
    provider: &str,
    session_id: &str,
    protection_frontier_store_id: i64,
) -> Result<(), LcmError> {
    conn.execute(
        "UPDATE lcm_summary_convergence_queue
         SET protection_frontier_store_id = MAX(
                 protection_frontier_store_id, ?3
             ),
             attempt_generation = attempt_generation + 1
         WHERE provider = ?1 AND session_id = ?2",
        params![provider, session_id, protection_frontier_store_id],
    )
    .await?;
    Ok(())
}

pub async fn complete_stale_raw_revision(
    conn: &(impl Executor + ?Sized),
    candidate: &LcmSummaryConvergenceCandidate,
) -> Result<bool, LcmError> {
    let Some(store_id) = candidate.stale_from_store_id else {
        return Ok(false);
    };
    let removed = conn
        .execute(
            "DELETE FROM lcm_summary_convergence_dirty_raw
             WHERE provider = ?1 AND session_id = ?2 AND store_id = ?3",
            params![
                candidate.provider.as_str(),
                candidate.session_id.as_str(),
                store_id,
            ],
        )
        .await?;
    if removed != 1 {
        require_candidate_revision(conn, candidate).await?;
        return Err(LcmError::Db(
            "retained raw revision disposition affected no row".to_string(),
        ));
    }
    let advanced = conn
        .execute(
            "UPDATE lcm_summary_convergence_queue
         SET stale_from_store_id = (
                 SELECT MIN(dirty.store_id)
                 FROM lcm_summary_convergence_dirty_raw AS dirty
                 WHERE dirty.provider = ?1 AND dirty.session_id = ?2
             ),
             attempt_generation = attempt_generation + 1
         WHERE provider = ?1 AND session_id = ?2
           AND raw_revision_generation = ?4",
            params![
                candidate.provider.as_str(),
                candidate.session_id.as_str(),
                store_id,
                candidate.raw_revision_generation,
            ],
        )
        .await?;
    if advanced != 1 {
        require_candidate_revision(conn, candidate).await?;
        return Err(LcmError::Db(
            "retained raw revision frontier affected no row".to_string(),
        ));
    }
    Ok(true)
}

pub async fn record_outcome(
    conn: &(impl Executor + ?Sized),
    candidate: &LcmSummaryConvergenceCandidate,
    state: LcmSummaryConvergenceQueueState,
    failure_code: Option<&str>,
    failure_count: u32,
    next_attempt_at_ms: i64,
) -> Result<bool, LcmError> {
    let affected = conn
        .execute(
            "UPDATE lcm_summary_convergence_queue
         SET attempted_raw_store_id = ?3,
             state = CASE
                 WHEN newest_raw_store_id > ?3 OR EXISTS (
                     SELECT 1 FROM lcm_summary_convergence_dirty_raw AS dirty
                     WHERE dirty.provider = ?1 AND dirty.session_id = ?2
                 ) THEN 'pending'
                 ELSE ?4
             END,
             failure_code = CASE
                 WHEN newest_raw_store_id > ?3 OR EXISTS (
                     SELECT 1 FROM lcm_summary_convergence_dirty_raw AS dirty
                     WHERE dirty.provider = ?1 AND dirty.session_id = ?2
                 ) THEN NULL
                 ELSE ?5
             END,
             failure_count = CASE
                 WHEN newest_raw_store_id > ?3 OR EXISTS (
                     SELECT 1 FROM lcm_summary_convergence_dirty_raw AS dirty
                     WHERE dirty.provider = ?1 AND dirty.session_id = ?2
                 ) THEN 0
                 ELSE ?6
             END,
             next_attempt_at_ms = CASE
                 WHEN newest_raw_store_id > ?3 OR EXISTS (
                     SELECT 1 FROM lcm_summary_convergence_dirty_raw AS dirty
                     WHERE dirty.provider = ?1 AND dirty.session_id = ?2
                 ) THEN 0
                 ELSE ?7
             END,
             attempt_generation = attempt_generation + 1,
             stale_from_store_id = (
                 SELECT MIN(dirty.store_id)
                 FROM lcm_summary_convergence_dirty_raw AS dirty
                 WHERE dirty.provider = ?1 AND dirty.session_id = ?2
             )
         WHERE provider = ?1 AND session_id = ?2
           AND raw_revision_generation = ?8",
            params![
                candidate.provider.as_str(),
                candidate.session_id.as_str(),
                candidate.newest_raw_store_id,
                state.as_str(),
                failure_code,
                i64::from(failure_count),
                next_attempt_at_ms,
                candidate.raw_revision_generation,
            ],
        )
        .await?;
    Ok(affected == 1)
}

pub async fn require_candidate_revision(
    conn: &(impl QueryExecutor + ?Sized),
    candidate: &LcmSummaryConvergenceCandidate,
) -> Result<(), LcmError> {
    let mut rows = conn
        .query(
            "SELECT raw_revision_generation
             FROM lcm_summary_convergence_queue
             WHERE provider = ?1 AND session_id = ?2",
            params![candidate.provider.as_str(), candidate.session_id.as_str()],
        )
        .await?;
    let actual = rows
        .next()
        .await?
        .map(|row| row.get::<i64>(0))
        .transpose()?;
    if actual != Some(candidate.raw_revision_generation) {
        return Err(LcmError::StaleRawRevision {
            expected: candidate.raw_revision_generation,
            actual,
        });
    }
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
