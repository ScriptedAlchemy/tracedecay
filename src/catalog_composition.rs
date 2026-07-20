//! Root-owned assembly of the inert application capability catalog.
//!
//! Composition validates metadata against the closed application handler
//! descriptors. It does not retain handlers or provide an invocation path.

use thiserror::Error;
use tracedecay_application::{
    APPLICATION_DEFAULT_PROFILE_ID, ApplicationContractError, application_catalog_contributions,
    application_handler_descriptors,
};
use tracedecay_tool_catalog::{
    CapabilityId, CatalogSnapshotBuilderV1, CatalogSnapshotV1, CatalogValidationError,
    IdentifierError, ProfileBudget, ProfileDefinition, ProfileDefinitionInputV1, ProfileId,
    ProfileKind, RoutingFixtureExpectation, RoutingFixtureV1,
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

/// Build the immutable catalog snapshot for currently implemented application
/// use cases.
pub fn build_application_catalog_snapshot() -> Result<CatalogSnapshotV1, CatalogCompositionError> {
    let contributions = application_catalog_contributions()?;
    let handlers = application_handler_descriptors()?;
    let mut builder = CatalogSnapshotBuilderV1::new();

    for contribution in contributions {
        builder.add_contribution(contribution);
    }
    for handler in handlers.catalog_descriptors()? {
        builder.add_handler(handler);
    }
    builder.add_profile(application_default_profile()?);

    Ok(builder.build()?)
}

fn application_default_profile() -> Result<ProfileDefinition, CatalogCompositionError> {
    let symbol_search = CapabilityId::new("capability.retrieval.symbol-search")?;
    Ok(ProfileDefinition::new(ProfileDefinitionInputV1 {
        profile_id: ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID)?,
        kind: ProfileKind::Default,
        capability_ids: vec![symbol_search.clone()],
        enabled_surfaces: Vec::new(),
        requires_cli_mcp_pairing: false,
        budget: ProfileBudget::DEFAULT,
        routing_fixtures: vec![
            RoutingFixtureV1::new(
                "Find this symbol",
                RoutingFixtureExpectation::Select {
                    capability_id: symbol_search,
                },
            )?,
            RoutingFixtureV1::new("Explain the weather", RoutingFixtureExpectation::Reject)?,
            RoutingFixtureV1::new(
                "Stage these selected hunks",
                RoutingFixtureExpectation::InsufficientCapability {
                    capability_id: CapabilityId::new("capability.git.stage-hunks")?,
                },
            )?,
        ],
    })?)
}
