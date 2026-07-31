use std::collections::BTreeSet;

use tracedecay_domain::{
    DurableObservationV1, ObservationOrderingDomainV1, ObservationSourceCursorV1,
};
use tracedecay_store::SESSION_MESSAGE_PROJECTOR_VERSION;

use crate::db::engine::{Executor, QueryExecutor, params};
use crate::global_db_operation_error;

use super::rows::{authority_violation, decode_authority_json, encode_authority_json};
use super::{AUDIT_PAGE_ROWS, OBSERVATION_AUDIT_PAGE_ROWS, OPERATION};

struct CommittedCursorCandidate {
    source_json: String,
    scope_json: String,
    cursor_json: String,
    cursor: ObservationSourceCursorV1,
}

/// Reads the current observation frontier: `COALESCE(MAX(sequence), 0)` over
/// `observations`. The row cursor lives only inside this function, so it is
/// fully consumed and dropped before returning — see
/// `observation_projection::rebuild::read_observation_frontier` for why a
/// caller doing further reads or writes on the same connection depends on
/// that.
async fn read_observation_frontier(conn: &impl QueryExecutor) -> crate::errors::Result<i64> {
    let mut rows = conn
        .query("SELECT COALESCE(MAX(sequence), 0) FROM observations", ())
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    rows.next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .ok_or_else(|| authority_violation("observation frontier query returned no row"))?
        .get::<i64>(0)
        .map_err(|error| global_db_operation_error(OPERATION, error))
}

pub(super) async fn repair_projection_frontier(
    conn: &impl Executor,
    trusted_checkpoint: i64,
) -> crate::errors::Result<i64> {
    let mut rows = conn
        .query(
            "SELECT last_sequence FROM observation_projection_checkpoints
             WHERE projector_version = ?1",
            params![SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let stored_checkpoint = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .map(|row| row.get::<i64>(0))
        .transpose()
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .unwrap_or(0);
    drop(rows);

    let observation_frontier = read_observation_frontier(conn).await?;
    let checkpoint = stored_checkpoint.min(observation_frontier);
    let coverage_start = if checkpoint < trusted_checkpoint {
        0
    } else {
        trusted_checkpoint
    };

    let mut repaired_checkpoint = coverage_start;
    let mut scan_cursor = coverage_start;
    loop {
        let mut coverage = conn
            .query(
                "SELECT observation.sequence,
                    ((EXISTS(
                        SELECT 1 FROM observation_projection_provenance AS provenance
                        WHERE provenance.projector_version = ?1
                          AND provenance.observation_id = observation.observation_id
                    ) OR EXISTS(
                        SELECT 1 FROM observation_workflow_facts AS workflow
                        WHERE workflow.projector_version = ?1
                          AND workflow.observation_id = observation.observation_id
                    )) + EXISTS(
                        SELECT 1 FROM observation_projection_dispositions AS disposition
                        WHERE disposition.projector_version = ?1
                          AND disposition.observation_id = observation.observation_id
                    )) AS disposition_count
             FROM observations AS observation
             WHERE observation.sequence > ?2 AND observation.sequence <= ?3
             ORDER BY observation.sequence ASC LIMIT ?4",
                params![
                    SESSION_MESSAGE_PROJECTOR_VERSION,
                    scan_cursor,
                    checkpoint,
                    AUDIT_PAGE_ROWS
                ],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let mut page_rows = 0_i64;
        let mut reached_gap = false;
        while let Some(row) = coverage
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        {
            page_rows += 1;
            let sequence = row
                .get::<i64>(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            let disposition_count = row
                .get::<i64>(1)
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            scan_cursor = sequence;
            if disposition_count != 1 {
                reached_gap = true;
                break;
            }
            repaired_checkpoint = sequence;
        }
        drop(coverage);
        if reached_gap || page_rows < AUDIT_PAGE_ROWS {
            break;
        }
    }

    if repaired_checkpoint != stored_checkpoint {
        conn.execute(
            "UPDATE observation_projection_checkpoints SET last_sequence = ?2
             WHERE projector_version = ?1",
            params![SESSION_MESSAGE_PROJECTOR_VERSION, repaired_checkpoint],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    }

    conn.execute(
        "DELETE FROM projection_queue
         WHERE NOT EXISTS (
                SELECT 1 FROM observations
                WHERE observations.observation_id = projection_queue.observation_id
                  AND observations.sequence = projection_queue.observation_sequence
            )",
        (),
    )
    .await
    .map_err(|error| global_db_operation_error(OPERATION, error))?;
    conn.execute(
        "DELETE FROM projection_queue WHERE observation_sequence <= ?1",
        params![repaired_checkpoint],
    )
    .await
    .map_err(|error| global_db_operation_error(OPERATION, error))?;
    conn.execute(
        "INSERT OR IGNORE INTO projection_queue (observation_id, observation_sequence)
         SELECT observation_id, sequence FROM observations WHERE sequence > ?1",
        params![repaired_checkpoint],
    )
    .await
    .map_err(|error| global_db_operation_error(OPERATION, error))?;
    Ok(repaired_checkpoint)
}

pub(super) async fn repair_committed_source_cursors(
    conn: &impl Executor,
    after_sequence: i64,
) -> crate::errors::Result<()> {
    let candidates = latest_committed_source_cursors(conn, after_sequence).await?;
    for candidate in candidates {
        let stored =
            read_source_cursor(conn, &candidate.source_json, &candidate.scope_json).await?;
        match stored {
            None => write_source_cursor(conn, &candidate).await?,
            Some(stored) if stored == candidate.cursor => {}
            Some(stored)
                if stored.generation() == candidate.cursor.generation()
                    && stored.ordering_domain() == candidate.cursor.ordering_domain()
                    && stored.position() < candidate.cursor.position() =>
            {
                write_source_cursor(conn, &candidate).await?;
            }
            Some(stored) if is_new_generation_frontier(&stored, &candidate.cursor) => {
                // A replacement generation may be recorded before its first
                // complete frame. Older committed observations must not pull
                // that zero frontier back into the previous generation.
            }
            Some(stored)
                if cursor_has_exact_advance_receipt(
                    conn,
                    &candidate.source_json,
                    &candidate.scope_json,
                    &stored,
                )
                .await? => {}
            Some(stored)
                if stored.generation() == candidate.cursor.generation()
                    && stored.ordering_domain() == candidate.cursor.ordering_domain()
                    && stored.position() > candidate.cursor.position() =>
            {
                // Older builds could advance this derived frontier without a
                // durable coverage receipt. Rewind to the last canonical
                // observation so replay can safely reconstruct the suffix.
                write_source_cursor(conn, &candidate).await?;
            }
            Some(_) => {
                return Err(authority_violation(
                    "source cursor cannot be reconciled with the latest committed observation",
                ));
            }
        }
    }
    Ok(())
}

async fn latest_committed_source_cursors(
    conn: &impl QueryExecutor,
    after_sequence: i64,
) -> crate::errors::Result<Vec<CommittedCursorCandidate>> {
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    // Newest-first keyset cursor. `sequence` is the observations rowid, so an
    // exclusive upper bound walks the suffix backwards one page at a time.
    let mut scan_cursor = i64::MAX;
    loop {
        let mut rows = conn
            .query(
                "SELECT sequence, observation_json, committed_cursor_json
             FROM observations WHERE sequence > ?1 AND sequence < ?2
             ORDER BY sequence DESC LIMIT ?3",
                params![after_sequence, scan_cursor, OBSERVATION_AUDIT_PAGE_ROWS],
            )
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?;
        let mut page_rows = 0_i64;
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| global_db_operation_error(OPERATION, error))?
        {
            page_rows += 1;
            scan_cursor = row
                .get::<i64>(0)
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            let observation_json = row
                .get::<String>(1)
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            let cursor_json = row
                .get::<String>(2)
                .map_err(|error| global_db_operation_error(OPERATION, error))?;
            let observation: DurableObservationV1 =
                decode_authority_json(&observation_json, "committed observation authority JSON")?;
            let cursor: ObservationSourceCursorV1 =
                decode_authority_json(&cursor_json, "committed source cursor authority JSON")?;
            let source_json =
                encode_authority_json(observation.source(), "observation source JSON")?;
            let scope_json = encode_authority_json(observation.scope(), "observation scope JSON")?;
            if seen.insert((source_json.clone(), scope_json.clone())) {
                candidates.push(CommittedCursorCandidate {
                    source_json,
                    scope_json,
                    cursor_json: encode_authority_json(&cursor, "committed source cursor JSON")?,
                    cursor,
                });
            }
        }
        drop(rows);
        if page_rows < OBSERVATION_AUDIT_PAGE_ROWS {
            return Ok(candidates);
        }
    }
}

fn is_new_generation_frontier(
    stored: &ObservationSourceCursorV1,
    committed: &ObservationSourceCursorV1,
) -> bool {
    stored.ordering_domain() == ObservationOrderingDomainV1::FileBytes
        && committed.ordering_domain() == ObservationOrderingDomainV1::FileBytes
        && stored.generation() > committed.generation()
        && stored.position() == 0
}

async fn read_source_cursor(
    conn: &impl QueryExecutor,
    source_json: &str,
    scope_json: &str,
) -> crate::errors::Result<Option<ObservationSourceCursorV1>> {
    let mut rows = conn
        .query(
            "SELECT cursor_json FROM source_cursors
             WHERE source_json = ?1 AND scope_json = ?2",
            params![source_json, scope_json],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    let cursor_json = rows
        .next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .map(|row| row.get::<String>(0))
        .transpose()
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    cursor_json
        .map(|json| decode_authority_json(&json, "source cursor authority JSON"))
        .transpose()
}

async fn write_source_cursor(
    conn: &impl Executor,
    candidate: &CommittedCursorCandidate,
) -> crate::errors::Result<()> {
    conn.execute(
        "INSERT INTO source_cursors(source_json, scope_json, cursor_json)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(source_json, scope_json) DO UPDATE SET
            cursor_json = excluded.cursor_json",
        params![
            candidate.source_json.as_str(),
            candidate.scope_json.as_str(),
            candidate.cursor_json.as_str()
        ],
    )
    .await
    .map(|_| ())
    .map_err(|error| global_db_operation_error(OPERATION, error))
}

async fn cursor_has_exact_advance_receipt(
    conn: &impl QueryExecutor,
    source_json: &str,
    scope_json: &str,
    cursor: &ObservationSourceCursorV1,
) -> crate::errors::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM source_cursor_advances
             WHERE source_json = ?1 AND scope_json = ?2
               AND CAST(json_extract(coverage_json, '$.generation') AS TEXT) = ?3
               AND json_extract(coverage_json, '$.ordering_domain') = ?4
               AND CAST(json_extract(coverage_json, '$.range.end') AS TEXT) = ?5
             LIMIT 1",
            params![
                source_json,
                scope_json,
                cursor.generation().generation_id().to_string(),
                cursor.ordering_domain().as_str(),
                cursor.position().to_string()
            ],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| global_db_operation_error(OPERATION, error))
}

pub(super) async fn validate_observation_cursor_coverage(
    conn: &impl QueryExecutor,
    after_sequence: i64,
) -> crate::errors::Result<()> {
    for candidate in latest_committed_source_cursors(conn, after_sequence).await? {
        let Some(stored) =
            read_source_cursor(conn, &candidate.source_json, &candidate.scope_json).await?
        else {
            return Err(authority_violation(
                "committed observation has no source cursor authority row",
            ));
        };
        if stored == candidate.cursor {
            continue;
        }
        if is_new_generation_frontier(&stored, &candidate.cursor) {
            continue;
        }
        if !cursor_has_exact_advance_receipt(
            conn,
            &candidate.source_json,
            &candidate.scope_json,
            &stored,
        )
        .await?
        {
            return Err(authority_violation(
                "source cursor does not exactly match committed or non-durable authority",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
