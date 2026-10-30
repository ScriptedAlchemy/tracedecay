use std::fs;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

use tracedecay_contracts::{
    CancellationSignal, CancellationStage, Deadline, EffectTermination, IdempotencyKey,
    SourceEditReconciliationDispositionV1, SourceEditReconciliationRequestV1,
    source_edit_operation,
};
use tracedecay_domain::UtcMicros;
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::test_support::{EffectUnknownFixture, effect_unknown_fixture};
use crate::SourceEditOutcome;
use crate::journal::{SourceEditDurability, SourceEditJournalStateV1};
use crate::outcome::SourceEditDurableOutcomeV1;
use crate::{
    SourceEditApplicationResult, SourceEditEffectControlV1, execute_source_edit,
    reconcile_source_edit_effect_unknown_with_control,
};

#[tokio::test]
async fn failed_move_keeps_published_destination_and_restart_preserves_partial_state() {
    let fixture = effect_unknown_fixture().await;
    fixture.assert_partial();
    let journal = fs::read_to_string(fixture.transaction_store().join("active.json")).unwrap();
    assert!(!journal.contains("pub fn"));
    assert!(!journal.contains("recovery_files"));

    let operation = source_edit_operation(fixture.request.edit.kind()).unwrap();
    let result = execute_source_edit(
        &fixture.graph,
        &fixture.code_graph,
        &operation,
        fixture.request.clone(),
        &fixture.authorization,
    )
    .await
    .unwrap();

    assert!(result.replayed);
    assert_eq!(
        result.effect.unwrap().receipt.outcome,
        EffectTermination::EffectUnknown
    );
    fixture.assert_partial();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(fixture.permission_preserving_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
}

#[tokio::test]
async fn prepared_restart_with_completed_edit_rolls_forward_and_preserves_bytes() {
    let fixture = effect_unknown_fixture().await;
    fixture.write_all_postimages();

    let operation = source_edit_operation(fixture.request.edit.kind()).unwrap();
    let result = execute_source_edit(
        &fixture.graph,
        &fixture.code_graph,
        &operation,
        fixture.request.clone(),
        &fixture.authorization,
    )
    .await
    .unwrap();

    assert!(result.replayed);
    assert_eq!(
        result.effect.unwrap().receipt.outcome,
        EffectTermination::Completed
    );
    fixture.assert_postimages();
}

async fn recover_by_replaying(
    fixture: &EffectUnknownFixture,
) -> Result<SourceEditApplicationResult> {
    let operation = source_edit_operation(fixture.request.edit.kind()).unwrap();
    execute_source_edit(
        &fixture.graph,
        &fixture.code_graph,
        &operation,
        fixture.request.clone(),
        &fixture.authorization,
    )
    .await
}

#[tokio::test]
async fn prepared_restart_with_unchanged_files_records_failure() {
    let fixture = effect_unknown_fixture().await;
    fixture.write_initial_state();

    let operation = source_edit_operation(fixture.request.edit.kind()).unwrap();
    let result = execute_source_edit(
        &fixture.graph,
        &fixture.code_graph,
        &operation,
        fixture.request.clone(),
        &fixture.authorization,
    )
    .await
    .unwrap();

    assert!(result.replayed);
    assert_eq!(
        result.effect.unwrap().receipt.outcome,
        EffectTermination::Failed
    );
    fixture.assert_preimages();
}

#[tokio::test]
async fn reconciliation_before_admission_cancellation_is_durable_and_replayable() {
    let fixture = effect_unknown_fixture().await;
    fixture.write_initial_state();
    let effect = fixture.result.effect.as_ref().unwrap();
    let reconciliation = SourceEditReconciliationRequestV1 {
        context: fixture.request.context.clone(),
        authority: fixture.request.authority.clone(),
        original_operation: effect.receipt.operation.clone(),
        effect_id: effect.effect_id.clone(),
        idempotency_key: fixture.request.idempotency_key.clone(),
        attempt_idempotency_key: IdempotencyKey::new("source-edit-reconciliation-attempt.fixture")
            .unwrap(),
        input_digest: effect.receipt.input_digest.clone(),
        disposition: SourceEditReconciliationDispositionV1::ConfirmUnchanged {},
        proof: fixture.request.proof.clone(),
        observed_at: UtcMicros(4),
    };
    let cancellation = CancellationSignal::active("cancel.reconcile.before-admission").unwrap();
    assert!(cancellation.cancel(UtcMicros(5)));
    let control =
        SourceEditEffectControlV1::new(Deadline::new(UtcMicros(i64::MAX)).unwrap(), cancellation);

    let result = reconcile_source_edit_effect_unknown_with_control(
        &fixture.graph,
        reconciliation.clone(),
        &fixture.authorization,
        &control,
    )
    .await
    .unwrap();
    assert_eq!(
        result.effect.as_ref().unwrap().receipt.outcome,
        EffectTermination::Cancelled
    );
    assert_eq!(
        result
            .effect
            .as_ref()
            .unwrap()
            .execution
            .cancellation
            .as_ref()
            .unwrap()
            .stage,
        CancellationStage::BeforeAdmission
    );
    fixture.assert_preimages();

    let replay = reconcile_source_edit_effect_unknown_with_control(
        &fixture.graph,
        reconciliation,
        &fixture.authorization,
        &control,
    )
    .await
    .unwrap();
    assert!(replay.replayed);
    assert_eq!(
        replay.effect.unwrap().receipt.outcome,
        EffectTermination::Cancelled
    );
}

#[cfg(windows)]
const FILE_SHARE_READ: u32 = 0x0000_0001;

#[tokio::test]
async fn completed_restart_retains_journal_when_receipt_publication_fails() {
    let fixture = effect_unknown_fixture().await;
    fixture.write_all_postimages();
    let durability = SourceEditDurability::for_graph(&fixture.graph);
    let journal_path = durability.journal_path();
    let before = fs::read(&journal_path).unwrap();
    let receipt_path = durability
        .receipt_path(&fixture.request.idempotency_key)
        .unwrap();
    let receipt_before = fs::read(&receipt_path).unwrap();
    #[cfg(not(windows))]
    let permissions = fs::metadata(receipt_path.parent().unwrap())
        .unwrap()
        .permissions();
    #[cfg(not(windows))]
    {
        let mut denied = permissions.clone();
        denied.set_readonly(true);
        fs::set_permissions(receipt_path.parent().unwrap(), denied).unwrap();
    }
    #[cfg(windows)]
    let held_receipt = {
        fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&receipt_path)
            .unwrap()
    };
    let outcome = recover_by_replaying(&fixture).await;
    #[cfg(not(windows))]
    fs::set_permissions(receipt_path.parent().unwrap(), permissions).unwrap();
    #[cfg(windows)]
    drop(held_receipt);

    let error = outcome.err().expect("receipt publication must fail");
    assert!(matches!(error, TraceDecayError::Config { .. }));
    assert_eq!(fs::read(&journal_path).unwrap(), before);
    assert_eq!(fs::read(&receipt_path).unwrap(), receipt_before);
    fixture.assert_postimages();

    let recovered = recover_by_replaying(&fixture).await.unwrap();
    assert!(!journal_path.exists());
    assert_eq!(
        recovered.effect.as_ref().unwrap().receipt.outcome,
        EffectTermination::Completed
    );
    let replay = recover_by_replaying(&fixture).await.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.effect, recovered.effect);
    fixture.assert_postimages();
}

#[tokio::test]
async fn completed_restart_replays_when_journal_cleanup_fails() {
    let fixture = effect_unknown_fixture().await;
    fixture.write_all_postimages();
    let durability = SourceEditDurability::for_graph(&fixture.graph);
    let journal_path = durability.journal_path();
    let before = fs::read(&journal_path).unwrap();
    let root = journal_path.parent().unwrap();
    #[cfg(not(windows))]
    let permissions = fs::metadata(root).unwrap().permissions();
    #[cfg(not(windows))]
    {
        let mut denied = permissions.clone();
        denied.set_readonly(true);
        fs::set_permissions(root, denied).unwrap();
    }
    #[cfg(windows)]
    let held_journal = {
        fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&journal_path)
            .unwrap()
    };
    let outcome = recover_by_replaying(&fixture).await;
    #[cfg(not(windows))]
    fs::set_permissions(root, permissions).unwrap();
    #[cfg(windows)]
    drop(held_journal);

    let error = outcome.err().expect("journal cleanup must fail");
    assert!(
        matches!(error, TraceDecayError::Config { message } if message.starts_with("remove source edit journal:"))
    );
    assert_eq!(fs::read(&journal_path).unwrap(), before);
    fixture.assert_postimages();
    let receipt = durability
        .load_receipt(&fixture.request.idempotency_key)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.effect.receipt.outcome, EffectTermination::Completed);

    let recovered = recover_by_replaying(&fixture).await.unwrap();
    assert!(!journal_path.exists());
    assert_eq!(
        recovered.effect.as_ref().unwrap().receipt.outcome,
        EffectTermination::Completed
    );
    let replay = recover_by_replaying(&fixture).await.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.effect, recovered.effect);
    fixture.assert_postimages();
}

#[tokio::test]
async fn applied_restart_preserves_files_and_replays_receipt() {
    let fixture = effect_unknown_fixture().await;
    fixture.write_all_postimages();
    let durability = SourceEditDurability::for_graph(&fixture.graph);
    let mut journal = durability.load_journal().unwrap().unwrap();
    let outcome = SourceEditOutcome::Reconciled {
        success: true,
        message: "completed move awaiting durable receipt".to_owned(),
    };
    journal.state = SourceEditJournalStateV1::Applied {
        outcome: SourceEditDurableOutcomeV1::from_live(&journal.request.operation, &outcome),
        committed_state: journal.predicted_state.clone().unwrap(),
        ended_at: UtcMicros(4),
        control_observation: None,
        verification_state: None,
    };
    durability.persist_journal(&journal).unwrap();

    let recovered = recover_by_replaying(&fixture).await.unwrap();
    assert_eq!(
        recovered.effect.as_ref().unwrap().receipt.outcome,
        EffectTermination::Completed
    );
    fixture.assert_postimages();
    assert_eq!(
        recovered.effect.as_ref().unwrap().receipt.outcome,
        EffectTermination::Completed
    );
    let replay = recover_by_replaying(&fixture).await.unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.effect, recovered.effect);
    fixture.assert_postimages();
}

#[tokio::test]
async fn restart_preserves_foreign_bytes_and_retains_pending_effect() {
    let fixture = effect_unknown_fixture().await;
    let foreign = b"pub fn foreign_change() {}\n";
    fs::write(fixture.destination_path(), foreign).unwrap();
    for _ in 0..2 {
        let result = recover_by_replaying(&fixture).await.unwrap();
        assert_eq!(
            result.effect.unwrap().receipt.outcome,
            EffectTermination::EffectUnknown
        );
        assert_eq!(fs::read(fixture.destination_path()).unwrap(), foreign);
        assert!(fixture.transaction_store().join("active.json").exists());
    }
}

async fn inspect_pending_effect(
    fixture: &EffectUnknownFixture,
    request: SourceEditReconciliationRequestV1,
) -> tracedecay_contracts::source_edit::SourceEditDurableEffectPayloadV1 {
    let control = SourceEditEffectControlV1::new(
        Deadline::new(UtcMicros(i64::MAX)).unwrap(),
        CancellationSignal::active("cancel.reconcile.inspection").unwrap(),
    );
    let result = reconcile_source_edit_effect_unknown_with_control(
        &fixture.graph,
        request,
        &fixture.authorization,
        &control,
    )
    .await
    .unwrap();
    let surface: tracedecay_contracts::source_edit::SourceEditSurfaceResultV1 =
        serde_json::from_value(result.value()).unwrap();
    assert!(surface.outcome.touched_files().is_empty());
    let tracedecay_contracts::source_edit::SourceEditSurfaceOutcomeV1::DurableMetadata(metadata) =
        surface.outcome
    else {
        panic!("inspection must retain its typed body-free observation");
    };
    assert!(!metadata.reconciled);
    assert!(!metadata.files.is_empty());
    assert!(metadata.observed_state.is_some());
    metadata
}

#[tokio::test]
async fn partial_acknowledgement_rejects_stale_digest_replays_and_admits_next_edit() {
    let fixture = effect_unknown_fixture().await;
    let durability = SourceEditDurability::for_graph(&fixture.graph);
    let journal = durability.load_journal().unwrap().unwrap();
    let effect = fixture.result.effect.as_ref().unwrap();
    let mut request = SourceEditReconciliationRequestV1 {
        context: fixture.request.context.clone(),
        authority: fixture.request.authority.clone(),
        original_operation: effect.receipt.operation.clone(),
        effect_id: effect.effect_id.clone(),
        idempotency_key: fixture.request.idempotency_key.clone(),
        attempt_idempotency_key: IdempotencyKey::new("partial.acknowledgement").unwrap(),
        input_digest: effect.receipt.input_digest.clone(),
        disposition: SourceEditReconciliationDispositionV1::Inspect {},
        proof: fixture.request.proof.clone(),
        observed_at: UtcMicros(4),
    };
    let inspection_request = request.clone();
    let inspection = inspect_pending_effect(&fixture, inspection_request.clone()).await;
    assert_eq!(inspection.files, journal.candidate_files);
    assert!(durability.journal_path().exists());
    assert_eq!(
        recover_by_replaying(&fixture)
            .await
            .unwrap()
            .effect
            .unwrap()
            .receipt
            .outcome,
        EffectTermination::EffectUnknown
    );
    request.attempt_idempotency_key =
        IdempotencyKey::new("partial.acknowledgement.resolve").unwrap();
    request.disposition = SourceEditReconciliationDispositionV1::AcknowledgePartial {
        observed_state: inspection.observed_state.clone().unwrap(),
    };
    let foreign = "pub fn foreign_change() {}\n";
    fs::write(fixture.destination_path(), foreign).unwrap();
    let replayed_inspection = inspect_pending_effect(&fixture, inspection_request.clone()).await;
    assert_eq!(replayed_inspection, inspection);
    let control = SourceEditEffectControlV1::new(
        Deadline::new(UtcMicros(i64::MAX)).unwrap(),
        CancellationSignal::active("partial.acknowledgement.control").unwrap(),
    );
    let error = reconcile_source_edit_effect_unknown_with_control(
        &fixture.graph,
        request.clone(),
        &fixture.authorization,
        &control,
    )
    .await
    .err()
    .unwrap();
    assert!(error.to_string().contains("acknowledgement is stale"));
    assert!(durability.journal_path().exists());
    assert_eq!(
        fs::read_to_string(fixture.destination_path()).unwrap(),
        foreign
    );

    let mut next = super::test_support::fixture_request_for_edit(
        tracedecay_contracts::SourceEditRequest::StrReplace {
            path: "src/b.rs".to_owned(),
            old_str: "foreign_change".to_owned(),
            new_str: "next_change".to_owned(),
            dry_run: false,
            verify: false,
        },
        "edit.after.partial.acknowledgement",
    );
    next.expected_state = crate::preview_source_edit_expected_state(
        &fixture.graph,
        &fixture.code_graph,
        &next.context,
        next.observed_at,
        next.edit.clone(),
    )
    .await
    .unwrap();
    let next_authorization = super::test_support::fixture_authorization(&next);
    let mut preview = next.clone();
    preview.edit = preview.edit.with_dry_run(true);
    preview.idempotency_key = IdempotencyKey::new("preview.after.partial.acknowledgement").unwrap();
    let operation = source_edit_operation(next.edit.kind()).unwrap();
    assert!(
        execute_source_edit(
            &fixture.graph,
            &fixture.code_graph,
            &operation,
            preview.clone(),
            &next_authorization,
        )
        .await
        .is_err()
    );
    assert_eq!(
        fs::read_to_string(fixture.destination_path()).unwrap(),
        foreign
    );

    let mut fresh_inspection = inspection_request;
    fresh_inspection.attempt_idempotency_key =
        IdempotencyKey::new("partial.inspect.fresh").unwrap();
    let current = inspect_pending_effect(&fixture, fresh_inspection)
        .await
        .observed_state
        .unwrap();
    assert_ne!(Some(current.clone()), inspection.observed_state);
    request.disposition = SourceEditReconciliationDispositionV1::AcknowledgePartial {
        observed_state: current.clone(),
    };
    let resolved = reconcile_source_edit_effect_unknown_with_control(
        &fixture.graph,
        request.clone(),
        &fixture.authorization,
        &control,
    )
    .await
    .unwrap();
    assert_eq!(
        resolved.effect.as_ref().unwrap().receipt.outcome,
        EffectTermination::Completed
    );
    assert!(!durability.journal_path().exists());
    durability.persist_journal(&journal).unwrap();
    let original = recover_by_replaying(&fixture).await.unwrap();
    assert!(!durability.journal_path().exists());
    assert_eq!(
        original.effect.as_ref().unwrap().receipt.outcome,
        EffectTermination::Partial
    );
    assert_eq!(
        original.effect.as_ref().unwrap().receipt.committed_state,
        Some(current)
    );
    assert_eq!(
        fs::read_to_string(fixture.destination_path()).unwrap(),
        foreign
    );
    let replay = reconcile_source_edit_effect_unknown_with_control(
        &fixture.graph,
        request.clone(),
        &fixture.authorization,
        &control,
    )
    .await
    .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.effect, resolved.effect);
    request.disposition = SourceEditReconciliationDispositionV1::ConfirmUnchanged {};
    assert!(
        reconcile_source_edit_effect_unknown_with_control(
            &fixture.graph,
            request,
            &fixture.authorization,
            &control,
        )
        .await
        .is_err()
    );

    let accepted_preview = execute_source_edit(
        &fixture.graph,
        &fixture.code_graph,
        &operation,
        preview,
        &next_authorization,
    )
    .await
    .unwrap();
    assert!(accepted_preview.outcome.success());
    assert!(accepted_preview.dry_run);
    next.expected_state = accepted_preview.expected_state;
    let next_result = execute_source_edit(
        &fixture.graph,
        &fixture.code_graph,
        &operation,
        next,
        &next_authorization,
    )
    .await
    .unwrap();
    assert_eq!(
        next_result.effect.unwrap().receipt.outcome,
        EffectTermination::Completed
    );
    assert_eq!(
        fs::read_to_string(fixture.destination_path()).unwrap(),
        "pub fn next_change() {}\n"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn reconciliation_receipt_gap_survives_process_exit_and_original_replay() {
    const CHILD_ROOT: &str = "TRACEDECAY_TEST_RECONCILIATION_GAP_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let (graph, _) = super::test_support::fixture_graph(std::path::Path::new(&root)).await;
        let durability = SourceEditDurability::for_graph(&graph);
        let journal = durability.load_journal().unwrap().unwrap();
        let original = super::test_support::fixture_request_for_edit(
            tracedecay_contracts::SourceEditRequest::MoveSymbol {
                symbol: "moved".to_owned(),
                dest_file: "src/b.rs".to_owned(),
                dry_run: false,
                update_references: false,
            },
            journal.request.idempotency_key.as_str(),
        );
        let inspection = durability
            .load_reconciliation_receipt(&IdempotencyKey::new("gap.inspect").unwrap())
            .unwrap()
            .unwrap();
        let mut request = crate::test_support::fixture_reconciliation(
            &original,
            &journal,
            SourceEditReconciliationDispositionV1::AcknowledgePartial {
                observed_state: inspection.outcome.observed_state.unwrap(),
            },
        );
        request.attempt_idempotency_key = IdempotencyKey::new("gap.acknowledge").unwrap();
        let authorization = super::test_support::fixture_authorization(&original);
        let control = SourceEditEffectControlV1::new(
            Deadline::new(UtcMicros(i64::MAX)).unwrap(),
            CancellationSignal::active("cancel.reconcile.receipt-gap").unwrap(),
        );
        let error = reconcile_source_edit_effect_unknown_with_control(
            &graph,
            request.clone(),
            &authorization,
            &control,
        )
        .await
        .err()
        .expect("second receipt publication must fail");
        assert!(
            error
                .to_string()
                .contains("persist source edit durable record")
        );
        assert_eq!(
            durability
                .load_receipt(&request.idempotency_key)
                .unwrap()
                .unwrap()
                .effect
                .receipt
                .outcome,
            EffectTermination::Partial
        );
        assert!(
            durability
                .load_reconciliation_receipt(&request.attempt_idempotency_key)
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            durability.load_journal().unwrap().unwrap().state,
            SourceEditJournalStateV1::Reconciled { .. }
        ));
        std::process::exit(73);
    }

    let fixture = effect_unknown_fixture().await;
    let durability = SourceEditDurability::for_graph(&fixture.graph);
    let journal = durability.load_journal().unwrap().unwrap();
    let mut request = crate::test_support::fixture_reconciliation(
        &fixture.request,
        &journal,
        SourceEditReconciliationDispositionV1::Inspect {},
    );
    request.attempt_idempotency_key = IdempotencyKey::new("gap.inspect").unwrap();
    let observed = inspect_pending_effect(&fixture, request.clone())
        .await
        .observed_state
        .unwrap();
    request.attempt_idempotency_key = IdempotencyKey::new("gap.acknowledge").unwrap();
    request.disposition = SourceEditReconciliationDispositionV1::AcknowledgePartial {
        observed_state: observed,
    };
    let receipt_directory = durability.root.join("reconciliation-receipts");
    let permissions = fs::metadata(&receipt_directory).unwrap().permissions();
    let mut denied = permissions.clone();
    denied.set_readonly(true);
    fs::set_permissions(&receipt_directory, denied).unwrap();
    let exited = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "edits::reconcile_tests::reconciliation_receipt_gap_survives_process_exit_and_original_replay", "--nocapture"])
        .env(CHILD_ROOT, fixture.project.path())
        .output().unwrap();
    fs::set_permissions(&receipt_directory, permissions).unwrap();
    assert_eq!(
        exited.status.code(),
        Some(73),
        "{}",
        String::from_utf8_lossy(&exited.stdout)
    );
    let completed = durability.load_journal().unwrap().unwrap();
    let SourceEditJournalStateV1::Reconciled { attempt, .. } = completed.state else {
        panic!("receipt pair must remain recoverable after process exit");
    };
    assert_eq!(
        attempt.input_digest,
        request.attempt_input_digest().unwrap()
    );
    let body_free = fs::read_to_string(durability.journal_path()).unwrap();
    assert!(!body_free.contains("pub fn"));
    assert!(!body_free.contains("recovery_files"));

    let original = recover_by_replaying(&fixture).await.unwrap();
    assert_eq!(
        original.effect.unwrap().receipt.outcome,
        EffectTermination::Partial
    );
    assert!(!durability.journal_path().exists());
    let control = SourceEditEffectControlV1::new(
        Deadline::new(UtcMicros(i64::MAX)).unwrap(),
        CancellationSignal::active("cancel.reconcile.replay").unwrap(),
    );
    let replay = reconcile_source_edit_effect_unknown_with_control(
        &fixture.graph,
        request,
        &fixture.authorization,
        &control,
    )
    .await
    .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.effect.as_ref(), Some(&attempt.effect));
    fixture.assert_partial();
}
