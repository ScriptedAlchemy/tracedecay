use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::binding::{BindingSurface, SurfaceBindingV1, SurfaceOperationName};
use crate::id::{BindingId, CapabilityId, ContributionId, ProfileId, RetrieverId, UseCaseId};
use crate::manifest::{CapabilityManifestV1, EffectClass, InverseContract};
use crate::profile::{ProfileDefinition, RoutingFixtureExpectation};
use crate::retrieval::RetrievalPrimitiveManifestV1;
use crate::snapshot::{ApplicationHandlerDescriptorV1, CatalogContributionV1};

/// Pure failures raised while assembling an immutable catalog snapshot.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CatalogValidationError {
    #[error("{field} contains a duplicate value")]
    DuplicateValue { field: &'static str },
    #[error("{field} must not be empty")]
    MissingValue { field: &'static str },
    #[error("{field} is invalid: {reason}")]
    InvalidValue {
        field: &'static str,
        reason: &'static str,
    },
    #[error("capability {capability_id} is invalid: {reason}")]
    InvalidCapability {
        capability_id: CapabilityId,
        reason: &'static str,
    },
    #[error("duplicate contribution ID {0}")]
    DuplicateContributionId(ContributionId),
    #[error("contribution {contribution_id} depends on missing contribution {dependency_id}")]
    MissingContributionDependency {
        contribution_id: ContributionId,
        dependency_id: ContributionId,
    },
    #[error("contribution dependency cycle includes {contribution_id}")]
    ContributionDependencyCycle { contribution_id: ContributionId },
    #[error("duplicate capability ID {0}")]
    DuplicateCapabilityId(CapabilityId),
    #[error("duplicate handler descriptor for use case {0}")]
    DuplicateHandlerUseCaseId(UseCaseId),
    #[error("capability {capability_id} has no application handler descriptor for {use_case_id}")]
    MissingHandler {
        capability_id: CapabilityId,
        use_case_id: UseCaseId,
    },
    #[error("capability {capability_id} references missing inverse capability {inverse_id}")]
    MissingInverseCapability {
        capability_id: CapabilityId,
        inverse_id: CapabilityId,
    },
    #[error("capability {capability_id} and its application handler use incompatible schemas")]
    HandlerSchemaMismatch { capability_id: CapabilityId },
    #[error("capability {capability_id} resolves to a handler for {handler_capability_id}")]
    HandlerCapabilityMismatch {
        capability_id: CapabilityId,
        handler_capability_id: CapabilityId,
    },
    #[error("duplicate binding ID {0}")]
    DuplicateBindingId(BindingId),
    #[error("duplicate {surface:?} operation spelling {operation}")]
    DuplicateSurfaceOperation {
        surface: BindingSurface,
        operation: SurfaceOperationName,
    },
    #[error("binding {binding_id} references missing capability {capability_id}")]
    MissingBindingCapability {
        binding_id: BindingId,
        capability_id: CapabilityId,
    },
    #[error("binding {binding_id} is not declared by capability {capability_id}")]
    BindingNotDeclaredByCapability {
        binding_id: BindingId,
        capability_id: CapabilityId,
    },
    #[error("capability {capability_id} declares missing binding {binding_id}")]
    MissingManifestBinding {
        capability_id: CapabilityId,
        binding_id: BindingId,
    },
    #[error("binding {binding_id} does not point back to capability {capability_id}")]
    BindingCapabilityMismatch {
        binding_id: BindingId,
        capability_id: CapabilityId,
    },
    #[error("binding {binding_id} aliases missing binding {alias_of}")]
    MissingAliasTarget {
        binding_id: BindingId,
        alias_of: BindingId,
    },
    #[error("binding alias {binding_id} must target the same capability")]
    AliasCapabilityMismatch { binding_id: BindingId },
    #[error("binding alias {binding_id} cannot target another alias")]
    AliasTargetsAlias { binding_id: BindingId },
    #[error("duplicate retrieval primitive capability ID {0}")]
    DuplicateRetrievalCapabilityId(CapabilityId),
    #[error("duplicate retriever ID {0}")]
    DuplicateRetrieverId(RetrieverId),
    #[error("retrieval primitive {retriever_id} references missing capability {capability_id}")]
    MissingRetrievalCapability {
        retriever_id: RetrieverId,
        capability_id: CapabilityId,
    },
    #[error("retrieval primitive {retriever_id} must reference a read capability")]
    RetrievalRequiresReadCapability { retriever_id: RetrieverId },
    #[error("retrieval primitive {retriever_id} has schemas incompatible with its capability")]
    RetrievalSchemaMismatch { retriever_id: RetrieverId },
    #[error("retrieval primitive {retriever_id} has pagination incompatible with its capability")]
    RetrievalPaginationMismatch { retriever_id: RetrieverId },
    #[error(
        "retrieval primitive {retriever_id} has lifecycle metadata incompatible with its capability"
    )]
    RetrievalLifecycleMismatch { retriever_id: RetrieverId },
    #[error("duplicate profile ID {0}")]
    DuplicateProfileId(ProfileId),
    #[error("capability {capability_id} references missing profile {profile_id}")]
    MissingManifestProfile {
        capability_id: CapabilityId,
        profile_id: ProfileId,
    },
    #[error("capability {capability_id} is missing from profile {profile_id}")]
    ProfileEligibilityMismatch {
        capability_id: CapabilityId,
        profile_id: ProfileId,
    },
    #[error("profile {profile_id} references missing capability {capability_id}")]
    MissingProfileCapability {
        profile_id: ProfileId,
        capability_id: CapabilityId,
    },
    #[error("profile {profile_id} includes capability {capability_id} without eligibility")]
    ProfileMembershipMismatch {
        profile_id: ProfileId,
        capability_id: CapabilityId,
    },
    #[error("profile {profile_id} exceeded its {budget} budget: {actual} > {maximum}")]
    ProfileBudgetExceeded {
        profile_id: ProfileId,
        budget: &'static str,
        actual: u64,
        maximum: u64,
    },
    #[error("paired profile {profile_id} lacks {surface:?} binding for {capability_id}")]
    PairedProfileMissingBinding {
        profile_id: ProfileId,
        capability_id: CapabilityId,
        surface: BindingSurface,
    },
    #[error(
        "profile {profile_id} routing fixture references incompatible capability {capability_id}"
    )]
    InvalidRoutingFixtureCapability {
        profile_id: ProfileId,
        capability_id: CapabilityId,
    },
}

pub(crate) fn validate_catalog(
    contributions: &[CatalogContributionV1],
    profiles: &[ProfileDefinition],
    handlers: &[ApplicationHandlerDescriptorV1],
) -> Result<(), CatalogValidationError> {
    validate_contribution_dependencies(contributions)?;

    let capabilities = index_capabilities(contributions)?;
    let bindings = index_bindings(contributions, &capabilities)?;
    let retrievals = index_retrievals(contributions, &capabilities)?;
    let profiles = index_profiles(profiles, &capabilities)?;
    let handlers = index_handlers(handlers)?;

    validate_inverse_contracts(&capabilities)?;
    validate_handler_contracts(&capabilities, &handlers)?;
    validate_profile_membership(&capabilities, &profiles)?;
    validate_retrieval_contracts(&retrievals, &capabilities)?;
    validate_profiles(&profiles, &capabilities, &bindings)?;
    Ok(())
}

fn validate_inverse_contracts(
    capabilities: &BTreeMap<CapabilityId, &CapabilityManifestV1>,
) -> Result<(), CatalogValidationError> {
    for capability in capabilities.values() {
        let InverseContract::Capability { capability_id } = capability.inverse() else {
            continue;
        };
        if !capabilities.contains_key(capability_id) {
            return Err(CatalogValidationError::MissingInverseCapability {
                capability_id: capability.capability_id().clone(),
                inverse_id: capability_id.clone(),
            });
        }
    }
    Ok(())
}

fn validate_contribution_dependencies(
    contributions: &[CatalogContributionV1],
) -> Result<(), CatalogValidationError> {
    let mut dependencies = BTreeMap::new();
    for contribution in contributions {
        if dependencies
            .insert(
                contribution.contribution_id().clone(),
                contribution.depends_on().to_vec(),
            )
            .is_some()
        {
            return Err(CatalogValidationError::DuplicateContributionId(
                contribution.contribution_id().clone(),
            ));
        }
    }

    for (contribution_id, dependency_ids) in &dependencies {
        for dependency_id in dependency_ids {
            if !dependencies.contains_key(dependency_id) {
                return Err(CatalogValidationError::MissingContributionDependency {
                    contribution_id: contribution_id.clone(),
                    dependency_id: dependency_id.clone(),
                });
            }
        }
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for contribution_id in dependencies.keys() {
        visit_contribution(contribution_id, &dependencies, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn visit_contribution(
    contribution_id: &ContributionId,
    dependencies: &BTreeMap<ContributionId, Vec<ContributionId>>,
    visiting: &mut BTreeSet<ContributionId>,
    visited: &mut BTreeSet<ContributionId>,
) -> Result<(), CatalogValidationError> {
    if visited.contains(contribution_id) {
        return Ok(());
    }
    if !visiting.insert(contribution_id.clone()) {
        return Err(CatalogValidationError::ContributionDependencyCycle {
            contribution_id: contribution_id.clone(),
        });
    }

    for dependency_id in dependencies
        .get(contribution_id)
        .expect("dependency index is constructed from this contribution")
    {
        visit_contribution(dependency_id, dependencies, visiting, visited)?;
    }

    visiting.remove(contribution_id);
    visited.insert(contribution_id.clone());
    Ok(())
}

fn index_capabilities(
    contributions: &[CatalogContributionV1],
) -> Result<BTreeMap<CapabilityId, &CapabilityManifestV1>, CatalogValidationError> {
    let mut capabilities = BTreeMap::new();
    for capability in contributions
        .iter()
        .flat_map(|contribution| contribution.capabilities())
    {
        capability.validate_intrinsic()?;
        if capabilities
            .insert(capability.capability_id().clone(), capability)
            .is_some()
        {
            return Err(CatalogValidationError::DuplicateCapabilityId(
                capability.capability_id().clone(),
            ));
        }
    }
    Ok(capabilities)
}

fn index_handlers(
    handlers: &[ApplicationHandlerDescriptorV1],
) -> Result<BTreeMap<UseCaseId, &ApplicationHandlerDescriptorV1>, CatalogValidationError> {
    let mut index = BTreeMap::new();
    for handler in handlers {
        if index
            .insert(handler.use_case_id().clone(), handler)
            .is_some()
        {
            return Err(CatalogValidationError::DuplicateHandlerUseCaseId(
                handler.use_case_id().clone(),
            ));
        }
    }
    Ok(index)
}

fn validate_handler_contracts(
    capabilities: &BTreeMap<CapabilityId, &CapabilityManifestV1>,
    handlers: &BTreeMap<UseCaseId, &ApplicationHandlerDescriptorV1>,
) -> Result<(), CatalogValidationError> {
    for capability in capabilities.values() {
        let Some(handler) = handlers.get(capability.use_case_id()) else {
            return Err(CatalogValidationError::MissingHandler {
                capability_id: capability.capability_id().clone(),
                use_case_id: capability.use_case_id().clone(),
            });
        };
        if handler.capability_id() != capability.capability_id() {
            return Err(CatalogValidationError::HandlerCapabilityMismatch {
                capability_id: capability.capability_id().clone(),
                handler_capability_id: handler.capability_id().clone(),
            });
        }
        if handler.request_schema() != capability.request_schema()
            || handler.result_schema() != capability.result_schema()
        {
            return Err(CatalogValidationError::HandlerSchemaMismatch {
                capability_id: capability.capability_id().clone(),
            });
        }
    }
    Ok(())
}

fn index_bindings<'a>(
    contributions: &'a [CatalogContributionV1],
    capabilities: &BTreeMap<CapabilityId, &'a CapabilityManifestV1>,
) -> Result<BTreeMap<BindingId, &'a SurfaceBindingV1>, CatalogValidationError> {
    let mut bindings = BTreeMap::new();
    let mut surface_names = BTreeSet::new();

    for binding in contributions
        .iter()
        .flat_map(|contribution| contribution.bindings())
    {
        if bindings
            .insert(binding.binding_id().clone(), binding)
            .is_some()
        {
            return Err(CatalogValidationError::DuplicateBindingId(
                binding.binding_id().clone(),
            ));
        }
        if !surface_names.insert((binding.surface(), binding.operation().clone())) {
            return Err(CatalogValidationError::DuplicateSurfaceOperation {
                surface: binding.surface(),
                operation: binding.operation().clone(),
            });
        }
        let Some(capability) = capabilities.get(binding.capability_id()) else {
            return Err(CatalogValidationError::MissingBindingCapability {
                binding_id: binding.binding_id().clone(),
                capability_id: binding.capability_id().clone(),
            });
        };
        if !capability.binding_ids().contains(binding.binding_id()) {
            return Err(CatalogValidationError::BindingNotDeclaredByCapability {
                binding_id: binding.binding_id().clone(),
                capability_id: binding.capability_id().clone(),
            });
        }
    }

    for capability in capabilities.values() {
        for binding_id in capability.binding_ids() {
            let Some(binding) = bindings.get(binding_id) else {
                return Err(CatalogValidationError::MissingManifestBinding {
                    capability_id: capability.capability_id().clone(),
                    binding_id: binding_id.clone(),
                });
            };
            if binding.capability_id() != capability.capability_id() {
                return Err(CatalogValidationError::BindingCapabilityMismatch {
                    binding_id: binding_id.clone(),
                    capability_id: capability.capability_id().clone(),
                });
            }
        }
    }

    for binding in bindings.values() {
        let Some(alias_of) = binding.alias_of() else {
            continue;
        };
        let Some(canonical) = bindings.get(alias_of) else {
            return Err(CatalogValidationError::MissingAliasTarget {
                binding_id: binding.binding_id().clone(),
                alias_of: alias_of.clone(),
            });
        };
        if canonical.is_alias() {
            return Err(CatalogValidationError::AliasTargetsAlias {
                binding_id: binding.binding_id().clone(),
            });
        }
        if canonical.capability_id() != binding.capability_id() {
            return Err(CatalogValidationError::AliasCapabilityMismatch {
                binding_id: binding.binding_id().clone(),
            });
        }
    }

    Ok(bindings)
}

fn index_retrievals<'a>(
    contributions: &'a [CatalogContributionV1],
    capabilities: &BTreeMap<CapabilityId, &'a CapabilityManifestV1>,
) -> Result<BTreeMap<CapabilityId, &'a RetrievalPrimitiveManifestV1>, CatalogValidationError> {
    let mut by_capability = BTreeMap::new();
    let mut retrievers = BTreeSet::new();

    for retrieval in contributions
        .iter()
        .flat_map(|contribution| contribution.retrieval_primitives())
    {
        if by_capability
            .insert(retrieval.capability_id().clone(), retrieval)
            .is_some()
        {
            return Err(CatalogValidationError::DuplicateRetrievalCapabilityId(
                retrieval.capability_id().clone(),
            ));
        }
        if !retrievers.insert(retrieval.retriever_id().clone()) {
            return Err(CatalogValidationError::DuplicateRetrieverId(
                retrieval.retriever_id().clone(),
            ));
        }
        if !capabilities.contains_key(retrieval.capability_id()) {
            return Err(CatalogValidationError::MissingRetrievalCapability {
                retriever_id: retrieval.retriever_id().clone(),
                capability_id: retrieval.capability_id().clone(),
            });
        }
    }
    Ok(by_capability)
}

fn validate_retrieval_contracts(
    retrievals: &BTreeMap<CapabilityId, &RetrievalPrimitiveManifestV1>,
    capabilities: &BTreeMap<CapabilityId, &CapabilityManifestV1>,
) -> Result<(), CatalogValidationError> {
    for retrieval in retrievals.values() {
        let capability = capabilities
            .get(retrieval.capability_id())
            .expect("retrieval capability was indexed before validation");
        if capability.effect() != EffectClass::Read {
            return Err(CatalogValidationError::RetrievalRequiresReadCapability {
                retriever_id: retrieval.retriever_id().clone(),
            });
        }
        if retrieval.request_schema() != capability.request_schema()
            || retrieval.evidence_packet_schema() != capability.result_schema()
        {
            return Err(CatalogValidationError::RetrievalSchemaMismatch {
                retriever_id: retrieval.retriever_id().clone(),
            });
        }
        let pagination = capability.pagination().ok_or_else(|| {
            CatalogValidationError::RetrievalPaginationMismatch {
                retriever_id: retrieval.retriever_id().clone(),
            }
        })?;
        if pagination.default_page_size() != retrieval.default_page_size()
            || pagination.maximum_page_size() != retrieval.maximum_page_size()
        {
            return Err(CatalogValidationError::RetrievalPaginationMismatch {
                retriever_id: retrieval.retriever_id().clone(),
            });
        }
        if capability.deadline().behavior() != retrieval.deadline_behavior()
            || retrieval
                .cancellation_points()
                .iter()
                .any(|point| !capability.cancellation().observes(*point))
        {
            return Err(CatalogValidationError::RetrievalLifecycleMismatch {
                retriever_id: retrieval.retriever_id().clone(),
            });
        }
    }
    Ok(())
}

fn index_profiles<'a>(
    profiles: &'a [ProfileDefinition],
    capabilities: &BTreeMap<CapabilityId, &CapabilityManifestV1>,
) -> Result<BTreeMap<ProfileId, &'a ProfileDefinition>, CatalogValidationError> {
    let mut index = BTreeMap::new();
    for profile in profiles {
        if index
            .insert(profile.profile_id().clone(), profile)
            .is_some()
        {
            return Err(CatalogValidationError::DuplicateProfileId(
                profile.profile_id().clone(),
            ));
        }
        for capability_id in profile.capability_ids() {
            if !capabilities.contains_key(capability_id) {
                return Err(CatalogValidationError::MissingProfileCapability {
                    profile_id: profile.profile_id().clone(),
                    capability_id: capability_id.clone(),
                });
            }
        }
    }
    Ok(index)
}

fn validate_profile_membership(
    capabilities: &BTreeMap<CapabilityId, &CapabilityManifestV1>,
    profiles: &BTreeMap<ProfileId, &ProfileDefinition>,
) -> Result<(), CatalogValidationError> {
    for capability in capabilities.values() {
        for profile_id in capability.profile_eligibility() {
            let Some(profile) = profiles.get(profile_id) else {
                return Err(CatalogValidationError::MissingManifestProfile {
                    capability_id: capability.capability_id().clone(),
                    profile_id: profile_id.clone(),
                });
            };
            if !profile.includes_capability(capability.capability_id()) {
                return Err(CatalogValidationError::ProfileEligibilityMismatch {
                    capability_id: capability.capability_id().clone(),
                    profile_id: profile_id.clone(),
                });
            }
        }
    }

    for profile in profiles.values() {
        for capability_id in profile.capability_ids() {
            let capability = capabilities
                .get(capability_id)
                .expect("profile capability was indexed before validation");
            if !capability
                .profile_eligibility()
                .contains(profile.profile_id())
            {
                return Err(CatalogValidationError::ProfileMembershipMismatch {
                    profile_id: profile.profile_id().clone(),
                    capability_id: capability_id.clone(),
                });
            }
        }
    }
    Ok(())
}

fn validate_profiles(
    profiles: &BTreeMap<ProfileId, &ProfileDefinition>,
    capabilities: &BTreeMap<CapabilityId, &CapabilityManifestV1>,
    bindings: &BTreeMap<BindingId, &SurfaceBindingV1>,
) -> Result<(), CatalogValidationError> {
    for profile in profiles.values() {
        validate_profile_budget(profile, capabilities, bindings)?;
        validate_paired_profile(profile, bindings)?;
        validate_routing_fixtures(profile, capabilities)?;
    }
    Ok(())
}

fn validate_profile_budget(
    profile: &ProfileDefinition,
    capabilities: &BTreeMap<CapabilityId, &CapabilityManifestV1>,
    bindings: &BTreeMap<BindingId, &SurfaceBindingV1>,
) -> Result<(), CatalogValidationError> {
    let profile_capabilities: BTreeSet<_> = profile.capability_ids().iter().cloned().collect();
    let selected_bindings: Vec<_> = bindings
        .values()
        .filter(|binding| {
            profile_capabilities.contains(binding.capability_id())
                && profile.enables_surface(binding.surface())
        })
        .collect();
    let binding_count = selected_bindings.len() as u64;
    let budget = profile.budget();
    if binding_count > u64::from(budget.maximum_bindings()) {
        return Err(CatalogValidationError::ProfileBudgetExceeded {
            profile_id: profile.profile_id().clone(),
            budget: "bindings",
            actual: binding_count,
            maximum: u64::from(budget.maximum_bindings()),
        });
    }

    let mut routing_tokens = 0_u64;
    for capability_id in profile.capability_ids() {
        let capability = capabilities
            .get(capability_id)
            .expect("profile capability was indexed before budget validation");
        routing_tokens += u64::from(capability.routing().estimated_routing_tokens());
    }
    if routing_tokens > u64::from(budget.maximum_routing_tokens()) {
        return Err(CatalogValidationError::ProfileBudgetExceeded {
            profile_id: profile.profile_id().clone(),
            budget: "routing tokens",
            actual: routing_tokens,
            maximum: u64::from(budget.maximum_routing_tokens()),
        });
    }
    Ok(())
}

fn validate_paired_profile(
    profile: &ProfileDefinition,
    bindings: &BTreeMap<BindingId, &SurfaceBindingV1>,
) -> Result<(), CatalogValidationError> {
    if !profile.requires_cli_mcp_pairing() {
        return Ok(());
    }

    for capability_id in profile.capability_ids() {
        let cli = bindings.values().any(|binding| {
            binding.capability_id() == capability_id
                && binding.surface() == BindingSurface::Cli
                && profile.enables_surface(BindingSurface::Cli)
        });
        let mcp = bindings.values().any(|binding| {
            binding.capability_id() == capability_id
                && binding.surface() == BindingSurface::Mcp
                && profile.enables_surface(BindingSurface::Mcp)
        });
        if !cli {
            return Err(CatalogValidationError::PairedProfileMissingBinding {
                profile_id: profile.profile_id().clone(),
                capability_id: capability_id.clone(),
                surface: BindingSurface::Cli,
            });
        }
        if !mcp {
            return Err(CatalogValidationError::PairedProfileMissingBinding {
                profile_id: profile.profile_id().clone(),
                capability_id: capability_id.clone(),
                surface: BindingSurface::Mcp,
            });
        }
    }
    Ok(())
}

/// Check that every routing fixture names capabilities that exist and sit on
/// the side of the profile boundary its expectation claims.
///
/// Fixture *completeness* is deliberately not checked. A fixture carries an
/// utterance and an expectation tag but nothing in the catalog evaluates an
/// utterance, so demanding one fixture per capability only forced composers to
/// mint a placeholder per capability and made every profile that omitted a
/// capability invalid the moment a capability was added anywhere.
fn validate_routing_fixtures(
    profile: &ProfileDefinition,
    capabilities: &BTreeMap<CapabilityId, &CapabilityManifestV1>,
) -> Result<(), CatalogValidationError> {
    let invalid =
        |capability_id: &CapabilityId| CatalogValidationError::InvalidRoutingFixtureCapability {
            profile_id: profile.profile_id().clone(),
            capability_id: capability_id.clone(),
        };

    for fixture in profile.routing_fixtures() {
        match fixture.expectation() {
            // A selectable or ambiguous outcome must name capabilities the
            // profile actually exposes.
            RoutingFixtureExpectation::Select { capability_id } => {
                if !profile.includes_capability(capability_id) {
                    return Err(invalid(capability_id));
                }
            }
            RoutingFixtureExpectation::Ambiguous { capability_ids } => {
                for capability_id in capability_ids {
                    if !profile.includes_capability(capability_id) {
                        return Err(invalid(capability_id));
                    }
                }
            }
            // An insufficient-capability outcome is only meaningful for a
            // known capability the profile withholds.
            RoutingFixtureExpectation::InsufficientCapability { capability_id } => {
                if !capabilities.contains_key(capability_id)
                    || profile.includes_capability(capability_id)
                {
                    return Err(invalid(capability_id));
                }
            }
            RoutingFixtureExpectation::Reject => {}
        }
    }
    Ok(())
}
