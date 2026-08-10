use std::collections::BTreeSet;

use tracedecay::catalog_composition::{
    CatalogCompositionError, build_application_catalog_snapshot, validate_application_catalog,
};
use tracedecay_api::{
    HttpApplicationOperation, http_route_documents, retained_application_route_path,
};
use tracedecay_application::{
    ApplicationContractError, ApplicationHandlerDescriptor, ApplicationHandlerDescriptors,
    ApplicationOperation, ResultContractRef, RetainedSurfaceOperation,
    application_catalog_contributions, application_handler_descriptors,
    retrieval::catalog::symbol_search_contribution,
};
use tracedecay_tool_catalog::{
    BindingSurface, CapabilityId, ProfileBudget, ProfileId, ProfileKind, SchemaId, SchemaRef,
    ScopeDimension, UseCaseId,
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
        "capability.application.configuration.semantic_model_retry",
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
    let expected_profiles = [
        (
            "profile.default",
            ProfileKind::Default,
            ProfileBudget::new(448, 18_000).unwrap(),
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
            match HttpApplicationOperation::from_catalog_name(binding.operation().as_str()) {
                Some(operation) => !operation.is_http_exposed(),
                None => RetainedSurfaceOperation::from_name(binding.operation().as_str())
                    .is_none_or(|operation| !operation.is_callable()),
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
        HttpApplicationOperation::from_catalog_name(&document.operation)
            .is_some_and(|operation| operation.application_route_path() == document.path)
            || RetainedSurfaceOperation::from_name(&document.operation).is_some_and(|operation| {
                operation.is_callable()
                    && retained_application_route_path(operation) == document.path
            })
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
