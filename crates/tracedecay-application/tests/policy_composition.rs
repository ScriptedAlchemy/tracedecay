use std::collections::{BTreeMap, BTreeSet};

use tracedecay_application::feedback::feedback_surface_operation;
use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    PolicyEvaluationContextV1, PolicyEvaluatorCompositionV1, PolicyEvidenceAgreementV1,
    PolicyEvidenceFrontierV1, PolicyEvidenceHorizonV1, RequestContext, RequestId, ResolvedScope,
    git_index_handler_descriptors,
};
use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotV1};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RefId, RepositoryId, ShardId, UtcMicros, VectorWatermark,
    WorktreeId,
};
use tracedecay_policy::routing::{
    CapabilityAvailabilityV1, CapabilityEffectClassV1, CapabilityRoutingDispositionV1,
    CapabilityRoutingReasonV1, ScopeMatchV1, TruthFreshnessRequirementV1, TruthSourceStateV1,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}

fn evaluation_context_for(
    capability: CapabilityId,
    use_case: UseCaseId,
) -> PolicyEvaluationContextV1 {
    let scope = ResolvedScope::new(
        id::<ProjectId>("project.policy.fixture"),
        id::<RepositoryId>("repository.policy.fixture"),
        id::<WorktreeId>("worktree.policy.fixture"),
        Some(id::<RefId>("refs/heads/policy-fixture")),
    )
    .unwrap();
    let actor = id::<ActorId>("actor.policy.fixture");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.policy.fixture").unwrap(),
        1,
        digest('a'),
        actor.clone(),
        UtcMicros(1),
        UtcMicros(100),
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Evidence,
    )
    .unwrap();
    let request = RequestContext::new(
        actor,
        scope,
        grant,
        RequestId::new("request.policy.fixture").unwrap(),
        Deadline::new(UtcMicros(90)).unwrap(),
        CancellationContext::active("cancellation.policy.fixture").unwrap(),
    )
    .unwrap();
    PolicyEvaluationContextV1::new(
        request,
        id::<ConfigurationRevisionId>("configuration.revision.policy.fixture"),
        ConfigurationSnapshotV1::new(BTreeMap::new(), BTreeMap::new()).unwrap(),
        7,
        digest('b'),
    )
    .unwrap()
}

fn evaluation_context() -> PolicyEvaluationContextV1 {
    evaluation_context_for(
        CapabilityId::new("capability.application.feedback.diagnostics").unwrap(),
        UseCaseId::new("use-case.application.feedback.diagnostics").unwrap(),
    )
}

fn watermark(shard: &str, sequence: u64) -> VectorWatermark {
    VectorWatermark {
        components: BTreeMap::from([(ShardId::new(shard).unwrap(), sequence)]),
    }
}

fn matching_horizon(state: TruthSourceStateV1) -> PolicyEvidenceHorizonV1 {
    PolicyEvidenceHorizonV1 {
        local_session: PolicyEvidenceFrontierV1 {
            watermark: watermark("local-session", 11),
            state,
        },
        live_git: PolicyEvidenceFrontierV1 {
            watermark: watermark("live-git", 7),
            state,
        },
        agreement: PolicyEvidenceAgreementV1::Agree,
    }
}

#[test]
fn production_composition_preserves_static_unavailability_for_policy() {
    let composition = PolicyEvaluatorCompositionV1::from_application_catalog().unwrap();

    for capability_id in [
        "capability.application.feedback.diagnostics",
        "capability.application.feedback.github-review-ingest",
        "capability.application.feedback.ci-failure-localize",
        "capability.application.feedback.proximity",
    ] {
        assert!(
            composition.registered_capability(capability_id).is_some(),
            "{capability_id} has a registered callable handler"
        );
    }
    // symbol-search is a callable production retrieval capability: its catalog
    // contribution is `AvailabilityContract::Available` and it carries a
    // registered typed handler descriptor (activated by "feat(search): complete
    // production retrieval activation"). It is therefore projected into a
    // callable policy route, exactly like the feedback capabilities above.
    assert!(
        composition
            .registered_capability("capability.retrieval.symbol-search")
            .is_some()
    );
    // An inert handler remains non-invocable at its transport surfaces, but
    // policy must retain the catalog fact so a direct route is typed
    // `CapabilityUnavailable` rather than rejected as an unknown route.
    let operation = git_index_handler_descriptors()
        .unwrap()
        .into_iter()
        .find(|descriptor| {
            descriptor.operation().capability_id().as_str() == "capability.git.stage-hunks"
        })
        .unwrap()
        .operation()
        .clone();
    let context = evaluation_context_for(
        operation.capability_id().clone(),
        operation.use_case_id().clone(),
    );
    let evaluation = operation
        .evaluate_local_live_policy(
            &composition,
            &context,
            CapabilityAvailabilityV1::Available,
            ScopeMatchV1::Match,
            TruthSourceStateV1::Fresh,
            CapabilityEffectClassV1::GitIndexStage,
            TruthFreshnessRequirementV1::Fresh,
            matching_horizon(TruthSourceStateV1::Fresh),
            UtcMicros(10),
        )
        .unwrap();
    assert_eq!(
        evaluation.decision.disposition,
        CapabilityRoutingDispositionV1::Indeterminate
    );
    assert_eq!(
        evaluation.decision.ordered_reason_codes,
        vec![CapabilityRoutingReasonV1::CapabilityUnavailable]
    );
}

#[test]
fn callable_route_preserves_runtime_unavailability_and_snapshot_digests() {
    let composition = PolicyEvaluatorCompositionV1::from_application_catalog().unwrap();
    let context = evaluation_context();
    let operation = feedback_surface_operation("feedback_diagnostics")
        .unwrap()
        .unwrap();

    for (availability, reason) in [
        (
            CapabilityAvailabilityV1::Unavailable,
            CapabilityRoutingReasonV1::CapabilityUnavailable,
        ),
        (
            CapabilityAvailabilityV1::Stale,
            CapabilityRoutingReasonV1::CapabilityStale,
        ),
        (
            CapabilityAvailabilityV1::Unknown,
            CapabilityRoutingReasonV1::CapabilityUnknown,
        ),
    ] {
        let evaluation = operation
            .evaluate_local_live_policy(
                &composition,
                &context,
                availability,
                ScopeMatchV1::Match,
                TruthSourceStateV1::Fresh,
                CapabilityEffectClassV1::Read,
                TruthFreshnessRequirementV1::Fresh,
                matching_horizon(TruthSourceStateV1::Fresh),
                UtcMicros(10),
            )
            .unwrap();

        assert_eq!(
            evaluation.decision.disposition,
            CapabilityRoutingDispositionV1::Indeterminate
        );
        assert_eq!(evaluation.decision.ordered_reason_codes, vec![reason]);
        assert_eq!(
            evaluation.decision.configuration_digest,
            context.configuration().effective_behavior_digest
        );
        assert_eq!(
            evaluation.context.request().grant().digest,
            context.request().grant().digest
        );
    }
}

#[test]
fn callable_route_returns_typed_denial_for_a_missing_operation_grant() {
    let composition = PolicyEvaluatorCompositionV1::from_application_catalog().unwrap();
    let operation = feedback_surface_operation("feedback_diagnostics")
        .unwrap()
        .unwrap();
    let context = evaluation_context_for(
        CapabilityId::new("capability.application.feedback.get").unwrap(),
        operation.use_case_id().clone(),
    );

    let evaluation = operation
        .evaluate_local_live_policy(
            &composition,
            &context,
            CapabilityAvailabilityV1::Available,
            ScopeMatchV1::Match,
            TruthSourceStateV1::Fresh,
            CapabilityEffectClassV1::Read,
            TruthFreshnessRequirementV1::Fresh,
            matching_horizon(TruthSourceStateV1::Fresh),
            UtcMicros(10),
        )
        .unwrap();

    assert_eq!(
        evaluation.decision.disposition,
        CapabilityRoutingDispositionV1::Deny
    );
    assert_eq!(
        evaluation.decision.ordered_reason_codes,
        vec![CapabilityRoutingReasonV1::CapabilityNotAuthorized]
    );
    assert_eq!(
        evaluation.context.request().grant().digest,
        context.request().grant().digest
    );
    assert_eq!(
        evaluation.decision.configuration_digest,
        context.configuration().effective_behavior_digest
    );
}

#[test]
fn local_live_disagreement_preserves_both_independent_watermarks() {
    let composition = PolicyEvaluatorCompositionV1::from_application_catalog().unwrap();
    let context = evaluation_context();
    let candidate = composition
        .candidate(
            "capability.application.feedback.diagnostics",
            CapabilityAvailabilityV1::Available,
            ScopeMatchV1::Match,
            TruthSourceStateV1::Partial,
        )
        .unwrap();
    let capability = candidate.capability_id.clone();
    let request = composition
        .routing_request(
            &context,
            &UseCaseId::new("use-case.application.feedback.diagnostics").unwrap(),
            vec![capability],
            vec![candidate],
            CapabilityEffectClassV1::Read,
            TruthFreshnessRequirementV1::FreshOrPartial,
            UtcMicros(10),
        )
        .unwrap();
    let horizon = PolicyEvidenceHorizonV1 {
        local_session: PolicyEvidenceFrontierV1 {
            watermark: watermark("local-session", 11),
            state: TruthSourceStateV1::Fresh,
        },
        live_git: PolicyEvidenceFrontierV1 {
            watermark: watermark("live-git", 7),
            state: TruthSourceStateV1::Partial,
        },
        agreement: PolicyEvidenceAgreementV1::Disagree,
    };

    let evaluation = composition
        .route_local_live(&context, &request, horizon.clone())
        .unwrap();

    assert_eq!(
        evaluation.decision.disposition,
        CapabilityRoutingDispositionV1::Allow
    );
    assert_eq!(evaluation.evidence_horizon, Some(horizon));
    assert_eq!(evaluation.context.scope(), context.scope());
}

#[test]
fn routing_rejects_a_substituted_configuration_snapshot() {
    let composition = PolicyEvaluatorCompositionV1::from_application_catalog().unwrap();
    let context = evaluation_context();
    let candidate = composition
        .candidate(
            "capability.application.feedback.diagnostics",
            CapabilityAvailabilityV1::Available,
            ScopeMatchV1::Match,
            TruthSourceStateV1::Fresh,
        )
        .unwrap();
    let capability = candidate.capability_id.clone();
    let mut request = composition
        .routing_request(
            &context,
            &UseCaseId::new("use-case.application.feedback.diagnostics").unwrap(),
            vec![capability],
            vec![candidate],
            CapabilityEffectClassV1::Read,
            TruthFreshnessRequirementV1::Fresh,
            UtcMicros(10),
        )
        .unwrap();
    request.configuration_digest = digest('f');

    assert!(
        composition
            .route_local_live(
                &context,
                &request,
                matching_horizon(TruthSourceStateV1::Fresh),
            )
            .is_err()
    );
}

#[test]
fn routing_returns_typed_cancellation_from_the_bound_request_authority() {
    let composition = PolicyEvaluatorCompositionV1::from_application_catalog().unwrap();
    let active = evaluation_context();
    let context = PolicyEvaluationContextV1::new(
        active.request().clone().with_cancellation(
            CancellationContext::cancelled("cancellation.policy.fixture", UtcMicros(9)).unwrap(),
        ),
        active.configuration_revision().clone(),
        active.configuration().clone(),
        active.policy_revision(),
        active.policy_digest().clone(),
    )
    .unwrap();
    let candidate = composition
        .candidate(
            "capability.application.feedback.diagnostics",
            CapabilityAvailabilityV1::Available,
            ScopeMatchV1::Match,
            TruthSourceStateV1::Fresh,
        )
        .unwrap();
    let request = composition
        .routing_request(
            &context,
            &UseCaseId::new("use-case.application.feedback.diagnostics").unwrap(),
            vec![candidate.capability_id.clone()],
            vec![candidate],
            CapabilityEffectClassV1::Read,
            TruthFreshnessRequirementV1::Fresh,
            UtcMicros(10),
        )
        .unwrap();

    let evaluation = composition
        .route_local_live(
            &context,
            &request,
            matching_horizon(TruthSourceStateV1::Fresh),
        )
        .unwrap();
    assert_eq!(
        evaluation.decision.ordered_reason_codes,
        vec![CapabilityRoutingReasonV1::RequestCancelled]
    );
}
