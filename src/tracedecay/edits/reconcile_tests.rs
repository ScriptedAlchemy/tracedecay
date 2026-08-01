use std::fs;

use tracedecay_application::{
    CancellationSignal, CancellationStage, Deadline, EffectTermination, IdempotencyKey,
    SourceEditReconciliationDispositionV1, SourceEditReconciliationRequestV1,
    source_edit_operation,
};
use tracedecay_domain::UtcMicros;

use super::test_support::effect_unknown_fixture;
use crate::application::edit::{
    SourceEditEffectControlV1, execute_source_edit,
    reconcile_source_edit_effect_unknown_with_control,
};

#[tokio::test]
async fn prepared_restart_with_preimages_restores_partial_bytes_before_another_edit() {
    let fixture = effect_unknown_fixture().await;
    fs::write(
        fixture.project.path().join("src/a.rs"),
        b"pub fn new_a() {}\n",
    )
    .unwrap();

    let operation = source_edit_operation(fixture.request.edit.kind()).unwrap();
    let result = execute_source_edit(
        &fixture.graph,
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
    assert_eq!(
        fs::read(fixture.project.path().join("src/a.rs")).unwrap(),
        b"pub fn old_a() {}\n"
    );
    assert_eq!(
        fs::read(fixture.project.path().join("src/locked/b.rs")).unwrap(),
        b"pub fn old_b() {}\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(fixture.project.path().join("src/a.rs"))
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
/// written byte — never silently revert it to the durable preimage.
#[tokio::test]
async fn prepared_restart_with_completed_edit_rolls_forward_and_preserves_bytes() {
    let fixture = effect_unknown_fixture().await;
    // The worktree already holds the exact previewed result for every candidate:
    // both files carry their intended post-edit content, as they would after a
    // crash that interrupted only the durable bookkeeping.
    fs::write(
        fixture.project.path().join("src/a.rs"),
        b"pub fn new_a() {}\n",
    )
    .unwrap();
    fs::write(
        fixture.project.path().join("src/locked/b.rs"),
        b"pub fn new_b() {}\n",
    )
    .unwrap();

    let operation = source_edit_operation(fixture.request.edit.kind()).unwrap();
    let result = execute_source_edit(
        &fixture.graph,
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
    assert_eq!(
        fs::read(fixture.project.path().join("src/a.rs")).unwrap(),
        b"pub fn new_a() {}\n"
    );
    assert_eq!(
        fs::read(fixture.project.path().join("src/locked/b.rs")).unwrap(),
        b"pub fn new_b() {}\n"
    );
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
    assert_eq!(
        fs::read(fixture.project.path().join("src/a.rs")).unwrap(),
        b"pub fn old_a() {}\n"
    );
    assert_eq!(
        fs::read(fixture.project.path().join("src/locked/b.rs")).unwrap(),
        b"pub fn old_b() {}\n"
    );
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
    assert_eq!(
        fs::read(fixture.project.path().join("src/a.rs")).unwrap(),
        b"pub fn old_a() {}\n"
    );
    assert_eq!(
        fs::read(fixture.project.path().join("src/locked/b.rs")).unwrap(),
        b"pub fn old_b() {}\n"
    );

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
