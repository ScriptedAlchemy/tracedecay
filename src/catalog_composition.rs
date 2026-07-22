//! Root-owned assembly of the application capability catalog.
//!
//! Composition validates metadata against the closed application handler
//! descriptors. It retains no executable handler, store, or query authority.

use thiserror::Error;
use tracedecay_application::{
    APPLICATION_DEFAULT_PROFILE_ID, ApplicationContractError, ApplicationHandlerDescriptors,
    application_catalog_contributions, application_handler_descriptors,
};
use tracedecay_tool_catalog::{
    BindingSurface, CapabilityId, CatalogContributionV1, CatalogSnapshotBuilderV1,
    CatalogSnapshotV1, CatalogValidationError, IdentifierError, ProfileBudget, ProfileDefinition,
    ProfileDefinitionInputV1, ProfileId, ProfileKind, RoutingFixtureExpectation, RoutingFixtureV1,
};

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CatalogCompositionError {
    #[error("application catalog contribution is invalid: {0}")]
    Application(#[from] ApplicationContractError),
    #[error("application catalog snapshot is invalid: {0}")]
    Catalog(#[from] CatalogValidationError),
    #[error("application catalog identifier is invalid: {0}")]
    Identifier(#[from] IdentifierError),
}

/// Build the immutable catalog snapshot used by transport binding resolution.
/// The snapshot carries metadata only; executable ownership stays elsewhere.
pub fn build_application_catalog_snapshot() -> Result<CatalogSnapshotV1, CatalogCompositionError> {
    let mut contributions = application_catalog_contributions()?;
    let handlers = application_handler_descriptors()?;
    contributions.sort_by(|left, right| left.contribution_id().cmp(right.contribution_id()));
    validate_application_catalog(&contributions, &handlers)?;
    let profile = application_default_profile(&contributions)?;
    let mut builder = CatalogSnapshotBuilderV1::new();

    for contribution in contributions {
        builder.add_contribution(contribution);
    }
    for handler in handlers.catalog_descriptors()? {
        builder.add_handler(handler);
    }
    builder.add_profile(profile);

    Ok(builder.build()?)
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

fn application_default_profile(
    contributions: &[CatalogContributionV1],
) -> Result<ProfileDefinition, CatalogCompositionError> {
    let capabilities: Vec<_> = contributions
        .iter()
        .flat_map(tracedecay_tool_catalog::CatalogContributionV1::capabilities)
        .filter(|capability| capability.availability().is_callable())
        .collect();
    let capability_ids = capabilities
        .iter()
        .map(|capability| capability.capability_id().clone())
        .collect();
    let mut routing_fixtures = capabilities
        .iter()
        .map(|capability| {
            let query = capability
                .routing()
                .examples()
                .first()
                .cloned()
                .unwrap_or_else(|| capability.routing().name().to_owned());
            RoutingFixtureV1::new(
                query,
                RoutingFixtureExpectation::Select {
                    capability_id: capability.capability_id().clone(),
                },
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    routing_fixtures.extend([
        RoutingFixtureV1::new(
            "Preview and apply these index changes",
            RoutingFixtureExpectation::ambiguous(vec![
                CapabilityId::new("capability.application.git.preview")?,
                CapabilityId::new("capability.application.git.apply")?,
            ])?,
        )?,
        RoutingFixtureV1::new("Explain the weather", RoutingFixtureExpectation::Reject)?,
        RoutingFixtureV1::new(
            "Stage these selected hunks",
            RoutingFixtureExpectation::InsufficientCapability {
                capability_id: CapabilityId::new("capability.git.stage-hunks")?,
            },
        )?,
    ]);
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
            .any(|binding| binding.surface() == *surface)
    })
    .collect();
    Ok(ProfileDefinition::new(ProfileDefinitionInputV1 {
        profile_id: ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID)?,
        kind: ProfileKind::Default,
        capability_ids,
        enabled_surfaces,
        requires_cli_mcp_pairing: true,
        budget: ProfileBudget::new(160, 20_000_000, 18_000)?,
        routing_fixtures,
    })?)
}
