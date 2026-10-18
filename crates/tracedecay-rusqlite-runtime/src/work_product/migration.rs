//! One-time conversion of the pre-projection journal.
//!
//! Before the head projection a database held each owner's chain in
//! `work_product_events_v1` and its verified versions in
//! `work_product_graph_versions_v1`, and recovered every answer by replaying
//! the whole chain. The chain is the same chain now; what is new is the head,
//! the checkpoints and the scope registry that let every operation avoid
//! replaying it, and a rebuilt events table keyed to hold them.
//!
//! The conversion runs in bounded batches, each its own writer transaction:
//! fold the next [`BATCH_EVENTS`] legacy events onto the head, re-deriving every
//! digest, copy their rows, register their scope sets, write the head and any
//! checkpoint, and delete the legacy rows just copied. A crash resumes from the
//! head, so the step is idempotent, and memory is one graph and one page of
//! events. A chain that does not verify stops the conversion and leaves the
//! legacy rows in place. While a legacy table exists every Work product
//! operation answers `Busy` (see
//! [`require_current_journal`](super::journal::require_current_journal)).

use tracedecay_domain::WorkGraphVersionV1;

use super::journal::{
    CHECKPOINT_INTERVAL, LEGACY_EVENTS_TABLE, PUBLISHED_COLUMNS, WriteFault, fold_event,
    insert_checkpoint_row, insert_event_row, insert_version_row, integer, journal_entry,
    published_from_values, register_scope_set, write_head_row,
};
use super::{StoreFault, WorkProductJournalEntryV1, recovered_graph_digest};
use crate::exact_sql::{ExactSqlError, ExactSqlValue};
use crate::work::{
    RegisteredWorkWrite, WorkSqliteStorage, exact_sql_integer, exact_sql_statement, exact_sql_text,
    registered_work_query,
};

const LEGACY_VERSIONS_TABLE: &str = "work_product_graph_versions_v1";

/// Legacy events read per query, few enough that a query of the largest legal
/// events stays far under the exact-SQL result cap.
const LEGACY_QUERY_EVENTS: usize = 16;

/// Stored payload bytes a batch reads before it stops and continues.
const BATCH_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;

/// Legacy events folded per writer transaction.
const BATCH_EVENTS: usize = 256;

/// Why the journal could not be converted.
#[derive(Debug)]
pub enum WorkProductJournalMigrationError {
    /// The writer could not run the batch. The conversion resumes where it
    /// stopped on the next call.
    Storage(ExactSqlError),
    /// A legacy row did not verify. The label names the check and never
    /// carries row content. The legacy rows stay in place and the journal stays
    /// closed until the chain is repaired.
    Corrupt(&'static str),
}

impl std::fmt::Display for WorkProductJournalMigrationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => write!(formatter, "Work product journal conversion: {error}"),
            Self::Corrupt(what) => write!(
                formatter,
                "Work product journal conversion: legacy {what} did not verify"
            ),
        }
    }
}

impl std::error::Error for WorkProductJournalMigrationError {}

enum Batch {
    Progress,
    Done,
}

impl WorkSqliteStorage {
    /// Converts a pre-projection Work product journal in bounded batches, and
    /// returns once no legacy row remains. A database that never held one
    /// answers after a single lookup.
    pub async fn converge_product_journal(&self) -> Result<(), WorkProductJournalMigrationError> {
        loop {
            let outcome = self
                .transact(|job| migrate_batch(job.transaction))
                .await
                .map_err(WorkProductJournalMigrationError::Storage)?;
            match outcome {
                Ok(Batch::Done) => return Ok(()),
                Ok(Batch::Progress) => {}
                Err(StoreFault::Sql(error)) => {
                    return Err(WorkProductJournalMigrationError::Storage(error));
                }
                Err(StoreFault::Corrupt(what)) => {
                    return Err(WorkProductJournalMigrationError::Corrupt(what));
                }
            }
        }
    }
}

fn write_fault(fault: WriteFault) -> StoreFault {
    match fault {
        WriteFault::Conflict => StoreFault::Corrupt("journal rows"),
        WriteFault::Store(fault) => fault,
    }
}

fn drop_legacy_tables(transaction: &impl RegisteredWorkWrite) -> Result<(), StoreFault> {
    for table in [LEGACY_EVENTS_TABLE, LEGACY_VERSIONS_TABLE] {
        transaction.work_execute(exact_sql_statement(
            &format!("DROP TABLE IF EXISTS {table}"),
            Vec::new(),
        )?)?;
    }
    Ok(())
}

fn migrate_batch(transaction: &impl RegisteredWorkWrite) -> Result<Batch, StoreFault> {
    let exists = registered_work_query(
        transaction,
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        vec![ExactSqlValue::Text(LEGACY_EVENTS_TABLE.to_owned())],
    )?;
    if exists.rows.is_empty() {
        return Ok(Batch::Done);
    }
    let next = registered_work_query(
        transaction,
        &format!(
            "SELECT owner_brain_id, owner_profile_id FROM {LEGACY_EVENTS_TABLE}
             ORDER BY owner_brain_id, owner_profile_id, sequence
             LIMIT 1"
        ),
        Vec::new(),
    )?;
    let Some(row) = next.rows.first() else {
        drop_legacy_tables(transaction)?;
        return Ok(Batch::Done);
    };
    let corrupt = || StoreFault::Corrupt("owner");
    let owner = vec![
        ExactSqlValue::Text(
            exact_sql_text(&row.values, 0)
                .ok_or_else(corrupt)?
                .to_owned(),
        ),
        ExactSqlValue::Text(
            exact_sql_text(&row.values, 1)
                .ok_or_else(corrupt)?
                .to_owned(),
        ),
    ];

    // Where a previous batch left off: the head this owner already has.
    let head = registered_work_query(
        transaction,
        "SELECT head_version, head_graph FROM work_product_journal_head_v1
         WHERE owner_brain_id = ?1 AND owner_profile_id = ?2",
        owner.clone(),
    )?;
    let (prior_version, mut graph) = match head.rows.first() {
        None => (None, None),
        Some(row) => {
            let corrupt = || StoreFault::Corrupt("journal head");
            let version = exact_sql_integer(&row.values, 0)
                .and_then(|value| u64::try_from(value).ok())
                .and_then(|value| WorkGraphVersionV1::new(value).ok())
                .ok_or_else(corrupt)?;
            let graph = serde_json::from_str(exact_sql_text(&row.values, 1).ok_or_else(corrupt)?)
                .map_err(|_| corrupt())?;
            (Some(version), Some(graph))
        }
    };
    let mut at = prior_version.map_or(0, WorkGraphVersionV1::get);

    // The batch is read a few events at a time and stops at a byte budget, so
    // no legacy history makes a single result too big. A short batch is fine:
    // the next call continues from the head.
    let mut entries = Vec::new();
    let mut payload_bytes = 0;
    while entries.len() < BATCH_EVENTS && payload_bytes < BATCH_PAYLOAD_BYTES {
        let mut params = owner.clone();
        params.push(integer(
            entries
                .last()
                .map_or(at, |entry: &WorkProductJournalEntryV1| entry.sequence.get()),
        )?);
        let rows = registered_work_query(
            transaction,
            &format!(
                "SELECT sequence, event_payload FROM {LEGACY_EVENTS_TABLE}
                 WHERE owner_brain_id = ?1 AND owner_profile_id = ?2 AND sequence > ?3
                 ORDER BY sequence
                 LIMIT {}",
                LEGACY_QUERY_EVENTS.min(BATCH_EVENTS - entries.len())
            ),
            params,
        )?;
        if rows.rows.is_empty() {
            break;
        }
        for row in &rows.rows {
            payload_bytes += exact_sql_text(&row.values, 1).map_or(0, str::len);
            entries.push(journal_entry(&row.values)?);
        }
    }
    let Some(last) = entries.last() else {
        return Err(StoreFault::Corrupt("legacy rows behind the head"));
    };
    let last_sequence = last.sequence.get();
    let mut versions_params = owner.clone();
    versions_params.extend([integer(at)?, integer(last_sequence)?]);
    let versions = registered_work_query(
        transaction,
        &format!(
            "SELECT {PUBLISHED_COLUMNS} FROM {LEGACY_VERSIONS_TABLE}
             WHERE owner_brain_id = ?1 AND owner_profile_id = ?2
               AND graph_version > ?3 AND graph_version <= ?4
             ORDER BY graph_version"
        ),
        versions_params,
    )?;
    if versions.rows.len() != entries.len() {
        return Err(StoreFault::Corrupt("published versions"));
    }

    for (entry, version_row) in entries.iter().zip(&versions.rows) {
        let published = published_from_values(&version_row.values)?;
        if entry.sequence.get() != at + 1
            || published.graph_version.get() != entry.sequence.get()
            || published.event_sequence != entry.sequence
        {
            return Err(StoreFault::Corrupt("event chain"));
        }
        let folded =
            fold_event(graph.take(), &entry.event).ok_or(StoreFault::Corrupt("event chain"))?;
        // Re-deriving a digest digests the whole graph, so it is done where a
        // later read starts from the result: at each checkpoint and at the end
        // of the batch. Every other version row is copied as recorded, and a
        // read that rebuilds one proves it against that row, failing closed.
        let is_checkpoint = entry.sequence.get().is_multiple_of(CHECKPOINT_INTERVAL);
        let is_batch_end = at + 1 == last_sequence;
        if is_checkpoint || is_batch_end {
            let digest =
                recovered_graph_digest(&folded).ok_or(StoreFault::Corrupt("recovered graph"))?;
            if digest.as_str() != published.recovered_graph_digest {
                return Err(StoreFault::Corrupt("recovered graph"));
            }
        }
        insert_event_row(transaction, &owner, &entry.event).map_err(write_fault)?;
        register_scope_set(transaction, &owner, &entry.event).map_err(write_fault)?;
        insert_version_row(transaction, &owner, &published).map_err(write_fault)?;
        if is_checkpoint {
            let json =
                serde_json::to_string(&folded).map_err(|_| StoreFault::Corrupt("checkpoint"))?;
            insert_checkpoint_row(transaction, &owner, folded.version(), &json)
                .map_err(write_fault)?;
        }
        at = entry.sequence.get();
        graph = Some(folded);
    }
    let graph = graph.ok_or(StoreFault::Corrupt("event chain"))?;
    let json = serde_json::to_string(&graph).map_err(|_| StoreFault::Corrupt("head"))?;
    write_head_row(transaction, &owner, prior_version, graph.version(), &json)
        .map_err(write_fault)?;

    let mut delete_params = owner;
    delete_params.push(integer(at)?);
    for (table, column) in [
        (LEGACY_EVENTS_TABLE, "sequence"),
        (LEGACY_VERSIONS_TABLE, "graph_version"),
    ] {
        transaction.work_execute(exact_sql_statement(
            &format!(
                "DELETE FROM {table}
                 WHERE owner_brain_id = ?1 AND owner_profile_id = ?2 AND {column} <= ?3"
            ),
            delete_params.clone(),
        )?)?;
    }
    Ok(Batch::Progress)
}
