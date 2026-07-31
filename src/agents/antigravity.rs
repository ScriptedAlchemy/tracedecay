//! Google Antigravity (formerly Windsurf) agent integration.
//!
//! Handles registration of the tracedecay MCP server in:
//!
//! - `~/.gemini/antigravity/mcp_config.json` — the Antigravity IDE config,
//!   shape `{"mcpServers": {"tracedecay": {...}}}`.
//! - `~/.gemini/antigravity-cli/plugins/tracedecay.json` — the Antigravity
//!   CLI (`agy`) plugin file, same shape. Required because the IDE config
//!   is not picked up by the CLI (#85).
//!
//! Both files are kept in sync by `install` and `uninstall`; `doctor` checks
//! both and reports each location separately.

use std::path::Path;

use serde_json::json;

use crate::errors::Result;

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, McpDoctorLabels,
    McpUninstallPolicy, backup_config_file, doctor_check_mcp_registration, install_mcp_server_entry,
    load_json_file, load_json_file_strict, safe_write_json_file, uninstall_mcp_server_entry,
};

/// Google Antigravity agent.
pub struct AntigravityIntegration;

fn mcp_config_path(home: &Path) -> std::path::PathBuf {
    home.join(".gemini/antigravity/mcp_config.json")
}

/// Per-plugin file used by the Antigravity CLI. Holds the same shape as
/// the IDE config so a future shared loader can read either location.
fn cli_plugin_path(home: &Path) -> std::path::PathBuf {
    home.join(".gemini/antigravity-cli/plugins/tracedecay.json")
}

impl AgentIntegration for AntigravityIntegration {
    fn name(&self) -> &'static str {
        "Antigravity"
    }

    fn id(&self) -> &'static str {
        "antigravity"
    }

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        // 1. Antigravity IDE config (~/.gemini/antigravity/mcp_config.json)
        let mcp_path = mcp_config_path(&ctx.home);
        install_mcp_server_entry(
            &mcp_path,
            "mcpServers",
            json!({
                "command": ctx.tracedecay_bin,
                "args": ["serve"]
            }),
            "Antigravity",
            load_json_file_strict,
        )?;

        // 2. Antigravity CLI plugin (~/.gemini/antigravity-cli/plugins/tracedecay.json).
        //    Same shape as the IDE config; required because the IDE config is
        //    not picked up by the CLI (#85).
        let plugin_path = cli_plugin_path(&ctx.home);
        if let Some(parent) = plugin_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let plugin_backup = backup_config_file(&plugin_path)?;
        let plugin_settings = json!({
            "mcpServers": {
                "tracedecay": {
                    "command": ctx.tracedecay_bin,
                    "args": ["serve"],
                }
            }
        });
        safe_write_json_file(&plugin_path, &plugin_settings, plugin_backup.as_deref())?;
        eprintln!(
            "\x1b[32m✔\x1b[0m Added tracedecay CLI plugin to {}",
            plugin_path.display()
        );

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tracedecay init");
        eprintln!(
            "  2. Restart Antigravity (IDE or `agy` CLI) — tracedecay tools are now available"
        );
        Ok(())
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        let mcp_path = mcp_config_path(&ctx.home);
        uninstall_mcp_server(&mcp_path);
        uninstall_cli_plugin(&cli_plugin_path(&ctx.home));

        eprintln!();
        eprintln!("Uninstall complete. Tracedecay has been removed from Antigravity.");
        eprintln!("Restart Antigravity (IDE or `agy` CLI) for changes to take effect.");
        Ok(())
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mAntigravity integration\x1b[0m");
        doctor_check_settings(dc, &ctx.home);
        doctor_check_cli_plugin(dc, &ctx.home);
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".gemini/antigravity").is_dir() || home.join(".gemini/antigravity-cli").is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(mcp_config_path(home))
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        let ide_ok = {
            let mcp_path = mcp_config_path(home);
            if mcp_path.exists() {
                let servers = load_json_file(&mcp_path).get("mcpServers").cloned();
                servers.as_ref().and_then(|v| v.get("tracedecay")).is_some()
            } else {
                false
            }
        };
        let cli_ok = {
            let plugin_path = cli_plugin_path(home);
            let has_entry = |path: &std::path::Path| {
                if !path.exists() {
                    return false;
                }
                let servers = load_json_file(path).get("mcpServers").cloned();
                servers.as_ref().and_then(|v| v.get("tracedecay")).is_some()
            };
            has_entry(&plugin_path)
        };
        ide_ok || cli_ok
    }
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

fn uninstall_mcp_server(mcp_path: &Path) {
    uninstall_mcp_server_entry(
        mcp_path,
        "mcpServers",
        load_json_file,
        McpUninstallPolicy {
            prune_empty_root: false,
            remove_empty_file: true,
        },
    );
}

/// Remove the per-plugin file the CLI loader picks up. Unlike the IDE config
/// — which is shared across other tools — the plugin file belongs exclusively
/// to tracedecay, so we just delete it.
fn uninstall_cli_plugin(plugin_path: &Path) {
    if !plugin_path.exists() {
        eprintln!("  {} not found, skipping", plugin_path.display());
        return;
    }
    if std::fs::remove_file(plugin_path).is_ok() {
        eprintln!("\x1b[32m✔\x1b[0m Removed {} ", plugin_path.display());
    }
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

fn doctor_check_settings(dc: &mut DoctorCounters, home: &Path) {
    doctor_check_mcp_registration(
        dc,
        &mcp_config_path(home),
        "mcpServers",
        load_json_file,
        &McpDoctorLabels {
            agent_id: "antigravity",
            product: "the Antigravity IDE",
            registered: "IDE MCP server registered",
            missing: "MCP server NOT registered",
        },
    );
}

fn doctor_check_cli_plugin(dc: &mut DoctorCounters, home: &Path) {
    doctor_check_mcp_registration(
        dc,
        &cli_plugin_path(home),
        "mcpServers",
        load_json_file,
        &McpDoctorLabels {
            agent_id: "antigravity",
            product: "the Antigravity CLI (#85)",
            registered: "CLI plugin registered",
            missing: "CLI plugin file exists but lacks `mcpServers.tracedecay`",
        },
    );
}
