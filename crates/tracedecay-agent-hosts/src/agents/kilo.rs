//! Kilo CLI agent integration.
//!
//! Owns the profile-wide Kilo CLI MCP registration lifecycle. Kilo uses the `mcp` key (not
//! `mcpServers`) with entries having `type`, `command` (as array), and
//! `enabled` fields.
//!
//! Kilo documents local stdio servers directly in `kilo.jsonc`; its CLI does
//! not expose an equivalent reversible local-server command. TraceDecay merges
//! only `mcp.tracedecay` and removes only that key. Kilo plugins and hooks stay
//! uninstalled until a real native fixture admits their protocol.

use std::path::{Path, PathBuf};

use crate::errors::Result;

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, McpDoctorLabels,
    McpUninstallPolicy, config_backup_path, doctor_check_mcp_registration,
    install_mcp_server_entry, load_jsonc_file, load_jsonc_file_strict, uninstall_mcp_server_entry,
};

/// Kilo CLI agent.
pub struct KiloIntegration;

fn kilo_config_dir(home: &Path) -> std::path::PathBuf {
    home.join(".config/kilo")
}

fn kilo_config_path(home: &Path) -> std::path::PathBuf {
    kilo_config_dir(home).join("kilo.jsonc")
}

impl AgentIntegration for KiloIntegration {
    fn name(&self) -> &'static str {
        "Kilo CLI"
    }

    fn id(&self) -> &'static str {
        "kilo"
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mKilo CLI integration\x1b[0m");
        doctor_check_settings(dc, &ctx.home);
    }

    fn host_component_registration(
        &self,
        component: super::host_bundle_v2::HostBundleComponentV1,
        ctx: &HealthcheckContext,
    ) -> super::host_bundle_v2::HostBundleRegistrationStateV1 {
        use super::host_bundle_v2::HostBundleRegistrationStateV1 as State;

        if component != super::host_bundle_v2::HostBundleComponentV1::ContextMcp {
            return State::Missing;
        }

        let path = kilo_config_path(&ctx.home);
        let Ok(bytes) = std::fs::read_to_string(path) else {
            return State::Missing;
        };
        let settings = super::parse_jsonc(&bytes);
        if settings
            .pointer("/mcp/tracedecay/enabled")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            && settings
                .pointer("/mcp/tracedecay/command")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|args| args.iter().any(|arg| arg.as_str() == Some("serve")))
        {
            State::Current
        } else {
            State::Missing
        }
    }

    fn is_detected(&self, home: &Path) -> bool {
        kilo_config_dir(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(kilo_config_path(home))
    }

    fn host_component_registration_paths(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        home: &Path,
    ) -> Vec<PathBuf> {
        if components == [super::host_bundle_v2::HostBundleComponentV1::ContextMcp] {
            let path = kilo_config_path(home);
            vec![path.clone(), config_backup_path(&path)]
        } else {
            Vec::new()
        }
    }

    fn activate_deployed_host_component_registration(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&super::host_bundle_v2::HostBundleComponentV1::ContextMcp) {
            install_mcp_server_entry(
                &kilo_config_path(&ctx.home),
                "mcp",
                serde_json::json!({
                    "type": "local",
                    "command": [ctx.tracedecay_bin.clone(), "serve"],
                    "enabled": true
                }),
                "Kilo",
                load_jsonc_file_strict,
            )?;
        }
        Ok(())
    }

    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        if components.contains(&super::host_bundle_v2::HostBundleComponentV1::ContextMcp) {
            uninstall_mcp_server_entry(
                &kilo_config_path(&ctx.home),
                "mcp",
                load_jsonc_file,
                McpUninstallPolicy::default(),
            );
        }
        Ok(())
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        let config_path = kilo_config_path(home);
        if !config_path.exists() {
            return false;
        }
        let json = load_jsonc_file(&config_path);
        let servers = json.get("mcp");
        servers.and_then(|v| v.get("tracedecay")).is_some()
    }
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

fn doctor_check_settings(dc: &mut DoctorCounters, home: &Path) {
    doctor_check_mcp_registration(
        dc,
        &kilo_config_path(home),
        "mcp",
        load_jsonc_file,
        &McpDoctorLabels {
            agent_id: "kilo",
            product: "Kilo CLI",
            registered: "MCP server registered",
            missing: "MCP server NOT registered",
        },
    );
}
