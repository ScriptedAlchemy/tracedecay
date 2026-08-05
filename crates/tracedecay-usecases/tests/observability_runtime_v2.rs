use std::sync::Arc;
use std::time::Duration;

use tracedecay_application::{
    AggregateShareExportRequestV1, ObservabilityAggregateExportApplicationV1,
    ObservabilityHorizonV1, ObservabilityQueryPort, ObservabilityQueryV1, ObservabilityRecordPort,
};
use tracedecay_domain::{
    AnalyticsModeV1, CoverageStateV1, ObservabilityEnvelopeV1, ObservabilityPayloadV1,
    ObservabilityRetentionClassV1, ObservabilityTerminalResultV1, RetrievalQueryObservedV1,
};
use tracedecay_usecases::observability::{
    BoundedObservabilityProducerV1, ObservabilityEmissionOutcomeV1,
    ObservabilityProducerDeadlinesV1, ObservabilityProducerIdentityV1,
    RegisteredAggregateShareExporterV1, RegisteredObservabilityPortV1,
};

fn envelope(scope: &str, boot: &str, id: u64, event_time_micros: i64) -> ObservabilityEnvelopeV1 {
    let payload = ObservabilityPayloadV1::RetrievalQuery(RetrievalQueryObservedV1 {
        query_family: "exact_technical".into(),
        enabled_lanes: vec!["exact_literal".into()],
        candidate_budget: 10,
        context_budget: 10,
        token_budget: 100,
        answered: true,
        source_coverage: CoverageStateV1::Known,
        lane_coverage: CoverageStateV1::Known,
    });
    ObservabilityEnvelopeV1 {
        event_id: format!("event:{id}"),
        event_kind: payload.event_kind().into(),
        schema_revision: 1,
        idempotency_key: format!("idempotency:{id}"),
        trace_id: format!("trace:{id}"),
        scope_ref: scope.into(),
        capability: "retrieval".into(),
        operation: "query".into(),
        event_time_micros,
        observation_time_micros: event_time_micros,
        valid_from_micros: None,
        valid_until_micros: None,
        quantity: Some(1.0),
        unit: Some("events".into()),
        terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
        producer_revision: "producer.v1".into(),
        configuration_revision: "configuration.v1".into(),
        policy_revision: "policy.v1".into(),
        watermark: format!("{boot}:{id}"),
        coverage: CoverageStateV1::Known,
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::OptionalLocalDetail30d,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count: 0,
        process_boot_id: boot.into(),
        producer_sequence: id,
        payload,
    }
}

async fn runtime() -> (
    tempfile::TempDir,
    tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime,
) {
    let project = tempfile::tempdir().expect("project");
    let project_id =
        tracedecay_domain::ProjectId::new("project.observability.v2").expect("project identifier");
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
        project.path(),
        project_id,
    )
    .await
    .expect("registered runtime");
    (project, runtime)
}

#[tokio::test]
async fn bounded_producer_persists_through_registered_authority_and_cancels_closed() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.v2".to_owned();
    let identity = ObservabilityProducerIdentityV1 {
        authorized_scope_ref: scope.clone(),
        process_boot_id: "boot:producer".into(),
        producer_revision: "producer.v1".into(),
        configuration_revision: "configuration.v1".into(),
        policy_revision: "policy.v1".into(),
    };
    let mut producer =
        BoundedObservabilityProducerV1::start(Arc::clone(&db), identity, 4).expect("producer");

    let mut leaking = envelope(&scope, "boot:producer", 9, 900_000);
    leaking.trace_id = "/private/operator/path".into();
    assert_eq!(
        producer
            .try_emit(leaking)
            .expect_err("private trace rejected"),
        "observability_producer_redaction"
    );
    assert_eq!(
        producer
            .try_emit(envelope(&scope, "boot:producer", 1, 1_000_000))
            .expect("enqueue"),
        ObservabilityEmissionOutcomeV1::Enqueued
    );
    let summary = producer.cancel().await.expect("cancel producer");
    assert!(summary.persisted <= 1);
    assert_eq!(
        producer
            .try_emit(envelope(&scope, "boot:producer", 2, 2_000_000))
            .expect_err("closed producer"),
        "observability_producer_closed"
    );
}

#[tokio::test]
async fn full_producer_queue_reports_drops_through_durable_coverage() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.v2".to_owned();
    let identity = ObservabilityProducerIdentityV1 {
        authorized_scope_ref: scope.clone(),
        process_boot_id: "boot:drops".into(),
        producer_revision: "producer.v1".into(),
        configuration_revision: "configuration.v1".into(),
        policy_revision: "policy.v1".into(),
    };
    let mut producer =
        BoundedObservabilityProducerV1::start(Arc::clone(&db), identity, 1).expect("producer");
    let mut observed_drop = false;
    for id in 1..=256 {
        observed_drop |= producer
            .try_emit(envelope(
                &scope,
                "boot:drops",
                id,
                i64::try_from(id).expect("small id"),
            ))
            .expect("bounded emission")
            == ObservabilityEmissionOutcomeV1::DroppedAtCapacity;
    }
    assert!(observed_drop);
    producer.shutdown().await.expect("shutdown producer");

    let page = RegisteredObservabilityPortV1::new(&db)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: scope.clone(),
            event_kinds: Vec::new(),
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 512,
        })
        .await
        .expect("coverage query");
    assert!(
        page.events.iter().any(|event| {
            event.dropped_count > 0
                || matches!(event.payload, ObservabilityPayloadV1::TelemetryDrop(_))
        }),
        "accepted or control-lane event must expose drops"
    );
}

#[tokio::test]
async fn drops_carried_by_a_later_normal_event_remain_explicit_and_counted() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.v2".to_owned();
    let identity = ObservabilityProducerIdentityV1 {
        authorized_scope_ref: scope.clone(),
        process_boot_id: "boot:carried-drops".into(),
        producer_revision: "producer.v1".into(),
        configuration_revision: "configuration.v1".into(),
        policy_revision: "policy.v1".into(),
    };
    let blocker = db
        .begin_write_transaction()
        .await
        .expect("hold registered writer");
    let mut producer =
        BoundedObservabilityProducerV1::start(Arc::clone(&db), identity, 1).expect("producer");
    assert_eq!(
        producer
            .try_emit(envelope(&scope, "boot:carried-drops", 1, 1))
            .expect("first emission"),
        ObservabilityEmissionOutcomeV1::Enqueued
    );
    tokio::task::yield_now().await;
    assert_eq!(
        producer
            .try_emit(envelope(&scope, "boot:carried-drops", 2, 2))
            .expect("queued emission"),
        ObservabilityEmissionOutcomeV1::Enqueued
    );
    let mut dropped = u64::from(
        producer
            .try_emit(envelope(&scope, "boot:carried-drops", 3, 3))
            .expect("capacity observation")
            == ObservabilityEmissionOutcomeV1::DroppedAtCapacity,
    );
    assert!(dropped > 0, "the held writer must make the data queue full");
    blocker.commit().await.expect("release registered writer");

    let mut next_id = 4_u64;
    let mut later_enqueued = false;
    for _ in 0..1_024 {
        match producer
            .try_emit(envelope(
                &scope,
                "boot:carried-drops",
                next_id,
                i64::try_from(next_id).expect("small event id"),
            ))
            .expect("bounded emission")
        {
            ObservabilityEmissionOutcomeV1::Enqueued => {
                later_enqueued = true;
                break;
            }
            ObservabilityEmissionOutcomeV1::DroppedAtCapacity => {
                dropped = dropped.saturating_add(1);
                next_id = next_id.saturating_add(1);
                tokio::task::yield_now().await;
            }
        }
    }
    assert!(
        later_enqueued,
        "worker must reopen a bounded data slot after the writer is released"
    );
    let summary = producer.shutdown().await.expect("shutdown producer");
    assert_eq!(summary.dropped, dropped);

    let page = RegisteredObservabilityPortV1::new(&db)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: scope.clone(),
            event_kinds: vec!["telemetry.drop.observed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 16,
        })
        .await
        .expect("drop query");
    let explicit = page
        .events
        .iter()
        .find_map(|event| match &event.payload {
            ObservabilityPayloadV1::TelemetryDrop(value) => Some(value),
            _ => None,
        })
        .expect("explicit durable drop range");
    assert_eq!(explicit.proved_drop_lower_bound, dropped);
    assert_eq!(
        explicit
            .last_missing_sequence
            .saturating_sub(explicit.first_missing_sequence)
            .saturating_add(1),
        dropped
    );
    let read_model =
        tracedecay_usecases::observability::observatory_read_model(&db, Some(&scope), 0).await;
    let drop_metric = read_model
        .metrics
        .iter()
        .find(|metric| metric.metric == "telemetry_drops_lower_bound")
        .expect("drop metric");
    assert_eq!(drop_metric.coverage.unknown, dropped);
}

#[tokio::test]
async fn cancellation_is_bounded_when_the_registered_writer_is_blocked() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.v2".to_owned();
    let identity = ObservabilityProducerIdentityV1 {
        authorized_scope_ref: scope.clone(),
        process_boot_id: "boot:deadline".into(),
        producer_revision: "producer.v1".into(),
        configuration_revision: "configuration.v1".into(),
        policy_revision: "policy.v1".into(),
    };
    let blocker = db
        .begin_write_transaction()
        .await
        .expect("hold registered writer");
    let mut producer = BoundedObservabilityProducerV1::start_with_deadlines(
        Arc::clone(&db),
        identity,
        1,
        ObservabilityProducerDeadlinesV1 {
            persistence: Duration::from_millis(50),
            shutdown: Duration::from_millis(250),
        },
    )
    .expect("producer");
    producer
        .try_emit(envelope(&scope, "boot:deadline", 1, 1))
        .expect("enqueue blocked record");
    tokio::task::yield_now().await;

    let cancellation = tokio::time::timeout(Duration::from_millis(500), producer.cancel())
        .await
        .expect("producer cancellation must honor its own database deadline");
    blocker.commit().await.expect("release registered writer");
    let error = cancellation.expect_err("blocked persistence is reported");
    assert!(
        error
            .to_string()
            .contains("observability_persistence_deadline"),
        "unexpected cancellation error: {error}"
    );
}

#[tokio::test]
async fn aggregate_share_export_suppresses_identity_and_small_contributions() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.v2".to_owned();
    let port = RegisteredObservabilityPortV1::new(&db);
    let day_micros = 86_400_000_000_i64;
    for day in 0..100_i64 {
        port.record(envelope(
            &scope,
            "boot:export",
            u64::try_from(day + 1).expect("positive day"),
            day.saturating_mul(day_micros).saturating_add(1),
        ))
        .await
        .expect("record contribution");
    }
    port.record(envelope(&scope, "boot:export", 101, 2))
        .await
        .expect("same-day contribution");

    let exporter = RegisteredAggregateShareExporterV1::new(&db);
    let packet = ObservabilityAggregateExportApplicationV1::new(exporter)
        .export(AggregateShareExportRequestV1 {
            mode: AnalyticsModeV1::AggregateShare,
            authorized_scope_ref: scope,
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: 100_i64.saturating_mul(day_micros),
            },
            max_cells: 16,
        })
        .await
        .expect("aggregate share packet");

    assert!(!packet.cells.is_empty());
    let retrieval_queries = packet
        .cells
        .iter()
        .find(|cell| {
            cell.metric == tracedecay_application::AggregateShareMetricV1::RetrievalQueries
        })
        .expect("retrieval query cell");
    assert_eq!(retrieval_queries.value, Some(101.0));
    let encoded = serde_json::to_string(&packet).expect("encode packet");
    for prohibited in [
        "project.observability.v2",
        "boot:export",
        "trace:",
        "event:",
    ] {
        assert!(!encoded.contains(prohibited));
    }
}

#[tokio::test]
async fn registered_retention_expires_detail_but_preserves_product_receipts() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.v2".to_owned();
    let port = RegisteredObservabilityPortV1::new(&db);
    let mut detail = envelope(&scope, "boot:retention", 1, 1_000_000);
    detail.retention_class = ObservabilityRetentionClassV1::OptionalLocalDetail30d;
    let mut receipt = envelope(&scope, "boot:retention", 2, 1_000_000);
    receipt.retention_class = ObservabilityRetentionClassV1::ProductReceipt;
    let mut rollup = envelope(&scope, "boot:retention", 3, 1_000_000);
    rollup.retention_class = ObservabilityRetentionClassV1::LocalRollup395d;
    port.record(detail).await.expect("detail");
    port.record(receipt).await.expect("receipt");
    port.record(rollup).await.expect("rollup");

    let result = db
        .prune_observability_events(400 * 86_400)
        .await
        .expect("retention");
    assert_eq!(result.expired_detail, 1);
    assert_eq!(result.expired_rollup, 1);

    let page = port
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: scope,
            event_kinds: Vec::new(),
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 10,
        })
        .await
        .expect("retained query");
    assert_eq!(page.events.len(), 1);
    assert_eq!(
        page.events[0].retention_class,
        ObservabilityRetentionClassV1::ProductReceipt
    );
}
