//! Kilo CLI agent integration.
//!
//! Handles registration of the tracedecay MCP server in Kilo CLI config files.
//! Kilo uses the `mcp` key (not `mcpServers`) with entries having `type`,
//! `command` (as array), and `enabled` fields.

use std::path::Path;

use serde_json::json;

use crate::errors::Result;

use super::{
    backup_and_write_json, backup_config_file, load_jsonc_file, load_jsonc_file_strict,
    safe_write_json_file, AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext,
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

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        let config_dir = kilo_config_dir(&ctx.home);
        std::fs::create_dir_all(&config_dir).ok();
        let config_path = kilo_config_path(&ctx.home);
        install_mcp_server(&config_path, &ctx.tracedecay_bin)?;

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tracedecay init");
        eprintln!("  2. Start a new Kilo CLI session — tracedecay tools are now available");
        Ok(())
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn install_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        install_mcp_server(&project_path.join("kilo.json"), &ctx.tracedecay_bin)
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        let config_path = kilo_config_path(&ctx.home);
        uninstall_mcp_server(&config_path);

        eprintln!();
        eprintln!("Uninstall complete. Tracedecay has been removed from Kilo CLI.");
        eprintln!("Start a new Kilo CLI session for changes to take effect.");
        Ok(())
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mKilo CLI integration\x1b[0m");
        doctor_check_settings(dc, &ctx.home);
    }

    fn is_detected(&self, home: &Path) -> bool {
        kilo_config_dir(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(kilo_config_path(home))
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        let config_path = kilo_config_path(home);
        if !config_path.exists() {
            return false;
        }
        let json = load_jsonc_file(&config_path);
        let servers = json.get("mcp");
        servers.and_then(|v| v.get("tracedecay")).is_some()
            || servers.and_then(|v| v.get("tokensave")).is_some()
    }
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

fn install_mcp_server(config_path: &Path, tracedecay_bin: &str) -> Result<()> {
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let backup = backup_config_file(config_path)?;
    let mut settings = match load_jsonc_file_strict(config_path) {
        Ok(v) => v,
        Err(e) => {
            if let Some(ref b) = backup {
                eprintln!("  Backup preserved at: {}", b.display());
            }
            return Err(e);
        }
    };

    settings["mcp"]["tracedecay"] = json!({
        "type": "local",
        "command": [tracedecay_bin, "serve"],
        "enabled": true
    });

    safe_write_json_file(config_path, &settings, backup.as_deref())?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tracedecay MCP server to {}",
        config_path.display()
    );
    Ok(())
}

fn uninstall_mcp_server(config_path: &Path) {
    if !config_path.exists() {
        eprintln!("  {} not found, skipping", config_path.display());
        return;
    }

    let Ok(contents) = std::fs::read_to_string(config_path) else {
        return;
    };
    let Ok(mut settings) = serde_json::from_str::<serde_json::Value>(&contents) else {
        // Try JSONC parsing
        let mut settings = super::parse_jsonc(&contents);
        let Some(servers) = settings.get_mut("mcp").and_then(|v| v.as_object_mut()) else {
            return;
        };
        let removed_new = servers.remove("tracedecay").is_some();
        let removed_legacy = servers.remove("tokensave").is_some();
        if (removed_new || removed_legacy) && backup_and_write_json(config_path, &settings) {
            eprintln!(
                "\x1b[32m✔\x1b[0m Removed tracedecay/tokensave MCP server from {}",
                config_path.display()
            );
        }
        return;
    };

    let Some(servers) = settings.get_mut("mcp").and_then(|v| v.as_object_mut()) else {
        eprintln!(
            "  No tracedecay/tokensave MCP server in {}, skipping",
            config_path.display()
        );
        return;
    };

    let removed_new = servers.remove("tracedecay").is_some();
    let removed_legacy = servers.remove("tokensave").is_some();
    if !removed_new && !removed_legacy {
        eprintln!(
            "  No tracedecay/tokensave MCP server in {}, skipping",
            config_path.display()
        );
        return;
    }

    if backup_and_write_json(config_path, &settings) {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay/tokensave MCP server from {}",
            config_path.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

fn doctor_check_settings(dc: &mut DoctorCounters, home: &Path) {
    let config_path = kilo_config_path(home);

    if !config_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent kilo` if you use Kilo CLI",
            config_path.display()
        ));
        return;
    }

    let settings = load_jsonc_file(&config_path);
    let server = settings.get("mcp").and_then(|v| v.get("tracedecay"));

    if server.and_then(|v| v.as_object()).is_some() {
        dc.pass(&format!(
            "MCP server registered in {}",
            config_path.display()
        ));
    } else {
        dc.fail(&format!(
            "MCP server NOT registered in {} — run `tracedecay install --agent kilo`",
            config_path.display()
        ));
    }
}
