use std::collections::BTreeSet;

use tracedecay_api::{
    http_application_full_route_path, http_route_documents, is_http_application_operation_exposed,
    retained_application_route_path,
};
use tracedecay_contracts::catalog_composition::{
    CatalogCompositionError, build_application_catalog_snapshot, validate_application_catalog,
};
use tracedecay_contracts::{
    APPLICATION_DEFAULT_PROFILE_ID, ApplicationContractError, ApplicationHandlerDescriptor,
    ApplicationHandlerDescriptors, ApplicationOperation, ResultContractRef,
    RetainedSurfaceOperation, application_catalog_contributions, application_handler_descriptors,
    retrieval::catalog::symbol_search_contribution,
};
use tracedecay_mcp::tools::catalog_discovery::{
    default_catalog_discovery_authority, get_catalog_filtered_tool_definitions_with_budget,
};
use tracedecay_mcp::{ToolRegistryMode, explore_call_budget, project_catalog_discovery_scope};
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, BindingSurface, CapabilityId, CatalogContributionV1,
    ProfileBudget, ProfileId, ProfileKind, SchemaId, SchemaRef, ScopeDimension, UseCaseId,
};

#[test]
fn root_snapshot_validates_every_application_contribution_against_declared_descriptors() {
    let contributions = application_catalog_contributions().unwrap();
    let handlers = application_handler_descriptors().unwrap();
    let snapshot = build_application_catalog_snapshot().unwrap();

    let contributed_capabilities = contributions
        .iter()
        .flat_map(|contribution| contribution.capabilities())
        .count();
    assert_eq!(contributed_capabilities, handlers.iter().count());
    assert_eq!(snapshot.capabilities().count(), contributed_capabilities);

    for contribution in &contributions {
        for capability in contribution.capabilities() {
            let handler = handlers
                .get(capability.use_case_id())
                .expect("every declared capability has one callable handler descriptor");
            assert_eq!(
                handler.operation().capability_id(),
                capability.capability_id()
            );
            assert_eq!(handler.operation().use_case_id(), capability.use_case_id());
            assert_eq!(handler.request_schema(), capability.request_schema());
            assert_eq!(handler.result_schema(), capability.result_schema());
            assert_eq!(
                capability.availability().is_callable() && !capability.binding_ids().is_empty(),
                !capability.profile_eligibility().is_empty(),
                "{} transport bindings and profile eligibility disagree",
                capability.capability_id()
            );
        }
    }

    let symbol_search = CapabilityId::new("capability.application.symbol-search").unwrap();
    assert!(snapshot.capability(&symbol_search).is_some());

    let default_profile = ProfileId::new("profile.default").unwrap();
    assert!(snapshot.profile(&default_profile).is_some());
    let visible_default = snapshot.visible_capabilities(&default_profile, &BTreeSet::new());
    for capability_id in [
        "capability.application.symbol-search",
        "capability.application.primitive.todos",
        "capability.application.retained.fact-store-search",
    ] {
        assert!(
            visible_default
                .iter()
                .any(|capability| capability.capability_id().as_str() == capability_id),
            "the composed default profile must expose {capability_id}"
        );
    }
}

#[test]
fn root_snapshot_composes_every_explicit_profile_without_widening_eligibility() {
    let snapshot = build_application_catalog_snapshot().unwrap();
    // The default budget is not a reviewed constant: it is the exact number of
    // bindings the composed manifests declare for default-eligible callable
    // capabilities, so a binding added anywhere widens the budget with it.
    let default_profile_id = ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).unwrap();
    let composed_default_bindings = snapshot
        .capabilities()
        .filter(|capability| {
            capability.availability().is_callable()
                && capability
                    .profile_eligibility()
                    .contains(&default_profile_id)
        })
        .map(|capability| capability.binding_ids().len())
        .sum::<usize>();
    let expected_profiles = [
        (
            "profile.default",
            ProfileKind::Default,
            ProfileBudget::new(u32::try_from(composed_default_bindings).unwrap(), 18_000).unwrap(),
        ),
        (
            "profile.compact",
            ProfileKind::Compact,
            ProfileBudget::new(22, 4_000).unwrap(),
        ),
        (
            "profile.administrative",
            ProfileKind::Administrative,
            ProfileBudget::new(48, 8_000).unwrap(),
        ),
        (
            "profile.host-limited",
            ProfileKind::HostLimited,
            ProfileBudget::new(17, 2_000).unwrap(),
        ),
    ];

    assert_eq!(snapshot.profiles().count(), expected_profiles.len());
    for (profile_id, kind, budget) in expected_profiles {
        let profile_id = ProfileId::new(profile_id).unwrap();
        let profile = snapshot
            .profile(&profile_id)
            .expect("every explicit application profile is composed");
        let eligible_capability_ids = snapshot
            .capabilities()
            .filter(|capability| {
                capability.availability().is_callable()
                    && capability.profile_eligibility().contains(&profile_id)
            })
            .map(|capability| capability.capability_id().clone())
            .collect::<Vec<_>>();

        assert_eq!(profile.kind(), kind);
        assert_eq!(profile.budget(), budget);
        assert_eq!(profile.capability_ids(), eligible_capability_ids);
        let enabled_features = snapshot
            .capabilities()
            .flat_map(|capability| capability.required_features().iter().cloned())
            .collect();
        assert_eq!(
            snapshot
                .visible_capabilities(&profile_id, &enabled_features)
                .into_iter()
                .map(|capability| capability.capability_id().clone())
                .collect::<Vec<_>>(),
            eligible_capability_ids,
        );
    }
}

#[test]
fn binding_discovery_intersects_profile_surface_authority_and_scope() {
    let snapshot = build_application_catalog_snapshot().unwrap();
    let profile = ProfileId::new("profile.compact").unwrap();
    let symbol_search = CapabilityId::new("capability.application.symbol-search").unwrap();
    let authorized = BTreeSet::from([symbol_search.clone()]);
    let scope = BTreeSet::from([
        ScopeDimension::ConfigurationLayer,
        ScopeDimension::Project,
        ScopeDimension::Repository,
        ScopeDimension::Worktree,
        ScopeDimension::Resource,
    ]);

    let visible = snapshot.visible_bindings(
        &profile,
        BindingSurface::Mcp,
        1,
        &BTreeSet::new(),
        &authorized,
        &scope,
    );
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].0.operation().as_str(), "code_symbol_search");
    assert_eq!(visible[0].1.capability_id(), &symbol_search);

    assert!(
        snapshot
            .visible_bindings(
                &profile,
                BindingSurface::Mcp,
                1,
                &BTreeSet::new(),
                &BTreeSet::new(),
                &scope,
            )
            .is_empty()
    );
    assert!(
        snapshot
            .visible_bindings(
                &profile,
                BindingSurface::Mcp,
                1,
                &BTreeSet::new(),
                &authorized,
                &BTreeSet::from([
                    ScopeDimension::Project,
                    ScopeDimension::Repository,
                    ScopeDimension::Worktree,
                ]),
            )
            .is_empty()
    );
}

#[test]
fn http_route_documents_follow_the_catalog_and_exclude_git_mutation_facades() {
    let snapshot = build_application_catalog_snapshot().unwrap();
    let profile = ProfileId::new("profile.default").unwrap();
    let authorized = snapshot
        .capabilities()
        .map(|capability| capability.capability_id().clone())
        .collect();
    let scope = BTreeSet::from([
        ScopeDimension::Project,
        ScopeDimension::Repository,
        ScopeDimension::Worktree,
        ScopeDimension::Branch,
        ScopeDimension::Session,
        ScopeDimension::Resource,
    ]);
    let documents = http_route_documents(
        &snapshot,
        &profile,
        &authorized,
        &scope,
        &BTreeSet::new(),
        1,
    );
    let visible_http_bindings = snapshot.visible_bindings(
        &profile,
        BindingSurface::Http,
        1,
        &BTreeSet::new(),
        &authorized,
        &scope,
    );
    let unrouted = visible_http_bindings
        .iter()
        .filter(|(binding, _)| {
            match ApplicationSurfaceOperation::from_catalog_name(binding.operation().as_str()) {
                Some(operation) => !is_http_application_operation_exposed(operation)
                    .expect("HTTP exposure registry"),
                None => RetainedSurfaceOperation::from_operation_name(binding.operation().as_str())
                    .is_none(),
            }
        })
        .map(|(binding, _)| binding.operation().as_str())
        .collect::<Vec<_>>();

    assert!(!documents.is_empty());
    assert!(
        unrouted.is_empty(),
        "every visible HTTP catalog binding must have a public route: {unrouted:?}"
    );
    assert_eq!(documents.len(), visible_http_bindings.len());
    assert!(documents.iter().all(|document| {
        ApplicationSurfaceOperation::from_catalog_name(&document.operation)
            .is_some_and(|operation| http_application_full_route_path(operation) == document.path)
            || RetainedSurfaceOperation::from_operation_name(&document.operation).is_some_and(
                |operation| retained_application_route_path(operation) == document.path,
            )
    }));
    assert!(documents.iter().all(|document| {
        !matches!(document.operation.as_str(), "git_preview" | "git_apply")
            && !matches!(
                document.path.as_str(),
                "/application/git/preview" | "/application/git/apply"
            )
    }));
    assert!(
        documents
            .iter()
            .any(|document| document.operation == "code_symbol_search"
                && document.path == "/application/code/code_symbol_search")
    );
}

#[test]
fn registered_capability_does_not_require_a_catalog_surface_binding() {
    assert_eq!(
        validate_application_catalog(
            &[symbol_search_contribution().unwrap()],
            &ApplicationHandlerDescriptors::new([descriptor_with_contract(
                "capability.application.symbol-search",
                "use-case.application.symbol-search",
                symbol_request_schema(),
                symbol_result_schema(),
            )])
            .unwrap(),
        ),
        Ok(())
    );
}

#[test]
fn mismatched_descriptor_schema_is_rejected() {
    let contribution = symbol_search_contribution().unwrap();
    let cases = [
        (
            descriptor_with_contract(
                "capability.application.symbol-search",
                "use-case.application.symbol-search",
                schema("schema.test.drifted-request"),
                symbol_result_schema(),
            ),
            "application capability schema mapping",
        ),
        (
            descriptor_with_contract(
                "capability.application.symbol-search",
                "use-case.application.symbol-search",
                symbol_request_schema(),
                schema("schema.test.drifted-result"),
            ),
            "application capability schema mapping",
        ),
    ];

    for (descriptor, field) in cases {
        let handlers = ApplicationHandlerDescriptors::new([descriptor]).unwrap();
        assert_eq!(
            validate_application_catalog(std::slice::from_ref(&contribution), &handlers),
            inconsistent(field),
            "descriptor mismatch for {field} must be rejected"
        );
    }
}

#[test]
fn mismatched_descriptor_capability_is_rejected() {
    let contribution = symbol_search_contribution().unwrap();
    let handlers = ApplicationHandlerDescriptors::new([descriptor_with_contract(
        "capability.retrieval.wrong-symbol-search",
        "use-case.application.symbol-search",
        symbol_request_schema(),
        symbol_result_schema(),
    )])
    .unwrap();

    assert_eq!(
        validate_application_catalog(std::slice::from_ref(&contribution), &handlers),
        inconsistent("application capability/use-case mapping")
    );
}

#[test]
fn capability_without_descriptor_is_rejected() {
    assert_eq!(
        validate_application_catalog(
            &[symbol_search_contribution().unwrap()],
            &ApplicationHandlerDescriptors::default(),
        ),
        inconsistent("application capability handler mapping")
    );
}

#[test]
fn orphan_handler_descriptor_is_rejected() {
    let mut descriptors: Vec<_> = application_handler_descriptors()
        .unwrap()
        .iter()
        .cloned()
        .collect();
    descriptors.push(descriptor_with_contract(
        "capability.application.orphan",
        "use-case.application.orphan",
        symbol_request_schema(),
        symbol_result_schema(),
    ));
    let handlers = ApplicationHandlerDescriptors::new(descriptors).unwrap();

    assert_eq!(
        validate_application_catalog(&application_catalog_contributions().unwrap(), &handlers),
        inconsistent("application handler use case")
    );
}

#[test]
fn root_composition_is_deterministic() {
    let first = build_application_catalog_snapshot().unwrap();
    let second = build_application_catalog_snapshot().unwrap();

    assert_eq!(first, second);
    assert_eq!(first.digest(), second.digest());
}

fn descriptor_with_contract(
    capability_id: &str,
    use_case_id: &str,
    request_schema: SchemaRef,
    result_schema: SchemaRef,
) -> ApplicationHandlerDescriptor {
    ApplicationHandlerDescriptor::new(
        ApplicationOperation::new(
            CapabilityId::new(capability_id).unwrap(),
            UseCaseId::new(use_case_id).unwrap(),
            ResultContractRef::from_schema(&result_schema),
            true,
        ),
        request_schema,
        result_schema,
    )
    .unwrap()
}

fn symbol_request_schema() -> SchemaRef {
    schema("schema.application.symbol-search.request")
}

fn symbol_result_schema() -> SchemaRef {
    schema("schema.application.symbol-search.result")
}

fn schema(id: &str) -> SchemaRef {
    SchemaRef::new(SchemaId::new(id).unwrap(), 1).unwrap()
}

fn inconsistent(field: &'static str) -> Result<(), CatalogCompositionError> {
    Err(CatalogCompositionError::Application(
        ApplicationContractError::Inconsistent { field },
    ))
}

// Growth tripwire only, not an MCP client or protocol limit. The complete
// final-V2 profile measures 626,799 bytes with every application, Work, and
// workflow tool projecting its canonical request schema through
// `tracedecay_mcp::mcp_input_schema` (the CAS-gated configuration writes bound
// their value unions). This reviewed 640 KiB ceiling leaves about 4% headroom;
// raising it requires another serialized tools/list measurement and a stated
// reason for the additional payload.
const DEFAULT_PROFILE_TOOLS_LIST_REGRESSION_CEILING_BYTES: usize = 640 * 1024;

/// The composed catalog's default profile must stay inside its reviewed budget
/// and keep the MCP `tools/list` payload it produces inside the measured
/// ceiling. The composed snapshot lives in `tracedecay-contracts`; the tool
/// registry it feeds lives here, so the joined measurement belongs in the root
/// suite.
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
    assert!(default_binding_count > 0);
    assert_eq!(
        default_binding_count,
        default_profile.budget().maximum_bindings() as usize,
        "the default profile budget must exactly track composition"
    );

    let definitions = get_catalog_filtered_tool_definitions_with_budget(
        0,
        explore_call_budget(0),
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
