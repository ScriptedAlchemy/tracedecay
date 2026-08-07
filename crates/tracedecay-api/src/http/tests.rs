use super::{
    DEFAULT_HTTP_PAGE_SIZE, HttpApplicationOperation, HttpApplicationOwnerKind, HttpPageQuery,
    parse_callable_code_operation, parse_configuration_operation, parse_context_scout_operation,
    parse_feedback_read_operation, parse_git_read_operation,
};
use tracedecay_application::{
    configuration::CONFIGURATION_SURFACE_OPERATION_NAMES, configuration_executable_binding_registry,
};
use tracedecay_tool_catalog::{OperationId, RouteExposureV1};

#[test]
fn omitted_http_page_query_uses_the_canonical_default() {
    let query: HttpPageQuery = serde_json::from_value(serde_json::json!({}))
        .expect("empty HTTP query uses adapter defaults");
    assert_eq!(query.page_size, DEFAULT_HTTP_PAGE_SIZE);
    assert!(query.cursor.is_none());
}

#[test]
fn git_read_operation_parser_is_exact_and_read_only() {
    for (route, operation) in [
        ("status", HttpApplicationOperation::GitStatus),
        ("diff", HttpApplicationOperation::GitDiff),
        ("history", HttpApplicationOperation::GitHistory),
        ("blame", HttpApplicationOperation::GitBlame),
        ("hunks", HttpApplicationOperation::GitHunks),
    ] {
        assert_eq!(parse_git_read_operation(route), Some(operation));
        assert_eq!(operation.owner_kind(), HttpApplicationOwnerKind::Git);
        assert_eq!(operation.as_str(), format!("git_{route}"));
    }
    for rejected in ["", "preview", "apply", "git_status", "status/"] {
        assert_eq!(parse_git_read_operation(rejected), None);
    }
}

#[test]
fn feedback_read_operation_parser_is_exact_and_separately_owned() {
    for (route, operation) in [
        ("get", HttpApplicationOperation::FeedbackGet),
        ("expand", HttpApplicationOperation::FeedbackExpand),
        ("list", HttpApplicationOperation::FeedbackList),
    ] {
        assert_eq!(parse_feedback_read_operation(route), Some(operation));
        assert_eq!(operation.owner_kind(), HttpApplicationOwnerKind::Feedback);
        assert_eq!(operation.as_str(), format!("feedback_{route}"));
    }
    for rejected in ["", "status", "get/", "feedback_get"] {
        assert_eq!(parse_feedback_read_operation(rejected), None);
    }
}

#[test]
fn callable_code_operation_parser_is_exact_and_separately_owned() {
    for (name, operation, owner) in [
        (
            "code_exact_occurrence",
            HttpApplicationOperation::CodeExactOccurrence,
            HttpApplicationOwnerKind::CallableCode,
        ),
        (
            "code_phrase_search",
            HttpApplicationOperation::CodePhraseSearch,
            HttpApplicationOwnerKind::CallableCode,
        ),
        (
            "code_symbol_search",
            HttpApplicationOperation::CodeSymbolSearch,
            HttpApplicationOwnerKind::Primitive,
        ),
        (
            "code_signature_search",
            HttpApplicationOperation::CodeSignatureSearch,
            HttpApplicationOwnerKind::Primitive,
        ),
        (
            "code_implementations",
            HttpApplicationOperation::CodeImplementations,
            HttpApplicationOwnerKind::Primitive,
        ),
        (
            "code_type_hierarchy",
            HttpApplicationOperation::CodeTypeHierarchy,
            HttpApplicationOwnerKind::Primitive,
        ),
        (
            "code_callers",
            HttpApplicationOperation::CodeCallers,
            HttpApplicationOwnerKind::Primitive,
        ),
        (
            "code_callees",
            HttpApplicationOperation::CodeCallees,
            HttpApplicationOwnerKind::CallableCode,
        ),
        (
            "code_facets",
            HttpApplicationOperation::CodeFacets,
            HttpApplicationOwnerKind::CallableCode,
        ),
        (
            "code_timeline",
            HttpApplicationOperation::CodeTimeline,
            HttpApplicationOwnerKind::CallableCode,
        ),
        (
            "code_declaration",
            HttpApplicationOperation::CodeDeclaration,
            HttpApplicationOwnerKind::CallableCode,
        ),
        (
            "code_definition",
            HttpApplicationOperation::CodeDefinition,
            HttpApplicationOwnerKind::CallableCode,
        ),
        (
            "code_type_definition",
            HttpApplicationOperation::CodeTypeDefinition,
            HttpApplicationOwnerKind::CallableCode,
        ),
        (
            "code_references",
            HttpApplicationOperation::CodeReferences,
            HttpApplicationOwnerKind::CallableCode,
        ),
    ] {
        assert_eq!(parse_callable_code_operation(name), Some(operation));
        assert_eq!(operation.as_str(), name);
        assert_eq!(operation.owner_kind(), owner);
    }
    for rejected in [
        "",
        "exact_occurrence",
        "phrase_search",
        "callees",
        "code_callers/",
        "code_callees/",
    ] {
        assert_eq!(parse_callable_code_operation(rejected), None);
    }
}

#[test]
fn configuration_operation_parser_is_exact_and_closed() {
    let expected = [
        (
            "configuration_list",
            HttpApplicationOperation::ConfigurationList,
        ),
        (
            "configuration_explain",
            HttpApplicationOperation::ConfigurationExplain,
        ),
        (
            "configuration_get",
            HttpApplicationOperation::ConfigurationGet,
        ),
        (
            "configuration_set",
            HttpApplicationOperation::ConfigurationSet,
        ),
        (
            "configuration_unset",
            HttpApplicationOperation::ConfigurationUnset,
        ),
        (
            "configuration_batch",
            HttpApplicationOperation::ConfigurationBatch,
        ),
        (
            "configuration_write_credential",
            HttpApplicationOperation::ConfigurationWriteCredential,
        ),
        (
            "configuration_observed_state",
            HttpApplicationOperation::ConfigurationObservedState,
        ),
        (
            "configuration_protected_preview",
            HttpApplicationOperation::ConfigurationProtectedPreview,
        ),
        (
            "configuration_protected_apply",
            HttpApplicationOperation::ConfigurationProtectedApply,
        ),
        (
            "configuration_rollback_preview",
            HttpApplicationOperation::ConfigurationRollbackPreview,
        ),
        (
            "configuration_rollback_apply",
            HttpApplicationOperation::ConfigurationRollbackApply,
        ),
        (
            "configuration_audit",
            HttpApplicationOperation::ConfigurationAudit,
        ),
    ];

    for (name, operation) in expected {
        assert_eq!(parse_configuration_operation(name), Some(operation));
        assert_eq!(operation.as_str(), name);
        assert_eq!(
            operation.application_route_path(),
            format!("/application/configuration/{name}")
        );
        assert_eq!(
            operation.owner_kind(),
            super::HttpApplicationOwnerKind::Configuration
        );
    }
    for rejected in [
        "",
        "list",
        "configuration",
        "configuration_LIST",
        "configuration_list/",
        "configuration_unknown",
    ] {
        assert_eq!(parse_configuration_operation(rejected), None);
    }
}

#[test]
fn configuration_http_routes_match_the_executable_sdk_catalog() {
    let registry = configuration_executable_binding_registry().expect("configuration registry");

    for name in CONFIGURATION_SURFACE_OPERATION_NAMES {
        let operation = HttpApplicationOperation::from_catalog_name(name).expect("HTTP operation");
        let operation_id =
            OperationId::new(format!("operation.application.{name}")).expect("operation ID");
        let binding = registry
            .get(&operation_id)
            .and_then(|availability| availability.binding())
            .expect("executable configuration binding");
        assert!(matches!(
            binding.exposure(),
            RouteExposureV1::Public { route_path, .. }
                if route_path == &operation.application_route_path()
        ));
    }
}

#[test]
fn context_scout_operation_parser_is_exact_and_backend_only() {
    for operation in [
        HttpApplicationOperation::ContextScoutStatus,
        HttpApplicationOperation::ContextScoutRecent,
        HttpApplicationOperation::ContextScoutExplain,
        HttpApplicationOperation::ContextScoutCapability,
        HttpApplicationOperation::ContextScoutBudget,
        HttpApplicationOperation::ContextScoutPause,
        HttpApplicationOperation::ContextScoutResume,
        HttpApplicationOperation::ContextScoutCancel,
        HttpApplicationOperation::ContextScoutClaim,
        HttpApplicationOperation::ContextScoutDelivery,
        HttpApplicationOperation::ContextScoutFeedback,
    ] {
        assert_eq!(
            parse_context_scout_operation(operation.as_str()),
            Some(operation)
        );
        assert_eq!(
            operation.owner_kind(),
            HttpApplicationOwnerKind::ContextScout
        );
    }
    assert_eq!(parse_context_scout_operation("context_scout"), None);
    assert_eq!(parse_context_scout_operation("context_scout_status/"), None);
}

#[test]
fn canonical_operation_authority_covers_all_surface_names_and_git_mutations() {
    assert_eq!(HttpApplicationOperation::ALL.len(), 66);
    for operation in HttpApplicationOperation::ALL {
        assert_eq!(
            HttpApplicationOperation::from_tool_name(&format!("tracedecay_{}", operation.as_str())),
            Some(operation),
            "{} must round-trip through the canonical tool name",
            operation.as_str()
        );
    }
    assert_eq!(
        HttpApplicationOperation::from_tool_name("tracedecay_diagnostics"),
        Some(HttpApplicationOperation::DiagnosticsRead)
    );
    assert!(!HttpApplicationOperation::GitPreview.is_http_exposed());
    assert!(!HttpApplicationOperation::GitApply.is_http_exposed());
    assert_eq!(
        HttpApplicationOperation::GitPreview.owner_kind(),
        HttpApplicationOwnerKind::Git
    );
    assert_eq!(
        HttpApplicationOperation::GitApply.owner_kind(),
        HttpApplicationOwnerKind::Git
    );
}
