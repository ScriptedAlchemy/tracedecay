//! `tracedecay_remote_status` — Remote Brain operational-plane read.

use std::path::Path;

use serde_json::Value;
use tracedecay_application::remote::status::RemoteOperationalStatusReadV1;

use crate::daemon::remote_protocol::RemoteOperationalStatusProviderV1;
use crate::errors::Result;
use crate::mcp::tools::ToolResult;

use super::super::support::tool_json;

/// Reads the daemon-mounted Remote Brain operational plane.
///
/// Absence of the provider is the typed unmounted-authority outcome
/// [`RemoteOperationalStatusReadV1::Unavailable`], never an empty success.
pub(crate) fn handle_remote_status(
    project_root: &Path,
    args: &Value,
    provider: Option<&RemoteOperationalStatusProviderV1>,
) -> Result<ToolResult> {
    let status = match provider {
        Some(provider) => provider(),
        None => RemoteOperationalStatusReadV1::Unavailable,
    };
    let value = serde_json::to_value(&status)?;
    Ok(tool_json(Some(project_root), args, &value))
}

#[cfg(test)]
// The env lock deliberately spans the fixture's await points: it serializes
// process-wide TRACEDECAY_DATA_DIR mutation exactly like the other dispatch
// test modules, which carry the same allowance.
#[allow(clippy::await_holding_lock)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use serde_json::{Value, json};
    use tempfile::TempDir;
    use tracedecay_application::remote::status::{
        RemoteOperationalStatusReadV1, RemoteOperationalStatusV1, RemoteSpoolOperationalStatusV1,
    };
    use tracedecay_application::{DoctorCoverageCompletenessV1, RemoteListenerReadV1};
    use tracedecay_domain::{CurrentRemoteAuthorityStateV1, UtcMicros};

    use super::handle_remote_status;
    use crate::config::lock_user_data_dir_test_env;
    use crate::daemon::remote_protocol::RemoteOperationalStatusProviderV1;
    use crate::mcp::tools::ToolResult;
    use crate::mcp::tools::binding::{McpToolDispatchGroup, dispatch_group_for_tool};
    use crate::mcp::tools::handlers::dispatch_test_support::SelectorEnv;
    use crate::mcp::tools::handlers::{
        ToolCallRegistryOptions, handle_tool_call_with_registry_options,
    };
    use crate::tracedecay::TraceDecay;

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
    fn remote_status_is_an_info_dispatch_tool() {
        assert_eq!(
            dispatch_group_for_tool("tracedecay_remote_status"),
            Some(McpToolDispatchGroup::Info)
        );
    }

    #[test]
    fn handler_returns_observed_json_when_provider_is_installed() {
        let expected = observed_fixture();
        let provider: RemoteOperationalStatusProviderV1 = {
            let expected = expected.clone();
            Arc::new(move || expected.clone())
        };
        let result = handle_remote_status(
            Path::new("."),
            &json!({ "format": "json" }),
            Some(&provider),
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

    #[tokio::test]
    async fn dispatch_returns_provider_json_or_typed_unavailable() {
        let _env_lock = lock_user_data_dir_test_env();
        let dir = TempDir::new().unwrap();
        let _env = SelectorEnv::new(dir.path());
        // The temp project and the temp profile must share one hermetic
        // authority root; a fixture that splits them exercises a different
        // (cross-authority) dispatch shape than the one under test.
        let canonical_root = dir.path().canonicalize().unwrap();
        let profile_root = crate::storage::default_profile_root().unwrap();
        assert!(
            profile_root.starts_with(&canonical_root),
            "hermetic profile authority {} must live under the fixture root {}",
            profile_root.display(),
            canonical_root.display()
        );
        let project = dir.path().join("remote-status");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(project.join("src/lib.rs"), "pub fn probe() {}\n").unwrap();
        let (cg, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
            &project,
            "project.mcp-remote-status",
        )
        .await
        .unwrap();

        let expected = observed_fixture();
        let provider: RemoteOperationalStatusProviderV1 = {
            let expected = expected.clone();
            Arc::new(move || expected.clone())
        };
        let observed = handle_tool_call_with_registry_options(
            &cg,
            "tracedecay_remote_status",
            json!({ "format": "json" }),
            None,
            None,
            ToolCallRegistryOptions {
                remote_operational_status: Some(provider),
                ..Default::default()
            },
        )
        .await
        .expect("installed provider dispatches");
        assert_eq!(
            parse_tool_json(&observed),
            serde_json::to_value(&expected).unwrap()
        );

        let absent = handle_tool_call_with_registry_options(
            &cg,
            "tracedecay_remote_status",
            json!({ "format": "json" }),
            None,
            None,
            ToolCallRegistryOptions::default(),
        )
        .await
        .expect("absent provider dispatches");
        let parsed = parse_tool_json(&absent);
        assert_eq!(parsed, json!({ "kind": "unavailable" }));
        assert_ne!(parsed, json!({}));
    }
}
