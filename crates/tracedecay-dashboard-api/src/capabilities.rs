//! Capability discovery from the current state's mounted runtime authorities.
use axum::extract::State;
use axum::response::Json;
use serde_json::{Value, json};
use tracedecay_automation_runtime::automation::backend;
use tracedecay_automation_runtime::automation::config::{AutomationBackend, AutomationHostMode};

use crate::{DashboardState, RequestControl, automation_config_api, multi_root_api};

/// Capability discovery for hosts and future delegated-host extensions. The UI
/// (or a wrapper) can probe this to decide which panels/actions to enable.
#[tracing::instrument(name = "dashboard_api.http.capabilities", level = "trace", skip_all)]
pub(super) async fn capabilities(
    State(state): State<DashboardState>,
    RequestControl(control): RequestControl,
) -> Json<Value> {
    let has_lcm = state.lcm_read_authority.is_some();
    let automation = automation_config_api::effective_automation_config(&state);
    let (automation_configured, automation_mode, automation_payload) = match automation {
        Ok((configuration_revision_id, config, codex)) => {
            let backend_supported = matches!(config.backend, AutomationBackend::CodexAppServer);
            let configured = config.enabled && backend_supported;
            let mode = if !configured {
                "disabled"
            } else if config.host_mode == AutomationHostMode::DelegatedHost {
                "delegated_host"
            } else {
                "standalone_backend"
            };
            (
                configured,
                mode,
                json!({
                    "available": true,
                    "configuration_revision_id": configuration_revision_id,
                    "enabled": config.enabled,
                    "mode": mode,
                    "backend": config.backend,
                    "host_mode": config.host_mode,
                    "availability": backend::backend_availability(&config, &codex),
                }),
            )
        }
        Err(error) => (
            false,
            "unavailable",
            json!({
                "available": false,
                "reason": error.to_string(),
                "required_authority": "pinned automation configuration",
            }),
        ),
    };
    let standalone_automation = automation_mode == "standalone_backend";
    // Multi-root reads are served by the daemon, never by the dashboard's own
    // stores. Capability discovery resolves through the mounted named-collection
    // resolver: with no configured default collection this reports the typed
    // no-collection state, and `/api/multi-root/collection` resolves explicit
    // targets through the same authority.
    let multi_root_resolver_mounted = state.application_invocation_executor.is_some();
    let multi_root = multi_root_api::resolve_collection_capability(&state, control, None).await;
    Json(json!({
        "name": "tracedecay-dashboard",
        "version": state.build_version,
        "mode": "standalone",
        "project_id": state.project_id,
        "project_root": state.project_root.display().to_string(),
        "storage_mode": state.storage_mode,
        "store_root": state.store_root.display().to_string(),
        "dashboard_root": state.dashboard_root.display().to_string(),
        "memory_db": state.mem_db_path,
        "graph_db": state.graph_db_path,
        "lcm_db": state.lcm_db_path,
        "lcm_scope": state.lcm_scope,
        "session_authority": state.session_authority.as_str(),
        "multi_root": multi_root,
        "features": {
            "memory": true,
            "lcm": has_lcm,
            "graph": true,
            "analytics": true,
            "feedback": state.feedback_status_reader.is_some(),
            "code_diagnostics": state.code_diagnostics_authority.is_some(),
            // Memory curation/refinement is served by the configured
            // standalone automation backend. Explicit operations use the
            // canonical retained application surface.
            "curation": true,
            "automation": automation_configured,
            "llm_curation": standalone_automation,
            "managed_skills": true,
            // Savings & Cost tab: savings-ledger analytics + per-session
            // cost accounting with OpenRouter-backed pricing.
            "savings": true,
            // Settings tab: aggregated project/user config editing plus
            // read-only environment and storage-path display.
            "settings": true,
            // Named-collection resolution is mounted whenever the daemon
            // application transport is admitted for this dashboard.
            "multi_root": multi_root_resolver_mounted,
        },
        "automation": automation_payload,
        "dashboards": ["tracedecay"],
    }))
}
