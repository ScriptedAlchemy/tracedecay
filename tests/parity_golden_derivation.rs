use std::collections::BTreeSet;

use tracedecay::application_surface::{
    ApplicationSurfaceOperation, parse_application_surface_request,
    resolve_application_surface_dispatch, resolve_http_application_surface_dispatch,
};
use tracedecay::daemon_client::{
    BindingResolution, BindingResolver, CatalogBindingResolver, RequestedOutputFormat,
};
use tracedecay::mcp::tools::dispatch::resolve_mcp_application_surface_dispatch;
use tracedecay_application::{APPLICATION_DEFAULT_PROFILE_ID, RequestId};
use tracedecay_tool_catalog::{BindingSurface, ProfileId, SurfaceOperationName};

fn code_body(extra: serde_json::Value) -> serde_json::Value {
    let mut request = serde_json::json!({
        "scope": {
            "generation": "generation.application-parity",
            "path_prefix": "src"
        },
        "meta": {
            "projection": "evidence",
            "order": "source_position"
        }
    });
    request
        .as_object_mut()
        .expect("request object")
        .extend(extra.as_object().expect("extra object").clone());
    request
}

fn graph_body(extra: serde_json::Value) -> serde_json::Value {
    let mut request = serde_json::json!({
        "scope": {
            "path_prefix": "src"
        },
        "meta": {
            "projection": "evidence",
            "order": "source_position"
        }
    });
    request
        .as_object_mut()
        .expect("request object")
        .extend(extra.as_object().expect("extra object").clone());
    request
}

fn requests() -> Vec<(ApplicationSurfaceOperation, serde_json::Value)> {
    vec![
        (
            ApplicationSurfaceOperation::CodeExactOccurrence,
            code_body(serde_json::json!({
                "literal": "ApplicationSurfaceOperation",
                "kind": "whole_symbol"
            })),
        ),
        (
            ApplicationSurfaceOperation::CodePhraseSearch,
            code_body(serde_json::json!({
                "query": "callable application surface",
                "phrases": ["callable application", "surface"],
                "field_filters": [{"field": "path", "include": true}],
                "fuzzy_budget": 7
            })),
        ),
        (
            ApplicationSurfaceOperation::CodeSymbolSearch,
            graph_body(serde_json::json!({
                "query": "ApplicationSurfaceOperation",
                "lazy_index_ignored_dependencies": false
            })),
        ),
        (
            ApplicationSurfaceOperation::CodeSignatureSearch,
            graph_body(serde_json::json!({
                "returns": "ApplicationResult",
                "params": ["RequestContext"],
                "is_async": true
            })),
        ),
        (
            ApplicationSurfaceOperation::CodeImplementations,
            graph_body(serde_json::json!({
                "selector": {"selector": "trait", "name": "HttpApplicationOwners"}
            })),
        ),
        (
            ApplicationSurfaceOperation::CodeTypeHierarchy,
            graph_body(serde_json::json!({
                "node_id": "node.application-parity",
                "maximum_depth": 3
            })),
        ),
        (
            ApplicationSurfaceOperation::CodeCallers,
            graph_body(serde_json::json!({
                "node_id": "node.application-parity",
                "maximum_depth": 3,
                "resolve_trait_dispatch": true
            })),
        ),
        (
            ApplicationSurfaceOperation::CodeCallees,
            code_body(serde_json::json!({
                "node_id": "node.application-parity",
                "maximum_depth": 3,
                "resolve_trait_dispatch": true
            })),
        ),
        (
            ApplicationSurfaceOperation::CodeFacets,
            code_body(serde_json::json!({"dimension": "language"})),
        ),
        (
            ApplicationSurfaceOperation::CodeTimeline,
            code_body(serde_json::json!({})),
        ),
        (
            ApplicationSurfaceOperation::CodeDeclaration,
            code_body(serde_json::json!({"node_id": "symbol.application-parity"})),
        ),
        (
            ApplicationSurfaceOperation::CodeDefinition,
            code_body(serde_json::json!({"node_id": "symbol.application-parity"})),
        ),
        (
            ApplicationSurfaceOperation::CodeTypeDefinition,
            code_body(serde_json::json!({"node_id": "symbol.application-parity"})),
        ),
        (
            ApplicationSurfaceOperation::CodeReferences,
            code_body(serde_json::json!({"node_id": "symbol.application-parity"})),
        ),
    ]
}

#[test]
fn dump_code_operation_contracts() {
    let catalog = tracedecay::application_surface::application_surface_catalog()
        .expect("application catalog");
    let resolver = CatalogBindingResolver::new(&catalog);
    let mut out = serde_json::Map::new();
    for (operation, body) in requests() {
        let resolution = BindingResolution {
            profile_id: ProfileId::new(APPLICATION_DEFAULT_PROFILE_ID).expect("profile"),
            operation: SurfaceOperationName::new(operation.as_str()).expect("operation"),
            protocol_revision: 1,
            negotiated_features: BTreeSet::new(),
        };
        let mut bindings = serde_json::Map::new();
        let mut request_schema = String::new();
        let mut result_schema = String::new();
        for (surface, name) in [
            (BindingSurface::Cli, "cli"),
            (BindingSurface::Mcp, "mcp"),
            (BindingSurface::Http, "http"),
        ] {
            let Some(resolved) = resolver.resolve_binding(surface, &resolution) else {
                continue;
            };
            let request = parse_application_surface_request(operation, body.clone())
                .unwrap_or_else(|error| {
                    panic!("{} body must parse: {error:?}", operation.as_str())
                });
            let request_id = RequestId::new(format!("request.{name}.{}", operation.as_str()))
                .expect("request id");
            let dispatched = match surface {
                BindingSurface::Cli => resolve_application_surface_dispatch(
                    BindingSurface::Cli,
                    operation,
                    request_id,
                    request,
                    RequestedOutputFormat::Json,
                ),
                BindingSurface::Mcp => resolve_mcp_application_surface_dispatch(
                    operation,
                    request_id,
                    request,
                    RequestedOutputFormat::Json,
                ),
                _ => resolve_http_application_surface_dispatch(
                    operation,
                    request_id,
                    request,
                    RequestedOutputFormat::Json,
                ),
            }
            .unwrap_or_else(|error| panic!("{} {name} dispatch: {error:?}", operation.as_str()));
            assert_eq!(
                dispatched.invocation.binding_id.as_str(),
                resolved.binding_id.as_str()
            );
            request_schema = dispatched
                .invocation
                .request_schema
                .schema_id()
                .as_str()
                .to_owned();
            result_schema = dispatched
                .invocation
                .result_schema
                .schema_id()
                .as_str()
                .to_owned();
            bindings.insert(
                name.to_owned(),
                serde_json::Value::from(resolved.binding_id.as_str()),
            );
        }
        let mut entry = serde_json::Map::new();
        entry.insert("request".to_owned(), body);
        entry.insert("request_schema".to_owned(), request_schema.into());
        entry.insert("result_schema".to_owned(), result_schema.into());
        entry.insert("bindings".to_owned(), bindings.into());
        out.insert(operation.as_str().to_owned(), entry.into());
    }
    println!(
        "DERIVED_GOLDEN_BEGIN\n{}\nDERIVED_GOLDEN_END",
        serde_json::to_string_pretty(&serde_json::Value::from(out.clone())).expect("json")
    );

    let pinned: BTreeSet<&str> = [
        "git_preview",
        "git_apply",
        "feedback_diagnostics",
        "feedback_get",
        "feedback_expand",
        "feedback_list",
        "feedback_impact",
        "affected_tests",
        "test_results",
    ]
    .into_iter()
    .chain(out.keys().map(String::as_str))
    .collect();
    let unpinned: Vec<&str> = tracedecay::application_surface::APPLICATION_SURFACE_OPERATIONS
        .iter()
        .map(|operation| operation.as_str())
        .filter(|operation| !pinned.contains(operation))
        .collect();
    println!(
        "DERIVED_UNPINNED_BEGIN\n{}\nDERIVED_UNPINNED_END",
        serde_json::to_string_pretty(&unpinned).expect("json")
    );
}
