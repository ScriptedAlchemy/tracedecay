//! Root-owned assembly of the application capability catalog.
//!
//! Composition validates metadata against the closed application handler
//! descriptors and binds them to one caller-supplied canonical dispatcher.

use std::collections::BTreeSet;

use thiserror::Error;
use tracedecay_application::handlers::BoundApplicationHandler;
use tracedecay_application::{
    APPLICATION_ADMINISTRATIVE_PROFILE_ID, APPLICATION_COMPACT_PROFILE_ID,
    APPLICATION_DEFAULT_PROFILE_ID, APPLICATION_HOST_LIMITED_PROFILE_ID, ApplicationContractError,
    ApplicationHandlerDescriptors, application_catalog_contributions,
    application_handler_descriptors,
};
use tracedecay_tool_catalog::{
    BindingSurface, CapabilityId, CatalogContributionV1, CatalogSnapshotBuilderV1,
    CatalogSnapshotV1, CatalogValidationError, IdentifierError, ProfileBudget, ProfileDefinition,
    ProfileDefinitionInputV1, ProfileId, ProfileKind, RoutingFixtureExpectation, RoutingFixtureV1,
    UseCaseId,
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
            ProfileBudget::new(320, 18_000)?,
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
        if capability_ids.len() > 1 {
            let (query, ambiguous_capability_ids) =
                if capability_ids.contains(&git_preview) && capability_ids.contains(&git_apply) {
                    (
                        "Preview and apply these index changes".to_owned(),
                        vec![git_preview, git_apply],
                    )
                } else {
                    let first = capabilities[0];
                    let second = capabilities[1];
                    (
                        format!("{} or {}", first.routing().name(), second.routing().name()),
                        vec![
                            first.capability_id().clone(),
                            second.capability_id().clone(),
                        ],
                    )
                };
            routing_fixtures.push(RoutingFixtureV1::new(
                query,
                RoutingFixtureExpectation::ambiguous(ambiguous_capability_ids)?,
            )?);
        }
        routing_fixtures.push(RoutingFixtureV1::new(
            "Explain the weather",
            RoutingFixtureExpectation::Reject,
        )?);
        let stage_hunks = CapabilityId::new("capability.git.stage-hunks")?;
        let insufficient_capability = contributions
            .iter()
            .flat_map(tracedecay_tool_catalog::CatalogContributionV1::capabilities)
            .find(|capability| {
                capability.capability_id() == &stage_hunks
                    && !capability_ids.contains(capability.capability_id())
            })
            .or_else(|| {
                contributions
                    .iter()
                    .flat_map(tracedecay_tool_catalog::CatalogContributionV1::capabilities)
                    .find(|capability| !capability_ids.contains(capability.capability_id()))
            });
        if let Some(capability) = insufficient_capability {
            let query = capability
                .routing()
                .examples()
                .first()
                .cloned()
                .unwrap_or_else(|| capability.routing().name().to_owned());
            routing_fixtures.push(RoutingFixtureV1::new(
                query,
                RoutingFixtureExpectation::InsufficientCapability {
                    capability_id: capability.capability_id().clone(),
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
    use crate::mcp::tools::{
        ToolRegistryMode, default_catalog_discovery_authority,
        get_catalog_filtered_tool_definitions_with_budget, project_catalog_discovery_scope,
    };
    use tracedecay_application::handlers::CanonicalApplicationDispatcher;
    use tracedecay_application::{
        ApplicationOperation, ApplicationProblem, RetryDirective, SafeDiagnostic,
    };
    use tracedecay_tool_catalog::SurfaceOperationName;

    // Growth tripwire only, not an MCP client or protocol limit. The original
    // full-registry measurement was 231,640 bytes; the exact deterministic
    // default-profile baseline is 222,664 bytes after catalog filtering.
    // Raising this 512 KiB ceiling requires a fresh serialized tools/list
    // measurement and a stated reason for the additional payload.
    const DEFAULT_PROFILE_TOOLS_LIST_REGRESSION_CEILING_BYTES: usize = 524_288;

    const DASHBOARD_OPERATIONS: [&str; 23] = [
        "feedback_diagnostics",
        "feedback_get",
        "feedback_expand",
        "feedback_list",
        "feedback_impact",
        "affected_tests",
        "test_results",
        "health_read",
        "storage_status",
        "diagnostics_read",
        "configuration_list",
        "configuration_explain",
        "configuration_get",
        "configuration_set",
        "configuration_unset",
        "configuration_batch",
        "configuration_write_credential",
        "configuration_observed_state",
        "configuration_protected_preview",
        "configuration_protected_apply",
        "configuration_rollback_preview",
        "configuration_rollback_apply",
        "configuration_audit",
    ];

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

        for operation in DASHBOARD_OPERATIONS {
            for outcome in [
                ParityOutcome::Ready,
                ParityOutcome::Unavailable,
                ParityOutcome::Denied,
            ] {
                let http =
                    invoke_pre_render(&composition, BindingSurface::Http, operation, outcome);
                let dashboard =
                    invoke_pre_render(&composition, BindingSurface::Dashboard, operation, outcome);
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
    fn raised_default_budget_routes_dashboard_operations_for_an_eager_client() {
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

        assert_eq!(profile.budget().maximum_bindings(), 320);
        assert!(
            eager_binding_count > 288
                && eager_binding_count <= profile.budget().maximum_bindings() as usize,
            "acceptance must exercise the raised budget with the full eager profile loaded"
        );

        for operation in DASHBOARD_OPERATIONS {
            let operation_name =
                SurfaceOperationName::new(operation).expect("surface operation name");
            let capability = snapshot
                .resolve_binding(
                    &profile_id,
                    BindingSurface::Dashboard,
                    &operation_name,
                    1,
                    &BTreeSet::new(),
                )
                .unwrap_or_else(|| panic!("{operation} must resolve from the eager profile"));
            let fixture = profile
                .routing_fixtures()
                .iter()
                .find(|fixture| {
                    matches!(
                        fixture.expectation(),
                        RoutingFixtureExpectation::Select { capability_id }
                            if capability_id == capability.capability_id()
                    )
                })
                .unwrap_or_else(|| panic!("{operation} must retain a routing fixture"));
            let routed = eager_visible_capabilities
                .iter()
                .filter(|candidate| {
                    candidate.routing().name() == fixture.utterance()
                        || candidate
                            .routing()
                            .examples()
                            .iter()
                            .any(|example| example == fixture.utterance())
                        || format!(
                            "{} [{}]",
                            candidate.routing().name(),
                            candidate.capability_id().as_str()
                        ) == fixture.utterance()
                })
                .map(|candidate| candidate.capability_id())
                .collect::<Vec<_>>();
            assert_eq!(
                routed,
                vec![capability.capability_id()],
                "{operation} must route unambiguously with every eager capability loaded"
            );
        }
    }

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
        assert_eq!(default_profile.budget().maximum_bindings(), 320);
        assert!(default_binding_count > 0);
        assert!(default_binding_count <= default_profile.budget().maximum_bindings() as usize);

        let definitions = get_catalog_filtered_tool_definitions_with_budget(
            0,
            crate::mcp::tools::explore_call_budget(0),
            &default_profile_id,
            &default_catalog_discovery_authority().expect("default discovery authority"),
            &project_catalog_discovery_scope(),
            ToolRegistryMode::DeterministicMaximal,
        )
        .expect("default-profile MCP definitions");
        let measured_bytes = serde_json::to_vec(&serde_json::json!({ "tools": &definitions }))
            .expect("serialize default-profile tools/list response")
            .len();
        let mut contributors = definitions
            .iter()
            .map(|definition| {
                (
                    definition.name.as_str(),
                    serde_json::to_vec(definition)
                        .expect("serialize tool definition")
                        .len(),
                )
            })
            .collect::<Vec<_>>();
        contributors.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(right.0)));
        let largest_contributors = contributors
            .iter()
            .take(10)
            .map(|(name, bytes)| format!("{name}={bytes}"))
            .collect::<Vec<_>>()
            .join(", ");

        assert!(
            measured_bytes <= DEFAULT_PROFILE_TOOLS_LIST_REGRESSION_CEILING_BYTES,
            "default-profile MCP tools/list payload measured {measured_bytes} bytes, exceeding \
             the {DEFAULT_PROFILE_TOOLS_LIST_REGRESSION_CEILING_BYTES}-byte regression ceiling; \
             largest serialized tool definitions: {largest_contributors}"
        );
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
