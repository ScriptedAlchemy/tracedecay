//! Bounded reclamation of bodies shared by raw messages and observations.
//!
//! Writer triggers maintain a derived reference index in the same transaction
//! as each owner. Existing owners and body candidates are enrolled incrementally
//! after those triggers are installed. No body is collected before all three
//! bootstrap frontiers are complete. A candidate and its indexed references are
//! checked under one guarded writer, so another owner cannot race collection.

use std::time::Instant;

use serde::{Deserialize, Serialize};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, WriteStatement, params};
use tracedecay_store::collect_body_refs;

#[cfg(test)]
mod bootstrap_write_tests;
pub(crate) mod schema;
#[cfg(test)]
mod tests;

const OPERATION: &str = "collect unreferenced session canonical bodies";
// Observation records may approach 1 MiB. This matches the observation audit's
// payload page bound and leaves headroom below the SQL channel's 64 MiB cap.
pub const MAX_CANONICAL_BODY_GC_ROWS: usize = 48;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalBodyGcReport {
    pub roots_scanned: u64,
    pub bodies_scanned: u64,
    /// Wall-clock duration from writer admission through commit, not CPU time.
    pub elapsed_ms: Option<u128>,
    /// Candidates discharged in the committed page, even if their body stays live.
    pub candidates_discharged: u64,
    /// Phase-qualified SQL bootstrap cursor before the committed page.
    /// The value is absent during candidate collection and idle ticks.
    pub bootstrap_frontier_before: Option<String>,
    /// The corresponding persisted cursor, verified in the same writer.
    pub bootstrap_frontier_after: Option<String>,
    pub bodies_deleted: u64,
    /// Stored BLOB bytes deleted, including the effect of their existing
    /// compression. This is not database-file or filesystem space reclaimed.
    pub stored_bytes_deleted: u64,
    pub bootstrap_complete: bool,
    /// Resume on the short maintenance cadence while bounded work remains.
    pub has_more: bool,
}

impl CanonicalBodyGcReport {
    /// A bootstrap page can commit its remaining frontier after every owner in it was deleted.
    pub fn has_committed_work(&self) -> bool {
        self.roots_scanned != 0
            || self.bodies_scanned != 0
            || self.bootstrap_frontier_after.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Progress {
    raw_through: i64,
    raw_until: i64,
    observations_through: i64,
    observations_until: i64,
    bodies_after: String,
    bodies_until: String,
}

impl Progress {
    fn complete(&self) -> bool {
        self.raw_through == self.raw_until
            && self.observations_through == self.observations_until
            && self.bodies_after == self.bodies_until
    }
}

fn db_error(error: impl std::fmt::Display) -> TraceDecayError {
    integrity_error(error.to_string())
}

fn integrity_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Database {
        operation: OPERATION.to_owned(),
        message: message.into(),
    }
}

async fn read_progress(conn: &(impl QueryExecutor + ?Sized)) -> Result<Progress> {
    let mut rows = conn
        .query(
            "SELECT raw_through, raw_until, observations_through, observations_until,
                    bodies_after, bodies_until
             FROM canonical_body_gc_progress WHERE singleton = 1",
            (),
        )
        .await
        .map_err(db_error)?;
    let row =
        rows.next().await.map_err(db_error)?.ok_or_else(|| {
            integrity_error("canonical body ownership bootstrap state is missing")
        })?;
    let progress = Progress {
        raw_through: row.get(0).map_err(db_error)?,
        raw_until: row.get(1).map_err(db_error)?,
        observations_through: row.get(2).map_err(db_error)?,
        observations_until: row.get(3).map_err(db_error)?,
        bodies_after: row.get(4).map_err(db_error)?,
        bodies_until: row.get(5).map_err(db_error)?,
    };
    if progress.raw_through < 0
        || progress.raw_through > progress.raw_until
        || progress.observations_through < 0
        || progress.observations_through > progress.observations_until
        || progress.bodies_after > progress.bodies_until
    {
        return Err(integrity_error(
            "canonical body ownership bootstrap state is invalid",
        ));
    }
    Ok(progress)
}

async fn save_progress(conn: &(impl Executor + ?Sized), progress: &Progress) -> Result<()> {
    let changed = conn
        .execute(
            "UPDATE canonical_body_gc_progress
             SET raw_through = ?1, observations_through = ?2, bodies_after = ?3
             WHERE singleton = 1 AND raw_until = ?4 AND observations_until = ?5
               AND bodies_until = ?6",
            params![
                progress.raw_through,
                progress.observations_through,
                progress.bodies_after.as_str(),
                progress.raw_until,
                progress.observations_until,
                progress.bodies_until.as_str()
            ],
        )
        .await
        .map_err(db_error)?;
    if changed != 1 {
        return Err(integrity_error("canonical body ownership frontier changed"));
    }
    // The UPDATE can report one affected row even if a database trigger
    // rewrites the checkpoint. Verify the durable value before committing the
    // references or candidate seed that depended on this bootstrap page.
    if read_progress(conn).await? != *progress {
        return Err(integrity_error(
            "canonical body ownership bootstrap frontier was not persisted",
        ));
    }
    Ok(())
}

#[tracing::instrument(name = "global_db.canonical_bodies.gc", level = "trace", skip_all)]
pub(crate) async fn run(database: &Database, max_rows: usize) -> Result<CanonicalBodyGcReport> {
    let started = Instant::now();
    let transaction = database.begin_write_transaction(OPERATION).await?;
    match step(&transaction, max_rows).await {
        Ok(mut report) => {
            transaction.commit().await?;
            report.elapsed_ms = Some(started.elapsed().as_millis());
            Ok(report)
        }
        Err(error) => match transaction.rollback().await {
            Ok(()) => Err(error),
            Err(rollback) => Err(integrity_error(format!(
                "{error}; rollback failed: {rollback}"
            ))),
        },
    }
}

async fn step(conn: &(impl Executor + ?Sized), max_rows: usize) -> Result<CanonicalBodyGcReport> {
    schema::validate(conn).await?;
    let limit = max_rows.clamp(1, MAX_CANONICAL_BODY_GC_ROWS) as i64;
    let mut progress = read_progress(conn).await?;
    let mut report = CanonicalBodyGcReport::default();
    if !progress.complete() {
        let before = progress.clone();
        let (frontier_before, frontier_after) = if progress.raw_through < progress.raw_until {
            report.roots_scanned = bootstrap_raw(conn, &mut progress, limit).await?;
            (
                format!("raw:{}", before.raw_through),
                format!("raw:{}", progress.raw_through),
            )
        } else if progress.observations_through < progress.observations_until {
            report.roots_scanned = bootstrap_observations(conn, &mut progress, limit).await?;
            (
                format!("observations:{}", before.observations_through),
                format!("observations:{}", progress.observations_through),
            )
        } else {
            report.bodies_scanned = bootstrap_bodies(conn, &mut progress, limit).await?;
            (
                format!("bodies:{}", before.bodies_after),
                format!("bodies:{}", progress.bodies_after),
            )
        };
        if progress == before {
            return Err(integrity_error(
                "canonical body ownership bootstrap page did not advance",
            ));
        }
        save_progress(conn, &progress).await?;
        report.bootstrap_frontier_before = Some(frontier_before);
        report.bootstrap_frontier_after = Some(frontier_after);
    } else {
        collect_candidates(conn, limit, &mut report).await?;
    }
    report.bootstrap_complete = progress.complete();
    report.has_more = !report.bootstrap_complete || has_candidates(conn).await?;
    Ok(report)
}

async fn bootstrap_raw(
    conn: &(impl Executor + ?Sized),
    progress: &mut Progress,
    limit: i64,
) -> Result<u64> {
    let mut rows = conn
        .query(
            "SELECT store_id FROM lcm_raw_messages
             WHERE store_id > ?1 AND store_id <= ?2 ORDER BY store_id LIMIT ?3",
            params![progress.raw_through, progress.raw_until, limit],
        )
        .await
        .map_err(db_error)?;
    let mut page = Vec::new();
    while let Some(row) = rows.next().await.map_err(db_error)? {
        page.push(row.get::<i64>(0).map_err(db_error)?);
    }
    drop(rows);
    if let Some(&last) = page.last() {
        // Both reads share the writer transaction. The last selected identity
        // bounds this indexed insert to the same page, including sparse IDs,
        // without one worker submission for every referenced owner.
        conn.execute(
            "INSERT OR IGNORE INTO canonical_body_gc_refs(owner_kind, owner_id, content_hash)
             SELECT 'raw', store_id, content_hash FROM lcm_raw_messages
             WHERE store_id > ?1 AND store_id <= ?2
               AND storage_kind = 'inline' AND content IS NULL",
            params![progress.raw_through, last],
        )
        .await
        .map_err(db_error)?;
        // A suppressed INSERT must not advance bootstrap past an owner whose
        // canonical body would subsequently appear unreferenced.
        let mut missing = conn
            .query(
                "SELECT 1 FROM lcm_raw_messages raw
                 WHERE raw.store_id > ?1 AND raw.store_id <= ?2
                   AND raw.storage_kind = 'inline' AND raw.content IS NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM canonical_body_gc_refs r
                       WHERE r.owner_kind = 'raw' AND r.owner_id = raw.store_id
                         AND r.content_hash = raw.content_hash
                   ) LIMIT 1",
                params![progress.raw_through, last],
            )
            .await
            .map_err(db_error)?;
        let incomplete = missing.next().await.map_err(db_error)?.is_some();
        drop(missing);
        if incomplete {
            return Err(integrity_error(
                "canonical raw bootstrap reference was not indexed",
            ));
        }
        progress.raw_through = last;
    }
    if page.len() < limit as usize {
        progress.raw_through = progress.raw_until;
    }
    Ok(page.len() as u64)
}

async fn bootstrap_observations(
    conn: &(impl Executor + ?Sized),
    progress: &mut Progress,
    limit: i64,
) -> Result<u64> {
    let mut rows = conn
        .query(
            "SELECT sequence, observation_json FROM observations
             WHERE sequence > ?1 AND sequence <= ?2 ORDER BY sequence LIMIT ?3",
            params![
                progress.observations_through,
                progress.observations_until,
                limit
            ],
        )
        .await
        .map_err(db_error)?;
    let mut page = Vec::new();
    while let Some(row) = rows.next().await.map_err(db_error)? {
        page.push((
            row.get::<i64>(0).map_err(db_error)?,
            row.get::<String>(1).map_err(db_error)?,
        ));
    }
    drop(rows);
    // Parsing preserves the canonical Rust reference contract, including
    // value deduplication. Submit validated writes in bounded groups so an
    // observation page cannot schedule one storage-worker hop per body hash.
    // The caller's writer transaction rolls back every group on any failure.
    const MAX_PENDING_OBSERVATION_REFS: usize = 128;
    let mut pending = Vec::new();
    let mut expected = Vec::new();
    for (owner, json) in &page {
        for hash in collect_body_refs(json).map_err(db_error)? {
            expected.push((*owner, hash.clone()));
            pending.push(
                WriteStatement::new(
                    "INSERT OR IGNORE INTO canonical_body_gc_refs(owner_kind, owner_id, content_hash)
                     VALUES ('observation', ?1, ?2)",
                    params![*owner, hash],
                )
                .map_err(db_error)?,
            );
            if pending.len() == MAX_PENDING_OBSERVATION_REFS {
                conn.execute_statements(std::mem::take(&mut pending))
                    .await
                    .map_err(db_error)?;
            }
        }
        progress.observations_through = *owner;
    }
    if !pending.is_empty() {
        conn.execute_statements(pending).await.map_err(db_error)?;
    }
    if !expected.is_empty() {
        // Verify the Rust-validated reference set, not a second parse of raw
        // observation JSON. INSERT OR IGNORE and SQLite trigger RAISE(IGNORE)
        // can both report success without creating a missing reference.
        let expected_json = serde_json::to_string(&expected).map_err(db_error)?;
        let mut missing = conn
            .query(
                "SELECT 1 FROM json_each(?1) pair
                 WHERE NOT EXISTS (
                     SELECT 1 FROM canonical_body_gc_refs r
                     WHERE r.owner_kind = 'observation'
                       AND r.owner_id = json_extract(pair.value, '$[0]')
                       AND r.content_hash = json_extract(pair.value, '$[1]')
                 ) LIMIT 1",
                params![expected_json],
            )
            .await
            .map_err(db_error)?;
        let incomplete = missing.next().await.map_err(db_error)?.is_some();
        drop(missing);
        if incomplete {
            return Err(integrity_error(
                "canonical observation bootstrap reference was not indexed",
            ));
        }
    }
    if page.len() < limit as usize {
        progress.observations_through = progress.observations_until;
    }
    Ok(page.len() as u64)
}

async fn bootstrap_bodies(
    conn: &(impl Executor + ?Sized),
    progress: &mut Progress,
    limit: i64,
) -> Result<u64> {
    let mut rows = conn
        .query(
            "SELECT content_hash FROM session_canonical_bodies
             WHERE content_hash > ?1 AND content_hash <= ?2 ORDER BY content_hash LIMIT ?3",
            params![
                progress.bodies_after.as_str(),
                progress.bodies_until.as_str(),
                limit
            ],
        )
        .await
        .map_err(db_error)?;
    let mut page = Vec::new();
    while let Some(row) = rows.next().await.map_err(db_error)? {
        page.push(row.get::<String>(0).map_err(db_error)?);
    }
    drop(rows);
    if let Some(last) = page.last() {
        // Seed the selected key range in one statement, not one asynchronous
        // writer submission per body. No body payload is materialized here.
        conn.execute(
            "INSERT OR IGNORE INTO canonical_body_gc_candidates(content_hash)
             SELECT content_hash FROM session_canonical_bodies
             WHERE content_hash > ?1 AND content_hash <= ?2",
            params![progress.bodies_after.as_str(), last.as_str()],
        )
        .await
        .map_err(db_error)?;
        // A vetoed candidate insert otherwise leaves an orphaned body behind
        // the permanently committed bootstrap cursor, with no future seed.
        let mut missing = conn
            .query(
                "SELECT 1 FROM session_canonical_bodies b
                 WHERE b.content_hash > ?1 AND b.content_hash <= ?2
                   AND NOT EXISTS (
                       SELECT 1 FROM canonical_body_gc_candidates c
                       WHERE c.content_hash = b.content_hash
                   ) LIMIT 1",
                params![progress.bodies_after.as_str(), last.as_str()],
            )
            .await
            .map_err(db_error)?;
        let incomplete = missing.next().await.map_err(db_error)?.is_some();
        drop(missing);
        if incomplete {
            return Err(integrity_error(
                "canonical body bootstrap candidate was not indexed",
            ));
        }
        progress.bodies_after.clone_from(last);
    }
    if page.len() < limit as usize {
        progress.bodies_after.clone_from(&progress.bodies_until);
    }
    Ok(page.len() as u64)
}

async fn collect_candidates(
    conn: &(impl Executor + ?Sized),
    limit: i64,
    report: &mut CanonicalBodyGcReport,
) -> Result<()> {
    let mut rows = conn
        .query(
            "SELECT content_hash FROM canonical_body_gc_candidates ORDER BY content_hash LIMIT ?1",
            params![limit],
        )
        .await
        .map_err(db_error)?;
    let mut hashes = Vec::new();
    while let Some(row) = rows.next().await.map_err(db_error)? {
        hashes.push(row.get::<String>(0).map_err(db_error)?);
    }
    drop(rows);
    report.bodies_scanned = hashes.len() as u64;
    if hashes.is_empty() {
        return Ok(());
    }
    let hashes = serde_json::to_string(&hashes).map_err(db_error)?;
    let mut deleted = conn
        .query(
            "DELETE FROM session_canonical_bodies
             WHERE content_hash IN (SELECT value FROM json_each(?1))
               AND NOT EXISTS (SELECT 1 FROM canonical_body_gc_refs r
                               WHERE r.content_hash = session_canonical_bodies.content_hash)
             RETURNING length(body)",
            params![hashes.as_str()],
        )
        .await
        .map_err(db_error)?;
    while let Some(row) = deleted.next().await.map_err(db_error)? {
        let bytes = u64::try_from(row.get::<i64>(0).map_err(db_error)?)
            .map_err(|_| integrity_error("canonical body stored byte count is negative"))?;
        report.bodies_deleted += 1;
        report.stored_bytes_deleted = report.stored_bytes_deleted.saturating_add(bytes);
    }
    drop(deleted);
    // An ignored or otherwise suppressed body DELETE must not silently
    // discharge an unreferenced candidate. Once that candidate disappears,
    // no owner-triggered re-enqueue is guaranteed, and the body can persist
    // indefinitely without another collection attempt.
    let mut uncollected = conn
        .query(
            "SELECT 1 FROM session_canonical_bodies b
             WHERE b.content_hash IN (SELECT value FROM json_each(?1))
               AND NOT EXISTS (SELECT 1 FROM canonical_body_gc_refs r
                               WHERE r.content_hash = b.content_hash)
             LIMIT 1",
            params![hashes.as_str()],
        )
        .await
        .map_err(db_error)?;
    if uncollected.next().await.map_err(db_error)?.is_some() {
        return Err(integrity_error(
            "canonical body collection left an unreferenced body behind",
        ));
    }
    drop(uncollected);
    // Referenced candidates are discharged too. The final owner removal
    // records them again; idle ticks never revisit every retained body.
    // Deleting an unreferenced body cascades to its candidate row. The
    // remaining explicit removals discharge referenced bodies that stay live.
    let explicitly_discharged = conn
        .execute(
            "DELETE FROM canonical_body_gc_candidates
             WHERE content_hash IN (SELECT value FROM json_each(?1))",
            params![hashes.as_str()],
        )
        .await
        .map_err(db_error)?;
    // A DELETE row count is not a durable discharge receipt: an AFTER DELETE
    // trigger may have re-enqueued the very same candidates. Refuse the page
    // under this writer instead of recommitting an identical 48-row batch.
    let mut remaining = conn
        .query(
            "SELECT 1 FROM canonical_body_gc_candidates
             WHERE content_hash IN (SELECT value FROM json_each(?1)) LIMIT 1",
            params![hashes.as_str()],
        )
        .await
        .map_err(db_error)?;
    if remaining.next().await.map_err(db_error)?.is_some() {
        return Err(integrity_error(
            "canonical body candidate discharge was not persisted",
        ));
    }
    drop(remaining);
    let discharged = explicitly_discharged
        .checked_add(report.bodies_deleted)
        .ok_or_else(|| integrity_error("canonical body candidate discharge overflow"))?;
    if discharged != report.bodies_scanned {
        return Err(integrity_error(
            "canonical body candidate page was not fully discharged",
        ));
    }
    report.candidates_discharged = discharged;
    Ok(())
}

async fn has_candidates(conn: &(impl QueryExecutor + ?Sized)) -> Result<bool> {
    let mut rows = conn
        .query("SELECT 1 FROM canonical_body_gc_candidates LIMIT 1", ())
        .await
        .map_err(db_error)?;
    rows.next().await.map(|row| row.is_some()).map_err(db_error)
}
