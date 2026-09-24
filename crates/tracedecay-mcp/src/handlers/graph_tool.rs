//! Graph and port reads served by the project's graph-tool owner.
//!
//! The owner computes each operation's typed catalog result; every surface
//! renders it here, so MCP and the CLI print the same tool result.

use std::path::Path;

use serde_json::Value;
use tracedecay_contracts::graph_tool::{GraphToolCompletionV1, GraphToolResultV1};
use tracedecay_contracts::retrieval::{NodeResultV1, RenamePreviewPrimitiveOutcomeV1};
use tracedecay_domain::errors::Result;
use tracedecay_tool_catalog::ApplicationSurfaceOperation;

use crate::handlers::graph::{
    compute_context, compute_impact, compute_node, compute_redundancy, compute_rename_preview,
    compute_similar, not_found_tool_result, render_context,
};
use crate::handlers::info::{compute_port_order, compute_port_status, compute_todos};
use crate::handlers::support::{generic_tool_result, unknown_tool_error};
use crate::handlers::verified_read::{VerifiedGraphOpen, verified_read_operation as read};
use crate::tools::response_trailers::append_code_graph_freshness;
use crate::{McpToolContext, ToolResult};

/// Computes one graph-tool operation's typed result on the owner's side.
pub async fn compute_graph_tool(
    ctx: &McpToolContext<'_>,
    open: &VerifiedGraphOpen<'_>,
    operation: ApplicationSurfaceOperation,
    args: Value,
    scope_prefix: Option<&str>,
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
        operation => Err(unknown_tool_error(operation.mcp_tool_name())),
    }
}

/// Renders a typed graph-tool result as its tool result, with the stale-graph
/// trailer every surface appends.
///
/// Token accounting stays with the caller that owns raw-file sizes. The MCP
/// server uses its retained cache; the CLI stats the project files. Rendering
/// them here would mark the result accounted and skip that cache.
pub fn render_graph_tool(
    project_root: Option<&Path>,
    args: &Value,
    completion: GraphToolCompletionV1,
) -> Result<ToolResult> {
    let GraphToolCompletionV1 {
        result,
        touched_files,
        code_graph,
        analytics,
    } = completion;
    let mut rendered = match &result {
        GraphToolResultV1::Context(context) => {
            render_context(project_root, args, context, touched_files)?
        }
        GraphToolResultV1::Node(NodeResultV1::NotFound(not_found))
        | GraphToolResultV1::RenamePreview(RenamePreviewPrimitiveOutcomeV1::NotFound(not_found)) => {
            not_found_tool_result(not_found)?
        }
        _ => generic_tool_result(project_root, args, &result.result_value()?, touched_files),
    };
    if let Some(served) = &code_graph {
        append_code_graph_freshness(&mut rendered, served);
    }
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
        ContextResultV1, PrimitiveFreshnessStateV1, PrimitiveLaneCompleteV1,
        PrimitiveLaneStatusV1, PrimitiveRecallV1, PrimitiveSearchCoverageV1,
        PrimitiveSearchFreshnessV1, PrimitiveSymbolLocationV1, ServedCodeGraphGenerationV1,
        TodoMarkerV1, TodosResultV1,
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
        };
        let completion = |analytics| GraphToolCompletionV1 {
            result: GraphToolResultV1::Context(plan_context()),
            touched_files: Vec::new(),
            code_graph: None,
            analytics,
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

        let as_json = render_graph_tool(None, &json!({"format": "json"}), completion(Some(analytics)))
            .expect("json");
        let payload: serde_json::Value =
            serde_json::from_str(&texts(&as_json)[0]).expect("json payload");
        assert_eq!(payload["plan"]["extension_points"][0]["implementor_count"], 2);
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
            format!("\ntracedecay_metrics: before=10 after={}", blocks[0].len() / 4)
        );
    }
}
