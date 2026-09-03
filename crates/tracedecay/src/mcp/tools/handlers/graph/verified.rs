//! Context markdown and plan-context enrichment that stay on the composition
//! root because they reach root `is_test_file` and context-heading layout.

use std::collections::HashSet;
use std::fmt::Write as _;

use serde_json::Value;
use tracedecay_domain::RelationEdgeKindV1;
use tracedecay_domain::code_intelligence::NodeKind;
use tracedecay_domain::errors::Result;
use tracedecay_graph_query::{CodeGraphSymbolSummaryV1, VerifiedGraphQuery};
use tracedecay_mcp::context_headings::{
    CONTEXT_CODE_HEADING, CONTEXT_ENTRY_POINTS_HEADING, CONTEXT_RELATED_SYMBOLS_HEADING,
};
use tracedecay_mcp::handlers::graph::{
    GRAPH_RELATION_READ_LIMIT, graph_symbol_corrupt, required_graph_file_path,
    required_graph_metadata, single_graph_adjacency_batch, traverse_verified_neighbors,
};
use tracedecay_mcp::path_tree::format_compact_path_list;

#[hotpath::measure(label = "mcp.graph.context_markdown")]
pub(super) fn verified_context_markdown(
    task: &str,
    symbols: &[Value],
    related: &[Value],
    code: &[Value],
) -> Result<String> {
    let mut output = format!("# Context for {task}\n\n{CONTEXT_CODE_HEADING}\n");
    if code.is_empty() {
        for symbol in symbols {
            let _ = writeln!(
                output,
                "- **{}** ({}) — {}:{}",
                field_str(symbol, "name")?,
                field_str(symbol, "kind")?,
                field_str(symbol, "file")?,
                field_i64(symbol, "start_line")?,
            );
        }
    } else {
        for block in code {
            let _ = writeln!(
                output,
                "#### {}:{}\n```\n{}\n```",
                field_str(block, "file")?,
                field_i64(block, "start_line")?,
                field_str(block, "code")?,
            );
        }
    }
    output.push('\n');
    output.push_str(CONTEXT_RELATED_SYMBOLS_HEADING);
    output.push('\n');
    for symbol in related {
        let _ = writeln!(
            output,
            "- **{}** ({}) — {}:{}",
            field_str(symbol, "name")?,
            field_str(symbol, "kind")?,
            field_str(symbol, "file")?,
            field_i64(symbol, "start_line")?,
        );
    }
    output.push('\n');
    output.push_str(CONTEXT_ENTRY_POINTS_HEADING);
    output.push('\n');
    for symbol in symbols.iter().take(5) {
        let _ = writeln!(output, "- `{}`", field_str(symbol, "qualified_name")?);
    }
    Ok(output)
}

#[hotpath::measure(label = "mcp.graph.plan_context")]
pub(super) fn append_verified_plan_context(
    graph: &VerifiedGraphQuery,
    symbols: &[CodeGraphSymbolSummaryV1],
    output: &mut String,
) -> Result<()> {
    output.push_str("\n### Extension Points\n");
    let mut found_extension = false;
    for node in symbols {
        let metadata = required_graph_metadata(node)?;
        if matches!(
            NodeKind::from_str(&metadata.kind),
            Some(NodeKind::Trait | NodeKind::Interface | NodeKind::InterfaceType)
        ) && matches!(metadata.visibility.as_str(), "pub" | "public")
        {
            let implementors = single_graph_adjacency_batch(graph.callers(
                std::slice::from_ref(&node.occurrence),
                &[RelationEdgeKindV1::Implements],
                GRAPH_RELATION_READ_LIMIT,
            )?)?;
            let _ = writeln!(
                output,
                "- **{}** ({}) - {}:{} ({} implementors)",
                metadata.simple_name,
                metadata.kind,
                required_graph_file_path(node)?,
                metadata.start_line.saturating_add(1),
                implementors.len(),
            );
            found_extension = true;
        }
    }
    if !found_extension {
        output.push_str("_No public traits/interfaces found in context._\n");
    }
    if symbols.is_empty() {
        return Ok(());
    }
    output.push_str("\n### Test Coverage\n");
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
            if crate::tracedecay::is_test_file(file_path) || annotated_files.contains(file_path) {
                test_files.insert(file_path.to_owned());
            }
        }
    }
    if test_files.is_empty() {
        output.push_str("_No test files found covering these modules._\n");
    } else {
        let mut sorted = test_files.into_iter().collect::<Vec<_>>();
        sorted.sort();
        output.push_str(&format_compact_path_list(
            sorted.iter().map(String::as_str),
            "- ",
            "",
        ));
        output.push('\n');
    }
    Ok(())
}

fn field_str<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value.get(key).and_then(Value::as_str).ok_or_else(|| {
        graph_symbol_corrupt(format!(
            "verified context value has no string field '{key}'"
        ))
    })
}

fn field_i64(value: &Value, key: &str) -> Result<i64> {
    value.get(key).and_then(Value::as_i64).ok_or_else(|| {
        graph_symbol_corrupt(format!(
            "verified context value has no integer field '{key}'"
        ))
    })
}
