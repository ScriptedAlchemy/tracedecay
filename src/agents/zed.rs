//! Zed agent integration.
//!
//! Handles registration of the tracedecay MCP server in Zed's `settings.json`
//! under the `context_servers.tracedecay` key.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::errors::Result;

use super::{
    backup_and_write_json, backup_config_file, load_jsonc_file, load_jsonc_file_strict,
    safe_write_json_file, AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext,
};

/// Zed agent.
pub struct ZedIntegration;

/// Returns the Zed config directory, platform-specific.
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

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        let config_dir = zed_config_dir(&ctx.home);
        let settings_path = config_dir.join("settings.json");
        install_context_server(&settings_path, &ctx.tracedecay_bin)?;

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tracedecay init");
        eprintln!("  2. Restart Zed — tracedecay tools are now available");
        Ok(())
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn install_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        install_context_server(
            &project_path.join(".zed/settings.json"),
            &ctx.tracedecay_bin,
        )
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        let settings_path = zed_config_dir(&ctx.home).join("settings.json");
        uninstall_context_server(&settings_path);

        eprintln!();
        eprintln!("Uninstall complete. Tracedecay has been removed from Zed.");
        eprintln!("Restart Zed for changes to take effect.");
        Ok(())
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mZed integration\x1b[0m");
        doctor_check_settings(dc, &ctx.home);
    }

    fn is_detected(&self, home: &Path) -> bool {
        zed_config_dir(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(zed_config_dir(home).join("settings.json"))
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        let settings_path = zed_config_dir(home).join("settings.json");
        if !settings_path.exists() {
            return false;
        }
        let json = load_jsonc_file(&settings_path);
        let servers = json.get("context_servers");
        servers.and_then(|v| v.get("tracedecay")).is_some()
            || servers.and_then(|v| v.get("tokensave")).is_some()
    }
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

fn install_context_server(settings_path: &Path, tracedecay_bin: &str) -> Result<()> {
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let backup = backup_config_file(settings_path)?;
    let mut settings = match load_jsonc_file_strict(settings_path) {
        Ok(v) => v,
        Err(e) => {
            if let Some(ref b) = backup {
                eprintln!("  Backup preserved at: {}", b.display());
            }
            return Err(e);
        }
    };
    settings["context_servers"]["tracedecay"] = json!({
        "command": {
            "path": tracedecay_bin,
            "args": ["serve"]
        }
    });

    safe_write_json_file(settings_path, &settings, backup.as_deref())?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tracedecay context server to {}",
        settings_path.display()
    );
    Ok(())
}

/// Remove context server entry from Zed settings.json.
/// Does not delete settings.json even if object is otherwise empty.
fn uninstall_context_server(settings_path: &Path) {
    if !settings_path.exists() {
        eprintln!("  {} not found, skipping", settings_path.display());
        return;
    }

    let mut settings = load_jsonc_file(settings_path);

    let removed = if let Some(map) = settings
        .get_mut("context_servers")
        .and_then(|v| v.as_object_mut())
    {
        let removed_new = map.remove("tracedecay").is_some();
        let removed_legacy = map.remove("tokensave").is_some();
        removed_new || removed_legacy
    } else {
        false
    };

    if !removed {
        eprintln!(
            "  No tracedecay/tokensave context server in {}, skipping",
            settings_path.display()
        );
        return;
    }

    // Clean up empty "context_servers" object
    let cs_empty = settings
        .get("context_servers")
        .and_then(|v| v.as_object())
        .is_some_and(serde_json::Map::is_empty);
    if cs_empty {
        settings
            .as_object_mut()
            .map(|o| o.remove("context_servers"));
    }

    // Always write back (never delete settings.json — it has other Zed settings).
    // backup_and_write_json leaves a .bak so any mistake is recoverable (issue #63).
    if backup_and_write_json(settings_path, &settings) {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay/tokensave context server from {}",
            settings_path.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

/// Check Zed settings.json has tracedecay context server registered.
fn doctor_check_settings(dc: &mut DoctorCounters, home: &Path) {
    let settings_path = zed_config_dir(home).join("settings.json");

    if !settings_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent zed` if you use Zed",
            settings_path.display()
        ));
        return;
    }

    let settings = load_jsonc_file(&settings_path);
    let server = settings
        .get("context_servers")
        .and_then(|v| v.get("tracedecay"));

    if server.and_then(|v| v.as_object()).is_some() {
        dc.pass(&format!(
            "Context server registered in {}",
            settings_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Context server NOT registered in {} — run `tracedecay install --agent zed`",
            settings_path.display()
        ));
    }
}
