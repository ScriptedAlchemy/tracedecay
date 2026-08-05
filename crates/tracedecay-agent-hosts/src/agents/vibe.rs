//! Mistral Vibe agent integration.
//!
//! Handles registration of the tracedecay MCP server in Vibe's
//! `~/.vibe/config.toml` as a `[[mcp_servers]]` entry with stdio transport,
//! and prompt rules via `~/.vibe/prompts/cli.md`.

use std::path::Path;

use crate::errors::Result;

use super::{AgentIntegration, DoctorCounters, HealthcheckContext};

/// Mistral Vibe agent.
pub struct VibeIntegration;

/// Returns the Vibe home directory.
/// Respects `VIBE_HOME` only when it falls under `home` (so tests with
/// temp-dir homes are not polluted by the real user's environment).
fn vibe_home(home: &Path) -> std::path::PathBuf {
    if let Ok(vibe) = std::env::var("VIBE_HOME") {
        let vibe_path = std::path::PathBuf::from(&vibe);
        if vibe_path.starts_with(home) {
            return vibe_path;
        }
    }
    home.join(".vibe")
}

fn vibe_config_path(home: &Path) -> std::path::PathBuf {
    vibe_home(home).join("config.toml")
}

fn vibe_prompt_path(home: &Path) -> std::path::PathBuf {
    vibe_home(home).join("prompts/cli.md")
}

/// The TOML marker that identifies a tracedecay MCP server entry.
const TOML_MARKER: &str = "name = \"tracedecay\"";

/// Vibe-only closing paragraph appended after the shared rules text.
const VIBE_EXTRA_PARAGRAPHS: &[&str] = &["When a tracedecay tool result contains a \
     `tracedecay_metrics:` line, report the savings to the user (e.g. \"TraceDecay'd ~N \
     tokens\"). Never silently omit this."];

impl AgentIntegration for VibeIntegration {
    fn name(&self) -> &'static str {
        "Mistral Vibe"
    }

    fn id(&self) -> &'static str {
        "vibe"
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mMistral Vibe integration\x1b[0m");
        doctor_check_config(dc, &ctx.home);
        doctor_check_prompt(dc, &ctx.home);
    }

    fn is_detected(&self, home: &Path) -> bool {
        vibe_home(home).is_dir()
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        let config_path = vibe_config_path(home);
        if !config_path.exists() {
            return false;
        }
        let contents = std::fs::read_to_string(&config_path).unwrap_or_default();
        contents.contains(TOML_MARKER)
    }

    fn export_managed_skills(
        &self,
        home: &Path,
        profile_root: &Path,
    ) -> Result<Vec<crate::automation::skill_targets::SkillInstallSummary>> {
        let prompt_path = vibe_prompt_path(home);
        if !self.has_tracedecay(home) || !prompt_path.exists() {
            return Ok(Vec::new());
        }
        Ok(vec![
            crate::automation::skill_targets::install_managed_skills(
                profile_root,
                crate::automation::skill_targets::SkillInstallTarget::Agents,
                &prompt_path,
            )?,
        ])
    }

    fn export_managed_skills_local(
        &self,
        project_root: &Path,
        profile_root: &Path,
    ) -> Result<Vec<crate::automation::skill_targets::SkillInstallSummary>> {
        let prompt_path = project_root.join(".vibe/prompts/cli.md");
        if !local_config_has_tracedecay(project_root) || !prompt_path.exists() {
            return Ok(Vec::new());
        }
        Ok(vec![
            crate::automation::skill_targets::install_managed_skills(
                profile_root,
                crate::automation::skill_targets::SkillInstallTarget::Agents,
                &prompt_path,
            )?,
        ])
    }
}

fn local_config_has_tracedecay(project_root: &Path) -> bool {
    let config_path = project_root.join(".vibe/config.toml");
    if !config_path.exists() {
        return false;
    }
    let contents = std::fs::read_to_string(&config_path).unwrap_or_default();
    contents.contains(TOML_MARKER)
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

fn doctor_check_config(dc: &mut DoctorCounters, home: &Path) {
    let config_path = vibe_config_path(home);

    if !config_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent vibe` if you use Mistral Vibe",
            config_path.display()
        ));
        return;
    }

    let Ok(config) = super::load_toml_file(&config_path) else {
        dc.fail(&format!("could not parse {}", config_path.display()));
        return;
    };
    let registered = config
        .get("mcp_servers")
        .and_then(toml::Value::as_array)
        .and_then(|servers| {
            servers.iter().find(|server| {
                server.get("name").and_then(toml::Value::as_str) == Some("tracedecay")
            })
        })
        .and_then(|server| server.get("command"))
        .and_then(toml::Value::as_str);
    let Some(expected) = super::which_tracedecay() else {
        dc.fail("could not resolve the active tracedecay binary for Vibe");
        return;
    };
    match registered {
        Some(command) if command == expected => dc.pass(&format!(
            "MCP server registered with the current binary in {}",
            config_path.display()
        )),
        Some(command) => dc.fail(&format!(
            "MCP server in {} uses stale command `{command}`; expected `{expected}` — run `tracedecay install --agent vibe`",
            config_path.display()
        )),
        None => dc.fail(&format!(
            "MCP server NOT registered in {} — run `tracedecay install --agent vibe`",
            config_path.display()
        )),
    }
}

fn doctor_check_prompt(dc: &mut DoctorCounters, home: &Path) {
    let prompt_path = vibe_prompt_path(home);
    if prompt_path.exists() {
        let has_rules = std::fs::read_to_string(&prompt_path)
            .unwrap_or_default()
            .contains("tracedecay");
        if has_rules {
            dc.pass("Vibe prompt contains tracedecay rules");
        } else {
            dc.fail("Vibe prompt missing tracedecay rules — run `tracedecay install --agent vibe`");
        }
    } else {
        dc.warn("Vibe prompt does not exist");
    }
}
