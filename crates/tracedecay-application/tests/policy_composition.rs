use std::collections::{BTreeMap, BTreeSet};

use tracedecay_application::{
    CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    PolicyConsumerV1, PolicyEvaluationContextV1, PolicyEvaluatorCompositionV1,
    PolicyEvidenceAgreementV1, PolicyEvidenceFrontierV1, PolicyEvidenceHorizonV1, RequestContext,
    RequestId, ResolvedScope,
};
use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotV1};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, RefId, RepositoryId, ShardId, UtcMicros, VectorWatermark,
    WorktreeId,
};
use tracedecay_policy::routing::{
    CapabilityEffectClassV1, CapabilityRoutingDispositionV1, CapabilityRoutingRequestV1,
    ScopeMatchV1, TruthFreshnessRequirementV1, TruthSourceStateV1,
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

fn evaluation_context() -> PolicyEvaluationContextV1 {
    let scope = ResolvedScope::new(
        id::<ProjectId>("project.policy.fixture"),
        id::<RepositoryId>("repository.policy.fixture"),
        id::<WorktreeId>("worktree.policy.fixture"),
        Some(id::<RefId>("refs/heads/policy-fixture")),
    )
    .unwrap();
    let capability = CapabilityId::new("capability.application.feedback.diagnostics").unwrap();
    let use_case = UseCaseId::new("use-case.application.feedback.diagnostics").unwrap();
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

fn watermark(shard: &str, sequence: u64) -> VectorWatermark {
    VectorWatermark {
        components: BTreeMap::from([(ShardId::new(shard).unwrap(), sequence)]),
    }
}

#[test]
fn production_composition_registers_only_callable_catalog_handlers() {
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
    // stage-hunks stays inert (`AvailabilityContract::Unavailable`) and never
    // becomes a callable route, so unavailable metadata is not registered.
    assert!(
        composition
            .registered_capability("capability.git.stage-hunks")
            .is_none()
    );
}

#[test]
fn local_live_disagreement_preserves_both_independent_watermarks() {
    let composition = PolicyEvaluatorCompositionV1::from_application_catalog().unwrap();
    let context = evaluation_context();
    let candidate = composition
        .candidate(
            "capability.application.feedback.diagnostics",
            ScopeMatchV1::Match,
            TruthSourceStateV1::Partial,
            0,
        )
        .unwrap();
    let capability = candidate.capability_id.clone();
    let request = CapabilityRoutingRequestV1 {
        declared_capability_order: vec![capability.clone()],
        candidates: vec![candidate],
        authorized_capabilities: BTreeSet::from([capability]),
        required_effect_class: CapabilityEffectClassV1::Read,
        required_freshness: TruthFreshnessRequirementV1::FreshOrPartial,
        policy_revision: context.policy_revision(),
        policy_digest: context.policy_digest().clone(),
        configuration_digest: context.configuration().effective_behavior_digest.clone(),
        evaluated_at: UtcMicros(10),
    };
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
        .route(
            PolicyConsumerV1::LocalLiveCorrelation,
            &context,
            &request,
            Some(horizon.clone()),
        )
        .unwrap();

    assert_eq!(
        evaluation.decision.disposition,
        CapabilityRoutingDispositionV1::Allow
    );
    assert_eq!(evaluation.evidence_horizon, Some(horizon));
    assert_eq!(evaluation.context.scope(), context.scope());
}

#[test]
fn routing_rejects_a_substituted_plan20_configuration_snapshot() {
    let composition = PolicyEvaluatorCompositionV1::from_application_catalog().unwrap();
    let context = evaluation_context();
    let candidate = composition
        .candidate(
            "capability.application.feedback.diagnostics",
            ScopeMatchV1::Match,
            TruthSourceStateV1::Fresh,
            0,
        )
        .unwrap();
    let capability = candidate.capability_id.clone();
    let request = CapabilityRoutingRequestV1 {
        declared_capability_order: vec![capability.clone()],
        candidates: vec![candidate],
        authorized_capabilities: BTreeSet::from([capability]),
        required_effect_class: CapabilityEffectClassV1::Read,
        required_freshness: TruthFreshnessRequirementV1::Fresh,
        policy_revision: context.policy_revision(),
        policy_digest: context.policy_digest().clone(),
        configuration_digest: digest('f'),
        evaluated_at: UtcMicros(10),
    };

    assert!(
        composition
            .route(PolicyConsumerV1::RetrievalRouting, &context, &request, None,)
            .is_err()
    );
}
