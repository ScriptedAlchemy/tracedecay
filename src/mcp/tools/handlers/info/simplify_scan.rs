//! `tracedecay_simplify_scan` — duplication, dead-code, complexity, and coupling findings for a file set.

use super::*;

/// Handles `tracedecay_simplify_scan` tool calls.
pub(crate) async fn handle_simplify_scan(
    cg: &TraceDecay,
    args: Value,
    _scope_prefix: Option<&str>,
) -> Result<ToolResult> {
    let files: Vec<String> = args
        .get("files")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .ok_or_else(|| TraceDecayError::Config {
            message: "missing required parameter: files (array of strings)".to_string(),
        })?;

    let mut duplications: Vec<Value> = Vec::new();
    let mut dead_introductions: Vec<Value> = Vec::new();
    let mut complexity_warnings: Vec<Value> = Vec::new();
    let mut coupling_warnings: Vec<Value> = Vec::new();

    for file in &files {
        // Store errors propagate: an empty scan result must mean "no
        // findings", never "the store query failed".
        let nodes = cg.get_nodes_by_file(file).await?;

        for node in &nodes {
            // 1. Duplication: find similar symbols elsewhere
            if matches!(node.kind, NodeKind::Function | NodeKind::Method) {
                let similar = cg.search(&node.name, 5).await?;
                let dupes: Vec<Value> = similar
                    .iter()
                    .filter(|s| {
                        s.node.id != node.id && s.score > 0.8 && s.node.file_path != node.file_path
                    })
                    .map(|d| {
                        json!({
                            "name": d.node.name,
                            "file": d.node.file_path,
                            "line": d.node.start_line,
                            "score": d.score,
                        })
                    })
                    .collect();
                if !dupes.is_empty() {
                    duplications.push(json!({
                        "symbol": node.name,
                        "file": node.file_path,
                        "line": node.start_line,
                        "similar_to": dupes,
                    }));
                }
            }

            // 2. Dead code: function/method with no incoming edges
            if matches!(node.kind, NodeKind::Function | NodeKind::Method)
                && node.visibility != Visibility::Pub
                && node.name != "main"
                && !node.name.starts_with("test_")
            {
                let incoming = cg.get_incoming_edges(&node.id).await?;
                if incoming.is_empty() {
                    dead_introductions.push(json!({
                        "symbol": node.name,
                        "file": node.file_path,
                        "line": node.start_line,
                        "reason": "no incoming edges (unreferenced)",
                    }));
                }
            }

            // 3. Complexity: check if function exceeds threshold
            if matches!(node.kind, NodeKind::Function | NodeKind::Method) {
                let lines = node.end_line.saturating_sub(node.start_line) as usize;
                let fan_out = cg
                    .get_outgoing_edges(&node.id)
                    .await?
                    .iter()
                    .filter(|e| matches!(e.kind, crate::types::EdgeKind::Calls))
                    .count();
                let score = lines + fan_out * 3;
                if score > 100 {
                    complexity_warnings.push(json!({
                        "symbol": node.name,
                        "file": node.file_path,
                        "line": node.start_line,
                        "lines": lines,
                        "fan_out": fan_out,
                        "score": score,
                    }));
                }
            }
        }

        // 4. Coupling: check file fan_in
        let file_deps = cg.get_file_dependents(file).await?;
        if file_deps.len() > 15 {
            coupling_warnings.push(json!({
                "file": file,
                "fan_in": file_deps.len(),
                "warning": "high fan-in — changes here affect many dependents",
            }));
        }
    }

    let output = json!({
        "duplications": duplications,
        "dead_introductions": dead_introductions,
        "complexity_warnings": complexity_warnings,
        "coupling_warnings": coupling_warnings,
    });

    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &output,
        files,
        || render_simplify_scan_markdown(&output),
    ))
}

fn render_simplify_scan_markdown(output: &Value) -> String {
    let mut md = Md::new();
    md.heading(1, "Simplify Scan");

    let duplications = output
        .get("duplications")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let dead = output
        .get("dead_introductions")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let complexity = output
        .get("complexity_warnings")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let coupling = output
        .get("coupling_warnings")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let total = duplications.len() + dead.len() + complexity.len() + coupling.len();

    if total == 0 {
        md.empty_note("No simplification findings for the scanned files.");
        return md.render();
    }

    md.field("Findings", &total.to_string()).blank();
    render_simplify_duplications(&mut md, duplications);
    render_simplify_dead_code(&mut md, dead);
    render_simplify_complexity(&mut md, complexity);
    render_simplify_coupling(&mut md, coupling);
    md.render()
}

fn render_simplify_duplications(md: &mut Md, items: &[Value]) {
    render_simplify_section(md, "Possible Duplications", items, "symbol", |md, item| {
        md.line(&format!("  **Location:** {}", finding_location(item)));
        let similar = summarize_similar_symbols(item);
        if !similar.is_empty() {
            md.line(&format!("  **Similar symbols:** {similar}"));
        }
    });
}

fn render_simplify_dead_code(md: &mut Md, items: &[Value]) {
    render_simplify_section(md, "Potential Dead Code", items, "symbol", |md, item| {
        md.line(&format!("  **Location:** {}", finding_location(item)));
        md.line(&format!(
            "  **Reason:** {}",
            render::field_str(item, "reason")
        ));
    });
}

fn render_simplify_complexity(md: &mut Md, items: &[Value]) {
    render_simplify_section(md, "Complexity Warnings", items, "symbol", |md, item| {
        md.line(&format!("  **Location:** {}", finding_location(item)));
        md.line(&format!(
            "  **Lines:** {}",
            render::field_i64(item, "lines")
        ));
        md.line(&format!(
            "  **Fan-out:** {}",
            render::field_i64(item, "fan_out")
        ));
        md.line(&format!(
            "  **Score:** {}",
            render::field_i64(item, "score")
        ));
    });
}

fn render_simplify_coupling(md: &mut Md, items: &[Value]) {
    render_simplify_section(md, "Coupling Warnings", items, "file", |md, item| {
        md.line(&format!(
            "  **Fan-in:** {}",
            render::field_i64(item, "fan_in")
        ));
        md.line(&format!(
            "  **Warning:** {}",
            render::field_str(item, "warning")
        ));
    });
}

fn render_simplify_section<FDetails>(
    md: &mut Md,
    title: &str,
    items: &[Value],
    label_field: &str,
    details: FDetails,
) where
    FDetails: Fn(&mut Md, &Value),
{
    if items.is_empty() {
        return;
    }
    md.heading(2, title);
    for item in items {
        md.bullet(&format!("**{}**", render::field_str(item, label_field)));
        details(md, item);
    }
    md.blank();
}

fn finding_location(item: &Value) -> String {
    format!(
        "{}:{}",
        render::field_str(item, "file"),
        render::field_i64(item, "line")
    )
}

fn summarize_similar_symbols(item: &Value) -> String {
    let Some(similar) = item.get("similar_to").and_then(Value::as_array) else {
        return String::new();
    };
    let Some(first) = similar.first() else {
        return String::new();
    };
    let mut summary = format!(
        "{} at {}:{}",
        render::field_str(first, "name"),
        render::field_str(first, "file"),
        render::field_i64(first, "line")
    );
    if similar.len() > 1 {
        let _ = write!(summary, " (+{} more)", similar.len() - 1);
    }
    summary
}
