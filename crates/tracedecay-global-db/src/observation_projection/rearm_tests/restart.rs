use crate::observation_projection::{RearmedProjectionRetries, rearm_queued_projection_retries};
use crate::tests::harness::{RegisteredGlobalDbHarness, writer_telemetry};

use super::{FAR_FUTURE_MICROS, queue_state, seed_queue};

async fn retry_deadlines(harness: &RegisteredGlobalDbHarness) -> Vec<(i64, i64)> {
    let snapshot = harness.registered.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT observation_sequence, next_retry_at_micros
             FROM projection_queue ORDER BY observation_sequence",
            (),
        )
        .await
        .unwrap();
    let mut deadlines = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        deadlines.push((row.get(0).unwrap(), row.get(1).unwrap()));
    }
    deadlines
}

#[asupersync::test]
async fn retry_rearm_resumes_committed_windows_after_failure_and_restart() {
    let harness = RegisteredGlobalDbHarness::open("rearm-interrupted-recovery").await;
    seed_queue(&harness, 9, 0).await;

    // Two windows commit before the third fails. The failing window must
    // roll back as a unit rather than lose its first observation's retry.
    let transaction = harness.registered.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "CREATE TRIGGER reject_retry_window
             BEFORE UPDATE OF next_retry_at_micros ON projection_queue
             WHEN OLD.observation_sequence = 6 AND NEW.next_retry_at_micros = 0
             BEGIN SELECT RAISE(ABORT, 'injected retry window failure'); END",
            (),
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    assert!(
        rearm_queued_projection_retries(harness.registered.runtime_database(), 2)
            .await
            .is_err(),
        "the injected failure must interrupt recovery"
    );

    let interrupted = (1..=9_i64)
        .map(|sequence| {
            (
                sequence,
                if sequence <= 4 { 0 } else { FAR_FUTURE_MICROS },
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(retry_deadlines(&harness).await, interrupted);
    assert_eq!(queue_state(&harness).await, (9, 5, 9));

    let transaction = harness.registered.begin_write_transaction().await.unwrap();
    transaction
        .execute("DROP TRIGGER reject_retry_window", ())
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    let (harness, _convergence) = harness.restart_for_daemon().await;
    assert_eq!(retry_deadlines(&harness).await, interrupted);

    // Make a replay of the committed prefix fail, even if it would write
    // identical values. Row counts alone cannot detect redundant updates.
    let transaction = harness.registered.begin_write_transaction().await.unwrap();
    transaction
        .execute(
            "CREATE TRIGGER reject_committed_retry_replay
             BEFORE UPDATE OF next_retry_at_micros ON projection_queue
             WHEN OLD.observation_sequence <= 4
             BEGIN SELECT RAISE(ABORT, 'committed retry prefix replayed'); END",
            (),
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();

    let before = writer_telemetry(&harness.registered).transactions;
    assert_eq!(
        rearm_queued_projection_retries(harness.registered.runtime_database(), 2)
            .await
            .unwrap(),
        RearmedProjectionRetries {
            rows: 5,
            batches: 3,
        },
        "restart must resume only the five remaining rows in bounded windows"
    );
    let after = writer_telemetry(&harness.registered).transactions;
    assert_eq!(
        after.commands - before.commands,
        3,
        "the final productive window must not be followed by an empty writer lease"
    );
    assert_eq!(queue_state(&harness).await, (9, 0, 9));
    assert_eq!(
        retry_deadlines(&harness).await,
        (1..=9_i64).map(|sequence| (sequence, 0)).collect::<Vec<_>>()
    );
    for _ in 0..2 {
        assert_eq!(
            rearm_queued_projection_retries(harness.registered.runtime_database(), 2)
                .await
                .unwrap(),
            RearmedProjectionRetries::default()
        );
        assert_eq!(writer_telemetry(&harness.registered).transactions, after);
    }

    let transaction = harness.registered.begin_write_transaction().await.unwrap();
    transaction
        .execute("DROP TRIGGER reject_committed_retry_replay", ())
        .await
        .unwrap();
    transaction.commit().await.unwrap();
}
