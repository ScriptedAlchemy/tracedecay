use std::collections::BTreeSet;

use super::{
    DEFAULT_HTTP_PAGE_SIZE, HttpApplicationOwnerKind, HttpPageQuery,
    http_application_full_route_path, http_application_owner_kind,
    is_http_application_operation_exposed, parse_callable_code_operation,
    parse_configuration_operation, parse_context_scout_operation, parse_feedback_read_operation,
    parse_git_read_operation, parse_native_integration_operation,
};
use tracedecay_application::{
    configuration::CONFIGURATION_SURFACE_OPERATION_NAMES, configuration_executable_binding_registry,
};
use tracedecay_tool_catalog::{ApplicationSurfaceOperation, OperationId, RouteExposureV1};

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
        ("status", ApplicationSurfaceOperation::GitStatus),
        ("diff", ApplicationSurfaceOperation::GitDiff),
        ("history", ApplicationSurfaceOperation::GitHistory),
        ("blame", ApplicationSurfaceOperation::GitBlame),
        ("hunks", ApplicationSurfaceOperation::GitHunks),
    ] {
        assert_eq!(parse_git_read_operation(route), Some(operation));
        assert_eq!(
            http_application_owner_kind(operation),
            HttpApplicationOwnerKind::Git
        );
        assert_eq!(operation.as_str(), format!("git_{route}"));
    }
    for rejected in ["", "preview", "apply", "git_status", "status/"] {
        assert_eq!(parse_git_read_operation(rejected), None);
    }
}

#[test]
fn feedback_read_operation_parser_is_exact_and_separately_owned() {
    for (route, operation) in [
        ("get", ApplicationSurfaceOperation::FeedbackGet),
        ("expand", ApplicationSurfaceOperation::FeedbackExpand),
        ("list", ApplicationSurfaceOperation::FeedbackList),
    ] {
        assert_eq!(parse_feedback_read_operation(route), Some(operation));
        assert_eq!(
            http_application_owner_kind(operation),
            HttpApplicationOwnerKind::Feedback
        );
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
            ApplicationSurfaceOperation::CodeExactOccurrence,
            HttpApplicationOwnerKind::CallableCode,
        ),
        (
            "code_phrase_search",
            ApplicationSurfaceOperation::CodePhraseSearch,
            HttpApplicationOwnerKind::CallableCode,
        ),
        (
            "code_symbol_search",
            ApplicationSurfaceOperation::CodeSymbolSearch,
            HttpApplicationOwnerKind::Primitive,
        ),
        (
            "code_signature_search",
            ApplicationSurfaceOperation::CodeSignatureSearch,
            HttpApplicationOwnerKind::Primitive,
        ),
        (
            "code_implementations",
            ApplicationSurfaceOperation::CodeImplementations,
            HttpApplicationOwnerKind::Primitive,
        ),
        (
            "code_type_hierarchy",
            ApplicationSurfaceOperation::CodeTypeHierarchy,
            HttpApplicationOwnerKind::Primitive,
        ),
        (
            "code_callers",
            ApplicationSurfaceOperation::CodeCallers,
            HttpApplicationOwnerKind::Primitive,
        ),
        (
            "code_callees",
            ApplicationSurfaceOperation::CodeCallees,
            HttpApplicationOwnerKind::CallableCode,
        ),
        (
            "code_facets",
            ApplicationSurfaceOperation::CodeFacets,
            HttpApplicationOwnerKind::CallableCode,
        ),
        (
            "code_timeline",
            ApplicationSurfaceOperation::CodeTimeline,
            HttpApplicationOwnerKind::CallableCode,
        ),
        (
            "code_declaration",
            ApplicationSurfaceOperation::CodeDeclaration,
            HttpApplicationOwnerKind::CallableCode,
        ),
        (
            "code_definition",
            ApplicationSurfaceOperation::CodeDefinition,
            HttpApplicationOwnerKind::CallableCode,
        ),
        (
            "code_type_definition",
            ApplicationSurfaceOperation::CodeTypeDefinition,
            HttpApplicationOwnerKind::CallableCode,
        ),
        (
            "code_references",
            ApplicationSurfaceOperation::CodeReferences,
            HttpApplicationOwnerKind::CallableCode,
        ),
    ] {
        assert_eq!(parse_callable_code_operation(name), Some(operation));
        assert_eq!(operation.as_str(), name);
        assert_eq!(http_application_owner_kind(operation), owner);
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
            ApplicationSurfaceOperation::ConfigurationList,
        ),
        (
            "configuration_explain",
            ApplicationSurfaceOperation::ConfigurationExplain,
        ),
        (
            "configuration_get",
            ApplicationSurfaceOperation::ConfigurationGet,
        ),
        (
            "configuration_set",
            ApplicationSurfaceOperation::ConfigurationSet,
        ),
        (
            "configuration_unset",
            ApplicationSurfaceOperation::ConfigurationUnset,
        ),
        (
            "configuration_batch",
            ApplicationSurfaceOperation::ConfigurationBatch,
        ),
        (
            "configuration_write_credential",
            ApplicationSurfaceOperation::ConfigurationWriteCredential,
        ),
        (
            "configuration_observed_state",
            ApplicationSurfaceOperation::ConfigurationObservedState,
        ),
        (
            "configuration_protected_preview",
            ApplicationSurfaceOperation::ConfigurationProtectedPreview,
        ),
        (
            "configuration_protected_apply",
            ApplicationSurfaceOperation::ConfigurationProtectedApply,
        ),
        (
            "configuration_rollback_preview",
            ApplicationSurfaceOperation::ConfigurationRollbackPreview,
        ),
        (
            "configuration_rollback_apply",
            ApplicationSurfaceOperation::ConfigurationRollbackApply,
        ),
        (
            "configuration_audit",
            ApplicationSurfaceOperation::ConfigurationAudit,
        ),
    ];

    for (name, operation) in expected {
        assert_eq!(parse_configuration_operation(name), Some(operation));
        assert_eq!(operation.as_str(), name);
        assert_eq!(
            http_application_full_route_path(operation),
            format!("/application/configuration/{name}")
        );
        assert_eq!(
            http_application_owner_kind(operation),
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
        let operation =
            ApplicationSurfaceOperation::from_catalog_name(name).expect("HTTP operation");
        let operation_id =
            OperationId::new(format!("operation.application.{name}")).expect("operation ID");
        let binding = registry
            .get(&operation_id)
            .and_then(|availability| availability.binding())
            .expect("executable configuration binding");
        assert!(matches!(
            binding.exposure(),
            RouteExposureV1::Public { route_path, .. }
                if route_path == &http_application_full_route_path(operation)
        ));
    }
}

#[test]
fn context_scout_operation_parser_is_exact_and_backend_only() {
    for operation in [
        ApplicationSurfaceOperation::ContextScoutStatus,
        ApplicationSurfaceOperation::ContextScoutRecent,
        ApplicationSurfaceOperation::ContextScoutExplain,
        ApplicationSurfaceOperation::ContextScoutCapability,
        ApplicationSurfaceOperation::ContextScoutBudget,
        ApplicationSurfaceOperation::ContextScoutPause,
        ApplicationSurfaceOperation::ContextScoutResume,
        ApplicationSurfaceOperation::ContextScoutCancel,
        ApplicationSurfaceOperation::ContextScoutClaim,
        ApplicationSurfaceOperation::ContextScoutDelivery,
        ApplicationSurfaceOperation::ContextScoutFeedback,
    ] {
        assert_eq!(
            parse_context_scout_operation(operation.as_str()),
            Some(operation)
        );
        assert_eq!(
            http_application_owner_kind(operation),
            HttpApplicationOwnerKind::ContextScout
        );
    }
    assert_eq!(parse_context_scout_operation("context_scout"), None);
    assert_eq!(parse_context_scout_operation("context_scout_status/"), None);
}

#[test]
fn canonical_operation_authority_covers_all_surface_names_and_git_mutations() {
    let mut names = BTreeSet::new();
    for operation in ApplicationSurfaceOperation::ALL {
        assert!(
            names.insert(operation.as_str()),
            "canonical operation names must be unique"
        );
        assert_eq!(
            ApplicationSurfaceOperation::from_tool_name(&format!(
                "tracedecay_{}",
                operation.as_str()
            )),
            Some(operation),
            "{} must round-trip through the canonical tool name",
            operation.as_str()
        );
    }
    assert_eq!(
        ApplicationSurfaceOperation::from_tool_name("tracedecay_diagnostics"),
        Some(ApplicationSurfaceOperation::DiagnosticsRead)
    );
    assert!(!is_http_application_operation_exposed(
        ApplicationSurfaceOperation::GitPreview
    ));
    assert!(!is_http_application_operation_exposed(
        ApplicationSurfaceOperation::GitApply
    ));
    assert!(!is_http_application_operation_exposed(
        ApplicationSurfaceOperation::ObservatoryRead
    ));
    assert_eq!(
        http_application_owner_kind(ApplicationSurfaceOperation::ObservatoryRead),
        HttpApplicationOwnerKind::Observatory
    );
    assert_eq!(
        http_application_owner_kind(ApplicationSurfaceOperation::GitPreview),
        HttpApplicationOwnerKind::Git
    );
    assert_eq!(
        http_application_owner_kind(ApplicationSurfaceOperation::GitApply),
        HttpApplicationOwnerKind::Git
    );
    assert!(is_http_application_operation_exposed(
        ApplicationSurfaceOperation::GitHubStackSignalExpand
    ));
    assert_eq!(
        http_application_full_route_path(ApplicationSurfaceOperation::GitHubStackSignalExpand),
        "/application/github-stack/signal-expand"
    );
}

#[test]
fn native_worktree_http_parser_admits_only_the_five_public_operations() {
    for operation in [
        ApplicationSurfaceOperation::NativeIntegrationWorktreeInventory,
        ApplicationSurfaceOperation::NativeIntegrationWorktreeInspect,
        ApplicationSurfaceOperation::NativeIntegrationWorktreeConfirm,
        ApplicationSurfaceOperation::NativeIntegrationWorktreeRemove,
        ApplicationSurfaceOperation::NativeIntegrationWorktreeReconcile,
    ] {
        assert_eq!(
            parse_native_integration_operation(operation.as_str()),
            Some(operation)
        );
        assert_eq!(
            http_application_full_route_path(operation),
            format!("/application/native-integration/{}", operation.as_str())
        );
    }
    for operation in [
        ApplicationSurfaceOperation::NativeIntegrationStackSnapshot,
        ApplicationSurfaceOperation::NativeIntegrationPreflight,
        ApplicationSurfaceOperation::NativeIntegrationApprove,
        ApplicationSurfaceOperation::NativeIntegrationApply,
        ApplicationSurfaceOperation::NativeIntegrationStatus,
        ApplicationSurfaceOperation::NativeIntegrationCancel,
    ] {
        assert_eq!(parse_native_integration_operation(operation.as_str()), None);
    }
}
