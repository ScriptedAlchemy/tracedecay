//! Graph- and git-backed reads and reports served by the project's graph-tool
//! owner.
//!
//! The owner computes each operation's typed catalog result; every surface
//! renders it here, so MCP and the CLI print the same tool result.

use std::path::Path;

use serde_json::{Value, json};
use tracedecay_application::code_index::CodeIndexIgnoredDependencyAdmissionPortV1;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{
    AdminCliSurfaceRequestV1, DerivesResultV1, ImpactResultV1, NodeResultV1,
    PrimitiveSearchFreshnessV1, RenamePreviewPrimitiveOutcomeV1, RetrieveResultV1, StatusResultV1,
};
use tracedecay_contracts::retrieval::{CallableCodeOperationKind, callable_code_operation};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_graph_query::VerifiedGraphQuery;
use tracedecay_tool_catalog::{
    ApplicationSurfaceOperation, OwnerStoreDeclarationV1, OwnerStoresV1,
};

use crate::handlers::analysis::{compute_analysis_report, render_circular_md};
use crate::handlers::ast_grep::{compute_ast_grep_search, render_ast_grep_search};
use crate::handlers::git::{
    compute_affected, compute_branch_diff, compute_branch_list, compute_branch_search,
    compute_changelog, compute_commit_context, compute_diff_context, compute_pr_context,
    git_tool_failure_message,
};
use crate::handlers::graph::{
    compute_by_qualified_name, compute_context, compute_derives, compute_find_exact_symbol,
    compute_impact, compute_node, compute_redundancy, compute_rename_preview, compute_search,
    compute_signature, compute_similar, freshness_lines, not_found_tool_result, render_context,
    render_search,
};
use crate::handlers::grep::{compute_grep, render_grep};
use crate::handlers::health::{
    compute_dependency_depth, compute_dsm, compute_gini, compute_health, compute_test_map,
    compute_test_risk, render_dsm_md,
};
use crate::handlers::hook_runtime::decode_hook_runtime_request;
use crate::handlers::info::{
    compute_config, compute_files, compute_port_order, compute_port_status, compute_todos,
    render_files_md, render_registry_listing_md, render_status_md,
};
use crate::handlers::retrieve::{compute_retrieve, render_retrieved_page};
use crate::handlers::support::{
    decode_primitive_request, generic_tool_result, rendered_tool_result, text_tool_result,
    unknown_tool_error,
};
use crate::handlers::verified_read::{VerifiedGraphOpen, verified_read_operation as read};
use crate::tools::response_trailers::{CODE_GRAPH_FRESHNESS_TRAILER_PREFIX, ResponseTrailer};
use crate::tools::{render, renderers};
use crate::{McpToolContext, ToolResult};

/// The stores the project's owner needs mounted to answer `operation` with
/// `arguments`, as the operation declares them in the tool catalog or, for an
/// operation that multiplexes actions, as the requested action declares them.
/// A request whose action does not decode declares nothing, so it waits for
/// every store and the full owner's decode refuses it.
pub fn graph_tool_owner_stores(
    operation: ApplicationSurfaceOperation,
    arguments: &serde_json::Map<String, Value>,
) -> OwnerStoresV1 {
    let action = match operation.owner_stores() {
        OwnerStoreDeclarationV1::Every(stores) => return stores,
        OwnerStoreDeclarationV1::PerAction => Value::Object(arguments.clone()),
    };
    let declared = match operation {
        ApplicationSurfaceOperation::AdminCli => {
            decode_primitive_request::<AdminCliSurfaceRequestV1>(&action, operation.mcp_tool_name())
                .map(|request| request.owner_stores())
        }
        ApplicationSurfaceOperation::HookRuntime => {
            decode_hook_runtime_request(&action).map(|request| request.owner_stores())
        }
        _ => return OwnerStoresV1::ProjectSessions,
    };
    declared.unwrap_or(OwnerStoresV1::ProjectSessions)
}

/// Computes one graph-tool operation's typed result on the owner's side.
///
/// `tracedecay_diagnose` publishes into the project's own store, so its owner
/// computes it beside this table.
pub async fn compute_graph_tool(
    ctx: &McpToolContext<'_>,
    open: &VerifiedGraphOpen<'_>,
    operation: ApplicationSurfaceOperation,
    args: Value,
    scope_prefix: Option<&str>,
    ignored_dependency_admission: Option<&dyn CodeIndexIgnoredDependencyAdmissionPortV1>,
) -> Result<GraphToolCompletionV1> {
    match operation {
        ApplicationSurfaceOperation::Context => {
            let operation = read("context")?;
            compute_context(ctx, || open(operation.clone()), args, scope_prefix).await
        }
        ApplicationSurfaceOperation::Impact => {
            compute_impact(&open(read("impact")?).await?, args).await
        }
        ApplicationSurfaceOperation::Node => compute_node(&open(read("node")?).await?, args).await,
        ApplicationSurfaceOperation::Similar => compute_similar(ctx, args).await,
        ApplicationSurfaceOperation::Redundancy => compute_redundancy(ctx, args).await,
        ApplicationSurfaceOperation::RenamePreview => {
            compute_rename_preview(ctx, &open(read("rename_preview")?).await?, args).await
        }
        ApplicationSurfaceOperation::PortStatus => {
            let graph = open(read("port_status")?).await?;
            with_read_cost(&graph, compute_port_status(&graph, args).await)
        }
        ApplicationSurfaceOperation::PortOrder => {
            let graph = open(read("port_order")?).await?;
            with_read_cost(&graph, compute_port_order(&graph, args).await)
        }
        ApplicationSurfaceOperation::Todos => {
            let graph = open(read("todos")?).await?;
            with_read_cost(&graph, compute_todos(&graph, args, scope_prefix).await)
        }
        ApplicationSurfaceOperation::TestMap
        | ApplicationSurfaceOperation::TestRisk
        | ApplicationSurfaceOperation::Gini
        | ApplicationSurfaceOperation::DependencyDepth
        | ApplicationSurfaceOperation::Health
        | ApplicationSurfaceOperation::Dsm => {
            compute_health_report(open, operation, args, scope_prefix).await
        }
        ApplicationSurfaceOperation::DeadCode
        | ApplicationSurfaceOperation::Circular
        | ApplicationSurfaceOperation::Hotspots
        | ApplicationSurfaceOperation::UnmountedFiles
        | ApplicationSurfaceOperation::Rank
        | ApplicationSurfaceOperation::Largest
        | ApplicationSurfaceOperation::Coupling
        | ApplicationSurfaceOperation::InheritanceDepth
        | ApplicationSurfaceOperation::Distribution
        | ApplicationSurfaceOperation::Recursion
        | ApplicationSurfaceOperation::Complexity
        | ApplicationSurfaceOperation::DocCoverage
        | ApplicationSurfaceOperation::GodClass
        | ApplicationSurfaceOperation::UnsafePatterns
        | ApplicationSurfaceOperation::Constructors
        | ApplicationSurfaceOperation::FieldSites => {
            compute_analysis_report(ctx, open, operation, args, scope_prefix).await
        }
        ApplicationSurfaceOperation::FindExactSymbol => {
            compute_find_exact_symbol(
                ctx,
                &open(read("qualified_name")?).await?,
                args,
                scope_prefix,
                ignored_dependency_admission,
            )
            .await
        }
        ApplicationSurfaceOperation::ByQualifiedName => {
            compute_by_qualified_name(&open(read("qualified_name")?).await?, args).await
        }
        ApplicationSurfaceOperation::Signature => {
            compute_signature(&open(read("qualified_name")?).await?, args).await
        }
        ApplicationSurfaceOperation::Derives => {
            compute_derives(&open(read("code_type_hierarchy")?).await?, args).await
        }
        ApplicationSurfaceOperation::Grep => {
            // Grep degrades to a lexical answer when the graph is unavailable,
            // so the open outcome travels to the handler instead of failing here.
            let graph = match read("source_lines") {
                Ok(operation) => open(operation).await,
                Err(error) => Err(error),
            };
            compute_grep(
                ctx.project_root(),
                ctx.index_path_policy(),
                graph.as_ref(),
                args,
                scope_prefix,
                ctx.deadline().cloned(),
                ctx.cancellation().cloned(),
            )
            .await
        }
        ApplicationSurfaceOperation::AstGrepSearch => {
            compute_ast_grep_search(
                ctx.project_root(),
                ctx.index_path_policy(),
                args,
                scope_prefix,
                ctx.deadline().cloned(),
                ctx.cancellation().cloned(),
            )
            .await
        }
        ApplicationSurfaceOperation::Affected => {
            compute_affected(ctx, open(read("file_dependents")?), args).await
        }
        ApplicationSurfaceOperation::DiffContext => {
            compute_diff_context(ctx, open(read("file_dependents")?), args).await
        }
        ApplicationSurfaceOperation::Changelog => compute_changelog(ctx, args).await,
        ApplicationSurfaceOperation::CommitContext => {
            compute_commit_context(ctx, open(read("file_dependents")?), args).await
        }
        ApplicationSurfaceOperation::PrContext => {
            compute_pr_context(ctx, open(read("file_dependents")?), args).await
        }
        ApplicationSurfaceOperation::BranchSearch => compute_branch_search(ctx, args).await,
        ApplicationSurfaceOperation::BranchDiff => compute_branch_diff(ctx, args).await,
        ApplicationSurfaceOperation::BranchList => compute_branch_list(ctx, args).await,
        ApplicationSurfaceOperation::Files => {
            // The request is decoded before the graph is admitted, so a
            // malformed call is refused even while the graph is unavailable.
            let request = decode_primitive_request(&args, "tracedecay_files")?;
            let operation = callable_code_operation(CallableCodeOperationKind::SourceMetadata)
                .map_err(|error| TraceDecayError::Config {
                    message: format!("invalid source metadata operation: {error}"),
                })?;
            let graph = open(operation).await?;
            // The freshness read names the latest text generation, which can
            // run ahead of the generation this verified graph serves. Attach
            // the omission block only when it describes the same sealed
            // generation the listing comes from.
            let worktree_omitted_sources = ctx.freshness().await.and_then(|payload| {
                payload
                    .worktrees
                    .into_iter()
                    .next()
                    .filter(|worktree| {
                        worktree.latest_generation_id.as_deref()
                            == Some(graph.generation().as_str())
                    })
                    .and_then(|worktree| worktree.omitted_sources)
            });
            with_read_cost(
                &graph,
                compute_files(&graph, request, scope_prefix, worktree_omitted_sources).await,
            )
        }
        ApplicationSurfaceOperation::Config => compute_config(ctx.project_root(), args).await,
        ApplicationSurfaceOperation::Search => {
            compute_search(
                ctx,
                open(read("code_symbol_search")?),
                args,
                scope_prefix,
                ignored_dependency_admission,
            )
            .await
        }
        ApplicationSurfaceOperation::Retrieve => {
            compute_retrieve(&ctx.store_layout().response_handle_root, &args).await
        }
        operation => Err(unknown_tool_error(operation.mcp_tool_name())),
    }
}

/// Runs one code-health report over a metered verified-graph reader and
/// reports what the read cost beside its result.
async fn compute_health_report(
    open: &VerifiedGraphOpen<'_>,
    operation: ApplicationSurfaceOperation,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<GraphToolCompletionV1> {
    let graph_operation = if operation == ApplicationSurfaceOperation::Health {
        "health_delta"
    } else {
        "health_read"
    };
    let graph = open(read(graph_operation)?).await?;
    let completion = match operation {
        ApplicationSurfaceOperation::TestMap => compute_test_map(&graph, args).await,
        ApplicationSurfaceOperation::TestRisk => {
            compute_test_risk(&graph, args, scope_prefix).await
        }
        ApplicationSurfaceOperation::Gini => compute_gini(&graph, args, scope_prefix).await,
        ApplicationSurfaceOperation::DependencyDepth => {
            compute_dependency_depth(&graph, args, scope_prefix).await
        }
        ApplicationSurfaceOperation::Health => compute_health(&graph, args, scope_prefix).await,
        ApplicationSurfaceOperation::Dsm => compute_dsm(&graph, args, scope_prefix).await,
        operation => Err(unknown_tool_error(operation.mcp_tool_name())),
    };
    with_read_cost(&graph, completion)
}

/// Reports what the verified-graph read cost beside its result.
fn with_read_cost(
    graph: &VerifiedGraphQuery,
    completion: Result<GraphToolCompletionV1>,
) -> Result<GraphToolCompletionV1> {
    let mut completion = completion?;
    completion.cost = Some(graph.read_cost());
    Ok(completion)
}

/// Renders a typed graph-tool result as its tool result, with the stale-graph
/// trailer every surface appends.
///
/// Token accounting stays with the caller that owns raw-file sizes. The MCP
/// server uses its retained cache; the CLI stats the project files. Rendering
/// them here would mark the result accounted and skip that cache.
pub fn render_graph_tool(
    response_handle_root: Option<&Path>,
    args: &Value,
    completion: GraphToolCompletionV1,
) -> Result<ToolResult> {
    let GraphToolCompletionV1 {
        result,
        touched_files,
        code_graph,
        analytics,
        cost,
    } = completion;
    let worktree = code_graph
        .as_ref()
        .and_then(|served| served.worktree.as_ref())
        .filter(|_| git_tool_failure_message(&result).is_none());
    // A JSON body takes the verdict before it renders, so a truncated
    // body's stored handle carries it too, not only the preview envelope.
    let body = || -> Result<Value> {
        let mut value = result.result_value()?;
        if render::wants_json(args)
            && let (Some(freshness), Value::Object(object)) = (worktree, &mut value)
        {
            object.insert("freshness".to_owned(), serde_json::to_value(freshness)?);
        }
        Ok(value)
    };
    let mut rendered = match &result {
        GraphToolResultV1::Context(context) => render_context(response_handle_root, args, context)?,
        GraphToolResultV1::Node(NodeResultV1::NotFound(not_found)) => {
            not_found_tool_result(not_found)?
        }
        GraphToolResultV1::Impact(ImpactResultV1::NotFound(not_found))
        | GraphToolResultV1::RenamePreview(RenamePreviewPrimitiveOutcomeV1::NotFound(not_found)) => {
            not_found_tool_result(not_found)?
        }
        GraphToolResultV1::Dsm(_) => {
            let value = body()?;
            rendered_tool_result(response_handle_root, args, &value, Vec::new(), || {
                render_dsm_md(&value)
            })
        }
        GraphToolResultV1::Diagnose(_) => {
            let value = body()?;
            rendered_tool_result(response_handle_root, args, &value, Vec::new(), || {
                render::diagnostics_md(&value)
            })
        }
        GraphToolResultV1::Circular(circular) => {
            rendered_tool_result(response_handle_root, args, &body()?, Vec::new(), || {
                render_circular_md(circular)
            })
        }
        GraphToolResultV1::UnmountedFiles(_) => {
            let value = body()?;
            rendered_tool_result(response_handle_root, args, &value, Vec::new(), || {
                render::unmounted_files_md(&value)
            })
        }
        GraphToolResultV1::UnsafePatterns(_) => {
            let value = body()?;
            rendered_tool_result(response_handle_root, args, &value, Vec::new(), || {
                render::risky_patterns_md(&value)
            })
        }
        GraphToolResultV1::Derives(DerivesResultV1(symbols)) if symbols.is_empty() => {
            text_tool_result("No matching symbol found.", Vec::new())
        }
        GraphToolResultV1::Grep(grep) => render_grep(response_handle_root, args, &body()?, grep)?,
        GraphToolResultV1::Files(files) => {
            rendered_tool_result(response_handle_root, args, &body()?, Vec::new(), || {
                render_files_md(files)
            })
        }
        GraphToolResultV1::Search(search) => render_search(response_handle_root, args, search)?,
        GraphToolResultV1::Retrieve(RetrieveResultV1::Page(page)) => {
            render_retrieved_page(args, page)?
        }
        GraphToolResultV1::AstGrepSearch(search) => {
            render_ast_grep_search(response_handle_root, args, &body()?, search)?
        }
        GraphToolResultV1::AutomationRunList(_) => render_value_markdown(
            response_handle_root,
            args,
            &body()?,
            renderers::automation_run_list_md,
        ),
        GraphToolResultV1::AutomationRunView(_) => render_value_markdown(
            response_handle_root,
            args,
            &body()?,
            renderers::automation_run_view_md,
        ),
        GraphToolResultV1::AutomationRunArtifactView(_) => render_value_markdown(
            response_handle_root,
            args,
            &body()?,
            renderers::automation_artifact_md,
        ),
        GraphToolResultV1::SkillList(_) => render_value_markdown(
            response_handle_root,
            args,
            &body()?,
            renderers::skill_list_md,
        ),
        GraphToolResultV1::SkillView(_) => render_value_markdown(
            response_handle_root,
            args,
            &body()?,
            renderers::skill_view_md,
        ),
        GraphToolResultV1::Analytics(_) => render_value_markdown(
            response_handle_root,
            args,
            &body()?,
            renderers::analytics_md,
        ),
        GraphToolResultV1::Status(status) => {
            let value = body()?;
            let mut rendered =
                rendered_tool_result(response_handle_root, args, &value, Vec::new(), || {
                    render_status_md(&value)
                });
            // Structured content beside the rendered body in every format,
            // like a typed `problem`, so a caller such as `tracedecay tool`
            // can type its exit status on the readiness wait's outcome.
            if let StatusResultV1::Project(project) = status
                && let Some(wait) = &project.wait
                && let Some(object) = rendered.value.as_object_mut()
            {
                object.insert("structuredContent".to_owned(), json!({ "wait": wait }));
            }
            rendered
        }
        // A listing renders inline: it is bounded by its own page limit.
        GraphToolResultV1::ProjectList(listing) | GraphToolResultV1::ProjectSearch(listing) => {
            rendered_tool_result(None, args, &body()?, Vec::new(), || {
                render_registry_listing_md(listing)
            })
        }
        _ => generic_tool_result(response_handle_root, args, &body()?, Vec::new()),
    };
    if let Some(message) = git_tool_failure_message(&result) {
        rendered = rendered
            .with_semantic_error(true)
            .with_failure_message(message);
    }
    let mut structured = result.result_value()?;
    if let Some(freshness) = worktree {
        open_with_worktree_freshness(&mut rendered, &mut structured, args, freshness)?;
    }
    rendered = rendered.with_structured_result(structured);
    ResponseTrailer {
        touched_files: &touched_files,
        code_graph: code_graph.as_ref(),
        cost: cost.as_ref(),
    }
    .attach(&mut rendered);
    Ok(match analytics {
        Some(analytics) => rendered.with_internal_analytics(analytics.ledger_value()),
        None => rendered,
    })
}

/// Opens a graph read with the worktree verdict search opens with: the
/// `freshness:` lines in markdown, a `freshness` key in a JSON object body
/// and in the structured result. A not-found body is JSON in every format,
/// so it takes the key rather than lines that would break its parse. A JSON
/// body that is not an object, such as a signature array, keeps its schema
/// and carries the verdict in a `code_graph_freshness` block beside it.
fn open_with_worktree_freshness(
    rendered: &mut ToolResult,
    structured: &mut Value,
    args: &Value,
    freshness: &PrimitiveSearchFreshnessV1,
) -> Result<()> {
    let verdict = serde_json::to_value(freshness)?;
    let mut beside = None;
    if let Some(Value::String(text)) = rendered.value.pointer_mut("/content/0/text") {
        match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(mut body)) => {
                body.insert("freshness".to_owned(), verdict.clone());
                *text = Value::Object(body).to_string();
            }
            _ if !render::wants_json(args) => text.insert_str(0, &freshness_lines(freshness)),
            _ => beside = Some(format!("\n{CODE_GRAPH_FRESHNESS_TRAILER_PREFIX} {verdict}")),
        }
    }
    if let Some(text) = beside
        && let Some(content) = rendered
            .value
            .get_mut("content")
            .and_then(Value::as_array_mut)
    {
        content.insert(1, json!({"type": "text", "text": text}));
    }
    if let Value::Object(object) = structured {
        object.insert("freshness".to_owned(), verdict);
    }
    Ok(())
}

/// Renders a result through a markdown renderer that reads its JSON value.
fn render_value_markdown(
    response_handle_root: Option<&Path>,
    args: &Value,
    value: &Value,
    markdown: fn(&Value) -> String,
) -> ToolResult {
    rendered_tool_result(response_handle_root, args, value, Vec::new(), || {
        markdown(value)
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use tracedecay_contracts::retrieval::DerivesSymbolV1;
    use tracedecay_contracts::retrieval::{
        CodeGraphReadFreshnessV1, ContextExtensionPointV1, ContextModeV1, ContextPlanV1,
        ContextResultV1, ContextRetrievalPlanV1, ContextStageV1, PrimitiveFreshnessStateV1,
        PrimitiveLaneCompleteV1, PrimitiveLaneStatusV1, PrimitiveRecallV1,
        PrimitiveSearchCoverageV1, PrimitiveSearchFreshnessV1, PrimitiveSymbolLocationV1,
        ServedCodeGraphGenerationV1, TodoMarkerV1, TodosResultV1,
    };
    use tracedecay_contracts::{ContextMemoryAnalyticsV1, InvocationAnalyticsV1};
    use tracedecay_domain::UtcMicros;
    use tracedecay_runtime_core::tracedecay::current_timestamp;

    use super::*;
    use crate::response_handles::{ResponseHandleLookup, retrieve_response_handle};

    fn todos_completion(code_graph: Option<ServedCodeGraphGenerationV1>) -> GraphToolCompletionV1 {
        GraphToolCompletionV1 {
            result: GraphToolResultV1::Todos(TodosResultV1 {
                match_count: 1,
                by_kind: BTreeMap::from([("TODO".to_owned(), 1)]),
                markers: vec![TodoMarkerV1 {
                    kind: "TODO".to_owned(),
                    file: "src/lib.rs".to_owned(),
                    line: 1,
                    text: "// TODO: probe".to_owned(),
                    enclosing: None,
                }],
                freshness: None,
            }),
            touched_files: vec!["src/lib.rs".to_owned()],
            code_graph,
            analytics: None,
            cost: None,
        }
    }

    fn texts(result: &ToolResult) -> Vec<String> {
        result.value["content"]
            .as_array()
            .expect("content blocks")
            .iter()
            .map(|block| block["text"].as_str().expect("text block").to_owned())
            .collect()
    }

    #[test]
    fn a_stale_seat_renders_its_trailer_before_the_accounting_footer() {
        let root = tempfile::tempdir().expect("project");
        std::fs::create_dir_all(root.path().join("src")).expect("src");
        std::fs::write(root.path().join("src/lib.rs"), "a".repeat(800)).expect("source");
        let mut rendered = render_graph_tool(
            Some(root.path()),
            &json!({"format": "json"}),
            todos_completion(Some(ServedCodeGraphGenerationV1 {
                generation: "generation.render.stale".to_owned(),
                worktree: None,
                freshness: CodeGraphReadFreshnessV1::LastCompleteStale {
                    sealed_at: UtcMicros(0),
                    rebuild_in_flight: false,
                },
            })),
        )
        .expect("rendered");
        crate::tools::response_trailers::account_tool_result(Some(root.path()), &mut rendered);
        let blocks = texts(&rendered);
        assert_eq!(blocks.len(), 3, "{blocks:?}");
        assert!(
            blocks[1].starts_with(
                "\ncode_graph_freshness: stale, serving the last complete generation \
                 generation.render.stale (sealed "
            ),
            "{blocks:?}"
        );
        assert!(
            blocks[1].ends_with(
                "ago) while source freshness remains unverified; results may trail the live worktree"
            ),
            "{blocks:?}"
        );
        let after = (blocks[0].len() + blocks[1].len()) / 4;
        assert_eq!(
            blocks[2],
            format!("\ntracedecay_metrics: before=200 after={after}")
        );
        assert_eq!(rendered.touched_files, vec!["src/lib.rs".to_owned()]);
    }

    fn plan_context() -> ContextResultV1 {
        ContextResultV1 {
            task: "extend the store".to_owned(),
            mode: ContextModeV1::Plan,
            freshness: PrimitiveSearchFreshnessV1 {
                state: PrimitiveFreshnessStateV1::Fresh,
                indexing: None,
            },
            code_generation: Some("generation.context".to_owned()),
            search_matches: Vec::new(),
            lexical_anchors: Vec::new(),
            symbols: vec![PrimitiveSymbolLocationV1 {
                node_id: "symbol.store".to_owned(),
                name: "Store".to_owned(),
                qualified_name: "crate::Store".to_owned(),
                kind: "trait".to_owned(),
                file: "src/store.rs".to_owned(),
                start_line: 3,
                end_line: 9,
                unavailable_fields: Vec::new(),
            }],
            related_symbols: Vec::new(),
            related_omission: None,
            code: Vec::new(),
            coverage: PrimitiveSearchCoverageV1 {
                exact: PrimitiveLaneStatusV1::Complete(PrimitiveLaneCompleteV1::Complete),
                lexical: PrimitiveLaneStatusV1::Complete(PrimitiveLaneCompleteV1::Complete),
                graph: PrimitiveLaneStatusV1::Complete(PrimitiveLaneCompleteV1::Complete),
                recall: PrimitiveRecallV1::Full,
            },
            memory_matches: Vec::new(),
            memory_graph_coverage: None,
            memory_matches_error: None,
            verified_graph_evidence: None,
            plan: Some(ContextPlanV1 {
                extension_points: vec![ContextExtensionPointV1 {
                    name: "Store".to_owned(),
                    kind: "trait".to_owned(),
                    file: "src/store.rs".to_owned(),
                    line: 3,
                    implementor_count: 2,
                }],
                test_files: Some(Vec::new()),
            }),
            retrieval: ContextRetrievalPlanV1 {
                search: ContextStageV1::ran(20, 1, true),
                graph: ContextStageV1::ran(20, 1, false),
                related: ContextStageV1::ran(20, 0, false),
                code: ContextStageV1::NotRequested,
                memory: ContextStageV1::Unavailable,
            },
        }
    }

    #[test]
    fn context_renders_plan_markdown_and_records_memory_analytics_beside_it() {
        let analytics = InvocationAnalyticsV1 {
            context_memory: Some(ContextMemoryAnalyticsV1 {
                include_memory: true,
                limit: 3,
                min_trust_millionths: 500_000,
                fact_ids: vec!["fact.one".to_owned()],
                error: None,
            }),
            pr_context: None,
        };
        let completion = |analytics| GraphToolCompletionV1 {
            result: GraphToolResultV1::Context(Box::new(plan_context())),
            touched_files: Vec::new(),
            code_graph: None,
            analytics,
            cost: None,
        };
        let markdown = render_graph_tool(None, &json!({}), completion(Some(analytics.clone())))
            .expect("markdown");
        let text = texts(&markdown).join("");
        assert!(
            text.contains(
                "### Extension Points\n- **Store** (trait) - src/store.rs:3 (2 implementors)\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("### Test Coverage\n_No test files found covering these modules._\n"),
            "{text}"
        );
        assert!(text.contains("seen_node_ids: [\"symbol.store\"]"), "{text}");
        assert!(
            text.contains("\n_Budget-bound, more may exist: search 1/20 (`max_nodes`)._\n"),
            "{text}"
        );
        assert_eq!(
            markdown.internal_analytics(),
            Some(&json!({"context_memory": {
                "include_memory": true,
                "limit": 3,
                "min_trust": 0.5,
                "match_count": 1,
                "fact_ids": ["fact.one"],
                "error": null,
            }}))
        );

        let as_json = render_graph_tool(
            None,
            &json!({"format": "json"}),
            completion(Some(analytics)),
        )
        .expect("json");
        let payload: serde_json::Value =
            serde_json::from_str(&texts(&as_json)[0]).expect("json payload");
        assert_eq!(
            payload["plan"]["extension_points"][0]["implementor_count"],
            2
        );
        assert_eq!(
            payload["retrieval"],
            json!({
                "search": {"state": "ran", "budget": 20, "admitted": 1, "truncated": true},
                "graph": {"state": "ran", "budget": 20, "admitted": 1, "truncated": false},
                "related": {"state": "ran", "budget": 20, "admitted": 0, "truncated": false},
                "code": {"state": "not_requested"},
                "memory": {"state": "unavailable"},
            })
        );
        assert!(payload.get("context_memory").is_none(), "{payload}");
        assert!(payload.get("analytics").is_none(), "{payload}");
    }

    fn fresh_worktree_seat() -> ServedCodeGraphGenerationV1 {
        ServedCodeGraphGenerationV1 {
            generation: "generation.render.worktree".to_owned(),
            worktree: Some(PrimitiveSearchFreshnessV1 {
                state: PrimitiveFreshnessStateV1::Fresh,
                indexing: None,
            }),
            freshness: CodeGraphReadFreshnessV1::Current,
        }
    }

    #[test]
    fn a_json_array_read_carries_the_worktree_verdict_beside_its_array() {
        let symbols = vec![DerivesSymbolV1 {
            node_id: "symbol.config".to_owned(),
            name: "Config".to_owned(),
            qualified_name: "crate::Config".to_owned(),
            kind: "struct".to_owned(),
            file: "src/lib.rs".to_owned(),
            line: 1,
            derives: Vec::new(),
        }];
        let rendered = render_graph_tool(
            None,
            &json!({"format": "json"}),
            GraphToolCompletionV1 {
                result: GraphToolResultV1::Derives(DerivesResultV1(symbols.clone())),
                touched_files: Vec::new(),
                code_graph: Some(fresh_worktree_seat()),
                analytics: None,
                cost: None,
            },
        )
        .expect("rendered");
        let blocks = texts(&rendered);
        let body: Value = serde_json::from_str(&blocks[0]).expect("array body");
        assert_eq!(body, serde_json::to_value(&symbols).expect("symbols"));
        assert_eq!(rendered.structured_result(), Some(&body));
        let verdicts: Vec<Value> = blocks[1..]
            .iter()
            .filter_map(|block| block.strip_prefix("\ncode_graph_freshness: "))
            .map(|verdict| serde_json::from_str(verdict).expect("verdict json"))
            .collect();
        assert_eq!(verdicts, vec![json!({"state": "fresh"})]);
    }

    #[test]
    fn a_truncated_json_read_stores_the_worktree_verdict_in_its_full_body() {
        let root = tempfile::tempdir().expect("project");
        let markers = (0..400)
            .map(|line| TodoMarkerV1 {
                kind: "TODO".to_owned(),
                file: "src/lib.rs".to_owned(),
                line,
                text: format!("// TODO: probe the truncated freshness body {line}"),
                enclosing: None,
            })
            .collect::<Vec<_>>();
        let rendered = render_graph_tool(
            Some(root.path()),
            &json!({"format": "json"}),
            GraphToolCompletionV1 {
                result: GraphToolResultV1::Todos(TodosResultV1 {
                    match_count: 400,
                    by_kind: BTreeMap::from([("TODO".to_owned(), 400)]),
                    markers,
                    freshness: None,
                }),
                touched_files: Vec::new(),
                code_graph: Some(fresh_worktree_seat()),
                analytics: None,
                cost: None,
            },
        )
        .expect("rendered");
        let envelope: Value = serde_json::from_str(&texts(&rendered)[0]).expect("envelope");
        assert_eq!(envelope["truncated"], true, "{envelope}");
        let handle = envelope["handle"].as_str().expect("retrieval handle");
        let ResponseHandleLookup::Found(stored) =
            retrieve_response_handle(root.path(), handle, current_timestamp()).expect("lookup")
        else {
            panic!("stored body expected");
        };
        let full: Value = serde_json::from_str(&stored.content).expect("full body");
        assert_eq!(full["freshness"], json!({"state": "fresh"}));
        assert_eq!(full["match_count"], 400);
    }

    #[test]
    fn a_current_seat_renders_only_the_accounting_footer() {
        let root = tempfile::tempdir().expect("project");
        std::fs::create_dir_all(root.path().join("src")).expect("src");
        std::fs::write(root.path().join("src/lib.rs"), "b".repeat(40)).expect("source");
        let mut rendered = render_graph_tool(
            Some(root.path()),
            &json!({"format": "json"}),
            todos_completion(Some(ServedCodeGraphGenerationV1 {
                generation: "generation.render.current".to_owned(),
                worktree: None,
                freshness: CodeGraphReadFreshnessV1::Current,
            })),
        )
        .expect("rendered");
        crate::tools::response_trailers::account_tool_result(Some(root.path()), &mut rendered);
        let blocks = texts(&rendered);
        assert_eq!(blocks.len(), 2, "{blocks:?}");
        assert_eq!(
            blocks[1],
            format!(
                "\ntracedecay_metrics: before=10 after={}",
                blocks[0].len() / 4
            )
        );
    }
}
