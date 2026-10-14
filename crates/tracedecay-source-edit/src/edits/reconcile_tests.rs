use std::fs;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

use tracedecay_contracts::{
    CancellationSignal, CancellationStage, Deadline, EffectTermination, IdempotencyKey,
    SourceEditAuthorizationAdmissionV1, SourceEditReconciliationDispositionV1,
    SourceEditReconciliationRequestV1, SourceEditRollbackRequestV1, source_edit_operation,
    source_edit_rollback_operation,
};
use tracedecay_domain::UtcMicros;
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::test_support::{
    EffectUnknownFixture, FixtureSourceEditAuthorization, effect_unknown_fixture,
};
use crate::SourceEditOutcome;
use crate::journal::{SourceEditDurability, SourceEditJournalStateV1};
use crate::outcome::SourceEditDurableOutcomeV1;
use crate::{
    SourceEditApplicationResult, SourceEditEffectControlV1, execute_source_edit,
    execute_source_edit_rollback, reconcile_source_edit_effect_unknown_with_control,
};

#[tokio::test]
async fn prepared_restart_with_preimages_restores_partial_bytes_before_another_edit() {
    let fixture = effect_unknown_fixture().await;
    fixture.write_partial_postimage();

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

/// The exact live-verified durability defect: a daemon crash (kill -9) lands
/// AFTER the edit's atomic writes reach disk but BEFORE the journal advances to
/// `Applied`. Recovery must roll the finished edit forward and preserve every
/// written byte, never silently revert it to the durable preimage.
#[tokio::test]
async fn prepared_restart_with_completed_edit_rolls_forward_and_preserves_bytes() {
    let fixture = effect_unknown_fixture().await;
    // The worktree already holds the exact previewed result for every candidate:
    // both files carry their intended post-edit content, as they would after a
    // crash that interrupted only the durable bookkeeping.
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

    // Roll forward: the completed edit is finalized, not reverted.
    assert!(result.replayed);
    assert_eq!(
        result.effect.unwrap().receipt.outcome,
        EffectTermination::Completed
    );
    // Every written byte is preserved on disk.
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

async fn roll_back(
    fixture: &EffectUnknownFixture,
    recovered: &SourceEditApplicationResult,
) -> SourceEditApplicationResult {
    let effect = recovered.effect.as_ref().unwrap();
    let request = SourceEditRollbackRequestV1 {
        context: fixture.request.context.clone(),
        authority: fixture.request.authority.clone(),
        effect_id: effect.effect_id.clone(),
        original_idempotency_key: fixture.request.idempotency_key.clone(),
        idempotency_key: IdempotencyKey::new("source-edit.recovery-fixture.rollback").unwrap(),
        original_input_digest: effect.receipt.input_digest.clone(),
        expected_state: effect.receipt.committed_state.clone().unwrap(),
        proof: fixture.request.proof.clone(),
        observed_at: fixture.request.observed_at,
    };
    let authorization = FixtureSourceEditAuthorization(
        SourceEditAuthorizationAdmissionV1::new(
            request.authority.clone(),
            request.proof.clone(),
            request.context.scope(),
        )
        .unwrap(),
    );
    execute_source_edit_rollback(
        &fixture.graph,
        &source_edit_rollback_operation().unwrap(),
        request,
        &authorization,
    )
    .await
    .unwrap()
}

/// A MoveSymbol rolled forward from `Prepared` keeps its exact preimages as
/// rollback material, so the supported rollback restores both files.
#[tokio::test]
async fn prepared_roll_forward_retains_move_symbol_rollback_material() {
    let fixture = effect_unknown_fixture().await;
    fixture.write_all_postimages();

    let recovered = recover_by_replaying(&fixture).await.unwrap();
    assert_eq!(
        recovered.effect.as_ref().unwrap().receipt.outcome,
        EffectTermination::Completed
    );
    let replayed = recover_by_replaying(&fixture).await.unwrap();
    assert!(replayed.replayed);
    assert_eq!(replayed.effect, recovered.effect);
    fixture.assert_postimages();

    let rolled_back = roll_back(&fixture, &recovered).await;
    assert_eq!(
        rolled_back.effect.as_ref().unwrap().receipt.outcome,
        EffectTermination::Completed
    );
    fixture.assert_preimages();
    let rollback_replay = roll_back(&fixture, &recovered).await;
    assert!(rollback_replay.replayed);
    fixture.assert_preimages();
}

#[tokio::test]
async fn prepared_roll_forward_rollback_refuses_foreign_bytes() {
    let fixture = effect_unknown_fixture().await;
    fixture.write_all_postimages();
    let recovered = recover_by_replaying(&fixture).await.unwrap();
    let foreign = b"pub fn existing() {}\n\npub fn moved() { foreign(); }\n";
    std::fs::write(fixture.destination_path(), foreign).unwrap();

    let refused = roll_back(&fixture, &recovered).await;

    assert_eq!(
        refused.effect.as_ref().unwrap().receipt.outcome,
        EffectTermination::Failed
    );
    assert_eq!(std::fs::read(fixture.destination_path()).unwrap(), foreign);
}

/// Recovery that cannot retain the rollback material must not publish success
/// or clear the journal holding the only preimages; a later retry finishes.
#[tokio::test]
async fn prepared_roll_forward_keeps_journal_when_rollback_material_cannot_persist() {
    let fixture = effect_unknown_fixture().await;
    fixture.write_all_postimages();
    let store = fixture.transaction_store();
    let obstruction = store.join("rollback-records");
    std::fs::write(&obstruction, b"not a directory").unwrap();
    let receipts =
        || std::fs::read_dir(store.join("receipts")).map_or(0, |entries| entries.count());
    let receipts_before = receipts();

    assert!(
        recover_by_replaying(&fixture).await.is_err(),
        "recovery must fail while rollback material cannot persist"
    );
    assert!(store.join("active.json").exists());
    assert_eq!(receipts(), receipts_before);
    fixture.assert_postimages();

    std::fs::remove_file(&obstruction).unwrap();
    let recovered = recover_by_replaying(&fixture).await.unwrap();
    assert_eq!(
        recovered.effect.as_ref().unwrap().receipt.outcome,
        EffectTermination::Completed
    );
    assert!(!store.join("active.json").exists());
    roll_back(&fixture, &recovered).await;
    fixture.assert_preimages();
}

/// The write never landed: after the fixture the worktree still holds every
/// preimage. Recovery rolls cleanly back (a no-op restore) and records the
/// failure without disturbing any byte.
#[tokio::test]
async fn prepared_restart_with_untouched_preimages_rolls_back_cleanly() {
    let fixture = effect_unknown_fixture().await;

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
    let effect = fixture.result.effect.as_ref().unwrap();
    let reconciliation = SourceEditReconciliationRequestV1 {
        context: fixture.request.context.clone(),
        authority: fixture.request.authority.clone(),
        kind: fixture.request.edit.kind(),
        effect_id: effect.effect_id.clone(),
        idempotency_key: fixture.request.idempotency_key.clone(),
        attempt_idempotency_key: IdempotencyKey::new("source-edit-reconciliation-attempt.fixture")
            .unwrap(),
        input_digest: effect.receipt.input_digest.clone(),
        disposition: SourceEditReconciliationDispositionV1::ConfirmRolledBack,
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

async fn assert_rollback_replays(
    fixture: &EffectUnknownFixture,
    recovered: &SourceEditApplicationResult,
) {
    let rolled_back = roll_back(fixture, recovered).await;
    assert_eq!(
        rolled_back.effect.as_ref().unwrap().receipt.outcome,
        EffectTermination::Completed
    );
    fixture.assert_preimages();
    let replay = roll_back(fixture, recovered).await;
    assert!(replay.replayed);
    assert_eq!(replay.effect, rolled_back.effect);
    fixture.assert_preimages();
}

#[tokio::test]
async fn prepared_roll_forward_keeps_the_inverse_when_receipt_publication_fails() {
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
    let journal = durability.load_journal().unwrap().unwrap();
    assert!(
        durability
            .load_rollback_record(&journal.effect_id)
            .unwrap()
            .is_some()
    );

    let recovered = recover_by_replaying(&fixture).await.unwrap();
    assert!(!journal_path.exists());
    assert_rollback_replays(&fixture, &recovered).await;
}

#[tokio::test]
async fn prepared_roll_forward_keeps_the_inverse_when_journal_cleanup_fails() {
    let fixture = effect_unknown_fixture().await;
    fixture.write_all_postimages();
    let durability = SourceEditDurability::for_graph(&fixture.graph);
    let journal_path = durability.journal_path();
    let before = fs::read(&journal_path).unwrap();
    let root = journal_path.parent().unwrap();
    fs::create_dir_all(root.join("rollback-records")).unwrap();
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
    let journal = durability.load_journal().unwrap().unwrap();
    assert!(
        durability
            .load_rollback_record(&journal.effect_id)
            .unwrap()
            .is_some()
    );
    let receipt = durability
        .load_receipt(&fixture.request.idempotency_key)
        .unwrap()
        .unwrap();
    assert_eq!(receipt.effect.receipt.outcome, EffectTermination::Completed);

    let recovered = recover_by_replaying(&fixture).await.unwrap();
    assert!(!journal_path.exists());
    assert_rollback_replays(&fixture, &recovered).await;
}

#[tokio::test]
async fn applied_restart_retains_the_exact_move_inverse() {
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
    assert_rollback_replays(&fixture, &recovered).await;
}
