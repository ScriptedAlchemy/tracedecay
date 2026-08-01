use std::sync::Arc;

use serde_json::Value;
use tracedecay_application::RetainedSurfaceOperation;

use crate::application_surface::ApplicationSurfaceOperation;
use crate::errors::{Result, TraceDecayError};
use crate::global_db::RegisteredGlobalDb;
use crate::tracedecay::TraceDecay;

use super::super::ToolResult;

use super::ToolCallRegistryOptions;
use super::lcm_tool_entry::dispatch_lcm_tool;
use super::retained_catalog::{
    CatalogBoundRetainedMcpRequest, RetainedMcpExecutionContext, invoke_retained_mcp_request,
};
use super::tool_call_support::handle_retrieve;
use super::unknown_tool_error;
use super::{
    admin_cli, admin_project, analysis, analytics, application_surface, ast_grep_search, dashboard,
    edit, git, graph, grep, health, hook_runtime, info, memory, redundancy, session, skills,
    workflow, workflow_query,
};

/// Dispatch code-graph navigation and lookup tools (`tracedecay_search`,
/// `tracedecay_callers`, ...). Returns `None` when `tool_name` belongs to a
/// different domain so the caller can try the next dispatch group.
pub(super) async fn dispatch_graph_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    selected_scope_prefix: Option<&str>,
    search_executor: Option<&crate::mcp::server::CodeIndexSearchExecutor>,
    search_authority: Option<&crate::mcp::server::CodeIndexSearchAuthorityV1>,
    deadline: Option<tracedecay_application::Deadline>,
    cancellation: Option<tracedecay_application::CancellationSignal>,
) -> Result<ToolResult> {
    match tool_name {
        "tracedecay_search" => {
            graph::handle_search(
                cg,
                args,
                selected_scope_prefix,
                search_executor,
                search_authority,
                deadline,
                cancellation,
            )
            .await
        }
        "tracedecay_grep" => grep::handle_grep(cg, args, selected_scope_prefix).await,
        "tracedecay_ast_grep_search" => {
            ast_grep_search::handle_ast_grep_search(cg, args, selected_scope_prefix).await
        }
        "tracedecay_retrieve" => handle_retrieve(cg, &args),
        "tracedecay_context" => graph::handle_context(cg, args, selected_scope_prefix).await,
        "tracedecay_callers" => graph::handle_callers(cg, args).await,
        "tracedecay_callees" => graph::handle_callees(cg, args).await,
        "tracedecay_impact" => graph::handle_impact(cg, args).await,
        "tracedecay_node" => graph::handle_node(cg, args).await,
        "tracedecay_similar" => graph::handle_similar(cg, args).await,
        "tracedecay_rename_preview" => graph::handle_rename_preview(cg, args).await,
        "tracedecay_implementations" => {
            graph::handle_implementations(cg, args, selected_scope_prefix).await
        }
        "tracedecay_callers_for" => graph::handle_callers_for(cg, args).await,
        "tracedecay_find_exact_symbol" => {
            graph::handle_find_exact_symbol(cg, args, selected_scope_prefix).await
        }
        "tracedecay_by_qualified_name" => graph::handle_by_qualified_name(cg, args).await,
        "tracedecay_signature" => graph::handle_signature(cg, args).await,
        "tracedecay_impls" => graph::handle_impls(cg, args).await,
        "tracedecay_derives" => graph::handle_derives(cg, args).await,
        _ => Err(unknown_tool_error(tool_name)),
    }
}

/// Dispatch project-info, registry, and file-inspection tools
/// (`tracedecay_status`, `tracedecay_project_list`, `tracedecay_read`, ...).
#[allow(clippy::too_many_arguments)]
pub(super) async fn dispatch_info_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    server_stats: Option<Value>,
    scope_prefix: Option<&str>,
    selected_scope_prefix: Option<&str>,
    active_project_session_db: Option<&Arc<RegisteredGlobalDb>>,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    match tool_name {
        "tracedecay_status" => {
            info::handle_status(
                cg,
                args,
                server_stats,
                scope_prefix,
                active_project_session_db.map(Arc::as_ref),
            )
            .await
        }
        "tracedecay_active_project" => Ok(info::handle_active_project(
            cg,
            &args,
            server_stats,
            scope_prefix,
        )),
        "tracedecay_project_list" => {
            info::handle_project_list(cg, args, options.project_registry_reads).await
        }
        "tracedecay_project_search" => {
            info::handle_project_search(cg, args, options.project_registry_reads).await
        }
        "tracedecay_project_context" => {
            info::handle_project_context(cg, args, options.project_registry_reads).await
        }
        "tracedecay_files" => info::handle_files(cg, args, selected_scope_prefix).await,
        "tracedecay_admin_sync" => info::handle_admin_sync(cg, args).await,
        "tracedecay_port_status" => info::handle_port_status(cg, args).await,
        "tracedecay_port_order" => info::handle_port_order(cg, args).await,
        "tracedecay_simplify_scan" => info::handle_simplify_scan(cg, args, scope_prefix).await,
        "tracedecay_type_hierarchy" => info::handle_type_hierarchy(cg, args).await,
        "tracedecay_body" => info::handle_body(cg, args, selected_scope_prefix).await,
        "tracedecay_todos" => info::handle_todos(cg, args, scope_prefix).await,
        "tracedecay_read" => info::handle_read(cg, args).await,
        "tracedecay_outline" => info::handle_outline(cg, args).await,
        "tracedecay_config" => info::handle_config(cg, &args),
        "tracedecay_signature_search" => {
            info::handle_signature_search(cg, args, selected_scope_prefix).await
        }
        _ => Err(unknown_tool_error(tool_name)),
    }
}

/// Dispatch administrative tools (`tracedecay_hook_runtime`,
/// `tracedecay_admin_cli`, `tracedecay_admin_project`).
pub(super) async fn dispatch_admin_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    match tool_name {
        "tracedecay_hook_runtime" => {
            hook_runtime::handle_hook_runtime(
                cg,
                args,
                options.global_db.map(std::sync::Arc::as_ref),
                options.accounting_db,
                options.session_authorities,
            )
            .await
        }
        "tracedecay_admin_cli" => {
            admin_cli::handle_admin_cli(
                cg,
                args,
                options.global_db,
                options.accounting_db,
                options.profile_root,
                options.session_authorities,
            )
            .await
        }
        "tracedecay_admin_project" => {
            admin_project::handle_admin_project(
                cg,
                args,
                options.global_db.map(std::sync::Arc::as_ref),
                options.automation_scheduler_reconciler.clone(),
            )
            .await
        }
        _ => Err(unknown_tool_error(tool_name)),
    }
}

/// Dispatch catalog-owned application surfaces.
pub(super) async fn dispatch_application_surface_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    let Some(operation) = ApplicationSurfaceOperation::from_tool_name(tool_name) else {
        return Err(unknown_tool_error(tool_name));
    };
    let normalized_args =
        match crate::application_surface::normalize_application_tool_args(tool_name, args) {
            Ok(args) => args,
            Err(error) => {
                return Err(TraceDecayError::Config {
                    message: error.to_string(),
                });
            }
        };
    application_surface::handle_application_surface(
        cg,
        operation,
        normalized_args,
        options.application_invocation_executor,
        options.application_invocation_target,
        options.application_request_id.clone(),
        options.application_deadline.clone(),
        options.application_cancellation.clone(),
    )
    .await
}

/// Dispatch static-analysis report tools (`tracedecay_dead_code`,
/// `tracedecay_complexity`, `tracedecay_diagnostics`, ...).
pub(super) async fn dispatch_analysis_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
    active_project_session_db: Option<&Arc<RegisteredGlobalDb>>,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    match tool_name {
        "tracedecay_dead_code" => analysis::handle_dead_code(cg, args, scope_prefix).await,
        "tracedecay_circular" => analysis::handle_circular(cg, args).await,
        "tracedecay_hotspots" => analysis::handle_hotspots(cg, args, scope_prefix).await,
        "tracedecay_unused_imports" => {
            analysis::handle_unused_imports(cg, args, scope_prefix).await
        }
        "tracedecay_rank" => analysis::handle_rank(cg, args, scope_prefix).await,
        "tracedecay_largest" => analysis::handle_largest(cg, args, scope_prefix).await,
        "tracedecay_coupling" => analysis::handle_coupling(cg, args, scope_prefix).await,
        "tracedecay_inheritance_depth" => {
            analysis::handle_inheritance_depth(cg, args, scope_prefix).await
        }
        "tracedecay_distribution" => analysis::handle_distribution(cg, args, scope_prefix).await,
        "tracedecay_recursion" => analysis::handle_recursion(cg, args, scope_prefix).await,
        "tracedecay_complexity" => analysis::handle_complexity(cg, args, scope_prefix).await,
        "tracedecay_doc_coverage" => analysis::handle_doc_coverage(cg, args, scope_prefix).await,
        "tracedecay_god_class" => analysis::handle_god_class(cg, args, scope_prefix).await,
        "tracedecay_unsafe_patterns" => {
            analysis::handle_unsafe_patterns(cg, args, scope_prefix).await
        }
        "tracedecay_constructors" => analysis::handle_constructors(cg, args, scope_prefix).await,
        "tracedecay_field_sites" => analysis::handle_field_sites(cg, args, scope_prefix).await,
        "tracedecay_diagnostics" => {
            analysis::handle_diagnostics(
                cg,
                args,
                options.diagnostics_cache,
                options.diagnostics_lsp.as_deref(),
                active_project_session_db.map(Arc::as_ref),
            )
            .await
        }
        _ => Err(unknown_tool_error(tool_name)),
    }
}

/// Dispatch git-aware tools (`tracedecay_affected`, `tracedecay_changelog`,
/// branch and PR context helpers).
pub(super) async fn dispatch_git_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    // Every git handler below performs unbounded gix work — tree walks,
    // revwalks, diffs, the branch-add index build — so a diverged or
    // pathological ref would hang the request. The admission layer carries a
    // dispatch deadline for exactly this (thirty seconds by default, see
    // `dispatch_deadline_horizon_micros`); enforcing it here bounds every
    // handler uniformly and reports exhaustion as the same typed semantic
    // error the other git failures surface.
    let carried_deadline = options.application_deadline.as_ref();
    let remaining = carried_deadline.and_then(crate::daemon_client::deadline_remaining);

    let handler = async {
        match tool_name {
            "tracedecay_admin_branch_add" => git::handle_admin_branch_add(cg, args).await,
            "tracedecay_affected" => git::handle_affected(cg, args).await,
            "tracedecay_diff_context" => git::handle_diff_context(cg, args).await,
            "tracedecay_changelog" => git::handle_changelog(cg, args).await,
            "tracedecay_commit_context" => git::handle_commit_context(cg, args).await,
            "tracedecay_pr_context" => git::handle_pr_context(cg, args).await,
            "tracedecay_branch_search" => git::handle_branch_search(cg, args).await,
            "tracedecay_branch_diff" => git::handle_branch_diff(cg, args).await,
            "tracedecay_branch_list" => Ok(git::handle_branch_list(cg, &args)),
            _ => Err(unknown_tool_error(tool_name)),
        }
    };

    match (carried_deadline.is_some(), remaining) {
        (_, Some(remaining)) => match tokio::time::timeout(remaining, handler).await {
            Ok(result) => result,
            Err(_elapsed) => Ok(git::git_dispatch_deadline_result(cg, tool_name)),
        },
        // `deadline_remaining` yields `None` for a non-positive budget, so a
        // carried deadline that already elapsed must be rejected rather than
        // dispatched unbounded.
        (true, None) => Ok(git::git_dispatch_deadline_result(cg, tool_name)),
        // Standalone / non-admission callers carry no deadline and stay
        // unbounded.
        (false, None) => handler.await,
    }
}

/// Dispatch source-editing tools (`tracedecay_str_replace`,
/// `tracedecay_move_symbol`, ...).
pub(super) async fn dispatch_edit_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    let invocation = edit::SourceEditInvocationContext {
        executor: options.source_edit_executor.clone(),
        reconciliation_executor: options.source_edit_reconciliation_executor.clone(),
        request_id: options.application_request_id.clone(),
        deadline: options.application_deadline.clone(),
        cancellation: options.application_cancellation.clone(),
    };
    match tool_name {
        "tracedecay_str_replace" => edit::handle_str_replace(cg, args, invocation.clone()).await,
        "tracedecay_multi_str_replace" => {
            edit::handle_multi_str_replace(cg, args, invocation.clone()).await
        }
        "tracedecay_insert_at" => edit::handle_insert_at(cg, args, invocation.clone()).await,
        "tracedecay_ast_grep_rewrite" => {
            edit::handle_ast_grep_rewrite(cg, args, invocation.clone()).await
        }
        "tracedecay_replace_symbol" => {
            edit::handle_replace_symbol(cg, args, invocation.clone()).await
        }
        "tracedecay_insert_at_symbol" => {
            edit::handle_insert_at_symbol(cg, args, invocation.clone()).await
        }
        "tracedecay_move_symbol" => edit::handle_move_symbol(cg, args, invocation.clone()).await,
        "tracedecay_api_migration_plan" => edit::handle_api_migration_plan(cg, args).await,
        "tracedecay_api_migration_apply" => {
            edit::handle_api_migration_apply(cg, args, invocation.clone()).await
        }
        "tracedecay_source_edit_reconcile" => {
            edit::handle_source_edit_reconcile(cg, args, invocation).await
        }
        _ => Err(unknown_tool_error(tool_name)),
    }
}

/// Dispatch code-health and session-baseline tools (`tracedecay_health`,
/// `tracedecay_test_risk`, `tracedecay_runtime`, ...).
pub(super) async fn dispatch_health_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
    active_project_session_db: Option<&Arc<RegisteredGlobalDb>>,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    match tool_name {
        "tracedecay_test_map" => health::handle_test_map(cg, args, scope_prefix).await,
        "tracedecay_gini" => health::handle_gini(cg, args, scope_prefix).await,
        "tracedecay_dependency_depth" => {
            health::handle_dependency_depth(cg, args, scope_prefix).await
        }
        "tracedecay_health" => health::handle_health(cg, args, scope_prefix).await,
        "tracedecay_redundancy" => redundancy::handle_redundancy(cg, args, scope_prefix).await,
        "tracedecay_runtime" => {
            health::handle_runtime(
                cg,
                args,
                options.global_db.map(std::sync::Arc::as_ref),
                active_project_session_db.map(Arc::as_ref),
                options.doctor_report_reader.as_ref(),
            )
            .await
        }
        "tracedecay_dsm" => health::handle_dsm(cg, args, scope_prefix).await,
        "tracedecay_test_risk" => health::handle_test_risk(cg, args, scope_prefix).await,
        _ => Err(unknown_tool_error(tool_name)),
    }
}

/// Dispatch retained memory, session, and workflow operations only after the
/// application-owned catalog has resolved their stable operation identity.
pub(super) async fn dispatch_retained_application_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
    active_project_session_db: Option<&Arc<RegisteredGlobalDb>>,
    active_lcm_context: session::LcmHandlerContext<'_>,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    let Some(operation) = RetainedSurfaceOperation::from_name(tool_name) else {
        return Err(unknown_tool_error(tool_name));
    };
    invoke_retained_mcp_request(
        RetainedMcpExecutionContext::Project {
            cg,
            scope_prefix,
            active_project_session_db,
            active_lcm_context,
            options: &options,
        },
        operation,
        args,
    )
    .await
}

/// Dispatch a retained memory operation (add/search/feedback/status) under one
/// central deadline.
///
/// Mirrors [`dispatch_git_tools`]: every memory handler performs unbounded
/// store-touching work — the add-path holographic encode, a serialized write
/// transaction, an optional digest refresh — so an admission-carried client
/// deadline bounds them all uniformly here, degrading a stalled store to a
/// typed, retryable problem instead of pinning the MCP transport open. A
/// standalone caller that carries no deadline stays unbounded; a carried
/// deadline that has already elapsed is rejected rather than dispatched.
pub(super) async fn dispatch_memory_operation(
    operation: RetainedSurfaceOperation,
    cg: &TraceDecay,
    args: Value,
    options: &ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    let global_db = options.global_db.map(std::sync::Arc::as_ref);
    let operation_label = match operation {
        RetainedSurfaceOperation::FactStore => "fact_store",
        RetainedSurfaceOperation::FactFeedback => "fact_feedback",
        RetainedSurfaceOperation::MemoryStatus => "memory_status",
        _ => unreachable!("dispatch_memory_operation handles memory operations only"),
    };

    let handler = async {
        match operation {
            RetainedSurfaceOperation::FactStore => {
                memory::handle_fact_store(cg, args, global_db).await
            }
            RetainedSurfaceOperation::FactFeedback => {
                memory::handle_fact_feedback(cg, args, global_db).await
            }
            RetainedSurfaceOperation::MemoryStatus => {
                memory::handle_memory_status(cg, args, global_db).await
            }
            _ => unreachable!("dispatch_memory_operation handles memory operations only"),
        }
    };

    let carried_deadline = options.application_deadline.as_ref();
    let remaining = carried_deadline.and_then(crate::daemon_client::deadline_remaining);
    match (carried_deadline.is_some(), remaining) {
        (_, Some(remaining)) => match tokio::time::timeout(remaining, handler).await {
            Ok(result) => result,
            Err(_elapsed) => Err(memory::memory_deadline_error(operation_label, remaining)),
        },
        // `deadline_remaining` yields `None` for a non-positive budget, so a
        // carried deadline that already elapsed must be rejected rather than
        // dispatched unbounded.
        (true, None) => Err(memory::memory_deadline_error(
            operation_label,
            std::time::Duration::ZERO,
        )),
        // Standalone / non-admission callers carry no deadline and stay
        // unbounded.
        (false, None) => handler.await,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_project_retained_application_tool(
    request: CatalogBoundRetainedMcpRequest,
    cg: &TraceDecay,
    scope_prefix: Option<&str>,
    active_project_session_db: Option<&Arc<RegisteredGlobalDb>>,
    active_lcm_context: session::LcmHandlerContext<'_>,
    options: &ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    match request.operation {
        RetainedSurfaceOperation::FactStore
        | RetainedSurfaceOperation::FactFeedback
        | RetainedSurfaceOperation::MemoryStatus => {
            dispatch_memory_operation(request.operation, cg, request.arguments, options).await
        }
        RetainedSurfaceOperation::SessionRefresh => {
            session::handle_session_refresh(
                request.arguments,
                options.session_authorities.refresh_services(),
            )
            .await
        }
        RetainedSurfaceOperation::MessageSearch => {
            Box::pin(
                session::message_search::handle_message_search_with_registry(
                    Some(cg.project_root()),
                    session::message_search::SessionRetrievalStoreScope::Project,
                    request.arguments,
                    options.session_authorities.project_retrieval,
                    options.project_registry_reads,
                ),
            )
            .await
        }
        RetainedSurfaceOperation::SessionsFor => {
            session::handle_sessions_for(
                cg,
                active_project_session_db.map(Arc::as_ref),
                request.arguments,
            )
            .await
        }
        RetainedSurfaceOperation::Workflows => {
            workflow_query::handle_workflows(cg, request.arguments, options.workflow_index_reads)
                .await
        }
        RetainedSurfaceOperation::LcmStatus
        | RetainedSurfaceOperation::LcmDoctor
        | RetainedSurfaceOperation::LcmLoadSession
        | RetainedSurfaceOperation::LcmGrep
        | RetainedSurfaceOperation::LcmDescribe
        | RetainedSurfaceOperation::LcmExpand
        | RetainedSurfaceOperation::LcmExpandQuery
        | RetainedSurfaceOperation::LcmPreflight
        | RetainedSurfaceOperation::LcmCompress
        | RetainedSurfaceOperation::LcmSessionBoundary => {
            dispatch_lcm_tool(request.operation, request.arguments, active_lcm_context).await
        }
        RetainedSurfaceOperation::SessionStart => {
            let db = active_project_session_db.ok_or_else(|| TraceDecayError::Config {
                message: "health-delta observation authority is unavailable".to_owned(),
            })?;
            health::handle_session_start(cg, db.as_ref(), request.arguments, scope_prefix).await
        }
        RetainedSurfaceOperation::SessionEnd => {
            let db = active_project_session_db.ok_or_else(|| TraceDecayError::Config {
                message: "health-delta observation authority is unavailable".to_owned(),
            })?;
            health::handle_session_end(cg, db.as_ref(), request.arguments, scope_prefix).await
        }
    }
}

/// Dispatch memory, skill, and analytics tools (`tracedecay_fact_store`,
/// `tracedecay_skill_list`, `tracedecay_analytics`, ...).
pub(super) async fn dispatch_memory_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    match tool_name {
        "tracedecay_automation_run_artifact_view" => {
            skills::handle_automation_run_artifact_view(cg, args).await
        }
        "tracedecay_analytics" => {
            analytics::handle_analytics(
                cg,
                args,
                options.global_db.map(std::sync::Arc::as_ref),
                options.accounting_db,
            )
            .await
        }
        "tracedecay_skill_list" => skills::handle_skill_list(cg, args, options.accounting_db).await,
        "tracedecay_skill_view" => skills::handle_skill_view(cg, args, options.accounting_db).await,
        "tracedecay_hermes_skill_bridge" => skills::handle_hermes_skill_bridge(cg, &args),
        _ => Err(unknown_tool_error(tool_name)),
    }
}

/// Dispatch session, dashboard, and workflow tools (`tracedecay_dashboard`,
/// `tracedecay_message_search`, `tracedecay_workflows`, ...).
pub(super) async fn dispatch_session_workflow_tools(
    tool_name: &str,
    cg: &TraceDecay,
    args: Value,
    options: ToolCallRegistryOptions<'_>,
) -> Result<ToolResult> {
    match tool_name {
        "tracedecay_diagnose" => {
            workflow::handle_diagnose(cg, args, options.code_index_publication_identity.as_deref())
                .await
        }
        "tracedecay_run_affected_tests" => {
            workflow::handle_run_affected_tests(
                cg,
                args,
                options.application_cancellation.clone(),
                options.code_index_publication_identity.as_deref(),
            )
            .await
        }
        "tracedecay_dashboard" => {
            dashboard::handle_dashboard(
                cg,
                args,
                options.retained_project_graph_resolver.clone(),
                options.registered_project_session_db.clone(),
                options.registered_savings_db.clone(),
                options.automation_scheduler_reconciler.clone(),
                options.automation_writer.clone(),
                options.doctor_report_reader.clone(),
                options.doctor_remediation_dispatcher.clone(),
                options.code_index_freshness_reader.clone(),
                options.feedback_status_reader.clone(),
                options.diagnostics_lsp.clone(),
                options.dashboard_application_invocation_executor.clone(),
            )
            .await
        }
        _ => Err(unknown_tool_error(tool_name)),
    }
}
