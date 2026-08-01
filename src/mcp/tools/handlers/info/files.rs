//! `tracedecay_files` — indexed file listing with prefix and glob filters.

use super::*;

/// Handles `tracedecay_files` tool calls.
pub(crate) async fn handle_files(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    require_object_args(&args, "tracedecay_files")?;
    let mut files = cg.get_all_files().await?;
    files.sort_by(|a, b| a.path.cmp(&b.path));

    // Apply directory prefix filter
    if let Some(dir) = effective_path(&args, scope_prefix) {
        let prefix = if dir.ends_with('/') {
            dir.to_string()
        } else {
            format!("{dir}/")
        };
        files.retain(|f| f.path.starts_with(&prefix) || f.path == dir);
    }

    // Apply glob pattern filter
    if let Some(pat) = args.get("pattern").and_then(|v| v.as_str())
        && let Ok(glob) = glob::Pattern::new(pat)
    {
        files.retain(|f| glob.matches(&f.path));
    }

    // Listing files is metadata-only — no source code is served, so no tokens saved.
    let touched_files = vec![];

    let layout = args
        .get("layout")
        .and_then(|v| v.as_str())
        .unwrap_or("grouped");

    let file_values: Vec<Value> = files
        .iter()
        .map(|f| json!({ "path": f.path, "symbols": f.node_count, "bytes": f.size }))
        .collect();
    let payload = json!({
        "count": files.len(),
        "layout": layout,
        "files": file_values,
    });
    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &payload,
        touched_files,
        || render_files_md(&files, layout),
    ))
}

/// Renders the listing from the records the handler already holds, rather than
/// reading the keys back out of the JSON payload it just built.
fn render_files_md(files: &[FileRecord], layout: &str) -> String {
    let mut md = Md::new();
    md.heading(2, "Files");
    md.field("indexed files", &files.len().to_string());
    md.field("layout", layout);

    if files.is_empty() {
        md.blank().empty_note("No indexed files matched.");
        return md.render();
    }

    if layout == "flat" {
        let listing = files
            .iter()
            .map(|file| {
                format!(
                    "- {} ({} symbols, {} bytes)",
                    file.path, file.node_count, file.size
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        md.blank().code("text", &listing);
        return md.render();
    }

    let suffixes = files
        .iter()
        .map(|file| format!(" ({} symbols)", file.node_count))
        .collect::<Vec<_>>();
    let annotated = files
        .iter()
        .zip(suffixes.iter())
        .map(|(file, suffix)| (file.path.as_str(), suffix.as_str()));
    let listing = format_compact_annotated_path_list(annotated, "- ", "");
    md.blank().code("text", &listing);
    md.render()
}
