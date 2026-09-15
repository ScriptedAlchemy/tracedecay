//! `tracedecay_remote_status` — Remote Brain operational-plane read.

use std::path::Path;

use serde_json::Value;
use tracedecay_contracts::remote::status::RemoteOperationalStatusReadV1;
use tracedecay_contracts::remote::status::RemoteOperationalStatusReaderV1;
use tracedecay_domain::errors::Result;

use crate::ToolResult;
use crate::tool_json;

/// Reads the daemon-mounted Remote Brain operational plane.
///
/// Absence of the provider is the typed unmounted-authority outcome
/// [`RemoteOperationalStatusReadV1::Unavailable`], never an empty success.
#[hotpath::measure(label = "mcp.info.remote_status.total")]
pub fn handle_remote_status(
    project_root: &Path,
    args: &Value,
    provider: Option<&RemoteOperationalStatusReaderV1>,
) -> Result<ToolResult> {
    let status = match provider {
        Some(provider) => provider(),
        None => RemoteOperationalStatusReadV1::Unavailable,
    };
    let value = serde_json::to_value(&status)?;
    Ok(tool_json(Some(project_root), args, &value))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::{Value, json};

    use super::handle_remote_status;
    use crate::ToolResult;

    fn parse_tool_json(result: &ToolResult) -> Value {
        serde_json::from_str(
            result.value["content"][0]["text"]
                .as_str()
                .expect("tool JSON text"),
        )
        .expect("parse tool JSON")
    }

    #[test]
    fn handler_returns_typed_unavailable_when_provider_is_absent() {
        let result = handle_remote_status(Path::new("."), &json!({ "format": "json" }), None)
            .expect("absent provider is a typed read");
        let parsed = parse_tool_json(&result);
        assert_eq!(parsed, json!({ "kind": "unavailable" }));
        assert_ne!(parsed, json!({}));
        assert_ne!(result.semantic_error(), Some(true));
    }
}
