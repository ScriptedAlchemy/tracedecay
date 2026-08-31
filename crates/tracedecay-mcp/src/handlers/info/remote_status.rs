//! `tracedecay_remote_status` — Remote Brain operational-plane read.

use std::path::Path;

use serde_json::Value;
use tracedecay_application::remote::status::RemoteOperationalStatusReadPort;
use tracedecay_application::remote::status::RemoteOperationalStatusReadV1;
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
    provider: Option<&dyn RemoteOperationalStatusReadPort>,
) -> Result<ToolResult> {
    let status = match provider {
        Some(provider) => provider.read(),
        None => RemoteOperationalStatusReadV1::Unavailable,
    };
    let value = serde_json::to_value(&status)?;
    Ok(tool_json(Some(project_root), args, &value))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use serde_json::{Value, json};
    use tracedecay_application::remote::status::{
        RemoteOperationalStatusReadV1, RemoteOperationalStatusV1, RemoteSpoolOperationalStatusV1,
    };
    use tracedecay_application::{DoctorCoverageCompletenessV1, RemoteListenerReadV1};
    use tracedecay_domain::{CurrentRemoteAuthorityStateV1, UtcMicros};

    use super::handle_remote_status;
    use crate::ToolResult;

    fn available_authority() -> CurrentRemoteAuthorityStateV1 {
        serde_json::from_value(json!({
            "state": "available",
            "value": {
                "fence": {
                    "brain_id": "brain.status",
                    "shard_id": "shard.status",
                    "generation_id": "generation.status",
                    "placement_revision": 1,
                    "authority_epoch": 1,
                    "authority_node_id": "node.authority"
                },
                "credential_revision": 1,
                "observed_at": 10
            }
        }))
        .unwrap()
    }

    fn observed_fixture() -> RemoteOperationalStatusReadV1 {
        let status = RemoteOperationalStatusV1::compose(
            true,
            available_authority(),
            RemoteSpoolOperationalStatusV1 {
                pending_count: 3,
                quarantined_count: 0,
                has_sequence_gap: false,
            },
            false,
            true,
            false,
            false,
            UtcMicros(10),
        )
        .unwrap();
        RemoteOperationalStatusReadV1::Observed {
            listener: RemoteListenerReadV1::Serving,
            status,
            coverage: DoctorCoverageCompletenessV1::Complete,
        }
    }

    fn parse_tool_json(result: &ToolResult) -> Value {
        serde_json::from_str(
            result.value["content"][0]["text"]
                .as_str()
                .expect("tool JSON text"),
        )
        .expect("parse tool JSON")
    }

    #[test]
    fn handler_returns_observed_json_when_provider_is_installed() {
        let expected = observed_fixture();
        let provider = {
            let expected = expected.clone();
            Arc::new(move || expected.clone())
        };
        let result = handle_remote_status(
            Path::new("."),
            &json!({ "format": "json" }),
            Some(provider.as_ref()),
        )
        .expect("observed remote status serializes");
        assert_eq!(
            parse_tool_json(&result),
            serde_json::to_value(&expected).unwrap()
        );
        assert_ne!(result.semantic_error(), Some(true));
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
