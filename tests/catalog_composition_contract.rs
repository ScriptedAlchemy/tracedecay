use std::collections::BTreeSet;

use tracedecay::catalog_composition::{
    CatalogCompositionError, build_application_catalog_snapshot, validate_application_catalog,
};
use tracedecay_api::{HttpApplicationOperation, http_route_documents};
use tracedecay_application::{
    ApplicationContractError, ApplicationHandlerDescriptor, ApplicationHandlerDescriptors,
    ApplicationOperation, ResultContractRef, application_catalog_contributions,
    application_handler_descriptors, retrieval::catalog::symbol_search_contribution,
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

    let symbol_search = CapabilityId::new("capability.retrieval.symbol-search").unwrap();
    assert!(snapshot.capability(&symbol_search).is_some());

    let default_profile = ProfileId::new("profile.default").unwrap();
    assert!(snapshot.profile(&default_profile).is_some());
    assert_eq!(
        snapshot
            .visible_capabilities(&default_profile, &BTreeSet::new())
            .into_iter()
            .map(|capability| capability.capability_id().as_str())
            .collect::<Vec<_>>(),
        vec![
            "capability.application.api-migration.plan",
            "capability.application.code-query.callees",
            "capability.application.code-query.declaration",
            "capability.application.code-query.definition",
            "capability.application.code-query.exact-occurrence",
            "capability.application.code-query.facets",
            "capability.application.code-query.phrase-search",
            "capability.application.code-query.references",
            "capability.application.code-query.timeline",
            "capability.application.code-query.type-definition",
            "capability.application.configuration.audit",
            "capability.application.configuration.batch",
            "capability.application.configuration.explain",
            "capability.application.configuration.get",
            "capability.application.configuration.list",
            "capability.application.configuration.observed_state",
            "capability.application.configuration.protected_apply",
            "capability.application.configuration.protected_preview",
            "capability.application.configuration.rollback_apply",
            "capability.application.configuration.rollback_preview",
            "capability.application.configuration.set",
            "capability.application.configuration.unset",
            "capability.application.configuration.write_credential",
            "capability.application.context-scout-budget",
            "capability.application.context-scout-cancel",
            "capability.application.context-scout-capability",
            "capability.application.context-scout-claim",
            "capability.application.context-scout-delivery",
            "capability.application.context-scout-explain",
            "capability.application.context-scout-feedback",
            "capability.application.context-scout-pause",
            "capability.application.context-scout-recent",
            "capability.application.context-scout-resume",
            "capability.application.context-scout-status",
            "capability.application.feedback.advisory-cycle",
            "capability.application.feedback.affected-tests",
            "capability.application.feedback.diagnostics",
            "capability.application.feedback.expand",
            "capability.application.feedback.get",
            "capability.application.feedback.impact",
            "capability.application.feedback.list",
            "capability.application.feedback.test-results",
            "capability.application.git.apply",
            "capability.application.git.blame",
            "capability.application.git.diff",
            "capability.application.git.history",
            "capability.application.git.hunks",
            "capability.application.git.preview",
            "capability.application.git.status",
            "capability.application.primitive.call-chain",
            "capability.application.primitive.code-callers",
            "capability.application.primitive.code-implementations",
            "capability.application.primitive.code-signature-search",
            "capability.application.primitive.code-type-hierarchy",
            "capability.application.primitive.diagnostics-read",
            "capability.application.primitive.file-dependents",
            "capability.application.primitive.file-metadata",
            "capability.application.primitive.health-delta",
            "capability.application.primitive.health-read",
            "capability.application.primitive.module-api",
            "capability.application.primitive.qualified-name",
            "capability.application.primitive.session-lookup",
            "capability.application.primitive.source-body",
            "capability.application.primitive.source-lines",
            "capability.application.primitive.source-outline",
            "capability.application.primitive.storage-status",
            "capability.application.retained.fact-feedback",
            "capability.application.retained.fact-store",
            "capability.application.retained.lcm-compress",
            "capability.application.retained.lcm-describe",
            "capability.application.retained.lcm-doctor",
            "capability.application.retained.lcm-expand",
            "capability.application.retained.lcm-expand-query",
            "capability.application.retained.lcm-grep",
            "capability.application.retained.lcm-load-session",
            "capability.application.retained.lcm-preflight",
            "capability.application.retained.lcm-session-boundary",
            "capability.application.retained.lcm-status",
            "capability.application.retained.memory-status",
            "capability.application.retained.message-search",
            "capability.application.retained.session-end",
            "capability.application.retained.session-refresh",
            "capability.application.retained.session-start",
            "capability.application.retained.sessions-for",
            "capability.application.retained.workflows",
            "capability.application.source-edit.api-migration-apply",
            "capability.application.source-edit.ast-grep-rewrite",
            "capability.application.source-edit.insert-at",
            "capability.application.source-edit.insert-at-symbol",
            "capability.application.source-edit.move-symbol",
            "capability.application.source-edit.multi-str-replace",
            "capability.application.source-edit.reconcile",
            "capability.application.source-edit.replace-symbol",
            "capability.application.source-edit.str-replace",
            "capability.retrieval.symbol-search",
        ]
    );
}

#[test]
fn root_snapshot_composes_every_explicit_profile_without_widening_eligibility() {
    let snapshot = build_application_catalog_snapshot().unwrap();
    let expected_profiles = [
        (
            "profile.default",
            ProfileKind::Default,
            ProfileBudget::new(320, 18_000).unwrap(),
        ),
        (
            "profile.compact",
            ProfileKind::Compact,
            ProfileBudget::new(22, 4_000).unwrap(),
        ),
        (
            "profile.administrative",
            ProfileKind::Administrative,
            ProfileBudget::new(40, 8_000).unwrap(),
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
    let symbol_search = CapabilityId::new("capability.retrieval.symbol-search").unwrap();
    let authorized = BTreeSet::from([symbol_search.clone()]);
    let scope = BTreeSet::from([
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

    assert!(!documents.is_empty());
    assert_eq!(documents.len(), visible_http_bindings.len());
    assert!(documents.iter().all(|document| {
        HttpApplicationOperation::from_catalog_name(&document.operation)
            .is_some_and(|operation| operation.route_path() == document.path)
    }));
    assert!(documents.iter().all(|document| {
        !matches!(document.operation.as_str(), "git_preview" | "git_apply")
            && !matches!(document.path.as_str(), "/git/preview" | "/git/apply")
    }));
}

#[test]
fn registered_capability_does_not_require_a_catalog_surface_binding() {
    assert_eq!(
        validate_application_catalog(
            &[symbol_search_contribution().unwrap()],
            &ApplicationHandlerDescriptors::new([descriptor_with_contract(
                "capability.retrieval.symbol-search",
                "use-case.retrieval.symbol-search",
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
                "capability.retrieval.symbol-search",
                "use-case.retrieval.symbol-search",
                schema("schema.test.drifted-request"),
                symbol_result_schema(),
            ),
            "application capability schema mapping",
        ),
        (
            descriptor_with_contract(
                "capability.retrieval.symbol-search",
                "use-case.retrieval.symbol-search",
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
        "use-case.retrieval.symbol-search",
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
