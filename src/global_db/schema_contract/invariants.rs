use libsql::{Connection, params};
use tracedecay_store::CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION;

use super::super::{global_db_operation_error, global_db_operation_message};

mod audit;
mod repair;
mod rows;
mod triggers;

use audit::{
    AuditCheckpoint, AuditProgress, audit_checkpoint_is_plausible, ensure_audit_checkpoint_schema,
    read_audit_checkpoint, validate_projection_authority_suffix, write_audit_checkpoint,
};
use repair::{
    repair_committed_source_cursors, repair_projection_frontier,
    validate_observation_cursor_coverage,
};
use rows::{
    authority_violation, query_has_rows, validate_mutable_invariant_rows,
    validate_observation_authority_rows, validate_receipt_authority_rows,
    validate_source_cursor_authority_rows,
};
pub(super) use triggers::{INVARIANTS, Trigger};
use triggers::{replace_trigger, trigger_contracts_intact};
pub(in crate::global_db) use triggers::{
    restore_immutability_after_canonical_repair, suspend_immutability_for_canonical_repair,
};

const OPERATION: &str = "ensure global database authority invariants";

async fn projection_checkpoint(conn: &Connection) -> crate::errors::Result<i64> {
    let mut rows = conn
        .query(
            "SELECT COALESCE((
                SELECT last_sequence FROM observation_projection_checkpoints
                WHERE projector_version = ?1
             ), 0)",
            params![CLAUDE_SESSION_MESSAGE_PROJECTOR_VERSION],
        )
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?;
    rows.next()
        .await
        .map_err(|error| global_db_operation_error(OPERATION, error))?
        .ok_or_else(|| authority_violation("projection checkpoint query returned no row"))?
        .get(0)
        .map_err(|error| global_db_operation_error(OPERATION, error))
}

fn normalize_trigger_sql(sql: &str) -> String {
    sql.trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

pub(in crate::global_db) async fn ensure_authority_invariants(
    conn: &Connection,
) -> crate::errors::Result<()> {
    ensure_audit_checkpoint_schema(conn).await?;
    let checkpoint = if trigger_contracts_intact(conn).await? {
        match read_audit_checkpoint(conn).await? {
            Some(checkpoint) if audit_checkpoint_is_plausible(conn, checkpoint).await? => {
                Some(checkpoint)
            }
            _ => None,
        }
    } else {
        None
    };
    for invariant in INVARIANTS {
        for trigger in invariant.triggers {
            replace_trigger(conn, trigger).await?;
        }
    }
    let exhaustive = checkpoint.is_none();
    let checkpoint = checkpoint.unwrap_or_default();
    let (receipt_rowid, receipts_audited) =
        validate_receipt_authority_rows(conn, checkpoint.receipt_rowid).await?;
    let (observation_sequence, observations_audited) =
        validate_observation_authority_rows(conn, checkpoint.observation_sequence).await?;
    validate_source_cursor_authority_rows(conn).await?;
    repair_committed_source_cursors(conn, checkpoint.observation_sequence).await?;
    validate_observation_cursor_coverage(conn, checkpoint.observation_sequence).await?;

    repair_projection_frontier(conn, checkpoint.projection_checkpoint).await?;
    let (mut checkpoint, provenance_audited, dispositions_audited, aliases_audited) =
        validate_projection_authority_suffix(
            conn,
            AuditCheckpoint {
                receipt_rowid,
                observation_sequence,
                ..checkpoint
            },
        )
        .await?;
    if exhaustive {
        validate_invariant_rows(conn).await?;
    } else {
        validate_mutable_invariant_rows(conn).await?;
    }
    checkpoint.bounded_passes_since_exhaustive = if exhaustive {
        0
    } else {
        checkpoint.bounded_passes_since_exhaustive.saturating_add(1)
    };
    write_audit_checkpoint(
        conn,
        AuditProgress {
            checkpoint,
            receipts_audited,
            observations_audited,
            provenance_audited,
            dispositions_audited,
            aliases_audited,
        },
    )
    .await
}

pub(super) async fn validate_invariant_rows(conn: &Connection) -> crate::errors::Result<()> {
    for invariant in INVARIANTS {
        if let Some(query) = invariant.audit_query
            && query_has_rows(conn, query).await?
        {
            return Err(global_db_operation_message(OPERATION, invariant.violation));
        }
    }
    Ok(())
}

pub(in crate::global_db) async fn validate_authority_rows_exhaustive(
    conn: &Connection,
) -> crate::errors::Result<()> {
    validate_receipt_authority_rows(conn, 0).await?;
    validate_observation_authority_rows(conn, 0).await?;
    validate_source_cursor_authority_rows(conn).await?;
    validate_observation_cursor_coverage(conn, 0).await?;
    validate_projection_authority_suffix(conn, AuditCheckpoint::default()).await?;
    validate_invariant_rows(conn).await
}
