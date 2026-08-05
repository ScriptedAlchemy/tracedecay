//! GitHub Copilot integration.
//!
//! Handles registration of the tracedecay MCP server in both:
//! - VS Code's `settings.json` under `mcp.servers.tracedecay`
//! - Copilot CLI's `~/.copilot/mcp-config.json` under `mcpServers.tracedecay`

use std::path::Path;

use crate::errors::Result;

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, load_json_file, load_jsonc_file,
};

/// GitHub Copilot agent.
pub struct CopilotIntegration;

impl AgentIntegration for CopilotIntegration {
    fn name(&self) -> &'static str {
        "GitHub Copilot"
    }

    fn id(&self) -> &'static str {
        "copilot"
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mGitHub Copilot integration\x1b[0m");
        doctor_check_vscode_settings(dc, &super::vscode_data_dir(&ctx.home), "VS Code");
        doctor_check_vscode_settings(
            dc,
            &super::vscode_insiders_data_dir(&ctx.home),
            "VS Code Insiders",
        );
        doctor_check_cli_settings(dc, &ctx.home);
    }

    fn is_detected(&self, home: &Path) -> bool {
        super::vscode_data_dir(home).join("User").is_dir()
            || super::vscode_insiders_data_dir(home).join("User").is_dir()
            || super::copilot_cli_dir(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(super::vscode_data_dir(home).join("User/settings.json"))
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        let vscode_settings_path = super::vscode_data_dir(home).join("User/settings.json");
        let insiders_settings_path =
            super::vscode_insiders_data_dir(home).join("User/settings.json");
        let cli_settings_path = super::copilot_cli_dir(home).join("mcp-config.json");

        let vscode_has_tracedecay = if vscode_settings_path.exists() {
            let json = load_jsonc_file(&vscode_settings_path);
            let servers = json.get("mcp").and_then(|v| v.get("servers"));
            servers.and_then(|v| v.get("tracedecay")).is_some()
        } else {
            false
        };

        let insiders_has_tracedecay = if insiders_settings_path.exists() {
            let json = load_jsonc_file(&insiders_settings_path);
            let servers = json.get("mcp").and_then(|v| v.get("servers"));
            servers.and_then(|v| v.get("tracedecay")).is_some()
        } else {
            false
        };

        let cli_has_tracedecay = if cli_settings_path.exists() {
            let json = load_json_file(&cli_settings_path);
            let servers = json.get("mcpServers");
            servers.and_then(|v| v.get("tracedecay")).is_some()
        } else {
            false
        };

        vscode_has_tracedecay || insiders_has_tracedecay || cli_has_tracedecay
    }

    fn export_managed_skills(
        &self,
        home: &Path,
        profile_root: &Path,
    ) -> Result<Vec<crate::automation::skill_targets::SkillInstallSummary>> {
        if !self.has_tracedecay(home) {
            return Ok(Vec::new());
        }
        let prompt_paths = [
            super::vscode_data_dir(home).join("User/prompts/copilot-instructions.md"),
            super::vscode_insiders_data_dir(home).join("User/prompts/copilot-instructions.md"),
            super::copilot_cli_dir(home).join("copilot-instructions.md"),
        ];
        prompt_paths
            .iter()
            .filter(|path| path.exists())
            .map(|path| {
                crate::automation::skill_targets::install_managed_skills(
                    profile_root,
                    crate::automation::skill_targets::SkillInstallTarget::Agents,
                    path,
                )
            })
            .collect()
    }

    fn export_managed_skills_local(
        &self,
        project_root: &Path,
        profile_root: &Path,
    ) -> Result<Vec<crate::automation::skill_targets::SkillInstallSummary>> {
        let instructions = project_root.join(".github/copilot-instructions.md");
        if !workspace_mcp_has_tracedecay(project_root) || !instructions.exists() {
            return Ok(Vec::new());
        }
        Ok(vec![
            crate::automation::skill_targets::install_managed_skills(
                profile_root,
                crate::automation::skill_targets::SkillInstallTarget::Agents,
                &instructions,
            )?,
        ])
    }
}

fn workspace_mcp_has_tracedecay(project_root: &Path) -> bool {
    let settings_path = project_root.join(".vscode/mcp.json");
    if !settings_path.exists() {
        return false;
    }
    let json = load_jsonc_file(&settings_path);
    json.get("servers")
        .and_then(|servers| servers.get("tracedecay"))
        .is_some()
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

/// Check VS Code (or VS Code Insiders) settings.json has tracedecay MCP server registered.
fn doctor_check_vscode_settings(dc: &mut DoctorCounters, vscode_dir: &Path, label: &str) {
    let settings_path = vscode_dir.join("User/settings.json");

    if !settings_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent copilot` if you use GitHub Copilot in {label}",
            settings_path.display()
        ));
        return;
    }

    let settings = load_jsonc_file(&settings_path);
    let server = settings
        .get("mcp")
        .and_then(|v| v.get("servers"))
        .and_then(|v| v.get("tracedecay"));

    let Some(server) = server.and_then(|v| v.as_object()) else {
        dc.fail(&format!(
            "MCP server NOT registered in {} — run `tracedecay install --agent copilot`",
            settings_path.display()
        ));
        return;
    };
    dc.pass(&format!(
        "MCP server registered in {}",
        settings_path.display()
    ));

    // Check args include "serve"
    let has_serve = server
        .get("args")
        .and_then(|v| v.as_array())
        .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some("serve")));
    if has_serve {
        dc.pass("MCP server args include \"serve\"");
    } else {
        dc.fail("MCP server args missing \"serve\" — run `tracedecay install --agent copilot`");
    }
}

/// Check Copilot CLI mcp-config.json has tracedecay MCP server registered.
fn doctor_check_cli_settings(dc: &mut DoctorCounters, home: &Path) {
    let settings_path = super::copilot_cli_dir(home).join("mcp-config.json");

    if !settings_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent copilot` if you use Copilot CLI",
            settings_path.display()
        ));
        return;
    }

    let settings = load_json_file(&settings_path);
    let server = settings.get("mcpServers").and_then(|v| v.get("tracedecay"));

    let Some(server) = server.and_then(|v| v.as_object()) else {
        dc.fail(&format!(
            "MCP server NOT registered in {} — run `tracedecay install --agent copilot`",
            settings_path.display()
        ));
        return;
    };
    dc.pass(&format!(
        "MCP server registered in {}",
        settings_path.display()
    ));

    let has_serve = server
        .get("args")
        .and_then(|v| v.as_array())
        .is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some("serve")));
    if has_serve {
        dc.pass("MCP server args include \"serve\"");
    } else {
        dc.fail("MCP server args missing \"serve\" — run `tracedecay install --agent copilot`");
    }
}
