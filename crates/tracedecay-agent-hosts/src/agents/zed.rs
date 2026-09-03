//! Zed agent integration.
//!
//! Handles registration of the tracedecay MCP server in Zed's `settings.json`
//! under the `context_servers.tracedecay` key.
//!
//! **Manual by necessity, not by preference (verified 2026-08-08).** The owner
//! policy is CLI-first, so this config write needs a justification. Zed ships
//! no non-interactive extension or context-server installation command at all:
//! that capability is an open feature request, not an implemented one, and
//! extensions are installed through the Command Palette and the Agent Panel.
//! There is nothing to drive, so the settings merge below is the only route.
//! See <https://github.com/zed-industries/zed/discussions/58417>.

use std::path::{Path, PathBuf};

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, McpDoctorLabels,
    doctor_check_mcp_registration, load_jsonc_file,
};

pub struct ZedIntegration;

fn zed_config_dir(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support/Zed")
    }
    #[cfg(not(target_os = "macos"))]
    {
        home.join(".config/zed")
    }
}

impl AgentIntegration for ZedIntegration {
    fn name(&self) -> &'static str {
        "Zed"
    }

    fn id(&self) -> &'static str {
        "zed"
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mZed integration\x1b[0m");
        doctor_check_settings(dc, &ctx.home);
    }

    fn is_detected(&self, home: &Path) -> bool {
        zed_config_dir(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(zed_settings_path(home))
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        super::mcp_config_has_tracedecay(
            &zed_settings_path(home),
            "context_servers",
            load_jsonc_file,
        )
    }
}

fn zed_settings_path(home: &Path) -> PathBuf {
    zed_config_dir(home).join("settings.json")
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

fn doctor_check_settings(dc: &mut DoctorCounters, home: &Path) {
    let settings_path = zed_settings_path(home);
    doctor_check_mcp_registration(
        dc,
        &settings_path,
        "context_servers",
        load_jsonc_file,
        &McpDoctorLabels {
            agent_id: "zed",
            product: "Zed",
            registered: "Context server registered",
            missing: "Context server NOT registered",
        },
    );
}
