use std::path::Path;

use super::*;
use crate::application::memory::MemoryApplication;
use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use crate::store::memory::DatabaseFactStore;
use tracedecay_domain::FactOwnerV1;
use tracedecay_store::{ProjectMemoryFactListQueryV1, ProjectMemoryFactProjectionV1};

async fn database(path: &Path, mode: TestDatabaseRuntimeMode) -> Database {
    crate::register_test_schema_installer();
    let authority = DatabaseAuthority::acquire_test(path, "automatic fact receipt test").unwrap();
    Database::publish_test_runtime(path, &authority, mode)
        .await
        .unwrap()
        .0
}

fn request(content: &str) -> AddFactRequest {
    AddFactRequest {
        content: content.to_string(),
        category: MemoryCategory::Project,
        source: Some("automatic-fact-test".to_string()),
        tags: vec!["automation".to_string()],
        entities: vec!["TraceDecay".to_string()],
        trust: Some(0.9),
        metadata: serde_json::json!({"fixture": "automatic-fact-receipt"}),
    }
}

fn admitted_fact(content: &str, validation: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "add_fact_request": request(content),
        "validation": validation,
    })
}

async fn canonical_fact_count(memory: &MemoryApplication<DatabaseFactStore<'_>>) -> usize {
    memory
        .list_project_memory_facts(
            ProjectMemoryFactListQueryV1::new(memory.owner().clone(), None, None, None, 10)
                .unwrap(),
        )
        .await
        .unwrap()
        .facts()
        .iter()
        .filter(|projection| matches!(projection, ProjectMemoryFactProjectionV1::Available(_)))
        .count()
}

fn shipped_sidecar(pending_request: Option<AddFactRequest>) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "proposals": [
            {
                "schema_version": 1,
                "proposal_id": "fact_0123456789abcdef",
                "run_id": "run-shipped-sidecar",
                "evidence_hash": "shipped-evidence-hash",
                "state": "pending_approval",
                "add_fact_request": pending_request,
                "proposal": {
                    "content": "Preserve shipped proposal provenance",
                    "source_span": {"message_id": "msg-shipped"}
                },
                "validation": {"status": "accepted"},
                "created_at": 1_700_000_000,
                "updated_at": 1_700_000_001,
                "duplicate_count": 2,
                "last_duplicate_run_id": "run-shipped-duplicate",
                "folded_contents": ["Earlier wording"]
            },
            {
                "schema_version": 1,
                "proposal_id": "fact_fedcba9876543210",
                "run_id": "run-shipped-sidecar",
                "state": "rejected",
                "proposal": {"content": "Transient rejected item"},
                "validation_reason": "not durable",
                "reviewer": "validator",
                "created_at": 1_700_000_002,
                "updated_at": 1_700_000_003
            }
        ]
    })
}

#[tokio::test]
async fn shipped_pending_proposals_receive_canonical_receipts_before_exact_archive() {
    let temp = tempfile::tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    tokio::fs::create_dir_all(&dashboard_root).await.unwrap();
    let source_path = dashboard_root.join("fact_proposals.json");
    let source_bytes = serde_json::to_vec_pretty(&shipped_sidecar(Some(request(
        "Apply this shipped pending proposal through canonical memory",
    ))))
    .unwrap();
    tokio::fs::write(&source_path, &source_bytes).await.unwrap();
    let db = database(
        &temp.path().join("memory.db"),
        TestDatabaseRuntimeMode::Initialize,
    )
    .await;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&db)).unwrap();

    let disposition = dispose_shipped_fact_proposals(&memory, &dashboard_root)
        .await
        .unwrap();
    let ShippedFactProposalDisposition::Archived {
        source_digest,
        archive_path,
        pending_receipts,
        preserved_terminal_records,
    } = disposition
    else {
        panic!("shipped sidecar must be archived after disposition");
    };

    assert!(source_digest.starts_with("sha256:"));
    assert_eq!(pending_receipts.len(), 1);
    assert_eq!(pending_receipts[0].apply_id, "fact_0123456789abcdef");
    assert_eq!(pending_receipts[0].state, AutomaticFactState::Applied);
    assert_eq!(pending_receipts[0].run_id, "run-shipped-sidecar");
    assert_eq!(
        pending_receipts[0]
            .item
            .as_ref()
            .and_then(|item| item.get("last_duplicate_run_id")),
        Some(&serde_json::json!("run-shipped-duplicate"))
    );
    assert_eq!(preserved_terminal_records, 1);
    assert_eq!(tokio::fs::read(&archive_path).await.unwrap(), source_bytes);
    assert!(!source_path.exists());
    assert_eq!(canonical_fact_count(&memory).await, 1);
    assert_eq!(
        load_automatic_fact_receipt(&memory, "fact_0123456789abcdef")
            .await
            .unwrap(),
        Some(pending_receipts[0].clone())
    );
    assert_eq!(
        dispose_shipped_fact_proposals(&memory, &dashboard_root)
            .await
            .unwrap(),
        ShippedFactProposalDisposition::NotPresent
    );
}

#[tokio::test]
async fn unsupported_shipped_pending_record_requires_reset_without_partial_effects() {
    let temp = tempfile::tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    tokio::fs::create_dir_all(&dashboard_root).await.unwrap();
    let source_path = dashboard_root.join("fact_proposals.json");
    let source_bytes = serde_json::to_vec(&shipped_sidecar(None)).unwrap();
    tokio::fs::write(&source_path, &source_bytes).await.unwrap();
    let db = database(
        &temp.path().join("memory.db"),
        TestDatabaseRuntimeMode::Initialize,
    )
    .await;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&db)).unwrap();

    let error = dispose_shipped_fact_proposals(&memory, &dashboard_root)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        TraceDecayError::ResetRequired { ref authority, .. }
            if authority == "shipped fact proposal sidecar"
    ));
    assert_eq!(tokio::fs::read(&source_path).await.unwrap(), source_bytes);
    assert_eq!(canonical_fact_count(&memory).await, 0);
    assert!(!dashboard_root.join("fact_proposals.archive").exists());
}

#[tokio::test]
async fn automatic_apply_commits_a_terminal_receipt_with_canonical_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let db = database(
        &temp.path().join("memory.db"),
        TestDatabaseRuntimeMode::Initialize,
    )
    .await;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&db)).unwrap();
    let admitted = admitted_fact(
        "Keep automatic fact effects in the canonical memory authority",
        serde_json::json!({"dedupe": {"source_index": 3}}),
    );

    let batch = record_session_automatic_facts(
        &memory,
        "run-terminal-effect",
        Some("evidence-hash-123"),
        &[admitted],
    )
    .await
    .unwrap();

    assert!(batch.retry_error.is_none());
    assert_eq!(batch.receipts.len(), 1);
    let receipt = &batch.receipts[0];
    assert_eq!(receipt.state, AutomaticFactState::Applied);
    assert_eq!(receipt.run_id, "run-terminal-effect");
    assert_eq!(receipt.evidence_hash.as_deref(), Some("evidence-hash-123"));
    assert_eq!(
        receipt.validation,
        Some(serde_json::json!({"dedupe": {"source_index": 3}}))
    );
    assert!(receipt.applied_canonical_fact_id.is_some());
    assert!(receipt.applied_fact_id.is_none());

    let loaded = load_automatic_fact_receipt(&memory, &receipt.apply_id)
        .await
        .unwrap();
    assert_eq!(loaded.as_ref(), Some(receipt));
    assert_eq!(
        list_automatic_fact_receipts(&memory, Some(AutomaticFactState::Applied), 10)
            .await
            .unwrap(),
        batch.receipts
    );
    assert!(
        list_automatic_fact_receipts(&memory, Some(AutomaticFactState::Quarantined), 10)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn automatic_apply_replays_the_exact_terminal_effect_without_another_fact() {
    let temp = tempfile::tempdir().unwrap();
    let database_path = temp.path().join("memory.db");
    let db = database(&database_path, TestDatabaseRuntimeMode::Initialize).await;
    let owner = FactOwnerV1::Profile;
    let memory = MemoryApplication::new(owner.clone(), DatabaseFactStore::new(&db)).unwrap();
    let admitted = admitted_fact(
        "Replay this exact terminal automatic fact effect once",
        serde_json::json!({"dedupe": {"source_index": 0}}),
    );

    let first = record_session_automatic_facts(
        &memory,
        "run-exact-replay",
        Some("evidence-hash-replay"),
        std::slice::from_ref(&admitted),
    )
    .await
    .unwrap();
    drop(memory);
    drop(db);

    let db = database(&database_path, TestDatabaseRuntimeMode::Existing).await;
    let memory = MemoryApplication::new(owner, DatabaseFactStore::new(&db)).unwrap();
    let replay = record_session_automatic_facts(
        &memory,
        "run-exact-replay",
        Some("evidence-hash-replay"),
        &[admitted],
    )
    .await
    .unwrap();

    assert!(first.retry_error.is_none());
    assert!(replay.retry_error.is_none());
    assert_eq!(replay.receipts, first.receipts);
    assert_eq!(canonical_fact_count(&memory).await, 1);
}

#[tokio::test]
async fn automatic_apply_collapses_semantic_duplicates_without_losing_first_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let db = database(
        &temp.path().join("memory.db"),
        TestDatabaseRuntimeMode::Initialize,
    )
    .await;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&db)).unwrap();
    let first = admitted_fact(
        "Keep terminal receipt evidence with the first automatic fact effect",
        serde_json::json!({"source_index": 0}),
    );
    let duplicate = admitted_fact(
        "  keep terminal receipt evidence with the FIRST automatic fact effect  ",
        serde_json::json!({"source_index": 1}),
    );

    let batch = record_session_automatic_facts(
        &memory,
        "run-semantic-duplicate",
        Some("evidence-hash-duplicate"),
        &[first, duplicate],
    )
    .await
    .unwrap();

    assert!(batch.retry_error.is_none());
    assert_eq!(batch.receipts.len(), 1);
    assert_eq!(
        batch.receipts[0].validation,
        Some(serde_json::json!({"source_index": 0}))
    );
    assert_eq!(
        list_automatic_fact_receipts(&memory, None, 10)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(canonical_fact_count(&memory).await, 1);
}

#[tokio::test]
async fn invalid_automatic_command_uses_the_memory_application_error_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let db = database(
        &temp.path().join("memory.db"),
        TestDatabaseRuntimeMode::Initialize,
    )
    .await;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&db)).unwrap();
    let mut invalid_request = request("Reject invalid automatic fact trust at the typed boundary");
    invalid_request.trust = Some(1.1);
    let admitted = serde_json::json!({
        "add_fact_request": invalid_request,
        "validation": {"status": "accepted"},
    });

    let error = match record_session_automatic_facts(
        &memory,
        "run-invalid-command",
        Some("evidence-hash-invalid-command"),
        &[admitted],
    )
    .await
    {
        Ok(_) => panic!("invalid automatic command must fail before authority apply"),
        Err(error) => error,
    };

    let TraceDecayError::Database { operation, message } = error else {
        panic!("memory application error must retain its canonical classification");
    };
    assert_eq!(operation, "memory application");
    assert!(message.contains("trust must be between 0.0 and 1.0"));
}

#[test]
fn automatic_fact_state_serializes_only_terminal_values() {
    for (state, wire) in [
        (AutomaticFactState::Applied, "applied"),
        (AutomaticFactState::Quarantined, "quarantined"),
    ] {
        assert_eq!(
            serde_json::to_value(state).unwrap(),
            serde_json::json!(wire)
        );
        assert_eq!(
            serde_json::from_value::<AutomaticFactState>(serde_json::json!(wire)).unwrap(),
            state
        );
    }
    assert!(AutomaticFactState::parse("retry").is_err());
}
