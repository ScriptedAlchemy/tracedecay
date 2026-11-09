use serde_json::Value;
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, ComponentVersion, DurableObservationV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, RetentionClass, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
};
use tracedecay_runtime_core::db::engine::params;

use super::state::{
    apply_rearm_projection_retry_candidates, read_rearm_projection_retry_candidates,
};
use super::{RearmedProjectionRetries, rearm_queued_projection_retries};
use crate::tests::harness::{RegisteredGlobalDbHarness, writer_telemetry};

const FAR_FUTURE_MICROS: i64 = 9_000_000_000_000_000;

fn queued_observation(index: i64) -> (DurableObservationV1, ObservationSourceCursorV1) {
    let record_id = format!("record.rearm-{index}");
    let session_id = format!("rearm-{index}");
    let mut fixture: Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/provider_normalization/codex/session_meta.expected_envelope.json"
    ))
    .unwrap();
    fixture["stable_record_id"] = Value::String(record_id.clone());
    fixture["relations"]["session_id"] = Value::String(session_id.clone());
    fixture["relations"]["thread_id"] = Value::String(session_id);
    let envelope: CanonicalObservationEnvelopeV1 = serde_json::from_value(fixture).unwrap();
    let source = ObservationSourceIdentityV1::for_provider(
        envelope.provider().clone(),
        envelope.relations().session_id().clone(),
    )
    .unwrap();
    let payload = serde_json::to_value(envelope).unwrap();
    let receipt = SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            SanitizationReceiptId::new(format!("receipt.rearm-{index}")).unwrap(),
            ComponentVersion::new("sanitizer.rearm.v1").unwrap(),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&payload).unwrap()),
    )
    .unwrap();
    let generation = ObservationSourceGenerationV1::new(1).unwrap();
    let observation = DurableObservationV1::new(
        ObservationIdentityMaterialV1::for_native_record(
            source.clone(),
            ObservationScopeV1::Profile,
            generation,
            ObservationSourceRangeV1::new(0, 100).unwrap(),
            ObservationOrderingDomainV1::FileBytes,
            ObservationId::new(record_id).unwrap(),
        )
        .unwrap(),
        receipt,
        RetentionClass::new("retention.rearm").unwrap(),
        payload,
    )
    .unwrap();
    let cursor = ObservationSourceCursorV1::for_ordering(
        source,
        ObservationScopeV1::Profile,
        generation,
        ObservationOrderingDomainV1::FileBytes,
        100,
    )
    .unwrap();
    (observation, cursor)
}

/// Queues `deferred` rows with a pending backoff and `due` rows already
/// eligible, interleaved so batches must skip due rows.
async fn seed_queue(harness: &RegisteredGlobalDbHarness, deferred: i64, due: i64) {
    let transaction = harness.registered.begin_write_transaction().await.unwrap();
    for index in 0..deferred + due {
        let (observation, cursor) = queued_observation(index);
        let receipt = observation.receipt();
        transaction
            .execute(
                "INSERT INTO sanitization_receipts
                 (receipt_id, sanitizer_version, payload_digest, receipt_json)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    receipt.receipt().receipt_id().as_str(),
                    receipt.receipt().sanitizer_version().as_str(),
                    observation.payload_reference().digest().as_str(),
                    serde_json::to_string(receipt).unwrap()
                ],
            )
            .await
            .unwrap();
        transaction
            .execute(
                "INSERT INTO observations
                 (observation_id, payload_digest, receipt_id, observation_json,
                  committed_cursor_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    observation.observation_id().as_str(),
                    observation.payload_reference().digest().as_str(),
                    receipt.receipt().receipt_id().as_str(),
                    serde_json::to_string(&observation).unwrap(),
                    serde_json::to_string(&cursor).unwrap()
                ],
            )
            .await
            .unwrap();
        transaction
            .execute(
                "INSERT INTO source_cursors(source_json, scope_json, cursor_json)
                 VALUES (?1, ?2, ?3)",
                params![
                    serde_json::to_string(cursor.source()).unwrap(),
                    serde_json::to_string(cursor.scope()).unwrap(),
                    serde_json::to_string(&cursor).unwrap()
                ],
            )
            .await
            .unwrap();
        let next_retry = if index % 2 == 1 && index / 2 < due {
            0
        } else {
            FAR_FUTURE_MICROS
        };
        transaction
            .execute(
                "INSERT INTO projection_queue
                 (observation_id, observation_sequence, attempt_count, next_retry_at_micros,
                  last_error)
                 SELECT observation_id, sequence, 3, ?2, 'recorded failure'
                 FROM observations WHERE observation_id = ?1",
                params![observation.observation_id().as_str(), next_retry],
            )
            .await
            .unwrap();
    }
    transaction.commit().await.unwrap();
}

async fn queue_state(harness: &RegisteredGlobalDbHarness) -> (i64, i64, i64) {
    let snapshot = harness.registered.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT COUNT(*),
                    SUM(next_retry_at_micros > 0),
                    SUM(attempt_count = 3 AND last_error = 'recorded failure')
             FROM projection_queue",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    (
        row.get(0).unwrap(),
        row.get(1).unwrap(),
        row.get(2).unwrap(),
    )
}

#[asupersync::test]
async fn rearm_writer_work_scales_with_deferred_rows() {
    async fn writer_steps(rows: i64) -> u64 {
        let harness = RegisteredGlobalDbHarness::open("rearm-dense-queue").await;
        seed_queue(&harness, rows, 0).await;
        let steps = || writer_telemetry(&harness.registered).sqlite_vm.vm_steps;
        let before = steps();
        let rearmed = rearm_queued_projection_retries(harness.registered.runtime_database(), 4)
            .await
            .unwrap();
        assert_eq!(rearmed.rows, u64::try_from(rows).unwrap());
        steps() - before
    }

    let base = writer_steps(32).await;
    let grown = writer_steps(256).await;
    assert!(base > 0);
    assert!(
        grown < base * 12,
        "an eightfold larger dense queue must not rescan its remainder per window: {base} -> {grown}"
    );
}

#[asupersync::test]
async fn rearm_walks_deferred_retries_in_bounded_batches() {
    let harness = RegisteredGlobalDbHarness::open("rearm-bounded-batches").await;
    seed_queue(&harness, 9, 3).await;
    assert_eq!(queue_state(&harness).await, (12, 9, 12));
    let before = writer_telemetry(&harness.registered).transactions;

    let rearmed = rearm_queued_projection_retries(harness.registered.runtime_database(), 4)
        .await
        .unwrap();

    assert_eq!(
        rearmed,
        RearmedProjectionRetries {
            rows: 9,
            batches: 3,
        },
        "nine deferred rows re-arm in ceil(9 / 4) statements, never one queue-sized one"
    );
    // The exact-SQL writer counts one dispatched command per leased transaction,
    // including a rollback. The persisted queue below proves the windows committed.
    let after = writer_telemetry(&harness.registered).transactions;
    assert_eq!(after.commands - before.commands, 3);
    assert_eq!(
        queue_state(&harness).await,
        (12, 0, 12),
        "every deadline clears while attempt history and last errors persist"
    );
    assert_eq!(
        rearm_queued_projection_retries(harness.registered.runtime_database(), 4)
            .await
            .unwrap(),
        RearmedProjectionRetries::default(),
        "an armed queue costs no write batches"
    );
    assert_eq!(
        writer_telemetry(&harness.registered).transactions,
        after,
        "zero reported batches must also mean zero actual writer transactions"
    );
}

#[asupersync::test]
async fn expired_retry_deadlines_survive_restarts_without_rearm_writes() {
    let harness = RegisteredGlobalDbHarness::open("rearm-expired-deadlines").await;
    seed_queue(&harness, 8, 0).await;

    // These positive deadlines have already elapsed. Normal projection may
    // consume them directly; a restart must not rewrite the entire overdue
    // queue just to change each deadline to zero.
    let transaction = harness.registered.begin_write_transaction().await.unwrap();
    assert_eq!(
        transaction
            .execute("UPDATE projection_queue SET next_retry_at_micros = 1", ())
            .await
            .unwrap(),
        8
    );
    transaction.commit().await.unwrap();

    let (harness, _convergence) = harness.restart_for_daemon().await;
    let before = writer_telemetry(&harness.registered).transactions;
    for _ in 0..2 {
        assert_eq!(
            rearm_queued_projection_retries(harness.registered.runtime_database(), 2)
                .await
                .unwrap(),
            RearmedProjectionRetries::default(),
            "expired retries already admit projection and need no restart writes"
        );
        assert_eq!(
            writer_telemetry(&harness.registered).transactions,
            before,
            "an overdue-only queue must not acquire any SQLite writer"
        );
    }
    assert_eq!(queue_state(&harness).await, (8, 8, 8));

    // Interleave actual deferred retries and a terminal collision. Each
    // bounded transaction must touch just its one deferred row, preserving
    // the other deadlines and the retry history.
    let transaction = harness.registered.begin_write_transaction().await.unwrap();
    assert_eq!(
        transaction
            .execute(
                "UPDATE projection_queue SET next_retry_at_micros = ?1
                 WHERE observation_sequence IN (2, 6)",
                params![FAR_FUTURE_MICROS],
            )
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        transaction
            .execute(
                "UPDATE projection_queue SET next_retry_at_micros = ?1
                 WHERE observation_sequence = 4",
                params![i64::MAX],
            )
            .await
            .unwrap(),
        1
    );
    transaction.commit().await.unwrap();

    let before = writer_telemetry(&harness.registered).transactions;
    assert_eq!(
        rearm_queued_projection_retries(harness.registered.runtime_database(), 1)
            .await
            .unwrap(),
        RearmedProjectionRetries {
            rows: 2,
            batches: 2,
        }
    );
    let after = writer_telemetry(&harness.registered).transactions;
    assert_eq!(after.commands - before.commands, 2);
    assert_eq!(queue_state(&harness).await, (8, 6, 8));

    let snapshot = harness.registered.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT observation_sequence, next_retry_at_micros
             FROM projection_queue ORDER BY observation_sequence",
            (),
        )
        .await
        .unwrap();
    for sequence in 1..=8_i64 {
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), sequence);
        let deadline = row.get::<i64>(1).unwrap();
        assert_eq!(
            deadline,
            if sequence == 4 {
                i64::MAX
            } else if sequence == 2 || sequence == 6 {
                0
            } else {
                1
            },
            "rearm must not rewrite expired or terminal deadlines"
        );
    }
    assert!(rows.next().await.unwrap().is_none());
}

#[asupersync::test]
async fn rearm_ignores_parked_terminal_collisions_without_empty_write_batches() {
    let harness = RegisteredGlobalDbHarness::open("rearm-terminal-collisions").await;
    seed_queue(&harness, 9, 3).await;

    // Six permanently parked collisions are interleaved with three deferred
    // retries and three already-due rows. Neither parked nor due entries may
    // consume a restart re-arm write window.
    let transaction = harness.registered.begin_write_transaction().await.unwrap();
    assert_eq!(
        transaction
            .execute(
                "UPDATE projection_queue SET next_retry_at_micros = ?1
                 WHERE observation_sequence IN (1, 3, 5, 7, 9, 11)",
                params![i64::MAX],
            )
            .await
            .unwrap(),
        6
    );
    transaction.commit().await.unwrap();
    assert_eq!(queue_state(&harness).await, (12, 9, 12));
    let before = writer_telemetry(&harness.registered).transactions;

    let rearmed = rearm_queued_projection_retries(harness.registered.runtime_database(), 2)
        .await
        .unwrap();
    assert_eq!(
        rearmed,
        RearmedProjectionRetries {
            rows: 3,
            batches: 2,
        },
        "parked collisions must not consume re-arm write windows"
    );
    let after = writer_telemetry(&harness.registered).transactions;
    assert_eq!(after.commands - before.commands, 2);
    assert_eq!(
        queue_state(&harness).await,
        (12, 6, 12),
        "only retryable deadlines clear; terminal collisions remain parked"
    );

    // Reopen the durable database through real daemon admission. Do not run
    // background convergence before measuring its retry re-arm directly.
    let (harness, _convergence) = harness.restart_for_daemon().await;
    assert_eq!(queue_state(&harness).await, (12, 6, 12));
    let before = writer_telemetry(&harness.registered).transactions;
    for _ in 0..2 {
        assert_eq!(
            rearm_queued_projection_retries(harness.registered.runtime_database(), 2)
                .await
                .unwrap(),
            RearmedProjectionRetries::default(),
            "restarts with only terminal collisions cannot schedule empty write batches"
        );
        assert_eq!(writer_telemetry(&harness.registered).transactions, before);
    }

    // An idle observation must not cache away a subsequently deferred row.
    let transaction = harness.registered.begin_write_transaction().await.unwrap();
    assert_eq!(
        transaction
            .execute(
                "UPDATE projection_queue SET next_retry_at_micros = ?1
                 WHERE observation_sequence = 2",
                params![FAR_FUTURE_MICROS],
            )
            .await
            .unwrap(),
        1
    );
    transaction.commit().await.unwrap();
    let before = writer_telemetry(&harness.registered).transactions;
    assert_eq!(
        rearm_queued_projection_retries(harness.registered.runtime_database(), 2)
            .await
            .unwrap(),
        RearmedProjectionRetries {
            rows: 1,
            batches: 1,
        }
    );
    let after = writer_telemetry(&harness.registered).transactions;
    assert_eq!(after.commands - before.commands, 1);
    assert_eq!(queue_state(&harness).await, (12, 6, 12));
}

#[asupersync::test]
async fn rearm_rejects_nonpositive_windows_before_acquiring_the_writer() {
    let harness = RegisteredGlobalDbHarness::open("rearm-invalid-window").await;
    seed_queue(&harness, 5, 0).await;
    let before = writer_telemetry(&harness.registered).transactions;
    for batch_rows in [0, -1] {
        assert!(
            rearm_queued_projection_retries(harness.registered.runtime_database(), batch_rows)
                .await
                .is_err(),
            "a nonpositive limit must not silently skip or unbound recovery"
        );
        assert_eq!(queue_state(&harness).await, (5, 5, 5));
        assert_eq!(writer_telemetry(&harness.registered).transactions, before);
    }
    assert_eq!(
        rearm_queued_projection_retries(harness.registered.runtime_database(), 2)
            .await
            .unwrap(),
        RearmedProjectionRetries {
            rows: 5,
            batches: 3,
        }
    );
    let after = writer_telemetry(&harness.registered).transactions;
    assert_eq!(after.commands - before.commands, 3);
    assert_eq!(queue_state(&harness).await, (5, 0, 5));
}

#[asupersync::test]
async fn sparse_retry_rearm_writer_work_is_bounded_by_updated_rows() {
    async fn writer_steps(prefix: i64, fixture: &'static str) -> u64 {
        let harness = RegisteredGlobalDbHarness::open(fixture).await;
        seed_queue(&harness, prefix + 4, 0).await;
        let transaction = harness.registered.begin_write_transaction().await.unwrap();
        assert_eq!(
            transaction
                .execute(
                    "UPDATE projection_queue SET next_retry_at_micros = 0
                     WHERE observation_sequence <= ?1",
                    params![prefix],
                )
                .await
                .unwrap(),
            u64::try_from(prefix).unwrap()
        );
        transaction.commit().await.unwrap();
        let before = writer_telemetry(&harness.registered);
        let rearmed = rearm_queued_projection_retries(harness.registered.runtime_database(), 4)
            .await
            .unwrap();
        assert_eq!(
            rearmed,
            RearmedProjectionRetries {
                rows: 4,
                batches: 1,
            }
        );
        let after = writer_telemetry(&harness.registered);
        assert_eq!(after.transactions.commands - before.transactions.commands, 1);
        assert_eq!(queue_state(&harness).await.1, 0);
        after.sqlite_vm.vm_steps - before.sqlite_vm.vm_steps
    }

    let small = writer_steps(32, "rearm-sparse-small").await;
    let large = writer_steps(256, "rearm-sparse-large").await;
    assert!(small > 0);
    assert!(
        large < small * 2,
        "eight times as many inert queue rows must not multiply SQLite writer work: {small} -> {large}"
    );
}

#[asupersync::test]
async fn rearm_reader_snapshot_cannot_clear_a_new_retry_generation() {
    let harness = RegisteredGlobalDbHarness::open("rearm-stale-candidates").await;
    seed_queue(&harness, 3, 0).await;
    let now = 1_000_000_i64;
    let (last, candidates) = read_rearm_projection_retry_candidates(
        &harness.registered.runtime_database().read_connection(),
        0,
        3,
        3,
        now,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(last, 3);

    // The reader's window is now stale: a foreground owner schedules a new
    // generation for one row and another row has become due.
    let transaction = harness.registered.begin_write_transaction().await.unwrap();
    assert_eq!(
        transaction
            .execute(
                "UPDATE projection_queue SET attempt_count = attempt_count + 1
                 WHERE observation_sequence = 2",
                (),
            )
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        transaction
            .execute(
                "UPDATE projection_queue SET next_retry_at_micros = 1
                 WHERE observation_sequence = 3",
                (),
            )
            .await
            .unwrap(),
        1
    );
    transaction.commit().await.unwrap();

    assert_eq!(
        apply_rearm_projection_retry_candidates(
            harness.registered.runtime_database(),
            &candidates,
            now,
        )
        .await
        .unwrap(),
        1,
        "only the unchanged candidate can be reset"
    );
    let snapshot = harness.registered.read_snapshot().await.unwrap();
    let mut rows = snapshot
        .query(
            "SELECT observation_sequence, attempt_count, next_retry_at_micros
             FROM projection_queue ORDER BY observation_sequence",
            (),
        )
        .await
        .unwrap();
    for (sequence, attempts, deadline) in [
        (1_i64, 3_i64, 0_i64),
        (2, 4, FAR_FUTURE_MICROS),
        (3, 3, 1),
    ] {
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), sequence);
        assert_eq!(row.get::<i64>(1).unwrap(), attempts);
        assert_eq!(row.get::<i64>(2).unwrap(), deadline);
    }
    assert!(rows.next().await.unwrap().is_none());
}

#[asupersync::test]
async fn daemon_admission_defers_rearm_to_background_convergence() {
    let harness = RegisteredGlobalDbHarness::open("rearm-daemon-admission").await;
    seed_queue(&harness, 5, 0).await;

    let (harness, convergence) = harness.restart_for_daemon().await;
    assert_eq!(
        queue_state(&harness).await,
        (5, 5, 5),
        "daemon admission must not pay the queue-sized re-arm"
    );

    harness
        .registered
        .converge_schema(
            convergence,
            &tracedecay_runtime_core::cancellation::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        queue_state(&harness).await,
        (5, 0, 5),
        "background convergence re-arms the fresh mount's queue"
    );
}
