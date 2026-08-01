//! `tracedecay_files` — indexed file listing with prefix and glob filters.

use super::*;

/// Handles `tracedecay_files` tool calls.
pub(crate) async fn handle_files(
    cg: &TraceDecay,
    args: Value,
    scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    debug_assert!(args.is_object(), "handle_files expects an object argument");
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
        || render_files_md(&payload),
    ))
}

fn render_files_md(value: &Value) -> String {
    let mut md = Md::new();
    md.heading(2, "Files");
    md.field(
        "indexed files",
        &render::field_i64(value, "count").to_string(),
    );
    let layout = render::field_str(value, "layout");
    md.field("layout", layout);

    let files = value
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if files.is_empty() {
        md.blank().empty_note("No indexed files matched.");
        return md.render();
    }

    if layout == "flat" {
        let lines = files
            .iter()
            .filter_map(|file| {
                let path = file.get("path").and_then(Value::as_str)?;
                let symbols = render::field_i64(file, "symbols");
                let bytes = render::field_i64(file, "bytes");
                Some(format!("- {path} ({symbols} symbols, {bytes} bytes)"))
            })
            .collect::<Vec<_>>();
        let listing = lines.join("\n");
        md.blank().code("text", &listing);
        return md.render();
    }

    let paths = files
        .iter()
        .filter_map(|file| {
            file.get("path")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
        .collect::<Vec<_>>();
    let suffixes = files
        .iter()
        .map(|file| format!(" ({} symbols)", render::field_i64(file, "symbols")))
        .collect::<Vec<_>>();
    let annotated = paths
        .iter()
        .zip(suffixes.iter())
        .map(|(path, suffix)| (path.as_str(), suffix.as_str()));
    let listing = format_compact_annotated_path_list(annotated, "- ", "");
    md.blank().code("text", &listing);
    md.render()
}
