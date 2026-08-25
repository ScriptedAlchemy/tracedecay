//! `tracedecay_type_hierarchy` — verified implements/extends tree rooted at a symbol.

use super::*;
use tracedecay_domain::{RelationEdgeKindV1, SymbolOccurrenceId};

pub(crate) async fn handle_type_hierarchy(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    let node_id = require_node_id(&args)?;
    let occurrence =
        SymbolOccurrenceId::new(node_id.to_owned()).map_err(|error| TraceDecayError::Config {
            message: format!("invalid node_id '{node_id}': {error}"),
        })?;
    let max_depth = args
        .get("max_depth")
        .and_then(serde_json::Value::as_u64)
        .map_or(5, |value| value.min(10) as usize);
    let root = graph
        .symbol_summary(&occurrence)?
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("node not found in verified generation: {node_id}"),
        })?;
    let (root_metadata, root_file) = required_symbol_parts(&root)?;

    let mut tree = format!(
        "{} ({}) -- {}:{}\n",
        root_metadata.simple_name,
        root_metadata.kind,
        root_file,
        root_metadata.start_line.saturating_add(1)
    );
    let root_display = format!(
        "{} ({}) - {}:{}",
        root_metadata.simple_name,
        root_metadata.kind,
        root_file,
        root_metadata.start_line.saturating_add(1)
    );
    let mut all_files = vec![root_file.to_owned()];
    let mut seen = HashSet::from([occurrence.clone()]);
    build_type_tree(
        graph,
        &occurrence,
        max_depth,
        0,
        &mut tree,
        &mut all_files,
        &mut seen,
    )?;

    let touched_files = unique_file_paths(all_files.iter().map(String::as_str));
    let payload = json!({
        "root": {
            "id": root.occurrence.as_str(),
            "name": root_metadata.simple_name,
            "kind": root_metadata.kind,
            "file": root_file,
            "line": root_metadata.start_line.saturating_add(1),
        },
        "max_depth": max_depth,
        "tree": tree,
    });
    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &payload,
        touched_files,
        || render_type_hierarchy_md(&root_display, max_depth, &tree),
    ))
}

fn render_type_hierarchy_md(root_display: &str, max_depth: usize, tree: &str) -> String {
    let mut md = Md::new();
    md.heading(2, "Type Hierarchy");
    md.field("root", root_display);
    md.field("max_depth", &max_depth.to_string());
    md.blank().code("text", tree);
    md.render()
}

fn build_type_tree(
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    node_id: &SymbolOccurrenceId,
    max_depth: usize,
    depth: usize,
    output: &mut String,
    all_files: &mut Vec<String>,
    seen: &mut HashSet<SymbolOccurrenceId>,
) -> Result<()> {
    if depth >= max_depth {
        return Ok(());
    }
    let mut batches = graph.callers(
        std::slice::from_ref(node_id),
        &[RelationEdgeKindV1::Implements, RelationEdgeKindV1::Extends],
        INFO_RELATION_LIMIT,
    )?;
    if batches.len() != 1 {
        return Err(info_graph_error(
            "verified-type-hierarchy-adjacency-invalid",
            "verified graph did not return exactly one hierarchy adjacency batch",
        ));
    }
    let pad = "  ".repeat(depth);
    for edge in batches.remove(0) {
        if !seen.insert(edge.neighbor.occurrence.clone()) {
            continue;
        }
        let (metadata, file) = required_symbol_parts(&edge.neighbor)?;
        let relation = match edge.edge.kind {
            RelationEdgeKindV1::Implements => "implements",
            RelationEdgeKindV1::Extends => "extends",
            _ => {
                return Err(info_graph_error(
                    "verified-type-hierarchy-relation-invalid",
                    "verified graph returned a non-hierarchy relation",
                ));
            }
        };
        writeln!(
            output,
            "{pad}|- {relation} {} ({}) -- {}:{}",
            metadata.simple_name,
            metadata.kind,
            file,
            metadata.start_line.saturating_add(1),
        )
        .map_err(|error| TraceDecayError::Config {
            message: format!("cannot render type hierarchy: {error}"),
        })?;
        all_files.push(file.to_owned());
        build_type_tree(
            graph,
            &edge.neighbor.occurrence,
            max_depth,
            depth + 1,
            output,
            all_files,
            seen,
        )?;
    }
    Ok(())
}
