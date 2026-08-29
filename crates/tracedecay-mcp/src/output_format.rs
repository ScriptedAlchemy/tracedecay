use serde_json::Value;

/// Presentation-only format requested by an MCP or CLI adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestedOutputFormat {
    Markdown,
    Json,
}

/// The single authority for reading `format` out of a tool argument object.
pub fn requested_output_format(args: &Value) -> RequestedOutputFormat {
    match args.get("format").and_then(Value::as_str) {
        Some(format) if format.eq_ignore_ascii_case("json") => RequestedOutputFormat::Json,
        _ => RequestedOutputFormat::Markdown,
    }
}
