//! Plan-context computation and the markdown every surface renders from a
//! typed context result.

use std::collections::{HashMap, HashSet};
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
use crate::tools::render::{self, Md, RankedRow, ranked_rows_md};
use serde_json::Value;
use tracedecay_code_index::graph_projection::CodeGraphSymbolSummaryV1;
use tracedecay_contracts::retrieval::{
    ContextCodeBlockV1, ContextExtensionPointV1, ContextLexicalAnchorV1, ContextPlanV1,
    ContextRelatedOmissionV1, ContextResultV1, ContextRetrievalPlanV1, ContextSearchMatchV1,
    ContextStageV1, PrimitiveSymbolLocationV1,
};
use tracedecay_domain::RelationEdgeKindV1;
use tracedecay_domain::code_intelligence::{NodeKind, Visibility};
use tracedecay_domain::errors::Result;
use tracedecay_graph_query::VerifiedGraphQuery;

use super::context_support::{context_markdown_lane_preview, insert_context_memory_section};
use super::lexical_routing::matched_anchor_line;
use super::search::append_coverage_md;
use super::search_evidence::append_verified_graph_evidence_md;

/// Renders a context result: the typed JSON for JSON callers, and a
/// lane-bounded markdown preview otherwise.
pub(crate) fn render_context(
    response_handle_root: Option<&Path>,
    args: &Value,
    result: &ContextResultV1,
) -> Result<ToolResult> {
    let value = serde_json::to_value(result)?;
    if render::wants_json(args) {
        let text = render::finalize(response_handle_root, args, &value, String::new);
        return Ok(text_tool_result(&text, Vec::new()));
    }
    let mut output = freshness_lines(&result.freshness);
    let (code_md, kept_symbols) = if result.code.is_empty() {
        let rows = context_symbol_rows(&result.symbols, &result.search_matches);
        let (rows_md, kept) = ranked_rows_md(response_handle_root, &result.task, &rows);
        (rows_md, &result.symbols[..kept])
    } else {
        (
            context_code_blocks_md(&result.code),
            result.symbols.as_slice(),
        )
    };
    output.push_str(&context_markdown(
        &result.task,
        &code_md,
        kept_symbols,
        &result.related_symbols,
        result.related_omission.as_ref(),
        &result.oversized_symbols,
    ));
    if result.symbols.is_empty() {
        append_context_search_matches(
            &mut output,
            response_handle_root,
            &result.task,
            &result.search_matches,
        );
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
    if !kept_symbols.is_empty() {
        let seen = kept_symbols
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
    append_retrieval_budget_md(&mut output, &result.retrieval);
    let preview = context_markdown_lane_preview(&output);
    let text = render::markdown_preview_with_handle(response_handle_root, &output, &preview);
    Ok(text_tool_result(&text, Vec::new()))
}

/// The selected symbols as ranked rows, in selection order, rendered as
/// bare locations: a ranking column would cost more bytes than the trim
/// saves. Selection follows search order, so each row ranks by its
/// position and scores the utility of the search match that selected it.
/// A symbol no search match selected has no utility, and the trim keeps it.
fn context_symbol_rows(
    symbols: &[PrimitiveSymbolLocationV1],
    matches: &[ContextSearchMatchV1],
) -> Vec<RankedRow> {
    let by_anchor = matches
        .iter()
        .map(|search_match| (search_match.anchor_id.as_str(), search_match))
        .collect::<HashMap<_, _>>();
    symbols
        .iter()
        .zip(1_u64..)
        .map(|(symbol, rank)| {
            // A search-hydrated symbol's node id is its match's anchor; a
            // graph-selected symbol's is the occurrence its anchor names.
            let search_match = by_anchor
                .get(symbol.node_id.as_str())
                .or_else(|| by_anchor.get(format!("code-symbol:{}", symbol.node_id).as_str()));
            RankedRow {
                rank,
                utility_micros: search_match.map(|search_match| search_match.utility_micros),
                name: symbol.name.clone(),
                anchored: search_match.is_some_and(|search_match| !search_match.anchors.is_empty()),
                markdown: format!(
                    "- **{}** ({}), {}:{}\n",
                    symbol.name, symbol.kind, symbol.file, symbol.start_line,
                ),
            }
        })
        .collect()
}

/// The anchor routes that ranked a match, in the search row's `via` form.
fn anchor_route_suffix(anchors: &[String]) -> String {
    if anchors.is_empty() {
        return String::new();
    }
    let routes = anchors
        .iter()
        .map(|anchor| format!("anchor:{anchor}"))
        .collect::<Vec<_>>();
    format!(" · via {}", routes.join(", "))
}

fn context_code_blocks_md(code: &[ContextCodeBlockV1]) -> String {
    let mut output = String::new();
    for block in code {
        let _ = writeln!(output, "#### {}:{}", block.file, block.start_line);
        let mut code = Md::new();
        code.code("", &block.code);
        output.push_str(&code.render());
    }
    output
}

/// `code_md` is the rendered Code section; `symbols` are the selected
/// symbols it shows, which name the entry points.
fn context_markdown(
    task: &str,
    code_md: &str,
    symbols: &[PrimitiveSymbolLocationV1],
    related: &[PrimitiveSymbolLocationV1],
    related_omission: Option<&ContextRelatedOmissionV1>,
    oversized: &[PrimitiveSymbolLocationV1],
) -> String {
    let mut output = format!("# Context for {task}\n\n{CONTEXT_CODE_HEADING}\n{code_md}");
    for symbol in oversized {
        let _ = writeln!(
            output,
            "- **{}** ({}), {}:{}-{} is larger than one code block; read it with \
             `tracedecay_source_body` or list its members with `tracedecay_source_outline`.",
            symbol.name, symbol.kind, symbol.file, symbol.start_line, symbol.end_line,
        );
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
    if let Some(omission) = related_omission {
        let at_least = if omission.total_is_lower_bound {
            "at least "
        } else {
            ""
        };
        let _ = writeln!(
            output,
            "- {} more omitted of {at_least}{} ranked related symbols",
            omission.omitted, omission.total,
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

/// A match an anchor route ranked stays on the page: the anchor receipt
/// counts it as returned.
fn append_context_search_matches(
    output: &mut String,
    response_handle_root: Option<&Path>,
    task: &str,
    matches: &[ContextSearchMatchV1],
) {
    if matches.is_empty() {
        return;
    }
    output.push_str("\n### Available Code Search Matches\n");
    let rows = matches
        .iter()
        .map(|search_match| RankedRow {
            rank: u64::from(search_match.rank),
            utility_micros: Some(search_match.utility_micros),
            name: search_match.name.clone(),
            anchored: !search_match.anchors.is_empty(),
            markdown: format!(
                "- **{}** ({}), `{}` · rank {} · utility {}{}\n",
                search_match.name,
                search_match.kind,
                search_match.file,
                search_match.rank,
                search_match.utility_micros,
                anchor_route_suffix(&search_match.anchors),
            ),
        })
        .collect::<Vec<_>>();
    output.push_str(&ranked_rows_md(response_handle_root, task, &rows).0);
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

/// Names every stage a budget stopped, with the argument that raises it. An
/// answer no budget bounded renders nothing.
fn append_retrieval_budget_md(output: &mut String, retrieval: &ContextRetrievalPlanV1) {
    let bounded = [
        ("search", retrieval.search, "max_nodes"),
        ("related", retrieval.related, "max_nodes"),
        ("code", retrieval.code, "max_code_blocks"),
        ("memory", retrieval.memory, "memory_limit"),
    ]
    .into_iter()
    .filter_map(|(stage, state, argument)| match state {
        ContextStageV1::Ran {
            budget,
            admitted,
            truncated: true,
        } => Some(format!("{stage} {admitted}/{budget} (`{argument}`)")),
        _ => None,
    })
    .collect::<Vec<_>>();
    if !bounded.is_empty() {
        let _ = writeln!(
            output,
            "\n_Budget-bound, more may exist: {}._",
            bounded.join(", ")
        );
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
#[tracing::instrument(name = "mcp.graph.plan_context", level = "trace", skip_all)]
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
    let mut test_files = HashSet::new();
    let mut unnamed_caller_files = HashSet::new();
    for symbol in symbols {
        for caller in traverse_verified_neighbors(
            graph,
            symbol.occurrence.clone(),
            &[RelationEdgeKindV1::Calls],
            true,
            2,
        )? {
            let file_path = required_graph_file_path(&caller.symbol)?;
            if tracedecay_code_index::is_test_file(file_path) {
                test_files.insert(file_path.to_owned());
            } else {
                unnamed_caller_files.insert(file_path.to_owned());
            }
        }
    }
    test_files.extend(graph.test_annotated_logical_files(
        &unnamed_caller_files,
        500_000,
        2_000_000,
    )?);
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

    fn search_matches() -> Vec<ContextSearchMatchV1> {
        (1..=10)
            .map(|rank| ContextSearchMatchV1 {
                anchor_id: format!("code-symbol:m{rank}"),
                name: format!("m{rank}"),
                qualified_name: format!("crate::m{rank}"),
                kind: "function".to_owned(),
                file: "src/lib.rs".to_owned(),
                exact_class: "exact_message".to_owned(),
                rank,
                utility_micros: if rank <= 3 { 2_000_000 } else { 1_000_000 },
                anchors: Vec::new(),
            })
            .collect()
    }

    #[test]
    fn search_matches_trim_the_weak_tail_after_the_last_anchored_match() {
        let root = tempfile::tempdir().expect("handle root");
        let mut trimmed = String::new();
        append_context_search_matches(
            &mut trimmed,
            Some(root.path()),
            "where is m1 defined",
            &search_matches(),
        );
        let (kept, marker) = trimmed
            .split_once("- _Trimmed 7 lower-ranked results (ranks 4-10, ")
            .expect("weak tail is trimmed");
        assert_eq!(
            kept,
            "\n### Available Code Search Matches\n\
             - **m1** (function), `src/lib.rs` · rank 1 · utility 2000000\n\
             - **m2** (function), `src/lib.rs` · rank 2 · utility 2000000\n\
             - **m3** (function), `src/lib.rs` · rank 3 · utility 2000000\n"
        );
        assert!(marker.contains("tracedecay_retrieve"), "{marker}");

        let mut matches = search_matches();
        matches[3].anchors = vec!["m4".to_owned()];
        let mut anchored = String::new();
        append_context_search_matches(
            &mut anchored,
            Some(root.path()),
            "where is m1 defined",
            &matches,
        );
        let (kept, _) = anchored
            .split_once("- _Trimmed 6 lower-ranked results (ranks 5-10, ")
            .expect("only the rows after the anchored match are trimmed");
        assert!(
            kept.ends_with(
                "- **m4** (function), `src/lib.rs` · rank 4 · utility 1000000 · via anchor:m4\n"
            ),
            "{kept}"
        );
    }

    /// Fourteen search-hydrated symbols: three strong, the rest weak.
    fn hydrated_symbols() -> (Vec<PrimitiveSymbolLocationV1>, Vec<ContextSearchMatchV1>) {
        let matches = (1..=14)
            .map(|rank| ContextSearchMatchV1 {
                anchor_id: format!("code-symbol:m{rank}"),
                name: format!("m{rank}"),
                qualified_name: format!("crate::m{rank}"),
                kind: "function".to_owned(),
                file: "crates/tracedecay-retrieval/src/ranked_symbols.rs".to_owned(),
                exact_class: "approximate".to_owned(),
                rank,
                utility_micros: if rank <= 3 { 2_000_000 } else { 1_000_000 },
                anchors: Vec::new(),
            })
            .collect::<Vec<_>>();
        let symbols = matches
            .iter()
            .map(|search_match| PrimitiveSymbolLocationV1 {
                node_id: search_match.anchor_id.clone(),
                name: search_match.name.clone(),
                qualified_name: search_match.qualified_name.clone(),
                kind: search_match.kind.clone(),
                file: search_match.file.clone(),
                start_line: search_match.rank,
                end_line: search_match.rank,
                unavailable_fields: Vec::new(),
            })
            .collect();
        (symbols, matches)
    }

    fn kept_symbol_rows(
        symbols: &[PrimitiveSymbolLocationV1],
        matches: &[ContextSearchMatchV1],
    ) -> (String, usize) {
        let root = tempfile::tempdir().expect("handle root");
        let rows = context_symbol_rows(symbols, matches);
        let (rendered, kept) = ranked_rows_md(Some(root.path()), "unrelated task", &rows);
        let kept_md = rendered
            .split_once("- _Trimmed ")
            .map_or(rendered.as_str(), |(kept, _)| kept)
            .to_owned();
        (kept_md, kept)
    }

    #[test]
    fn symbol_rows_keep_every_symbol_through_the_last_anchored_or_unranked_one() {
        let (symbols, matches) = hydrated_symbols();
        assert_eq!(kept_symbol_rows(&symbols, &matches).1, 3);

        let mut anchored = matches.clone();
        anchored[5].anchors = vec!["m6".to_owned()];
        let (kept_md, kept) = kept_symbol_rows(&symbols, &anchored);
        assert_eq!(kept, 6);
        assert!(
            kept_md.ends_with(
                "- **m6** (function), crates/tracedecay-retrieval/src/ranked_symbols.rs:6\n"
            ),
            "{kept_md}"
        );

        let unmatched = matches
            .iter()
            .filter(|search_match| search_match.rank != 6)
            .cloned()
            .collect::<Vec<_>>();
        let (kept_md, kept) = kept_symbol_rows(&symbols, &unmatched);
        assert_eq!(kept, 6, "a symbol no search match ranks is kept");
        assert!(
            kept_md.ends_with(
                "- **m6** (function), crates/tracedecay-retrieval/src/ranked_symbols.rs:6\n"
            ),
            "{kept_md}"
        );
    }

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
