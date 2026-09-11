//! Assembly of the application capability catalog from its own descriptors.
//!
//! Composition validates metadata against the closed application handler
//! descriptors and binds them to one caller-supplied canonical dispatcher. It
//! lives beside `application_catalog_contributions` and
//! `application_handler_descriptors` so the descriptor-derived catalog has one
//! owner every transport can reach.

use crate::handlers::BoundApplicationHandler;
use crate::{
    APPLICATION_ADMINISTRATIVE_PROFILE_ID, APPLICATION_COMPACT_PROFILE_ID,
    APPLICATION_DEFAULT_PROFILE_ID, APPLICATION_HOST_LIMITED_PROFILE_ID, ApplicationContractError,
    ApplicationHandlerDescriptors, application_catalog_contributions,
    application_handler_descriptors,
};
use thiserror::Error;
use tracedecay_tool_catalog::{
    BindingSurface, CatalogContributionV1, CatalogSnapshotBuilderV1, CatalogSnapshotV1,
    CatalogValidationError, IdentifierError, ProfileBudget, ProfileDefinition,
    ProfileDefinitionInputV1, ProfileId, ProfileKind, UseCaseId,
};

// The default profile currently composes 403 shipped bindings. This reviewed
// ceiling leaves 45 bindings of admission headroom while the eager-profile
// routing and serialized discovery assertions in the root
// `product_surface_suite/catalog_composition_contract.rs` suite bound the
// client-facing cost.
const DEFAULT_PROFILE_MAXIMUM_BINDINGS: u32 = 448;

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

#[hotpath::measure(label = "catalog_composition.assemble")]
fn assemble_application_catalog()
-> Result<(CatalogSnapshotV1, ApplicationHandlerDescriptors), CatalogCompositionError> {
    let (mut contributions, handlers) =
        hotpath::measure_block!("catalog_composition.contributions", {
            (
                application_catalog_contributions()?,
                application_handler_descriptors()?,
            )
        });
    contributions.sort_by(|left, right| left.contribution_id().cmp(right.contribution_id()));
    hotpath::measure_block!(
        "catalog_composition.validate",
        validate_application_catalog(&contributions, &handlers)?
    );
    let profiles = hotpath::measure_block!(
        "catalog_composition.profiles",
        application_profiles(&contributions)?
    );
    let snapshot = hotpath::measure_block!("catalog_composition.snapshot", {
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
        builder.build()?
    });
    Ok((snapshot, handlers))
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
    [
        (
            APPLICATION_DEFAULT_PROFILE_ID,
            ProfileKind::Default,
            ProfileBudget::new(DEFAULT_PROFILE_MAXIMUM_BINDINGS, 18_000)?,
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
    use std::collections::BTreeSet;

    use super::*;
    use crate::handlers::CanonicalApplicationDispatcher;
    use crate::{ApplicationOperation, ApplicationProblem, RetryDirective, SafeDiagnostic};
    use tracedecay_tool_catalog::{
        BindingStatus, CapabilityId, SurfaceBindingV1, SurfaceOperationName,
    };

    fn current_bindings_on(surface: BindingSurface) -> Vec<SurfaceBindingV1> {
        application_catalog_contributions()
            .expect("application contributions")
            .into_iter()
            .flat_map(|contribution| contribution.bindings().to_vec())
            .filter(|binding| {
                binding.surface() == surface
                    && matches!(binding.status(), BindingStatus::Current)
                    && !binding.is_alias()
            })
            .collect()
    }

    fn dashboard_operations() -> Vec<String> {
        current_bindings_on(BindingSurface::Dashboard)
            .iter()
            .map(|binding| binding.operation().as_str().to_owned())
            .collect()
    }

    /// Dashboard operations whose capability HTTP also serves. A dashboard-only
    /// read such as `native_integration_status` has no HTTP handler to agree
    /// with, so it cannot take part in the pre-render parity check.
    fn dashboard_operations_shared_with_http() -> Vec<String> {
        let http_capabilities = current_bindings_on(BindingSurface::Http)
            .into_iter()
            .map(|binding| binding.capability_id().clone())
            .collect::<BTreeSet<_>>();
        current_bindings_on(BindingSurface::Dashboard)
            .iter()
            .filter(|binding| http_capabilities.contains(binding.capability_id()))
            .map(|binding| binding.operation().as_str().to_owned())
            .collect()
    }

    #[derive(Clone, Copy)]
    enum ParityOutcome {
        Ready,
        Unavailable,
        Denied,
    }

    #[derive(Clone)]
    struct ParityRequest {
        outcome: ParityOutcome,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct ParityResult {
        capability_id: CapabilityId,
        use_case_id: UseCaseId,
        outcome: Result<&'static str, ApplicationProblem>,
    }

    struct ParityDispatcher;

    impl CanonicalApplicationDispatcher<ParityRequest> for ParityDispatcher {
        type Output = ParityResult;

        fn invoke(&self, operation: &ApplicationOperation, request: ParityRequest) -> Self::Output {
            let outcome = match request.outcome {
                ParityOutcome::Ready => Ok("canonical-result"),
                ParityOutcome::Unavailable => {
                    Err(ApplicationProblem::unavailable(SafeDiagnostic {
                        code: "application.fixture.unavailable".to_owned(),
                        message: "The canonical owner is unavailable".to_owned(),
                    }))
                }
                ParityOutcome::Denied => Err(ApplicationProblem::not_found_or_not_authorized(
                    RetryDirective::Never,
                )),
            };
            ParityResult {
                capability_id: operation.capability_id().clone(),
                use_case_id: operation.use_case_id().clone(),
                outcome,
            }
        }
    }

    fn invoke_pre_render(
        composition: &ApplicationCatalogComposition<ParityDispatcher>,
        surface: BindingSurface,
        operation: &str,
        outcome: ParityOutcome,
    ) -> ParityResult {
        let profile = ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).expect("profile");
        let operation = SurfaceOperationName::new(operation).expect("surface operation");
        let capability = composition
            .snapshot()
            .resolve_binding(&profile, surface, &operation, 1, &BTreeSet::new())
            .unwrap_or_else(|| panic!("{operation} must resolve on {surface:?}"));
        composition
            .handler(capability.use_case_id())
            .expect("resolved capability has its canonical application handler")
            .invoke(ParityRequest { outcome })
    }

    #[test]
    fn dashboard_requests_invoke_the_same_pre_render_handlers_as_http() {
        let composition =
            compose_application_catalog(ParityDispatcher).expect("application composition");
        let operations = dashboard_operations_shared_with_http();
        assert!(
            operations.contains(&"diagnostics_read".to_owned()),
            "the shared dashboard/HTTP set must include the diagnostics read: {operations:?}"
        );

        for operation in operations {
            for outcome in [
                ParityOutcome::Ready,
                ParityOutcome::Unavailable,
                ParityOutcome::Denied,
            ] {
                let http =
                    invoke_pre_render(&composition, BindingSurface::Http, &operation, outcome);
                let dashboard =
                    invoke_pre_render(&composition, BindingSurface::Dashboard, &operation, outcome);
                assert_eq!(dashboard, http, "{operation} changed before rendering");
            }
        }
    }

    #[test]
    fn dashboard_does_not_advertise_an_uncallable_metadata_only_binding() {
        let composition =
            compose_application_catalog(ParityDispatcher).expect("application composition");
        let profile = ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).expect("profile");
        let operation = SurfaceOperationName::new("git_apply").expect("surface operation");

        assert!(
            composition
                .snapshot()
                .resolve_binding(
                    &profile,
                    BindingSurface::Dashboard,
                    &operation,
                    1,
                    &BTreeSet::new(),
                )
                .is_none()
        );
    }

    #[test]
    fn reviewed_default_budget_admits_the_full_eager_profile() {
        let snapshot = build_application_catalog_snapshot().expect("application catalog");
        let profile_id = ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).expect("profile");
        let profile = snapshot.profile(&profile_id).expect("default profile");
        let eager_visible_capabilities =
            snapshot.visible_capabilities(&profile_id, &BTreeSet::new());
        let eager_binding_count = eager_visible_capabilities
            .iter()
            .flat_map(|capability| capability.binding_ids())
            .filter_map(|binding_id| snapshot.binding(binding_id))
            .filter(|binding| profile.enables_surface(binding.surface()))
            .count();

        assert_eq!(
            profile.budget().maximum_bindings(),
            DEFAULT_PROFILE_MAXIMUM_BINDINGS
        );
        let expected_binding_count = application_catalog_contributions()
            .expect("application contributions")
            .iter()
            .flat_map(CatalogContributionV1::bindings)
            .filter(|binding| {
                profile.includes_capability(binding.capability_id())
                    && profile.enables_surface(binding.surface())
            })
            .count();
        assert_eq!(eager_binding_count, expected_binding_count);
        assert!(
            eager_binding_count <= profile.budget().maximum_bindings() as usize,
            "the derived eager profile must stay within its reviewed budget"
        );

        for operation in dashboard_operations() {
            let operation_name =
                SurfaceOperationName::new(&operation).expect("surface operation name");
            let capability = snapshot
                .resolve_binding(
                    &profile_id,
                    BindingSurface::Dashboard,
                    &operation_name,
                    1,
                    &BTreeSet::new(),
                )
                .unwrap_or_else(|| panic!("{operation} must resolve from the eager profile"));
            assert!(
                eager_visible_capabilities
                    .iter()
                    .any(|candidate| candidate.capability_id() == capability.capability_id()),
                "{operation} must resolve to an eager-visible capability"
            );
        }
    }
}
