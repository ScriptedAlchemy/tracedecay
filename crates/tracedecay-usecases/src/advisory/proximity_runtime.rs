//! One-shot concurrent-work proximity provider.
//!
//! The provider consumes canonical observation/graph evidence, resolves the
//! effective Plan 20 threshold, and projects one Plan 09 contribution batch.
//! Shared feedback publication owns durable dedupe. This module creates no
//! fixture authority, evidence store, lock, task, schedule, or continuation.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracedecay_application::feedback::{
    FeedbackPortFuture, PROXIMITY_CAPABILITY_ID_V1, PROXIMITY_USE_CASE_ID_V1,
    ProximityEvaluationRequestV1,
};
use tracedecay_application::{
    AdvisoryFindingContributionBatchV1, AdvisoryFindingContributorV1,
    AdvisoryFindingValidityWindowV1, ApplicationContractError, RequestContext, now_micros,
};
use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationValueV1, SettingKey};
use tracedecay_domain::feedback::{
    FeedbackScopeV1, PROXIMITY_RISK_THRESHOLD_SETTING_KEY_V1, ProviderEvaluationStateV1,
    ProximityAddressV1, ProximityContributionIdV1, ProximityContributionV1, ProximityCoverageV1,
    ProximityInclusionV1, ProximityObservationIdV1, ProximityRelationPathV1, ProximityRiskInputsV1,
    ProximityTierV1, ProximityWarningClassV1, ProximityWarningIdV1,
};
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, ManifestDigest, RetrievalAnchorId, UtcMicros, canonical_sha256,
};

use crate::configuration::{ConfigurationControlStore, ConfigurationCurrentStateV1};

use super::context_allows_feedback_operation;

mod authority;

pub(crate) use authority::production_proximity_evidence_authority_v1;
pub use authority::{
    ProductionProximityEvidenceAuthorityV1, SharedCanonicalProximityEvidenceAuthorityV1,
};

const PROXIMITY_CONTRIBUTION_ID_DOMAIN_V1: &str = "tracedecay.pr13.proximity.contribution-id.v1";
const PROXIMITY_CONFIGURATION_REVISION_DOMAIN_V1: &str =
    "tracedecay.pr13.proximity.configuration-revision.v1";

/// Exact Plan 20 threshold input for one evaluation. Revision and effective
/// behavior digest are pinned together before provider evidence is read.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProximityThresholdPinV1 {
    pub configuration_revision: ConfigurationRevisionId,
    pub configuration_digest: ManifestDigest,
    pub value_basis_points: u16,
}

impl ProximityThresholdPinV1 {
    pub fn from_current_configuration(current: &ConfigurationCurrentStateV1) -> Option<Self> {
        current.revision_id.validate().ok()?;
        current.snapshot.validate().ok()?;
        let key = SettingKey::new(PROXIMITY_RISK_THRESHOLD_SETTING_KEY_V1).ok()?;
        let ConfigurationValueV1::Unsigned(value) = current.snapshot.effective_values.get(&key)?
        else {
            return None;
        };
        Self::new(
            current.revision_id.clone(),
            current.snapshot.effective_behavior_digest.clone(),
            u16::try_from(*value).ok()?,
        )
    }

    pub fn new(
        configuration_revision: ConfigurationRevisionId,
        configuration_digest: ManifestDigest,
        value_basis_points: u16,
    ) -> Option<Self> {
        configuration_revision.validate().ok()?;
        configuration_digest.validate().ok()?;
        (value_basis_points <= 10_000).then_some(Self {
            configuration_revision,
            configuration_digest,
            value_basis_points,
        })
    }

    pub fn validate(&self) -> bool {
        self.configuration_revision.validate().is_ok()
            && self.configuration_digest.validate().is_ok()
            && self.value_basis_points <= 10_000
    }
}

/// Authorized graph/query result tied to canonical session observations.
///
/// Full envelopes are transient provider input. Plan 09 receives only opaque
/// observation IDs, anchors, privacy-safe code shape, relation paths, risk
/// inputs, freshness, coverage, and inclusion state.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalProximityEvidenceV1 {
    pub observations: Vec<CanonicalObservationEnvelopeV1>,
    pub retrieval_anchor_ids: Vec<RetrievalAnchorId>,
    pub address: ProximityAddressV1,
    pub relation_paths: Vec<ProximityRelationPathV1>,
    pub risk_inputs: ProximityRiskInputsV1,
    pub warning_class: ProximityWarningClassV1,
    pub raw_risk_basis_points: u16,
    pub observed_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub coverage: ProximityCoverageV1,
}

impl CanonicalProximityEvidenceV1 {
    fn validate_for(&self, request: &ProximityEvaluationRequestV1) -> bool {
        if self.observations.is_empty()
            || self.retrieval_anchor_ids.is_empty()
            || self.address.validate().is_err()
            || self.address.scope != request.scope
            || self.risk_inputs.validate().is_err()
            || self.raw_risk_basis_points > 10_000
            || self.observed_at.0 >= self.expires_at.0
            || !matches!(
                self.coverage,
                ProximityCoverageV1::Complete
                    | ProximityCoverageV1::Partial
                    | ProximityCoverageV1::Stale
            )
        {
            return false;
        }
        if self
            .retrieval_anchor_ids
            .iter()
            .any(|anchor| anchor.validate().is_err())
            || self
                .relation_paths
                .iter()
                .any(|path| path.validate().is_err())
        {
            return false;
        }
        self.observations
            .iter()
            .enumerate()
            .all(|(index, observation)| {
                observation.validate().is_ok()
                    && observation.relations().agent_id().is_some()
                    && !self.observations[index.saturating_add(1)..]
                        .iter()
                        .any(|other| other.stable_record_id() == observation.stable_record_id())
            })
    }
}

/// One bounded provider read. Empty evidence is a successful current result;
/// `Partial` preserves truncation or missing joins without making the owner
/// unavailable.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalProximityEvidenceBatchV1 {
    pub evidence: Vec<CanonicalProximityEvidenceV1>,
    pub coverage: ProximityCoverageV1,
}

impl CanonicalProximityEvidenceBatchV1 {
    pub fn new(
        evidence: Vec<CanonicalProximityEvidenceV1>,
        coverage: ProximityCoverageV1,
    ) -> Option<Self> {
        matches!(
            coverage,
            ProximityCoverageV1::Complete
                | ProximityCoverageV1::Partial
                | ProximityCoverageV1::Stale
        )
        .then_some(Self { evidence, coverage })
    }
}

/// Stable provider seam retained for dashboard reads, independently
/// authorized roots, workspace-local computation, and task evidence.
/// Implementations reuse canonical observation and graph/query authorities.
pub trait CanonicalProximityEvidenceAuthorityV1 {
    fn current_evidence<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a ProximityEvaluationRequestV1,
    ) -> FeedbackPortFuture<'a, Option<CanonicalProximityEvidenceBatchV1>>;
}

impl<T> CanonicalProximityEvidenceAuthorityV1 for Arc<T>
where
    T: CanonicalProximityEvidenceAuthorityV1 + ?Sized,
{
    fn current_evidence<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a ProximityEvaluationRequestV1,
    ) -> FeedbackPortFuture<'a, Option<CanonicalProximityEvidenceBatchV1>> {
        (**self).current_evidence(context, request)
    }
}

/// Synchronous Plan 09 publication contribution produced by the one async
/// provider evaluation.
#[derive(Clone, Debug)]
pub struct ProximityFindingContributorV1 {
    provider_state: ProviderEvaluationStateV1,
    contributions: Vec<ProximityContributionV1>,
}

impl ProximityFindingContributorV1 {
    fn new(
        contributions: Vec<ProximityContributionV1>,
        coverage: ProximityCoverageV1,
    ) -> Option<Self> {
        if contributions
            .iter()
            .enumerate()
            .any(|(index, contribution)| {
                contribution.validate().is_err()
                    || contributions[index.saturating_add(1)..]
                        .iter()
                        .any(|other| other.contribution_id == contribution.contribution_id)
            })
        {
            return None;
        }
        Some(Self {
            provider_state: proximity_provider_state(coverage, &contributions),
            contributions,
        })
    }

    pub fn contributions(&self) -> &[ProximityContributionV1] {
        &self.contributions
    }
}

impl AdvisoryFindingContributorV1 for ProximityFindingContributorV1 {
    fn advisory_findings(
        &self,
        window: AdvisoryFindingValidityWindowV1,
    ) -> Result<AdvisoryFindingContributionBatchV1, ApplicationContractError> {
        let mut findings = Vec::new();
        for contribution in &self.contributions {
            let batch = contribution.advisory_findings(window)?;
            findings.extend(batch.findings.into_iter().map(|mut finding| {
                finding.provider_state = self.provider_state;
                finding
            }));
        }
        let batch = AdvisoryFindingContributionBatchV1 {
            provider_state: self.provider_state,
            findings,
        };
        batch.validate()?;
        Ok(batch)
    }
}

fn proximity_provider_state(
    coverage: ProximityCoverageV1,
    contributions: &[ProximityContributionV1],
) -> ProviderEvaluationStateV1 {
    if matches!(
        coverage,
        ProximityCoverageV1::Unavailable
            | ProximityCoverageV1::Denied
            | ProximityCoverageV1::Private
    ) || contributions.iter().any(|contribution| {
        matches!(
            contribution.coverage,
            ProximityCoverageV1::Unavailable
                | ProximityCoverageV1::Denied
                | ProximityCoverageV1::Private
        )
    }) {
        ProviderEvaluationStateV1::Unavailable
    } else if coverage == ProximityCoverageV1::Stale
        || contributions
            .iter()
            .any(|contribution| contribution.coverage == ProximityCoverageV1::Stale)
    {
        ProviderEvaluationStateV1::Stale
    } else if coverage == ProximityCoverageV1::Partial
        || contributions
            .iter()
            .any(|contribution| contribution.coverage == ProximityCoverageV1::Partial)
    {
        ProviderEvaluationStateV1::Partial
    } else {
        ProviderEvaluationStateV1::SupportedCompletedComplete
    }
}

#[derive(Clone, Debug)]
pub enum ProximityRuntimeOutcomeV1 {
    Completed(ProximityFindingContributorV1),
    Denied,
    Cancelled,
    TimedOut,
    Unavailable,
}

/// One exact-scope evaluation owner. The outer feedback cycle owns completed
/// publication and dedupe, so this owner retains no local ledger or proof.
pub struct ProximityRuntimeOwnerV1<A, C> {
    scope: FeedbackScopeV1,
    evidence: A,
    configuration: C,
}

impl<A, C> ProximityRuntimeOwnerV1<A, C> {
    pub fn new(scope: FeedbackScopeV1, evidence: A, configuration: C) -> Option<Self> {
        scope.validate().ok()?;
        Some(Self {
            scope,
            evidence,
            configuration,
        })
    }

    pub fn scope(&self) -> &FeedbackScopeV1 {
        &self.scope
    }
}

impl<A, C> ProximityRuntimeOwnerV1<A, C>
where
    A: CanonicalProximityEvidenceAuthorityV1 + Sync,
{
    /// Evaluates against the exact configuration snapshot already authorized
    /// for the enclosing feedback cycle. The provider never rereads mutable
    /// configuration while that cycle is in flight.
    pub async fn evaluate_with_threshold_pin(
        &self,
        context: &RequestContext,
        request: &ProximityEvaluationRequestV1,
        threshold: &ProximityThresholdPinV1,
    ) -> ProximityRuntimeOutcomeV1 {
        if context.cancellation().is_cancelled() {
            return ProximityRuntimeOutcomeV1::Cancelled;
        }
        if context.deadline().is_elapsed_at(now_micros()) {
            return ProximityRuntimeOutcomeV1::TimedOut;
        }
        if request.validate().is_err() || request.scope != self.scope {
            return ProximityRuntimeOutcomeV1::Denied;
        }
        if !context_allows_feedback_operation(
            context,
            &request.scope,
            PROXIMITY_CAPABILITY_ID_V1,
            PROXIMITY_USE_CASE_ID_V1,
        ) {
            return ProximityRuntimeOutcomeV1::Denied;
        }
        if !threshold.validate() {
            return ProximityRuntimeOutcomeV1::Unavailable;
        }
        let Some(batch) = self.evidence.current_evidence(context, request).await else {
            return ProximityRuntimeOutcomeV1::Unavailable;
        };
        if let Some(interruption) = interrupted(context) {
            return interruption;
        }
        let mut contributions = Vec::with_capacity(batch.evidence.len());
        for item in batch.evidence {
            let Some(contribution) = build_proximity_contribution(request, threshold, item) else {
                return ProximityRuntimeOutcomeV1::Unavailable;
            };
            contributions.push(contribution);
        }
        let Some(contributor) =
            ProximityFindingContributorV1::new(contributions, batch.coverage)
        else {
            return ProximityRuntimeOutcomeV1::Unavailable;
        };
        ProximityRuntimeOutcomeV1::Completed(contributor)
    }
}

impl<A, C> ProximityRuntimeOwnerV1<A, C>
where
    A: CanonicalProximityEvidenceAuthorityV1 + Sync,
    C: ConfigurationControlStore,
{
    /// Compatibility entry for the aggregate advisory runtime. The current
    /// threshold is accepted only when its behavior digest is the exact
    /// configuration identity already authorized by the enclosing request.
    pub async fn evaluate_for_configuration_digest(
        &self,
        context: &RequestContext,
        request: &ProximityEvaluationRequestV1,
        expected_configuration_digest: &ManifestDigest,
    ) -> ProximityRuntimeOutcomeV1 {
        if context.cancellation().is_cancelled() {
            return ProximityRuntimeOutcomeV1::Cancelled;
        }
        if context.deadline().is_elapsed_at(now_micros()) {
            return ProximityRuntimeOutcomeV1::TimedOut;
        }
        if request.validate().is_err() || request.scope != self.scope {
            return ProximityRuntimeOutcomeV1::Denied;
        }
        if !context_allows_feedback_operation(
            context,
            &request.scope,
            PROXIMITY_CAPABILITY_ID_V1,
            PROXIMITY_USE_CASE_ID_V1,
        ) {
            return ProximityRuntimeOutcomeV1::Denied;
        }
        let Ok(configuration) = self.configuration.current().await else {
            return ProximityRuntimeOutcomeV1::Unavailable;
        };
        if let Some(interruption) = interrupted(context) {
            return interruption;
        }
        let Some(threshold) = ProximityThresholdPinV1::from_current_configuration(&configuration)
        else {
            return ProximityRuntimeOutcomeV1::Unavailable;
        };
        if threshold.configuration_digest != *expected_configuration_digest {
            return ProximityRuntimeOutcomeV1::Denied;
        }
        self.evaluate_with_threshold_pin(context, request, &threshold)
            .await
    }

    pub async fn evaluate(
        &self,
        context: &RequestContext,
        request: &ProximityEvaluationRequestV1,
    ) -> ProximityRuntimeOutcomeV1 {
        if context.cancellation().is_cancelled() {
            return ProximityRuntimeOutcomeV1::Cancelled;
        }
        if context.deadline().is_elapsed_at(now_micros()) {
            return ProximityRuntimeOutcomeV1::TimedOut;
        }
        if request.validate().is_err() || request.scope != self.scope {
            return ProximityRuntimeOutcomeV1::Denied;
        }
        if !context_allows_feedback_operation(
            context,
            &request.scope,
            PROXIMITY_CAPABILITY_ID_V1,
            PROXIMITY_USE_CASE_ID_V1,
        ) {
            return ProximityRuntimeOutcomeV1::Denied;
        }
        let Ok(configuration) = self.configuration.current().await else {
            return ProximityRuntimeOutcomeV1::Unavailable;
        };
        if let Some(interruption) = interrupted(context) {
            return interruption;
        }
        let Some(threshold) = ProximityThresholdPinV1::from_current_configuration(&configuration)
        else {
            return ProximityRuntimeOutcomeV1::Unavailable;
        };
        self.evaluate_with_threshold_pin(context, request, &threshold)
            .await
    }
}

fn interrupted(context: &RequestContext) -> Option<ProximityRuntimeOutcomeV1> {
    if context.cancellation().is_cancelled() {
        Some(ProximityRuntimeOutcomeV1::Cancelled)
    } else if context.deadline().is_elapsed_at(now_micros()) {
        Some(ProximityRuntimeOutcomeV1::TimedOut)
    } else {
        None
    }
}

fn build_proximity_contribution(
    request: &ProximityEvaluationRequestV1,
    threshold: &ProximityThresholdPinV1,
    evidence: CanonicalProximityEvidenceV1,
) -> Option<ProximityContributionV1> {
    if !threshold.validate() || !evidence.validate_for(request) {
        return None;
    }
    let source_observation_ids = evidence
        .observations
        .iter()
        .map(|observation| ProximityObservationIdV1::new(observation.stable_record_id().as_str()))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let tier = if matches!(
        evidence.warning_class,
        ProximityWarningClassV1::SameFile
            | ProximityWarningClassV1::OverlappingRange
            | ProximityWarningClassV1::SameSymbol
    ) {
        ProximityTierV1::Immediate
    } else {
        ProximityTierV1::Configured
    };
    let inclusion = if evidence.expires_at.0 <= request.observed_at.0
        || evidence.coverage == ProximityCoverageV1::Stale
    {
        ProximityInclusionV1::Stale
    } else if tier == ProximityTierV1::Immediate
        || evidence.raw_risk_basis_points >= threshold.value_basis_points
    {
        ProximityInclusionV1::Included
    } else {
        ProximityInclusionV1::BelowThreshold
    };
    let coverage = if inclusion == ProximityInclusionV1::Stale {
        ProximityCoverageV1::Stale
    } else {
        evidence.coverage
    };
    let identity = canonical_sha256(&(
        PROXIMITY_CONTRIBUTION_ID_DOMAIN_V1,
        &request.scope,
        &source_observation_ids,
        &evidence.retrieval_anchor_ids,
        &evidence.address,
        &evidence.relation_paths,
        evidence.warning_class,
        evidence.observed_at,
        evidence.expires_at,
        tier,
        (tier == ProximityTierV1::Configured).then_some((
            &threshold.configuration_revision,
            &threshold.configuration_digest,
        )),
    ))
    .ok()?;
    let suffix = identity
        .as_str()
        .strip_prefix("sha256:")
        .unwrap_or(identity.as_str());
    let threshold_revision = if tier == ProximityTierV1::Configured {
        Some(
            canonical_sha256(&(
                PROXIMITY_CONFIGURATION_REVISION_DOMAIN_V1,
                &threshold.configuration_revision,
                &threshold.configuration_digest,
            ))
            .ok()?,
        )
    } else {
        None
    };
    let contribution = ProximityContributionV1 {
        contribution_id: ProximityContributionIdV1::new(format!("contribution.proximity.{suffix}"))
            .ok()?,
        // Shared domain/publication compatibility still carries this alias.
        // Runtime-local dedupe no longer stores or compares it.
        warning_id: ProximityWarningIdV1::new(format!("warning.proximity.{suffix}")).ok()?,
        warning_class: evidence.warning_class,
        source_observation_ids,
        retrieval_anchor_ids: evidence.retrieval_anchor_ids,
        address: Some(evidence.address),
        relation_paths: evidence.relation_paths,
        risk_inputs: Some(evidence.risk_inputs),
        tier,
        threshold_value_basis_points: (tier == ProximityTierV1::Configured)
            .then_some(threshold.value_basis_points),
        // One digest binds both canonical revision and effective behavior.
        threshold_revision,
        raw_risk_basis_points: Some(evidence.raw_risk_basis_points),
        observed_at: evidence.observed_at,
        expires_at: evidence.expires_at,
        coverage,
        inclusion,
    };
    contribution.validate().ok()?;
    Some(contribution)
}

pub type ConcreteProximityRuntimeOwnerV1<A, C> = ProximityRuntimeOwnerV1<A, C>;

/// Concrete registration factory retaining only provider and configuration
/// extension seams. Shared Plan 09 publication owns durable dedupe.
pub fn open_proximity_runtime<A, C>(
    scope: FeedbackScopeV1,
    evidence: A,
    configuration: C,
) -> Option<ConcreteProximityRuntimeOwnerV1<A, C>> {
    ProximityRuntimeOwnerV1::new(scope, evidence, configuration)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tracedecay_application::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestId, ResolvedScope,
    };
    use tracedecay_domain::configuration::{ConfigurationLayerIdV1, ConfigurationValueV1};
    use tracedecay_domain::{ActorId, CommitId, ProjectId, RefId, RepositoryId, WorktreeId};
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use super::*;

    struct MutatingEvidence {
        current_configuration: Arc<Mutex<ConfigurationCurrentStateV1>>,
        drifted_configuration: ConfigurationCurrentStateV1,
        calls: Arc<AtomicUsize>,
    }

    impl CanonicalProximityEvidenceAuthorityV1 for MutatingEvidence {
        fn current_evidence<'a>(
            &'a self,
            _context: &'a RequestContext,
            _request: &'a ProximityEvaluationRequestV1,
        ) -> FeedbackPortFuture<'a, Option<CanonicalProximityEvidenceBatchV1>> {
            Box::pin(async move {
                *self
                    .current_configuration
                    .lock()
                    .expect("configuration lock") = self.drifted_configuration.clone();
                self.calls.fetch_add(1, Ordering::SeqCst);
                CanonicalProximityEvidenceBatchV1::new(Vec::new(), ProximityCoverageV1::Complete)
            })
        }
    }

    fn scope_and_context() -> (FeedbackScopeV1, RequestContext) {
        let project_id = ProjectId::new("project.proximity-pin").expect("project");
        let repository_id = RepositoryId::new("repository.proximity-pin").expect("repository");
        let worktree_id = WorktreeId::new("worktree.proximity-pin").expect("worktree");
        let branch_ref = "refs/heads/proximity-pin".to_owned();
        let scope = FeedbackScopeV1 {
            project_id: project_id.clone(),
            repository_id: repository_id.clone(),
            worktree_id: worktree_id.clone(),
            branch_ref: branch_ref.clone(),
            head_commit_id: CommitId::new("commit.proximity-pin").expect("commit"),
        };
        let resolved_scope = ResolvedScope::new(
            project_id,
            repository_id,
            worktree_id,
            Some(RefId::new(branch_ref).expect("ref")),
        )
        .expect("resolved scope");
        let capability =
            CapabilityId::new(PROXIMITY_CAPABILITY_ID_V1.to_owned()).expect("capability");
        let use_case = UseCaseId::new(PROXIMITY_USE_CASE_ID_V1.to_owned()).expect("use case");
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.proximity-pin").expect("grant"),
            1,
            canonical_sha256(&("proximity-pin-grant", &resolved_scope)).expect("digest"),
            ActorId::new("actor.proximity-pin").expect("issuer"),
            UtcMicros(1),
            UtcMicros(i64::MAX),
            resolved_scope.clone(),
            BTreeSet::from([capability]),
            BTreeSet::from([use_case]),
            DisclosureClass::Evidence,
        )
        .expect("grant");
        let context = RequestContext::new(
            ActorId::new("actor.proximity-pin").expect("actor"),
            resolved_scope,
            grant,
            RequestId::new("request.proximity-pin").expect("request"),
            Deadline::new(UtcMicros(i64::MAX)).expect("deadline"),
            CancellationContext::active("cancel.proximity-pin").expect("cancellation"),
        )
        .expect("context");
        (scope, context)
    }

    fn configuration(revision: &str, threshold: Option<u64>) -> ConfigurationCurrentStateV1 {
        let layers = threshold
            .map(|threshold| crate::config::resolver::ConfigurationLayerV1 {
                layer: ConfigurationLayerIdV1::Project {
                    project_id: ProjectId::new("project.proximity-pin").expect("project"),
                },
                revision_id: ConfigurationRevisionId::new(revision).expect("revision"),
                entries: BTreeMap::from([(
                    SettingKey::new(PROXIMITY_RISK_THRESHOLD_SETTING_KEY_V1).expect("key"),
                    ConfigurationValueV1::Unsigned(threshold),
                )]),
            })
            .into_iter()
            .collect::<Vec<_>>();
        let snapshot = crate::config::resolver::resolve_configuration(
            &crate::config::registry::ConfigurationRegistry::core().expect("registry"),
            &layers,
        )
        .expect("configuration")
        .snapshot;
        ConfigurationCurrentStateV1 {
            revision_id: ConfigurationRevisionId::new(revision).expect("revision"),
            snapshot,
        }
    }

    #[tokio::test]
    async fn threshold_pin_is_immutable_in_cycle_and_refreshes_next_cycle() {
        let (scope, context) = scope_and_context();
        let authorized = configuration("configuration.proximity-pin.authorized", None);
        let authorized_pin = ProximityThresholdPinV1::from_current_configuration(&authorized)
            .expect("authorized threshold");
        let drifted = configuration("configuration.proximity-pin.drifted", Some(7_500));
        let drifted_pin = ProximityThresholdPinV1::from_current_configuration(&drifted)
            .expect("drifted threshold");
        assert_ne!(
            authorized_pin.configuration_revision,
            drifted_pin.configuration_revision
        );
        assert_ne!(
            authorized_pin.configuration_digest,
            drifted_pin.configuration_digest
        );
        let current_configuration = Arc::new(Mutex::new(authorized.clone()));
        let calls = Arc::new(AtomicUsize::new(0));
        let owner = open_proximity_runtime(
            scope.clone(),
            MutatingEvidence {
                current_configuration: Arc::clone(&current_configuration),
                drifted_configuration: drifted.clone(),
                calls: Arc::clone(&calls),
            },
            (),
        )
        .expect("owner");
        let request = ProximityEvaluationRequestV1 {
            scope,
            observed_at: UtcMicros(2),
        };

        assert!(matches!(
            owner
                .evaluate_with_threshold_pin(&context, &request, &authorized_pin)
                .await,
            ProximityRuntimeOutcomeV1::Completed(_)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            current_configuration
                .lock()
                .expect("configuration lock")
                .revision_id,
            drifted.revision_id
        );

        assert!(matches!(
            owner
                .evaluate_with_threshold_pin(&context, &request, &drifted_pin)
                .await,
            ProximityRuntimeOutcomeV1::Completed(_)
        ));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a later cycle must use its newly authorized threshold pin"
        );
    }
}
