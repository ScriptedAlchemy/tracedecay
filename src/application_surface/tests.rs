use std::collections::BTreeSet;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;
use tracedecay_application::{
    ApplicationProblem, ApplicationProblemEnvelope, CancellationContext, CancellationSignal,
    CancellationState, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
    OpaqueCursor, OperationBudgetUsage, OperationReceipt, PageRequest, RequestContext, RequestId,
    ResolvedScope, ResultContractRef, SafeDiagnostic, StreamEvent,
};
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_domain::{
    ActorId, ManifestDigest, ProjectId, QueryNormalizationRevision, RefId, RepositoryId,
    SanitizerRevision, UtcMicros, WorktreeId,
};
use tracedecay_tool_catalog::{BindingId, CapabilityId, SchemaId, UseCaseId};

use super::{
    APPLICATION_PROTOCOL_REVISION, APPLICATION_SURFACE_OPERATIONS, ApplicationSurfaceAdapterError,
    ApplicationSurfaceOperation, ApplicationSurfaceRequest, CallableCodeSurfaceRequest,
    ConfigurationListSurfaceRequest, ConfigurationSurfaceRequest, ContextScoutClaimSurfaceRequest,
    ContextScoutClaimWindowSurfaceV1, ContextScoutControlSurfaceRequest,
    ContextScoutSurfaceRequest, FeedbackSurfaceRequest, HttpCancellationRegistry,
    HttpDisconnectCancellation, HttpOperationEventState, PrimitiveCodeSurfaceRequest,
    application_negotiated_features, application_surface_dispatch_input_with_controls,
    current_micros, execute_application_surface, http_operation_event_router,
    normalize_application_tool_args, parse_application_surface_request, plan26_sse_stream_event,
    resolve_application_surface_dispatch, resolve_authenticated_http_request_context,
    surface_rejection_metadata,
};
use crate::application::feedback::observations::{
    Plan26ArgumentRejectionClassV1, Plan26FeedbackOutcomeV1, Plan26RejectedArgumentV1,
};
use crate::application::operation_stream::{
    OperationEventAuthority, OperationEventError, OperationId, OperationKind, OperationStreamConfig,
};
use crate::application::primitives::{Pr12PrimitiveRequest, StorageStatusPrimitiveRequest};
use crate::daemon_client::RequestedOutputFormat;

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

#[test]
fn every_configuration_operation_enters_the_canonical_dispatch_catalog() {
    let catalog = super::application_surface_catalog().expect("application catalog");
    let resolver = crate::daemon_client::CatalogBindingResolver::new(&catalog);
    let profile_id = tracedecay_tool_catalog::ProfileId::new(
        tracedecay_application::APPLICATION_DEFAULT_PROFILE_ID,
    )
    .expect("application profile");

    for name in tracedecay_application::configuration::CONFIGURATION_SURFACE_OPERATION_NAMES {
        let operation = ApplicationSurfaceOperation::from_tool_name(name)
            .unwrap_or_else(|| panic!("{name} must be a canonical surface operation"));
        assert_eq!(operation.as_str(), name);
        for surface in [
            tracedecay_tool_catalog::BindingSurface::Cli,
            tracedecay_tool_catalog::BindingSurface::Mcp,
            tracedecay_tool_catalog::BindingSurface::Http,
        ] {
            assert!(
                crate::daemon_client::BindingResolver::resolve_binding(
                    &resolver,
                    surface,
                    &crate::daemon_client::BindingResolution {
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
        ApplicationSurfaceRequest::Configuration(ConfigurationSurfaceRequest::List(
            ConfigurationListSurfaceRequest::default(),
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
    let resolver = crate::daemon_client::CatalogBindingResolver::new(&catalog);
    let profile_id = tracedecay_tool_catalog::ProfileId::new(
        tracedecay_application::APPLICATION_DEFAULT_PROFILE_ID,
    )
    .expect("application profile");

    for operation in APPLICATION_SURFACE_OPERATIONS {
        let operation_name = tracedecay_tool_catalog::SurfaceOperationName::new(operation.as_str())
            .expect("operation name");
        let expected_surfaces = if matches!(
            operation,
            ApplicationSurfaceOperation::GitPreview | ApplicationSurfaceOperation::GitApply
        ) {
            &[
                (tracedecay_tool_catalog::BindingSurface::Cli, "cli"),
                (tracedecay_tool_catalog::BindingSurface::Mcp, "mcp"),
            ][..]
        } else {
            &[
                (tracedecay_tool_catalog::BindingSurface::Cli, "cli"),
                (tracedecay_tool_catalog::BindingSurface::Mcp, "mcp"),
                (tracedecay_tool_catalog::BindingSurface::Http, "http"),
            ][..]
        };
        for &(surface, surface_name) in expected_surfaces {
            let binding = crate::daemon_client::BindingResolver::resolve_binding(
                &resolver,
                surface,
                &crate::daemon_client::BindingResolution {
                    profile_id: profile_id.clone(),
                    operation: operation_name.clone(),
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
fn root_surface_operation_authority_is_the_http_catalog_authority() {
    assert_eq!(
        APPLICATION_SURFACE_OPERATIONS,
        tracedecay_api::HttpApplicationOperation::ALL
    );
    assert_eq!(
        ApplicationSurfaceOperation::from_tool_name("tracedecay_git_preview"),
        Some(ApplicationSurfaceOperation::GitPreview)
    );
    assert_eq!(
        ApplicationSurfaceOperation::from_tool_name("tracedecay_git_apply"),
        Some(ApplicationSurfaceOperation::GitApply)
    );
}

#[test]
fn health_delta_has_cli_mcp_http_parity_and_one_typed_request() {
    let catalog = super::application_surface_catalog().expect("application catalog");
    let resolver = crate::daemon_client::CatalogBindingResolver::new(&catalog);
    let profile_id = tracedecay_tool_catalog::ProfileId::new(
        tracedecay_application::APPLICATION_DEFAULT_PROFILE_ID,
    )
    .expect("application profile");
    for (surface, name) in [
        (tracedecay_tool_catalog::BindingSurface::Cli, "cli"),
        (tracedecay_tool_catalog::BindingSurface::Mcp, "mcp"),
        (tracedecay_tool_catalog::BindingSurface::Http, "http"),
    ] {
        let binding = crate::daemon_client::BindingResolver::resolve_binding(
            &resolver,
            surface,
            &crate::daemon_client::BindingResolution {
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
        ApplicationSurfaceRequest::Primitive(Pr12PrimitiveRequest::HealthDelta(_))
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
            serde_json::json!({
                "scope": "staged",
                "preview_id": "preview.catalog-owned",
                "snapshot_digest": format!("sha256:{}", "c".repeat(64)),
            }),
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
                tracedecay_api::CanonicalInvocationResult::new(
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
                    )),
                )
            }
        },
    );

    for (index, (route, operation)) in [
        (
            "/git/status?page_size=7",
            tracedecay_api::HttpApplicationOperation::GitStatus,
        ),
        (
            "/git/diff?page_size=7",
            tracedecay_api::HttpApplicationOperation::GitDiff,
        ),
        (
            "/git/history?page_size=7",
            tracedecay_api::HttpApplicationOperation::GitHistory,
        ),
        (
            "/git/blame?page_size=7",
            tracedecay_api::HttpApplicationOperation::GitBlame,
        ),
        (
            "/git/hunks?page_size=7",
            tracedecay_api::HttpApplicationOperation::GitHunks,
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
    let mut resolved_bindings = 0;

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
            resolved_bindings += 1;
        }
    }

    assert_eq!(resolved_bindings, 58);
    assert_eq!(
        compatibility_operations,
        [
            "api_migration_apply",
            "api_migration_plan",
            "ast_grep_rewrite",
            "fact_feedback",
            "fact_store",
            "insert_at",
            "insert_at_symbol",
            "lcm_compress",
            "lcm_describe",
            "lcm_doctor",
            "lcm_expand",
            "lcm_expand_query",
            "lcm_grep",
            "lcm_load_session",
            "lcm_preflight",
            "lcm_session_boundary",
            "lcm_status",
            "memory_status",
            "message_search",
            "move_symbol",
            "multi_str_replace",
            "replace_symbol",
            "session_end",
            "session_refresh",
            "session_start",
            "sessions_for",
            "source_edit_reconcile",
            "str_replace",
            "workflows",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
}

#[test]
fn context_scout_controls_and_claims_preserve_the_exact_address() {
    let address = crate::agents::context_scout_v2::ContextScoutAddressV1 {
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
        serde_json::to_value(ContextScoutControlSurfaceRequest {
            address,
            expected_revision: ConfigurationRevisionId::new("revision.scout.surface")
                .expect("revision"),
        })
        .expect("pause request"),
    )
    .expect("exact-address pause");
    assert!(matches!(
        pause,
        ApplicationSurfaceRequest::ContextScout(ContextScoutSurfaceRequest::Pause(request))
            if request.address == address
    ));

    let claim_body = serde_json::to_value(ContextScoutClaimSurfaceRequest {
        address,
        window: ContextScoutClaimWindowSurfaceV1::IdleWindow,
    })
    .expect("claim request");
    let claim = parse_application_surface_request(
        ApplicationSurfaceOperation::ContextScoutClaim,
        claim_body.clone(),
    )
    .expect("exact-address claim");
    assert!(matches!(
        claim,
        ApplicationSurfaceRequest::ContextScout(ContextScoutSurfaceRequest::Claim(request))
            if request.address == address
                && request.window == ContextScoutClaimWindowSurfaceV1::IdleWindow
    ));
    assert!(matches!(
        parse_application_surface_request(
            ApplicationSurfaceOperation::ContextScoutPause,
            claim_body,
        ),
        Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
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
        [tracedecay_application::retrieval::CodeLexicalFieldFilter {
            field: tracedecay_application::retrieval::CodeLexicalField::Path,
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
            if request.dimension == tracedecay_application::retrieval::CodeFacetDimension::Language
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
    let Pr12PrimitiveRequest::SymbolSearch(symbol_search) = symbol_search
        .into_primitive(
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
            "maximum_depth": 3,
            "resolve_trait_dispatch": true
        })),
    )
    .expect("callers request");
    assert!(matches!(
        callers,
        ApplicationSurfaceRequest::PrimitiveCode(PrimitiveCodeSurfaceRequest::Callers(request))
            if request.resolve_trait_dispatch
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
        match request {
            ApplicationSurfaceRequest::FeedbackImpact(request) => {
                assert_eq!(request.request_handle, "rh_feedback-cycle.fixture");
            }
            ApplicationSurfaceRequest::AffectedTests(request) => {
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
fn primitive_requests_must_match_the_catalog_operation() {
    let request = ApplicationSurfaceRequest::Primitive(Pr12PrimitiveRequest::StorageStatus(
        StorageStatusPrimitiveRequest {
            include_details: false,
        },
    ));
    assert!(request.matches(ApplicationSurfaceOperation::StorageStatus));
    assert!(!request.matches(ApplicationSurfaceOperation::QualifiedName));
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
fn sse_item_maps_to_content_free_delivery_lifecycle() {
    let event = StreamEvent::item(7, "content-is-not-observed").expect("stream item");
    assert_eq!(
        plan26_sse_stream_event(&event),
        Some((
            crate::application::feedback::observations::Plan26SseLifecycleV1::EventDelivered,
            1,
            false,
        ))
    );
}

#[test]
fn dropped_http_request_unregisters_without_cancelling_work() {
    let request_id = RequestId::new("request.http.disconnect").expect("request");
    let cancellation = CancellationSignal::active("cancel.http.disconnect").expect("cancellation");
    let registry: HttpCancellationRegistry = Arc::default();
    registry
        .lock()
        .expect("registry")
        .insert(request_id.clone(), cancellation.clone());

    drop(HttpDisconnectCancellation::new(
        Arc::clone(&registry),
        request_id.clone(),
    ));

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

#[test]
fn storage_status_empty_request_uses_typed_default() {
    let request = parse_application_surface_request(
        ApplicationSurfaceOperation::StorageStatus,
        serde_json::json!({}),
    )
    .expect("empty storage-status request");
    assert!(matches!(
        request,
        ApplicationSurfaceRequest::Primitive(Pr12PrimitiveRequest::StorageStatus(request))
            if !request.include_details
    ));
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
            Plan26RejectedArgumentV1::RequestBody,
            Plan26ArgumentRejectionClassV1::InvalidShape,
            Plan26FeedbackOutcomeV1::Rejected,
        ))
    );
    assert_eq!(
        surface_rejection_metadata(&ApplicationSurfaceAdapterError::UnknownOrNotAuthorized),
        Some((
            Plan26RejectedArgumentV1::Operation,
            Plan26ArgumentRejectionClassV1::Unauthorized,
            Plan26FeedbackOutcomeV1::Denied,
        ))
    );
    assert_eq!(
        surface_rejection_metadata(&ApplicationSurfaceAdapterError::DaemonUnavailable),
        None
    );
}

#[test]
fn legacy_diagnostics_name_routes_to_canonical_read_surface() {
    assert_eq!(
        ApplicationSurfaceOperation::from_tool_name("tracedecay_diagnostics"),
        Some(ApplicationSurfaceOperation::DiagnosticsRead)
    );
    assert_eq!(
        ApplicationSurfaceOperation::from_tool_name("tracedecay_diagnostics_read"),
        Some(ApplicationSurfaceOperation::DiagnosticsRead)
    );
    assert_eq!(
        normalize_application_tool_args("tracedecay_diagnostics", json!({}))
            .unwrap()
            .request,
        json!({"scope": "workspace", "maximum_diagnostics": 1000, "cursor": null})
    );
    assert_eq!(
        normalize_application_tool_args(
            "tracedecay_diagnostics",
            json!({"scope": "file", "path": "src/lib.rs"}),
        )
        .unwrap()
        .request,
        json!({"scope": {"file": "src/lib.rs"}, "maximum_diagnostics": 1000, "cursor": null})
    );
    assert_eq!(
        normalize_application_tool_args(
            "tracedecay_diagnostics",
            json!({"maximum_diagnostics": 25, "cursor": "opaque"}),
        )
        .unwrap()
        .request,
        json!({"scope": "workspace", "maximum_diagnostics": 25, "cursor": "opaque"})
    );
    assert!(
        normalize_application_tool_args("tracedecay_diagnostics", json!({"scope": "package"}),)
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
}
