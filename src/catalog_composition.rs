//! Root-owned assembly of the inert application capability catalog.
//!
//! Composition validates metadata against the closed application handler
//! descriptors. It does not retain handlers or provide an invocation path.

use std::collections::BTreeMap;

use thiserror::Error;
use tracedecay_application::{
    APPLICATION_DEFAULT_PROFILE_ID, ApplicationContractError, ApplicationHandlerDescriptors,
    application_catalog_contributions, application_handler_descriptors,
};
use tracedecay_tool_catalog::{
    AvailabilityContract, BindingId, CapabilityId, CatalogContributionV1, CatalogSnapshotBuilderV1,
    CatalogSnapshotV1, CatalogValidationError, IdentifierError, ProfileBudget, ProfileDefinition,
    ProfileDefinitionInputV1, ProfileId, ProfileKind, RoutingFixtureExpectation, RoutingFixtureV1,
    SchemaRef, UnavailabilityReason,
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

/// Schema references declared for one public surface binding.
///
/// The index stores only catalog `SchemaRef` values, never schema bodies,
/// handlers, authorization state, or executable routes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingSchemaReferences {
    request_schema: SchemaRef,
    result_schema: SchemaRef,
}

impl BindingSchemaReferences {
    fn new(request_schema: SchemaRef, result_schema: SchemaRef) -> Self {
        Self {
            request_schema,
            result_schema,
        }
    }

    pub fn request_schema(&self) -> &SchemaRef {
        &self.request_schema
    }

    pub fn result_schema(&self) -> &SchemaRef {
        &self.result_schema
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum SchemaReferenceIndexError {
    #[error("catalog contains more than one schema reference for a binding")]
    DuplicateBinding,
}

/// Immutable binding-to-schema-reference lookup derived from a catalog
/// snapshot. It is metadata only and intentionally cannot dispatch a binding.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CatalogSchemaReferenceIndex {
    by_binding: BTreeMap<BindingId, BindingSchemaReferences>,
}

impl CatalogSchemaReferenceIndex {
    fn insert(
        &mut self,
        binding_id: BindingId,
        schemas: BindingSchemaReferences,
    ) -> Result<(), SchemaReferenceIndexError> {
        if self.by_binding.contains_key(&binding_id) {
            return Err(SchemaReferenceIndexError::DuplicateBinding);
        }
        self.by_binding.insert(binding_id, schemas);
        Ok(())
    }

    pub fn get(&self, binding_id: &BindingId) -> Option<&BindingSchemaReferences> {
        self.by_binding.get(binding_id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&BindingId, &BindingSchemaReferences)> {
        self.by_binding.iter()
    }
}

/// Indexes schema references already declared by the immutable catalog.
///
/// This function performs no handler lookup and provides no invocation path.
pub fn catalog_schema_reference_index(
    snapshot: &CatalogSnapshotV1,
) -> Result<CatalogSchemaReferenceIndex, SchemaReferenceIndexError> {
    let mut index = CatalogSchemaReferenceIndex::default();
    for capability in snapshot.capabilities() {
        let schemas = BindingSchemaReferences::new(
            capability.request_schema().clone(),
            capability.result_schema().clone(),
        );
        for binding_id in capability.binding_ids() {
            index.insert(binding_id.clone(), schemas.clone())?;
        }
    }
    Ok(index)
}

/// Build the immutable catalog snapshot for currently declared application use
/// cases. The snapshot carries no runtime wiring.
pub fn build_application_catalog_snapshot() -> Result<CatalogSnapshotV1, CatalogCompositionError> {
    let mut contributions = application_catalog_contributions()?;
    let handlers = application_handler_descriptors()?;
    contributions.sort_by(|left, right| left.contribution_id().cmp(right.contribution_id()));
    validate_application_catalog(&contributions, &handlers)?;
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

/// Validates the application-owned catalog before application-only handler
/// identity is lowered to the generic tool-catalog descriptor.
///
/// This packet has no runtime factories or transport bindings, so every
/// application capability must remain explicitly unavailable as not
/// implemented. A future packet may relax that gate only when it also owns the
/// corresponding runtime wiring.
pub fn validate_application_catalog(
    contributions: &[CatalogContributionV1],
    handlers: &ApplicationHandlerDescriptors,
) -> Result<(), CatalogCompositionError> {
    for capability in contributions
        .iter()
        .flat_map(|contribution| contribution.capabilities())
    {
        if !matches!(
            capability.availability(),
            AvailabilityContract::Unavailable {
                reason: UnavailabilityReason::NotImplemented,
            }
        ) {
            return Err(CatalogCompositionError::Application(
                ApplicationContractError::Inconsistent {
                    field: "unwired application capability availability",
                },
            ));
        }
    }
    handlers.validate_against(contributions)?;
    Ok(())
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
