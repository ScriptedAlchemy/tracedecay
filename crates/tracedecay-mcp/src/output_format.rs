use serde_json::Value;

/// Presentation-only format requested by an MCP or CLI adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestedOutputFormat {
    Markdown,
    Json,
}

/// The single portable authority for reading `format` out of a tool argument
/// object. Matches the root application-surface parser so adapters stay
/// aligned without this crate naming daemon types.
pub fn requested_output_format(args: &Value) -> RequestedOutputFormat {
    match args.get("format").and_then(Value::as_str) {
        Some(format) if format.eq_ignore_ascii_case("json") => RequestedOutputFormat::Json,
        _ => RequestedOutputFormat::Markdown,
    }
}
