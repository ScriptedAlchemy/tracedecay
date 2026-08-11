//! Writing and reading one fact.
//!
//! The executor owns the transaction shape; the siblings own the pieces it
//! composes — [`assertion`] the assertion rows and their replay comparison,
//! [`writes`] the fact/anchor/projection rows around them, and [`reads`] the
//! two read operations.

use std::collections::HashSet;

use rusqlite::{Savepoint, Transaction, params, params_from_iter};
use tracedecay_domain::{
    FactCurationActionV1, FactLineageEventKindV1, FactOwnerV1, RetrievalAnchorId,
};
use tracedecay_store::{FactReadOperationV1, FactReadResultV1, FactWriteBatch};

use super::support::{encode, invalid};

/// The largest `anchor_id IN (...)` batch one referenced-anchor availability
/// probe binds, kept clear of SQLite's default variable ceiling.
const REFERENCED_ANCHOR_BATCH: usize = 500;

/// Leaves ample room below SQLite's variable limit for the three owner keys.
const NORMALIZED_TAG_EVIDENCE_BATCH: usize = 250;

mod assertion;
mod reads;
mod writes;

use assertion::insert_assertion;
use reads::{read_current, read_lineage};
use writes::{current_last_event, ensure_fact, insert_anchor, publish_projection};

#[derive(Clone, Default)]
pub struct FactExecutor;

impl FactExecutor {
    pub fn execute_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        batch: &FactWriteBatch,
    ) -> rusqlite::Result<()> {
        let owner = OwnerColumns::new(batch.owner())?;
        let actual_last = current_last_event(savepoint, &owner, batch.fact_id())?;
        if actual_last.as_ref() != batch.expected_last_event_id() {
            return Err(invalid("fact lineage last-event conflict"));
        }

        require_normalized_tag_evidence_available(savepoint, &owner, batch)?;
        ensure_fact(savepoint, &owner, batch)?;
        require_referenced_anchors_available(savepoint, &owner, batch.referenced_anchor_ids())?;
        for anchor in batch.new_anchors() {
            insert_anchor(savepoint, &owner, anchor)?;
        }
        if let Some(assertion) = batch.assertion() {
            insert_assertion(savepoint, &owner, assertion)?;
        }
        for event in batch.events() {
            savepoint.execute(
                "INSERT INTO memory_v2_lineage_events (
                    event_id, fact_id, owner_kind, project_id,
                    event_json, occurred_at, recorded_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    event.event_id().as_str(),
                    event.fact_id().as_str(),
                    owner.kind,
                    owner.project_id,
                    encode(event)?,
                    event.occurred_at().0,
                    event.occurred_at().0,
                ],
            )?;
        }
        publish_projection(savepoint, &owner, batch)
    }

    pub fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        operation: &FactReadOperationV1,
    ) -> rusqlite::Result<FactReadResultV1> {
        match operation {
            FactReadOperationV1::Current(query) => {
                read_current(snapshot, query).map(|fact| FactReadResultV1::Current(Box::new(fact)))
            }
            FactReadOperationV1::Lineage(query) => {
                read_lineage(snapshot, query).map(FactReadResultV1::Lineage)
            }
        }
    }
}

/// Confirms normalized-tag provenance points at facts already owned here.
///
/// This runs before any mutation. In particular, self-evidence is available
/// only for an existing target fact; a new fact cannot make its own evidence
/// true by being inserted later in the same write.
fn require_normalized_tag_evidence_available(
    connection: &rusqlite::Connection,
    owner: &OwnerColumns,
    batch: &FactWriteBatch,
) -> rusqlite::Result<()> {
    let evidence_fact_ids = batch
        .events()
        .iter()
        .filter_map(|event| match event.kind() {
            FactLineageEventKindV1::Curated {
                action:
                    FactCurationActionV1::TagsNormalized {
                        evidence_fact_ids, ..
                    },
                ..
            } => Some(evidence_fact_ids.as_slice()),
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>();
    if evidence_fact_ids.is_empty() {
        return Ok(());
    }

    let mut present: HashSet<String> = HashSet::new();
    for chunk in evidence_fact_ids.chunks(NORMALIZED_TAG_EVIDENCE_BATCH) {
        let placeholders = (1..=chunk.len())
            .map(|index| format!("?{}", index + 3))
            .collect::<Vec<_>>()
            .join(", ");
        let mut statement = connection.prepare(&format!(
            "SELECT fact_id FROM memory_v2_facts
             WHERE owner_kind = ?1 AND project_id = ?2 AND owner_json = ?3
               AND fact_id IN ({placeholders})",
        ))?;
        let mut bindings: Vec<&str> = Vec::with_capacity(chunk.len() + 3);
        bindings.extend([owner.kind, owner.project_id.as_str(), owner.json.as_str()]);
        bindings.extend(chunk.iter().map(|fact_id| fact_id.as_str()));
        let rows =
            statement.query_map(params_from_iter(bindings), |row| row.get::<_, String>(0))?;
        for row in rows {
            present.insert(row?);
        }
    }
    for fact_id in evidence_fact_ids {
        if !present.contains(fact_id.as_str()) {
            return Err(invalid("normalized tag evidence fact is unavailable"));
        }
    }
    Ok(())
}

/// Confirms every anchor a fact references is present under the fact's owner.
///
/// This replaces a `SELECT EXISTS` per referenced anchor with one batched
/// `anchor_id IN (...)` load per chunk: the referenced set is proven available
/// exactly when every id comes back present, which is the same "all must exist"
/// contract the per-anchor loop enforced, down to the error it raises.
fn require_referenced_anchors_available(
    connection: &rusqlite::Connection,
    owner: &OwnerColumns,
    anchor_ids: &[RetrievalAnchorId],
) -> rusqlite::Result<()> {
    if anchor_ids.is_empty() {
        return Ok(());
    }
    let mut present: HashSet<String> = HashSet::new();
    for chunk in anchor_ids.chunks(REFERENCED_ANCHOR_BATCH) {
        let placeholders = (1..=chunk.len())
            .map(|index| format!("?{}", index + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let mut statement = connection.prepare(&format!(
            "SELECT anchor_id FROM retrieval_anchors
             WHERE owner_json = ?1 AND anchor_id IN ({placeholders})",
        ))?;
        let mut bindings: Vec<&str> = Vec::with_capacity(chunk.len() + 1);
        bindings.push(owner.json.as_str());
        bindings.extend(chunk.iter().map(RetrievalAnchorId::as_str));
        let rows =
            statement.query_map(params_from_iter(bindings), |row| row.get::<_, String>(0))?;
        for row in rows {
            present.insert(row?);
        }
    }
    for anchor_id in anchor_ids {
        if !present.contains(anchor_id.as_str()) {
            return Err(invalid("fact references an unavailable retrieval anchor"));
        }
    }
    Ok(())
}

/// The three columns every fact row is filed under, derived once per operation.
pub(super) struct OwnerColumns {
    pub(super) kind: &'static str,
    pub(super) project_id: String,
    pub(super) json: String,
}

impl OwnerColumns {
    pub(super) fn new(owner: &FactOwnerV1) -> rusqlite::Result<Self> {
        let (kind, project_id) = match owner {
            FactOwnerV1::Profile => ("profile", String::new()),
            FactOwnerV1::Project { project_id } => ("project", project_id.as_str().to_owned()),
        };
        Ok(Self {
            kind,
            project_id,
            json: encode(owner)?,
        })
    }
}

#[cfg(test)]
mod tests;
