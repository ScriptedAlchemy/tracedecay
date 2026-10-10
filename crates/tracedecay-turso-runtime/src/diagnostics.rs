use crate::{
    Error, Value,
    ledger::{LedgerError, RequestSql, Row, params},
    writer::NativeWriteExecutor,
};
use tracedecay_domain::{
    CodeGenerationId, DiagnosticEvidenceClassV1, DiagnosticProducerKindV1, DiagnosticProvenanceV1,
    DiagnosticRecordStateV1, DiagnosticSeverityV1, FileOccurrenceId, GenerationDiagnosticV1,
    RetrievalAnchorId, SourceSpan, UtcMicros,
};
use tracedecay_store::{
    DIAGNOSTIC_STATE_CLEARED, DIAGNOSTIC_STATE_CURRENT, DiagnosticReadOperationV1,
    DiagnosticReadResultV1, DiagnosticRecordStateKindV1, SanitizedCleanDiagnosticSnapshotV1,
    diagnostic_evidence_class_name, diagnostic_producer_kind_name, diagnostic_severity_name,
    diagnostic_snapshot_observation_eq, diagnostic_state_columns, parse_diagnostic_evidence_class,
    parse_diagnostic_producer_kind, parse_diagnostic_severity,
};

const CURRENT: &str = DIAGNOSTIC_STATE_CURRENT;
const CLEARED: &str = DIAGNOSTIC_STATE_CLEARED;
const ANCHOR_COLLISION_BATCH: usize = 500;

#[derive(Default)]
pub struct NativeDiagnosticExecutor;
impl NativeWriteExecutor for NativeDiagnosticExecutor {
    fn execute(
        &mut self,
        sql: &RequestSql<'_>,
        payload: &tracedecay_store::RepositoryWritePayloadV1,
    ) -> Result<(), LedgerError> {
        use tracedecay_store::RepositoryWritePayloadV1;
        match payload {
            RepositoryWritePayloadV1::Diagnostics(snapshot) => {
                self.publish(sql, snapshot).map_err(Into::into)
            }
            RepositoryWritePayloadV1::Configuration(_)
            | RepositoryWritePayloadV1::Fact(_)
            | RepositoryWritePayloadV1::Observation(_)
            | RepositoryWritePayloadV1::ObservationBatch(_)
            | RepositoryWritePayloadV1::ObservationCursorAdvance(_)
            | RepositoryWritePayloadV1::RemoteObservationReplay(_)
            | RepositoryWritePayloadV1::RemoteWriterFenceInstall(_)
            | RepositoryWritePayloadV1::ExternalSource(_)
            | RepositoryWritePayloadV1::ExternalSourceBatch(_)
            | RepositoryWritePayloadV1::ExternalSourceProjection(_)
            | RepositoryWritePayloadV1::ExternalSourceAcquisition(_)
            | RepositoryWritePayloadV1::RetrievalAnchorDisposition(_)
            | RepositoryWritePayloadV1::RetrievalAnchorDerivative(_)
            | RepositoryWritePayloadV1::GitIndexTransaction(_)
            | RepositoryWritePayloadV1::EnqueueOutbox(_)
            | RepositoryWritePayloadV1::ApplyInbox(_)
            | RepositoryWritePayloadV1::AcknowledgeOutbox(_) => Err(Error::Unsupported(format!(
                "native diagnostic executor does not own {}",
                payload.name()
            ))
            .into()),
        }
    }
}
impl NativeDiagnosticExecutor {
    pub fn publish(
        &mut self,
        sql: &RequestSql<'_>,
        snapshot: &SanitizedCleanDiagnosticSnapshotV1,
    ) -> crate::Result<()> {
        let generation = snapshot.generation_id();
        let existing = sql.query("SELECT publication_revision, record_state FROM diagnostic_generation_publications WHERE generation_id = ?1 ORDER BY publication_revision DESC LIMIT 1", &[Value::Text(generation.as_str().to_owned())])?;
        let revision = match existing.values.into_iter().next().map(Row) {
            Some(row) => {
                if row.get::<_, String>(1)? != CURRENT {
                    return Err(invalid(
                        "historical diagnostic generation cannot be republished",
                    ));
                }
                let revision: i64 = row.get(0)?;
                let existing = read_records(
                    sql,
                    "WHERE generation_id = ?1 AND publication_revision = ?2 ORDER BY diagnostic_anchor",
                    &[
                        Value::Text(generation.as_str().to_owned()),
                        Value::Integer(revision),
                    ],
                )?;
                if diagnostic_snapshot_observation_eq(&existing, snapshot.records()) {
                    return Ok(());
                }
                revision
                    .checked_add(1)
                    .ok_or_else(|| invalid("diagnostic publication revision overflow"))?
            }
            None => 1,
        };
        let anchors = snapshot
            .records()
            .iter()
            .map(|record| record.diagnostic_anchor.as_str())
            .collect::<Vec<_>>();
        for chunk in anchors.chunks(ANCHOR_COLLISION_BATCH) {
            let placeholders = (2..=chunk.len() + 1)
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            let mut bindings = params![generation.as_str()];
            bindings.extend(chunk.iter().map(|anchor| Value::Text((*anchor).to_owned())));
            let collision = sql.query(&format!("SELECT 1 FROM generation_diagnostics WHERE generation_id != ?1 AND diagnostic_anchor IN ({placeholders}) LIMIT 1"), &bindings)?;
            if !collision.values.is_empty() {
                return Err(invalid(
                    "diagnostic anchor is already bound to another generation",
                ));
            }
        }
        sql.execute("UPDATE generation_diagnostics SET record_state = ?1, state_generation = ?2 WHERE record_state = ?3 AND generation_id != ?2 AND (generation_id, publication_revision) IN (SELECT generation_id, publication_revision FROM diagnostic_generation_publications WHERE record_state = ?3)", &[
            Value::Text(CLEARED.to_owned()),
            Value::Text(generation.as_str().to_owned()),
            Value::Text(CURRENT.to_owned()),
        ])?;
        sql.execute("UPDATE diagnostic_generation_publications SET record_state = ?1, state_generation = ?2 WHERE record_state = ?3", &[
            Value::Text(CLEARED.to_owned()),
            Value::Text(generation.as_str().to_owned()),
            Value::Text(CURRENT.to_owned()),
        ])?;
        for record in snapshot.records() {
            insert_record(sql, revision, record)?;
        }
        let published_at = snapshot
            .records()
            .iter()
            .map(|record| record.collected_at.0)
            .max()
            .unwrap_or(0);
        sql.execute("INSERT INTO diagnostic_generation_publications (generation_id, publication_revision, record_state, state_generation, published_at) VALUES (?1, ?2, ?3, NULL, ?4)", &[
            Value::Text(generation.as_str().to_owned()),
            Value::Integer(revision),
            Value::Text(CURRENT.to_owned()),
            Value::Integer(published_at),
        ])?;
        Ok(())
    }
    pub fn read(
        &mut self,
        sql: &RequestSql<'_>,
        operation: &DiagnosticReadOperationV1,
    ) -> crate::Result<DiagnosticReadResultV1> {
        match operation {
            DiagnosticReadOperationV1::CurrentGeneration => {
                let result = sql.query("SELECT generation_id FROM diagnostic_generation_publications WHERE record_state = 'current'", &[])?;
                if result.values.len() > 1 { return Err(invalid("multiple current diagnostic generations")); }
                let generation = result.values.into_iter().next().map(Row).map(|row| row.get::<_, String>(0)).transpose()?.map(CodeGenerationId::new).transpose().map_err(invalid)?;
                Ok(DiagnosticReadResultV1::CurrentGeneration(generation))
            }
            DiagnosticReadOperationV1::Generation(generation) => read_records(sql, "WHERE generation_id = ?1 AND publication_revision = (SELECT MAX(publication_revision) FROM diagnostic_generation_publications WHERE generation_id = ?1) ORDER BY diagnostic_anchor", &[Value::Text(generation.as_str().to_owned())]).map(DiagnosticReadResultV1::Records),
            DiagnosticReadOperationV1::Publication { generation_id, publication_revision } => {
                if *publication_revision == 0 { return Err(invalid("diagnostic publication revision must be positive")); }
                let revision = u64_to_i64(*publication_revision, "publication revision")?;
                read_records(sql, "WHERE generation_id = ?1 AND publication_revision = ?2 ORDER BY diagnostic_anchor", &[Value::Text(generation_id.as_str().to_owned()), Value::Integer(revision)]).map(DiagnosticReadResultV1::Records)
            }
            DiagnosticReadOperationV1::CurrentForFile { generation_id, file_occurrence_id } => read_records(sql, "WHERE generation_id = ?1 AND file_occurrence_id = ?2 AND record_state = 'current' AND publication_revision = (SELECT publication_revision FROM diagnostic_generation_publications WHERE generation_id = ?1 AND record_state = 'current') ORDER BY diagnostic_anchor", &[Value::Text(generation_id.as_str().to_owned()), Value::Text(file_occurrence_id.as_str().to_owned())]).map(DiagnosticReadResultV1::Records),
            DiagnosticReadOperationV1::ByAnchor(anchor) => read_records(sql, "WHERE diagnostic_anchor = ?1 ORDER BY CASE WHEN EXISTS (SELECT 1 FROM diagnostic_generation_publications AS publication WHERE publication.generation_id = generation_diagnostics.generation_id AND publication.publication_revision = generation_diagnostics.publication_revision AND publication.record_state = 'current') THEN 0 ELSE 1 END, publication_revision DESC LIMIT 1", &[Value::Text(anchor.as_str().to_owned())]).map(|mut records| DiagnosticReadResultV1::Record(Box::new(records.pop()))),
        }
    }
}
fn read_records(
    sql: &RequestSql<'_>,
    clause: &str,
    parameters: &[Value],
) -> crate::Result<Vec<GenerationDiagnosticV1>> {
    sql.query(&format!("{SELECT_RECORDS} {clause}"), parameters)?
        .values
        .into_iter()
        .map(|values| record_from_row(&Row(values)))
        .collect()
}
const SELECT_RECORDS: &str = "SELECT diagnostic_anchor, generation_id, repository, worktree, reference, source_revision, file_occurrence_id, content_digest, symbol_occurrence_id, span_start, span_end, code, severity, message, message_digest, producer_kind, producer, analyzer_revision, configuration_revision, sanitization_receipt, evidence_class, collected_at, record_state, state_generation FROM generation_diagnostics";
fn invalid(error: impl std::fmt::Display) -> Error {
    Error::InvalidOperation(error.to_string())
}
fn u64_to_i64(value: u64, field: &'static str) -> crate::Result<i64> {
    i64::try_from(value).map_err(|_| invalid(format!("{field} exceeds native integer range")))
}
fn insert_record(
    savepoint: &RequestSql<'_>,
    publication_revision: i64,
    record: &GenerationDiagnosticV1,
) -> crate::Result<()> {
    record.validate().map_err(invalid)?;
    let (state, state_generation) = state_columns(&record.state);
    savepoint.execute(
        "INSERT INTO generation_diagnostics (
            diagnostic_anchor, generation_id, publication_revision,
            repository, worktree, reference,
            source_revision, file_occurrence_id, content_digest, symbol_occurrence_id,
            span_start, span_end, code, severity, message, message_digest,
            producer_kind, producer, analyzer_revision, configuration_revision,
            sanitization_receipt, evidence_class, collected_at, record_state,
            state_generation, persisted_at
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
            ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26
         )",
        &params![
            record.diagnostic_anchor.as_str(),
            record.generation_id.as_str(),
            publication_revision,
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

fn record_from_row(row: &Row) -> crate::Result<GenerationDiagnosticV1> {
    let text = |index| row.get::<_, String>(index);
    let optional_text = |index| row.get::<_, Option<String>>(index);
    let stored_state = text(22)?;
    let kind = DiagnosticRecordStateKindV1::parse(&stored_state)
        .ok_or_else(|| invalid(format!("unknown diagnostic state {stored_state}")))?;
    let state_generation = match (kind.state_generation_field(), optional_text(23)?) {
        (Some(_), Some(value)) => Some(CodeGenerationId::new(value).map_err(invalid)?),
        (Some(_), None) => return Err(invalid("cleared diagnostic has no generation")),
        (None, _) => None,
    };
    let state = kind
        .into_state(state_generation)
        .ok_or_else(|| invalid("current diagnostic carries a state generation"))?;
    let start = row.get::<_, i64>(9)?;
    let end = row.get::<_, i64>(10)?;
    if start < 0 || end < 0 {
        return Err(invalid("diagnostic span is negative"));
    }
    let record = GenerationDiagnosticV1 {
        diagnostic_anchor: RetrievalAnchorId::new(text(0)?).map_err(invalid)?,
        generation_id: CodeGenerationId::new(text(1)?).map_err(invalid)?,
        repository: tracedecay_domain::RepositoryId::new(text(2)?).map_err(invalid)?,
        worktree: optional_text(3)?
            .map(tracedecay_domain::WorktreeId::new)
            .transpose()
            .map_err(invalid)?,
        reference: optional_text(4)?
            .map(tracedecay_domain::RefId::new)
            .transpose()
            .map_err(invalid)?,
        source_revision: optional_text(5)?
            .map(tracedecay_domain::CommitId::new)
            .transpose()
            .map_err(invalid)?,
        file_occurrence_id: FileOccurrenceId::new(text(6)?).map_err(invalid)?,
        content_digest: tracedecay_domain::ContentDigest::new(text(7)?).map_err(invalid)?,
        symbol_occurrence_id: optional_text(8)?
            .map(tracedecay_domain::SymbolOccurrenceId::new)
            .transpose()
            .map_err(invalid)?,
        span: SourceSpan {
            start_byte: start as u64,
            end_byte: end as u64,
        },
        code: text(11)?,
        severity: parse_severity(&text(12)?)?,
        message: text(13)?,
        message_digest: tracedecay_domain::ManifestDigest::new(text(14)?).map_err(invalid)?,
        provenance: DiagnosticProvenanceV1 {
            producer_kind: parse_producer(&text(15)?)?,
            producer: tracedecay_domain::ProviderId::new(text(16)?).map_err(invalid)?,
            analyzer_revision: tracedecay_domain::ComponentVersion::new(text(17)?)
                .map_err(invalid)?,
            configuration_revision: tracedecay_domain::ComponentVersion::new(text(18)?)
                .map_err(invalid)?,
            sanitization_receipt: optional_text(19)?
                .map(tracedecay_domain::SanitizationReceiptId::new)
                .transpose()
                .map_err(invalid)?,
        },
        evidence_class: parse_evidence(&text(20)?)?,
        collected_at: UtcMicros(row.get(21)?),
        state,
    };
    record.validate().map_err(invalid)?;
    Ok(record)
}

fn state_columns(state: &DiagnosticRecordStateV1) -> (&'static str, Option<&str>) {
    diagnostic_state_columns(state)
}

fn severity_name(value: DiagnosticSeverityV1) -> &'static str {
    diagnostic_severity_name(value)
}

fn parse_severity(value: &str) -> crate::Result<DiagnosticSeverityV1> {
    parse_diagnostic_severity(value)
        .ok_or_else(|| invalid(format!("unknown diagnostic severity {value}")))
}

fn producer_name(value: DiagnosticProducerKindV1) -> &'static str {
    diagnostic_producer_kind_name(value)
}

fn parse_producer(value: &str) -> crate::Result<DiagnosticProducerKindV1> {
    parse_diagnostic_producer_kind(value)
        .ok_or_else(|| invalid(format!("unknown diagnostic producer {value}")))
}

fn evidence_name(value: DiagnosticEvidenceClassV1) -> &'static str {
    diagnostic_evidence_class_name(value)
}

fn parse_evidence(value: &str) -> crate::Result<DiagnosticEvidenceClassV1> {
    parse_diagnostic_evidence_class(value)
        .ok_or_else(|| invalid(format!("unknown diagnostic evidence class {value}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Access, Database, ExecutionGuard, TransactionBehavior};
    use std::sync::{Arc, atomic::AtomicBool};
    use tracedecay_domain::{
        CommitId, ComponentVersion, ContentDigest, ManifestDigest, ProviderId, RefId, RepositoryId,
        WorktreeId,
    };
    fn record(generation: &str, anchor: &str) -> GenerationDiagnosticV1 {
        let mut record = GenerationDiagnosticV1 {
            diagnostic_anchor: RetrievalAnchorId::new(anchor).unwrap(),
            generation_id: CodeGenerationId::new(generation).unwrap(),
            repository: RepositoryId::new("repository.native").unwrap(),
            worktree: Some(WorktreeId::new("worktree.native").unwrap()),
            reference: Some(RefId::new("ref.main").unwrap()),
            source_revision: Some(CommitId::new("commit.native").unwrap()),
            file_occurrence_id: FileOccurrenceId::new("file.native").unwrap(),
            content_digest: ContentDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
            span: SourceSpan {
                start_byte: 10,
                end_byte: 42,
            },
            symbol_occurrence_id: None,
            code: "E0308".into(),
            severity: DiagnosticSeverityV1::Error,
            message: "mismatched types".into(),
            message_digest: ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap(),
            provenance: DiagnosticProvenanceV1 {
                producer_kind: DiagnosticProducerKindV1::UpstreamCompiler,
                producer: ProviderId::new("producer.rustc").unwrap(),
                analyzer_revision: ComponentVersion::new("analyzer.v1").unwrap(),
                configuration_revision: ComponentVersion::new("config.v1").unwrap(),
                sanitization_receipt: None,
            },
            evidence_class: DiagnosticEvidenceClassV1::ProducerReported,
            collected_at: UtcMicros(1),
            state: DiagnosticRecordStateV1::Current,
        };
        record.message_digest = record.compute_message_digest().unwrap();
        record
    }
    #[test]
    fn native_diagnostic_publication_preserves_history_collision_and_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diagnostics.db");
        let guard = ExecutionGuard::new(None, Arc::new(AtomicBool::new(false)), None);
        let database = Database::open(&path).unwrap();
        let mut connection = database.connect(Access::Writer).unwrap();
        connection
            .execute_batch(tracedecay_store::GENERATION_DIAGNOSTICS_SCHEMA_DDL, &guard)
            .unwrap();
        let first = record("generation.first", "anchor.first");
        let second = record("generation.second", "anchor.second");
        let snapshot = SanitizedCleanDiagnosticSnapshotV1::new(
            first.generation_id.clone(),
            vec![first.clone()],
        )
        .unwrap();
        let mut executor = NativeDiagnosticExecutor;
        connection
            .begin(TransactionBehavior::Immediate, &guard)
            .unwrap();
        {
            let sql = RequestSql::new(&mut connection, &guard);
            executor.publish(&sql, &snapshot).unwrap();
            executor.publish(&sql, &snapshot).unwrap();
        }
        connection.commit(&guard).unwrap();
        assert_eq!(
            connection
                .query(
                    "SELECT COUNT(*) FROM diagnostic_generation_publications",
                    &[],
                    &guard
                )
                .unwrap()
                .values,
            vec![vec![Value::Integer(1)]]
        );
        connection
            .begin(TransactionBehavior::Immediate, &guard)
            .unwrap();
        {
            let sql = RequestSql::new(&mut connection, &guard);
            executor
                .publish(
                    &sql,
                    &SanitizedCleanDiagnosticSnapshotV1::new(
                        second.generation_id.clone(),
                        vec![second.clone()],
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        connection.commit(&guard).unwrap();
        connection
            .begin(TransactionBehavior::Immediate, &guard)
            .unwrap();
        {
            let sql = RequestSql::new(&mut connection, &guard);
            assert!(executor.publish(&sql, &snapshot).is_err());
        }
        connection.rollback().unwrap();
        let collision = record("generation.third", "anchor.second");
        connection
            .begin(TransactionBehavior::Immediate, &guard)
            .unwrap();
        {
            let sql = RequestSql::new(&mut connection, &guard);
            assert!(
                executor
                    .publish(
                        &sql,
                        &SanitizedCleanDiagnosticSnapshotV1::new(
                            collision.generation_id.clone(),
                            vec![collision]
                        )
                        .unwrap()
                    )
                    .is_err()
            );
        }
        connection.rollback().unwrap();
        drop(connection);
        drop(database);
        let database = Database::open(&path).unwrap();
        let mut reader = database.connect(Access::Reader).unwrap();
        let sql = RequestSql::new(&mut reader, &guard);
        assert_eq!(
            executor
                .read(&sql, &DiagnosticReadOperationV1::CurrentGeneration)
                .unwrap(),
            DiagnosticReadResultV1::CurrentGeneration(Some(second.generation_id.clone()))
        );
        let DiagnosticReadResultV1::Record(found) = executor
            .read(
                &sql,
                &DiagnosticReadOperationV1::ByAnchor(second.diagnostic_anchor.clone()),
            )
            .unwrap()
        else {
            panic!("anchor record")
        };
        assert_eq!(*found, Some(second));
        let DiagnosticReadResultV1::Records(found) = executor
            .read(
                &sql,
                &DiagnosticReadOperationV1::Generation(first.generation_id.clone()),
            )
            .unwrap()
        else {
            panic!("historical records")
        };
        assert_eq!(found.len(), 1);
        assert!(matches!(
            found[0].state,
            DiagnosticRecordStateV1::Cleared { .. }
        ));
    }
}
