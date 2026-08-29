use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use super::super::lifecycle::AutomationRunControl;
use super::*;
use crate::application::memory::MemoryApplication;
use crate::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use crate::store::memory::DatabaseFactStore;
use tracedecay_domain::{Confidence, FactCategoryV1, FactOwnerV1, ProvenanceId, UtcMicros};
use tracedecay_store::{
    ProjectMemoryAutomaticFactApplyDispositionV1, ProjectMemoryAutomaticFactApplyResultV1,
    ProjectMemoryAutomaticFactEffectV1, ProjectMemoryAutomaticFactEvidenceV1,
    ProjectMemoryAutomaticFactReceiptV1, ProjectMemoryAutomaticFactStateV1,
    ProjectMemoryFactAddMaterialV1, ProjectMemoryFactListQueryV1, ProjectMemoryFactProjectionV1,
};
use tracedecay_usecases::memory::{
    MemoryApplicationError, MemoryMutationError, ProjectMemoryFactAddRequest,
    automatic_fact_add_command,
};

async fn database(path: &Path, mode: TestDatabaseRuntimeMode) -> Database {
    crate::register_test_schema_installer();
    let authority = DatabaseAuthority::acquire_test(path, "automatic fact receipt test").unwrap();
    Database::publish_test_runtime(path, &authority, mode)
        .await
        .unwrap()
        .0
}

fn test_run_control(interrupted: bool) -> (AutomationRunControl, Arc<AtomicBool>) {
    let interrupted = Arc::new(AtomicBool::new(interrupted));
    let observed = Arc::clone(&interrupted);
    let control =
        AutomationRunControl::from_interrupted(Arc::new(move || observed.load(Ordering::Acquire)));
    (control, interrupted)
}

fn request(content: &str) -> ProjectMemoryFactAddRequest {
    ProjectMemoryFactAddRequest {
        content: content.to_string(),
        category: FactCategoryV1::Project,
        source_label: Some("automatic-fact-test".to_string()),
        tags: vec!["automation".to_string()],
        entities: vec!["TraceDecay".to_string()],
        trust: Some(Confidence::new(0.9).unwrap()),
        metadata: serde_json::json!({"fixture": "automatic-fact-receipt"}),
    }
}

fn quarantined_apply_result(
    apply_id: &str,
    run_id: &str,
    content: &str,
) -> ProjectMemoryAutomaticFactApplyResultV1 {
    let owner = FactOwnerV1::Profile;
    let apply_id = ProvenanceId::new(apply_id.to_string()).unwrap();
    let command = automatic_fact_add_command(
        owner.clone(),
        request(content),
        run_id,
        apply_id.as_str(),
        None,
    )
    .unwrap();
    let evidence = ProjectMemoryAutomaticFactEvidenceV1::new(
        Some(format!("evidence-{}", apply_id.as_str())),
        None,
        Some(serde_json::json!({"status": "accepted"})),
    )
    .unwrap();
    let receipt = ProjectMemoryAutomaticFactReceiptV1::new(
        apply_id,
        owner,
        ProjectMemoryAutomaticFactStateV1::Quarantined,
        command,
        evidence,
        ProjectMemoryAutomaticFactEffectV1::Quarantined {
            reason: "canonical terminal quarantine".to_string(),
        },
        UtcMicros(1_700_000_000_000_000),
    )
    .unwrap();
    ProjectMemoryAutomaticFactApplyResultV1::new(
        receipt,
        ProjectMemoryAutomaticFactApplyDispositionV1::Quarantined,
    )
    .unwrap()
}

fn admitted_fact(content: &str, validation: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "add_fact_request": request(content),
        "validation": validation,
    })
}

#[test]
fn public_receipt_preserves_exact_canonical_recorded_at_micros() {
    let authority_result = quarantined_apply_result(
        "fact_recorded_at_micros",
        "run-recorded-at-micros",
        "Preserve exact canonical receipt time",
    );

    let receipt = automatic_fact_receipt(authority_result.receipt()).unwrap();
    assert_eq!(receipt.recorded_at_micros, 1_700_000_000_000_000);
    let wire = serde_json::to_value(receipt).unwrap();
    assert_eq!(
        wire["recorded_at_micros"],
        serde_json::json!(1_700_000_000_000_000_i64)
    );
    assert!(wire.get("recorded_at").is_none());

    let mut legacy_wire = wire;
    let exact_micros = legacy_wire
        .as_object_mut()
        .unwrap()
        .remove("recorded_at_micros")
        .unwrap();
    legacy_wire["recorded_at"] = exact_micros;
    assert!(serde_json::from_value::<AutomaticFactReceipt>(legacy_wire).is_err());
}

#[test]
fn automatic_fact_authority_digest_is_stable_and_binds_complete_receipt() {
    let first = quarantined_apply_result(
        "fact_digest_first",
        "run-digest",
        "Canonical digest material",
    );
    let same = first.clone();
    let changed = quarantined_apply_result(
        "fact_digest_changed",
        "run-digest",
        "Canonical digest material",
    );
    let changed_content = quarantined_apply_result(
        "fact_digest_first",
        "run-digest",
        "Canonical digest material changed",
    );
    let changed_run = quarantined_apply_result(
        "fact_digest_first",
        "run-digest-changed",
        "Canonical digest material",
    );

    assert_eq!(
        first.canonical_digest().unwrap(),
        same.canonical_digest().unwrap()
    );
    assert_ne!(
        first.canonical_digest().unwrap(),
        changed.canonical_digest().unwrap()
    );
    assert_ne!(
        first.canonical_digest().unwrap(),
        changed_content.canonical_digest().unwrap()
    );
    assert_ne!(
        first.canonical_digest().unwrap(),
        changed_run.canonical_digest().unwrap()
    );
}

async fn canonical_fact_count(
    memory: &MemoryApplication<DatabaseFactStore<'_>>,
    run_control: &AutomationRunControl,
) -> usize {
    memory
        .list_project_memory_facts(
            ProjectMemoryFactListQueryV1::new(memory.owner().clone(), None, None, None, 10)
                .unwrap(),
            run_control.read_control(),
        )
        .await
        .unwrap()
        .facts()
        .iter()
        .filter(|projection| matches!(projection, ProjectMemoryFactProjectionV1::Available(_)))
        .count()
}

fn shipped_sidecar(pending_request: Option<ProjectMemoryFactAddRequest>) -> serde_json::Value {
    let pending_request = pending_request.map(|request| {
        let mut request = serde_json::to_value(request).unwrap();
        let source_label = request
            .as_object_mut()
            .and_then(|request| request.remove("source_label"));
        if let (Some(source_label), Some(request)) = (source_label, request.as_object_mut()) {
            request.insert("source".to_string(), source_label);
        }
        request
    });
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

fn terminal_shipped_sidecar() -> serde_json::Value {
    let mut sidecar = shipped_sidecar(None);
    let record = sidecar["proposals"][0]
        .as_object_mut()
        .expect("fixture proposal must remain an object");
    record.insert("state".to_string(), serde_json::json!("applied"));
    record.insert("applied_fact_id".to_string(), serde_json::json!(42));
    record.insert(
        "apply_outcome".to_string(),
        serde_json::json!({"state": "applied", "fact_id": 42}),
    );
    sidecar
}

fn write_private_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[cfg(windows)]
    drop(tracedecay_runtime_core::windows_security::make_private_file(path).unwrap());
}

#[tokio::test]
async fn shipped_pending_proposals_require_reset_without_mutation_or_archive() {
    let temp = tempfile::tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    tokio::fs::create_dir_all(&dashboard_root).await.unwrap();
    let source_path = dashboard_root.join("fact_proposals.json");
    let source_bytes = serde_json::to_vec_pretty(&shipped_sidecar(Some(request(
        "Apply this shipped pending proposal through canonical memory",
    ))))
    .unwrap();
    let shipped_request = serde_json::from_slice::<serde_json::Value>(&source_bytes)
        .unwrap()["proposals"][0]["add_fact_request"]
        .clone();
    assert_eq!(
        shipped_request["source"],
        serde_json::json!("automatic-fact-test")
    );
    assert!(shipped_request.get("source_label").is_none());
    write_private_file(&source_path, &source_bytes);
    let db = database(
        &temp.path().join("memory.db"),
        TestDatabaseRuntimeMode::Initialize,
    )
    .await;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&db)).unwrap();
    let (run_control, _) = test_run_control(false);

    let disposition = inspect_shipped_fact_proposals(&dashboard_root)
        .await
        .unwrap();

    assert!(matches!(
        disposition,
        ShippedFactProposalDisposition::ResetRequired { .. }
    ));
    assert_eq!(tokio::fs::read(&source_path).await.unwrap(), source_bytes);
    assert!(!dashboard_root.join("fact_proposals.archive").exists());
    assert_eq!(canonical_fact_count(&memory, &run_control).await, 0);
    assert!(
        load_automatic_fact_receipt(&memory, "fact_0123456789abcdef", run_control.read_control(),)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn shipped_pending_record_without_request_still_requires_reset_without_effects() {
    let temp = tempfile::tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    tokio::fs::create_dir_all(&dashboard_root).await.unwrap();
    let source_path = dashboard_root.join("fact_proposals.json");
    let source_bytes = serde_json::to_vec(&shipped_sidecar(None)).unwrap();
    write_private_file(&source_path, &source_bytes);
    let db = database(
        &temp.path().join("memory.db"),
        TestDatabaseRuntimeMode::Initialize,
    )
    .await;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&db)).unwrap();
    let (run_control, _) = test_run_control(false);

    let disposition = inspect_shipped_fact_proposals(&dashboard_root)
        .await
        .unwrap();

    assert!(matches!(
        disposition,
        ShippedFactProposalDisposition::ResetRequired { .. }
    ));
    assert_eq!(tokio::fs::read(&source_path).await.unwrap(), source_bytes);
    assert_eq!(canonical_fact_count(&memory, &run_control).await, 0);
    assert!(!dashboard_root.join("fact_proposals.archive").exists());
}

#[tokio::test]
async fn terminal_shipped_records_are_classified_without_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    tokio::fs::create_dir_all(&dashboard_root).await.unwrap();
    let source_path = dashboard_root.join("fact_proposals.json");
    let source_bytes = serde_json::to_vec_pretty(&terminal_shipped_sidecar()).unwrap();
    write_private_file(&source_path, &source_bytes);
    let db = database(
        &temp.path().join("memory.db"),
        TestDatabaseRuntimeMode::Initialize,
    )
    .await;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&db)).unwrap();
    let (run_control, _) = test_run_control(false);

    let disposition = inspect_shipped_fact_proposals(&dashboard_root)
        .await
        .unwrap();
    assert!(matches!(
        disposition,
        ShippedFactProposalDisposition::TerminalHistory { .. }
    ));
    assert_eq!(tokio::fs::read(&source_path).await.unwrap(), source_bytes);
    assert_eq!(canonical_fact_count(&memory, &run_control).await, 0);
    assert!(!dashboard_root.join("fact_proposals.archive").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn shipped_sidecar_symlink_is_rejected_without_reading_its_target() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    std::fs::create_dir_all(&dashboard_root).unwrap();
    let target_path = temp.path().join("private-terminal-target.json");
    let target_bytes = serde_json::to_vec(&terminal_shipped_sidecar()).unwrap();
    write_private_file(&target_path, &target_bytes);
    let source_path = dashboard_root.join("fact_proposals.json");
    symlink(&target_path, &source_path).unwrap();

    let error = inspect_shipped_fact_proposals(&dashboard_root)
        .await
        .expect_err("the shipped source must never follow a symbolic link");

    assert!(error.to_string().contains("failed to open"));
    assert_eq!(std::fs::read(&target_path).unwrap(), target_bytes);
    assert!(
        std::fs::symlink_metadata(&source_path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[tokio::test]
async fn nonregular_shipped_sidecar_is_rejected_before_read() {
    let temp = tempfile::tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    std::fs::create_dir_all(&dashboard_root).unwrap();
    let source_path = dashboard_root.join("fact_proposals.json");
    std::fs::create_dir(&source_path).unwrap();

    let error = inspect_shipped_fact_proposals(&dashboard_root)
        .await
        .expect_err("a directory cannot become shipped proposal bytes");

    assert!(error.to_string().contains("failed to open"));
    assert!(std::fs::metadata(source_path).unwrap().is_dir());
}

#[cfg(unix)]
#[tokio::test]
async fn shipped_sidecar_requires_exact_unix_private_mode() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    std::fs::create_dir_all(&dashboard_root).unwrap();
    let source_path = dashboard_root.join("fact_proposals.json");
    let source_bytes = serde_json::to_vec(&terminal_shipped_sidecar()).unwrap();
    write_private_file(&source_path, &source_bytes);
    std::fs::set_permissions(&source_path, std::fs::Permissions::from_mode(0o640)).unwrap();

    let error = inspect_shipped_fact_proposals(&dashboard_root)
        .await
        .expect_err("a group-readable shipped sidecar must fail closed");

    assert!(error.to_string().contains("not private"));
    assert_eq!(std::fs::read(source_path).unwrap(), source_bytes);
}

#[tokio::test]
async fn oversized_sparse_shipped_sidecar_is_rejected_before_allocation() {
    let temp = tempfile::tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    std::fs::create_dir_all(&dashboard_root).unwrap();
    let source_path = dashboard_root.join("fact_proposals.json");
    write_private_file(&source_path, b"");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&source_path)
        .unwrap()
        .set_len(MAX_SHIPPED_FACT_PROPOSAL_BYTES as u64 + 1)
        .unwrap();

    let error = inspect_shipped_fact_proposals(&dashboard_root)
        .await
        .expect_err("an oversized sparse legacy sidecar must fail closed");

    assert!(error.to_string().contains("byte limit"));
    assert_eq!(
        std::fs::metadata(source_path).unwrap().len(),
        MAX_SHIPPED_FACT_PROPOSAL_BYTES as u64 + 1
    );
}

#[test]
fn shipped_sidecar_growth_after_metadata_is_rejected_by_bounded_read() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("fact_proposals.json");
    write_private_file(&source_path, b"bounded");
    let file = open_shipped_fact_proposal_file(&source_path).unwrap();
    let initial = file.metadata().unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&source_path)
        .unwrap()
        .set_len(MAX_SHIPPED_FACT_PROPOSAL_BYTES as u64 + 1)
        .unwrap();

    let error = read_opened_shipped_fact_proposal_bytes(&source_path, file, initial)
        .expect_err("growth after the initial metadata check must hit the bounded sentinel");

    assert!(error.to_string().contains("grew beyond"));
}

#[tokio::test]
async fn exact_maximum_shipped_sidecar_remains_a_valid_terminal_history() {
    let temp = tempfile::tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    std::fs::create_dir_all(&dashboard_root).unwrap();
    let source_path = dashboard_root.join("fact_proposals.json");
    let mut sidecar = terminal_shipped_sidecar();
    sidecar["proposals"][0]["proposal"]["bounded_padding"] = serde_json::json!("");
    let base = serde_json::to_vec(&sidecar).unwrap();
    assert!(base.len() < MAX_SHIPPED_FACT_PROPOSAL_BYTES);
    sidecar["proposals"][0]["proposal"]["bounded_padding"] =
        serde_json::json!("x".repeat(MAX_SHIPPED_FACT_PROPOSAL_BYTES - base.len()));
    let source_bytes = serde_json::to_vec(&sidecar).unwrap();
    assert_eq!(source_bytes.len(), MAX_SHIPPED_FACT_PROPOSAL_BYTES);
    write_private_file(&source_path, &source_bytes);

    let disposition = inspect_shipped_fact_proposals(&dashboard_root)
        .await
        .expect("the exact byte ceiling remains admitted");

    let ShippedFactProposalDisposition::TerminalHistory {
        source_bytes: observed,
        ..
    } = disposition
    else {
        panic!("the bounded terminal sidecar must remain terminal history")
    };
    assert_eq!(observed.len(), MAX_SHIPPED_FACT_PROPOSAL_BYTES);
    assert_eq!(observed, source_bytes);
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
    let (run_control, _) = test_run_control(false);
    let admitted = admitted_fact(
        "Keep automatic fact effects in the canonical memory authority",
        serde_json::json!({"dedupe": {"source_index": 3}}),
    );

    let batch = record_session_automatic_facts(
        &memory,
        &run_control,
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
    assert!(receipt.applied_fact_id.is_some());
    assert_eq!(
        receipt.add_fact_request.source_label.as_deref(),
        Some("automatic-fact-test")
    );

    let loaded =
        load_automatic_fact_receipt(&memory, &receipt.apply_id, run_control.read_control())
            .await
            .unwrap();
    assert_eq!(loaded.as_ref(), Some(receipt));
    assert_eq!(
        list_automatic_fact_receipts(
            &memory,
            Some(AutomaticFactState::Applied),
            10,
            run_control.read_control(),
        )
        .await
        .unwrap(),
        batch.receipts
    );
    assert!(
        list_automatic_fact_receipts(
            &memory,
            Some(AutomaticFactState::Quarantined),
            10,
            run_control.read_control(),
        )
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
    let (run_control, _) = test_run_control(false);
    let admitted = admitted_fact(
        "Replay this exact terminal automatic fact effect once",
        serde_json::json!({"dedupe": {"source_index": 0}}),
    );

    let first = record_session_automatic_facts(
        &memory,
        &run_control,
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
        &run_control,
        "run-exact-replay",
        Some("evidence-hash-replay"),
        &[admitted],
    )
    .await
    .unwrap();

    assert!(first.retry_error.is_none());
    assert!(replay.retry_error.is_none());
    assert_eq!(replay.receipts, first.receipts);
    assert_eq!(canonical_fact_count(&memory, &run_control).await, 1);
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
    let (run_control, _) = test_run_control(false);
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
        &run_control,
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
        list_automatic_fact_receipts(&memory, None, 10, run_control.read_control())
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(canonical_fact_count(&memory, &run_control).await, 1);
}

#[tokio::test]
async fn invalid_automatic_command_rejects_invalid_canonical_trust() {
    let temp = tempfile::tempdir().unwrap();
    let db = database(
        &temp.path().join("memory.db"),
        TestDatabaseRuntimeMode::Initialize,
    )
    .await;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&db)).unwrap();
    let (run_control, _) = test_run_control(false);
    let admitted = serde_json::json!({
        "add_fact_request": {
            "content": "Reject invalid automatic fact trust at the typed boundary",
            "category": "project",
            "source_label": "automatic-fact-test",
            "tags": ["automation"],
            "entities": ["TraceDecay"],
            "trust": 1.1,
            "metadata": {"fixture": "automatic-fact-receipt"}
        },
        "validation": {"status": "accepted"},
    });

    let error = match record_session_automatic_facts(
        &memory,
        &run_control,
        "run-invalid-command",
        Some("evidence-hash-invalid-command"),
        &[admitted],
    )
    .await
    {
        Ok(_) => panic!("invalid automatic command must fail before authority apply"),
        Err(error) => error,
    };

    let TraceDecayError::Config { message } = error else {
        panic!("invalid wire request must fail at canonical deserialization");
    };
    assert!(message.contains("invalid admitted automatic fact request"));
    assert!(message.contains("confidence must be finite and within [0.0, 1.0]"));
}

#[tokio::test]
async fn interrupted_automation_run_returns_a_retry_error_without_committing_a_fact() {
    let temp = tempfile::tempdir().unwrap();
    let db = database(
        &temp.path().join("memory.db"),
        TestDatabaseRuntimeMode::Initialize,
    )
    .await;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&db)).unwrap();
    let (run_control, interrupted) = test_run_control(false);
    interrupted.store(true, Ordering::Release);

    let batch = record_session_automatic_facts(
        &memory,
        &run_control,
        "run-interrupted-before-commit",
        Some("evidence-hash-interrupted"),
        &[admitted_fact(
            "Do not commit a fact after the automation run is interrupted",
            serde_json::json!({"status": "accepted"}),
        )],
    )
    .await
    .unwrap();

    assert!(batch.receipts.is_empty());
    let TraceDecayError::Database { operation, message } = batch
        .retry_error
        .expect("interrupted fact application must remain retryable")
    else {
        panic!("interrupted fact application must retain memory application typing");
    };
    assert_eq!(operation, "memory application");
    assert!(message.contains("fact write was interrupted before transaction admission"));
    interrupted.store(false, Ordering::Release);
    assert_eq!(canonical_fact_count(&memory, &run_control).await, 0);
}

#[test]
fn settled_invalid_authority_fact_preserves_its_receipt_across_effect_boundaries() {
    let apply_id = ProvenanceId::new("fact_settled_invalid_authority".to_string()).unwrap();
    let authority_result = quarantined_apply_result(
        apply_id.as_str(),
        "run-settled-invalid-authority",
        "Preserve the canonical receipt from a settled invalid authority result",
    );

    let settlement =
        automatic_fact_apply_settlement(Err(MemoryMutationError::InvalidAuthorityResult {
            error: MemoryApplicationError::InvalidAuthorityResult {
                invariant: "automatic fact receipt fixture invariant",
            },
            authority_result,
        }))
        .unwrap();
    let AutomaticFactApplySettlement::Terminal {
        receipt,
        validation_error: Some(error),
    } = settlement
    else {
        panic!("settled invalid authority result must retain its canonical receipt");
    };
    assert_eq!(receipt.apply_id(), apply_id.as_str());
    let projected = receipt
        .projected()
        .expect("valid invalid-authority receipt remains projectable");
    assert_eq!(projected.run_id, "run-settled-invalid-authority");
    assert_eq!(projected.state, AutomaticFactState::Quarantined);
    assert_eq!(
        projected.quarantine_reason.as_deref(),
        Some("canonical terminal quarantine")
    );
    let TraceDecayError::Database { operation, message } = error else {
        panic!("authority validation failure must keep memory application typing");
    };
    assert_eq!(operation, "memory application");
    assert!(message.contains("automatic fact receipt fixture invariant"));
}

#[test]
fn unprojectable_invalid_authority_fact_retains_raw_receipt_with_null_run_id() {
    let owner = FactOwnerV1::Profile;
    let apply_id = ProvenanceId::new("fact_unprojectable_invalid_authority".to_string()).unwrap();
    let automatic = automatic_fact_add_command(
        owner.clone(),
        request("Preserve an invalid receipt without fabricating its run identity"),
        "run-must-not-be-fabricated",
        apply_id.as_str(),
        None,
    )
    .unwrap();
    let command = ProjectMemoryFactAddMaterialV1::new(
        owner.clone(),
        automatic.content().to_string(),
        automatic.category(),
        automatic.source_label().map(ToOwned::to_owned),
        automatic.tags().to_vec(),
        automatic.entities().to_vec(),
        automatic.metadata().clone(),
        automatic.sanitization_receipt().clone(),
        None,
        automatic.default_trust(),
        automatic.actor().cloned(),
    )
    .unwrap()
    .into_command(automatic.operation_id().clone())
    .unwrap();
    let evidence = ProjectMemoryAutomaticFactEvidenceV1::new(
        Some("evidence-unprojectable-invalid-authority".to_string()),
        Some(serde_json::json!({"source": "invalid-authority"})),
        Some(serde_json::json!({"status": "accepted"})),
    )
    .unwrap();
    let receipt = ProjectMemoryAutomaticFactReceiptV1::new(
        apply_id.clone(),
        owner,
        ProjectMemoryAutomaticFactStateV1::Quarantined,
        command,
        evidence,
        ProjectMemoryAutomaticFactEffectV1::Quarantined {
            reason: "exact request binding rejected".to_string(),
        },
        UtcMicros(1_700_000_000_000_000),
    )
    .unwrap();
    let authority_result = ProjectMemoryAutomaticFactApplyResultV1::new(
        receipt,
        ProjectMemoryAutomaticFactApplyDispositionV1::Quarantined,
    )
    .unwrap();

    let AutomaticFactApplySettlement::Terminal {
        receipt: settled,
        validation_error: Some(_),
    } = automatic_fact_apply_settlement(Err(MemoryMutationError::InvalidAuthorityResult {
        error: MemoryApplicationError::InvalidAuthorityResult {
            invariant: "automatic fact exact request and evidence identity",
        },
        authority_result,
    }))
    .unwrap()
    else {
        panic!("invalid authority result must cross the boundary as a raw settled receipt");
    };
    let SettledAutomaticFactReceipt::InvalidAuthority(result) = &settled else {
        panic!("unprojectable invalid result must not enter the successful public wire");
    };
    assert!(result.receipt().automation_run_id().is_none());
    let ledger = settled.ledger_value();
    assert_eq!(ledger["run_id"], serde_json::Value::Null);
    assert_eq!(
        ledger["request"]["automation_run_id"],
        serde_json::Value::Null
    );
    assert_eq!(ledger["apply_id"], serde_json::json!(apply_id));
    assert_eq!(ledger["disposition"], serde_json::json!("quarantined"));
}

#[tokio::test]
async fn terminal_sidecar_never_touches_an_existing_archive_path() {
    let temp = tempfile::tempdir().unwrap();
    let dashboard_root = temp.path().join("dashboard");
    tokio::fs::create_dir_all(&dashboard_root).await.unwrap();
    let source_path = dashboard_root.join("fact_proposals.json");
    let source_bytes = serde_json::to_vec(&terminal_shipped_sidecar()).unwrap();
    write_private_file(&source_path, &source_bytes);
    let archive_blocker = dashboard_root.join("fact_proposals.archive");
    tokio::fs::write(&archive_blocker, b"not a directory")
        .await
        .unwrap();
    let db = database(
        &temp.path().join("memory.db"),
        TestDatabaseRuntimeMode::Initialize,
    )
    .await;
    let memory = MemoryApplication::new(FactOwnerV1::Profile, DatabaseFactStore::new(&db)).unwrap();
    let (run_control, _) = test_run_control(false);

    let disposition = inspect_shipped_fact_proposals(&dashboard_root)
        .await
        .unwrap();
    assert!(matches!(
        disposition,
        ShippedFactProposalDisposition::TerminalHistory { .. }
    ));
    assert_eq!(tokio::fs::read(&source_path).await.unwrap(), source_bytes);
    assert!(
        tokio::fs::metadata(&archive_blocker)
            .await
            .unwrap()
            .is_file()
    );
    assert_eq!(canonical_fact_count(&memory, &run_control).await, 0);
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
