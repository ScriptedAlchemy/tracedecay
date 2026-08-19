use rusqlite::{OptionalExtension, Savepoint, Transaction, params};
use tracedecay_domain::{
    CodeGenerationId, DiagnosticEvidenceClassV1, DiagnosticProducerKindV1, DiagnosticProvenanceV1,
    DiagnosticRecordStateV1, DiagnosticSeverityV1, FileOccurrenceId, GenerationDiagnosticV1,
    RetrievalAnchorId, SourceSpan, UtcMicros,
};
use tracedecay_store::{
    DIAGNOSTIC_STATE_CLEARED, DIAGNOSTIC_STATE_CURRENT, DIAGNOSTIC_STATE_SUPERSEDED,
    DiagnosticGenerationSupersessionV1, DiagnosticReadOperationV1, DiagnosticReadResultV1,
    DiagnosticRecordStateKindV1, SanitizedCleanDiagnosticSnapshotV1,
    diagnostic_evidence_class_name, diagnostic_producer_kind_name, diagnostic_severity_name,
    diagnostic_state_columns, parse_diagnostic_evidence_class, parse_diagnostic_producer_kind,
    parse_diagnostic_severity,
};

use super::support::{conversion, invalid, u64_to_i64};

// The stored column text is owned by `tracedecay_store::diagnostics::codec` so
// this executor and the root `DiagnosticsStore` cannot drift apart across a
// migration. These aliases keep the SQL below readable.
const CURRENT: &str = DIAGNOSTIC_STATE_CURRENT;
const SUPERSEDED: &str = DIAGNOSTIC_STATE_SUPERSEDED;
const CLEARED: &str = DIAGNOSTIC_STATE_CLEARED;

#[derive(Clone, Default)]
pub struct DiagnosticExecutor;

impl DiagnosticExecutor {
    pub fn execute_write(
        &mut self,
        savepoint: &Savepoint<'_>,
        snapshot: &SanitizedCleanDiagnosticSnapshotV1,
    ) -> rusqlite::Result<()> {
        let generation = snapshot.generation_id();
        if let Some(state) = savepoint
            .query_row(
                "SELECT record_state FROM diagnostic_generation_publications
                 WHERE generation_id = ?1",
                [generation.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            if state != CURRENT {
                return Err(invalid(
                    "historical diagnostic generation cannot be republished",
                ));
            }
            let existing = read_records(
                savepoint,
                "WHERE generation_id = ?1 AND record_state = 'current'
                 ORDER BY diagnostic_anchor",
                [generation.as_str()],
            )?;
            return if existing == snapshot.records() {
                Ok(())
            } else {
                Err(invalid(
                    "diagnostic generation conflicts with immutable publication",
                ))
            };
        }

        savepoint.execute(
            "UPDATE generation_diagnostics
             SET record_state = ?1, state_generation = ?2
             WHERE record_state = ?3 AND generation_id != ?2",
            params![CLEARED, generation.as_str(), CURRENT],
        )?;
        savepoint.execute(
            "UPDATE diagnostic_generation_publications
             SET record_state = ?1, state_generation = ?2
             WHERE record_state = ?3 AND generation_id != ?2",
            params![CLEARED, generation.as_str(), CURRENT],
        )?;
        for record in snapshot.records() {
            insert_record(savepoint, record)?;
        }
        let published_at = snapshot
            .records()
            .iter()
            .map(|record| record.collected_at.0)
            .max()
            .unwrap_or(0);
        savepoint.execute(
            "INSERT INTO diagnostic_generation_publications (
                generation_id, record_state, state_generation, published_at
             ) VALUES (?1, ?2, NULL, ?3)",
            params![generation.as_str(), CURRENT, published_at],
        )?;
        Ok(())
    }

    /// Transitions every current record of `request.prior_generation()` into
    /// the superseded state, back-pointing at the successor generation, and
    /// moves the prior generation's publication row with it.
    ///
    /// This mirrors `DiagnosticsStore::supersede_generation` exactly: the same
    /// two `UPDATE`s over the same predicates, the same `state_generation`
    /// back-pointer, and the same refusal to let a generation supersede itself
    /// (enforced by [`DiagnosticGenerationSupersessionV1`] before admission,
    /// and re-checked here so a hand-built request cannot bypass it). Returns
    /// the number of diagnostic rows transitioned.
    ///
    /// Clearing (the publication path above) and supersession are distinct
    /// lanes and must stay so: clearing marks records a newer clean generation
    /// replaced wholesale, while supersession preserves a walkable chain from
    /// a prior finding to its logical successor.
    pub fn execute_supersession(
        &mut self,
        savepoint: &Savepoint<'_>,
        request: &DiagnosticGenerationSupersessionV1,
    ) -> rusqlite::Result<u64> {
        request.validate().map_err(invalid)?;
        let prior = request.prior_generation().as_str();
        let successor = request.successor_generation().as_str();
        let transitioned = savepoint.execute(
            "UPDATE generation_diagnostics
             SET record_state = ?1, state_generation = ?2
             WHERE record_state = ?3 AND generation_id = ?4",
            params![SUPERSEDED, successor, CURRENT, prior],
        )?;
        savepoint.execute(
            "UPDATE diagnostic_generation_publications
             SET record_state = ?1, state_generation = ?2
             WHERE record_state = ?3 AND generation_id = ?4",
            params![SUPERSEDED, successor, CURRENT, prior],
        )?;
        Ok(transitioned as u64)
    }

    pub fn execute_read(
        &mut self,
        snapshot: &Transaction<'_>,
        operation: &DiagnosticReadOperationV1,
    ) -> rusqlite::Result<DiagnosticReadResultV1> {
        match operation {
            DiagnosticReadOperationV1::CurrentGeneration => {
                let generation = snapshot
                    .query_row(
                        "SELECT generation_id
                         FROM diagnostic_generation_publications
                         WHERE record_state = 'current'",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .map(CodeGenerationId::new)
                    .transpose()
                    .map_err(conversion)?;
                Ok(DiagnosticReadResultV1::CurrentGeneration(generation))
            }
            DiagnosticReadOperationV1::Generation(generation) => read_records(
                snapshot,
                "WHERE generation_id = ?1 ORDER BY diagnostic_anchor",
                [generation.as_str()],
            )
            .map(DiagnosticReadResultV1::Records),
            DiagnosticReadOperationV1::CurrentForFile {
                generation_id,
                file_occurrence_id,
            } => read_records(
                snapshot,
                "WHERE generation_id = ?1 AND file_occurrence_id = ?2
                   AND record_state = 'current'
                 ORDER BY diagnostic_anchor",
                [generation_id.as_str(), file_occurrence_id.as_str()],
            )
            .map(DiagnosticReadResultV1::Records),
            DiagnosticReadOperationV1::ByAnchor(anchor) => {
                let record = read_record_by_anchor(snapshot, anchor)?;
                Ok(DiagnosticReadResultV1::Record(Box::new(record)))
            }
            // Stale findings stay queryable but never re-enter active
            // publication, so this lane selects the exact complement of the
            // current set rather than naming the two stale states.
            DiagnosticReadOperationV1::Stale(generation) => read_records(
                snapshot,
                "WHERE generation_id = ?1 AND record_state != 'current'
                 ORDER BY diagnostic_anchor",
                [generation.as_str()],
            )
            .map(DiagnosticReadResultV1::Records),
            DiagnosticReadOperationV1::SupersessionChain(anchor) => {
                read_supersession_chain(snapshot, anchor).map(DiagnosticReadResultV1::Records)
            }
        }
    }
}

/// Walks the supersession chain from `anchor`, oldest first and including the
/// starting record.
///
/// Each step follows the record's `Superseded { successor_generation }` edge to
/// the record in the successor generation carrying the same logical finding key
/// — repository, producer, code, file occurrence, span, and message digest.
/// The walk stops at a current, cleared, or missing successor. An anchor
/// already visited also stops the walk, so a cyclic `state_generation` graph
/// cannot spin here.
fn read_supersession_chain(
    connection: &rusqlite::Connection,
    anchor: &RetrievalAnchorId,
) -> rusqlite::Result<Vec<GenerationDiagnosticV1>> {
    let mut chain = Vec::new();
    let Some(start) = read_record_by_anchor(connection, anchor)? else {
        return Ok(chain);
    };
    chain.push(start);
    loop {
        let Some(last) = chain.last() else {
            return Ok(chain);
        };
        let DiagnosticRecordStateV1::Superseded {
            successor_generation,
        } = &last.state
        else {
            return Ok(chain);
        };
        let Some(successor) = read_logical_successor(connection, last, successor_generation)?
        else {
            return Ok(chain);
        };
        if chain
            .iter()
            .any(|seen| seen.diagnostic_anchor == successor.diagnostic_anchor)
        {
            return Ok(chain);
        }
        chain.push(successor);
    }
}

fn read_logical_successor(
    connection: &rusqlite::Connection,
    prior: &GenerationDiagnosticV1,
    successor_generation: &CodeGenerationId,
) -> rusqlite::Result<Option<GenerationDiagnosticV1>> {
    let sql = format!(
        "{SELECT_RECORDS} WHERE generation_id = ?1 AND repository = ?2 \
         AND producer = ?3 AND code = ?4 AND file_occurrence_id = ?5 \
         AND span_start = ?6 AND span_end = ?7 AND message_digest = ?8 \
         ORDER BY diagnostic_anchor"
    );
    let mut statement = connection.prepare(&sql)?;
    let mut records = statement
        .query_map(
            params![
                successor_generation.as_str(),
                prior.repository.as_str(),
                prior.provenance.producer.as_str(),
                prior.code,
                prior.file_occurrence_id.as_str(),
                u64_to_i64(prior.span.start_byte, "diagnostic span start")?,
                u64_to_i64(prior.span.end_byte, "diagnostic span end")?,
                prior.message_digest.as_str(),
            ],
            record_from_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if records.len() > 1 {
        return Err(conversion(format!(
            "ambiguous logical successor for {} in {successor_generation}",
            prior.diagnostic_anchor
        )));
    }
    Ok(records.pop())
}

fn insert_record(
    savepoint: &Savepoint<'_>,
    record: &GenerationDiagnosticV1,
) -> rusqlite::Result<()> {
    record.validate().map_err(invalid)?;
    let (state, state_generation) = state_columns(&record.state);
    savepoint.execute(
        "INSERT INTO generation_diagnostics (
            diagnostic_anchor, generation_id, repository, worktree, reference,
            source_revision, file_occurrence_id, content_digest, symbol_occurrence_id,
            span_start, span_end, code, severity, message, message_digest,
            producer_kind, producer, analyzer_revision, configuration_revision,
            sanitization_receipt, evidence_class, collected_at, record_state,
            state_generation, persisted_at
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
            ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25
         )",
        params![
            record.diagnostic_anchor.as_str(),
            record.generation_id.as_str(),
            record.repository.as_str(),
            record.worktree.as_ref().map(|value| value.as_str()),
            record.reference.as_ref().map(|value| value.as_str()),
            record.source_revision.as_ref().map(|value| value.as_str()),
            record.file_occurrence_id.as_str(),
            record.content_digest.as_str(),
            record
                .symbol_occurrence_id
                .as_ref()
                .map(|value| value.as_str()),
            u64_to_i64(record.span.start_byte, "diagnostic span start")?,
            u64_to_i64(record.span.end_byte, "diagnostic span end")?,
            record.code,
            severity_name(record.severity),
            record.message,
            record.message_digest.as_str(),
            producer_name(record.provenance.producer_kind),
            record.provenance.producer.as_str(),
            record.provenance.analyzer_revision.as_str(),
            record.provenance.configuration_revision.as_str(),
            record
                .provenance
                .sanitization_receipt
                .as_ref()
                .map(|value| value.as_str()),
            evidence_name(record.evidence_class),
            record.collected_at.0,
            state,
            state_generation,
            record.collected_at.0,
        ],
    )?;
    Ok(())
}

fn read_record_by_anchor(
    connection: &rusqlite::Connection,
    anchor: &RetrievalAnchorId,
) -> rusqlite::Result<Option<GenerationDiagnosticV1>> {
    let sql = format!("{SELECT_RECORDS} WHERE diagnostic_anchor = ?1");
    connection
        .query_row(&sql, [anchor.as_str()], record_from_row)
        .optional()
}

fn read_records<const N: usize>(
    connection: &rusqlite::Connection,
    clause: &str,
    parameters: [&str; N],
) -> rusqlite::Result<Vec<GenerationDiagnosticV1>> {
    let sql = format!("{SELECT_RECORDS} {clause}");
    let mut statement = connection.prepare(&sql)?;
    statement
        .query_map(rusqlite::params_from_iter(parameters), record_from_row)?
        .collect()
}

const SELECT_RECORDS: &str = "SELECT diagnostic_anchor, generation_id, repository, worktree,
    reference, source_revision, file_occurrence_id, content_digest, symbol_occurrence_id,
    span_start, span_end, code, severity, message, message_digest, producer_kind, producer,
    analyzer_revision, configuration_revision, sanitization_receipt, evidence_class,
    collected_at, record_state, state_generation
 FROM generation_diagnostics";

fn record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GenerationDiagnosticV1> {
    let text = |index| row.get::<_, String>(index);
    let optional_text = |index| row.get::<_, Option<String>>(index);
    let stored_state = text(22)?;
    let kind = DiagnosticRecordStateKindV1::parse(&stored_state)
        .ok_or_else(|| conversion(format!("unknown diagnostic state {stored_state}")))?;
    let state_generation = match (kind.state_generation_field(), optional_text(23)?) {
        (Some(_), Some(value)) => Some(CodeGenerationId::new(value).map_err(conversion)?),
        (Some(_), None) => {
            return Err(conversion(match kind {
                DiagnosticRecordStateKindV1::Cleared => "cleared diagnostic has no generation",
                _ => "superseded diagnostic has no generation",
            }));
        }
        (None, _) => None,
    };
    let state = kind
        .into_state(state_generation)
        .ok_or_else(|| conversion("current diagnostic carries a state generation"))?;
    let start = row.get::<_, i64>(9)?;
    let end = row.get::<_, i64>(10)?;
    if start < 0 || end < 0 {
        return Err(conversion("diagnostic span is negative"));
    }
    let record = GenerationDiagnosticV1 {
        diagnostic_anchor: RetrievalAnchorId::new(text(0)?).map_err(conversion)?,
        generation_id: CodeGenerationId::new(text(1)?).map_err(conversion)?,
        repository: tracedecay_domain::RepositoryId::new(text(2)?).map_err(conversion)?,
        worktree: optional_text(3)?
            .map(tracedecay_domain::WorktreeId::new)
            .transpose()
            .map_err(conversion)?,
        reference: optional_text(4)?
            .map(tracedecay_domain::RefId::new)
            .transpose()
            .map_err(conversion)?,
        source_revision: optional_text(5)?
            .map(tracedecay_domain::CommitId::new)
            .transpose()
            .map_err(conversion)?,
        file_occurrence_id: FileOccurrenceId::new(text(6)?).map_err(conversion)?,
        content_digest: tracedecay_domain::ContentDigest::new(text(7)?).map_err(conversion)?,
        symbol_occurrence_id: optional_text(8)?
            .map(tracedecay_domain::SymbolOccurrenceId::new)
            .transpose()
            .map_err(conversion)?,
        span: SourceSpan {
            start_byte: start as u64,
            end_byte: end as u64,
        },
        code: text(11)?,
        severity: parse_severity(&text(12)?)?,
        message: text(13)?,
        message_digest: tracedecay_domain::ManifestDigest::new(text(14)?).map_err(conversion)?,
        provenance: DiagnosticProvenanceV1 {
            producer_kind: parse_producer(&text(15)?)?,
            producer: tracedecay_domain::ProviderId::new(text(16)?).map_err(conversion)?,
            analyzer_revision: tracedecay_domain::ComponentVersion::new(text(17)?)
                .map_err(conversion)?,
            configuration_revision: tracedecay_domain::ComponentVersion::new(text(18)?)
                .map_err(conversion)?,
            sanitization_receipt: optional_text(19)?
                .map(tracedecay_domain::SanitizationReceiptId::new)
                .transpose()
                .map_err(conversion)?,
        },
        evidence_class: parse_evidence(&text(20)?)?,
        collected_at: UtcMicros(row.get(21)?),
        state,
    };
    record.validate().map_err(conversion)?;
    Ok(record)
}

// The mappings below delegate to the shared store codec; only the failure
// wording stays local, because it is observable in this adapter's errors.

fn state_columns(state: &DiagnosticRecordStateV1) -> (&'static str, Option<&str>) {
    diagnostic_state_columns(state)
}

fn severity_name(value: DiagnosticSeverityV1) -> &'static str {
    diagnostic_severity_name(value)
}

fn parse_severity(value: &str) -> rusqlite::Result<DiagnosticSeverityV1> {
    parse_diagnostic_severity(value)
        .ok_or_else(|| conversion(format!("unknown diagnostic severity {value}")))
}

fn producer_name(value: DiagnosticProducerKindV1) -> &'static str {
    diagnostic_producer_kind_name(value)
}

fn parse_producer(value: &str) -> rusqlite::Result<DiagnosticProducerKindV1> {
    parse_diagnostic_producer_kind(value)
        .ok_or_else(|| conversion(format!("unknown diagnostic producer {value}")))
}

fn evidence_name(value: DiagnosticEvidenceClassV1) -> &'static str {
    diagnostic_evidence_class_name(value)
}

fn parse_evidence(value: &str) -> rusqlite::Result<DiagnosticEvidenceClassV1> {
    parse_diagnostic_evidence_class(value)
        .ok_or_else(|| conversion(format!("unknown diagnostic evidence class {value}")))
}
