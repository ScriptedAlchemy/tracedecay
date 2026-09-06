//! Scoped operator recovery for a refused observation authority.
//!
//! Admission refuses a sessions store whose `observations` or
//! `source_cursor_advances` table carries a pre-release branch-local shape,
//! or whose retained rows were committed under a superseded native-source
//! scheme (see [`OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION`]) — both typed
//! `ResetRequired` naming [`OBSERVATION_AUTHORITY`]. Because the refusal
//! fires before any runtime can mount the store, recovery runs offline over a
//! plain connection while the operator holds the profile's exclusive
//! maintenance lease.
//!
//! The reset is scoped to exactly the refused authority: it drops the
//! observation-authority tables plus their pure projection derivations,
//! recreates every one of them empty at the canonical shape (through the same
//! DDL, index, and trigger authorities the schema installer uses — attach
//! only validates existing stores, it never reinstalls), clears the
//! `session_messages` projector output (classified `Recoverable`; the cleared
//! evidence re-derives by re-ingesting provider transcripts), and preserves
//! everything else in the store — transcripts, LCM content, configuration,
//! registry, workflow, and the session-temporal state that is not a
//! projection of observations. The session-temporal projection *is* one, so it
//! resets with the stream it projects (see
//! [`OBSERVATION_DERIVED_TEMPORAL_DELETES`]) rather than being orphaned or
//! left advertising coverage of rows that no longer exist.
//!
//! The derived usage (`observation_provider_usage`) and the admission cursors
//! (`source_cursors`, `source_cursor_advances`) always reset together: leaving
//! either behind is what would let the rebuilt authority double-count or skip
//! the native events it re-reads.
//!
//! Two invariants bound the deletion. Rows the reset preserves must never be
//! left pointing at rows it removes: [`PRESERVED_DEPENDENT_TABLES`] refuses
//! atomically for the one such dependency that has no safe scoped treatment,
//! and `PRAGMA foreign_key_check` proves the rest before the transaction
//! commits (the reset suspends per-statement enforcement for its own
//! intermediate drop states, and `PRAGMA integrity_check` does not cover
//! foreign keys). And every step — trigger removal, deletion, temporal
//! invalidation, trigger restoration, scheme enrollment — runs inside that one
//! transaction, so a failure anywhere leaves the store exactly as refused.

use std::collections::BTreeSet;

use tracedecay_domain::errors::TraceDecayError;

use super::schema::{
    OBSERVATION_AUTHORITY, OBSERVATION_AUTHORITY_SCHEMA_SQL, OBSERVATION_CANONICAL_COLUMNS,
    OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION, OBSERVATION_SCHEMA_MIGRATION,
    SOURCE_CURSOR_ADVANCES_CANONICAL_COLUMNS,
};
use crate::observation_projection::{
    OBSERVATION_PROJECTION_BINDING_TRIGGERS_SQL, OBSERVATION_PROJECTION_PERFORMANCE_INDEX_SQL,
    OBSERVATION_PROJECTION_SCHEMA_SQL,
};
use crate::schema_contract::{
    invariant_trigger_names_for_tables, invariant_trigger_sql_for_tables,
};

const OPERATION: &str = "reset refused observation authority";

/// Tables owned by `ensure_observation_schema`; the next admission recreates
/// every one of them empty at the canonical shape.
const OBSERVATION_AUTHORITY_TABLES: &[&str] = &[
    "observations",
    "sanitization_receipts",
    "source_cursors",
    "source_cursor_advances",
    "observation_admission_refusals",
    "projection_queue",
    "remote_writer_fences",
    "remote_observation_events",
    "observation_retrieval_anchors",
    "observation_repository_provenance",
];

/// Pure derivations of the observation stream owned by
/// `ensure_observation_projection_schema`. They are outputs of the projection
/// and rebuild machinery over `observations`, so they reset with it; the next
/// admission recreates them empty.
const OBSERVATION_PROJECTION_TABLES: &[&str] = &[
    "observation_projection_provenance",
    "observation_projection_checkpoints",
    "observation_projection_aliases",
    "observation_projection_dispositions",
    "observation_workflow_facts",
    "observation_provider_usage",
    "observation_projection_rebuilds",
    "observation_projection_rebuild_provider_usage",
    "observation_projection_rebuild_aliases",
    "observation_projection_rebuild_sessions",
    "observation_projection_rebuild_messages",
    "observation_projection_rebuild_provenance",
    "observation_projection_rebuild_dispositions",
    "observation_projection_rebuild_workflow_facts",
];

/// Derived-temporal tables whose invariant triggers would abort the deletes
/// below. Their triggers are dropped and reinstalled from the same canonical
/// authority inside the reset transaction, the way the observation tables are
/// dropped and recreated: immutability guards ordinary writers, not the
/// authority rebuild itself. Runtime immutability is unchanged — the store
/// leaves this function carrying the same trigger set it arrived with.
///
/// `session_occurrences` is deliberately absent: its triggers keep the
/// external-content `session_occurrences_fts` index in step with the deletes,
/// so they must stay installed while the rows go.
const IMMUTABLE_DERIVED_TEMPORAL_TABLES: &[&str] = &[
    "session_refresh_batch_bindings",
    "session_refresh_bindings",
    "session_refresh_progress",
    "session_refresh_receipts",
    "session_refresh_operations",
    "session_temporal_projection_receipts",
    "session_temporal_generations",
    "session_temporal_observation_effects",
];

/// The whole session-temporal projection of the observation stream, in
/// dependency order (foreign-key enforcement is suspended, so the cascades
/// these rows normally ride do not fire; each table is named explicitly and
/// `PRAGMA foreign_key_check` proves nothing was missed).
///
/// Every generation is projector output over `observations`: its occurrences
/// and their anchors, turn/thread/agent grouping, assertions and their
/// supersession, current entities, span and burst evidence, summary
/// availability, and the projection receipts that certify exactly those
/// counts and digests. Deleting occurrences while preserving the generation
/// would leave those receipts certifying coverage that no longer exists and
/// `validate_final_projection_receipt` recomputing a disagreement — and a
/// retained active frontier of 100 would exclude the re-ingested effects
/// 1..=100 from `pending_session_temporal_refresh_page_result`, suppressing
/// the rebuild it is supposed to trigger. So the generation is invalidated
/// outright, together with the relation receipts and refresh bindings that
/// authorize serving or resuming against it; the operator-visible refresh
/// history in `session_refresh_operations` goes with them because a `running`
/// row would keep the rebuilt stream out of discovery forever.
///
/// Everything session-temporal that is not projector output stays: summary
/// nodes and their FTS index, external payload manifests (see
/// [`PRESERVED_DEPENDENT_TABLES`]), cursor keys, and the retrieval anchors the
/// rebuilt projection re-attaches to.
const OBSERVATION_DERIVED_TEMPORAL_DELETES: &[&str] = &[
    "DELETE FROM session_refresh_batch_bindings",
    "DELETE FROM session_refresh_bindings",
    "DELETE FROM session_refresh_progress",
    "DELETE FROM session_refresh_receipts",
    "DELETE FROM session_refresh_operations",
    "DELETE FROM session_derived_evidence_members",
    "DELETE FROM session_derived_evidence",
    "DELETE FROM session_current_entities",
    "DELETE FROM session_assertion_supersession",
    "DELETE FROM session_assertions",
    "DELETE FROM session_turn_members",
    "DELETE FROM session_summary_availability",
    "DELETE FROM session_occurrences",
    "DELETE FROM session_turns",
    "DELETE FROM session_threads",
    "DELETE FROM session_agents",
    "DELETE FROM session_temporal_projection_receipts",
    "DELETE FROM session_relation_effect_journal",
    "DELETE FROM session_relation_receipts",
    "DELETE FROM session_temporal_generations",
    "DELETE FROM session_temporal_observation_effects",
];

/// Preserved rows that would be orphaned by the reset, with the authority
/// they would be orphaned from.
///
/// `session_external_payload_manifests` is LCM publication metadata — durable,
/// immutable by trigger, and outside every deletion above — whose `receipt_id`
/// names a `sanitization_receipts` row the reset drops with the observation
/// authority. There is no scoped treatment that keeps both coherent: the
/// manifests are not reconstructible from the transcripts, and deleting
/// external-payload metadata to make the reset succeed would destroy exactly
/// the evidence a scoped reset promises to preserve. Such a store refuses
/// atomically and needs an LCM-side remediation first.
const PRESERVED_DEPENDENT_TABLES: &[(&str, &str)] = &[(
    "session_external_payload_manifests",
    "sanitization_receipts",
)];

/// Outcome of one completed scoped reset.
#[derive(Debug)]
pub struct ObservationAuthorityResetV1 {
    /// Tables dropped and recreated empty at the canonical shape.
    pub reset_tables: Vec<String>,
    /// `session_messages` projector-output rows cleared (`Recoverable`; the
    /// external-content FTS index is synchronized by its delete trigger).
    pub cleared_session_message_rows: u64,
    /// Session-temporal projection rows cleared because they derive from the
    /// reset observation stream (see [`OBSERVATION_DERIVED_TEMPORAL_DELETES`]).
    pub cleared_derived_temporal_rows: u64,
}

fn reset_storage(error: rusqlite::Error) -> TraceDecayError {
    TraceDecayError::Database {
        operation: OPERATION.to_string(),
        message: error.to_string(),
    }
}

fn table_exists(conn: &rusqlite::Connection, table: &str) -> Result<bool, TraceDecayError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get::<_, bool>(0),
    )
    .map_err(reset_storage)
}

fn migration_recorded(
    conn: &rusqlite::Connection,
    migration: &str,
) -> Result<bool, TraceDecayError> {
    if !table_exists(conn, "global_schema_migrations")? {
        return Ok(false);
    }
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM global_schema_migrations WHERE migration = ?1)",
        [migration],
        |row| row.get::<_, bool>(0),
    )
    .map_err(reset_storage)
}

fn table_columns(
    conn: &rusqlite::Connection,
    table: &str,
) -> Result<BTreeSet<String>, TraceDecayError> {
    let mut statement = conn
        .prepare("SELECT name FROM pragma_table_xinfo(?1)")
        .map_err(reset_storage)?;
    let columns = statement
        .query_map([table], |row| row.get::<_, String>(0))
        .map_err(reset_storage)?
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(reset_storage)?;
    Ok(columns)
}

fn row_count(conn: &rusqlite::Connection, table: &str) -> Result<u64, TraceDecayError> {
    let count = conn
        .query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(reset_storage)?;
    u64::try_from(count).map_err(|_| TraceDecayError::Database {
        operation: OPERATION.to_string(),
        message: format!("{table} row count was negative"),
    })
}

fn canonical(columns: &[&str]) -> BTreeSet<String> {
    columns.iter().map(|column| (*column).to_string()).collect()
}

/// Whether the store currently carries a shape the observation authority
/// refuses at admission. Mirrors the refusal predicates in `super::schema`
/// through the shared canonical column sets.
fn observation_authority_refused(conn: &rusqlite::Connection) -> Result<bool, TraceDecayError> {
    if table_exists(conn, "observations")? {
        if !migration_recorded(conn, OBSERVATION_SCHEMA_MIGRATION)?
            || table_columns(conn, "observations")? != canonical(OBSERVATION_CANONICAL_COLUMNS)
        {
            return Ok(true);
        }
        // Content identity, not shape: rows written under a superseded
        // native-source scheme refuse admission because re-offering them
        // would double-count. Mirrors the schema-side predicate.
        let populated = row_count(conn, "observations")? > 0
            || (table_exists(conn, "source_cursors")? && row_count(conn, "source_cursors")? > 0);
        if populated && !migration_recorded(conn, OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION)? {
            return Ok(true);
        }
    }
    if table_exists(conn, "source_cursor_advances")?
        && table_columns(conn, "source_cursor_advances")?
            != canonical(SOURCE_CURSOR_ADVANCES_CANONICAL_COLUMNS)
    {
        return Ok(true);
    }
    Ok(false)
}

/// Whether any preserved row is left pointing at a row that no longer exists.
///
/// The reset suspends per-statement foreign-key enforcement for its own
/// intermediate drop states, so this is what proves the committed store is
/// referentially coherent. `PRAGMA integrity_check` does not look at foreign
/// keys, and `foreign_key_check` reports violations whether or not
/// enforcement is on.
fn require_referential_integrity(conn: &rusqlite::Connection) -> Result<(), TraceDecayError> {
    let mut statement = conn
        .prepare("PRAGMA foreign_key_check")
        .map_err(reset_storage)?;
    let mut rows = statement.query([]).map_err(reset_storage)?;
    let Some(row) = rows.next().map_err(reset_storage)? else {
        return Ok(());
    };
    let table = row.get::<_, String>(0).map_err(reset_storage)?;
    let parent = row.get::<_, String>(2).map_err(reset_storage)?;
    Err(TraceDecayError::Config {
        message: format!(
            "a scoped {OBSERVATION_AUTHORITY} reset would leave {table} row(s) referencing \
             missing {parent} row(s); this store needs a remediation for that dependency \
             first, so nothing was reset"
        ),
    })
}

/// Resets exactly the refused observation authority in one transaction.
///
/// Fails closed, mutating nothing, when the authority is not actually in a
/// refused shape (protecting healthy data from an accidental reset), when a
/// preserved dependency has no safe scoped treatment, or when the resulting
/// store would not be referentially coherent.
pub fn reset_refused_observation_authority(
    conn: &mut rusqlite::Connection,
) -> Result<ObservationAuthorityResetV1, TraceDecayError> {
    // The reset drops the refused tables together with every table that
    // references them, so per-statement foreign-key enforcement would only
    // reject the intermediate drop states of an exclusive maintenance
    // connection; `require_referential_integrity` proves the committed result
    // instead. The setting belongs to the connection, not to this operation,
    // so it is restored before the caller reuses it — on both paths.
    let enforced_foreign_keys = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, bool>(0))
        .map_err(reset_storage)?;
    conn.pragma_update(None, "foreign_keys", false)
        .map_err(reset_storage)?;
    let outcome = reset_within_maintenance_transaction(conn);
    let restored = conn
        .pragma_update(None, "foreign_keys", enforced_foreign_keys)
        .map_err(reset_storage);
    let report = outcome?;
    restored?;
    Ok(report)
}

fn reset_within_maintenance_transaction(
    conn: &mut rusqlite::Connection,
) -> Result<ObservationAuthorityResetV1, TraceDecayError> {
    let transaction = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(reset_storage)?;
    if !observation_authority_refused(&transaction)? {
        return Err(TraceDecayError::Config {
            message: format!(
                "the {OBSERVATION_AUTHORITY} authority in this store is not in a refused state; \
                 nothing was reset"
            ),
        });
    }
    for (table, parent) in PRESERVED_DEPENDENT_TABLES {
        if !table_exists(&transaction, table)? {
            continue;
        }
        let rows = row_count(&transaction, table)?;
        if rows > 0 {
            return Err(TraceDecayError::Config {
                message: format!(
                    "a scoped {OBSERVATION_AUTHORITY} reset would orphan {rows} preserved row(s) \
                     in {table} from the {parent} the reset recreates empty; this store needs a \
                     remediation for that dependency first, so nothing was reset"
                ),
            });
        }
    }
    // The session-temporal projection derives from the observation stream, so
    // it resets with it rather than being orphaned or refused over.
    let mut cleared_derived_temporal_rows = 0u64;
    for name in invariant_trigger_names_for_tables(IMMUTABLE_DERIVED_TEMPORAL_TABLES) {
        transaction
            .execute_batch(&format!("DROP TRIGGER IF EXISTS \"{name}\""))
            .map_err(reset_storage)?;
    }
    for statement in OBSERVATION_DERIVED_TEMPORAL_DELETES {
        let table = statement
            .strip_prefix("DELETE FROM ")
            .and_then(|rest| rest.split_whitespace().next())
            .expect("each derived-temporal statement names its table");
        if !table_exists(&transaction, table)? {
            continue;
        }
        let deleted = transaction.execute(statement, []).map_err(reset_storage)?;
        cleared_derived_temporal_rows =
            cleared_derived_temporal_rows.saturating_add(u64::try_from(deleted).map_err(|_| {
                TraceDecayError::Database {
                    operation: OPERATION.to_string(),
                    message: format!("{table} delete count overflowed"),
                }
            })?);
    }
    for sql in invariant_trigger_sql_for_tables(IMMUTABLE_DERIVED_TEMPORAL_TABLES) {
        transaction.execute_batch(sql).map_err(reset_storage)?;
    }

    // Clear the recoverable projector output before dropping the projection
    // tables: the audit-invalidation trigger on `session_messages` reads
    // `observation_projection_provenance` and must still resolve while the
    // deletes run.
    let cleared_session_message_rows = if table_exists(&transaction, "session_messages")? {
        u64::try_from(
            transaction
                .execute("DELETE FROM session_messages", [])
                .map_err(reset_storage)?,
        )
        .map_err(|_| TraceDecayError::Database {
            operation: OPERATION.to_string(),
            message: "session_messages delete count overflowed".to_string(),
        })?
    } else {
        0
    };
    let mut reset_tables = Vec::new();
    for table in OBSERVATION_AUTHORITY_TABLES
        .iter()
        .chain(OBSERVATION_PROJECTION_TABLES)
    {
        if table_exists(&transaction, table)? {
            transaction
                .execute_batch(&format!("DROP TABLE \"{table}\""))
                .map_err(reset_storage)?;
        }
        reset_tables.push((*table).to_string());
    }
    // Recreate the authority empty at the canonical shape through the same
    // DDL, index, and invariant-trigger authorities the schema installer
    // uses. Attach only validates an existing store — it never reinstalls —
    // so the reset itself must leave the store at the final contract.
    transaction
        .execute_batch(OBSERVATION_AUTHORITY_SCHEMA_SQL)
        .map_err(reset_storage)?;
    transaction
        .execute_batch(OBSERVATION_PROJECTION_SCHEMA_SQL)
        .map_err(reset_storage)?;
    transaction
        .execute_batch(OBSERVATION_PROJECTION_BINDING_TRIGGERS_SQL)
        .map_err(reset_storage)?;
    for sql in OBSERVATION_PROJECTION_PERFORMANCE_INDEX_SQL {
        transaction.execute_batch(sql).map_err(reset_storage)?;
    }
    let reset_table_names = reset_tables.iter().map(String::as_str).collect::<Vec<_>>();
    for sql in invariant_trigger_sql_for_tables(&reset_table_names) {
        transaction.execute_batch(sql).map_err(reset_storage)?;
    }
    for migration in [
        OBSERVATION_SCHEMA_MIGRATION,
        OBSERVATION_NATIVE_SOURCE_SCHEME_MIGRATION,
    ] {
        transaction
            .execute(
                "INSERT OR IGNORE INTO global_schema_migrations(migration) VALUES (?1)",
                [migration],
            )
            .map_err(reset_storage)?;
    }
    // The observation-authority audit checkpoint attests to rows that no
    // longer exist; clear it so convergence re-audits the recreated authority
    // from the start.
    if table_exists(&transaction, "authority_audit_checkpoints")? {
        transaction
            .execute(
                "DELETE FROM authority_audit_checkpoints WHERE audit_name = 'observation-authority'",
                [],
            )
            .map_err(reset_storage)?;
    }
    require_referential_integrity(&transaction)?;
    transaction.commit().map_err(reset_storage)?;
    Ok(ObservationAuthorityResetV1 {
        reset_tables,
        cleared_session_message_rows,
        cleared_derived_temporal_rows,
    })
}

#[cfg(test)]
mod tests;
