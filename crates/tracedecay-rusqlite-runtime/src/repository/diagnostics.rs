use rusqlite::{OptionalExtension, Savepoint, Transaction, params};
use tracedecay_domain::{
    CodeGenerationId, DiagnosticEvidenceClassV1, DiagnosticProducerKindV1, DiagnosticProvenanceV1,
    DiagnosticRecordStateV1, DiagnosticSeverityV1, FileOccurrenceId, GenerationDiagnosticV1,
    RetrievalAnchorId, SourceSpan, UtcMicros,
};
use tracedecay_store::{
    DiagnosticReadOperationV1, DiagnosticReadResultV1, SanitizedCleanDiagnosticSnapshotV1,
};

use super::support::{conversion, invalid, u64_to_i64};

const CURRENT: &str = "current";
const SUPERSEDED: &str = "superseded";
const CLEARED: &str = "cleared";

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
        }
    }
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
    let state = match text(22)?.as_str() {
        CURRENT => DiagnosticRecordStateV1::Current,
        SUPERSEDED => DiagnosticRecordStateV1::Superseded {
            successor_generation: CodeGenerationId::new(
                optional_text(23)?
                    .ok_or_else(|| conversion("superseded diagnostic has no generation"))?,
            )
            .map_err(conversion)?,
        },
        CLEARED => DiagnosticRecordStateV1::Cleared {
            cleared_in_generation: CodeGenerationId::new(
                optional_text(23)?
                    .ok_or_else(|| conversion("cleared diagnostic has no generation"))?,
            )
            .map_err(conversion)?,
        },
        state => return Err(conversion(format!("unknown diagnostic state {state}"))),
    };
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

fn state_columns(state: &DiagnosticRecordStateV1) -> (&'static str, Option<&str>) {
    match state {
        DiagnosticRecordStateV1::Current => (CURRENT, None),
        DiagnosticRecordStateV1::Superseded {
            successor_generation,
        } => (SUPERSEDED, Some(successor_generation.as_str())),
        DiagnosticRecordStateV1::Cleared {
            cleared_in_generation,
        } => (CLEARED, Some(cleared_in_generation.as_str())),
    }
}

fn severity_name(value: DiagnosticSeverityV1) -> &'static str {
    match value {
        DiagnosticSeverityV1::Error => "error",
        DiagnosticSeverityV1::Warning => "warning",
        DiagnosticSeverityV1::Information => "information",
        DiagnosticSeverityV1::Hint => "hint",
    }
}

fn parse_severity(value: &str) -> rusqlite::Result<DiagnosticSeverityV1> {
    match value {
        "error" => Ok(DiagnosticSeverityV1::Error),
        "warning" => Ok(DiagnosticSeverityV1::Warning),
        "information" => Ok(DiagnosticSeverityV1::Information),
        "hint" => Ok(DiagnosticSeverityV1::Hint),
        value => Err(conversion(format!("unknown diagnostic severity {value}"))),
    }
}

fn producer_name(value: DiagnosticProducerKindV1) -> &'static str {
    match value {
        DiagnosticProducerKindV1::UpstreamCompiler => "upstream_compiler",
        DiagnosticProducerKindV1::LanguageServer => "language_server",
        DiagnosticProducerKindV1::TracedecayStructural => "tracedecay_structural",
        DiagnosticProducerKindV1::TracedecayGraphIntegrity => "tracedecay_graph_integrity",
        DiagnosticProducerKindV1::TracedecayPolicy => "tracedecay_policy",
        DiagnosticProducerKindV1::TracedecayCodeHealth => "tracedecay_code_health",
        DiagnosticProducerKindV1::GenerationConsistency => "generation_consistency",
        DiagnosticProducerKindV1::AuthorizedExternalAnalyzer => "authorized_external_analyzer",
    }
}

fn parse_producer(value: &str) -> rusqlite::Result<DiagnosticProducerKindV1> {
    match value {
        "upstream_compiler" => Ok(DiagnosticProducerKindV1::UpstreamCompiler),
        "language_server" => Ok(DiagnosticProducerKindV1::LanguageServer),
        "tracedecay_structural" => Ok(DiagnosticProducerKindV1::TracedecayStructural),
        "tracedecay_graph_integrity" => Ok(DiagnosticProducerKindV1::TracedecayGraphIntegrity),
        "tracedecay_policy" => Ok(DiagnosticProducerKindV1::TracedecayPolicy),
        "tracedecay_code_health" => Ok(DiagnosticProducerKindV1::TracedecayCodeHealth),
        "generation_consistency" => Ok(DiagnosticProducerKindV1::GenerationConsistency),
        "authorized_external_analyzer" => Ok(DiagnosticProducerKindV1::AuthorizedExternalAnalyzer),
        value => Err(conversion(format!("unknown diagnostic producer {value}"))),
    }
}

fn evidence_name(value: DiagnosticEvidenceClassV1) -> &'static str {
    match value {
        DiagnosticEvidenceClassV1::ObservedCurrent => "observed_current",
        DiagnosticEvidenceClassV1::ProducerReported => "producer_reported",
        DiagnosticEvidenceClassV1::DerivedStructural => "derived_structural",
        DiagnosticEvidenceClassV1::UnknownUnsupported => "unknown_unsupported",
    }
}

fn parse_evidence(value: &str) -> rusqlite::Result<DiagnosticEvidenceClassV1> {
    match value {
        "observed_current" => Ok(DiagnosticEvidenceClassV1::ObservedCurrent),
        "producer_reported" => Ok(DiagnosticEvidenceClassV1::ProducerReported),
        "derived_structural" => Ok(DiagnosticEvidenceClassV1::DerivedStructural),
        "unknown_unsupported" => Ok(DiagnosticEvidenceClassV1::UnknownUnsupported),
        value => Err(conversion(format!(
            "unknown diagnostic evidence class {value}"
        ))),
    }
}
