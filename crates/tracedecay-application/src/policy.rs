//! Application composition for the retained pure policy evaluators.
//!
//! This module owns no policy rules. It binds exact request scope and the
//! current Plan-20 configuration snapshot to the existing `tracedecay-policy`
//! evaluators, and projects catalog/application handler pairs plus their
//! static availability into capability routing.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tracedecay_domain::configuration::{ConfigurationRevisionId, ConfigurationSnapshotV1};
use tracedecay_domain::{
    CapabilityId as DomainCapabilityId, ManifestDigest, UtcMicros, VectorWatermark,
    canonical_sha256,
};
use tracedecay_policy::analyzer::{
    AnalyzerAdmissionEvaluatorV1, AnalyzerAdmissionInputV1, AnalyzerAdmissionSnapshotV1,
};
use tracedecay_policy::authorization::{
    PolicyIdentifierV1, SourceAuthorizationDecisionV1, SourceAuthorizationEvaluator,
    SourceAuthorizationEvaluatorV1, SourceAuthorizationInputV1, SourceOwnerV1,
};
use tracedecay_policy::configuration::{
    ConfigurationMutationGrantSnapshotV1, ConfigurationMutationPolicyEvaluator,
    ConfigurationMutationPolicyEvaluatorV1, ConfigurationMutationRecheckDispositionV1,
    ConfigurationMutationRecheckInputV1,
};
use tracedecay_policy::git::{
    GitEffectClassificationInputV1, GitEffectClassifier, GitEffectClassifierV1, GitEffectDecisionV1,
};
use tracedecay_policy::routing::{
    CapabilityAvailabilityV1, CapabilityEffectClassV1, CapabilityRouteCandidateV1,
    CapabilityRoutingCancellationV1, CapabilityRoutingDecisionV1, CapabilityRoutingEvaluator,
    CapabilityRoutingEvaluatorV1, CapabilityRoutingGrantStateV1, CapabilityRoutingGrantV1,
    CapabilityRoutingRequestV1, ScopeMatchV1, TruthFreshnessRequirementV1, TruthSourceStateV1,
};
use tracedecay_tool_catalog::{AvailabilityContract, EffectClass, UseCaseId};

use crate::context::{CancellationState, RequestAdmission, RequestContext, ResolvedScope};
use crate::error::ApplicationContractError;
use crate::handlers::{ApplicationHandlerDescriptors, application_handler_descriptors};
use crate::retrieval::catalog::application_catalog_contributions;

const POLICY_CAPABILITY_DIGEST_DOMAIN: &str = "tracedecay.application.policy-capability.v1";
const POLICY_ROUTING_CATALOG_DIGEST_DOMAIN: &str =
    "tracedecay.application.policy-routing-catalog.v1";
const POLICY_ROUTING_CATALOG_REVISION: u64 = 1;

/// Named production journey consuming one retained pure evaluator.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PolicyConsumerV1 {
    CapabilityRouting,
    AnalyzerAdmission,
    SourceAuthorization,
    ConfigurationPolicy,
    HintRouting,
    RetrievalRouting,
    LocalLiveCorrelation,
    DiagnosticsCuration,
    MemoryRouting,
    ConflictRouting,
    ExperimentRouting,
}

impl PolicyConsumerV1 {
    const fn uses_capability_routing(self) -> bool {
        matches!(
            self,
            Self::CapabilityRouting
                | Self::HintRouting
                | Self::RetrievalRouting
                | Self::LocalLiveCorrelation
                | Self::DiagnosticsCuration
                | Self::MemoryRouting
                | Self::ExperimentRouting
        )
    }
}

/// Explicit relation between local/session and live-Git evidence.
///
/// The relation is supplied by the owning correlation authority. Watermark
/// ordering alone cannot prove semantic agreement.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PolicyEvidenceAgreementV1 {
    Agree,
    Disagree,
    Incomparable,
}

/// Independent evidence frontiers carried unchanged through policy routing.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PolicyEvidenceFrontierV1 {
    pub watermark: VectorWatermark,
    pub state: TruthSourceStateV1,
}

/// Independent evidence frontiers carried unchanged through policy routing.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PolicyEvidenceHorizonV1 {
    pub local_session: PolicyEvidenceFrontierV1,
    pub live_git: PolicyEvidenceFrontierV1,
    pub agreement: PolicyEvidenceAgreementV1,
}

impl PolicyEvidenceHorizonV1 {
    /// Conservative routing prerequisite without replacing either recorded
    /// frontier. The full independent states remain on the result.
    pub const fn routing_state(&self) -> TruthSourceStateV1 {
        use TruthSourceStateV1::{Fresh, Partial, Stale, Unavailable, Unknown};

        match (self.local_session.state, self.live_git.state) {
            (Unavailable, _) | (_, Unavailable) => Unavailable,
            (Stale, _) | (_, Stale) => Stale,
            (Unknown, _) | (_, Unknown) => Unknown,
            (Partial, _) | (_, Partial) => Partial,
            (Fresh, Fresh) => Fresh,
        }
    }
}

/// Exact application authority supplied to every composed evaluator call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyEvaluationContextV1 {
    request: RequestContext,
    configuration_revision: ConfigurationRevisionId,
    configuration: ConfigurationSnapshotV1,
    policy_revision: u64,
    policy_digest: ManifestDigest,
}

impl PolicyEvaluationContextV1 {
    pub fn new(
        request: RequestContext,
        configuration_revision: ConfigurationRevisionId,
        configuration: ConfigurationSnapshotV1,
        policy_revision: u64,
        policy_digest: ManifestDigest,
    ) -> Result<Self, ApplicationContractError> {
        let context = Self {
            request,
            configuration_revision,
            configuration,
            policy_revision,
            policy_digest,
        };
        context.validate()?;
        Ok(context)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.request.validate()?;
        self.configuration_revision.validate()?;
        self.configuration.validate()?;
        self.policy_digest.validate()?;
        if self.policy_revision == 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "policy evaluation revision",
            });
        }
        Ok(())
    }

    pub fn request(&self) -> &RequestContext {
        &self.request
    }

    pub fn scope(&self) -> &ResolvedScope {
        self.request.scope()
    }

    pub fn configuration_revision(&self) -> &ConfigurationRevisionId {
        &self.configuration_revision
    }

    pub fn configuration(&self) -> &ConfigurationSnapshotV1 {
        &self.configuration
    }

    pub const fn policy_revision(&self) -> u64 {
        self.policy_revision
    }

    pub fn policy_digest(&self) -> &ManifestDigest {
        &self.policy_digest
    }

    fn validate_common(
        &self,
        policy_revision: u64,
        policy_digest: &ManifestDigest,
        configuration_digest: &ManifestDigest,
    ) -> Result<(), ApplicationContractError> {
        self.validate()?;
        if self.policy_revision != policy_revision
            || &self.policy_digest != policy_digest
            || &self.configuration.effective_behavior_digest != configuration_digest
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "policy evaluation snapshot",
            });
        }
        Ok(())
    }
}

/// One evaluator result pinned to exact application and evidence authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyEvaluationV1<T> {
    pub consumer: PolicyConsumerV1,
    pub context: PolicyEvaluationContextV1,
    pub evidence_horizon: Option<PolicyEvidenceHorizonV1>,
    pub decision: T,
}

/// Handler-backed catalog capability projected for pure routing.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RegisteredPolicyCapabilityV1 {
    capability_id: String,
    use_case_id: String,
    catalog_availability: CapabilityAvailabilityV1,
    effect_class: EffectClass,
    capability_digest: ManifestDigest,
}

impl RegisteredPolicyCapabilityV1 {
    pub fn capability_id(&self) -> &str {
        &self.capability_id
    }

    pub const fn effect_class(&self) -> EffectClass {
        self.effect_class
    }

    pub const fn catalog_availability(&self) -> CapabilityAvailabilityV1 {
        self.catalog_availability
    }

    pub fn use_case_id(&self) -> &str {
        &self.use_case_id
    }

    pub fn capability_digest(&self) -> &ManifestDigest {
        &self.capability_digest
    }
}

/// Production composition of existing pure evaluators.
///
/// This is ordinary typed application wiring, not a policy engine or generic
/// operation dispatcher.
#[derive(Clone, Debug)]
pub struct PolicyEvaluatorCompositionV1 {
    capabilities: BTreeMap<String, RegisteredPolicyCapabilityV1>,
    catalog_revision: u64,
    catalog_digest: ManifestDigest,
    routing: CapabilityRoutingEvaluatorV1,
    analyzer: AnalyzerAdmissionEvaluatorV1,
    source_authorization: SourceAuthorizationEvaluatorV1,
    configuration: ConfigurationMutationPolicyEvaluatorV1,
    git: GitEffectClassifierV1,
}

impl PolicyEvaluatorCompositionV1 {
    /// Builds the routing projection from the canonical catalog and matching
    /// application handlers. Static unavailability remains a policy fact even
    /// though transport/profile composition keeps the operation inert.
    pub fn from_application_catalog() -> Result<Self, ApplicationContractError> {
        let contributions = application_catalog_contributions()?;
        let handlers = application_handler_descriptors()?;
        handlers.validate_against(&contributions)?;
        Self::from_catalog(&handlers, &contributions)
    }

    pub fn from_catalog(
        handlers: &ApplicationHandlerDescriptors,
        contributions: &[tracedecay_tool_catalog::CatalogContributionV1],
    ) -> Result<Self, ApplicationContractError> {
        let mut capabilities = BTreeMap::new();
        for capability in contributions
            .iter()
            .flat_map(|contribution| contribution.capabilities())
        {
            let Some(handler) = handlers.get(capability.use_case_id()) else {
                return Err(ApplicationContractError::Inconsistent {
                    field: "policy capability handler",
                });
            };
            if handler.operation().capability_id() != capability.capability_id() {
                return Err(ApplicationContractError::Inconsistent {
                    field: "policy capability identity",
                });
            }
            let capability_id = capability.capability_id().as_str().to_owned();
            let registered = RegisteredPolicyCapabilityV1 {
                capability_id: capability_id.clone(),
                use_case_id: capability.use_case_id().as_str().to_owned(),
                catalog_availability: catalog_availability(capability.availability()),
                effect_class: capability.effect(),
                capability_digest: canonical_sha256(&(
                    POLICY_CAPABILITY_DIGEST_DOMAIN,
                    capability,
                ))?,
            };
            if capabilities.insert(capability_id, registered).is_some() {
                return Err(ApplicationContractError::Duplicate {
                    field: "policy capability",
                });
            }
        }
        let catalog_digest = canonical_sha256(&(
            POLICY_ROUTING_CATALOG_DIGEST_DOMAIN,
            POLICY_ROUTING_CATALOG_REVISION,
            &capabilities,
        ))?;
        Ok(Self {
            capabilities,
            catalog_revision: POLICY_ROUTING_CATALOG_REVISION,
            catalog_digest,
            routing: CapabilityRoutingEvaluatorV1::default(),
            analyzer: AnalyzerAdmissionEvaluatorV1::default(),
            source_authorization: SourceAuthorizationEvaluatorV1::default(),
            configuration: ConfigurationMutationPolicyEvaluatorV1,
            git: GitEffectClassifierV1::default(),
        })
    }

    pub fn registered_capability(
        &self,
        capability_id: &str,
    ) -> Option<&RegisteredPolicyCapabilityV1> {
        self.capabilities.get(capability_id)
    }

    /// Projects current catalog metadata into one evaluator candidate.
    pub fn candidate(
        &self,
        capability_id: &str,
        runtime_availability: CapabilityAvailabilityV1,
        scope_match: ScopeMatchV1,
        truth_source_state: TruthSourceStateV1,
    ) -> Result<CapabilityRouteCandidateV1, ApplicationContractError> {
        let registered =
            self.capabilities
                .get(capability_id)
                .ok_or(ApplicationContractError::Inconsistent {
                    field: "policy route",
                })?;
        Ok(CapabilityRouteCandidateV1 {
            capability_id: DomainCapabilityId::new(registered.capability_id.clone())?,
            use_case_id: policy_identifier(&registered.use_case_id)?,
            availability: if registered.catalog_availability
                == CapabilityAvailabilityV1::Unavailable
            {
                CapabilityAvailabilityV1::Unavailable
            } else {
                runtime_availability
            },
            scope_match,
            effect_class: route_effect(registered.effect_class)?,
            truth_source_state,
            catalog_revision: self.catalog_revision,
            catalog_digest: self.catalog_digest.clone(),
            capability_digest: registered.capability_digest.clone(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn routing_request(
        &self,
        context: &PolicyEvaluationContextV1,
        use_case_id: &UseCaseId,
        declared_capability_order: Vec<DomainCapabilityId>,
        candidates: Vec<CapabilityRouteCandidateV1>,
        required_effect_class: CapabilityEffectClassV1,
        required_freshness: TruthFreshnessRequirementV1,
        evaluated_at: UtcMicros,
    ) -> Result<CapabilityRoutingRequestV1, ApplicationContractError> {
        context.validate()?;
        Ok(CapabilityRoutingRequestV1 {
            requested_use_case_id: policy_identifier(use_case_id.as_str())?,
            declared_capability_order,
            candidates,
            grant: routing_grant(context.request())?,
            required_effect_class,
            required_freshness,
            catalog_revision: self.catalog_revision,
            catalog_digest: self.catalog_digest.clone(),
            policy_revision: context.policy_revision(),
            policy_digest: context.policy_digest().clone(),
            configuration_digest: context.configuration().effective_behavior_digest.clone(),
            deadline: context.request().deadline().expires_at,
            cancellation: routing_cancellation(context.request()),
            evaluated_at,
        })
    }

    pub fn route(
        &self,
        consumer: PolicyConsumerV1,
        context: &PolicyEvaluationContextV1,
        request: &CapabilityRoutingRequestV1,
        evidence_horizon: Option<PolicyEvidenceHorizonV1>,
    ) -> Result<PolicyEvaluationV1<CapabilityRoutingDecisionV1>, ApplicationContractError> {
        if !consumer.uses_capability_routing()
            || (consumer == PolicyConsumerV1::LocalLiveCorrelation && evidence_horizon.is_none())
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "policy routing consumer",
            });
        }
        if let Some(horizon) = evidence_horizon
            .as_ref()
            .filter(|_| consumer == PolicyConsumerV1::LocalLiveCorrelation)
        {
            let state = horizon.routing_state();
            if request
                .candidates
                .iter()
                .any(|candidate| candidate.truth_source_state != state)
            {
                return Err(ApplicationContractError::Inconsistent {
                    field: "local/live policy routing state",
                });
            }
        }
        context.validate_common(
            request.policy_revision,
            &request.policy_digest,
            &request.configuration_digest,
        )?;
        self.validate_route_request(context, request)?;
        Ok(PolicyEvaluationV1 {
            consumer,
            context: context.clone(),
            evidence_horizon,
            decision: self.routing.evaluate(request),
        })
    }

    pub fn admit_analyzer(
        &self,
        consumer: PolicyConsumerV1,
        context: &PolicyEvaluationContextV1,
        input: &AnalyzerAdmissionInputV1,
    ) -> Result<PolicyEvaluationV1<AnalyzerAdmissionSnapshotV1>, ApplicationContractError> {
        if !matches!(
            consumer,
            PolicyConsumerV1::AnalyzerAdmission | PolicyConsumerV1::DiagnosticsCuration
        ) {
            return Err(ApplicationContractError::Inconsistent {
                field: "analyzer policy consumer",
            });
        }
        context.validate_common(
            input.policy_revision,
            &input.policy_digest,
            &input.configuration_digest,
        )?;
        if context.request.admission_at(input.evaluated_at) != RequestAdmission::Admitted {
            return Err(ApplicationContractError::Inconsistent {
                field: "analyzer policy request authority",
            });
        }
        Ok(PolicyEvaluationV1 {
            consumer,
            context: context.clone(),
            evidence_horizon: None,
            decision: self.analyzer.snapshot(input),
        })
    }

    pub fn authorize_source(
        &self,
        consumer: PolicyConsumerV1,
        context: &PolicyEvaluationContextV1,
        input: &SourceAuthorizationInputV1,
    ) -> Result<PolicyEvaluationV1<SourceAuthorizationDecisionV1>, ApplicationContractError> {
        if !matches!(
            consumer,
            PolicyConsumerV1::SourceAuthorization
                | PolicyConsumerV1::HintRouting
                | PolicyConsumerV1::RetrievalRouting
                | PolicyConsumerV1::MemoryRouting
        ) {
            return Err(ApplicationContractError::Inconsistent {
                field: "source authorization policy consumer",
            });
        }
        context.validate_common(
            input.policy_revision,
            &input.policy_digest,
            &input.configuration_digest,
        )?;
        if context.request.admission_at(input.evaluated_at) != RequestAdmission::Admitted
            || input.requester != *context.request.actor()
            || input.resolved_owner_scope.owner
                != SourceOwnerV1::Project(context.scope().project_id.clone())
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "source authorization application scope",
            });
        }
        Ok(PolicyEvaluationV1 {
            consumer,
            context: context.clone(),
            evidence_horizon: None,
            decision: self.source_authorization.evaluate(input),
        })
    }

    pub fn recheck_configuration(
        &self,
        context: &PolicyEvaluationContextV1,
        current: &ConfigurationMutationGrantSnapshotV1,
        input: ConfigurationMutationRecheckInputV1<'_>,
    ) -> Result<
        PolicyEvaluationV1<ConfigurationMutationRecheckDispositionV1>,
        ApplicationContractError,
    > {
        context.validate()?;
        if context.request.admission_at(input.evaluated_at) != RequestAdmission::Admitted
            || current.actor_id != *context.request.actor()
            || current.scope_digest != context.scope().scope_digest
            || current.expected_configuration_revision != context.configuration_revision
            || current.policy_epoch != context.policy_revision
            || current.policy_digest.as_str() != context.policy_digest.as_str()
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "configuration policy application scope",
            });
        }
        Ok(PolicyEvaluationV1 {
            consumer: PolicyConsumerV1::ConfigurationPolicy,
            context: context.clone(),
            evidence_horizon: None,
            decision: self.configuration.evaluate(current, input),
        })
    }

    pub fn classify_git_conflict(
        &self,
        context: &PolicyEvaluationContextV1,
        input: &GitEffectClassificationInputV1,
    ) -> Result<PolicyEvaluationV1<GitEffectDecisionV1>, ApplicationContractError> {
        context.validate_common(
            input.policy_revision,
            &input.policy_digest,
            &input.configuration_digest,
        )?;
        if context.request.admission_at(input.evaluated_at) != RequestAdmission::Admitted {
            return Err(ApplicationContractError::Inconsistent {
                field: "Git policy request authority",
            });
        }
        Ok(PolicyEvaluationV1 {
            consumer: PolicyConsumerV1::ConflictRouting,
            context: context.clone(),
            evidence_horizon: None,
            decision: self.git.evaluate(input),
        })
    }

    fn validate_route_request(
        &self,
        context: &PolicyEvaluationContextV1,
        request: &CapabilityRoutingRequestV1,
    ) -> Result<(), ApplicationContractError> {
        let granted = context
            .request
            .grant()
            .allowed_capabilities
            .iter()
            .map(|capability| capability.as_str())
            .collect::<BTreeSet<_>>();
        if request
            .grant
            .allowed_capabilities
            .iter()
            .any(|capability| !granted.contains(capability.as_str()))
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "policy route authorization",
            });
        }
        if request.catalog_revision != self.catalog_revision
            || request.catalog_digest != self.catalog_digest
            || request.grant != routing_grant(context.request())?
            || request.deadline != context.request().deadline().expires_at
            || request.cancellation != routing_cancellation(context.request())
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "policy route authority snapshot",
            });
        }
        for capability in &request.declared_capability_order {
            let Some(registered) = self.capabilities.get(capability.as_str()) else {
                return Err(ApplicationContractError::Inconsistent {
                    field: "declared policy route",
                });
            };
            if policy_identifier(&registered.use_case_id)? != request.requested_use_case_id {
                return Err(ApplicationContractError::Inconsistent {
                    field: "declared policy use case",
                });
            }
        }
        for candidate in &request.candidates {
            let Some(registered) = self.capabilities.get(candidate.capability_id.as_str()) else {
                return Err(ApplicationContractError::Inconsistent {
                    field: "candidate policy route",
                });
            };
            if (registered.catalog_availability == CapabilityAvailabilityV1::Unavailable
                && candidate.availability != CapabilityAvailabilityV1::Unavailable)
                || candidate.use_case_id != policy_identifier(&registered.use_case_id)?
                || candidate.effect_class != route_effect(registered.effect_class)?
                || candidate.catalog_revision != self.catalog_revision
                || candidate.catalog_digest != self.catalog_digest
                || candidate.capability_digest != registered.capability_digest
            {
                return Err(ApplicationContractError::Inconsistent {
                    field: "policy route catalog projection",
                });
            }
        }
        Ok(())
    }
}

fn policy_identifier(value: &str) -> Result<PolicyIdentifierV1, ApplicationContractError> {
    PolicyIdentifierV1::new(value).map_err(|_| ApplicationContractError::InvalidIdentifier {
        field: "policy routing identifier",
    })
}

fn routing_grant(
    request: &RequestContext,
) -> Result<CapabilityRoutingGrantV1, ApplicationContractError> {
    let grant = request.grant();
    Ok(CapabilityRoutingGrantV1 {
        grant_id: policy_identifier(grant.grant_id.as_str())?,
        revision: grant.revision,
        digest: grant.digest.clone(),
        allowed_capabilities: grant
            .allowed_capabilities
            .iter()
            .map(|capability| DomainCapabilityId::new(capability.as_str().to_owned()))
            .collect::<Result<_, _>>()?,
        allowed_use_cases: grant
            .allowed_use_cases
            .iter()
            .map(|use_case| policy_identifier(use_case.as_str()))
            .collect::<Result<_, _>>()?,
        issued_at: grant.issued_at,
        expires_at: grant.expires_at,
        state: CapabilityRoutingGrantStateV1::Active,
    })
}

fn routing_cancellation(request: &RequestContext) -> CapabilityRoutingCancellationV1 {
    match &request.cancellation().state {
        CancellationState::Active => CapabilityRoutingCancellationV1::Active,
        CancellationState::Cancelled { requested_at } => {
            CapabilityRoutingCancellationV1::Cancelled {
                requested_at: *requested_at,
            }
        }
    }
}

fn catalog_availability(availability: &AvailabilityContract) -> CapabilityAvailabilityV1 {
    match availability {
        AvailabilityContract::Available | AvailabilityContract::Deprecated { .. } => {
            CapabilityAvailabilityV1::Available
        }
        AvailabilityContract::Unavailable { .. } => CapabilityAvailabilityV1::Unavailable,
    }
}

fn route_effect(effect: EffectClass) -> Result<CapabilityEffectClassV1, ApplicationContractError> {
    match effect {
        EffectClass::Read => Ok(CapabilityEffectClassV1::Read),
        EffectClass::Preview => Ok(CapabilityEffectClassV1::Preview),
        EffectClass::GitIndexStage => Ok(CapabilityEffectClassV1::GitIndexStage),
        EffectClass::GitIndexUnstage => Ok(CapabilityEffectClassV1::GitIndexUnstage),
        EffectClass::GitIndexCommit => Ok(CapabilityEffectClassV1::GitIndexCommit),
        EffectClass::SourceEdit | EffectClass::ConfigurationWrite | EffectClass::Administrative => {
            Err(ApplicationContractError::Inconsistent {
                field: "capability routing effect class",
            })
        }
    }
}
