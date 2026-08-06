use std::collections::BTreeSet;

use crate::global_db_operation_error;
use tracedecay_domain::{
    DurableObservationV1, ObservationOrderingDomainV1, ObservationSourceCursorV1,
};
use tracedecay_runtime_core::db::engine::{QueryExecutor, params};

use super::rows::{authority_violation, decode_authority_json, encode_authority_json};
use super::{OBSERVATION_AUDIT_PAGE_ROWS, OPERATION};

struct CommittedCursorCandidate {
    source_json: String,
    scope_json: String,
    cursor: ObservationSourceCursorV1,
}

async fn latest_committed_source_cursors(
    conn: &impl QueryExecutor,
    after_sequence: i64,
) -> tracedecay_runtime_core::errors::Result<Vec<CommittedCursorCandidate>> {
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
) -> tracedecay_runtime_core::errors::Result<Option<ObservationSourceCursorV1>> {
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

async fn cursor_has_exact_advance_receipt(
    conn: &impl QueryExecutor,
    source_json: &str,
    scope_json: &str,
    cursor: &ObservationSourceCursorV1,
) -> tracedecay_runtime_core::errors::Result<bool> {
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
) -> tracedecay_runtime_core::errors::Result<()> {
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
