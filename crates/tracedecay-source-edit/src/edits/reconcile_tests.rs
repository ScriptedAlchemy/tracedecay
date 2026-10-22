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
        disposition: SourceEditReconciliationDispositionV1::ConfirmUnchanged,
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

#[tokio::test]
async fn partial_acknowledgement_rejects_stale_digest_replays_and_admits_next_edit() {
    let fixture = effect_unknown_fixture().await;
    let durability = SourceEditDurability::for_graph(&fixture.graph);
    let journal = durability.load_journal().unwrap().unwrap();
    let observed =
        crate::digest::source_edit_state_digest(fixture.project.path(), &journal.candidate_files)
            .unwrap();
    let effect = fixture.result.effect.as_ref().unwrap();
    let mut request = SourceEditReconciliationRequestV1 {
        context: fixture.request.context.clone(),
        authority: fixture.request.authority.clone(),
        original_operation: effect.receipt.operation.clone(),
        effect_id: effect.effect_id.clone(),
        idempotency_key: fixture.request.idempotency_key.clone(),
        attempt_idempotency_key: IdempotencyKey::new("partial.acknowledgement").unwrap(),
        input_digest: effect.receipt.input_digest.clone(),
        disposition: SourceEditReconciliationDispositionV1::AcknowledgePartial {
            observed_state: observed,
        },
        proof: fixture.request.proof.clone(),
        observed_at: UtcMicros(4),
    };
    let foreign = "pub fn foreign_change() {}\n";
    fs::write(fixture.destination_path(), foreign).unwrap();
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
    let operation = source_edit_operation(next.edit.kind()).unwrap();
    assert!(
        execute_source_edit(
            &fixture.graph,
            &fixture.code_graph,
            &operation,
            next.clone(),
            &next_authorization,
        )
        .await
        .is_err()
    );
    assert_eq!(
        fs::read_to_string(fixture.destination_path()).unwrap(),
        foreign
    );

    let current =
        crate::digest::source_edit_state_digest(fixture.project.path(), &journal.candidate_files)
            .unwrap();
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
    request.disposition = SourceEditReconciliationDispositionV1::ConfirmUnchanged;
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

#[tokio::test]
async fn retired_operation_journal_discards_bodies_and_allows_partial_acknowledgement() {
    let fixture = effect_unknown_fixture().await;
    let durability = SourceEditDurability::for_graph(&fixture.graph);
    let mut legacy: serde_json::Value =
        serde_json::from_slice(&fs::read(durability.journal_path()).unwrap()).unwrap();
    legacy["request"]["operation"] = serde_json::json!("use-case.application.source-edit.rollback");
    let original_expected = legacy["expected_state"].clone();
    legacy["expected_state"] = legacy["predicted_state"].clone();
    legacy["predicted_state"] = original_expected;
    legacy["recovery_files"] = serde_json::json!([{
        "relative_path": "src/b.rs", "expected": "retained private preimage",
        "intended": "retained private postimage",
    }]);
    legacy["recovery_digest"] = serde_json::json!("obsolete body digest");
    fs::write(
        durability.journal_path(),
        serde_json::to_vec(&legacy).unwrap(),
    )
    .unwrap();
    let obsolete = durability.root.join("rollback-records");
    fs::create_dir(&obsolete).unwrap();
    fs::write(obsolete.join("saved.json"), b"private undo body").unwrap();
    let unrelated = fixture.project.path().join("rollback-records");
    fs::create_dir(&unrelated).unwrap();
    fs::write(unrelated.join("owned-elsewhere"), b"unrelated").unwrap();

    for _ in 0..2 {
        let _lease = durability.lock().await.unwrap();
        let journal = durability.load_journal().unwrap().unwrap();
        assert_eq!(
            journal.request.operation.as_str(),
            "use-case.application.source-edit.rollback"
        );
        crate::reconcile::recover_source_edit_transaction(
            &durability,
            &fixture.graph,
            fixture.request.context.scope(),
        )
        .await
        .unwrap();
        fixture.assert_partial();
    }
    let normalized = fs::read_to_string(durability.journal_path()).unwrap();
    assert!(!normalized.contains("retained private"));
    assert!(!normalized.contains("recovery_files"));
    assert!(!normalized.contains("recovery_digest"));
    assert!(!obsolete.exists());
    assert_eq!(
        fs::read(unrelated.join("owned-elsewhere")).unwrap(),
        b"unrelated"
    );
    let journal = durability.load_journal().unwrap().unwrap();
    let observed =
        crate::digest::source_edit_state_digest(fixture.project.path(), &journal.candidate_files)
            .unwrap();
    let pending = durability
        .load_receipt(&fixture.request.idempotency_key)
        .unwrap()
        .unwrap();
    assert_eq!(
        pending.effect.receipt.outcome,
        EffectTermination::EffectUnknown
    );
    assert_eq!(pending.effect.receipt.operation, journal.request.operation);
    let mut request = SourceEditReconciliationRequestV1 {
        context: fixture.request.context.clone(),
        authority: fixture.request.authority.clone(),
        original_operation: fixture
            .result
            .effect
            .as_ref()
            .unwrap()
            .receipt
            .operation
            .clone(),
        effect_id: journal.effect_id.clone(),
        idempotency_key: journal.request.idempotency_key.clone(),
        attempt_idempotency_key: IdempotencyKey::new("legacy.partial.acknowledgement").unwrap(),
        input_digest: journal.input_digest.clone(),
        disposition: SourceEditReconciliationDispositionV1::AcknowledgePartial {
            observed_state: observed.clone(),
        },
        proof: fixture.request.proof.clone(),
        observed_at: UtcMicros(4),
    };
    let control = SourceEditEffectControlV1::new(
        Deadline::new(UtcMicros(i64::MAX)).unwrap(),
        CancellationSignal::active("legacy.partial.acknowledgement.control").unwrap(),
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
    assert!(error.to_string().contains("identity does not match"));
    assert!(durability.journal_path().exists());
    fixture.assert_partial();

    request.original_operation = journal.request.operation.clone();
    let result = reconcile_source_edit_effect_unknown_with_control(
        &fixture.graph,
        request,
        &fixture.authorization,
        &control,
    )
    .await
    .unwrap();
    assert_eq!(
        result.effect.unwrap().receipt.outcome,
        EffectTermination::Completed
    );
    assert!(!durability.journal_path().exists());
    fixture.assert_partial();
    let original = durability
        .load_receipt(&fixture.request.idempotency_key)
        .unwrap()
        .unwrap();
    assert_eq!(original.effect.receipt.operation, journal.request.operation);
    assert_eq!(original.effect.receipt.outcome, EffectTermination::Partial);
    assert_eq!(original.effect.receipt.committed_state, Some(observed));

    let mut next = super::test_support::fixture_request_for_edit(
        tracedecay_contracts::SourceEditRequest::StrReplace {
            path: "src/b.rs".to_owned(),
            old_str: "existing".to_owned(),
            new_str: "next_change".to_owned(),
            dry_run: false,
            verify: false,
        },
        "edit.after.legacy.partial.acknowledgement",
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
    let authorization = super::test_support::fixture_authorization(&next);
    let operation = source_edit_operation(next.edit.kind()).unwrap();
    let next_result = execute_source_edit(
        &fixture.graph,
        &fixture.code_graph,
        &operation,
        next,
        &authorization,
    )
    .await
    .unwrap();
    assert_eq!(
        next_result.effect.unwrap().receipt.outcome,
        EffectTermination::Completed
    );
    assert!(
        fs::read_to_string(fixture.destination_path())
            .unwrap()
            .contains("next_change")
    );
}

#[tokio::test]
async fn malformed_journal_is_preserved_when_obsolete_fields_cannot_be_normalized() {
    let fixture = effect_unknown_fixture().await;
    let durability = SourceEditDurability::for_graph(&fixture.graph);
    let mut invalid: serde_json::Value =
        serde_json::from_slice(&fs::read(durability.journal_path()).unwrap()).unwrap();
    invalid["recovery_files"] = serde_json::json!(["obsolete body"]);
    invalid["unexpected_authority"] = serde_json::json!(true);
    let bytes = serde_json::to_vec(&invalid).unwrap();
    fs::write(durability.journal_path(), &bytes).unwrap();
    assert!(durability.lock().await.is_err());
    assert_eq!(fs::read(durability.journal_path()).unwrap(), bytes);
    fixture.assert_partial();

    let truncated = b"{\"recovery_files\":[";
    fs::write(durability.journal_path(), truncated).unwrap();
    assert!(durability.lock().await.is_err());
    assert_eq!(fs::read(durability.journal_path()).unwrap(), truncated);
    fixture.assert_partial();
}
