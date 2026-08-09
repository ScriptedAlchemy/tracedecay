use std::sync::Arc;
use std::time::Duration;

use tracedecay_application::{
    AggregateShareExportRequestV1, ObservabilityAggregateExportApplicationV1,
    ObservabilityHorizonV1, ObservabilityQueryPort, ObservabilityQueryV1, ObservabilityRecordPort,
};
use tracedecay_domain::{
    AnalyticsModeV1, CoverageStateV1, ExecutionPlacementV1, ExecutionTopologyKindV1,
    ExecutionTopologySampledV1, IntegrationStrategyV1, ObservabilityEnvelopeV1,
    ObservabilityPayloadV1, ObservabilityRetentionClassV1, ObservabilityTerminalResultV1,
    RetrievalQueryObservedV1, ReviewTopologyV1, WorkTopologyBranchV1,
};
use tracedecay_usecases::observability::{
    BoundedObservabilityProducerV1, ObservabilityEmissionOutcomeV1,
    ObservabilityOwnerEmissionOutcomeV1, ObservabilityProducerDeadlinesV1,
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

fn topology_envelope(scope: &str, id: u64, event_time_micros: i64) -> ObservabilityEnvelopeV1 {
    let payload = ObservabilityPayloadV1::ExecutionTopology(ExecutionTopologySampledV1 {
        topology: ExecutionTopologyKindV1::Parallel,
        placement: ExecutionPlacementV1::LinkedWorktree,
        branch_topology: WorkTopologyBranchV1::IndependentBranches,
        review_topology: ReviewTopologyV1::IndependentReview,
        integration_strategy: IntegrationStrategyV1::FastForwardOnly,
        requested_width: 4,
        accepted_width: 4,
        admitted_width: 3,
        active_width: 3,
        useful_width: 2,
        runnable_count: 3,
        blocked_count: 0,
        shared_authority_serialized_count: 0,
        local_anchor_refs: vec![format!("anchor:{id}")],
    });
    let mut envelope = envelope(scope, "boot:rollup-source", id, event_time_micros);
    envelope.event_kind = payload.event_kind().to_owned();
    envelope.payload = payload;
    envelope.quantity = None;
    envelope.unit = None;
    envelope.retention_class = ObservabilityRetentionClassV1::LocalRollup395d;
    envelope.validate().expect("topology envelope");
    envelope
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
    let producer =
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
async fn bounded_producer_stamps_its_mounted_identity_and_preserves_delayed_evidence() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.v2".to_owned();
    let identity = ObservabilityProducerIdentityV1 {
        authorized_scope_ref: scope.clone(),
        process_boot_id: "boot:mounted".into(),
        producer_revision: "producer.mounted.v1".into(),
        configuration_revision: "configuration.mounted.v1".into(),
        policy_revision: "policy.mounted.v1".into(),
    };
    let producer = BoundedObservabilityProducerV1::start(Arc::clone(&db), identity.clone(), 4)
        .expect("producer");

    let mut delayed = envelope(&scope, "boot:caller", 41, 1_000_000);
    delayed.producer_revision = "producer.caller.v1".into();
    delayed.configuration_revision = "configuration.caller.v1".into();
    delayed.policy_revision = "policy.caller.v1".into();
    delayed.watermark = "caller:41".into();
    delayed.observation_time_micros = 1_000_001;
    delayed.delayed_count = 1;
    assert_eq!(
        producer.try_emit(delayed).expect("enqueue delayed event"),
        ObservabilityEmissionOutcomeV1::Enqueued
    );
    producer.shutdown().await.expect("flush producer");

    let page = RegisteredObservabilityPortV1::new(&db)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: scope,
            event_kinds: vec!["retrieval.query.completed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 8,
        })
        .await
        .expect("query stamped event");
    assert_eq!(page.events.len(), 1);
    let persisted = &page.events[0];
    assert_eq!(persisted.process_boot_id, identity.process_boot_id);
    assert_eq!(persisted.producer_revision, identity.producer_revision);
    assert_eq!(
        persisted.configuration_revision,
        identity.configuration_revision
    );
    assert_eq!(persisted.policy_revision, identity.policy_revision);
    assert_eq!(persisted.producer_sequence, 1);
    assert_eq!(persisted.watermark, "boot:mounted:1");
    assert_eq!(persisted.delayed_count, 1);
}

#[tokio::test]
async fn durable_owner_replay_reuses_the_exact_delivery_across_producer_restart() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.v2".to_owned();
    let identity = ObservabilityProducerIdentityV1 {
        authorized_scope_ref: scope.clone(),
        process_boot_id: "boot:owner-first".into(),
        producer_revision: "producer.owner.v1".into(),
        configuration_revision: "configuration.owner.v1".into(),
        policy_revision: "policy.owner.v1".into(),
    };
    let producer =
        BoundedObservabilityProducerV1::start(Arc::clone(&db), identity, 4).expect("producer");
    let owner = envelope(&scope, "caller", 71, 7_100_000);
    assert_eq!(
        producer
            .emit_owner_fact(owner.clone())
            .await
            .expect("first owner emission"),
        ObservabilityOwnerEmissionOutcomeV1::Enqueued
    );
    producer.shutdown().await.expect("first shutdown");

    let restarted_identity = ObservabilityProducerIdentityV1 {
        authorized_scope_ref: scope.clone(),
        process_boot_id: "boot:owner-restarted".into(),
        producer_revision: "producer.owner.v2".into(),
        configuration_revision: "configuration.owner.v2".into(),
        policy_revision: "policy.owner.v2".into(),
    };
    let restarted = BoundedObservabilityProducerV1::start(Arc::clone(&db), restarted_identity, 4)
        .expect("restarted producer");
    assert_eq!(
        restarted
            .emit_owner_fact(owner.clone())
            .await
            .expect("owner replay"),
        ObservabilityOwnerEmissionOutcomeV1::Replayed
    );
    let mut conflicting = owner;
    let ObservabilityPayloadV1::RetrievalQuery(payload) = &mut conflicting.payload else {
        unreachable!()
    };
    payload.candidate_budget = 11;
    let conflict = restarted
        .emit_owner_fact(conflicting)
        .await
        .expect_err("changed owner fact conflicts");
    assert!(conflict.to_string().contains("owner fact conflict"));
    restarted.shutdown().await.expect("restarted shutdown");

    let page = RegisteredObservabilityPortV1::new(&db)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: scope,
            event_kinds: vec!["retrieval.query.completed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 8,
        })
        .await
        .expect("query owner delivery");
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].process_boot_id, "boot:owner-first");
    assert_eq!(page.events[0].producer_sequence, 1);
}

#[tokio::test]
async fn live_queued_owner_claim_is_not_recovered_as_delayed_work() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.v2".to_owned();
    let blocker = db
        .begin_write_transaction()
        .await
        .expect("hold registered writer");
    let producer = Arc::new(
        BoundedObservabilityProducerV1::start(
            Arc::clone(&db),
            ObservabilityProducerIdentityV1 {
                authorized_scope_ref: scope.clone(),
                process_boot_id: "boot:live-owner".into(),
                producer_revision: "producer.owner.v1".into(),
                configuration_revision: "configuration.owner.v1".into(),
                policy_revision: "policy.owner.v1".into(),
            },
            2,
        )
        .expect("producer"),
    );
    assert_eq!(
        producer
            .try_emit(envelope(&scope, "caller", 72, 7_200_000))
            .expect("first queue carrier"),
        ObservabilityEmissionOutcomeV1::Enqueued
    );
    tokio::task::yield_now().await;
    assert_eq!(
        producer
            .try_emit(envelope(&scope, "caller", 73, 7_300_000))
            .expect("second queue carrier"),
        ObservabilityEmissionOutcomeV1::Enqueued
    );
    let owner = envelope(&scope, "caller", 74, 7_400_000);
    let owner_admission = {
        let producer = Arc::clone(&producer);
        tokio::spawn(async move { producer.emit_owner_fact(owner).await })
    };
    tokio::task::yield_now().await;
    blocker.commit().await.expect("release registered writer");
    let outcome = tokio::time::timeout(Duration::from_secs(2), owner_admission)
        .await
        .expect("owner admission deadline")
        .expect("owner admission task")
        .expect("owner admission");
    assert_eq!(outcome, ObservabilityOwnerEmissionOutcomeV1::Enqueued);
    producer.shutdown().await.expect("producer shutdown");

    let page = RegisteredObservabilityPortV1::new(&db)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: scope,
            event_kinds: vec!["retrieval.query.completed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 8,
        })
        .await
        .expect("query owner delivery");
    let owner = page
        .events
        .iter()
        .find(|event| event.idempotency_key == "idempotency:74")
        .expect("live owner delivery");
    assert_eq!(owner.delayed_count, 0);
    assert_eq!(owner.coverage, CoverageStateV1::Known);
}

#[tokio::test]
async fn producer_idle_worker_rebuilds_a_dirty_daily_rollup_without_a_request() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.v2".to_owned();
    let day_start_seconds = 86_400_i64;
    let day_start_micros = day_start_seconds * 1_000_000;
    let port = RegisteredObservabilityPortV1::new(&db);
    let mut source_watermark = None;
    for id in 1..=5_u64 {
        let cursor = port
            .record(topology_envelope(
                &scope,
                id,
                day_start_micros + i64::try_from(id).expect("small event identifier"),
            ))
            .await
            .expect("record topology source event");
        source_watermark = Some(
            cursor
                .strip_prefix("analytics:")
                .expect("analytics cursor prefix")
                .parse::<i64>()
                .expect("analytics cursor identifier"),
        );
    }
    port.record(envelope(
        &scope,
        "boot:unrelated",
        99,
        day_start_micros + 99,
    ))
    .await
    .expect("record newer unrelated observability event");

    let producer = BoundedObservabilityProducerV1::start(
        Arc::clone(&db),
        ObservabilityProducerIdentityV1 {
            authorized_scope_ref: scope.clone(),
            process_boot_id: "boot:rollup-worker".into(),
            producer_revision: "producer.rollup.v1".into(),
            configuration_revision: "configuration.rollup.v1".into(),
            policy_revision: "policy.rollup.v1".into(),
        },
        4,
    )
    .expect("producer");
    producer.shutdown().await.expect("rollup worker shutdown");

    let fragments = db
        .query_observability_rollup_fragments(
            &tracedecay_global_db::ObservabilityRollupFragmentQueryV1 {
                authorized_scope_ref: scope.clone(),
                since_day_start_seconds: day_start_seconds,
                until_day_start_seconds: day_start_seconds + 86_400,
            },
        )
        .await
        .expect("query daily fragment");
    assert_eq!(fragments.fragments.len(), 1);
    assert_eq!(
        fragments.fragments[0].source_watermark,
        source_watermark.expect("recorded source watermark")
    );
}

#[tokio::test]
async fn persisted_topology_wakes_idle_rollup_after_unrelated_queue_tail() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.v2".to_owned();
    let day = 259_200_i64;
    let producer = BoundedObservabilityProducerV1::start(
        Arc::clone(&db),
        ObservabilityProducerIdentityV1 {
            authorized_scope_ref: scope.clone(),
            process_boot_id: "boot:rollup-source-wakeup".into(),
            producer_revision: "producer.rollup.v1".into(),
            configuration_revision: "configuration.rollup.v1".into(),
            policy_revision: "policy.rollup.v1".into(),
        },
        16,
    )
    .expect("producer");
    for id in 1..=5_u64 {
        producer
            .try_emit(topology_envelope(
                &scope,
                id,
                day * 1_000_000 + i64::try_from(id).expect("small source id"),
            ))
            .expect("enqueue topology source");
    }
    producer
        .try_emit(envelope(
            &scope,
            "boot:unrelated-tail",
            99,
            day * 1_000_000 + 99,
        ))
        .expect("enqueue unrelated tail");
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let page = db
                .query_observability_rollup_fragments(
                    &tracedecay_global_db::ObservabilityRollupFragmentQueryV1 {
                        authorized_scope_ref: scope.clone(),
                        since_day_start_seconds: day,
                        until_day_start_seconds: day + 86_400,
                    },
                )
                .await
                .expect("query source-triggered rollup");
            if page.fragments.len() == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("topology source must bypass the five-minute idle poll");
    producer.shutdown().await.expect("producer shutdown");
}

#[tokio::test]
async fn refused_daily_projection_releases_its_claim_and_leaves_the_day_dirty() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.v2".to_owned();
    let day_start_seconds = 172_800_i64;
    let mut partial = topology_envelope(&scope, 1, day_start_seconds * 1_000_000 + 1);
    partial.coverage = CoverageStateV1::Partial;
    RegisteredObservabilityPortV1::new(&db)
        .record(partial)
        .await
        .expect("record partial topology source event");

    let producer = BoundedObservabilityProducerV1::start(
        Arc::clone(&db),
        ObservabilityProducerIdentityV1 {
            authorized_scope_ref: scope.clone(),
            process_boot_id: "boot:rollup-refusal".into(),
            producer_revision: "producer.rollup.v1".into(),
            configuration_revision: "configuration.rollup.v1".into(),
            policy_revision: "policy.rollup.v1".into(),
        },
        4,
    )
    .expect("producer");
    producer
        .shutdown()
        .await
        .expect("refused rollup does not fail observation shutdown");

    let claim = db
        .claim_observability_rollup_dirty_day(&scope, "test:retry", 30)
        .await
        .expect("claim retained dirty marker")
        .expect("refused day remains retryable");
    assert_eq!(claim.day_start_seconds, day_start_seconds);
    assert!(
        db.release_observability_rollup_dirty_day(&claim)
            .await
            .expect("release test claim")
    );
    let fragments = db
        .query_observability_rollup_fragments(
            &tracedecay_global_db::ObservabilityRollupFragmentQueryV1 {
                authorized_scope_ref: scope,
                since_day_start_seconds: day_start_seconds,
                until_day_start_seconds: day_start_seconds + 86_400,
            },
        )
        .await
        .expect("query refused day");
    assert!(fragments.fragments.is_empty());
}

#[tokio::test]
async fn restart_recovers_pending_owner_delivery_without_allocating_a_new_carrier_identity() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.v2".to_owned();
    let owner = envelope(&scope, "caller", 76, 7_600_000);
    let mut normalized_owner = owner.clone();
    normalized_owner.producer_revision = "producer-owned".into();
    normalized_owner.configuration_revision = "producer-owned".into();
    normalized_owner.policy_revision = "producer-owned".into();
    normalized_owner.watermark = "producer-owned".into();
    normalized_owner.process_boot_id = "producer-owned".into();
    normalized_owner.producer_sequence = 0;
    let owner_json = serde_json::to_string(&normalized_owner).expect("normalized owner fact");
    let mut first_delivery = owner;
    first_delivery.process_boot_id = "boot:pending-first".into();
    first_delivery.producer_revision = "producer.owner.v1".into();
    first_delivery.configuration_revision = "configuration.owner.v1".into();
    first_delivery.policy_revision = "policy.owner.v1".into();
    first_delivery.producer_sequence = 17;
    first_delivery.watermark = "boot:pending-first:17".into();
    first_delivery.validate().expect("first delivery");
    let delivery_json = serde_json::to_string(&first_delivery).expect("delivery bytes");
    db.claim_observability_emission(
        &scope,
        &first_delivery.idempotency_key,
        &owner_json,
        &delivery_json,
    )
    .await
    .expect("pending durable claim");

    let restarted = BoundedObservabilityProducerV1::start(
        Arc::clone(&db),
        ObservabilityProducerIdentityV1 {
            authorized_scope_ref: scope.clone(),
            process_boot_id: "boot:pending-restarted".into(),
            producer_revision: "producer.owner.v2".into(),
            configuration_revision: "configuration.owner.v2".into(),
            policy_revision: "policy.owner.v2".into(),
        },
        4,
    )
    .expect("restarted producer");
    restarted
        .shutdown()
        .await
        .expect("recover and flush pending");

    let page = RegisteredObservabilityPortV1::new(&db)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: scope,
            event_kinds: vec!["retrieval.query.completed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 8,
        })
        .await
        .expect("query recovered delivery");
    assert_eq!(page.events.len(), 1);
    assert_eq!(page.events[0].process_boot_id, "boot:pending-first");
    assert_eq!(page.events[0].producer_sequence, 17);
    assert_eq!(page.events[0].watermark, "boot:pending-first:17");
    assert_eq!(page.events[0].delayed_count, 1);
    assert_eq!(page.events[0].coverage, CoverageStateV1::Partial);
}

#[tokio::test]
async fn nonblocking_owner_offer_is_claimed_by_worker_and_replay_keeps_first_delivery() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let (_project, runtime) = runtime().await;
    let db = runtime.project_database_arc().expect("project database");
    let scope = "project.observability.v2".to_owned();
    let first = BoundedObservabilityProducerV1::start(
        Arc::clone(&db),
        ObservabilityProducerIdentityV1 {
            authorized_scope_ref: scope.clone(),
            process_boot_id: "boot:nonblocking-first".into(),
            producer_revision: "producer.owner.v1".into(),
            configuration_revision: "configuration.owner.v1".into(),
            policy_revision: "policy.owner.v1".into(),
        },
        4,
    )
    .expect("first producer");
    let owner = envelope(&scope, "caller", 81, 8_100_000);
    assert_eq!(
        first
            .try_emit_owner_fact(owner.clone())
            .expect("offer owner fact"),
        ObservabilityEmissionOutcomeV1::Enqueued
    );
    first.shutdown().await.expect("first shutdown");

    let restarted = BoundedObservabilityProducerV1::start(
        Arc::clone(&db),
        ObservabilityProducerIdentityV1 {
            authorized_scope_ref: scope.clone(),
            process_boot_id: "boot:nonblocking-restarted".into(),
            producer_revision: "producer.owner.v2".into(),
            configuration_revision: "configuration.owner.v2".into(),
            policy_revision: "policy.owner.v2".into(),
        },
        4,
    )
    .expect("restarted producer");
    assert_eq!(
        restarted
            .try_emit_owner_fact(owner)
            .expect("offer owner replay"),
        ObservabilityEmissionOutcomeV1::Enqueued
    );
    assert_eq!(
        restarted
            .try_emit_owner_fact(envelope(&scope, "caller", 82, 8_200_000,))
            .expect("offer new owner after replay"),
        ObservabilityEmissionOutcomeV1::Enqueued
    );
    restarted.shutdown().await.expect("restarted shutdown");

    let page = RegisteredObservabilityPortV1::new(&db)
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: scope,
            event_kinds: vec!["retrieval.query.completed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: i64::MAX,
            },
            after_watermark: None,
            limit: 8,
        })
        .await
        .expect("query owner delivery");
    assert_eq!(page.events.len(), 2);
    let first_delivery = page
        .events
        .iter()
        .find(|event| event.idempotency_key == "idempotency:81")
        .expect("first delivery");
    assert_eq!(first_delivery.process_boot_id, "boot:nonblocking-first");
    assert_eq!(first_delivery.producer_sequence, 1);
    let new_delivery = page
        .events
        .iter()
        .find(|event| event.idempotency_key == "idempotency:82")
        .expect("new delivery");
    assert_eq!(new_delivery.process_boot_id, "boot:nonblocking-restarted");
    assert_eq!(new_delivery.producer_sequence, 1);
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
    let producer =
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
    let producer =
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
    let producer = BoundedObservabilityProducerV1::start_with_deadlines(
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
    assert_eq!(result.expired_settled_outbox, 0);
    assert!(!result.has_more);

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
