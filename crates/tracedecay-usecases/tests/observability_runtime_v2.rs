use std::sync::Arc;

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
    ObservabilityProducerIdentityV1, RegisteredAggregateShareExporterV1,
    RegisteredObservabilityPortV1,
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
            authorized_scope_ref: scope,
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
    assert_eq!(retrieval_queries.value, Some(100.0));
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
