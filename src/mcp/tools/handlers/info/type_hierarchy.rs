//! `tracedecay_type_hierarchy` — implements/extends tree rooted at a node.

use super::*;

/// Handles `tracedecay_type_hierarchy` tool calls.
pub(crate) async fn handle_type_hierarchy(cg: &TraceDecay, args: Value) -> Result<ToolResult> {
    let node_id = require_node_id(&args)?;
    let max_depth = args
        .get("max_depth")
        .and_then(serde_json::Value::as_u64)
        .map_or(5, |v| v.min(10) as usize);

    let root = cg
        .get_node(node_id)
        .await?
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("node not found: {node_id}"),
        })?;

    let mut tree = format!(
        "{} ({}) -- {}:{}\n",
        root.name,
        root.kind.as_str(),
        root.file_path,
        root.start_line
    );
    let mut all_files: Vec<String> = vec![root.file_path.clone()];

    // Recursively build the hierarchy
    build_type_tree(cg, &root.id, max_depth, 0, &mut tree, &mut all_files).await?;

    let touched_files = unique_file_paths(all_files.iter().map(std::string::String::as_str));
    let payload = json!({
        "root": {
            "id": root.id,
            "name": root.name,
            "kind": root.kind.as_str(),
            "file": root.file_path,
            "line": root.start_line,
        },
        "max_depth": max_depth,
        "tree": tree,
    });
    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &payload,
        touched_files,
        || render_type_hierarchy_md(&root, max_depth, &tree),
    ))
}

/// Renders from the root node and tree the handler already holds, rather than
/// reading them back out of the JSON payload it just built.
fn render_type_hierarchy_md(root: &crate::types::Node, max_depth: usize, tree: &str) -> String {
    let mut md = Md::new();
    md.heading(2, "Type Hierarchy");
    md.field(
        "root",
        &format!(
            "{} ({}) - {}:{}",
            root.name,
            root.kind.as_str(),
            root.file_path,
            root.start_line
        ),
    );
    md.field("max_depth", &max_depth.to_string());
    md.blank().code("text", tree);
    md.render()
}

/// Recursively appends type hierarchy lines to the output string.
fn build_type_tree<'a>(
    cg: &'a TraceDecay,
    node_id: &'a str,
    max_depth: usize,
    depth: usize,
    output: &'a mut String,
    all_files: &'a mut Vec<String>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        if depth >= max_depth {
            return Ok(());
        }

        let incoming = cg.get_incoming_edges(node_id).await?;
        let pad = "  ".repeat(depth);

        for edge in &incoming {
            if !matches!(
                edge.kind,
                crate::types::EdgeKind::Implements | crate::types::EdgeKind::Extends
            ) {
                continue;
            }
            if let Ok(Some(child)) = cg.get_node(&edge.source).await {
                let _ = writeln!(
                    output,
                    "{}|- {} {} ({}) -- {}:{}",
                    pad,
                    edge.kind.as_str(),
                    child.name,
                    child.kind.as_str(),
                    child.file_path,
                    child.start_line,
                );
                all_files.push(child.file_path.clone());
                build_type_tree(cg, &child.id, max_depth, depth + 1, output, all_files).await?;
            }
        }
        Ok(())
    })
}
