//! Graph- and git-backed reads and reports served by the project's graph-tool
//! owner.
//!
//! The owner computes each operation's typed catalog result; every surface
//! renders it here, so MCP and the CLI print the same tool result.

use std::path::Path;

use serde_json::Value;
use tracedecay_application::code_index::CodeIndexIgnoredDependencyAdmissionPortV1;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{CallableCodeOperationKind, callable_code_operation};
use tracedecay_contracts::retrieval::{
    DerivesResultV1, NodeResultV1, RenamePreviewPrimitiveOutcomeV1,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use crate::handlers::analysis::{compute_analysis_report, render_circular_md};
use crate::handlers::ast_grep::{compute_ast_grep_search, render_ast_grep_search};
use crate::handlers::git::{
    compute_affected, compute_branch_diff, compute_branch_list, compute_branch_search,
    compute_changelog, compute_commit_context, compute_diff_context, compute_pr_context,
    git_tool_failure_message,
};
use crate::handlers::graph::{
    compute_by_qualified_name, compute_context, compute_derives, compute_find_exact_symbol,
    compute_impact, compute_node, compute_redundancy, compute_rename_preview, compute_signature,
    compute_similar, not_found_tool_result, render_context,
};
use crate::handlers::grep::{compute_grep, render_grep};
use crate::handlers::health::{
    compute_dependency_depth, compute_dsm, compute_gini, compute_health, compute_test_map,
    compute_test_risk, render_dsm_md,
};
use crate::handlers::info::{
    compute_config, compute_files, compute_port_order, compute_port_status, compute_todos,
    render_files_md,
};
use crate::handlers::support::{
    decode_primitive_request, generic_tool_result, rendered_tool_result, text_tool_result,
    unknown_tool_error,
};
use crate::handlers::verified_read::{VerifiedGraphOpen, verified_read_operation as read};
use crate::tools::render;
use crate::tools::response_trailers::ResponseTrailer;
use crate::{McpToolContext, ToolResult};

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
            compute_context(ctx, open(read("context")?), args, scope_prefix).await
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
            compute_port_status(&open(read("port_status")?).await?, args).await
        }
        ApplicationSurfaceOperation::PortOrder => {
            compute_port_order(&open(read("port_order")?).await?, args).await
        }
        ApplicationSurfaceOperation::Todos => {
            compute_todos(&open(read("todos")?).await?, args, scope_prefix).await
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
            compute_analysis_report(ctx.project_root(), open, operation, args, scope_prefix).await
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
            compute_files(&open(operation).await?, request, scope_prefix).await
        }
        ApplicationSurfaceOperation::Config => compute_config(ctx.project_root(), args).await,
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
    let mut completion = match operation {
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
    }?;
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
    let mut rendered = match &result {
        GraphToolResultV1::Context(context) => render_context(response_handle_root, args, context)?,
        GraphToolResultV1::Node(NodeResultV1::NotFound(not_found))
        | GraphToolResultV1::RenamePreview(RenamePreviewPrimitiveOutcomeV1::NotFound(not_found)) => {
            not_found_tool_result(not_found)?
        }
        GraphToolResultV1::Dsm(_) => {
            let value = result.result_value()?;
            rendered_tool_result(response_handle_root, args, &value, Vec::new(), || {
                render_dsm_md(&value)
            })
        }
        GraphToolResultV1::Diagnose(_) => {
            let value = result.result_value()?;
            rendered_tool_result(response_handle_root, args, &value, Vec::new(), || {
                render::diagnostics_md(&value)
            })
        }
        GraphToolResultV1::Circular(circular) => rendered_tool_result(
            response_handle_root,
            args,
            &result.result_value()?,
            Vec::new(),
            || render_circular_md(circular),
        ),
        GraphToolResultV1::UnmountedFiles(_) => {
            let value = result.result_value()?;
            rendered_tool_result(response_handle_root, args, &value, Vec::new(), || {
                render::unmounted_files_md(&value)
            })
        }
        GraphToolResultV1::UnsafePatterns(_) => {
            let value = result.result_value()?;
            rendered_tool_result(response_handle_root, args, &value, Vec::new(), || {
                render::risky_patterns_md(&value)
            })
        }
        GraphToolResultV1::Derives(DerivesResultV1(symbols)) if symbols.is_empty() => {
            text_tool_result("No matching symbol found.", Vec::new())
        }
        GraphToolResultV1::Grep(grep) => render_grep(response_handle_root, args, grep)?,
        GraphToolResultV1::Files(files) => rendered_tool_result(
            response_handle_root,
            args,
            &result.result_value()?,
            Vec::new(),
            || render_files_md(files),
        ),
        GraphToolResultV1::AstGrepSearch(search) => {
            render_ast_grep_search(response_handle_root, args, search)?
        }
        _ => generic_tool_result(
            response_handle_root,
            args,
            &result.result_value()?,
            Vec::new(),
        ),
    };
    if let Some(message) = git_tool_failure_message(&result) {
        rendered = rendered
            .with_semantic_error(true)
            .with_failure_message(message);
    }
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use tracedecay_contracts::retrieval::{
        CodeGraphReadFreshnessV1, ContextExtensionPointV1, ContextModeV1, ContextPlanV1,
        ContextResultV1, ContextRetrievalPlanV1, ContextStageV1, PrimitiveFreshnessStateV1,
        PrimitiveLaneCompleteV1, PrimitiveLaneStatusV1, PrimitiveRecallV1,
        PrimitiveSearchCoverageV1, PrimitiveSearchFreshnessV1, PrimitiveSymbolLocationV1,
        ServedCodeGraphGenerationV1, TodoMarkerV1, TodosResultV1,
    };
    use tracedecay_contracts::{ContextMemoryAnalyticsV1, InvocationAnalyticsV1};
    use tracedecay_domain::UtcMicros;

    use super::*;

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
