use std::collections::BTreeSet;
use std::sync::Arc;

use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    ExecutionDurationBucketV1, ExecutionIntervalStateV1, ExecutionStackDriftKindV1,
    ExecutionTopologyDimensionV1, ExecutionTopologyMetricsRequestV1, ObservabilityHorizonV1,
    ObservabilityQueryPort, ObservabilityQueryV1, RequestContext, RequestId, ResolvedScope,
    execution_topology_rollup_metrics,
};
use tracedecay_domain::{
    ActorId, AnchorOwnerBindingV1, CoverageStateV1, DurationBucketV1, IntervalStateV1,
    ManifestDigest, PrivacyDomainId, ProjectId, ProviderId, RefId, RepositoryId, RetrievalAnchorId,
    StackDriftKindV1, UserProfileId, UtcMicros, WorkStackDriftObservedV1, WorktreeId,
    configuration::GitHubStackedPullRequestPolicyV1, safe_work_topology_policy_v1,
};
use tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};
use tracedecay_usecases::{
    advisory::GitHubStackObservabilityV1,
    observability::{
        BoundedObservabilityProducerV1, GitHubStackDriftObservationResultV1,
        GitHubStackDriftObservationUnavailableV1, GitHubStackProbeOwnerV1,
        ObservabilityProducerIdentityV1, RegisteredObservabilityPortV1, record_github_stack_drifts,
        recover_open_github_stack_drifts,
    },
    stack_coordinator::{
        DaemonGitHubStackCoordinatorV1, GitHubStackDriftObservationV1,
        GitHubStackProviderSourceBindingV1,
    },
};

fn resolved_scope(name: &str) -> ResolvedScope {
    ResolvedScope::new(
        ProjectId::new(format!("project.github-stack-drift.{name}")).expect("project id"),
        RepositoryId::new(format!("repository.github-stack-drift.{name}")).expect("repository id"),
        WorktreeId::new(format!("worktree.github-stack-drift.{name}")).expect("worktree id"),
        Some(RefId::new(format!("refs/heads/github-stack-drift-{name}")).expect("branch ref")),
    )
    .expect("scope")
}

fn source_binding(scope: &ResolvedScope) -> GitHubStackProviderSourceBindingV1 {
    GitHubStackProviderSourceBindingV1 {
        owner: AnchorOwnerBindingV1::for_project(
            UserProfileId::new("profile.github-stack-drift").expect("profile"),
            scope.project_id.clone(),
            PrivacyDomainId::new("privacy.github-stack-drift").expect("privacy domain"),
        )
        .expect("anchor owner"),
        capability_source_anchor_id: RetrievalAnchorId::new("anchor.github-stack-drift.capability")
            .expect("capability source anchor"),
        snapshot_source_anchor_id: None,
    }
}

fn producer_identity(project_id: &ProjectId) -> ObservabilityProducerIdentityV1 {
    ObservabilityProducerIdentityV1 {
        authorized_scope_ref: project_id.as_str().to_owned(),
        process_boot_id: "boot:github-stack-drift".to_owned(),
        producer_revision: "github-stack-drift-test.v1".to_owned(),
        configuration_revision: "github-stack-drift-config.v1".to_owned(),
        policy_revision: "github-stack-drift-policy.v1".to_owned(),
    }
}

fn read_context(scope: ResolvedScope) -> RequestContext {
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.github-stack-drift").expect("grant"),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("grant digest"),
        ActorId::new("actor.github-stack-drift.issuer").expect("issuer"),
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
        ActorId::new("actor.github-stack-drift.reader").expect("reader"),
        scope,
        grant,
        RequestId::new("request.github-stack-drift.read").expect("request id"),
        Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
        CancellationContext::active("cancel.github-stack-drift.read").expect("cancellation"),
    )
    .expect("request context")
}

fn drift(
    scope: &ResolvedScope,
    first_observed_micros: i64,
    observed_at: i64,
    state: IntervalStateV1,
) -> GitHubStackDriftObservationV1 {
    GitHubStackDriftObservationV1::from_persisted(
        &scope.scope_digest,
        UtcMicros(observed_at),
        WorkStackDriftObservedV1 {
            kind: StackDriftKindV1::BaseAdvanced,
            state,
            first_observed_micros,
            terminal_micros: (state == IntervalStateV1::Closed).then_some(observed_at),
            age_bucket: DurationBucketV1::Under1m,
            coverage: CoverageStateV1::Known,
        },
    )
    .expect("canonical drift observation")
}

/// The review refresh owner's Observatory lane (`GitHubStackObservabilityV1::
/// record`, invoked from `GitHubReviewRuntimeOwnerV1::refresh` after each
/// coordinator observation) must persist one capability receipt plus one
/// receipt per exact drift interval through the bounded producer.
#[tokio::test]
async fn review_owner_observability_lane_records_capability_and_drift_receipts() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project");
    let scope = resolved_scope("owner-lane");
    let runtime = RegisteredGlobalDbTestRuntime::project(
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
        project.path(),
        scope.project_id.clone(),
    )
    .await
    .expect("registered runtime");
    let database = runtime.project_database_arc().expect("project database");
    let producer = Arc::new(
        BoundedObservabilityProducerV1::start(
            database.clone(),
            producer_identity(&scope.project_id),
            64,
        )
        .expect("producer"),
    );
    let lane = GitHubStackObservabilityV1 {
        probe_owner: GitHubStackProbeOwnerV1::mount(
            scope.clone(),
            safe_work_topology_policy_v1(),
            "octo-org",
            "stack-repository",
            true,
        )
        .expect("stack probe owner"),
        producer: Arc::clone(&producer),
        observation_db: database.clone(),
    };
    let coordinator = DaemonGitHubStackCoordinatorV1::default();
    coordinator
        .register_scope(&scope, GitHubStackedPullRequestPolicyV1::Disabled)
        .expect("register scope");
    let source_binding = source_binding(&scope);
    let mut observation = coordinator
        .observe_policy(
            scope.clone(),
            ProviderId::new("provider.github").expect("provider"),
            source_binding.clone(),
            UtcMicros(1_000),
        )
        .expect("policy observation");
    let open = drift(&scope, 2_000, 2_000, IntervalStateV1::Open);
    observation.observed_at = open.observed_at;
    observation.drift_observations = vec![open];

    lane.record(&source_binding, &observation);

    producer.shutdown().await.expect("flush producer");
    drop(lane);
    drop(producer);
    let port = RegisteredObservabilityPortV1::new(database.as_ref());
    let query = |event_kind: &str| ObservabilityQueryV1 {
        authorized_scope_ref: scope.project_id.as_str().to_owned(),
        event_kinds: vec![event_kind.to_owned()],
        horizon: ObservabilityHorizonV1 {
            since_micros: 0,
            until_micros: 10_000,
        },
        after_watermark: None,
        limit: 16,
    };
    let capability_page = port
        .query(query("work.github_stack_capability.observed.v1"))
        .await
        .expect("capability receipts");
    assert_eq!(
        capability_page.events.len(),
        1,
        "one coordinator observation produces exactly one capability receipt"
    );
    let drift_page = port
        .query(query("work.stack_drift.observed.v1"))
        .await
        .expect("drift receipts");
    assert_eq!(
        drift_page.events.len(),
        1,
        "one open drift interval produces exactly one drift receipt"
    );
}

#[tokio::test]
async fn coordinator_drift_is_durable_closed_monotone_and_scope_denial_writes_nothing() {
    let _pin = tracedecay_runtime_core::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project");
    let scope = resolved_scope("durable");
    let runtime = RegisteredGlobalDbTestRuntime::project(
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
        project.path(),
        scope.project_id.clone(),
    )
    .await
    .expect("registered runtime");
    let database = runtime.project_database_arc().expect("project database");
    let producer = BoundedObservabilityProducerV1::start(
        database.clone(),
        producer_identity(&scope.project_id),
        64,
    )
    .expect("producer");
    let owner = GitHubStackProbeOwnerV1::mount(
        scope.clone(),
        safe_work_topology_policy_v1(),
        "octo-org",
        "stack-repository",
        true,
    )
    .expect("stack probe owner");
    let coordinator = DaemonGitHubStackCoordinatorV1::default();
    coordinator
        .register_scope(&scope, GitHubStackedPullRequestPolicyV1::Disabled)
        .expect("register scope");
    let source_binding = source_binding(&scope);
    let mut observation = coordinator
        .observe_policy(
            scope.clone(),
            ProviderId::new("provider.github").expect("provider"),
            source_binding.clone(),
            UtcMicros(1_000),
        )
        .expect("policy observation");

    for index in 0..5_i64 {
        let first = 2_000 + index * 10;
        for drift in [
            drift(&scope, first, first, IntervalStateV1::Open),
            drift(&scope, first, first + 2, IntervalStateV1::Closed),
            drift(&scope, first, first + 3, IntervalStateV1::Open),
        ] {
            observation.observed_at = drift.observed_at;
            observation.drift_observations = vec![drift];
            assert_eq!(
                record_github_stack_drifts(
                    database.as_ref(),
                    Some(&producer),
                    &owner,
                    &source_binding,
                    &observation
                ),
                GitHubStackDriftObservationResultV1::Emitted {
                    enqueued: 1,
                    dropped: 0,
                }
            );
        }
    }

    let surviving_open = drift(&scope, 3_000, 3_000, IntervalStateV1::Open);
    observation.observed_at = surviving_open.observed_at;
    observation.drift_observations = vec![surviving_open.clone()];
    assert_eq!(
        record_github_stack_drifts(
            database.as_ref(),
            Some(&producer),
            &owner,
            &source_binding,
            &observation,
        ),
        GitHubStackDriftObservationResultV1::Emitted {
            enqueued: 1,
            dropped: 0,
        }
    );

    let mut denied = observation.clone();
    denied.scope = resolved_scope("denied");
    assert_eq!(
        record_github_stack_drifts(
            database.as_ref(),
            Some(&producer),
            &owner,
            &source_binding,
            &denied,
        ),
        GitHubStackDriftObservationResultV1::Unavailable {
            event_kind: "work.stack_drift.observed.v1",
            reason: GitHubStackDriftObservationUnavailableV1::CoordinatorScopeMismatch,
        }
    );
    producer.shutdown().await.expect("flush producer");
    drop(producer);

    let port = RegisteredObservabilityPortV1::new(database.as_ref());
    let page = port
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: scope.project_id.as_str().to_owned(),
            event_kinds: vec!["work.stack_drift.observed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: 10_000,
            },
            after_watermark: None,
            limit: 64,
        })
        .await
        .expect("durable drift query");
    assert_eq!(
        page.events.len(),
        16,
        "denied observations never enter the outbox"
    );

    let model = execution_topology_rollup_metrics(
        &port,
        &port,
        &read_context(scope.clone()),
        &ExecutionTopologyMetricsRequestV1 {
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: 10_000,
            },
            max_events: 64,
        },
    )
    .await
    .expect("authorized topology rollup read");
    let expected_dimensions = vec![
        ExecutionTopologyDimensionV1::StackDriftKind(ExecutionStackDriftKindV1::BaseAdvanced),
        ExecutionTopologyDimensionV1::IntervalState(ExecutionIntervalStateV1::Closed),
        ExecutionTopologyDimensionV1::DurationBucket(ExecutionDurationBucketV1::Under1m),
    ];
    let cell = model
        .measurements
        .iter()
        .find(|measurement| {
            measurement.value.metric == "work_stale_stack_age_seconds"
                && measurement.dimensions == expected_dimensions
        })
        .expect("durable closed stack-drift rollup cell");
    assert_eq!(cell.value.value, Some(5.0));
    assert_eq!(cell.value.denominator, "observed_stack_drifts");

    let _ = port;
    drop(database);
    drop(runtime);
    let restarted_runtime = RegisteredGlobalDbTestRuntime::project(
        tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
        project.path(),
        scope.project_id.clone(),
    )
    .await
    .expect("restart registered runtime");
    let restarted_database = restarted_runtime
        .project_database_arc()
        .expect("restarted project database");
    let recovered =
        recover_open_github_stack_drifts(restarted_database.as_ref(), &scope, UtcMicros(10_000))
            .await
            .expect("canonical recovery projection after database restart");
    assert_eq!(
        recovered,
        vec![surviving_open],
        "closed intervals stay closed while the one latest-open interval survives"
    );
    let restarted = DaemonGitHubStackCoordinatorV1::default();
    restarted
        .register_scope(&scope, GitHubStackedPullRequestPolicyV1::Disabled)
        .expect("register restarted scope");
    restarted
        .restore_open_drift_interval(&scope, &recovered[0])
        .expect("restore exact open interval after restart");
}
