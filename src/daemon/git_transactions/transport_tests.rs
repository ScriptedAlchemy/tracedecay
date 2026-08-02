use std::sync::atomic::{AtomicUsize, Ordering};

use tracedecay_application::{
    GitIndexTransactionPort, GitIndexTransactionPortError, OperationTermination,
};
use tracedecay_domain::{GitIndexReceiptOutcomeV1, GitIndexTransactionOperationV1, UtcMicros};

use super::test_support::{NativeMode, RecoveryMode, digest, test_port_from_preview};

const EFFECTS: [GitIndexTransactionOperationV1; 3] = [
    GitIndexTransactionOperationV1::StageHunks,
    GitIndexTransactionOperationV1::UnstageHunks,
    GitIndexTransactionOperationV1::CommitIndex,
];

#[test]
fn preview_apply_owner_mounts_all_git_effects_and_replays() {
    for operation in EFFECTS {
        let harness = test_port_from_preview(
            operation,
            [NativeMode::Completed(GitIndexReceiptOutcomeV1::Committed)],
            [],
        );
        assert_eq!(harness.preview.operation, operation);
        let first = harness
            .port
            .apply(&harness.request)
            .expect("effect reaches the preview owner");
        assert_eq!(first.receipt.operation, operation);
        assert_eq!(first.receipt.outcome, GitIndexReceiptOutcomeV1::Committed);

        let replay = harness
            .port
            .apply(&harness.request)
            .expect("same input replays the durable receipt");
        assert_eq!(replay.receipt, first.receipt);
        assert_eq!(harness.apply_calls.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn preview_apply_owner_rejects_a_conflicting_preview_cas() {
    for operation in EFFECTS {
        let harness = test_port_from_preview(
            operation,
            [NativeMode::Completed(GitIndexReceiptOutcomeV1::Committed)],
            [],
        );
        harness
            .port
            .apply(&harness.request)
            .expect("seed durable receipt for the CAS check");
        let mut conflicting = harness.request.clone();
        conflicting.preview_digest = digest('f');
        assert_eq!(
            harness.port.apply(&conflicting),
            Err(GitIndexTransactionPortError::IdempotencyConflict)
        );
        assert_eq!(harness.apply_calls.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn preview_apply_owner_honors_cancellation_before_native_entry() {
    for operation in EFFECTS {
        let harness = test_port_from_preview(operation, [], []);
        let checks = AtomicUsize::new(0);
        let result = harness
            .port
            .apply_cancellable(&harness.request, || {
                (checks.fetch_add(1, Ordering::SeqCst) + 1 >= 5).then_some(UtcMicros(25))
            })
            .expect("cancellation is a durable no-change result");
        assert_eq!(
            result.receipt.outcome,
            GitIndexReceiptOutcomeV1::AbortedNoChange
        );
        assert_eq!(
            result.execution.termination,
            OperationTermination::Cancelled
        );
        assert_eq!(harness.apply_calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn preview_apply_owner_recovers_an_ambiguous_boundary_without_replay() {
    for operation in EFFECTS {
        let harness = test_port_from_preview(
            operation,
            [NativeMode::CommitBoundaryUnknown],
            [RecoveryMode::AbortedNoChange],
        );
        let result = harness
            .port
            .apply(&harness.request)
            .expect("recovery result");
        assert_eq!(
            result.receipt.outcome,
            GitIndexReceiptOutcomeV1::AbortedNoChange
        );
        assert_eq!(harness.apply_calls.load(Ordering::SeqCst), 1);
        assert_eq!(harness.recovery_calls.load(Ordering::SeqCst), 1);
    }
}
