//! `tracedecay_read` — mode-aware file read with cross-session cache.

use super::*;

/// Handles `tracedecay_read` — mode-aware file read with cross-session cache.
pub(crate) async fn handle_read(
    cg: &TraceDecay,
    graph: &crate::tracedecay::queries::graph::VerifiedGraphQuery,
    args: Value,
) -> Result<ToolResult> {
    let file =
        args.get("file")
            .and_then(|v| v.as_str())
            .ok_or_else(|| TraceDecayError::Config {
                message: "missing required parameter: file".to_string(),
            })?;

    let mode_str = args.get("mode").and_then(|v| v.as_str()).unwrap_or("full");
    let mode = ReadMode::parse(mode_str).ok_or_else(|| TraceDecayError::Config {
        message: format!("unknown mode '{mode_str}'; expected one of full, lines, map, signatures"),
    })?;
    let include_symbols = args
        .get("include_symbols")
        .and_then(Value::as_bool)
        .unwrap_or(mode == ReadMode::Lines);

    let line_range = if mode == ReadMode::Lines {
        let raw =
            args.get("lines")
                .and_then(|v| v.as_str())
                .ok_or_else(|| TraceDecayError::Config {
                    message: "mode='lines' requires the 'lines' argument (e.g. '120-180')"
                        .to_string(),
                })?;
        Some(
            LineRange::parse(raw).ok_or_else(|| TraceDecayError::Config {
                message: format!("invalid 'lines' value '{raw}'; expected 'A' or 'A-B'"),
            })?,
        )
    } else {
        None
    };

    let project_id = cg.project_root().to_string_lossy();
    let output = read_source(
        cg.project_root(),
        cg.db(),
        cg.is_read_only(),
        graph.reader(),
        graph.cancellation(),
        SourceReadRequest {
            file,
            mode,
            line_range,
            raw_lines: args.get("lines").and_then(Value::as_str),
            include_symbols,
            project_id: &project_id,
        },
    )
    .await?;
    let display_file = output.file;
    let mut payload = json!({
        "file": &display_file,
        "mode": output.mode.as_str(),
        "mtime_ns": output.mtime_ns,
        "digest": output.digest,
        "token_count": output.token_count,
    });
    if output.unchanged {
        payload["unchanged"] = Value::Bool(true);
    }
    if let Some(body) = output.body {
        payload["body"] = Value::String(body);
    }
    if let Some(context) = output.context {
        payload["context"] = context;
    }
    Ok(rendered_tool_result(
        Some(cg.project_root()),
        &args,
        &payload,
        vec![display_file],
        || render_read_md(&payload),
    ))
}

fn render_read_md(value: &Value) -> String {
    let mut md = Md::new();
    let file = render::field_str(value, "file");
    let mode = render::field_str(value, "mode");
    md.heading(2, &format!("{file} ({mode})"));
    if value
        .get("unchanged")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        md.field("unchanged", "true");
        let digest = render::field_str(value, "digest");
        if !digest.is_empty() {
            md.field("digest", digest);
        }
    }
    md.field(
        "tokens",
        &render::field_i64(value, "token_count").to_string(),
    );
    render_read_context_md(&mut md, value.get("context"));
    if value.get("body").is_none() {
        return md.render();
    }
    md.blank();
    let lang = file.rsplit_once('.').map_or("", |(_, ext)| ext);
    md.code(lang, render::field_str(value, "body"));
    md.render()
}

fn render_read_context_md(md: &mut Md, context: Option<&Value>) {
    let Some(context) = context else {
        return;
    };
    let Some(symbols) = context.get("symbols").and_then(Value::as_array) else {
        return;
    };
    if symbols.is_empty() {
        return;
    }

    md.blank();
    md.heading(3, "Context");
    let symbol_count = context
        .get("symbol_count")
        .and_then(Value::as_u64)
        .unwrap_or(symbols.len() as u64);
    md.field("symbols", &symbol_count.to_string());
    for symbol in symbols {
        let kind = render::field_str(symbol, "kind");
        let name = render::field_str(symbol, "name");
        let line = render::field_i64(symbol, "line");
        let end_line = render::field_i64(symbol, "end_line");
        let signature = render::field_str(symbol, "signature");
        let span = if end_line > line {
            format!("{line}-{end_line}")
        } else {
            line.to_string()
        };
        if signature.is_empty() {
            md.bullet(&format!("{kind} {name} {span}"));
        } else {
            md.bullet(&format!("{kind} {name} {span}: `{signature}`"));
        }
    }
    if context
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        md.empty_note("symbol list truncated");
    }
}
