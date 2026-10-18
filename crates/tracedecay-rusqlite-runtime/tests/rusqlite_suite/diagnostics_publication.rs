use rusqlite::Connection;
use tempfile::TempDir;
use tracedecay_domain::test_fixtures::{id, repeated_sha256_text as digest};
use tracedecay_domain::{
    DiagnosticEvidenceClassV1, DiagnosticProducerKindV1, DiagnosticProvenanceV1,
    DiagnosticRecordStateV1, DiagnosticSeverityV1, GenerationDiagnosticV1, SourceSpan, UtcMicros,
};
use tracedecay_rusqlite_runtime::StorageOperationExecutor;
use tracedecay_rusqlite_runtime::repository::ConcreteRepositoryWriteExecutor;
use tracedecay_store::{
    GENERATION_DIAGNOSTICS_SCHEMA_DDL, RepositoryWritePayloadV1, SanitizedCleanDiagnosticSnapshotV1,
};

fn record(generation: &str, anchor: &str) -> GenerationDiagnosticV1 {
    let mut record = GenerationDiagnosticV1 {
        diagnostic_anchor: id(anchor),
        generation_id: id(generation),
        repository: id("repository.fixture"),
        worktree: Some(id("worktree.fixture")),
        reference: Some(id("ref.main")),
        source_revision: Some(id("commit.abc123")),
        file_occurrence_id: id("file.occurrence.1"),
        content_digest: id(&digest('a')),
        span: SourceSpan {
            start_byte: 10,
            end_byte: 42,
        },
        symbol_occurrence_id: Some(id("symbol.occurrence.1")),
        code: "E0308".to_owned(),
        severity: DiagnosticSeverityV1::Error,
        message: "mismatched types".to_owned(),
        message_digest: id(&digest('b')),
        provenance: DiagnosticProvenanceV1 {
            producer_kind: DiagnosticProducerKindV1::UpstreamCompiler,
            producer: id("producer.rustc"),
            analyzer_revision: id("analyzer.v1"),
            configuration_revision: id("config.v1"),
            sanitization_receipt: Some(id("receipt.sanitization.1")),
        },
        evidence_class: DiagnosticEvidenceClassV1::ProducerReported,
        collected_at: UtcMicros(1_700_000_000_000_000),
        state: DiagnosticRecordStateV1::Current,
    };
    record.message_digest = record.compute_message_digest().unwrap();
    record
}

fn publish(
    connection: &mut Connection,
    generation: &str,
    anchors: &[String],
) -> Result<(), String> {
    let snapshot = SanitizedCleanDiagnosticSnapshotV1::new(
        id(generation),
        anchors
            .iter()
            .map(|anchor| record(generation, anchor))
            .collect(),
    )
    .unwrap();
    let savepoint = connection.savepoint().unwrap();
    ConcreteRepositoryWriteExecutor::default()
        .execute(
            &savepoint,
            &RepositoryWritePayloadV1::Diagnostics(Box::new(snapshot)),
        )
        .map_err(|error| error.to_string())?;
    savepoint.commit().unwrap();
    Ok(())
}

fn rows(connection: &Connection, generation: &str) -> i64 {
    connection
        .query_row(
            "SELECT COUNT(*) FROM generation_diagnostics WHERE generation_id = ?1",
            [generation],
            |row| row.get(0),
        )
        .unwrap()
}

/// An anchor already bound to another generation refuses the whole
/// publication even when it sits past the first probe batch, and the
/// refused publication writes nothing.
#[test]
fn an_anchor_bound_to_another_generation_refuses_the_publication() {
    let directory = TempDir::new().unwrap();
    let mut connection = Connection::open(directory.path().join("project.db")).unwrap();
    connection
        .execute_batch(GENERATION_DIAGNOSTICS_SCHEMA_DDL)
        .unwrap();
    publish(
        &mut connection,
        "generation.base",
        &["anchor.shared".to_owned()],
    )
    .unwrap();

    let mut anchors = (0..700)
        .map(|index| format!("anchor.next.{index:04}"))
        .collect::<Vec<_>>();
    let fresh = anchors.clone();
    anchors[650] = "anchor.shared".to_owned();

    let refusal = publish(&mut connection, "generation.next", &anchors).unwrap_err();
    assert!(
        refusal.contains("diagnostic anchor is already bound to another generation"),
        "{refusal}"
    );
    assert_eq!(rows(&connection, "generation.next"), 0);

    publish(&mut connection, "generation.next", &fresh).unwrap();
    assert_eq!(rows(&connection, "generation.next"), 700);
    assert_eq!(rows(&connection, "generation.base"), 1);
}
