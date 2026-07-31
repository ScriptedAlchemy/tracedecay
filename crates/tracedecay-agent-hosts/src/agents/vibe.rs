//! Mistral Vibe agent integration.
//!
//! Handles registration of the tracedecay MCP server in Vibe's
//! `~/.vibe/config.toml` as a `[[mcp_servers]]` entry with stdio transport,
//! and prompt rules via `~/.vibe/prompts/cli.md`.

use std::path::Path;

use crate::errors::{Result, TraceDecayError};

use super::{AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext};

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

use super::prompt_rules::{PROMPT_RULE_MARKER, PromptRulesOptions};

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

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        let vibe_dir = vibe_home(&ctx.home);
        std::fs::create_dir_all(&vibe_dir).ok();

        let config_path = vibe_config_path(&ctx.home);
        install_mcp_server(&config_path, &ctx.tracedecay_bin)?;

        let prompt_dir = vibe_dir.join("prompts");
        std::fs::create_dir_all(&prompt_dir).ok();
        let prompt_path = vibe_prompt_path(&ctx.home);
        install_prompt_rules(&prompt_path)?;
        super::install_managed_skill_prompt_index(
            &ctx.home,
            &prompt_path,
            crate::automation::skill_targets::SkillInstallTarget::Agents,
        )?;

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tracedecay init");
        eprintln!("  2. Start a new Vibe session — tracedecay tools are now available");
        Ok(())
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn install_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        let vibe_dir = project_path.join(".vibe");
        let config_path = vibe_dir.join("config.toml");
        let prompt_path = vibe_dir.join("prompts/cli.md");
        super::ensure_project_local_safe_paths(
            project_path,
            [config_path.as_path(), prompt_path.as_path()],
        )?;
        std::fs::create_dir_all(&vibe_dir).ok();
        std::fs::create_dir_all(vibe_dir.join("prompts")).ok();

        install_mcp_server(&config_path, &ctx.tracedecay_bin)?;
        install_prompt_rules(&prompt_path)?;
        super::install_managed_skill_prompt_index(
            &ctx.home,
            &prompt_path,
            crate::automation::skill_targets::SkillInstallTarget::Agents,
        )
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        let config_path = vibe_config_path(&ctx.home);
        uninstall_mcp_server(&config_path);
        let prompt_path = vibe_prompt_path(&ctx.home);
        super::remove_managed_skill_prompt_index(
            &ctx.home,
            &prompt_path,
            crate::automation::skill_targets::SkillInstallTarget::Agents,
        )?;
        uninstall_prompt_rules(&prompt_path);

        eprintln!();
        eprintln!("Uninstall complete. Tracedecay has been removed from Mistral Vibe.");
        eprintln!("Start a new Vibe session for changes to take effect.");
        Ok(())
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
// Install helpers
// ---------------------------------------------------------------------------

/// Install or refresh tracedecay's `[[mcp_servers]]` entry (idempotent).
fn install_mcp_server(config_path: &Path, tracedecay_bin: &str) -> Result<()> {
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| TraceDecayError::Config {
            message: format!("failed to create {}: {error}", parent.display()),
        })?;
    }
    let mut config = super::load_toml_file(config_path)?;
    let root = config
        .as_table_mut()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("{} is not a TOML document", config_path.display()),
        })?;
    let servers = root
        .entry("mcp_servers")
        .or_insert_with(|| toml::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "{} has a non-array mcp_servers value; refusing to overwrite it",
                config_path.display()
            ),
        })?;
    let existing = servers
        .iter_mut()
        .find(|server| server.get("name").and_then(toml::Value::as_str) == Some("tracedecay"));
    if let Some(server) = existing {
        let table = server
            .as_table_mut()
            .ok_or_else(|| TraceDecayError::Config {
                message: format!(
                    "{} has a non-table tracedecay MCP entry; refusing to overwrite it",
                    config_path.display()
                ),
            })?;
        if table.get("command").and_then(toml::Value::as_str) == Some(tracedecay_bin) {
            eprintln!(
                "  tracedecay MCP server already current in {}, skipping",
                config_path.display()
            );
            return Ok(());
        }
        table.insert(
            "command".to_string(),
            toml::Value::String(tracedecay_bin.to_string()),
        );
    } else {
        let mut server = toml::Table::new();
        server.insert(
            "name".to_string(),
            toml::Value::String("tracedecay".to_string()),
        );
        server.insert(
            "transport".to_string(),
            toml::Value::String("stdio".to_string()),
        );
        server.insert(
            "command".to_string(),
            toml::Value::String(tracedecay_bin.to_string()),
        );
        server.insert(
            "args".to_string(),
            toml::Value::Array(vec![toml::Value::String("serve".to_string())]),
        );
        servers.push(toml::Value::Table(server));
    }
    super::write_toml_file(config_path, &config)?;

    eprintln!(
        "\x1b[32m✔\x1b[0m Registered tracedecay MCP server in {}",
        config_path.display()
    );
    Ok(())
}

/// Install-or-refresh prompt rules in the Vibe system prompt.
fn install_prompt_rules(prompt_path: &Path) -> Result<()> {
    let block = super::prompt_rules::standard_prompt_rules(
        PROMPT_RULE_MARKER,
        &PromptRulesOptions {
            extra_paragraphs: VIBE_EXTRA_PARAGRAPHS,
        },
    );
    super::prompt_rules::reconcile_prompt_rules(prompt_path, PROMPT_RULE_MARKER, &block)
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

/// Remove the tracedecay `[[mcp_servers]]` block from `config.toml`.
fn uninstall_mcp_server(config_path: &Path) {
    if !config_path.exists() {
        eprintln!("  {} not found, skipping", config_path.display());
        return;
    }

    let Ok(contents) = std::fs::read_to_string(config_path) else {
        return;
    };

    if !contents.contains(TOML_MARKER) {
        eprintln!(
            "  No tracedecay MCP server in {}, skipping",
            config_path.display()
        );
        return;
    }

    // Remove the [[mcp_servers]] block that contains name = "tracedecay".
    // Strategy: split into lines, find the block, remove it.
    let lines: Vec<&str> = contents.lines().collect();
    let mut result: Vec<&str> = Vec::new();
    let mut skip = false;

    for line in &lines {
        if line.trim() == "[[mcp_servers]]" {
            // Peek ahead to see if this block is the tracedecay one.
            // We'll collect the block and decide whether to keep it.
            skip = false;
        }

        if skip {
            // If we hit a new section header, stop skipping.
            let trimmed = line.trim();
            if trimmed.starts_with("[[") || (trimmed.starts_with('[') && !trimmed.starts_with("[["))
            {
                skip = false;
            } else {
                continue;
            }
        }

        if line.contains(TOML_MARKER) {
            // This line is inside the tracedecay block — remove it and
            // the preceding [[mcp_servers]] header.
            // Pop the header we already pushed.
            while let Some(last) = result.last() {
                if last.trim() == "[[mcp_servers]]" {
                    result.pop();
                    break;
                }
                // Pop blank lines between header and this line
                if last.trim().is_empty() {
                    result.pop();
                } else {
                    break;
                }
            }
            skip = true;
            continue;
        }

        result.push(line);
    }

    // Trim trailing blank lines
    while result.last().is_some_and(|l| l.trim().is_empty()) {
        result.pop();
    }

    let new_contents = if result.is_empty() {
        String::new()
    } else {
        format!("{}\n", result.join("\n"))
    };

    if new_contents.trim().is_empty() {
        std::fs::remove_file(config_path).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed {} (was empty)",
            config_path.display()
        );
    } else {
        std::fs::write(config_path, &new_contents).ok();
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay MCP server from {}",
            config_path.display()
        );
    }
}

/// Remove tracedecay rules from the Vibe system prompt.
fn uninstall_prompt_rules(prompt_path: &Path) {
    super::prompt_rules::remove_prompt_rules(prompt_path, PROMPT_RULE_MARKER);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_rewrites_a_stale_tracedecay_command_and_preserves_other_servers() {
        let home = tempfile::tempdir().unwrap();
        let config_path = vibe_config_path(home.path());
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            r#"[[mcp_servers]]
name = "other"
transport = "stdio"
command = "/bin/other"
args = ["serve"]

[[mcp_servers]]
name = "tracedecay"
transport = "stdio"
command = "/old/tracedecay"
args = ["serve"]
"#,
        )
        .unwrap();

        install_mcp_server(&config_path, "/new/tracedecay").unwrap();

        let config = super::super::load_toml_file(&config_path).unwrap();
        let servers = config["mcp_servers"].as_array().unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers[0]["name"].as_str(), Some("other"));
        assert_eq!(servers[0]["command"].as_str(), Some("/bin/other"));
        assert_eq!(servers[1]["name"].as_str(), Some("tracedecay"));
        assert_eq!(servers[1]["command"].as_str(), Some("/new/tracedecay"));
    }

    #[test]
    fn healthcheck_rejects_a_stale_tracedecay_command() {
        let home = tempfile::tempdir().unwrap();
        let config_path = vibe_config_path(home.path());
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let expected = super::super::which_tracedecay().unwrap_or_else(|| "tracedecay".into());
        std::fs::write(
            &config_path,
            format!(
                "[[mcp_servers]]\nname = \"tracedecay\"\ntransport = \"stdio\"\ncommand = \"{expected}-stale\"\nargs = [\"serve\"]\n"
            ),
        )
        .unwrap();

        let mut counters = DoctorCounters::new();
        doctor_check_config(&mut counters, home.path());

        assert_eq!(
            counters.issues, 1,
            "Doctor must reject a registration that names the wrong binary"
        );
    }
}
