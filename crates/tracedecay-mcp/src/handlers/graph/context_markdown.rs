//! Plan-context computation and the markdown every surface renders from a
//! typed context result.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;

use super::search_freshness::freshness_lines;
use super::{
    GRAPH_RELATION_READ_LIMIT, required_graph_file_path, required_graph_metadata,
    single_graph_adjacency_batch, traverse_verified_neighbors,
};
use crate::ToolResult;
use crate::context_headings::{
    CONTEXT_CODE_HEADING, CONTEXT_ENTRY_POINTS_HEADING, CONTEXT_EXTENSION_POINTS_HEADING,
    CONTEXT_RELATED_SYMBOLS_HEADING, CONTEXT_SEEN_NODE_IDS_LABEL, CONTEXT_TEST_COVERAGE_HEADING,
};
use crate::handlers::support::text_tool_result;
use crate::path_tree::format_compact_path_list;
use crate::tools::render::{self, Md};
use serde_json::Value;
use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_contracts::retrieval::{
    ContextCodeBlockV1, ContextExtensionPointV1, ContextLexicalAnchorV1, ContextPlanV1,
    ContextResultV1, ContextSearchMatchV1, PrimitiveSymbolLocationV1,
};
use tracedecay_domain::RelationEdgeKindV1;
use tracedecay_domain::code_intelligence::{NodeKind, Visibility};
use tracedecay_domain::errors::Result;
use tracedecay_graph_query::VerifiedGraphQuery;

use super::context_support::{context_markdown_lane_preview, insert_context_memory_section};
use super::lexical_routing::matched_anchor_line;
use super::search::append_coverage_md;
use super::search_evidence::append_verified_graph_evidence_md;

/// Renders a context result: the full markdown for JSON callers' fallback,
/// and a lane-bounded preview for markdown callers.
pub(crate) fn render_context(
    project_root: Option<&Path>,
    args: &Value,
    result: &ContextResultV1,
    touched_files: Vec<String>,
) -> Result<ToolResult> {
    let value = serde_json::to_value(result)?;
    let mut output = freshness_lines(&result.freshness);
    output.push_str(&context_markdown(
        &result.task,
        &result.symbols,
        &result.related_symbols,
        &result.code,
    ));
    if result.symbols.is_empty() {
        append_context_search_matches(&mut output, &result.search_matches);
    }
    append_context_lexical_anchors(&mut output, &result.lexical_anchors);
    insert_context_memory_section(
        &mut output,
        &result.memory_matches,
        result.memory_matches_error.as_deref(),
    );
    if let Some(plan) = &result.plan {
        append_plan_markdown(&mut output, plan);
    }
    if !result.symbols.is_empty() {
        let seen = result
            .symbols
            .iter()
            .map(|symbol| symbol.node_id.as_str())
            .collect::<Vec<_>>();
        let _ = write!(
            output,
            "\n{} {}\n",
            CONTEXT_SEEN_NODE_IDS_LABEL,
            serde_json::to_string(&seen)?
        );
    }
    let mut degradation = Md::new();
    append_coverage_md(&mut degradation, &value);
    append_verified_graph_evidence_md(&mut degradation, &value);
    let degradation = degradation.render();
    if !degradation.is_empty() {
        output.push('\n');
        output.push_str(&degradation);
    }
    let text = if render::wants_json(args) {
        render::finalize(project_root, args, &value, || output)
    } else {
        let preview = context_markdown_lane_preview(&output);
        render::markdown_preview_with_handle(project_root, &output, &preview)
    };
    Ok(text_tool_result(&text, touched_files))
}

fn context_markdown(
    task: &str,
    symbols: &[PrimitiveSymbolLocationV1],
    related: &[PrimitiveSymbolLocationV1],
    code: &[ContextCodeBlockV1],
) -> String {
    let mut output = format!("# Context for {task}\n\n{CONTEXT_CODE_HEADING}\n");
    if code.is_empty() {
        for symbol in symbols {
            let _ = writeln!(
                output,
                "- **{}** ({}), {}:{}",
                symbol.name, symbol.kind, symbol.file, symbol.start_line,
            );
        }
    } else {
        for block in code {
            let _ = writeln!(
                output,
                "#### {}:{}\n```\n{}\n```",
                block.file, block.start_line, block.code,
            );
        }
    }
    output.push('\n');
    output.push_str(CONTEXT_RELATED_SYMBOLS_HEADING);
    output.push('\n');
    for symbol in related {
        let _ = writeln!(
            output,
            "- **{}** ({}), {}:{}",
            symbol.name, symbol.kind, symbol.file, symbol.start_line,
        );
    }
    output.push('\n');
    output.push_str(CONTEXT_ENTRY_POINTS_HEADING);
    output.push('\n');
    for symbol in symbols.iter().take(5) {
        let _ = writeln!(output, "- `{}`", symbol.qualified_name);
    }
    output
}

fn append_context_search_matches(output: &mut String, matches: &[ContextSearchMatchV1]) {
    if matches.is_empty() {
        return;
    }
    output.push_str("\n### Available Code Search Matches\n");
    for search_match in matches {
        let _ = writeln!(
            output,
            "- **{}** ({}), `{}` · rank {} · utility {}",
            search_match.name,
            search_match.kind,
            search_match.file,
            search_match.rank,
            search_match.utility_micros,
        );
    }
}

/// Every caller anchor's outcome, so an anchor that matched nothing is never
/// mistaken for one that was outranked.
fn append_context_lexical_anchors(output: &mut String, anchors: &[ContextLexicalAnchorV1]) {
    if anchors.is_empty() {
        return;
    }
    output.push_str("\n### Lexical Anchors\n");
    for anchor in anchors {
        let line = match anchor {
            ContextLexicalAnchorV1::Matched {
                anchor,
                matched,
                admitted,
                dropped,
            } => matched_anchor_line(anchor, *matched, *admitted, dropped),
            ContextLexicalAnchorV1::Unmatched { anchor } => format!("- `{anchor}`: no matches"),
            ContextLexicalAnchorV1::NotServed { anchor } => {
                format!("- `{anchor}`: route not served")
            }
        };
        output.push_str(&line);
        output.push('\n');
    }
}

fn append_plan_markdown(output: &mut String, plan: &ContextPlanV1) {
    let _ = write!(output, "\n{CONTEXT_EXTENSION_POINTS_HEADING}\n");
    if plan.extension_points.is_empty() {
        output.push_str("_No public traits/interfaces found in context._\n");
    }
    for point in &plan.extension_points {
        let _ = writeln!(
            output,
            "- **{}** ({}) - {}:{} ({} implementors)",
            point.name, point.kind, point.file, point.line, point.implementor_count,
        );
    }
    let Some(test_files) = &plan.test_files else {
        return;
    };
    let _ = write!(output, "\n{CONTEXT_TEST_COVERAGE_HEADING}\n");
    if test_files.is_empty() {
        output.push_str("_No test files found covering these modules._\n");
    } else {
        output.push_str(&format_compact_path_list(
            test_files.iter().map(String::as_str),
            "- ",
            "",
        ));
        output.push('\n');
    }
}

/// The plan section for the selected symbols: public traits and interfaces
/// with their implementor counts, and the test files reaching the selection.
#[hotpath::measure(label = "mcp.graph.plan_context")]
pub(super) fn verified_plan_context(
    graph: &VerifiedGraphQuery,
    symbols: &[CodeGraphSymbolSummaryV1],
) -> Result<ContextPlanV1> {
    let mut extension_points = Vec::new();
    for node in symbols {
        let metadata = required_graph_metadata(node)?;
        if matches!(
            NodeKind::from_str(&metadata.kind),
            Some(NodeKind::Trait | NodeKind::Interface | NodeKind::InterfaceType)
        ) && Visibility::from_str(&metadata.visibility) == Some(Visibility::Pub)
        {
            let implementors = single_graph_adjacency_batch(graph.callers(
                std::slice::from_ref(&node.occurrence),
                &[RelationEdgeKindV1::Implements],
                GRAPH_RELATION_READ_LIMIT,
            )?)?;
            extension_points.push(ContextExtensionPointV1 {
                name: metadata.simple_name.clone(),
                kind: metadata.kind.clone(),
                file: required_graph_file_path(node)?.to_owned(),
                line: metadata.start_line.saturating_add(1),
                implementor_count: implementors.len(),
            });
        }
    }
    if symbols.is_empty() {
        return Ok(ContextPlanV1 {
            extension_points,
            test_files: None,
        });
    }
    let annotated_files = graph.test_annotated_logical_files(None, 500_000, 2_000_000)?;
    let mut test_files = HashSet::new();
    for symbol in symbols {
        for caller in traverse_verified_neighbors(
            graph,
            symbol.occurrence.clone(),
            &[RelationEdgeKindV1::Calls],
            true,
            2,
        )? {
            let file_path = required_graph_file_path(&caller.symbol)?;
            if tracedecay_code_index::is_test_file(file_path) || annotated_files.contains(file_path)
            {
                test_files.insert(file_path.to_owned());
            }
        }
    }
    let mut test_files = test_files.into_iter().collect::<Vec<_>>();
    test_files.sort();
    Ok(ContextPlanV1 {
        extension_points,
        test_files: Some(test_files),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_markdown_renders_the_typed_sections() {
        let mut output = String::new();
        append_plan_markdown(
            &mut output,
            &ContextPlanV1 {
                extension_points: vec![ContextExtensionPointV1 {
                    name: "Store".to_owned(),
                    kind: "trait".to_owned(),
                    file: "src/store.rs".to_owned(),
                    line: 3,
                    implementor_count: 2,
                }],
                test_files: Some(vec!["tests/store.rs".to_owned()]),
            },
        );
        assert!(output.starts_with(
            "\n### Extension Points\n- **Store** (trait) - src/store.rs:3 (2 implementors)\n\n### Test Coverage\n"
        ));
        assert!(output.contains("tests/store.rs"), "{output}");

        let mut empty = String::new();
        append_plan_markdown(
            &mut empty,
            &ContextPlanV1 {
                extension_points: Vec::new(),
                test_files: None,
            },
        );
        assert_eq!(
            empty,
            "\n### Extension Points\n_No public traits/interfaces found in context._\n"
        );

        let mut uncovered = String::new();
        append_plan_markdown(
            &mut uncovered,
            &ContextPlanV1 {
                extension_points: Vec::new(),
                test_files: Some(Vec::new()),
            },
        );
        assert!(
            uncovered
                .ends_with("\n### Test Coverage\n_No test files found covering these modules._\n"),
            "{uncovered}"
        );
    }
}
