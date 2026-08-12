use std::sync::{Arc, Barrier};
use std::time::Duration;

use tracedecay_application::{
    AggregateShareExportRequestV1, ObservabilityAggregateExportApplicationV1,
    ObservabilityHorizonV1, ObservabilityQueryPort, ObservabilityQueryV1, ObservabilityRecordPort,
    now_micros,
};
use tracedecay_domain::{
    AnalyticsModeV1, CoverageStateV1, ObservabilityEnvelopeV1, ObservabilityPayloadV1,
    ObservabilityRetentionClassV1, ObservabilityTerminalResultV1, RetrievalQueryObservedV1,
    TelemetryDropObservedV1,
};
use tracedecay_usecases::observability::{
    BoundedObservabilityProducerV1, ObservabilityEmissionOutcomeV1,
    ObservabilityOwnerEmissionOutcomeV1, ObservabilityProducerDeadlinesV1,
    ObservabilityProducerIdentityV1, RegisteredAggregateShareExporterV1,
    RegisteredObservabilityPortV1, provider_latency_read_model,
};
use tracedecay_usecases::provider_usage::{
    AggregatedProviderUsageCountersV1, ProviderUsageAggregateV1, ProviderUsageCoverageV1,
};

fn identity(scope: &str, boot: &str) -> ObservabilityProducerIdentityV1 {
    ObservabilityProducerIdentityV1 {
        authorized_scope_ref: scope.to_owned(),
        process_boot_id: boot.to_owned(),
        producer_revision: "producer.v1".to_owned(),
        configuration_revision: "configuration.v1".to_owned(),
        policy_revision: "policy.v1".to_owned(),
    }
}

fn envelope(scope: &str) -> ObservabilityEnvelopeV1 {
    let payload = ObservabilityPayloadV1::RetrievalQuery(RetrievalQueryObservedV1 {
        query_family: "exact_technical".to_owned(),
        enabled_lanes: vec!["exact_literal".to_owned()],
        candidate_budget: 10,
        context_budget: 10,
        token_budget: 100,
        answered: true,
        source_coverage: CoverageStateV1::Known,
        lane_coverage: CoverageStateV1::Known,
    });
    ObservabilityEnvelopeV1 {
        event_id: "event:active".to_owned(),
        event_kind: payload.event_kind().to_owned(),
        schema_revision: 1,
        idempotency_key: "idempotency:active".to_owned(),
        trace_id: "trace:active".to_owned(),
        scope_ref: scope.to_owned(),
        capability: "retrieval".to_owned(),
        operation: "query".to_owned(),
        event_time_micros: 1,
        observation_time_micros: 1,
        valid_from_micros: None,
        valid_until_micros: None,
        quantity: Some(1.0),
        unit: Some("events".to_owned()),
        terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
        producer_revision: "caller.v1".to_owned(),
        configuration_revision: "caller.v1".to_owned(),
        policy_revision: "caller.v1".to_owned(),
        watermark: "caller:1".to_owned(),
        coverage: CoverageStateV1::Known,
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::OptionalLocalDetail30d,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count: 0,
        process_boot_id: "boot:caller".to_owned(),
        producer_sequence: 1,
        payload,
    }
}

fn drop_receipt(
    scope: &str,
    boot: &str,
    sequence: u64,
    lower_bound: u64,
    clean: bool,
    event_time_micros: i64,
) -> ObservabilityEnvelopeV1 {
    let payload = ObservabilityPayloadV1::TelemetryDrop(TelemetryDropObservedV1 {
        first_missing_sequence: sequence,
        last_missing_sequence: sequence,
        proved_drop_lower_bound: lower_bound.min(1),
        clean_shutdown_observed: clean,
    });
    let envelope = ObservabilityEnvelopeV1 {
        event_id: format!("{boot}:drop:{sequence}"),
        event_kind: payload.event_kind().to_owned(),
        schema_revision: 1,
        idempotency_key: format!("{boot}:drop:{sequence}"),
        trace_id: boot.to_owned(),
        scope_ref: scope.to_owned(),
        capability: "observability".to_owned(),
        operation: "drop".to_owned(),
        event_time_micros,
        observation_time_micros: event_time_micros,
        valid_from_micros: None,
        valid_until_micros: None,
        quantity: Some(lower_bound as f64),
        unit: Some("events".to_owned()),
        terminal_result: Some(if lower_bound > 0 {
            ObservabilityTerminalResultV1::Partial
        } else if clean {
            ObservabilityTerminalResultV1::Succeeded
        } else {
            ObservabilityTerminalResultV1::Unknown
        }),
        producer_revision: "producer.v1".to_owned(),
        configuration_revision: "configuration.v1".to_owned(),
        policy_revision: "policy.v1".to_owned(),
        watermark: format!("{boot}:{sequence}"),
        coverage: if lower_bound > 0 {
            CoverageStateV1::Partial
        } else if clean {
            CoverageStateV1::Known
        } else {
            CoverageStateV1::Unknown
        },
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count: lower_bound,
        process_boot_id: boot.to_owned(),
        producer_sequence: sequence,
        payload,
    };
    envelope.validate().expect("valid drop receipt fixture");
    envelope
}

async fn runtime() -> (
    tempfile::TempDir,
    tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime,
) {
    let project = tempfile::tempdir().expect("project");
    let project_id = tracedecay_domain::ProjectId::new("project.observability.shutdown")
        .expect("project identifier");
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
async fn clean_shutdown_persists_zero_drop_terminal_without_relabeling_cancel() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.shutdown";

    let idle = BoundedObservabilityProducerV1::start(
        Arc::clone(&db),
        identity(scope, "boot:clean-idle"),
        4,
    )
    .expect("idle producer");
    let idle_summary = idle.shutdown().await.expect("idle clean shutdown");
    assert_eq!(idle_summary.persisted, 1);
    assert_eq!(idle_summary.dropped, 0);
    assert!(!idle_summary.cancelled);

    let active = BoundedObservabilityProducerV1::start(
        Arc::clone(&db),
        identity(scope, "boot:clean-active"),
        4,
    )
    .expect("active producer");
    assert_eq!(
        active
            .try_emit(envelope(scope))
            .expect("active observation"),
        ObservabilityEmissionOutcomeV1::Enqueued
    );
    let active_summary = active.shutdown().await.expect("active clean shutdown");
    assert_eq!(active_summary.persisted, 2);
    assert_eq!(active_summary.dropped, 0);
    assert!(!active_summary.cancelled);

    let cancelled = BoundedObservabilityProducerV1::start(
        Arc::clone(&db),
        identity(scope, "boot:cancelled"),
        4,
    )
    .expect("cancelled producer");
    let cancelled_summary = cancelled.cancel().await.expect("cancel producer");
    assert_eq!(cancelled_summary.persisted, 0);
    assert_eq!(cancelled_summary.dropped, 0);
    assert!(cancelled_summary.cancelled);

    let page = RegisteredObservabilityPortV1::new(&db)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: scope.to_owned(),
            event_kinds: Vec::new(),
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 8,
        })
        .await
        .expect("terminal receipt query");
    assert_eq!(page.events.len(), 3);
    assert!(
        page.events
            .iter()
            .all(|event| event.process_boot_id != "boot:cancelled"),
        "cancellation must not fabricate a clean terminal"
    );

    let terminal_for = |boot: &str| {
        page.events
            .iter()
            .find(|event| {
                event.process_boot_id == boot
                    && matches!(&event.payload, ObservabilityPayloadV1::TelemetryDrop(_))
            })
            .unwrap_or_else(|| panic!("missing terminal for {boot}"))
    };
    let idle_terminal = terminal_for("boot:clean-idle");
    assert_eq!(idle_terminal.producer_sequence, 1);
    assert_eq!(idle_terminal.watermark, "boot:clean-idle:1");
    assert_eq!(idle_terminal.dropped_count, 0);
    assert_eq!(idle_terminal.coverage, CoverageStateV1::Known);
    assert_eq!(
        idle_terminal.terminal_result,
        Some(ObservabilityTerminalResultV1::Succeeded)
    );
    let ObservabilityPayloadV1::TelemetryDrop(idle_drop) = &idle_terminal.payload else {
        unreachable!()
    };
    assert_eq!(idle_drop.first_missing_sequence, 1);
    assert_eq!(idle_drop.last_missing_sequence, 1);
    assert_eq!(idle_drop.proved_drop_lower_bound, 0);
    assert!(idle_drop.clean_shutdown_observed);

    let active_observation = page
        .events
        .iter()
        .find(|event| event.process_boot_id == "boot:clean-active" && event.operation == "query")
        .expect("active observation");
    assert_eq!(active_observation.producer_sequence, 1);
    let active_terminal = terminal_for("boot:clean-active");
    assert_eq!(active_terminal.producer_sequence, 2);
    assert_eq!(active_terminal.watermark, "boot:clean-active:2");
    let ObservabilityPayloadV1::TelemetryDrop(active_drop) = &active_terminal.payload else {
        unreachable!()
    };
    assert_eq!(active_drop.first_missing_sequence, 2);
    assert_eq!(active_drop.last_missing_sequence, 2);
    assert_eq!(active_drop.proved_drop_lower_bound, 0);
    assert!(active_drop.clean_shutdown_observed);

    let horizon = ObservabilityHorizonV1 {
        since_micros: 0,
        until_micros: now_micros().0,
    };
    let latency = provider_latency_read_model(
        Some(&db),
        Some(scope),
        &horizon,
        &ProviderUsageAggregateV1 {
            coverage: ProviderUsageCoverageV1::Complete,
            observations_seen: 0,
            totals: AggregatedProviderUsageCountersV1::unknown(),
            deltas: Vec::new(),
            issues: Vec::new(),
            upper_observation_sequence: None,
        },
    )
    .await;
    assert_eq!(latency.len(), 1);
    assert_eq!(
        latency[0].queue.p50.unavailable_reason.as_deref(),
        Some("provider_operation_resources_not_recorded")
    );

    let packet = ObservabilityAggregateExportApplicationV1::new(
        RegisteredAggregateShareExporterV1::new(&db),
    )
    .export(AggregateShareExportRequestV1 {
        mode: AnalyticsModeV1::AggregateShare,
        authorized_scope_ref: scope.to_owned(),
        horizon,
        max_cells: 16,
    })
    .await
    .expect("clean terminal aggregate export");
    assert!(packet.cells.is_empty());
    assert_eq!(packet.suppressed_cell_count, 2);
    assert_eq!(packet.capped_cell_count, 0);
}

#[tokio::test]
async fn persistence_failure_cannot_be_rewritten_as_a_clean_terminal() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.shutdown";
    let blocker = db
        .begin_write_transaction()
        .await
        .expect("hold registered writer");
    let producer = BoundedObservabilityProducerV1::start_with_deadlines(
        Arc::clone(&db),
        identity(scope, "boot:persistence-failed"),
        1,
        ObservabilityProducerDeadlinesV1 {
            persistence: Duration::from_millis(50),
            shutdown: Duration::from_millis(500),
        },
    )
    .expect("producer");
    assert_eq!(
        producer
            .try_emit(envelope(scope))
            .expect("queued observation"),
        ObservabilityEmissionOutcomeV1::Enqueued
    );
    tokio::task::yield_now().await;
    let mut queued = envelope(scope);
    queued.event_id = "event:persistence-failed:queued".to_owned();
    queued.idempotency_key = "idempotency:persistence-failed:queued".to_owned();
    queued.trace_id = "trace:persistence-failed:queued".to_owned();
    assert_eq!(
        producer
            .try_emit(queued)
            .expect("second queued observation"),
        ObservabilityEmissionOutcomeV1::Enqueued
    );
    let mut dropped = envelope(scope);
    dropped.event_id = "event:persistence-failed:dropped".to_owned();
    dropped.idempotency_key = "idempotency:persistence-failed:dropped".to_owned();
    dropped.trace_id = "trace:persistence-failed:dropped".to_owned();
    assert_eq!(
        producer.try_emit(dropped).expect("capacity observation"),
        ObservabilityEmissionOutcomeV1::DroppedAtCapacity
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    blocker.commit().await.expect("release registered writer");

    let error = producer
        .shutdown()
        .await
        .expect_err("lost observation keeps shutdown typed as failed");
    assert!(
        error
            .to_string()
            .contains("observability_persistence_deadline"),
        "unexpected shutdown error: {error}"
    );

    let page = RegisteredObservabilityPortV1::new(&db)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: scope.to_owned(),
            event_kinds: vec!["telemetry.drop.observed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 2,
        })
        .await
        .expect("failed terminal query");
    assert_eq!(page.events.len(), 2);
    let positive = page
        .events
        .iter()
        .find(|event| event.dropped_count > 0)
        .expect("positive drop receipt");
    let ObservabilityPayloadV1::TelemetryDrop(positive_drop) = &positive.payload else {
        panic!("positive receipt used the wrong payload")
    };
    assert!(!positive_drop.clean_shutdown_observed);
    assert_eq!(positive_drop.proved_drop_lower_bound, 1);
    assert_eq!(positive.coverage, CoverageStateV1::Partial);
    assert_eq!(
        positive.terminal_result,
        Some(ObservabilityTerminalResultV1::Partial)
    );
    let terminal = page
        .events
        .iter()
        .find(|event| event.dropped_count == 0)
        .expect("nonclean zero terminal");
    let ObservabilityPayloadV1::TelemetryDrop(terminal_drop) = &terminal.payload else {
        panic!("failed terminal used the wrong payload")
    };
    assert!(!terminal_drop.clean_shutdown_observed);
    assert_eq!(terminal_drop.proved_drop_lower_bound, 0);
    assert_eq!(terminal.coverage, CoverageStateV1::Unknown);
    assert_eq!(
        terminal.terminal_result,
        Some(ObservabilityTerminalResultV1::Unknown)
    );
    assert_eq!(terminal.dropped_count, 0);

    let latency = provider_latency_read_model(
        Some(&db),
        Some(scope),
        &ObservabilityHorizonV1 {
            since_micros: 0,
            until_micros: now_micros().0,
        },
        &ProviderUsageAggregateV1 {
            coverage: ProviderUsageCoverageV1::Complete,
            observations_seen: 0,
            totals: AggregatedProviderUsageCountersV1::unknown(),
            deltas: Vec::new(),
            issues: Vec::new(),
            upper_observation_sequence: None,
        },
    )
    .await;
    assert_eq!(latency.len(), 1);
    assert_eq!(
        latency[0].queue.p50.coverage.state,
        CoverageStateV1::Unknown
    );
}

#[tokio::test]
async fn carried_positive_drop_then_clean_terminal_remains_partial() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.shutdown";
    let port = RegisteredObservabilityPortV1::new(&db);
    port.record(drop_receipt(scope, "boot:carried", 2, 1, false, 1))
        .await
        .expect("positive carried receipt");
    port.record(drop_receipt(scope, "boot:carried", 3, 0, true, 2))
        .await
        .expect("clean terminal");

    let latency = provider_latency_read_model(
        Some(&db),
        Some(scope),
        &ObservabilityHorizonV1 {
            since_micros: 0,
            until_micros: now_micros().0,
        },
        &ProviderUsageAggregateV1 {
            coverage: ProviderUsageCoverageV1::Complete,
            observations_seen: 0,
            totals: AggregatedProviderUsageCountersV1::unknown(),
            deltas: Vec::new(),
            issues: Vec::new(),
            upper_observation_sequence: None,
        },
    )
    .await;
    assert_eq!(latency.len(), 1);
    assert_eq!(
        latency[0].queue.p50.unavailable_reason.as_deref(),
        Some("incomplete_operation_resource_coverage")
    );
    assert_eq!(
        latency[0].queue.p50.coverage.state,
        CoverageStateV1::Partial
    );
}

#[tokio::test]
async fn unclean_zero_terminal_degrades_aggregate_without_fabricating_a_drop_cell() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.shutdown";
    let port = RegisteredObservabilityPortV1::new(&db);
    let day_micros = 86_400_000_000_i64;
    for day in 0..100_i64 {
        let mut event = envelope(scope);
        event.event_id = format!("event:aggregate:{day}");
        event.idempotency_key = format!("idempotency:aggregate:{day}");
        event.trace_id = format!("trace:aggregate:{day}");
        event.event_time_micros = day.saturating_mul(day_micros).saturating_add(1);
        event.observation_time_micros = event.event_time_micros;
        event.producer_sequence = u64::try_from(day + 1).expect("positive sequence");
        port.record(event).await.expect("aggregate contribution");
    }
    port.record(drop_receipt(
        scope,
        "boot:aggregate-unclean",
        1,
        0,
        false,
        99_i64.saturating_mul(day_micros).saturating_add(2),
    ))
    .await
    .expect("unclean terminal");

    let packet = ObservabilityAggregateExportApplicationV1::new(
        RegisteredAggregateShareExporterV1::new(&db),
    )
    .export(AggregateShareExportRequestV1 {
        mode: AnalyticsModeV1::AggregateShare,
        authorized_scope_ref: scope.to_owned(),
        horizon: ObservabilityHorizonV1 {
            since_micros: 0,
            until_micros: 100_i64.saturating_mul(day_micros),
        },
        max_cells: 16,
    })
    .await
    .expect("aggregate export");

    assert_eq!(packet.cells.len(), 2);
    assert!(packet.cells.iter().all(|cell| {
        cell.metric != tracedecay_application::AggregateShareMetricV1::TelemetryDropsLowerBound
            && cell.coverage == CoverageStateV1::Unknown
            && cell.value.is_none()
    }));
}

#[tokio::test]
async fn shutdown_waits_for_durable_owner_admission_before_terminal() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.shutdown";
    let blocker = db
        .begin_write_transaction()
        .await
        .expect("hold registered writer");
    let producer = Arc::new(
        BoundedObservabilityProducerV1::start(
            Arc::clone(&db),
            identity(scope, "boot:durable-owner-stop"),
            4,
        )
        .expect("producer"),
    );
    let owner_admission = {
        let producer = Arc::clone(&producer);
        tokio::spawn(async move { producer.emit_owner_fact(envelope(scope)).await })
    };
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !owner_admission.is_finished(),
        "registered writer must hold durable owner admission"
    );
    let shutdown = {
        let producer = Arc::clone(&producer);
        tokio::spawn(async move { producer.shutdown().await })
    };
    tokio::time::sleep(Duration::from_millis(25)).await;
    blocker.commit().await.expect("release registered writer");

    assert_eq!(
        owner_admission
            .await
            .expect("owner task")
            .expect("owner admission"),
        ObservabilityOwnerEmissionOutcomeV1::Enqueued
    );
    shutdown.await.expect("shutdown task").expect("shutdown");

    let page = RegisteredObservabilityPortV1::new(&db)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: scope.to_owned(),
            event_kinds: Vec::new(),
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 4,
        })
        .await
        .expect("owner and terminal query");
    assert_eq!(page.events.len(), 2);
    let owner = page
        .events
        .iter()
        .find(|event| matches!(event.payload, ObservabilityPayloadV1::RetrievalQuery(_)))
        .expect("owner delivery");
    let terminal = page
        .events
        .iter()
        .find(|event| matches!(event.payload, ObservabilityPayloadV1::TelemetryDrop(_)))
        .expect("terminal");
    assert_eq!(owner.producer_sequence, 1);
    assert_eq!(terminal.producer_sequence, 2);
}

#[tokio::test]
async fn clean_shutdown_with_pending_drop_remains_partial() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.shutdown";
    let blocker = db
        .begin_write_transaction()
        .await
        .expect("hold registered writer");
    let producer = BoundedObservabilityProducerV1::start(
        Arc::clone(&db),
        identity(scope, "boot:pending-drop"),
        1,
    )
    .expect("producer");
    let mut dropped = 0_u64;
    for index in 0..256_u64 {
        let mut event = envelope(scope);
        event.event_id = format!("event:pending-drop:{index}");
        event.idempotency_key = format!("idempotency:pending-drop:{index}");
        event.trace_id = format!("trace:pending-drop:{index}");
        dropped = dropped.saturating_add(u64::from(
            producer.try_emit(event).expect("bounded observation")
                == ObservabilityEmissionOutcomeV1::DroppedAtCapacity,
        ));
    }
    assert!(dropped > 0);
    blocker.commit().await.expect("release registered writer");
    let summary = producer.shutdown().await.expect("clean shutdown");
    assert_eq!(summary.dropped, dropped);

    let page = RegisteredObservabilityPortV1::new(&db)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: scope.to_owned(),
            event_kinds: vec!["telemetry.drop.observed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 2,
        })
        .await
        .expect("drop terminal query");
    assert_eq!(page.events.len(), 1);
    let terminal = &page.events[0];
    let ObservabilityPayloadV1::TelemetryDrop(drop) = &terminal.payload else {
        unreachable!()
    };
    assert!(drop.clean_shutdown_observed);
    assert_eq!(drop.proved_drop_lower_bound, dropped);
    assert_eq!(terminal.dropped_count, dropped);
    assert_eq!(terminal.coverage, CoverageStateV1::Partial);
    assert_eq!(
        terminal.terminal_result,
        Some(ObservabilityTerminalResultV1::Partial)
    );
}

#[tokio::test]
async fn shutdown_terminal_linearizes_after_concurrent_admission() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.shutdown";
    let producer = Arc::new(
        BoundedObservabilityProducerV1::start(
            Arc::clone(&db),
            identity(scope, "boot:concurrent-stop"),
            1_024,
        )
        .expect("producer"),
    );
    let start = Arc::new(Barrier::new(257));
    let emitters = (0..256)
        .map(|index| {
            let producer = Arc::clone(&producer);
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                let mut event = envelope(scope);
                event.event_id = format!("event:concurrent:{index}");
                event.idempotency_key = format!("idempotency:concurrent:{index}");
                event.trace_id = format!("trace:concurrent:{index}");
                start.wait();
                producer.try_emit(event)
            })
        })
        .collect::<Vec<_>>();
    start.wait();
    producer.shutdown().await.expect("concurrent shutdown");
    let admitted = emitters
        .into_iter()
        .filter_map(|emitter| emitter.join().expect("emitter thread").ok())
        .count() as u64;

    let page = RegisteredObservabilityPortV1::new(&db)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: scope.to_owned(),
            event_kinds: vec!["telemetry.drop.observed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 2,
        })
        .await
        .expect("terminal query");
    assert_eq!(page.events.len(), 1);
    let terminal = &page.events[0];
    let ObservabilityPayloadV1::TelemetryDrop(drop) = &terminal.payload else {
        unreachable!()
    };
    assert!(drop.clean_shutdown_observed);
    assert_eq!(drop.proved_drop_lower_bound, 0);
    assert_eq!(terminal.producer_sequence, admitted.saturating_add(1));
}
