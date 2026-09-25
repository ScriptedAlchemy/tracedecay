use serde_json::Value;
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, ComponentVersion, DurableObservationV1, ObservationId,
    ObservationIdentityMaterialV1, ObservationOrderingDomainV1, ObservationScopeV1,
    ObservationSourceCursorV1, ObservationSourceGenerationV1, ObservationSourceIdentityV1,
    ObservationSourceRangeV1, PayloadReferenceV1, RetentionClass, SanitizationReceiptId,
    SanitizationReceiptRefV1, SanitizationReceiptV1, SanitizerDispositionV1, SensitivityV1,
};
use tracedecay_runtime_core::db::engine::params;

use super::{RearmedProjectionRetries, rearm_queued_projection_retries};
use crate::tests::harness::RegisteredGlobalDbHarness;

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

#[tokio::test]
async fn rearm_walks_deferred_retries_in_bounded_batches() {
    let harness = RegisteredGlobalDbHarness::open("rearm-bounded-batches").await;
    seed_queue(&harness, 9, 3).await;
    assert_eq!(queue_state(&harness).await, (12, 9, 12));

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
}

#[tokio::test]
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
        .converge_schema(convergence)
        .await
        .unwrap();
    assert_eq!(
        queue_state(&harness).await,
        (5, 0, 5),
        "background convergence re-arms the fresh mount's queue"
    );
}
