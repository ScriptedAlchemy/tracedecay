//! Root-owned assembly of the application capability catalog.
//!
//! Composition validates metadata against the closed application handler
//! descriptors and binds them to one caller-supplied canonical dispatcher.

use std::collections::BTreeSet;

use thiserror::Error;
use tracedecay_application::handlers::BoundApplicationHandler;
use tracedecay_application::{
    APPLICATION_DEFAULT_PROFILE_ID, ApplicationContractError, ApplicationHandlerDescriptors,
    application_catalog_contributions, application_handler_descriptors,
};
use tracedecay_tool_catalog::{
    BindingSurface, CapabilityId, CatalogContributionV1, CatalogSnapshotBuilderV1,
    CatalogSnapshotV1, CatalogValidationError, IdentifierError, ProfileBudget, ProfileDefinition,
    ProfileDefinitionInputV1, ProfileId, ProfileKind, RoutingFixtureExpectation, RoutingFixtureV1,
    UseCaseId,
};

const APPLICATION_COMPACT_PROFILE_ID: &str = "profile.compact";
const APPLICATION_ADMINISTRATIVE_PROFILE_ID: &str = "profile.administrative";
const APPLICATION_HOST_LIMITED_PROFILE_ID: &str = "profile.host-limited";

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CatalogCompositionError {
    #[error("application catalog contribution is invalid: {0}")]
    Application(#[from] ApplicationContractError),
    #[error("application catalog snapshot is invalid: {0}")]
    Catalog(#[from] CatalogValidationError),
    #[error("application catalog identifier is invalid: {0}")]
    Identifier(#[from] IdentifierError),
}

/// Immutable catalog metadata and the application descriptors bound to one
/// retained canonical dispatcher.
pub struct ApplicationCatalogComposition<Dispatcher> {
    snapshot: CatalogSnapshotV1,
    handlers: ApplicationHandlerDescriptors,
    dispatcher: Dispatcher,
}

impl<Dispatcher> ApplicationCatalogComposition<Dispatcher> {
    pub fn snapshot(&self) -> &CatalogSnapshotV1 {
        &self.snapshot
    }

    pub fn handler(
        &self,
        use_case_id: &UseCaseId,
    ) -> Option<BoundApplicationHandler<'_, Dispatcher>> {
        self.handlers
            .get(use_case_id)
            .map(|descriptor| descriptor.bind(&self.dispatcher))
    }

    /// Bind one validated descriptor to a request-scoped dispatcher.
    ///
    /// Long-lived catalog metadata stays immutable while adapters supply the
    /// exact mounted authorities for one invocation. The descriptor remains
    /// the same application-owned handler validated during composition.
    pub fn bind_handler<'a, RequestDispatcher>(
        &'a self,
        use_case_id: &UseCaseId,
        dispatcher: &'a RequestDispatcher,
    ) -> Option<BoundApplicationHandler<'a, RequestDispatcher>> {
        self.handlers
            .get(use_case_id)
            .map(|descriptor| descriptor.bind(dispatcher))
    }
}

/// Compose the immutable catalog and retain its one canonical application
/// dispatcher. Request and result types remain compile-time checked by the
/// dispatcher's per-request trait implementations.
pub fn compose_application_catalog<Dispatcher>(
    dispatcher: Dispatcher,
) -> Result<ApplicationCatalogComposition<Dispatcher>, CatalogCompositionError> {
    compose_application_catalog_with(|_snapshot| dispatcher)
}

/// Compose the catalog when the retained dispatcher also needs the validated
/// immutable snapshot for its own binding checks.
pub fn compose_application_catalog_with<Dispatcher>(
    dispatcher: impl FnOnce(&CatalogSnapshotV1) -> Dispatcher,
) -> Result<ApplicationCatalogComposition<Dispatcher>, CatalogCompositionError> {
    let (snapshot, handlers) = assemble_application_catalog()?;
    let dispatcher = dispatcher(&snapshot);
    Ok(ApplicationCatalogComposition {
        snapshot,
        handlers,
        dispatcher,
    })
}

/// Build the immutable catalog snapshot used by transport binding resolution.
/// Callers that execute operations must use [`compose_application_catalog`].
pub fn build_application_catalog_snapshot() -> Result<CatalogSnapshotV1, CatalogCompositionError> {
    assemble_application_catalog().map(|(snapshot, _handlers)| snapshot)
}

fn assemble_application_catalog()
-> Result<(CatalogSnapshotV1, ApplicationHandlerDescriptors), CatalogCompositionError> {
    let mut contributions = application_catalog_contributions()?;
    let handlers = application_handler_descriptors()?;
    contributions.sort_by(|left, right| left.contribution_id().cmp(right.contribution_id()));
    validate_application_catalog(&contributions, &handlers)?;
    let profiles = application_profiles(&contributions)?;
    let mut builder = CatalogSnapshotBuilderV1::new();

    for contribution in contributions {
        builder.add_contribution(contribution);
    }
    for handler in handlers.catalog_descriptors()? {
        builder.add_handler(handler);
    }
    for profile in profiles {
        builder.add_profile(profile);
    }

    Ok((builder.build()?, handlers))
}

/// Validates the application-owned catalog before application-only handler
/// identity is lowered to the generic tool-catalog descriptor.
///
/// Contribution builders derive availability and bindings from their concrete
/// runtime registrars. Root composition only validates the resulting
/// use-case/schema mapping; it does not maintain a second availability list.
pub fn validate_application_catalog(
    contributions: &[CatalogContributionV1],
    handlers: &ApplicationHandlerDescriptors,
) -> Result<(), CatalogCompositionError> {
    handlers.validate_against(contributions)?;
    Ok(())
}

fn application_profiles(
    contributions: &[CatalogContributionV1],
) -> Result<Vec<ProfileDefinition>, CatalogCompositionError> {
    [
        (
            APPLICATION_DEFAULT_PROFILE_ID,
            ProfileKind::Default,
            ProfileBudget::new(256, 80_000_000, 18_000)?,
            true,
        ),
        (
            APPLICATION_COMPACT_PROFILE_ID,
            ProfileKind::Compact,
            ProfileBudget::COMPACT,
            false,
        ),
        (
            APPLICATION_ADMINISTRATIVE_PROFILE_ID,
            ProfileKind::Administrative,
            ProfileBudget::ADMINISTRATIVE,
            false,
        ),
        (
            APPLICATION_HOST_LIMITED_PROFILE_ID,
            ProfileKind::HostLimited,
            ProfileBudget::HOST_LIMITED,
            false,
        ),
    ]
    .into_iter()
    .map(|(profile_id, kind, budget, requires_cli_mcp_pairing)| {
        application_profile(
            contributions,
            profile_id,
            kind,
            budget,
            requires_cli_mcp_pairing,
        )
    })
    .collect()
}

fn application_profile(
    contributions: &[CatalogContributionV1],
    profile_id: &str,
    kind: ProfileKind,
    budget: ProfileBudget,
    requires_cli_mcp_pairing: bool,
) -> Result<ProfileDefinition, CatalogCompositionError> {
    let profile_id = ProfileId::new(profile_id)?;
    let capabilities: Vec<_> = contributions
        .iter()
        .flat_map(tracedecay_tool_catalog::CatalogContributionV1::capabilities)
        .filter(|capability| {
            capability.availability().is_callable()
                && capability.profile_eligibility().contains(&profile_id)
        })
        .collect();
    let capability_ids = capabilities
        .iter()
        .map(|capability| capability.capability_id().clone())
        .collect::<Vec<_>>();
    let mut used_fixture_utterances = BTreeSet::from([
        "Preview and apply these index changes".to_owned(),
        "Explain the weather".to_owned(),
        "Stage these selected hunks".to_owned(),
    ]);
    let mut routing_fixtures = capabilities
        .iter()
        .map(|capability| -> Result<_, CatalogCompositionError> {
            let query = unique_routing_fixture_utterance(capability, &mut used_fixture_utterances)?;
            Ok(RoutingFixtureV1::new(
                query,
                RoutingFixtureExpectation::Select {
                    capability_id: capability.capability_id().clone(),
                },
            )?)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !capability_ids.is_empty() {
        let git_preview = CapabilityId::new("capability.application.git.preview")?;
        let git_apply = CapabilityId::new("capability.application.git.apply")?;
        if capability_ids.contains(&git_preview) && capability_ids.contains(&git_apply) {
            routing_fixtures.push(RoutingFixtureV1::new(
                "Preview and apply these index changes",
                RoutingFixtureExpectation::ambiguous(vec![git_preview, git_apply])?,
            )?);
        }
        routing_fixtures.push(RoutingFixtureV1::new(
            "Explain the weather",
            RoutingFixtureExpectation::Reject,
        )?);
        let stage_hunks = CapabilityId::new("capability.git.stage-hunks")?;
        let insufficient_capability_id = contributions
            .iter()
            .flat_map(tracedecay_tool_catalog::CatalogContributionV1::capabilities)
            .map(|capability| capability.capability_id())
            .find(|capability_id| {
                *capability_id == &stage_hunks && !capability_ids.contains(*capability_id)
            })
            .or_else(|| {
                contributions
                    .iter()
                    .flat_map(tracedecay_tool_catalog::CatalogContributionV1::capabilities)
                    .map(|capability| capability.capability_id())
                    .find(|capability_id| !capability_ids.contains(*capability_id))
            });
        if let Some(capability_id) = insufficient_capability_id {
            routing_fixtures.push(RoutingFixtureV1::new(
                "Stage these selected hunks",
                RoutingFixtureExpectation::InsufficientCapability {
                    capability_id: capability_id.clone(),
                },
            )?);
        }
    }
    let enabled_surfaces = [
        BindingSurface::Cli,
        BindingSurface::Mcp,
        BindingSurface::Http,
        BindingSurface::Lsp,
        BindingSurface::Dashboard,
    ]
    .into_iter()
    .filter(|surface| {
        contributions
            .iter()
            .flat_map(tracedecay_tool_catalog::CatalogContributionV1::bindings)
            .any(|binding| {
                binding.surface() == *surface && capability_ids.contains(binding.capability_id())
            })
    })
    .collect();
    Ok(ProfileDefinition::new(ProfileDefinitionInputV1 {
        profile_id,
        kind,
        capability_ids,
        enabled_surfaces,
        requires_cli_mcp_pairing,
        budget,
        routing_fixtures,
    })?)
}

fn unique_routing_fixture_utterance(
    capability: &tracedecay_tool_catalog::CapabilityManifestV1,
    used: &mut BTreeSet<String>,
) -> Result<String, CatalogCompositionError> {
    for candidate in capability
        .routing()
        .examples()
        .iter()
        .cloned()
        .chain(std::iter::once(capability.routing().name().to_owned()))
    {
        if used.insert(candidate.clone()) {
            return Ok(candidate);
        }
    }

    let fallback = format!(
        "{} [{}]",
        capability.routing().name(),
        capability.capability_id().as_str()
    );
    if !used.insert(fallback.clone()) {
        return Err(CatalogValidationError::DuplicateValue {
            field: "profile routing fixture utterances",
        }
        .into());
    }
    Ok(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_capacity_tracks_composed_runtime() {
        let snapshot = build_application_catalog_snapshot().expect("application catalog");
        let contributions = application_catalog_contributions().expect("application contributions");
        let default_profile_id =
            ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).expect("default profile id");
        let default_profile = snapshot
            .profile(&default_profile_id)
            .expect("default application profile");
        let default_binding_count = contributions
            .iter()
            .flat_map(CatalogContributionV1::bindings)
            .filter(|binding| {
                default_profile.includes_capability(binding.capability_id())
                    && default_profile.enables_surface(binding.surface())
            })
            .count();
        assert_eq!(default_binding_count, 235);
        assert_eq!(default_profile.budget().maximum_bindings(), 256);
        assert!(default_binding_count <= default_profile.budget().maximum_bindings() as usize);
        let mut default_schemas = std::collections::BTreeMap::new();
        for capability in contributions
            .iter()
            .flat_map(CatalogContributionV1::capabilities)
            .filter(|capability| default_profile.includes_capability(capability.capability_id()))
        {
            for schema in capability.schema_refs() {
                default_schemas
                    .entry((schema.schema_id().clone(), schema.revision()))
                    .or_insert(schema.canonical_size_bytes());
            }
        }
        let default_schema_bytes = default_schemas
            .values()
            .copied()
            .map(u64::from)
            .sum::<u64>();
        assert_eq!(default_schema_bytes, 76_033_408);
        assert_eq!(default_profile.budget().maximum_schema_bytes(), 80_000_000);
        assert!(default_schema_bytes <= u64::from(default_profile.budget().maximum_schema_bytes()));
    }

    #[test]
    fn application_profiles_disambiguate_duplicate_routing_examples() {
        let snapshot = build_application_catalog_snapshot().expect("application catalog");
        for profile in snapshot.profiles() {
            let utterances = profile
                .routing_fixtures()
                .iter()
                .map(RoutingFixtureV1::utterance)
                .collect::<BTreeSet<_>>();
            assert_eq!(
                utterances.len(),
                profile.routing_fixtures().len(),
                "{} contains duplicate routing fixtures",
                profile.profile_id().as_str()
            );
        }
    }
}
