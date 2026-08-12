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
    let mut stale = topology_envelope(&scope, 1, day_start_seconds * 1_000_000 + 1);
    stale.coverage = CoverageStateV1::Stale;
    RegisteredObservabilityPortV1::new(&db)
        .record(stale)
        .await
        .expect("record stale topology source event");

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
    let explicit_event = page
        .events
        .iter()
        .find(|event| matches!(event.payload, ObservabilityPayloadV1::TelemetryDrop(_)))
        .expect("explicit durable drop range");
    let ObservabilityPayloadV1::TelemetryDrop(explicit) = &explicit_event.payload else {
        unreachable!()
    };
    assert!(!explicit.clean_shutdown_observed);
    assert_eq!(explicit.proved_drop_lower_bound, dropped);
    assert_eq!(
        explicit
            .last_missing_sequence
            .saturating_sub(explicit.first_missing_sequence)
            .saturating_add(1),
        dropped
    );
    assert_eq!(explicit_event.coverage, CoverageStateV1::Partial);
    assert_eq!(
        explicit_event.terminal_result,
        Some(ObservabilityTerminalResultV1::Partial)
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

#[tokio::test]
async fn real_work_owner_receipts_converge_to_rollup() {
    let report = work_rollup_harness::run_work_rollup_case().await;

    assert_eq!(report.offered_sources, 512, "{report:#?}");
    assert_eq!(report.dropped_sources, 0, "{report:#?}");
    assert_eq!(report.durable_sources, 512, "{report:#?}");
    assert_eq!(report.fragment_count, 1, "{report:#?}");
    assert_eq!(report.coverage, CoverageStateV1::Known, "{report:#?}");
    assert!(report.raw_rollup_equal, "{report:#?}");
}

pub mod work_rollup_harness {
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use tracedecay_application::{
        AdjudicateWorkLeakCommandV1, CancellationContext, CapabilityGrantId,
        CapabilityGrantSnapshot, Deadline, DisclosureClass, ExecutionTopologyMetricsRequestV1,
        ExecutionTopologyMetricsV1, ExecutionTopologyRollupFragmentQueryV1,
        ExecutionTopologyRollupFragmentV1, ExecutionTopologyRollupQueryPort,
        ObservabilityHorizonV1, ObservabilityQueryPort, ObservabilityQueryV1, RequestContext,
        RequestId, ResolvedScope, RetryWorkAttemptCommandV1, VerifiedWorkLeakEvidenceV1,
        VerifiedWorkRetryFailureV1, WorkLeakAdjudicationReceiptV1, WorkRetryCauseV1,
        WorkRetryFailureSelectorV1, WorkRetryReceiptV1, WorkRetrySourceV1,
        canonical_execution_topology_rollup_fragment_bytes, execution_topology_rollup_metrics,
    };
    use tracedecay_domain::{
        ActorId, AttemptId, CoverageStateV1, DuplicateEffectOutcomeV1, DuplicateEffortKindV1,
        LeakOwnerClassV1, ManifestDigest, ProjectId, ProjectionGenerationId,
        QuantityEvidenceClassV1, RepositoryId, RunId, TaskId, UtcMicros, WorkAttemptIdentityV1,
        WorkAuthority, WorkBlockedIntervalCauseV1, WorkBlockedIntervalClosureV1,
        WorkBlockedIntervalIdentityV1, WorkBlockedIntervalReceiptV1, WorkCommandId,
        WorkDuplicateAdjudicationCommandV1, WorkDuplicateAdjudicationEvidenceV1,
        WorkDuplicateAdjudicationQuantitiesV1, WorkDuplicateAdjudicationReceiptV1,
        WorkDuplicateAdjudicationRevisionV1, WorkExecutionLeakKindV1, WorkExecutionLeakRecoveryV1,
        WorkRunControlAuthorityV1, WorkRunControlReasonV1, WorkTopologyGenerationRefV1,
        WorkflowStepId, WorktreeId, canonical_sha256,
    };
    use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};
    use tracedecay_usecases::observability::{
        BoundedObservabilityProducerV1, ObservabilityProducerIdentityV1,
        RegisteredObservabilityPortV1, WorkOwnerObservationResultV1,
        record_work_blocked_interval_observation, record_work_duplicate_observation,
        record_work_leak_observation, record_work_retry_observation,
    };

    pub const SOURCE_COUNT: usize = 512;
    pub const PRODUCER_CAPACITY: usize = 1_024;
    pub const MAX_EVENTS: u32 = 10_000;
    pub const TRIPWIRE: Duration = Duration::from_secs(2);
    pub const READ_TRIPWIRE: Duration = Duration::from_millis(250);

    const FAMILY_SOURCE_COUNT: usize = SOURCE_COUNT / 4;
    const SCOPE: &str = "project.work-rollup.rc01";
    const DAY_START_SECONDS: i64 = 86_400;
    const DAY_MICROS: i64 = 86_400_000_000;
    const DAY_START_MICROS: i64 = DAY_START_SECONDS * 1_000_000;

    #[derive(Clone, Debug)]
    pub struct WorkRollupReport {
        pub offered_sources: usize,
        pub dropped_sources: usize,
        pub durable_sources: usize,
        pub fragment_count: usize,
        pub fragment_coverage: CoverageStateV1,
        pub fragment_is_application_canonical: bool,
        pub raw_coverage: CoverageStateV1,
        pub coverage: CoverageStateV1,
        pub raw_watermark: String,
        pub rollup_watermark: String,
        pub raw_rollup_equal: bool,
        pub setup_elapsed: Duration,
        pub offer_elapsed: Duration,
        pub fragment_ready_elapsed: Duration,
        pub raw_read_elapsed: Duration,
        pub application_read_elapsed: Duration,
        pub total_elapsed: Duration,
    }

    fn id<T>(value: impl Into<String>) -> T
    where
        T: TryFrom<String>,
        T::Error: std::fmt::Debug,
    {
        T::try_from(value.into()).expect("valid Work rollup identifier")
    }

    fn attempt(index: usize, suffix: &str) -> WorkAttemptIdentityV1 {
        WorkAttemptIdentityV1::new(
            id::<TaskId>(format!("task.work-rollup.{index}")),
            id::<RunId>(format!("run.work-rollup.{index}")),
            id::<AttemptId>(format!("attempt.work-rollup.{index}.{suffix}")),
        )
        .expect("valid Work attempt identity")
    }

    fn blocked_receipt(index: usize, occurred_at: i64) -> WorkBlockedIntervalReceiptV1 {
        WorkBlockedIntervalReceiptV1::opened(
            WorkBlockedIntervalIdentityV1::new(
                id::<TaskId>(format!("task.work-rollup.blocked.{index}")),
                id::<RunId>(format!("run.work-rollup.blocked.{index}")),
                id::<AttemptId>(format!("attempt.work-rollup.blocked.{index}")),
                id::<WorkflowStepId>(format!("step.work-rollup.blocked.{index}")),
            ),
            WorkBlockedIntervalCauseV1::new(
                WorkRunControlReasonV1::BudgetExhausted,
                WorkRunControlAuthorityV1::FIRST,
            ),
            UtcMicros(occurred_at),
        )
        .expect("open blocked interval")
        .close(
            UtcMicros(occurred_at + 100),
            WorkBlockedIntervalClosureV1::AttemptTerminal,
        )
        .expect("close blocked interval")
    }

    fn retry_receipt(index: usize, occurred_at: i64) -> WorkRetryReceiptV1 {
        let original = attempt(index, "retry-original");
        let retried = attempt(index, "retry-new");
        let selector = WorkRetryFailureSelectorV1 {
            source: WorkRetrySourceV1::Runtime,
            cause: WorkRetryCauseV1::RuntimeFailure,
            evidence_ref: format!("runtime-terminal:work-rollup-{index}"),
        };
        let command = RetryWorkAttemptCommandV1 {
            original_attempt: original,
            new_attempt_id: retried.attempt_id().clone(),
            failure: selector.clone(),
            command_id: id::<WorkCommandId>(format!("command.work-rollup.retry.{index}")),
        };
        WorkRetryReceiptV1::new(
            command,
            VerifiedWorkRetryFailureV1 {
                selector,
                evidence_digest: canonical_sha256(&("work-rollup-retry-evidence.v1", index))
                    .expect("retry evidence digest"),
                observed_at: UtcMicros(occurred_at),
            },
            retried,
            UtcMicros(occurred_at),
            UtcMicros(occurred_at + 100),
        )
        .expect("canonical retry receipt")
    }

    fn leak_receipt(index: usize, occurred_at: i64) -> WorkLeakAdjudicationReceiptV1 {
        let leak_attempt = attempt(index, "leak");
        let command = AdjudicateWorkLeakCommandV1 {
            adjudication_id: format!("adjudication.work-rollup.leak.{index}"),
            expected_revision: None,
            attempt: leak_attempt.clone(),
            detection_horizon_micros: 1_000,
            command_id: id::<WorkCommandId>(format!("command.work-rollup.leak.{index}")),
        };
        let evidence = VerifiedWorkLeakEvidenceV1 {
            attempt: leak_attempt,
            kind: WorkExecutionLeakKindV1::AttemptWithoutLiveOwner,
            recovery: WorkExecutionLeakRecoveryV1::Pending,
            owner_class: LeakOwnerClassV1::Work,
            coverage: CoverageStateV1::Known,
            detection_horizon_micros: command.detection_horizon_micros,
            scan_started_at: UtcMicros(occurred_at),
            scan_completed_at: UtcMicros(occurred_at + 100),
            evidence_refs: vec![format!("owner-receipt:work-rollup-leak-{index}")],
        };
        let scan_deadline = UtcMicros(occurred_at + 200);
        WorkLeakAdjudicationReceiptV1 {
            canonical_input_digest: canonical_sha256(&(
                "tracedecay.application.work-leak-adjudication.v1",
                &command,
                &evidence,
                scan_deadline,
            ))
            .expect("leak input digest"),
            command,
            revision: 1,
            evidence,
            scan_deadline,
        }
    }

    fn duplicate_receipt(
        authority: &WorkAuthority,
        work_generation: &ProjectionGenerationId,
        topology_generation: &WorkTopologyGenerationRefV1,
        index: usize,
        occurred_at: i64,
    ) -> WorkDuplicateAdjudicationReceiptV1 {
        let command = WorkDuplicateAdjudicationCommandV1 {
            expected_revision: None,
            first_attempt: attempt(index, "duplicate-a"),
            second_attempt: attempt(index, "duplicate-b"),
            evidence: WorkDuplicateAdjudicationEvidenceV1 {
                work_generation: work_generation.clone(),
                topology_generation: topology_generation.clone(),
            },
            verdict: DuplicateEffortKindV1::ExactDuplicate,
            quantities: WorkDuplicateAdjudicationQuantitiesV1 {
                wall_micros: Some(1_000),
                token_count: Some(10),
                cost_micros: Some(5),
                test_count: Some(1),
                effect_count: Some(1),
                evidence: QuantityEvidenceClassV1::OwnerReceipt,
                effect_outcome: DuplicateEffectOutcomeV1::NotApplicable,
                coverage: CoverageStateV1::Known,
            },
            reason: "independent owner-receipt review".to_owned(),
            command_id: id::<WorkCommandId>(format!("command.work-rollup.duplicate.{index}")),
            occurred_at: UtcMicros(occurred_at),
        }
        .canonicalized();
        let input_digest = command
            .canonical_input_digest()
            .expect("duplicate input digest");
        WorkDuplicateAdjudicationReceiptV1::new(
            authority,
            command,
            WorkDuplicateAdjudicationRevisionV1::initial(),
            input_digest,
        )
        .expect("canonical duplicate receipt")
    }

    fn work_authority() -> WorkAuthority {
        WorkAuthority::new(
            id::<ProjectId>(SCOPE),
            id::<RepositoryId>("repository.work-rollup.rc01"),
            id::<WorktreeId>("worktree.work-rollup.rc01"),
            id::<ActorId>("actor.work-rollup.owner"),
            ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("policy digest"),
        )
        .expect("Work authority")
    }

    fn read_context() -> RequestContext {
        let scope = ResolvedScope::new(
            id::<ProjectId>(SCOPE),
            id::<RepositoryId>("repository.work-rollup.rc01"),
            id::<WorktreeId>("worktree.work-rollup.rc01"),
            None,
        )
        .expect("resolved Work scope");
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.work-rollup.rc01").expect("grant id"),
            1,
            ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).expect("grant digest"),
            id::<ActorId>("actor.work-rollup.issuer"),
            UtcMicros(1),
            UtcMicros(i64::MAX),
            scope.clone(),
            BTreeSet::from([
                CapabilityId::new("capability.work.topology_metrics").expect("capability")
            ]),
            BTreeSet::from([UseCaseId::new("use-case.work.topology_metrics").expect("use case")]),
            DisclosureClass::Sensitive,
        )
        .expect("grant snapshot");
        RequestContext::new(
            id::<ActorId>("actor.work-rollup.reader"),
            scope,
            grant,
            RequestId::new("request.work-rollup.read").expect("request id"),
            Deadline::new(UtcMicros(i64::MAX)).expect("read deadline"),
            CancellationContext::active("cancel.work-rollup.read").expect("cancellation"),
        )
        .expect("read context")
    }

    fn account(outcome: WorkOwnerObservationResultV1, dropped: &mut usize) {
        match outcome {
            WorkOwnerObservationResultV1::Enqueued => {}
            WorkOwnerObservationResultV1::DroppedAtCapacity => *dropped += 1,
            WorkOwnerObservationResultV1::Unavailable => {
                panic!("real Work owner receipt was rejected by its observation adapter")
            }
        }
    }

    fn equivalent_metrics(
        raw: &ExecutionTopologyMetricsV1,
        rollup: &ExecutionTopologyMetricsV1,
    ) -> bool {
        let mut raw = raw.clone();
        let mut rollup = rollup.clone();
        let normalized_horizon = ObservabilityHorizonV1 {
            since_micros: 0,
            until_micros: DAY_MICROS,
        };
        for model in [&mut raw, &mut rollup] {
            model.horizon = normalized_horizon.clone();
            model.observed_at_micros = 0;
            model.watermark = "normalized-watermark".to_owned();
            model.drill_anchors.clear();
            for measurement in &mut model.measurements {
                measurement.value.provenance.watermark = "normalized-watermark".to_owned();
                measurement.value.temporal.horizon = normalized_horizon.clone();
            }
        }
        raw == rollup
    }

    pub async fn run_work_rollup_case() -> WorkRollupReport {
        let total_started = Instant::now();
        let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
        let runtime = RegisteredGlobalDbTestRuntime::profile(
            tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
        )
        .await
        .expect("registered fresh-store runtime");
        let database = runtime.profile_database_arc();
        let producer = BoundedObservabilityProducerV1::start(
            Arc::clone(&database),
            ObservabilityProducerIdentityV1 {
                authorized_scope_ref: SCOPE.to_owned(),
                process_boot_id: "boot.work-rollup.rc01".to_owned(),
                producer_revision: "producer.work-rollup.rc01".to_owned(),
                configuration_revision: "configuration.work-rollup.rc01".to_owned(),
                policy_revision: "policy.work-rollup.rc01".to_owned(),
            },
            PRODUCER_CAPACITY,
        )
        .expect("bounded observability producer");
        let authority = work_authority();
        let work_generation = authority
            .projection_generation_id()
            .expect("Work projection generation");
        let topology_generation =
            id::<WorkTopologyGenerationRefV1>(format!("sha256:{}", "c".repeat(64)));
        let setup_elapsed = total_started.elapsed();

        let mut dropped_sources = 0;
        let offer_started = Instant::now();
        for index in 0..FAMILY_SOURCE_COUNT {
            let occurred_at = DAY_START_MICROS
                + 1_000_000
                + i64::try_from(index).expect("bounded receipt index") * 1_000_000;
            account(
                record_work_blocked_interval_observation(
                    Some(&producer),
                    SCOPE,
                    &blocked_receipt(index, occurred_at),
                ),
                &mut dropped_sources,
            );
            account(
                record_work_retry_observation(
                    Some(&producer),
                    SCOPE,
                    &retry_receipt(index, occurred_at + 200),
                ),
                &mut dropped_sources,
            );
            account(
                record_work_leak_observation(
                    Some(&producer),
                    SCOPE,
                    &leak_receipt(index, occurred_at + 400),
                ),
                &mut dropped_sources,
            );
            account(
                record_work_duplicate_observation(
                    Some(&producer),
                    SCOPE,
                    &authority,
                    &duplicate_receipt(
                        &authority,
                        &work_generation,
                        &topology_generation,
                        index,
                        occurred_at + 700,
                    ),
                ),
                &mut dropped_sources,
            );
        }
        let offer_elapsed = offer_started.elapsed();

        let fragment_wait_started = Instant::now();
        producer
            .shutdown()
            .await
            .expect("flush owner receipts and rebuild idle rollup");
        let port = RegisteredObservabilityPortV1::new(database.as_ref());
        let full_day = ObservabilityHorizonV1 {
            since_micros: DAY_START_MICROS,
            until_micros: DAY_START_MICROS + DAY_MICROS,
        };
        let fragment_page = tokio::time::timeout(TRIPWIRE, async {
            loop {
                let page = port
                    .query_rollup_fragments(ExecutionTopologyRollupFragmentQueryV1 {
                        authorized_scope_ref: SCOPE.to_owned(),
                        horizon: full_day.clone(),
                    })
                    .await
                    .expect("registered rollup fragment query");
                if !page.fragment_documents.is_empty() {
                    break page;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("idle rollup rebuild within two seconds");
        let fragment_ready_elapsed = fragment_wait_started.elapsed();

        let source_page = port
            .query(ObservabilityQueryV1 {
                authorized_scope_ref: SCOPE.to_owned(),
                event_kinds: tracedecay_application::EXECUTION_TOPOLOGY_EVENT_KINDS_V1
                    .iter()
                    .map(|kind| (*kind).to_owned())
                    .collect(),
                horizon: full_day.clone(),
                after_watermark: None,
                limit: MAX_EVENTS,
            })
            .await
            .expect("registered durable source query");
        assert!(source_page.next_watermark.is_none());
        assert_eq!(source_page.coverage, CoverageStateV1::Known);

        let raw_horizon = ObservabilityHorizonV1 {
            since_micros: DAY_START_MICROS + 1,
            until_micros: DAY_START_MICROS + DAY_MICROS - 1,
        };
        let context = read_context();
        let raw_read_started = Instant::now();
        let raw = execution_topology_rollup_metrics(
            &port,
            &port,
            &context,
            &ExecutionTopologyMetricsRequestV1 {
                horizon: raw_horizon,
                max_events: MAX_EVENTS,
            },
        )
        .await
        .expect("application raw-boundary metrics read");
        let raw_read_elapsed = raw_read_started.elapsed();
        let application_read_started = Instant::now();
        let rollup = execution_topology_rollup_metrics(
            &port,
            &port,
            &context,
            &ExecutionTopologyMetricsRequestV1 {
                horizon: full_day,
                max_events: MAX_EVENTS,
            },
        )
        .await
        .expect("application retained-rollup metrics read");
        let application_read_elapsed = application_read_started.elapsed();
        let stored_fragment = fragment_page
            .fragment_documents
            .first()
            .expect("one registered fragment document");
        let typed_fragment =
            serde_json::from_str::<ExecutionTopologyRollupFragmentV1>(stored_fragment)
                .ok()
                .and_then(|fragment| {
                    canonical_execution_topology_rollup_fragment_bytes(&fragment).ok()
                });
        if let Some(directory) = std::env::var_os("TRACEDECAY_WORK_ROLLUP_DIAGNOSTICS") {
            let directory = std::path::PathBuf::from(directory);
            std::fs::create_dir_all(&directory).expect("create Work rollup diagnostics directory");
            std::fs::write(directory.join("stored-fragment.json"), stored_fragment)
                .expect("write stored fragment diagnostic");
            std::fs::write(
                directory.join("typed-fragment.json"),
                typed_fragment.as_deref().unwrap_or_default(),
            )
            .expect("write typed fragment diagnostic");
        }
        let fragment_is_application_canonical = typed_fragment
            .as_ref()
            .is_some_and(|canonical| canonical == stored_fragment.as_bytes());

        WorkRollupReport {
            offered_sources: SOURCE_COUNT,
            dropped_sources,
            durable_sources: source_page.events.len(),
            fragment_count: fragment_page.fragment_documents.len(),
            fragment_coverage: fragment_page.coverage,
            fragment_is_application_canonical,
            raw_coverage: raw.coverage.state,
            coverage: rollup.coverage.state,
            raw_watermark: raw.watermark.clone(),
            rollup_watermark: rollup.watermark.clone(),
            raw_rollup_equal: equivalent_metrics(&raw, &rollup),
            setup_elapsed,
            offer_elapsed,
            fragment_ready_elapsed,
            raw_read_elapsed,
            application_read_elapsed,
            total_elapsed: total_started.elapsed(),
        }
    }
}
