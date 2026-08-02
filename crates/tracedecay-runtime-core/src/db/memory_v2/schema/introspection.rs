//! Schema-shape introspection probes and the payload-bearing assertion-header
//! scrub used during V20 upgrades.

use serde_json::{Value, json};

use crate::db::engine::params;
use crate::errors::Result;

use super::super::{
    MemoryV2Executor, db_error, db_message, json_text, optional_string, row_exists,
};
use super::baseline::create_schema;

pub(in crate::db::memory_v2) async fn table_has_column(
    conn: &impl MemoryV2Executor,
    table: &str,
    column: &str,
    operation: &str,
) -> Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM pragma_table_xinfo(?1) WHERE name = ?2 COLLATE NOCASE",
            params![table, column],
        )
        .await
        .map_err(|error| db_error(operation, error))?;
    rows.next()
        .await
        .map(|row| row.is_some())
        .map_err(|error| db_error(operation, error))
}

pub(in crate::db::memory_v2) async fn table_exists(
    conn: &impl MemoryV2Executor,
    table: &str,
) -> Result<bool> {
    row_exists(
        conn,
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
    )
    .await
}

async fn trigger_exists(conn: &impl MemoryV2Executor, trigger: &str) -> Result<bool> {
    row_exists(
        conn,
        "SELECT 1 FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
        params![trigger],
    )
    .await
}

/// V20/V21 retain their original feedback-backfill behavior. The V22 map and
/// history must be installed together before a backfill can write either.
async fn proposal_current_is_v22(conn: &impl MemoryV2Executor) -> Result<bool> {
    let Some(sql) = optional_string(
        conn,
        "SELECT sql FROM sqlite_master
         WHERE type = 'table' AND name = 'memory_v2_proposal_current'",
        (),
    )
    .await?
    else {
        return Ok(false);
    };
    let sql = sql.to_ascii_lowercase();
    Ok(sql.contains("'quarantined'")
        && !sql.contains("'applying'")
        && sql.contains("revision >= 1"))
}

pub(in crate::db::memory_v2) async fn proposal_schema_is_v22(
    conn: &impl MemoryV2Executor,
) -> Result<bool> {
    if !proposal_current_is_v22(conn).await? {
        return Ok(false);
    }
    let Some(transitions_sql) = optional_string(
        conn,
        "SELECT sql FROM sqlite_master
         WHERE type = 'table' AND name = 'memory_v2_proposal_transitions'",
        (),
    )
    .await?
    else {
        return Ok(false);
    };
    Ok(transitions_sql
        .to_ascii_lowercase()
        .contains("'quarantined'")
        && trigger_exists(conn, "memory_v2_proposal_transitions_no_new_applying").await?)
}

pub(super) async fn scrub_payload_bearing_assertion_headers(
    conn: &impl MemoryV2Executor,
    operation: &str,
) -> Result<()> {
    const SCRUB_BATCH_SIZE: i64 = 256;

    struct HeaderRow {
        assertion_id: String,
        fact_id: String,
        owner_kind: String,
        project_id: String,
        owner_json: String,
        kind_json: String,
        payload_reference_json: String,
        asserted_at: i64,
        actor_id: Option<String>,
    }

    let mut trigger_dropped = false;
    loop {
        let mut rows = conn
            .query(
                "SELECT assertion_id, fact_id, owner_kind, project_id, owner_json,
                        kind_json, payload_reference_json, asserted_at, actor_id
                 FROM memory_v2_assertions
                 WHERE json_type(assertion_header_json, '$.payload') IS NOT NULL
                    OR json_type(assertion_header_json, '$.content') IS NOT NULL
                 ORDER BY assertion_id, fact_id, owner_kind, project_id
                 LIMIT ?1",
                params![SCRUB_BATCH_SIZE],
            )
            .await
            .map_err(|error| db_error(operation, error))?;
        let mut headers = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| db_error(operation, error))?
        {
            headers.push(HeaderRow {
                assertion_id: row.get(0).map_err(|error| db_error(operation, error))?,
                fact_id: row.get(1).map_err(|error| db_error(operation, error))?,
                owner_kind: row.get(2).map_err(|error| db_error(operation, error))?,
                project_id: row.get(3).map_err(|error| db_error(operation, error))?,
                owner_json: row.get(4).map_err(|error| db_error(operation, error))?,
                kind_json: row.get(5).map_err(|error| db_error(operation, error))?,
                payload_reference_json: row.get(6).map_err(|error| db_error(operation, error))?,
                asserted_at: row.get(7).map_err(|error| db_error(operation, error))?,
                actor_id: row.get(8).map_err(|error| db_error(operation, error))?,
            });
        }
        drop(rows);
        if headers.is_empty() {
            break;
        }

        if !trigger_dropped {
            conn.execute_batch("DROP TRIGGER IF EXISTS memory_v2_assertions_no_update;")
                .await
                .map_err(|error| db_error(operation, error))?;
            trigger_dropped = true;
        }
        for header in headers {
            let owner = serde_json::from_str::<Value>(&header.owner_json)
                .map_err(|_| db_message(operation, "legacy assertion owner is not valid JSON"))?;
            let kind = serde_json::from_str::<Value>(&header.kind_json)
                .map_err(|_| db_message(operation, "legacy assertion kind is not valid JSON"))?;
            let payload_reference = serde_json::from_str::<Value>(&header.payload_reference_json)
                .map_err(|_| {
                db_message(operation, "legacy payload reference is not valid JSON")
            })?;
            let mut evidence = Vec::new();
            let mut evidence_cursor = -1_i64;
            loop {
                let mut evidence_rows = conn
                    .query(
                        "SELECT evidence.evidence_json, binding.ordinal
                         FROM memory_v2_assertion_evidence AS binding
                         JOIN memory_v2_evidence AS evidence
                           ON evidence.evidence_id = binding.evidence_id
                          AND evidence.fact_id = binding.fact_id
                          AND evidence.owner_kind = binding.owner_kind
                          AND evidence.project_id = binding.project_id
                         WHERE binding.assertion_id = ?1 AND binding.fact_id = ?2
                           AND binding.owner_kind = ?3 AND binding.project_id = ?4
                           AND binding.ordinal > ?5
                         ORDER BY binding.ordinal
                         LIMIT ?6",
                        params![
                            header.assertion_id.as_str(),
                            header.fact_id.as_str(),
                            header.owner_kind.as_str(),
                            header.project_id.as_str(),
                            evidence_cursor,
                            SCRUB_BATCH_SIZE
                        ],
                    )
                    .await
                    .map_err(|error| db_error(operation, error))?;
                let mut advanced = false;
                while let Some(row) = evidence_rows
                    .next()
                    .await
                    .map_err(|error| db_error(operation, error))?
                {
                    let encoded: String = row.get(0).map_err(|error| db_error(operation, error))?;
                    evidence_cursor = row.get(1).map_err(|error| db_error(operation, error))?;
                    evidence.push(serde_json::from_str::<Value>(&encoded).map_err(|_| {
                        db_message(operation, "legacy assertion evidence is not valid JSON")
                    })?);
                    advanced = true;
                }
                drop(evidence_rows);
                if !advanced {
                    break;
                }
            }
            let canonical = json!({
                "assertion_id": &header.assertion_id,
                "fact_id": &header.fact_id,
                "owner": owner,
                "kind": kind,
                "payload_reference": payload_reference,
                "evidence": evidence,
                "asserted_at": header.asserted_at,
                "actor_id": header.actor_id.as_deref(),
            });
            conn.execute(
                "UPDATE memory_v2_assertions SET assertion_header_json = ?1
                 WHERE assertion_id = ?2 AND fact_id = ?3
                   AND owner_kind = ?4 AND project_id = ?5",
                params![
                    json_text(&canonical)?,
                    header.assertion_id,
                    header.fact_id,
                    header.owner_kind,
                    header.project_id
                ],
            )
            .await
            .map_err(|error| db_error(operation, error))?;
        }
    }
    if trigger_dropped {
        create_schema(conn, operation).await
    } else {
        Ok(())
    }
}
