//! `tracedecay_outline` — flat symbol map for a file, enriched with ast-grep structure.

use super::*;

/// Handles `tracedecay_outline` — flat symbol map for a file with optional
/// `kinds` filter.
pub(crate) async fn handle_outline(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    use crate::context::read_modes::render_map;

    let file =
        args.get("file")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "missing required parameter: file".to_string(),
            })?;

    let kinds: Option<Vec<String>> = args.get("kinds").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    });

    let (abs_path, display_file) = resolve_indexed_source_file(
        cg.project_root(),
        graph.reader(),
        graph.cancellation(),
        file,
    )?;

    let kinds_slice: Option<&[String]> = kinds.as_deref();
    let mut value = render_map(
        graph.reader(),
        graph.cancellation(),
        &display_file,
        kinds_slice,
    )?;
    match ast_grep_outline(&abs_path) {
        Ok(outline) => {
            value["ast_grep_outline"] = outline;
        }
        Err(err) => {
            value["ast_grep_outline"] = Value::Null;
            value["ast_grep_outline_error"] = json!(err.to_string());
        }
    }
    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &value,
        vec![display_file],
        || render_outline_md(&value),
    ))
}

fn ast_grep_outline(abs_path: &Path) -> Result<Value> {
    ensure_ast_grep_outline_available()?;

    let output = crate::external_tools::ast_grep_command()
        .args([
            "outline",
            "--json=compact",
            "--items",
            "structure",
            "--view",
            "expanded",
        ])
        .arg(abs_path)
        .output()
        .map_err(|err| TraceDecayError::Config {
            message: format!("failed to run ast-grep outline: {err}"),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim()
        } else if !stdout.trim().is_empty() {
            stdout.trim()
        } else {
            "no output"
        };
        return Err(TraceDecayError::Config {
            message: format!("ast-grep outline failed: {detail}"),
        });
    }

    serde_json::from_slice::<Value>(&output.stdout).map_err(|err| TraceDecayError::Config {
        message: format!("failed to parse ast-grep outline JSON: {err}"),
    })
}

fn ensure_ast_grep_outline_available() -> Result<()> {
    let diagnostics = definitions::ast_grep_diagnostics();
    if diagnostics.outline_available {
        Ok(())
    } else {
        Err(TraceDecayError::Config {
            message: format!(
                "tracedecay_outline requires ast-grep outline >= 0.44: {}",
                diagnostics.message
            ),
        })
    }
}

fn render_outline_md(value: &Value) -> String {
    let mut md = Md::new();
    let file = render::field_str(value, "file");
    let count = render::field_i64(value, "symbol_count");
    md.heading(2, &format!("Outline — {file}"));
    md.field("symbols", &count.to_string());
    md.blank();
    match value.get("symbols").and_then(Value::as_array) {
        Some(symbols) if !symbols.is_empty() => {
            for symbol in symbols {
                let name = render::field_str(symbol, "name");
                let kind = render::field_str(symbol, "kind");
                let visibility = render::field_str(symbol, "visibility");
                let line = render::field_i64(symbol, "line");
                let end = render::field_i64(symbol, "end_line");
                let span = if end > line {
                    format!("{line}-{end}")
                } else {
                    line.to_string()
                };
                let signature = render::field_str(symbol, "signature");
                md.bullet(&format!(
                    "**{name}** ({kind}) - lines {span} - {visibility}"
                ));
                if !signature.is_empty() {
                    md.line(&format!("  `{signature}`"));
                }
            }
        }
        _ => {
            md.empty_note("No symbols.");
        }
    }
    md.render()
}
