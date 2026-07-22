//! Cline agent integration.
//!
//! Handles registration of the tracedecay MCP server in Cline's
//! `cline_mcp_settings.json` under the `mcpServers.tracedecay` key.

use std::env;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::errors::{Result, TraceDecayError};

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, backup_config_file,
    load_json_file, load_json_file_strict, safe_write_json_file,
};

/// Cline agent.
pub struct ClineIntegration;

fn cline_data_dir(home: &Path) -> PathBuf {
    env::var_os("CLINE_DATA_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cline/data"))
}

/// Current Cline CLI/IDE user MCP settings path.
fn cline_mcp_settings_path(home: &Path) -> PathBuf {
    cline_data_dir(home).join("settings/cline_mcp_settings.json")
}

/// Legacy VS Code extension storage path retained only for migration/removal.
fn legacy_cline_mcp_settings_path(home: &Path) -> PathBuf {
    super::vscode_data_dir(home)
        .join("User/globalStorage/saoudrizwan.claude-dev")
        .join("settings/cline_mcp_settings.json")
}

fn cline_settings_paths(home: &Path) -> [PathBuf; 2] {
    [
        cline_mcp_settings_path(home),
        legacy_cline_mcp_settings_path(home),
    ]
}

fn settings_have_tracedecay(path: &Path) -> bool {
    path.exists()
        && load_json_file(path)
            .get("mcpServers")
            .and_then(|servers| servers.get("tracedecay"))
            .is_some()
}

impl AgentIntegration for ClineIntegration {
    fn name(&self) -> &'static str {
        "Cline"
    }

    fn id(&self) -> &'static str {
        "cline"
    }

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        let settings_path = cline_mcp_settings_path(&ctx.home);
        install_mcp_server(&settings_path, &ctx.tracedecay_bin)?;
        let legacy_path = legacy_cline_mcp_settings_path(&ctx.home);
        if legacy_path != settings_path && settings_have_tracedecay(&legacy_path) {
            uninstall_mcp_server(&legacy_path)?;
            eprintln!(
                "\x1b[32m✔\x1b[0m Removed legacy duplicate registration from {}",
                legacy_path.display()
            );
        }

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tracedecay init");
        eprintln!("  2. Restart Cline — tracedecay tools are now available");
        Ok(())
    }

    fn supports_local_install(&self) -> bool {
        false
    }

    fn install_local(&self, _ctx: &InstallContext, _project_path: &Path) -> Result<()> {
        Err(TraceDecayError::Config {
            message: "Cline does not currently document or ship a project-local MCP config path. \
                      `tracedecay install --local --agent cline` is unsupported. \
                      Run `tracedecay install --agent cline` for a global install."
                .to_string(),
        })
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        for settings_path in cline_settings_paths(&ctx.home) {
            uninstall_mcp_server(&settings_path)?;
        }

        eprintln!();
        eprintln!("Uninstall complete. Tracedecay has been removed from Cline.");
        eprintln!("Restart VS Code for changes to take effect.");
        Ok(())
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mCline integration\x1b[0m");
        doctor_check_settings(dc, &ctx.home);
    }

    fn host_component_registration(
        &self,
        _component: super::host_bundle_v2::HostBundleComponentV1,
        ctx: &HealthcheckContext,
    ) -> super::host_bundle_v2::HostBundleRegistrationStateV1 {
        use super::host_bundle_v2::HostBundleRegistrationStateV1 as State;

        let path = cline_mcp_settings_path(&ctx.home);
        let Ok(bytes) = std::fs::read(path) else {
            return State::Missing;
        };
        let Ok(settings) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return State::Corrupt;
        };
        if settings
            .pointer("/mcpServers/tracedecay/disabled")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
            && settings
                .pointer("/mcpServers/tracedecay/args")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|args| args.iter().any(|arg| arg.as_str() == Some("serve")))
        {
            State::Current
        } else {
            State::Missing
        }
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".cline").is_dir()
            || legacy_cline_mcp_settings_path(home)
                .parent()
                .is_some_and(Path::is_dir)
    }

    fn primary_config_path(&self, home: &Path) -> Option<PathBuf> {
        Some(cline_mcp_settings_path(home))
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        cline_settings_paths(home)
            .iter()
            .any(|path| settings_have_tracedecay(path))
    }
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

fn install_mcp_server(settings_path: &Path, tracedecay_bin: &str) -> Result<()> {
    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| TraceDecayError::Config {
            message: format!(
                "cannot create Cline config directory {}: {error}",
                parent.display()
            ),
        })?;
    }

    let backup = backup_config_file(settings_path)?;
    let mut settings = match load_json_file_strict(settings_path) {
        Ok(v) => v,
        Err(e) => {
            if let Some(ref b) = backup {
                eprintln!("  Backup preserved at: {}", b.display());
            }
            return Err(e);
        }
    };
    settings["mcpServers"]["tracedecay"] = json!({
        "command": tracedecay_bin,
        "args": ["serve"],
        "disabled": false
    });

    safe_write_json_file(settings_path, &settings, backup.as_deref())?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tracedecay MCP server to {}",
        settings_path.display()
    );
    Ok(())
}

/// Remove MCP server entry from Cline's `cline_mcp_settings.json`.
fn uninstall_mcp_server(settings_path: &Path) -> Result<()> {
    if !settings_path.exists() {
        eprintln!("  {} not found, skipping", settings_path.display());
        return Ok(());
    }

    let mut settings = load_json_file_strict(settings_path)?;

    let Some(servers) = settings
        .get_mut("mcpServers")
        .and_then(|v| v.as_object_mut())
    else {
        eprintln!(
            "  No tracedecay MCP server in {}, skipping",
            settings_path.display()
        );
        return Ok(());
    };

    if servers.remove("tracedecay").is_none() {
        eprintln!(
            "  No tracedecay MCP server in {}, skipping",
            settings_path.display()
        );
        return Ok(());
    }

    let backup = backup_config_file(settings_path)?;
    safe_write_json_file(settings_path, &settings, backup.as_deref())?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Removed tracedecay MCP server from {}",
        settings_path.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

/// Check Cline's `cline_mcp_settings.json` has tracedecay MCP server registered.
fn doctor_check_settings(dc: &mut DoctorCounters, home: &Path) {
    let settings_path = cline_mcp_settings_path(home);

    if settings_have_tracedecay(&settings_path) {
        dc.pass(&format!(
            "MCP server registered in {}",
            settings_path.display()
        ));
        return;
    }
    let legacy_path = legacy_cline_mcp_settings_path(home);
    if settings_have_tracedecay(&legacy_path) {
        dc.warn(&format!(
            "legacy Cline MCP registration found in {} — run `tracedecay install --agent cline` to repair",
            legacy_path.display()
        ));
        return;
    }
    dc.fail(&format!(
        "MCP server NOT registered in {} — run `tracedecay install --agent cline`",
        settings_path.display()
    ));
}
