use std::collections::BTreeSet;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;
use tracedecay_api::is_http_application_operation_exposed;
use tracedecay_contracts::{
    ApplicationContractError, ApplicationEnvelope, ApplicationOutcome, ApplicationProblem,
    ApplicationProblemEnvelope, ApplicationResponse, CancellationContext, CancellationSignal,
    CancellationState, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    OpaqueCursor, OperationBudgetUsage, OperationReceipt, PageRequest, RequestContext, RequestId,
    ResolvedScope, ResultContractRef, SafeDiagnostic,
};
use tracedecay_contracts::{ConfigurationListRequestV1, ConfigurationWireRequestV1};
use tracedecay_domain::configuration::{ConfigurationIdempotencyKey, ConfigurationRevisionId};
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, QueryNormalizationRevision, RefId, RepositoryId,
    SanitizerRevision, UtcMicros, WorktreeId,
};
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, BindingId, BindingSurface, CapabilityId, SchemaId, UseCaseId,
};

use super::handoff::validate_catalog_bindings as validate_handoff_catalog_bindings;
use super::workflow::validate_catalog_bindings as validate_workflow_catalog_bindings;
use super::{
    APPLICATION_PROTOCOL_REVISION, ActiveHttpRequest, CallableCodeSurfaceRequest,
    HttpCancellationRegistry, HttpOperationEventState, NativeIntegrationSurfaceRequest,
    PrimitiveCodeSurfaceRequest, application_http_context, application_negotiated_features,
    application_surface_dispatch_input_with_controls, current_micros, execute_application_surface,
    http_operation_event_router, invocation_problem, parse_http_application_surface_request,
    resolve_application_binding, resolve_application_surface_dispatch,
    resolve_authenticated_http_request_context, surface_rejection_metadata,
};
use tracedecay_application::operation_stream::{
    OperationEventAuthority, OperationEventError, OperationId, OperationKind, OperationStreamConfig,
};
use tracedecay_contracts::context_scout::{
    ContextScoutAddressV1, ContextScoutClaimRequestV1, ContextScoutClaimWindowV1,
    ContextScoutControlRequestV1, ContextScoutSurfaceRequestV1,
};
use tracedecay_contracts::feedback::observations::{
    FeedbackArgumentRejectionClassV1, FeedbackOutcomeV1, FeedbackRejectedArgumentV1,
};
use tracedecay_contracts::retrieval::PrimitiveRequest;
use tracedecay_daemon_protocol::RequestedOutputFormat;
use tracedecay_daemon_protocol::{
    ApplicationSurfaceAdapterError, ApplicationSurfaceRequest, FeedbackSurfaceRequest,
    adapt_application_tool_request, parse_application_surface_request,
};

fn operation_context(project_id: &ProjectId) -> RequestContext {
    let observed_at = current_micros().expect("current time");
    let expires_at = UtcMicros(observed_at.0.saturating_add(60_000_000));
    let scope = ResolvedScope::new(
        project_id.clone(),
        RepositoryId::new("repository.http-adapter").expect("repository"),
        WorktreeId::new("worktree.http-adapter").expect("worktree"),
        Some(RefId::new("refs/heads/http-adapter").expect("reference")),
    )
    .expect("scope");
    let capability = CapabilityId::new("capability.git.commit-index").expect("capability");
    let use_case = UseCaseId::new("use-case.git.preview").expect("use case");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.http-adapter").expect("grant"),
        1,
        ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).expect("digest"),
        ActorId::new("actor.tracedecay-daemon").expect("issuer"),
        observed_at,
        expires_at,
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Metadata,
    )
    .expect("grant");
    RequestContext::new(
        ActorId::new("actor.tracedecay-client").expect("actor"),
        scope,
        grant,
        RequestId::new("request.http-adapter").expect("request"),
        Deadline::new(expires_at).expect("deadline"),
        CancellationContext::active("cancel.http-adapter").expect("cancellation"),
    )
    .expect("context")
}

#[test]
fn daemon_reset_problem_preserves_reset_terminal_contract() {
    let problem =
        invocation_problem(tracedecay_daemon_protocol::DaemonInvocationProblem::ResetRequired)
            .expect("canonical reset problem");
    let ApplicationProblem::ResetRequired {
        retry,
        legal_actions,
        ..
    } = problem
    else {
        panic!("application surface must preserve reset-required");
    };

    assert_eq!(retry, tracedecay_contracts::RetryDirective::Never);
    assert_eq!(
        legal_actions,
        vec![tracedecay_contracts::LegalAction::Reset]
    );
}

#[test]
fn workflow_http_descriptors_match_every_executable_manifest_route() {
    validate_workflow_catalog_bindings().expect("every Workflow descriptor has one mounted route");
}

#[test]
fn handoff_http_descriptors_match_every_executable_manifest_route() {
    validate_handoff_catalog_bindings().expect("every handoff descriptor has one mounted route");
}

#[test]
fn every_http_exposed_operation_resolves_from_the_canonical_catalog() {
    let catalog = super::application_surface_catalog_ref().expect("application catalog");
    let resolver = tracedecay_daemon_protocol::CatalogBindingResolver::new(catalog);

    for operation in ApplicationSurfaceOperation::ALL {
        if is_http_application_operation_exposed(operation).expect("HTTP exposure") {
            assert!(
                resolve_application_binding(
                    &resolver,
                    tracedecay_tool_catalog::BindingSurface::Http,
                    operation,
                )
                .is_some(),
                "{operation:?} must resolve on the public HTTP surface",
            );
        }
    }
}

#[test]
fn native_worktree_http_bodies_parse_to_the_exact_daemon_operation() {
    let digest = format!("sha256:{}", "a".repeat(64));
    let repository_target = json!({
        "kind": "repository",
        "project_id": "project.worktree-http",
        "repository_id": "repository.worktree-http"
    });
    let worktree_target = json!({
        "kind": "worktree",
        "project_id": "project.worktree-http",
        "repository_id": "repository.worktree-http",
        "worktree_id": "worktree.worktree-http"
    });
    let binding = |target: Value| {
        json!({
            "scope_set_id": "scope-set.worktree-http",
            "scope_set_revision": 1,
            "scope_set_digest": digest.clone(),
            "target": target
        })
    };
    for (operation, mut body) in [
        (
            ApplicationSurfaceOperation::NativeIntegrationWorktreeInventory,
            binding(repository_target),
        ),
        (
            ApplicationSurfaceOperation::NativeIntegrationWorktreeInspect,
            binding(worktree_target.clone()),
        ),
        (
            ApplicationSurfaceOperation::NativeIntegrationWorktreeConfirm,
            binding(worktree_target.clone()),
        ),
        (
            ApplicationSurfaceOperation::NativeIntegrationWorktreeRemove,
            binding(worktree_target.clone()),
        ),
        (
            ApplicationSurfaceOperation::NativeIntegrationWorktreeReconcile,
            binding(worktree_target),
        ),
    ] {
        if matches!(
            operation,
            ApplicationSurfaceOperation::NativeIntegrationWorktreeConfirm
                | ApplicationSurfaceOperation::NativeIntegrationWorktreeRemove
        ) {
            body["inspection_digest"] = Value::String(digest.clone());
        }
        if operation == ApplicationSurfaceOperation::NativeIntegrationWorktreeRemove {
            body["confirmed_at"] = json!(10);
        }
        if matches!(
            operation,
            ApplicationSurfaceOperation::NativeIntegrationWorktreeRemove
                | ApplicationSurfaceOperation::NativeIntegrationWorktreeReconcile
        ) {
            body["confirmation_digest"] = Value::String(digest.clone());
        }
        let parsed = parse_application_surface_request(operation, body)
            .expect("canonical native worktree body");
        let ApplicationSurfaceRequest::NativeIntegration(
            NativeIntegrationSurfaceRequest::Worktree(request),
        ) = parsed
        else {
            panic!("native worktree request must keep its typed daemon envelope");
        };
        assert_eq!(request.operation(), operation.as_str());
    }
}

async fn response_text(response: axum::response::Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body")
            .to_vec(),
    )
    .expect("UTF-8 response")
}

#[test]
fn dispatch_controls_retain_the_callers_deadline_and_live_cancellation_identity() {
    let deadline = Deadline::new(UtcMicros(91)).expect("deadline");
    let cancellation =
        CancellationSignal::active("cancel.application-surface").expect("cancellation");
    let caller = cancellation.clone();
    let input = application_surface_dispatch_input_with_controls(
        BindingSurface::Mcp,
        ApplicationSurfaceOperation::FeedbackList,
        RequestId::new("request.application-surface").expect("request"),
        ApplicationSurfaceRequest::Feedback(
            FeedbackSurfaceRequest::new("feedback-handle.fixture".to_owned()).expect("handle"),
        ),
        PageRequest::first(7).expect("page"),
        Some(deadline.clone()),
        cancellation,
        RequestedOutputFormat::Json,
    )
    .expect("dispatch input");

    caller.cancel(UtcMicros(41));
    assert_eq!(input.controls.deadline, Some(deadline));
    assert!(matches!(
        input.controls.cancellation.context().state,
        CancellationState::Cancelled {
            requested_at: UtcMicros(41)
        }
    ));
}

#[tokio::test]
async fn http_context_caps_caller_deadline_at_the_transport_budget() {
    let before = current_micros().expect("time before request");
    let app = axum::Router::new()
        .route(
            "/deadline",
            axum::routing::get(
                |axum::extract::Extension(controls): axum::extract::Extension<
                    tracedecay_api::HttpApplicationControls,
                >| async move { controls.deadline.expires_at.0.to_string() },
            ),
        )
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new())),
            application_http_context,
        ));
    let response = app
        .oneshot(
            Request::get("/deadline")
                .header(super::HTTP_DEADLINE_HEADER, i64::MAX.to_string())
                .body(Body::empty())
                .expect("deadline request"),
        )
        .await
        .expect("deadline response");
    let after = current_micros().expect("time after request");
    let expires_at = response_text(response)
        .await
        .parse::<i64>()
        .expect("numeric effective deadline");

    assert!(expires_at >= before.0.saturating_add(super::DEFAULT_DEADLINE_MICROS));
    assert!(expires_at <= after.0.saturating_add(super::DEFAULT_DEADLINE_MICROS));
}

#[test]
fn every_configuration_operation_enters_the_canonical_dispatch_catalog() {
    let catalog = super::application_surface_catalog().expect("application catalog");
    let resolver = tracedecay_daemon_protocol::CatalogBindingResolver::new(&catalog);
    let profile_id = tracedecay_tool_catalog::ProfileId::new(
        tracedecay_contracts::APPLICATION_DEFAULT_PROFILE_ID,
    )
    .expect("application profile");
    for name in tracedecay_contracts::configuration::configuration_surface_operation_names() {
        let operation = ApplicationSurfaceOperation::from_tool_name(name)
            .unwrap_or_else(|| panic!("{name} must be a canonical surface operation"));
        assert_eq!(operation.as_str(), name);
        for surface in [
            tracedecay_tool_catalog::BindingSurface::Cli,
            tracedecay_tool_catalog::BindingSurface::Mcp,
            tracedecay_tool_catalog::BindingSurface::Http,
        ] {
            assert!(
                tracedecay_daemon_protocol::BindingResolver::resolve_binding(
                    &resolver,
                    surface,
                    &tracedecay_daemon_protocol::BindingResolution {
                        profile_id: profile_id.clone(),
                        operation: tracedecay_tool_catalog::SurfaceOperationName::new(name)
                            .expect("operation"),
                        protocol_revision: APPLICATION_PROTOCOL_REVISION,
                        negotiated_features: application_negotiated_features(),
                    },
                )
                .is_some(),
                "{name} must resolve on {surface:?}"
            );
        }
    }

    assert!(
        parse_application_surface_request(
            ApplicationSurfaceOperation::ConfigurationList,
            serde_json::json!({}),
        )
        .is_ok()
    );
}

#[test]
fn dashboard_configuration_dispatch_preserves_http_application_semantics() {
    let operation = ApplicationSurfaceOperation::ConfigurationList;
    let request = || {
        ApplicationSurfaceRequest::Configuration(ConfigurationWireRequestV1::List(
            ConfigurationListRequestV1::default(),
        ))
    };
    let http = resolve_application_surface_dispatch(
        tracedecay_tool_catalog::BindingSurface::Http,
        operation,
        RequestId::new("request.configuration.http").expect("HTTP request"),
        request(),
        RequestedOutputFormat::Json,
    )
    .expect("HTTP configuration dispatch");
    let dashboard = resolve_application_surface_dispatch(
        tracedecay_tool_catalog::BindingSurface::Dashboard,
        operation,
        RequestId::new("request.configuration.dashboard").expect("Dashboard request"),
        request(),
        RequestedOutputFormat::Json,
    )
    .expect("Dashboard configuration dispatch");

    assert_eq!(
        http.invocation.request_schema,
        dashboard.invocation.request_schema
    );
    assert_eq!(
        http.invocation.result_schema,
        dashboard.invocation.result_schema
    );
    assert_ne!(http.invocation.binding_id, dashboard.invocation.binding_id);
    assert_eq!(
        serde_json::to_value(&http.invocation.invocation.request).expect("HTTP request value"),
        serde_json::to_value(&dashboard.invocation.invocation.request)
            .expect("Dashboard request value")
    );
}

#[test]
fn cli_mcp_and_http_resolve_every_operation_through_the_current_catalog_gate() {
    let catalog = super::application_surface_catalog().expect("application catalog");
    let resolver = tracedecay_daemon_protocol::CatalogBindingResolver::new(&catalog);
    let profile_id = tracedecay_tool_catalog::ProfileId::new(
        tracedecay_contracts::APPLICATION_DEFAULT_PROFILE_ID,
    )
    .expect("application profile");
    for operation in ApplicationSurfaceOperation::ALL {
        let resolution_profile = &profile_id;
        // The production HTTP-exposure authority decides which operations
        // carry a public HTTP binding; everything resolves via CLI and MCP.
        let expected_surfaces =
            if is_http_application_operation_exposed(operation).expect("HTTP exposure") {
                &[
                    (tracedecay_tool_catalog::BindingSurface::Cli, "cli"),
                    (tracedecay_tool_catalog::BindingSurface::Mcp, "mcp"),
                    (tracedecay_tool_catalog::BindingSurface::Http, "http"),
                ][..]
            } else {
                &[
                    (tracedecay_tool_catalog::BindingSurface::Cli, "cli"),
                    (tracedecay_tool_catalog::BindingSurface::Mcp, "mcp"),
                ][..]
            };
        for &(surface, surface_name) in expected_surfaces {
            let operation_name = tracedecay_tool_catalog::SurfaceOperationName::new(
                operation.name_for_surface(surface),
            )
            .expect("operation name");
            let binding = tracedecay_daemon_protocol::BindingResolver::resolve_binding(
                &resolver,
                surface,
                &tracedecay_daemon_protocol::BindingResolution {
                    profile_id: resolution_profile.clone(),
                    operation: operation_name,
                    protocol_revision: APPLICATION_PROTOCOL_REVISION,
                    negotiated_features: application_negotiated_features(),
                },
            )
            .unwrap_or_else(|| {
                panic!(
                    "{} must resolve through the current {surface:?} catalog",
                    operation.as_str()
                )
            });
            assert_eq!(
                binding.binding_id.as_str(),
                format!("binding.{surface_name}.{}.v1", operation.as_str())
            );
            assert_eq!(binding.request_schema.revision(), 1);
            assert_eq!(binding.result_schema.revision(), 1);
        }
    }
}

#[test]
fn health_delta_has_cli_mcp_http_parity_and_one_typed_request() {
    let catalog = super::application_surface_catalog().expect("application catalog");
    let resolver = tracedecay_daemon_protocol::CatalogBindingResolver::new(&catalog);
    let profile_id = tracedecay_tool_catalog::ProfileId::new(
        tracedecay_contracts::APPLICATION_DEFAULT_PROFILE_ID,
    )
    .expect("application profile");
    for (surface, name) in [
        (tracedecay_tool_catalog::BindingSurface::Cli, "cli"),
        (tracedecay_tool_catalog::BindingSurface::Mcp, "mcp"),
        (tracedecay_tool_catalog::BindingSurface::Http, "http"),
    ] {
        let binding = tracedecay_daemon_protocol::BindingResolver::resolve_binding(
            &resolver,
            surface,
            &tracedecay_daemon_protocol::BindingResolution {
                profile_id: profile_id.clone(),
                operation: tracedecay_tool_catalog::SurfaceOperationName::new("health_delta")
                    .expect("operation"),
                protocol_revision: APPLICATION_PROTOCOL_REVISION,
                negotiated_features: application_negotiated_features(),
            },
        )
        .unwrap_or_else(|| panic!("health_delta must resolve on {surface:?}"));
        assert_eq!(
            binding.binding_id.as_str(),
            format!("binding.{name}.health_delta.v1")
        );
    }

    let parsed = parse_application_surface_request(
        ApplicationSurfaceOperation::HealthDelta,
        serde_json::json!({
            "before_cursor": "health-delta.v1.aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "path_prefix": "src",
            "meta": {
                "temporal": {"kind": "current"},
                "page": {"page_size": 10, "cursor": null},
                "projection": "summary",
                "order": "stable_identity"
            }
        }),
    )
    .expect("typed health delta request");
    assert!(matches!(
        parsed,
        ApplicationSurfaceRequest::Primitive(PrimitiveRequest::HealthDelta(_))
    ));
}

#[test]
fn git_reads_parse_the_existing_mcp_shapes_into_catalog_owned_requests() {
    let fixtures = [
        (
            ApplicationSurfaceOperation::GitStatus,
            serde_json::json!({}),
            "capability.application.git.status",
        ),
        (
            ApplicationSurfaceOperation::GitDiff,
            serde_json::json!({
                "scope": "commit_range",
                "base": "a".repeat(40),
                "head": "b".repeat(40),
            }),
            "capability.application.git.diff",
        ),
        (
            ApplicationSurfaceOperation::GitHistory,
            serde_json::json!({
                "count": 1_000,
                "path": "src/lib.rs",
                "follow": true,
                "first_parent": true,
            }),
            "capability.application.git.history",
        ),
        (
            ApplicationSurfaceOperation::GitBlame,
            serde_json::json!({"path": "src/lib.rs", "follow_renames": true}),
            "capability.application.git.blame",
        ),
        (
            ApplicationSurfaceOperation::GitHunks,
            serde_json::json!({"scope": "staged"}),
            "capability.application.git.hunks",
        ),
    ];

    for (operation, args, capability) in fixtures {
        assert_eq!(
            ApplicationSurfaceOperation::from_tool_name(&format!(
                "tracedecay_{}",
                operation.as_str()
            )),
            Some(operation)
        );
        let ApplicationSurfaceRequest::GitRead(request) =
            parse_application_surface_request(operation, args).expect("Git read request")
        else {
            panic!("Git reads must use the catalog-owned request")
        };
        assert_eq!(request.request.capability_id(), capability);
        assert_eq!(request.max_entries, 1_000);
        assert_eq!(request.max_bytes, 4 * 1024 * 1024);
    }
}

#[test]
fn git_mutation_surface_rejects_caller_minted_native_authority() {
    assert!(
        parse_application_surface_request(
            ApplicationSurfaceOperation::GitHunks,
            serde_json::json!({
                "scope": "staged",
                "preview_id": "preview.caller",
                "snapshot_digest": format!("sha256:{}", "a".repeat(64)),
            }),
        )
        .is_err()
    );
    assert!(
        parse_application_surface_request(
            ApplicationSurfaceOperation::GitPreview,
            serde_json::json!({
                "operation": "commit_index",
                "repository_snapshot": {},
                "selected_hunks": [],
                "commit_intent": null,
            }),
        )
        .is_err()
    );
    assert!(
        parse_application_surface_request(
            ApplicationSurfaceOperation::GitApply,
            serde_json::json!({
                "preview": {},
                "idempotency_key": "idempotency.caller",
            }),
        )
        .is_err()
    );
}

#[test]
fn git_read_parser_rejects_values_outside_the_catalog_schema() {
    for (operation, args) in [
        (
            ApplicationSurfaceOperation::GitStatus,
            serde_json::json!({"max_entries": 0}),
        ),
        (
            ApplicationSurfaceOperation::GitStatus,
            serde_json::json!({"max_bytes": 4_194_305}),
        ),
        (
            ApplicationSurfaceOperation::GitHistory,
            serde_json::json!({"count": 1_001}),
        ),
        (
            ApplicationSurfaceOperation::GitDiff,
            serde_json::json!({"scope": "working_tree", "base": "a".repeat(40)}),
        ),
    ] {
        assert!(matches!(
            parse_application_surface_request(operation, args),
            Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        ));
    }
}

#[tokio::test]
async fn http_git_read_routes_preserve_the_canonical_typed_request() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let owner_seen = Arc::clone(&seen);
    let app = tracedecay_api::application_router(
        move |request: tracedecay_api::HttpApplicationRequest| {
            owner_seen.lock().expect("capture Git read").push((
                request.operation,
                request.request_id.clone(),
                request.page.clone(),
                request.cancellation.clone(),
                request.body.clone(),
            ));
            async move {
                Ok::<_, ApplicationContractError>(tracedecay_api::CanonicalInvocationResult::new(
                    BindingId::new(format!("binding.http.{}.v1", request.operation.as_str()))
                        .expect("binding"),
                    Err(ApplicationProblemEnvelope::new(
                        ResultContractRef::new(
                            SchemaId::new("schema.application.git.fixture.result").expect("schema"),
                            1,
                        )
                        .expect("contract"),
                        request.request_id,
                        ApplicationProblem::unavailable(
                            SafeDiagnostic::new(
                                "git.fixture.unavailable",
                                "Fixture Git owner is unavailable",
                            )
                            .expect("diagnostic"),
                        ),
                    )
                    .expect("canonical Git fixture problem")),
                ))
            }
        },
    );

    for (index, (route, operation)) in [
        (
            "/git/status?page_size=7",
            ApplicationSurfaceOperation::GitStatus,
        ),
        (
            "/git/diff?page_size=7",
            ApplicationSurfaceOperation::GitDiff,
        ),
        (
            "/git/history?page_size=7",
            ApplicationSurfaceOperation::GitHistory,
        ),
        (
            "/git/blame?page_size=7",
            ApplicationSurfaceOperation::GitBlame,
        ),
        (
            "/git/hunks?page_size=7",
            ApplicationSurfaceOperation::GitHunks,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let request_id = RequestId::new(format!("request.http.git-read.{index}")).expect("request");
        let cancellation =
            CancellationSignal::active(format!("cancellation.http.git-read.{index}"))
                .expect("cancellation");
        let deadline = Deadline::new(UtcMicros(9_999_999)).expect("deadline");
        let response = app
            .clone()
            .oneshot(
                Request::post(route)
                    .header("content-type", "application/json")
                    .extension(request_id.clone())
                    .extension(tracedecay_api::HttpApplicationControls {
                        deadline,
                        cancellation: cancellation.clone(),
                    })
                    .body(Body::from("{}"))
                    .expect("HTTP request"),
            )
            .await
            .expect("HTTP response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        let captured = seen.lock().expect("captured Git reads");
        let (actual_operation, actual_request_id, page, actual_cancellation, body) =
            captured.last().expect("one captured Git read");
        assert_eq!(*actual_operation, operation);
        assert_eq!(actual_request_id, &request_id);
        assert_eq!(page.page_size, 7);
        assert!(page.cursor.is_none());
        assert_eq!(
            actual_cancellation.context().token_id,
            cancellation.context().token_id
        );
        assert_eq!(body, &serde_json::json!({}));
    }
}

#[test]
fn catalog_bound_compatibility_tools_resolve_before_retained_dispatch() {
    let catalog = super::application_surface_catalog().expect("application catalog");
    let mut compatibility_operations = std::collections::BTreeSet::new();

    for capability in catalog.capabilities() {
        if !capability.availability().is_callable() {
            continue;
        }
        for binding_id in capability.binding_ids() {
            let binding = catalog.binding(binding_id).expect("catalog binding");
            if !matches!(
                binding.surface(),
                tracedecay_tool_catalog::BindingSurface::Cli
                    | tracedecay_tool_catalog::BindingSurface::Mcp
            ) || ApplicationSurfaceOperation::from_tool_name(binding.operation().as_str())
                .is_some()
            {
                continue;
            }

            let tool_name = format!("tracedecay_{}", binding.operation().as_str());
            let resolved = super::resolve_catalog_tool_binding(binding.surface(), &tool_name)
                .expect("compatibility binding resolution")
                .unwrap_or_else(|| panic!("{tool_name} must resolve before retained dispatch"));
            assert_eq!(resolved.binding_id, *binding_id);
            compatibility_operations.insert(binding.operation().as_str().to_owned());
        }
    }

    assert_eq!(
        compatibility_operations,
        [
            "ast_grep_rewrite",
            "callees",
            "context",
            "fact_feedback",
            "fact_store_add",
            "fact_store_contradict",
            "fact_store_curate",
            "fact_store_get",
            "fact_store_list",
            "fact_store_probe",
            "fact_store_reason",
            "fact_store_related",
            "fact_store_remove",
            "fact_store_search",
            "fact_store_supersede",
            "fact_store_update",
            "impact",
            "insert_at",
            "insert_at_symbol",
            "lcm_describe",
            "lcm_doctor",
            "lcm_expand",
            "lcm_expand_query",
            "lcm_grep",
            "lcm_load_session",
            "lcm_status",
            "memory_status",
            "message_search",
            "move_symbol",
            "multi_str_replace",
            "node",
            "port_order",
            "port_status",
            "redundancy",
            "rename_preview",
            "rename_symbol",
            "replace_symbol",
            "session_refresh_begin",
            "session_refresh_cancel",
            "session_refresh_status",
            "sessions_for",
            "similar",
            "source_edit_reconcile",
            "source_edit_rollback",
            "str_replace",
            "todos",
            "workflows",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
}

#[test]
fn context_scout_controls_and_claims_preserve_the_exact_address() {
    let address = ContextScoutAddressV1 {
        profile_id: [1; 16],
        provider_id: [2; 16],
        protected_session_id: [3; 32],
        thread_id: [4; 16],
        turn_id: [5; 16],
        agent_id: [6; 16],
        logical_message_id: [7; 16],
        project_id: [8; 16],
    };
    let pause = parse_application_surface_request(
        ApplicationSurfaceOperation::ContextScoutPause,
        serde_json::to_value(ContextScoutControlRequestV1 {
            address,
            expected_revision: ConfigurationRevisionId::new("revision.scout.surface")
                .expect("revision"),
            idempotency_key: ConfigurationIdempotencyKey::new(
                "configuration.idempotency.scout.surface",
            )
            .expect("idempotency key"),
        })
        .expect("pause request"),
    )
    .expect("exact-address pause");
    assert!(matches!(
        pause,
        ApplicationSurfaceRequest::ContextScout(ContextScoutSurfaceRequestV1::Pause(request))
            if request.address == address
                && request.idempotency_key.as_str()
                    == "configuration.idempotency.scout.surface"
    ));

    let claim_body = serde_json::to_value(ContextScoutClaimRequestV1 {
        address,
        window: ContextScoutClaimWindowV1::IdleWindow,
        idempotency_key: tracedecay_contracts::IdempotencyKey::new("context-scout.claim.surface")
            .expect("claim key"),
    })
    .expect("claim request");
    let claim = parse_application_surface_request(
        ApplicationSurfaceOperation::ContextScoutClaim,
        claim_body.clone(),
    )
    .expect("exact-address claim");
    assert!(matches!(
        claim,
        ApplicationSurfaceRequest::ContextScout(ContextScoutSurfaceRequestV1::Claim(request))
            if request.address == address
                && request.window == ContextScoutClaimWindowV1::IdleWindow
                && request.idempotency_key.as_str() == "context-scout.claim.surface"
    ));
    assert!(matches!(
        parse_application_surface_request(
            ApplicationSurfaceOperation::ContextScoutPause,
            claim_body,
        ),
        Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
    ));

    let work = serde_json::json!({
        "address": address,
        "generation": 1,
        "input_watermark": vec![9_u8; 32],
    });
    let cancel = parse_application_surface_request(
        ApplicationSurfaceOperation::ContextScoutCancel,
        serde_json::json!({
            "address": address,
            "work": work.clone(),
            "idempotency_key": "context-scout.cancel.surface",
        }),
    )
    .expect("canonical cancel request");
    assert!(matches!(
        cancel,
        ApplicationSurfaceRequest::ContextScout(ContextScoutSurfaceRequestV1::Cancel(request))
            if request.address == address
    ));

    let receipt = serde_json::json!({
        "receipt_id": vec![10_u8; 16],
        "envelope_id": vec![11_u8; 16],
        "delivered_at": 12,
        "outcome": "displayed",
    });
    let delivery = parse_application_surface_request(
        ApplicationSurfaceOperation::ContextScoutDelivery,
        serde_json::json!({
            "address": address,
            "claim": {
                "work": work,
                "envelope_id": vec![11_u8; 16],
                "lease_id": vec![13_u8; 16],
                "lease_expires_at": 14,
            },
            "delivered_at": 12,
            "outcome": "displayed",
            "idempotency_key": "context-scout.delivery.surface",
        }),
    )
    .expect("canonical public claim-handle delivery request");
    assert!(matches!(
        delivery,
        ApplicationSurfaceRequest::ContextScout(ContextScoutSurfaceRequestV1::Delivery(request))
            if request.claim.lease_id == [13; 16]
    ));

    let feedback = parse_application_surface_request(
        ApplicationSurfaceOperation::ContextScoutFeedback,
        serde_json::json!({
            "address": address,
            "receipt": receipt,
            "feedback": {
                "receipt_id": vec![10_u8; 16],
                "kind": "explicitly_accepted",
            },
            "idempotency_key": "context-scout.feedback.surface",
        }),
    )
    .expect("canonical feedback request");
    assert!(matches!(
        feedback,
        ApplicationSurfaceRequest::ContextScout(ContextScoutSurfaceRequestV1::Feedback(request))
            if request.address == address
    ));
}

#[tokio::test]
async fn execution_rejects_a_direct_operation_binding_bypass() {
    let dispatched = resolve_application_surface_dispatch(
        tracedecay_tool_catalog::BindingSurface::Cli,
        ApplicationSurfaceOperation::FeedbackList,
        RequestId::new("request.binding-bypass").expect("request"),
        ApplicationSurfaceRequest::Feedback(
            FeedbackSurfaceRequest::new("feedback-handle.fixture".to_owned()).expect("handle"),
        ),
        RequestedOutputFormat::Json,
    )
    .expect("canonical list dispatch");

    let result = execute_application_surface(
        ApplicationSurfaceOperation::FeedbackDiagnostics,
        dispatched,
        None,
    )
    .await;
    assert!(matches!(
        result,
        Err(ApplicationSurfaceAdapterError::UnknownOrNotAuthorized)
    ));
}

#[tokio::test]
async fn feedback_get_accepts_the_daemon_returned_evidence_envelope() {
    struct ReturnedEnvelopeExecutor(ApplicationEnvelope<Value>);

    impl tracedecay_contracts::ApplicationInvocationExecutor for ReturnedEnvelopeExecutor {
        fn invoke(
            &self,
            _invocation: tracedecay_contracts::ApplicationInvocation,
        ) -> tracedecay_contracts::ApplicationInvocationFuture<
            '_,
            Result<ApplicationResponse, tracedecay_contracts::InvocationError>,
        > {
            let envelope = self.0.clone();
            Box::pin(async move { Ok(ApplicationResponse::unary(envelope)) })
        }
    }

    impl tracedecay_daemon_protocol::DaemonInvocationExecutor for ReturnedEnvelopeExecutor {
        fn invoke_controlled(
            &self,
            _request: tracedecay_daemon_protocol::DaemonInvocationRequest,
            _deadline: Deadline,
            _cancellation: CancellationSignal,
            _policy: tracedecay_daemon_protocol::InvocationCancellationPolicy,
        ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<
            '_,
            Result<
                tracedecay_daemon_protocol::DaemonInvocationResponse,
                tracedecay_daemon_protocol::DaemonInvocationError,
            >,
        > {
            Box::pin(async { unreachable!("feedback_get uses the generic application executor") })
        }

        fn observe_feedback(
            &self,
            _subject_digest: ManifestDigest,
            _observed_at: UtcMicros,
            _event: tracedecay_contracts::feedback::observations::FeedbackSourceEventV1,
        ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<
            '_,
            tracedecay_domain::errors::Result<()>,
        > {
            Box::pin(async { Ok(()) })
        }
    }

    let request_id = RequestId::new("request.feedback-get.returned-envelope").expect("request");
    let request = parse_application_surface_request(
        ApplicationSurfaceOperation::FeedbackGet,
        json!({"request_handle": "rh_cycle_issued_feedback_get"}),
    )
    .expect("canonical feedback_get request");
    let dispatched = resolve_application_surface_dispatch(
        BindingSurface::Cli,
        ApplicationSurfaceOperation::FeedbackGet,
        request_id.clone(),
        request,
        RequestedOutputFormat::Json,
    )
    .expect("canonical feedback_get dispatch");
    let envelope = serde_json::from_value::<ApplicationEnvelope<Value>>(json!({
        "contract": {
            "schema_id": "schema.application.feedback.get.result",
            "schema_revision": 1
        },
        "request_id": request_id,
        "scope": {
            "project_id": "proj_bf4c1fa0122e05e8",
            "repository_id": "repository.daemon.677b1829df61dcff9dc039b8da0afe3c7c32571f1f11012193cf855b534cb26e",
            "worktree_id": "worktree.daemon.bf4c1fa0122e05e8b1ebfb8f19fffa8143aef907e9f06048b042a12f90f74038",
            "reference": "refs/heads/master",
            "scope_digest": "sha256:fac231c1fbf06d284fa338dd404baeb8bcd8c2375f0306491129dae46a155ca4"
        },
        "outcome": {
          "outcome": "evidence",
          "value": {
            "temporal": {
                "requested_mode": {"kind": "current"},
                "requested_at": 1_789_199_880_928_342_i64,
                "resolved_at": 1_789_199_880_933_278_i64,
                "source_generation": null,
                "watermark_digest": null,
                "freshness": "current"
            },
            "authority": {
                "grant_id": "grant.daemon.feedback.rh_cycle_issued_feedback_get",
                "grant_revision": 1,
                "grant_digest": "sha256:efb35febf5bf96e698bc3b6877d30c6f2839ce0132cec4b12df21ac66d367ae1",
                "authorized_scope_digest": "sha256:fac231c1fbf06d284fa338dd404baeb8bcd8c2375f0306491129dae46a155ca4",
                "disclosure": "evidence",
                "policy": {
                    "decision_id": "route.feedback.binding.tracedecay-daemon.project-open",
                    "revision": 1,
                    "digest": "sha256:37efd56aea2ea74a69b53fd93f1159132f23cd1fd19c3aca6dc4e2cc631df5ba",
                    "evaluator_revision": "project-source-access.v1"
                },
                "revalidated_at": 1_789_199_880_933_278_i64
            },
            "evidence_authorities": [],
            "coverage": {
                "requested_domains": ["diagnostic"],
                "visited": 1,
                "eligible": 1,
                "returned": 1,
                "completeness": "complete",
                "domains": [{"domain": "diagnostic", "completeness": "complete"}]
            },
            "omissions": [],
            "scores": [],
            "contributions": [],
            "page": {
                "sort_contract_id": "sort.application.feedback.finding-id.v1",
                "sort_revision": 1,
                "total": 1,
                "returned": 1,
                "cursor": null,
                "expires_at": null
            },
            "execution": {
                "started_at": 1_789_199_880_928_342_i64,
                "ended_at": 1_789_199_880_933_278_i64,
                "effective_deadline": {"expires_at": 1_789_199_895_928_342_i64},
                "cancellation": null,
                "budget": {"units_consumed": 0, "bytes_consumed": 0, "elapsed_micros": 0},
                "termination": "completed"
            },
            "payload": {
                "finding": {
                    "result_id": "feedback.result.v1.426f528366ceb830f8dba78a42b5001d164b71e6a197386f32fada825aef60eb",
                    "cycle_id": "cycle.project-open.e2d5491610c8f424351226304c069b7030168eb2d8d176462ae9151f51f22b83",
                    "scope": {
                        "project_id": "proj_bf4c1fa0122e05e8",
                        "repository_id": "repository.daemon.677b1829df61dcff9dc039b8da0afe3c7c32571f1f11012193cf855b534cb26e",
                        "worktree_id": "worktree.daemon.bf4c1fa0122e05e8b1ebfb8f19fffa8143aef907e9f06048b042a12f90f74038",
                        "branch_ref": "refs/heads/master",
                        "head_commit_id": "3d85d0893109ce76ee3c369049033dbaa14819c4"
                    },
                    "finding": {
                        "finding_id": "feedback.finding.v1.3b322f4acdede57174617592e487c958398b4d2dd7099f9ac1a4a5fd5e87eba2",
                        "classification": "new",
                        "lifecycle": "active",
                        "retrieval_anchor_id": "anchor.diagnostic.compiler.6d52fec508284e6f6330e7e87d0486f222735e0e5ed66a825c51e0df0299658d",
                        "provider_state": "supported_completed_complete",
                        "safe_bounded_preview": "cannot find function `feedback_public_replay_missing_symbol` in this scope: not found in this scope"
                    },
                    "get_handle": "rh_returned_feedback_get",
                    "expand_handle": "rh_returned_feedback_expand"
                }
            }
          }
        }
    }))
    .expect("real daemon feedback_get envelope");

    let result = execute_application_surface(
        ApplicationSurfaceOperation::FeedbackGet,
        dispatched,
        Some(&ReturnedEnvelopeExecutor(envelope)),
    )
    .await
    .expect("feedback_get adapter invocation");
    let envelope = result
        .result
        .expect("returned feedback_get envelope must remain evidence");
    let ApplicationOutcome::Evidence(evidence) = envelope.outcome else {
        panic!("feedback_get must return evidence");
    };
    assert_eq!(
        evidence.payload.expect("feedback_get payload")["finding"]["finding"]["finding_id"],
        "feedback.finding.v1.3b322f4acdede57174617592e487c958398b4d2dd7099f9ac1a4a5fd5e87eba2"
    );
}

fn callable_code_request_body(extra: Value) -> Value {
    let mut request = serde_json::json!({
        "scope": {
            "generation": "generation.callable-surface",
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

fn callable_symbol_graph_request_body(extra: Value) -> Value {
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

#[test]
fn callable_code_operations_parse_distinct_application_requests() {
    let exact = parse_application_surface_request(
        ApplicationSurfaceOperation::CodeExactOccurrence,
        callable_code_request_body(serde_json::json!({
            "literal": "ApplicationSurfaceOperation",
            "kind": "whole_symbol"
        })),
    )
    .expect("exact occurrence request");
    assert!(matches!(
        exact,
        ApplicationSurfaceRequest::CallableCode(
            CallableCodeSurfaceRequest::ExactOccurrence(request)
        ) if request.literal == "ApplicationSurfaceOperation"
    ));

    let phrase = parse_application_surface_request(
        ApplicationSurfaceOperation::CodePhraseSearch,
        callable_code_request_body(serde_json::json!({
            "query": "callable application surface",
            "phrases": ["callable application", "surface"],
            "field_filters": [{"field": "path", "include": true}],
            "fuzzy_budget": 7
        })),
    )
    .expect("phrase search request");
    let ApplicationSurfaceRequest::CallableCode(CallableCodeSurfaceRequest::PhraseSearch(phrase)) =
        phrase
    else {
        panic!("phrase search must remain a distinct callable-code request");
    };
    let phrase = phrase
        .into_application_request(
            SanitizerRevision::new("sanitizer.surface-test.v1").expect("sanitizer revision"),
            QueryNormalizationRevision::new("normalization.surface-test.v1")
                .expect("normalization revision"),
            PageRequest::first(25).expect("page"),
        )
        .expect("validated phrase request");
    assert_eq!(phrase.query.as_str(), "callable application surface");
    assert_eq!(phrase.fuzzy_budget, 7);
    assert_eq!(
        phrase.field_filters,
        [tracedecay_contracts::retrieval::CodeLexicalFieldFilter {
            field: tracedecay_contracts::retrieval::CodeLexicalField::Path,
            include: true,
        }]
    );

    let callees = parse_application_surface_request(
        ApplicationSurfaceOperation::CodeCallees,
        callable_code_request_body(serde_json::json!({
            "node_id": "node.application-surface",
            "maximum_depth": 3,
            "resolve_trait_dispatch": true
        })),
    )
    .expect("callees request");
    assert!(matches!(
        callees,
        ApplicationSurfaceRequest::CallableCode(CallableCodeSurfaceRequest::Callees(request))
            if request.node_id == "node.application-surface"
    ));

    let facets = parse_application_surface_request(
        ApplicationSurfaceOperation::CodeFacets,
        callable_code_request_body(serde_json::json!({"dimension": "language"})),
    )
    .expect("facets request");
    assert!(matches!(
        facets,
        ApplicationSurfaceRequest::CallableCode(CallableCodeSurfaceRequest::Facets(request))
            if request.dimension == tracedecay_contracts::retrieval::CodeFacetDimension::Language
    ));

    let timeline = parse_application_surface_request(
        ApplicationSurfaceOperation::CodeTimeline,
        callable_code_request_body(serde_json::json!({})),
    )
    .expect("timeline request");
    assert!(matches!(
        timeline,
        ApplicationSurfaceRequest::CallableCode(CallableCodeSurfaceRequest::Timeline(_))
    ));

    for operation in [
        ApplicationSurfaceOperation::CodeDeclaration,
        ApplicationSurfaceOperation::CodeDefinition,
        ApplicationSurfaceOperation::CodeTypeDefinition,
        ApplicationSurfaceOperation::CodeReferences,
    ] {
        let request = parse_application_surface_request(
            operation,
            callable_code_request_body(
                serde_json::json!({"node_id": "symbol.application-surface"}),
            ),
        )
        .expect("navigation request");
        assert!(request.matches(operation));
    }
}

#[test]
fn callable_symbol_graph_operations_reuse_primitive_requests() {
    let symbol_search = parse_application_surface_request(
        ApplicationSurfaceOperation::CodeSymbolSearch,
        callable_symbol_graph_request_body(serde_json::json!({
            "query": "ApplicationSurfaceOperation",
            "lazy_index_ignored_dependencies": false
        })),
    )
    .expect("symbol search request");
    assert!(matches!(
        &symbol_search,
        ApplicationSurfaceRequest::PrimitiveCode(PrimitiveCodeSurfaceRequest::SymbolSearch(request))
            if request.query == "ApplicationSurfaceOperation"
    ));
    let ApplicationSurfaceRequest::PrimitiveCode(symbol_search) = symbol_search else {
        unreachable!("parsed symbol search uses the primitive-code adapter");
    };
    let sanitizer_revision =
        SanitizerRevision::new("sanitizer.daemon-owned-test.v1").expect("sanitizer revision");
    let normalization_revision =
        QueryNormalizationRevision::new("normalization.daemon-owned-test.v1")
            .expect("normalization revision");
    let PrimitiveRequest::SymbolSearch(symbol_search) =
        tracedecay_contracts::primitive_code_into_primitive(
            symbol_search,
            sanitizer_revision.clone(),
            normalization_revision.clone(),
            PageRequest::first(25).expect("page"),
        )
        .expect("daemon revisions create the primitive request")
    else {
        unreachable!("symbol search preserves its primitive kind");
    };
    assert_eq!(
        symbol_search.query.sanitizer_revision(),
        &sanitizer_revision
    );
    assert_eq!(
        symbol_search.query.normalization_revision(),
        &normalization_revision
    );

    let signature_search = parse_application_surface_request(
        ApplicationSurfaceOperation::CodeSignatureSearch,
        callable_symbol_graph_request_body(serde_json::json!({
            "returns": "ApplicationResult",
            "params": ["RequestContext"],
            "is_async": true
        })),
    )
    .expect("signature search request");
    assert!(matches!(
        signature_search,
        ApplicationSurfaceRequest::PrimitiveCode(PrimitiveCodeSurfaceRequest::SignatureSearch(request))
            if request.returns.as_deref() == Some("ApplicationResult")
    ));

    let implementations = parse_application_surface_request(
        ApplicationSurfaceOperation::CodeImplementations,
        callable_symbol_graph_request_body(serde_json::json!({
            "selector": {"selector": "trait", "name": "HttpApplicationOwners"}
        })),
    )
    .expect("implementations request");
    assert!(matches!(
        implementations,
        ApplicationSurfaceRequest::PrimitiveCode(PrimitiveCodeSurfaceRequest::Implementations(_))
    ));

    let type_hierarchy = parse_application_surface_request(
        ApplicationSurfaceOperation::CodeTypeHierarchy,
        callable_symbol_graph_request_body(serde_json::json!({
            "node_id": "node.application-surface",
            "maximum_depth": 3
        })),
    )
    .expect("type hierarchy request");
    assert!(matches!(
        type_hierarchy,
        ApplicationSurfaceRequest::PrimitiveCode(PrimitiveCodeSurfaceRequest::TypeHierarchy(request))
            if request.node_id == "node.application-surface"
    ));

    let callers = parse_application_surface_request(
        ApplicationSurfaceOperation::CodeCallers,
        callable_symbol_graph_request_body(serde_json::json!({
            "node_id": "node.application-surface",
            "maximum_depth": 3
        })),
    )
    .expect("callers request");
    assert!(matches!(
        callers,
        ApplicationSurfaceRequest::PrimitiveCode(PrimitiveCodeSurfaceRequest::Callers(request))
            if request.node_id == "node.application-surface"
    ));
}

#[test]
fn feedback_cycle_projections_require_the_canonical_handle() {
    for operation in [
        ApplicationSurfaceOperation::FeedbackImpact,
        ApplicationSurfaceOperation::AffectedTests,
    ] {
        let request = parse_application_surface_request(
            operation,
            serde_json::json!({"request_handle": "rh_feedback-cycle.fixture"}),
        )
        .expect("canonical feedback-cycle request");
        // Impact and affected-tests reads are handle-addressed feedback reads:
        // the operation selects the daemon route, the body is the one handle
        // request every other feedback read decodes into.
        assert!(request.matches(operation));
        match request {
            ApplicationSurfaceRequest::Feedback(request) => {
                assert_eq!(request.request_handle, "rh_feedback-cycle.fixture");
            }
            other => panic!("unexpected feedback-cycle request: {other:?}"),
        }

        assert!(matches!(
            parse_application_surface_request(operation, serde_json::json!({"node_id": "node"})),
            Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        ));
        assert!(matches!(
            parse_application_surface_request(
                operation,
                serde_json::json!({"files": ["src/lib.rs"]})
            ),
            Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
        ));
        assert!(matches!(
            parse_application_surface_request(
                operation,
                serde_json::json!({"request_handle": " invalid"})
            ),
            Err(ApplicationSurfaceAdapterError::InvalidRequestHandle)
        ));
    }
}

#[test]
fn explicit_feedback_cycle_accepts_only_a_document_uri() {
    let request = parse_application_surface_request(
        ApplicationSurfaceOperation::FeedbackAdvisoryCycle,
        serde_json::json!({"document_uri": "file:///project/src/lib.rs"}),
    )
    .expect("explicit feedback-cycle request");
    assert!(matches!(
        request,
        ApplicationSurfaceRequest::FeedbackAdvisoryCycle(request)
            if request.document_uri == "file:///project/src/lib.rs"
    ));

    assert!(matches!(
        parse_application_surface_request(
            ApplicationSurfaceOperation::FeedbackAdvisoryCycle,
            serde_json::json!({
                "document_uri": "file:///project/src/lib.rs",
                "request_handle": "rh.client.selected"
            }),
        ),
        Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
    ));
}

#[test]
fn callable_code_page_is_transport_owned() {
    let rejected = parse_application_surface_request(
        ApplicationSurfaceOperation::CodeExactOccurrence,
        callable_code_request_body(serde_json::json!({
            "literal": "ApplicationSurfaceOperation",
            "kind": "whole_symbol",
            "meta": {
                "projection": "evidence",
                "order": "source_position",
                "page": { "page_size": 25, "cursor": null }
            }
        })),
    );
    assert!(matches!(
        rejected,
        Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
    ));

    let rejected = parse_application_surface_request(
        ApplicationSurfaceOperation::CodeSymbolSearch,
        callable_symbol_graph_request_body(serde_json::json!({
            "query": "ApplicationSurfaceOperation",
            "lazy_index_ignored_dependencies": false,
            "meta": {
                "projection": "evidence",
                "order": "source_position",
                "page": { "page_size": 25, "cursor": null }
            }
        })),
    );
    assert!(matches!(
        rejected,
        Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
    ));
}

#[test]
fn callable_code_operation_names_are_exact_and_not_primitive_aliases() {
    for (operation, name) in [
        (
            ApplicationSurfaceOperation::CodeExactOccurrence,
            "code_exact_occurrence",
        ),
        (
            ApplicationSurfaceOperation::CodePhraseSearch,
            "code_phrase_search",
        ),
        (
            ApplicationSurfaceOperation::CodeSymbolSearch,
            "code_symbol_search",
        ),
        (
            ApplicationSurfaceOperation::CodeSignatureSearch,
            "code_signature_search",
        ),
        (
            ApplicationSurfaceOperation::CodeImplementations,
            "code_implementations",
        ),
        (
            ApplicationSurfaceOperation::CodeTypeHierarchy,
            "code_type_hierarchy",
        ),
        (ApplicationSurfaceOperation::CodeCallers, "code_callers"),
        (ApplicationSurfaceOperation::CodeCallees, "code_callees"),
        (ApplicationSurfaceOperation::CodeFacets, "code_facets"),
        (ApplicationSurfaceOperation::CodeTimeline, "code_timeline"),
        (
            ApplicationSurfaceOperation::CodeDeclaration,
            "code_declaration",
        ),
        (
            ApplicationSurfaceOperation::CodeDefinition,
            "code_definition",
        ),
        (
            ApplicationSurfaceOperation::CodeTypeDefinition,
            "code_type_definition",
        ),
        (
            ApplicationSurfaceOperation::CodeReferences,
            "code_references",
        ),
    ] {
        assert_eq!(operation.as_str(), name);
        assert_eq!(
            ApplicationSurfaceOperation::from_tool_name(&format!("tracedecay_{name}")),
            Some(operation)
        );
    }
    for primitive_alias in [
        "exact_occurrence",
        "phrase_search",
        "symbol_search",
        "signature_search",
        "implementations",
        "type_hierarchy",
        "callers",
        "callees",
        "facets",
        "timeline",
        "declaration",
        "definition",
        "type_definition",
        "references",
    ] {
        assert_eq!(
            ApplicationSurfaceOperation::from_tool_name(primitive_alias),
            None
        );
    }
}

#[test]
fn dropped_http_request_unregisters_without_cancelling_work() {
    let request_id = RequestId::new("request.http.disconnect").expect("request");
    let cancellation = CancellationSignal::active("cancel.http.disconnect").expect("cancellation");
    let registry: HttpCancellationRegistry = Arc::default();
    drop(
        ActiveHttpRequest::register(
            Arc::clone(&registry),
            request_id.clone(),
            cancellation.clone(),
        )
        .expect("active request"),
    );

    assert!(!cancellation.is_cancelled());
    assert!(!registry.lock().expect("registry").contains_key(&request_id));
}

fn open_resume_token(body: &str) -> String {
    body.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .find(|value| value["event"] == "open")
        .and_then(|value| {
            value["data"]["frontier"]["resume_token"]
                .as_str()
                .map(str::to_owned)
        })
        .expect("open event has a real resume token")
}

fn sse_event_count(body: &str, event: &str) -> usize {
    body.lines()
        .filter_map(|line| line.strip_prefix("event:"))
        .filter(|name| name.trim() == event)
        .count()
}

fn completed_receipt(context: &RequestContext) -> OperationReceipt {
    let started_at = current_micros().expect("current time");
    OperationReceipt::completed(
        started_at,
        UtcMicros(started_at.0.saturating_add(1)),
        context.deadline().clone(),
        OperationBudgetUsage::default(),
    )
    .expect("completed receipt")
}

#[tokio::test]
async fn authenticated_context_reuses_exact_scope_and_transport_controls() {
    let project_id = ProjectId::new("project.http-adapter").expect("project");
    let authority = OperationEventAuthority::default();
    let original = operation_context(&project_id);
    let operation_id = OperationId::from_request(original.request_id().clone());
    let _emitter = authority
        .begin(
            &original,
            OperationKind::GitPreview,
            current_micros().expect("current time"),
        )
        .await
        .expect("begin operation");
    let state = HttpOperationEventState {
        authority,
        active_project_id: project_id,
        cancellations: Arc::default(),
        executor: None,
    };
    let observed_at = current_micros().expect("current time");
    let request_id = RequestId::new("request.http.subscription").expect("HTTP request");
    let cancellation =
        CancellationContext::active("cancel.http.subscription").expect("HTTP cancellation");
    let deadline =
        Deadline::new(UtcMicros(observed_at.0.saturating_add(7_000_000))).expect("HTTP deadline");

    let resolved = resolve_authenticated_http_request_context(
        &state,
        &operation_id,
        request_id.clone(),
        deadline.clone(),
        cancellation.clone(),
        observed_at,
        None,
    )
    .await
    .expect("resolved context");

    assert_eq!(resolved.actor(), original.actor());
    assert_eq!(resolved.scope(), original.scope());
    assert_eq!(
        resolved.grant().allowed_capabilities,
        original.grant().allowed_capabilities
    );
    assert_eq!(
        resolved.grant().allowed_use_cases,
        original.grant().allowed_use_cases
    );
    assert_eq!(resolved.request_id(), &request_id);
    assert_eq!(resolved.cancellation(), &cancellation);
    assert_eq!(resolved.deadline(), &deadline);
}

#[tokio::test]
async fn sse_disconnect_does_not_cancel_but_explicit_cancel_does() {
    let project_id = ProjectId::new("project.http-adapter").expect("project");
    let authority = OperationEventAuthority::default();
    let context = operation_context(&project_id);
    let operation_id = OperationId::from_request(context.request_id().clone());
    let emitter = authority
        .begin(
            &context,
            OperationKind::GitPreview,
            current_micros().expect("current time"),
        )
        .await
        .expect("begin operation");
    let app = http_operation_event_router(authority, project_id, Arc::default(), None);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/operations/{operation_id}/events?next_sequence=0"))
                .body(Body::empty())
                .expect("SSE request"),
        )
        .await
        .expect("SSE response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    drop(response);
    assert!(!emitter.is_cancelled());

    let cancelled = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/operations/{operation_id}/cancel"))
                .body(Body::empty())
                .expect("cancel request"),
        )
        .await
        .expect("cancel response");
    assert_eq!(cancelled.status(), StatusCode::ACCEPTED);
    assert!(emitter.is_cancelled());
}

#[tokio::test]
async fn sse_last_event_id_resumes_after_the_delivered_event() {
    let project_id = ProjectId::new("project.http-adapter").expect("project");
    let authority = OperationEventAuthority::default();
    let context = operation_context(&project_id);
    let operation_id = OperationId::from_request(context.request_id().clone());
    let emitter = authority
        .begin(
            &context,
            OperationKind::GitPreview,
            current_micros().expect("current time"),
        )
        .await
        .expect("begin operation");
    let app = http_operation_event_router(authority, project_id, Arc::default(), None);
    let initial = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/operations/{operation_id}/events"))
                .body(Body::empty())
                .expect("initial SSE request"),
        )
        .await
        .expect("initial SSE response");

    emitter
        .progress(1, Some(1))
        .await
        .expect("publish progress");
    emitter
        .terminal(completed_receipt(&context))
        .await
        .expect("publish terminal");
    let resume_token = open_resume_token(&response_text(initial).await);

    let resumed = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/operations/{operation_id}/events?resume_token={resume_token}"
                ))
                .header("last-event-id", "0")
                .body(Body::empty())
                .expect("resumed SSE request"),
        )
        .await
        .expect("resumed SSE response");

    assert_eq!(resumed.status(), StatusCode::OK);
    let body = response_text(resumed).await;
    assert!(!body.contains("id: 0"));
    assert!(body.contains("id: 1"));
    assert!(body.contains("event: completed"));
}

#[tokio::test]
async fn sse_malformed_or_overflowing_last_event_id_is_rejected() {
    for last_event_id in ["not-a-sequence", "-1", "18446744073709551615"] {
        let response = http_operation_event_router(
            OperationEventAuthority::default(),
            ProjectId::new("project.http-adapter").expect("project"),
            Arc::default(),
            None,
        )
        .oneshot(
            Request::builder()
                .uri("/operations/request.http-adapter/events")
                .header("last-event-id", last_event_id)
                .body(Body::empty())
                .expect("SSE request"),
        )
        .await
        .expect("SSE response");

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{last_event_id}"
        );
    }
}

#[tokio::test]
async fn sse_conflicting_explicit_cursor_and_last_event_id_is_rejected() {
    let response = http_operation_event_router(
        OperationEventAuthority::default(),
        ProjectId::new("project.http-adapter").expect("project"),
        Arc::default(),
        None,
    )
    .oneshot(
        Request::builder()
            .uri("/operations/request.http-adapter/events?next_sequence=43")
            .header("last-event-id", "41")
            .body(Body::empty())
            .expect("SSE request"),
    )
    .await
    .expect("SSE response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn sse_scope_denial_is_concealed_at_the_active_project_mount() {
    let operation_project = ProjectId::new("project.http-adapter").expect("project");
    let authority = OperationEventAuthority::default();
    let context = operation_context(&operation_project);
    let operation_id = OperationId::from_request(context.request_id().clone());
    let _emitter = authority
        .begin(
            &context,
            OperationKind::GitPreview,
            current_micros().expect("current time"),
        )
        .await
        .expect("begin operation");
    let app = http_operation_event_router(
        authority,
        ProjectId::new("project.other").expect("other project"),
        Arc::default(),
        None,
    );

    let denied = app
        .oneshot(
            Request::builder()
                .uri(format!("/operations/{operation_id}/events"))
                .body(Body::empty())
                .expect("denied request"),
        )
        .await
        .expect("denied response");
    assert_eq!(denied.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn resolver_conceals_cross_project_scope_with_one_typed_denial() {
    let operation_project = ProjectId::new("project.http-adapter").expect("project");
    let authority = OperationEventAuthority::default();
    let context = operation_context(&operation_project);
    let operation_id = OperationId::from_request(context.request_id().clone());
    let _emitter = authority
        .begin(
            &context,
            OperationKind::GitPreview,
            current_micros().expect("current time"),
        )
        .await
        .expect("begin operation");
    let state = HttpOperationEventState {
        authority,
        active_project_id: ProjectId::new("project.other").expect("other project"),
        cancellations: Arc::default(),
        executor: None,
    };

    let denied = resolve_authenticated_http_request_context(
        &state,
        &operation_id,
        RequestId::new("request.http.denied").expect("request"),
        context.deadline().clone(),
        CancellationContext::active("cancel.http.denied").expect("cancellation"),
        current_micros().expect("current time"),
        None,
    )
    .await;

    assert_eq!(
        denied.expect_err("cross-project scope must be concealed"),
        OperationEventError::NotFoundOrNotAuthorized
    );
}

/// A dead daemon socket must be a fail-fast dispatch error carrying the
/// typed connect diagnostic — never a retryable problem envelope. Wrapping
/// it as retryable made the CLI re-dispatch (re-paying the 8 s connect
/// grace each pass) until its 120 s deadline: 131 s measured for
/// `storage_status` against a dead socket while sibling compatibility tools
/// failed typed in ~9 s.
#[tokio::test]
async fn dead_daemon_surface_dispatch_fails_fast_with_typed_unreachable() {
    struct UnreachableExecutor;

    impl tracedecay_contracts::ApplicationInvocationExecutor for UnreachableExecutor {
        fn invoke(
            &self,
            _invocation: tracedecay_contracts::ApplicationInvocation,
        ) -> tracedecay_contracts::ApplicationInvocationFuture<
            '_,
            Result<
                tracedecay_contracts::ApplicationResponse,
                tracedecay_contracts::InvocationError,
            >,
        > {
            Box::pin(async {
                Err(tracedecay_contracts::InvocationError::Unreachable {
                    reason_code: "daemon_connect_down".to_owned(),
                    detail: "could not connect to TraceDecay daemon endpoint 'unix:///dead.sock'"
                        .to_owned(),
                })
            })
        }
    }

    impl tracedecay_daemon_protocol::DaemonInvocationExecutor for UnreachableExecutor {
        fn invoke_controlled(
            &self,
            _request: tracedecay_daemon_protocol::DaemonInvocationRequest,
            _deadline: Deadline,
            _cancellation: CancellationSignal,
            _policy: tracedecay_daemon_protocol::InvocationCancellationPolicy,
        ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<
            '_,
            Result<
                tracedecay_daemon_protocol::DaemonInvocationResponse,
                tracedecay_daemon_protocol::DaemonInvocationError,
            >,
        > {
            Box::pin(async {
                Err(
                    tracedecay_daemon_protocol::DaemonInvocationError::Unreachable {
                        reason_code: "daemon_connect_down".to_owned(),
                        detail:
                            "could not connect to TraceDecay daemon endpoint 'unix:///dead.sock'"
                                .to_owned(),
                    },
                )
            })
        }

        fn observe_feedback(
            &self,
            _subject_digest: ManifestDigest,
            _observed_at: UtcMicros,
            _event: tracedecay_contracts::feedback::observations::FeedbackSourceEventV1,
        ) -> tracedecay_daemon_protocol::DaemonInvocationExecutorFuture<
            '_,
            tracedecay_domain::errors::Result<()>,
        > {
            Box::pin(async { Ok(()) })
        }
    }

    let request = parse_application_surface_request(
        ApplicationSurfaceOperation::StorageStatus,
        serde_json::json!({}),
    )
    .expect("storage-status request");
    let dispatched = resolve_application_surface_dispatch(
        tracedecay_tool_catalog::BindingSurface::Cli,
        ApplicationSurfaceOperation::StorageStatus,
        RequestId::new("request.dead-daemon-storage-status").expect("request id"),
        request,
        RequestedOutputFormat::Json,
    )
    .expect("storage-status dispatch");

    let result = execute_application_surface(
        ApplicationSurfaceOperation::StorageStatus,
        dispatched,
        Some(&UnreachableExecutor),
    )
    .await;
    match result {
        Err(ApplicationSurfaceAdapterError::DaemonUnreachable {
            reason_code,
            detail,
        }) => {
            assert_eq!(reason_code, "daemon_connect_down");
            assert!(
                detail.contains("could not connect"),
                "the connect diagnostic must survive to the dispatcher: {detail}"
            );
        }
        Err(other) => panic!(
            "a dead daemon must be a fail-fast typed dispatch error, not a problem envelope: {other:?}"
        ),
        Ok(_) => panic!("a dead daemon must be a fail-fast typed dispatch error, not a success"),
    }
}

#[tokio::test]
async fn sse_resume_replays_retained_history_with_one_terminal_receipt() {
    let project_id = ProjectId::new("project.http-adapter").expect("project");
    let authority = OperationEventAuthority::new(OperationStreamConfig {
        retained_event_capacity: 2,
        max_operations: 8,
        max_subscribers_per_operation: 2,
    })
    .expect("operation authority");
    let context = operation_context(&project_id);
    let operation_id = OperationId::from_request(context.request_id().clone());
    let emitter = authority
        .begin(
            &context,
            OperationKind::GitPreview,
            current_micros().expect("current time"),
        )
        .await
        .expect("begin operation");
    let app = http_operation_event_router(authority, project_id, Arc::default(), None);

    let slow_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/operations/{operation_id}/events"))
                .body(Body::empty())
                .expect("initial SSE request"),
        )
        .await
        .expect("initial SSE response");
    assert_eq!(slow_response.status(), StatusCode::OK);

    for completed in 1..=4 {
        emitter
            .progress(completed, Some(4))
            .await
            .expect("publish progress");
    }
    let receipt = completed_receipt(&context);
    let terminal = emitter
        .terminal(receipt.clone())
        .await
        .expect("publish terminal");
    assert_eq!(
        emitter
            .terminal(receipt)
            .await
            .expect("idempotent terminal"),
        terminal
    );

    let slow_body = response_text(slow_response).await;
    let resume_token = open_resume_token(&slow_body);
    assert_eq!(sse_event_count(&slow_body, "resume_gap"), 1);
    assert_eq!(sse_event_count(&slow_body, "completed"), 1);

    let tokenless = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/operations/{operation_id}/events?next_sequence=3"))
                .body(Body::empty())
                .expect("tokenless resume request"),
        )
        .await
        .expect("tokenless resume response");
    assert_eq!(tokenless.status(), StatusCode::CONFLICT);

    let resumed = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/operations/{operation_id}/events?next_sequence=3&resume_token={resume_token}"
                ))
                .body(Body::empty())
                .expect("resume SSE request"),
        )
        .await
        .expect("resume SSE response");
    assert_eq!(resumed.status(), StatusCode::OK);
    let resumed_body = response_text(resumed).await;
    assert_eq!(sse_event_count(&resumed_body, "resume_gap"), 1);
    assert!(resumed_body.contains("\"first_missing_sequence\":3"));
    assert!(resumed_body.contains("\"last_missing_sequence\":3"));
    assert_eq!(sse_event_count(&resumed_body, "completed"), 1);
}

#[tokio::test]
async fn sse_resume_after_memory_restart_returns_canonical_expired_problem() {
    let project_id = ProjectId::new("project.http-adapter").expect("project");
    let authority = OperationEventAuthority::default();
    let context = operation_context(&project_id);
    let operation_id = OperationId::from_request(context.request_id().clone());
    let emitter = authority
        .begin(
            &context,
            OperationKind::GitPreview,
            current_micros().expect("current time"),
        )
        .await
        .expect("begin operation");
    emitter
        .terminal(completed_receipt(&context))
        .await
        .expect("publish terminal");
    let live_app = http_operation_event_router(authority, project_id.clone(), Arc::default(), None);
    let initial = live_app
        .oneshot(
            Request::builder()
                .uri(format!("/operations/{operation_id}/events"))
                .body(Body::empty())
                .expect("initial SSE request"),
        )
        .await
        .expect("initial SSE response");
    let resume_token = open_resume_token(&response_text(initial).await);

    let restarted_app = http_operation_event_router(
        OperationEventAuthority::default(),
        project_id,
        Arc::default(),
        None,
    );
    let expired = restarted_app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/operations/{operation_id}/events?next_sequence=1&resume_token={resume_token}"
                ))
                .body(Body::empty())
                .expect("expired resume request"),
        )
        .await
        .expect("expired resume response");

    assert_eq!(expired.status(), StatusCode::CONFLICT);
    let problem =
        serde_json::from_str::<Value>(&response_text(expired).await).expect("expired problem JSON");
    assert_eq!(problem["kind"], "problem");
    assert_eq!(problem["value"]["problem"]["kind"], "stale");
    assert_eq!(problem["value"]["problem"]["revision"], 1);
    assert_eq!(problem["value"]["problem"]["owning_layer"], "runtime");
    assert_eq!(problem["value"]["problem"]["terminality"], "pre_admission");
    assert_eq!(problem["value"]["problem"]["retry"], "after_revalidate");
    assert_eq!(problem["value"]["problem"]["retry_scope"], "fresh_request");
    assert_eq!(
        problem["value"]["problem"]["request_id"],
        problem["value"]["problem"]["trace_id"]
    );
    assert_eq!(
        problem["value"]["problem"]["code"],
        "operation_event.resume_expired"
    );
}

#[test]
fn surface_rejection_metadata_distinguishes_invalid_input_from_authorization() {
    assert_eq!(
        surface_rejection_metadata(&ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
        Some((
            FeedbackRejectedArgumentV1::RequestBody,
            FeedbackArgumentRejectionClassV1::InvalidShape,
            FeedbackOutcomeV1::Rejected,
        ))
    );
    assert_eq!(
        surface_rejection_metadata(&ApplicationSurfaceAdapterError::UnknownOrNotAuthorized),
        Some((
            FeedbackRejectedArgumentV1::Operation,
            FeedbackArgumentRejectionClassV1::Unauthorized,
            FeedbackOutcomeV1::Denied,
        ))
    );
    assert_eq!(
        surface_rejection_metadata(&ApplicationSurfaceAdapterError::DaemonUnavailable),
        None
    );
}

#[test]
fn diagnostics_public_name_adapts_the_shipped_flat_request() {
    assert_eq!(
        ApplicationSurfaceOperation::from_tool_name("tracedecay_diagnostics"),
        Some(ApplicationSurfaceOperation::DiagnosticsRead)
    );
    assert_eq!(
        ApplicationSurfaceOperation::from_tool_name("tracedecay_diagnostics_read"),
        None
    );
    let canonical = json!({
        "scope": {"file": "src/lib.rs"},
        "maximum_diagnostics": 25,
        "cursor": "opaque",
    });
    let canonical_request = parse_application_surface_request(
        ApplicationSurfaceOperation::DiagnosticsRead,
        canonical.clone(),
    )
    .expect("canonical application request");
    let separated = adapt_application_tool_request(
        "tracedecay_diagnostics",
        json!({
            "scope": "file",
            "path": "src/lib.rs",
            "maximum_diagnostics": 25,
            "cursor": "opaque",
            "format": "json"
        }),
    )
    .expect("shipped flat MCP/CLI request");
    assert_eq!(separated.request, canonical);
    let edge_request = parse_application_surface_request(
        ApplicationSurfaceOperation::DiagnosticsRead,
        separated.request,
    );
    assert_eq!(
        serde_json::to_value(edge_request.expect("adapted canonical request")).unwrap(),
        serde_json::to_value(canonical_request).unwrap()
    );
    assert!(
        adapt_application_tool_request("tracedecay_diagnostics", json!({"scope": "package"}))
            .is_err()
    );

    let page = PageRequest::new(25, Some(OpaqueCursor::new("opaque-http").expect("cursor")))
        .expect("page");
    assert_eq!(
        super::apply_http_page_to_surface_body(
            ApplicationSurfaceOperation::DiagnosticsRead,
            json!({
                "scope": "workspace",
                "maximum_diagnostics": 999,
                "cursor": "body-cursor"
            }),
            &page,
        ),
        json!({
            "scope": "workspace",
            "maximum_diagnostics": 25,
            "cursor": "opaque-http"
        })
    );

    let omitted_mcp =
        adapt_application_tool_request("tracedecay_diagnostics", json!({"scope": "workspace"}))
            .expect("omitted MCP page controls");
    let omitted_mcp = parse_application_surface_request(
        ApplicationSurfaceOperation::DiagnosticsRead,
        omitted_mcp.request,
    )
    .expect("canonical MCP request");
    let omitted_http = parse_http_application_surface_request(
        ApplicationSurfaceOperation::DiagnosticsRead,
        json!({"scope": "workspace"}),
        &PageRequest::first(
            tracedecay_contracts::application_operation_default_page_size(
                ApplicationSurfaceOperation::DiagnosticsRead,
            ),
        )
        .expect("HTTP page"),
    )
    .expect("canonical HTTP request");
    assert_eq!(
        serde_json::to_value(&omitted_http).expect("HTTP diagnostics request"),
        serde_json::to_value(&omitted_mcp).expect("MCP diagnostics request")
    );
    let ApplicationSurfaceRequest::Primitive(PrimitiveRequest::DiagnosticsRead(request)) =
        omitted_http
    else {
        panic!("HTTP omission must decode to diagnostics read");
    };
    assert_eq!(request.maximum_diagnostics, 1_000);
    assert!(request.cursor.is_none());
}
