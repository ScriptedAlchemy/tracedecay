use std::collections::BTreeSet;
use std::time::Duration;

use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    ExecutionTopologyMetricsRequestV1, ObservabilityHorizonV1, ObservabilityRecordPort,
    RequestContext, RequestId, ResolvedScope, execution_topology_rollup_metrics,
};
use tracedecay_domain::{
    ActorId, CoverageStateV1, ExecutionPlacementV1, ExecutionTopologyKindV1,
    ExecutionTopologySampledV1, IntegrationStrategyV1, ManifestDigest, ObservabilityEnvelopeV1,
    ObservabilityPayloadV1, ObservabilityRetentionClassV1, ObservabilityTerminalResultV1,
    ProjectId, RepositoryId, ReviewTopologyV1, UtcMicros, WorkTopologyBranchV1, WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};
use tracedecay_usecases::observability::{
    BoundedObservabilityProducerV1, ObservabilityProducerIdentityV1, RegisteredObservabilityPortV1,
};

fn read_context(scope_ref: &str) -> RequestContext {
    let scope = ResolvedScope::new(
        ProjectId::new(scope_ref).expect("project id"),
        RepositoryId::new("repository.observability.rollup-convergence").expect("repository id"),
        WorktreeId::new("worktree.observability.rollup-convergence").expect("worktree id"),
        None,
    )
    .expect("resolved scope");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.observability.rollup-convergence").expect("grant id"),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("grant digest"),
        ActorId::new("actor.observability.rollup-convergence.issuer").expect("issuer"),
        UtcMicros(1),
        UtcMicros(i64::MAX),
        scope.clone(),
        BTreeSet::from(
            [CapabilityId::new("capability.work.topology_metrics").expect("capability")],
        ),
        BTreeSet::from([UseCaseId::new("use-case.work.topology_metrics").expect("use case")]),
        DisclosureClass::Sensitive,
    )
    .expect("grant snapshot");
    RequestContext::new(
        ActorId::new("actor.observability.rollup-convergence.reader").expect("reader"),
        scope,
        grant,
        RequestId::new("request.observability.rollup-convergence").expect("request id"),
        Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        CancellationContext::active("cancel.observability.rollup-convergence")
            .expect("cancellation"),
    )
    .expect("request context")
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
    let envelope = ObservabilityEnvelopeV1 {
        event_id: format!("event:{id}"),
        event_kind: payload.event_kind().to_owned(),
        schema_revision: 1,
        idempotency_key: format!("idempotency:{id}"),
        trace_id: format!("trace:{id}"),
        scope_ref: scope.to_owned(),
        capability: "retrieval".to_owned(),
        operation: "query".to_owned(),
        event_time_micros,
        observation_time_micros: event_time_micros,
        valid_from_micros: None,
        valid_until_micros: None,
        quantity: None,
        unit: None,
        terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
        producer_revision: "producer.v1".to_owned(),
        configuration_revision: "configuration.v1".to_owned(),
        policy_revision: "policy.v1".to_owned(),
        watermark: format!("boot:rollup-source:{id}"),
        coverage: CoverageStateV1::Known,
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count: 0,
        process_boot_id: "boot:rollup-source".to_owned(),
        producer_sequence: id,
        payload,
    };
    envelope.validate().expect("topology envelope");
    envelope
}

#[tokio::test]
async fn idle_producer_converges_dirty_days_into_application_readable_fragments() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project");
    let scope = "project.observability.rollup-convergence.v2";
    let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
        project.path(),
        tracedecay_domain::ProjectId::new(scope).expect("project identifier"),
    )
    .await
    .expect("registered runtime");
    let db = runtime.project_database_arc().expect("project database");
    let first_day = 86_400_i64;
    let port = RegisteredObservabilityPortV1::new(&db);
    for (offset, day) in [first_day, first_day + 86_400].into_iter().enumerate() {
        for sample in 1..=5_u64 {
            let id = u64::try_from(offset).expect("small day offset") * 10 + sample;
            port.record(topology_envelope(
                scope,
                id,
                day * 1_000_000 + i64::try_from(sample).expect("small sample"),
            ))
            .await
            .expect("record dirty-day topology source");
        }
    }

    let producer = BoundedObservabilityProducerV1::start(
        db.clone(),
        ObservabilityProducerIdentityV1 {
            authorized_scope_ref: scope.to_owned(),
            process_boot_id: "boot:rollup-idle-drain".to_owned(),
            producer_revision: "producer.rollup.v1".to_owned(),
            configuration_revision: "configuration.rollup.v1".to_owned(),
            policy_revision: "policy.rollup.v1".to_owned(),
        },
        4,
    )
    .expect("producer");
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let fragments = db
                .query_observability_rollup_fragments(
                    &tracedecay_global_db::ObservabilityRollupFragmentQueryV1 {
                        authorized_scope_ref: scope.to_owned(),
                        since_day_start_seconds: first_day,
                        until_day_start_seconds: first_day + 2 * 86_400,
                    },
                )
                .await
                .expect("query idle rollup convergence");
            if fragments.fragments.len() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("idle producer must converge every dirty day");

    let horizon = ObservabilityHorizonV1 {
        since_micros: first_day * 1_000_000,
        until_micros: (first_day + 2 * 86_400) * 1_000_000,
    };
    let model = execution_topology_rollup_metrics(
        &port,
        &port,
        &read_context(scope),
        &ExecutionTopologyMetricsRequestV1 {
            horizon,
            max_events: 10_000,
        },
    )
    .await
    .expect("registered application rollup read");
    assert_eq!(model.coverage.state, CoverageStateV1::Known);
    assert_ne!(model.watermark, "execution-topology:rollup-unavailable");

    producer.shutdown().await.expect("idle producer shutdown");
}
