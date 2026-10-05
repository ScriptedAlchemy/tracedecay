//! Assembly of the application capability catalog from its own descriptors.
//!
//! Composition validates metadata against the closed application handler
//! descriptors. It lives beside `application_catalog_contributions` and
//! `application_handler_descriptors` so the descriptor-derived catalog has one
//! owner every transport can reach.

use std::collections::BTreeSet;
use std::sync::{Arc, LazyLock};

use crate::retrieval::catalog::application_catalog_contributions_with;
use crate::schema_bodies::SchemaBodyMaterialization;
use crate::{
    APPLICATION_ADMINISTRATIVE_PROFILE_ID, APPLICATION_COMPACT_PROFILE_ID,
    APPLICATION_DEFAULT_PROFILE_ID, APPLICATION_HOST_LIMITED_PROFILE_ID, ApplicationContractError,
    ApplicationHandlerDescriptors, application_handler_descriptors,
};
use thiserror::Error;
use tracedecay_tool_catalog::{
    BindingSurface, CatalogContributionV1, CatalogSnapshotBuilderV1, CatalogSnapshotV1,
    CatalogValidationError, IdentifierError, ProfileBudget, ProfileDefinition,
    ProfileDefinitionInputV1, ProfileId, ProfileKind,
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

/// The process's one catalog snapshot with schema bodies.
///
/// Every descriptor behind it is `const`, so it cannot change while the
/// process runs. The snapshot is validated against those handler descriptors
/// once, then shared by every transport adapter without duplicating schema bodies.
pub fn application_catalog_snapshot()
-> Result<&'static Arc<CatalogSnapshotV1>, CatalogCompositionError> {
    static SNAPSHOT: LazyLock<Result<Arc<CatalogSnapshotV1>, CatalogCompositionError>> =
        LazyLock::new(|| build_application_catalog_snapshot().map(Arc::new));
    SNAPSHOT.as_ref().map_err(Clone::clone)
}

/// Build the immutable catalog snapshot used by transport binding resolution.
pub fn build_application_catalog_snapshot() -> Result<CatalogSnapshotV1, CatalogCompositionError> {
    assemble_application_catalog_with(SchemaBodyMaterialization::Materialize)
}

/// Binding snapshot for dispatch. Schema references stay; JSON Schema bodies do not.
///
/// A CLI process resolves one operation from this snapshot. Generating every
/// executable schema body is discovery and SDK work, not a dispatch prerequisite.
pub fn build_application_binding_snapshot() -> Result<CatalogSnapshotV1, CatalogCompositionError> {
    assemble_application_catalog_with(SchemaBodyMaterialization::Omit)
}

#[tracing::instrument(name = "catalog_composition.assemble", level = "trace", skip_all)]
fn assemble_application_catalog_with(
    materialize: SchemaBodyMaterialization,
) -> Result<CatalogSnapshotV1, CatalogCompositionError> {
    let (mut contributions, handlers) = {
        let _span = tracing::trace_span!("catalog_composition.contributions").entered();
        {
            (
                application_catalog_contributions_with(materialize)?,
                application_handler_descriptors()?,
            )
        }
    };
    contributions.sort_by(|left, right| left.contribution_id().cmp(right.contribution_id()));
    {
        let _span = tracing::trace_span!("catalog_composition.validate").entered();
        validate_application_catalog(&contributions, &handlers)?
    };
    let profiles = {
        let _span = tracing::trace_span!("catalog_composition.profiles").entered();
        application_profiles(&contributions)?
    };
    let snapshot = {
        let _span = tracing::trace_span!("catalog_composition.snapshot").entered();
        {
            let mut builder = CatalogSnapshotBuilderV1::new();
            for contribution in contributions {
                builder.add_contribution(contribution);
            }
            for handler in handlers.catalog_descriptors() {
                builder.add_handler(handler);
            }
            for profile in profiles {
                builder.add_profile(profile);
            }
            builder.build()?
        }
    };
    Ok(snapshot)
}

/// Validates the application-owned catalog before application-only handler
/// identity is lowered to the generic tool-catalog descriptor.
///
/// Contribution builders derive availability and bindings from their concrete
/// runtime registrars. This crate validates only the resulting use-case/schema
/// mapping; it does not maintain a second availability list.
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
    let default_maximum_bindings = u32::try_from(profile_binding_count(
        contributions,
        APPLICATION_DEFAULT_PROFILE_ID,
    ))
    .map_err(|_| CatalogValidationError::InvalidValue {
        field: "default profile binding budget",
        reason: "composed binding count exceeds u32",
    })?;
    [
        (
            APPLICATION_DEFAULT_PROFILE_ID,
            ProfileKind::Default,
            ProfileBudget::new(default_maximum_bindings, 18_000)?,
            true,
        ),
        (
            APPLICATION_COMPACT_PROFILE_ID,
            ProfileKind::Compact,
            ProfileBudget::new(22, 4_000)?,
            false,
        ),
        (
            APPLICATION_ADMINISTRATIVE_PROFILE_ID,
            ProfileKind::Administrative,
            ProfileBudget::new(48, 8_000)?,
            false,
        ),
        (
            APPLICATION_HOST_LIMITED_PROFILE_ID,
            ProfileKind::HostLimited,
            ProfileBudget::new(17, 2_000)?,
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

fn profile_binding_count(contributions: &[CatalogContributionV1], profile_id: &str) -> usize {
    let capability_ids = contributions
        .iter()
        .flat_map(CatalogContributionV1::capabilities)
        .filter(|capability| {
            capability.availability().is_callable()
                && capability
                    .profile_eligibility()
                    .iter()
                    .any(|eligible| eligible.as_str() == profile_id)
        })
        .map(|capability| capability.capability_id())
        .collect::<BTreeSet<_>>();
    contributions
        .iter()
        .flat_map(CatalogContributionV1::bindings)
        .filter(|binding| capability_ids.contains(binding.capability_id()))
        .count()
}

fn application_profile(
    contributions: &[CatalogContributionV1],
    profile_id: &str,
    kind: ProfileKind,
    budget: ProfileBudget,
    requires_cli_mcp_pairing: bool,
) -> Result<ProfileDefinition, CatalogCompositionError> {
    let profile_id = ProfileId::new(profile_id)?;
    let capability_ids = contributions
        .iter()
        .flat_map(CatalogContributionV1::capabilities)
        .filter(|capability| {
            capability.availability().is_callable()
                && capability.profile_eligibility().contains(&profile_id)
        })
        .map(|capability| capability.capability_id().clone())
        .collect::<Vec<_>>();
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
            .flat_map(CatalogContributionV1::bindings)
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
    })?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_tool_catalog::SurfaceOperationName;

    #[test]
    fn dashboard_does_not_advertise_an_uncallable_metadata_only_binding() {
        let snapshot = application_catalog_snapshot().expect("application catalog snapshot");
        let profile = ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).expect("profile");
        let dashboard_use_case = |operation: &str| {
            snapshot
                .resolve_binding(
                    &profile,
                    BindingSurface::Dashboard,
                    &SurfaceOperationName::new(operation).expect("surface operation"),
                    1,
                    &BTreeSet::new(),
                )
                .map(|capability| capability.use_case_id().to_string())
        };

        assert_eq!(dashboard_use_case("git_apply"), None);
        assert_eq!(
            dashboard_use_case("feedback_get").as_deref(),
            Some("use-case.application.feedback.get")
        );
    }
}
