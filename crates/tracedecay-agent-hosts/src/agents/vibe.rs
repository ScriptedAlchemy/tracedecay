//! Mistral Vibe agent integration.
//!
//! Handles registration of the tracedecay MCP server in Vibe's
//! `~/.vibe/config.toml` as a `[[mcp_servers]]` entry with stdio transport,
//! and prompt rules via `~/.vibe/prompts/cli.md`.
//!
//! **Manual by necessity, not by preference (verified 2026-08-08).** The owner
//! policy is CLI-first, so this config write needs a justification. Vibe's
//! `vibe mcp add` is genuinely non-interactive and `vibe mcp remove <name>`
//! exists — but `add` is **remote-transport only** (`--url`, `--transport`,
//! `--header`, `--api-key-*`). It has no `--command`/`--args`, so a local
//! stdio server, which is exactly what `tracedecay serve` is, has no
//! representation on that command line; Mistral's own documentation registers
//! stdio servers by editing `config.toml`. Adopting `remove` alone would leave
//! the lifecycle half-driven, with the registration created by one authority
//! and destroyed by another. This is the closest host to adoptable: a single
//! stdio `add` flag would flip the verdict outright. See
//! <https://github.com/mistralai/mistral-vibe/blob/main/README.md> and
//! <https://docs.mistral.ai/vibe/code/cli/mcp-servers>.

use std::path::{Path, PathBuf};

use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, value};

use crate::errors::{Result, TraceDecayError};

use super::host_bundle_v2::{HostBundleComponentV1, HostBundleRegistrationStateV1};
use super::prompt_rules::{PROMPT_RULE_MARKER, PromptRulesOptions};
use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, TextFileMutation,
    config_backup_path, update_config_file_transactionally,
};

pub struct VibeIntegration;

/// Respects `VIBE_HOME` only when it falls under `home` (so tests with
/// temp-dir homes are not polluted by the real user's environment).
fn vibe_home(home: &Path) -> std::path::PathBuf {
    super::host_home_override(home, "VIBE_HOME", ".vibe")
}

fn vibe_config_path(home: &Path) -> std::path::PathBuf {
    vibe_home(home).join("config.toml")
}

fn vibe_prompt_path(home: &Path) -> std::path::PathBuf {
    vibe_home(home).join("prompts/cli.md")
}

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
        let project_config = vibe_project_config_path(&ctx.project_path);
        if vibe_config_path(&ctx.home).exists() || !project_config.exists() {
            doctor_check_config(dc, &vibe_config_path(&ctx.home), "vibe");
            doctor_check_prompt(dc, &vibe_prompt_path(&ctx.home), "Vibe prompt");
        }
        if project_config.exists() {
            doctor_check_config(dc, &project_config, "vibe --local");
            doctor_check_prompt(
                dc,
                &vibe_project_prompt_path(&ctx.project_path),
                "Vibe project prompt",
            );
        }
    }

    fn reports_absence_to_doctor(&self) -> bool {
        true
    }

    fn host_component_registration(
        &self,
        component: HostBundleComponentV1,
        ctx: &HealthcheckContext,
    ) -> HostBundleRegistrationStateV1 {
        vibe_component_state(
            component,
            &vibe_config_path(&ctx.home),
            &vibe_prompt_path(&ctx.home),
            None,
        )
    }

    fn host_component_registration_for_lifecycle(
        &self,
        component: HostBundleComponentV1,
        ctx: &HealthcheckContext,
        install: &InstallContext,
    ) -> HostBundleRegistrationStateV1 {
        vibe_component_state(
            component,
            &vibe_config_path(&ctx.home),
            &vibe_prompt_path(&ctx.home),
            Some(&install.tracedecay_bin),
        )
    }

    fn project_host_component_registration_for_lifecycle(
        &self,
        component: HostBundleComponentV1,
        ctx: &HealthcheckContext,
        install: &InstallContext,
    ) -> HostBundleRegistrationStateV1 {
        vibe_component_state(
            component,
            &vibe_project_config_path(&ctx.project_path),
            &vibe_project_prompt_path(&ctx.project_path),
            Some(&install.tracedecay_bin),
        )
    }

    fn is_detected(&self, home: &Path) -> bool {
        vibe_home(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<PathBuf> {
        Some(vibe_config_path(home))
    }

    fn host_component_registration_paths(
        &self,
        components: &[HostBundleComponentV1],
        home: &Path,
    ) -> Vec<PathBuf> {
        vibe_registration_paths(
            components,
            vibe_config_path(home),
            vibe_prompt_path(home),
        )
    }

    fn project_host_component_registration_paths(
        &self,
        components: &[HostBundleComponentV1],
        _home: &Path,
        project_path: &Path,
    ) -> Result<Vec<PathBuf>> {
        Ok(vibe_registration_paths(
            components,
            vibe_project_config_path(project_path),
            vibe_project_prompt_path(project_path),
        ))
    }

    fn activate_deployed_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        activate_vibe_components(
            components,
            ctx,
            &vibe_config_path(&ctx.home),
            &vibe_prompt_path(&ctx.home),
        )
    }

    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        deactivate_vibe_components(
            components,
            ctx,
            &vibe_config_path(&ctx.home),
            &vibe_prompt_path(&ctx.home),
        )
    }

    fn activate_project_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        let config = vibe_project_config_path(project_path);
        let prompt = vibe_project_prompt_path(project_path);
        let paths = vibe_registration_paths(components, config.clone(), prompt.clone());
        super::ensure_project_local_safe_paths(project_path, paths.iter().map(PathBuf::as_path))?;
        activate_vibe_components(components, ctx, &config, &prompt)
    }

    fn deactivate_project_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        let config = vibe_project_config_path(project_path);
        let prompt = vibe_project_prompt_path(project_path);
        let paths = vibe_registration_paths(components, config.clone(), prompt.clone());
        super::ensure_project_local_safe_paths(project_path, paths.iter().map(PathBuf::as_path))?;
        deactivate_vibe_components(components, ctx, &config, &prompt)
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        vibe_mcp_state(&vibe_config_path(home), None) == HostBundleRegistrationStateV1::Current
    }

    fn export_managed_skills(
        &self,
        home: &Path,
        profile_root: &Path,
    ) -> Result<Vec<tracedecay_automation_runtime::automation::skill_targets::SkillInstallSummary>>
    {
        let prompt_path = vibe_prompt_path(home);
        if !self.has_tracedecay(home) || !prompt_path.exists() {
            return Ok(Vec::new());
        }
        Ok(vec![
            tracedecay_automation_runtime::automation::skill_targets::install_managed_skills(
                profile_root,
                tracedecay_automation_runtime::automation::skill_targets::SkillInstallTarget::Agents,
                &prompt_path,
            )?,
        ])
    }

    fn export_managed_skills_local(
        &self,
        project_root: &Path,
        profile_root: &Path,
    ) -> Result<Vec<tracedecay_automation_runtime::automation::skill_targets::SkillInstallSummary>>
    {
        let prompt_path = project_root.join(".vibe/prompts/cli.md");
        if !local_config_has_tracedecay(project_root) || !prompt_path.exists() {
            return Ok(Vec::new());
        }
        Ok(vec![
            tracedecay_automation_runtime::automation::skill_targets::install_managed_skills(
                profile_root,
                tracedecay_automation_runtime::automation::skill_targets::SkillInstallTarget::Agents,
                &prompt_path,
            )?,
        ])
    }
}

fn local_config_has_tracedecay(project_root: &Path) -> bool {
    vibe_mcp_state(&vibe_project_config_path(project_root), None)
        == HostBundleRegistrationStateV1::Current
}

fn vibe_project_config_path(project: &Path) -> PathBuf {
    project.join(".vibe/config.toml")
}

fn vibe_project_prompt_path(project: &Path) -> PathBuf {
    project.join(".vibe/prompts/cli.md")
}

fn vibe_registration_paths(
    components: &[HostBundleComponentV1],
    config: PathBuf,
    prompt: PathBuf,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if components.contains(&HostBundleComponentV1::ContextMcp) {
        paths.push(config.clone());
        paths.push(config_backup_path(&config));
    }
    if components.contains(&HostBundleComponentV1::Agent) {
        paths.push(prompt);
    }
    paths
}

fn vibe_component_state(
    component: HostBundleComponentV1,
    config: &Path,
    prompt: &Path,
    expected_binary: Option<&str>,
) -> HostBundleRegistrationStateV1 {
    match component {
        HostBundleComponentV1::ContextMcp => vibe_mcp_state(config, expected_binary),
        HostBundleComponentV1::Agent => match std::fs::read_to_string(prompt) {
            Ok(contents) if contents.contains(PROMPT_RULE_MARKER) => {
                HostBundleRegistrationStateV1::Current
            }
            Ok(_) => HostBundleRegistrationStateV1::Repairable,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                HostBundleRegistrationStateV1::Missing
            }
            Err(_) => HostBundleRegistrationStateV1::Corrupt,
        },
        HostBundleComponentV1::Core | HostBundleComponentV1::OperatorMcp => {
            HostBundleRegistrationStateV1::Missing
        }
    }
}

fn vibe_mcp_state(
    config_path: &Path,
    expected_binary: Option<&str>,
) -> HostBundleRegistrationStateV1 {
    let contents = match std::fs::read_to_string(config_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return HostBundleRegistrationStateV1::Missing;
        }
        Err(_) => return HostBundleRegistrationStateV1::Corrupt,
    };
    let config = match toml::from_str::<toml::Value>(&contents) {
        Ok(config) => config,
        Err(_) => return HostBundleRegistrationStateV1::Corrupt,
    };
    let Some(server) = config
        .get("mcp_servers")
        .and_then(toml::Value::as_array)
        .and_then(|servers| {
            servers
                .iter()
                .find(|server| server.get("name").and_then(toml::Value::as_str) == Some("tracedecay"))
        })
    else {
        return HostBundleRegistrationStateV1::Missing;
    };
    let command_is_current = server
        .get("command")
        .and_then(toml::Value::as_str)
        .is_some_and(|command| {
            expected_binary.map_or_else(|| !command.is_empty(), |expected| command == expected)
        });
    let serves = server
        .get("args")
        .and_then(toml::Value::as_array)
        .is_some_and(|args| args.iter().any(|arg| arg.as_str() == Some("serve")));
    if command_is_current && serves {
        HostBundleRegistrationStateV1::Current
    } else {
        HostBundleRegistrationStateV1::Repairable
    }
}

fn update_vibe_mcp(config_path: &Path, tracedecay_bin: Option<&str>) -> Result<()> {
    update_config_file_transactionally(config_path, |existing| {
        let mut document = if existing.trim().is_empty() {
            DocumentMut::new()
        } else {
            existing
                .parse::<DocumentMut>()
                .map_err(|error| TraceDecayError::Config {
                    message: format!("could not parse {}: {error}", config_path.display()),
                })?
        };
        let servers = document
            .entry("mcp_servers")
            .or_insert(Item::ArrayOfTables(ArrayOfTables::new()))
            .as_array_of_tables_mut()
            .ok_or_else(|| TraceDecayError::Config {
                message: format!("{}.mcp_servers must be an array of tables", config_path.display()),
            })?;
        let existing_index = servers.iter().position(|server| {
            server.get("name").and_then(Item::as_str) == Some("tracedecay")
        });
        match tracedecay_bin {
            Some(binary) => {
                let index = match existing_index {
                    Some(index) => index,
                    None => {
                        servers.push(Table::new());
                        servers.len() - 1
                    }
                };
                let server = servers.get_mut(index).ok_or_else(|| TraceDecayError::Config {
                    message: format!("could not edit {}", config_path.display()),
                })?;
                server.insert("name", value("tracedecay"));
                server.insert("transport", value("stdio"));
                server.insert("command", value(binary));
                let mut args = Array::new();
                args.push("serve");
                server.insert("args", value(args));
            }
            None => {
                if let Some(index) = existing_index {
                    servers.remove(index);
                } else {
                    return Ok(((), TextFileMutation::Unchanged));
                }
                if servers.is_empty() {
                    document.remove("mcp_servers");
                }
            }
        }
        let rendered = document.to_string();
        let mutation = if rendered.trim().is_empty() {
            TextFileMutation::Remove
        } else {
            TextFileMutation::Write(rendered)
        };
        Ok(((), mutation))
    })
}

fn install_vibe_agent(prompt_path: &Path, ctx: &InstallContext) -> Result<()> {
    let block = super::prompt_rules::standard_prompt_rules(
        PROMPT_RULE_MARKER,
        &PromptRulesOptions {
            extra_paragraphs: &[],
        },
    );
    super::prompt_rules::reconcile_prompt_rules(prompt_path, PROMPT_RULE_MARKER, &block)?;
    super::install_managed_skill_prompt_index(
        &ctx.home,
        prompt_path,
        tracedecay_automation_runtime::automation::skill_targets::SkillInstallTarget::Agents,
    )
}

fn uninstall_vibe_agent(prompt_path: &Path, ctx: &InstallContext) -> Result<()> {
    super::remove_managed_skill_prompt_index(
        &ctx.home,
        prompt_path,
        tracedecay_automation_runtime::automation::skill_targets::SkillInstallTarget::Agents,
    )?;
    super::prompt_rules::remove_standard_prompt_rules(prompt_path)
}

fn activate_vibe_components(
    components: &[HostBundleComponentV1],
    ctx: &InstallContext,
    config_path: &Path,
    prompt_path: &Path,
) -> Result<()> {
    if components.contains(&HostBundleComponentV1::ContextMcp) {
        update_vibe_mcp(config_path, Some(&ctx.tracedecay_bin))?;
    }
    if components.contains(&HostBundleComponentV1::Agent) {
        install_vibe_agent(prompt_path, ctx)?;
    }
    Ok(())
}

fn deactivate_vibe_components(
    components: &[HostBundleComponentV1],
    ctx: &InstallContext,
    config_path: &Path,
    prompt_path: &Path,
) -> Result<()> {
    if components.contains(&HostBundleComponentV1::Agent) {
        uninstall_vibe_agent(prompt_path, ctx)?;
    }
    if components.contains(&HostBundleComponentV1::ContextMcp) {
        update_vibe_mcp(config_path, None)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

fn doctor_check_config(dc: &mut DoctorCounters, config_path: &Path, install_selector: &str) {
    if !config_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent {install_selector}` if you use Mistral Vibe",
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

fn doctor_check_prompt(dc: &mut DoctorCounters, prompt_path: &Path, subject: &str) {
    super::doctor_check_prompt_contains_tracedecay(
        dc,
        prompt_path,
        subject,
        "vibe",
    );
}
