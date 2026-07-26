//! Root-owned assembly of the application capability catalog.
//!
//! Composition validates metadata against the closed application handler
//! descriptors and binds them to one caller-supplied canonical dispatcher.

use std::collections::BTreeMap;

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
            ProfileBudget::new(192, 70_000_000, 18_000)?,
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
    let mut fixture_capabilities = BTreeMap::<String, Vec<CapabilityId>>::new();
    for capability in &capabilities {
        let query = capability
            .routing()
            .examples()
            .first()
            .cloned()
            .unwrap_or_else(|| capability.routing().name().to_owned());
        fixture_capabilities
            .entry(query)
            .or_default()
            .push(capability.capability_id().clone());
    }
    let mut routing_fixtures = Vec::new();
    for (query, fixture_ids) in fixture_capabilities {
        if fixture_ids.len() == 1 {
            routing_fixtures.push(RoutingFixtureV1::new(
                query,
                RoutingFixtureExpectation::Select {
                    capability_id: fixture_ids
                        .into_iter()
                        .next()
                        .expect("one capability was counted"),
                },
            )?);
            continue;
        }

        routing_fixtures.push(RoutingFixtureV1::new(
            query.clone(),
            RoutingFixtureExpectation::ambiguous(fixture_ids.clone())?,
        )?);
        for capability_id in fixture_ids {
            routing_fixtures.push(RoutingFixtureV1::new(
                format!("{query} ({})", capability_id.as_str()),
                RoutingFixtureExpectation::Select { capability_id },
            )?);
        }
    }
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
