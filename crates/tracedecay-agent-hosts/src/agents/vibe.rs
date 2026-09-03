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
use tracedecay_automation_runtime::automation::skill_targets::{
    SkillInstallSummary, SkillInstallTarget, install_managed_skills,
};

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
fn vibe_home(home: &Path) -> PathBuf {
    super::host_home_override(home, "VIBE_HOME", ".vibe")
}

fn vibe_config_path(home: &Path) -> PathBuf {
    vibe_home(home).join("config.toml")
}

fn vibe_prompt_path(home: &Path) -> PathBuf {
    vibe_home(home).join("prompts/cli.md")
}

fn project_vibe_home(project: &Path) -> PathBuf {
    project.join(".vibe")
}

fn original_config_path(config: &Path) -> PathBuf {
    PathBuf::from(format!("{}.tracedecay-original", config.display()))
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
        doctor_check_registration(
            dc,
            &vibe_config_path(&ctx.home),
            &vibe_prompt_path(&ctx.home),
        );
        let project_home = project_vibe_home(&ctx.project_path);
        if project_home.exists() {
            doctor_check_registration(
                dc,
                &project_home.join("config.toml"),
                &project_home.join("prompts/cli.md"),
            );
        }
    }

    fn host_component_registration(
        &self,
        component: HostBundleComponentV1,
        ctx: &HealthcheckContext,
    ) -> HostBundleRegistrationStateV1 {
        component_registration_state(
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
        component_registration_state(
            component,
            &vibe_config_path(&ctx.home),
            &vibe_prompt_path(&ctx.home),
            Some(&install.tracedecay_bin),
        )
    }

    fn is_detected(&self, home: &Path) -> bool {
        vibe_home(home).is_dir()
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        mcp_registration_state(&vibe_config_path(home), None)
            == HostBundleRegistrationStateV1::Current
            || prompt_registration_state(&vibe_prompt_path(home))
                == HostBundleRegistrationStateV1::Current
    }

    fn primary_config_path(&self, home: &Path) -> Option<PathBuf> {
        Some(vibe_config_path(home))
    }

    fn host_component_registration_paths(
        &self,
        components: &[HostBundleComponentV1],
        home: &Path,
    ) -> Vec<PathBuf> {
        registration_paths(components, &vibe_config_path(home), &vibe_prompt_path(home))
    }

    fn project_host_component_registration_paths(
        &self,
        components: &[HostBundleComponentV1],
        _home: &Path,
        project_path: &Path,
    ) -> Result<Vec<PathBuf>> {
        let root = project_vibe_home(project_path);
        Ok(registration_paths(
            components,
            &root.join("config.toml"),
            &root.join("prompts/cli.md"),
        ))
    }

    #[hotpath::measure(label = "vibe_component_install")]
    fn activate_deployed_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        activate_components(
            components,
            &vibe_config_path(&ctx.home),
            &vibe_prompt_path(&ctx.home),
            ctx,
        )
    }

    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        deactivate_components(
            components,
            &vibe_config_path(&ctx.home),
            &vibe_prompt_path(&ctx.home),
            &ctx.home,
        )
    }

    fn activate_project_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        let root = project_vibe_home(project_path);
        let config = root.join("config.toml");
        let prompt = root.join("prompts/cli.md");
        let original = original_config_path(&config);
        super::ensure_project_local_safe_paths(
            project_path,
            [config.as_path(), prompt.as_path(), original.as_path()],
        )?;
        activate_components(components, &config, &prompt, ctx)
    }

    fn deactivate_project_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        let root = project_vibe_home(project_path);
        let config = root.join("config.toml");
        let prompt = root.join("prompts/cli.md");
        let original = original_config_path(&config);
        super::ensure_project_local_safe_paths(
            project_path,
            [config.as_path(), prompt.as_path(), original.as_path()],
        )?;
        deactivate_components(components, &config, &prompt, &ctx.home)
    }

    fn reports_absence_to_doctor(&self) -> bool {
        true
    }

    fn export_managed_skills(
        &self,
        home: &Path,
        profile_root: &Path,
    ) -> Result<Vec<SkillInstallSummary>> {
        let prompt_path = vibe_prompt_path(home);
        if !prompt_path.exists() {
            return Ok(Vec::new());
        }
        Ok(vec![install_managed_skills(
            profile_root,
            SkillInstallTarget::Agents,
            &prompt_path,
        )?])
    }

    fn export_managed_skills_local(
        &self,
        project_root: &Path,
        profile_root: &Path,
    ) -> Result<Vec<SkillInstallSummary>> {
        let prompt_path = project_root.join(".vibe/prompts/cli.md");
        if !prompt_path.exists() {
            return Ok(Vec::new());
        }
        Ok(vec![install_managed_skills(
            profile_root,
            SkillInstallTarget::Agents,
            &prompt_path,
        )?])
    }
}

fn registration_paths(
    components: &[HostBundleComponentV1],
    config: &Path,
    prompt: &Path,
) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if components.contains(&HostBundleComponentV1::ContextMcp) {
        paths.push(config.to_path_buf());
        paths.push(config_backup_path(config));
        paths.push(original_config_path(config));
    }
    if components.contains(&HostBundleComponentV1::Core) {
        paths.push(prompt.to_path_buf());
    }
    paths
}

fn component_registration_state(
    component: HostBundleComponentV1,
    config: &Path,
    prompt: &Path,
    expected_binary: Option<&str>,
) -> HostBundleRegistrationStateV1 {
    match component {
        HostBundleComponentV1::ContextMcp => mcp_registration_state(config, expected_binary),
        HostBundleComponentV1::Core => prompt_registration_state(prompt),
        HostBundleComponentV1::Agent | HostBundleComponentV1::OperatorMcp => {
            HostBundleRegistrationStateV1::Missing
        }
    }
}

fn parse_document(config: &Path, existing: &str) -> Result<DocumentMut> {
    existing
        .parse::<DocumentMut>()
        .map_err(|error| TraceDecayError::Config {
            message: format!("failed to parse {}: {error}", config.display()),
        })
}

fn tracedecay_server(document: &DocumentMut) -> Option<&Table> {
    document
        .get("mcp_servers")
        .and_then(Item::as_array_of_tables)
        .and_then(|servers| {
            servers
                .iter()
                .find(|server| server.get("name").and_then(Item::as_str) == Some("tracedecay"))
        })
}

fn mcp_registration_state(
    config: &Path,
    expected_binary: Option<&str>,
) -> HostBundleRegistrationStateV1 {
    let Ok(existing) = std::fs::read_to_string(config) else {
        return HostBundleRegistrationStateV1::Missing;
    };
    let Ok(document) = parse_document(config, &existing) else {
        return HostBundleRegistrationStateV1::Corrupt;
    };
    let Some(server) = tracedecay_server(&document) else {
        return HostBundleRegistrationStateV1::Missing;
    };
    let command_matches = server
        .get("command")
        .and_then(Item::as_str)
        .is_some_and(|command| {
            expected_binary.map_or_else(|| !command.is_empty(), |expected| command == expected)
        });
    let transport_matches = server.get("transport").and_then(Item::as_str) == Some("stdio");
    let args_match = server
        .get("args")
        .and_then(Item::as_array)
        .is_some_and(|args| {
            args.len() == 1 && args.get(0).and_then(|arg| arg.as_str()) == Some("serve")
        });
    if command_matches && transport_matches && args_match {
        HostBundleRegistrationStateV1::Current
    } else {
        HostBundleRegistrationStateV1::Repairable
    }
}

fn prompt_registration_state(prompt: &Path) -> HostBundleRegistrationStateV1 {
    match std::fs::read_to_string(prompt) {
        Ok(contents) if contents.contains(PROMPT_RULE_MARKER) => {
            HostBundleRegistrationStateV1::Current
        }
        // The operator's own prompt text without the managed block is an
        // unregistered document, not a damaged registration: uninstall keeps
        // foreign content in place and must verify as `Missing`.
        Ok(_) => HostBundleRegistrationStateV1::Missing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            HostBundleRegistrationStateV1::Missing
        }
        Err(_) => HostBundleRegistrationStateV1::Corrupt,
    }
}

fn doctor_check_registration(dc: &mut DoctorCounters, config: &Path, prompt: &Path) {
    match mcp_registration_state(config, None) {
        HostBundleRegistrationStateV1::Current => {
            dc.pass(&format!("MCP server registered in {}", config.display()))
        }
        HostBundleRegistrationStateV1::Missing => dc.warn(&format!(
            "{} has no tracedecay MCP server — run `tracedecay install --agent vibe`",
            config.display()
        )),
        HostBundleRegistrationStateV1::Repairable => dc.fail(&format!(
            "MCP server in {} is foreign-modified — run `tracedecay repair --agent vibe`",
            config.display()
        )),
        HostBundleRegistrationStateV1::Corrupt => {
            dc.fail(&format!("could not parse {}", config.display()))
        }
    }
    super::doctor_check_prompt_contains_tracedecay(dc, prompt, "Vibe prompt", "vibe");
}

fn install_mcp(config: &Path, binary: &str) -> Result<()> {
    let original = original_config_path(config);
    update_config_file_transactionally(config, |existing| {
        let mut document = parse_document(config, existing)?;
        let had_registration = tracedecay_server(&document).is_some();
        if !had_registration && config.is_file() && !original.exists() {
            super::safe_write_bytes_file(&original, existing.as_bytes(), None)?;
        }
        let servers = document
            .entry("mcp_servers")
            .or_insert(Item::ArrayOfTables(ArrayOfTables::new()))
            .as_array_of_tables_mut()
            .ok_or_else(|| TraceDecayError::Config {
                message: format!(
                    "{}.mcp_servers must be an array of tables",
                    config.display()
                ),
            })?;
        // The array-of-tables iterator is a boxed borrow; bind the position
        // first so the borrow ends before the mutable remove.
        let existing_index = servers
            .iter()
            .position(|server| server.get("name").and_then(Item::as_str) == Some("tracedecay"));
        if let Some(index) = existing_index {
            servers.remove(index);
        }
        let mut server = Table::new();
        server["name"] = value("tracedecay");
        server["transport"] = value("stdio");
        server["command"] = value(binary);
        let mut args = Array::new();
        args.push("serve");
        server["args"] = value(args);
        servers.push(server);
        Ok(((), TextFileMutation::Write(document.to_string())))
    })
}

#[derive(Clone, Copy)]
enum McpRemoval {
    NoEntry,
    RestoredOriginal,
    RemovedFile,
    Rewritten,
}

fn uninstall_mcp(config: &Path) -> Result<()> {
    if !config.exists() {
        return Ok(());
    }
    let original = original_config_path(config);
    let outcome = update_config_file_transactionally(config, |existing| {
        let mut document = parse_document(config, existing)?;
        let Some(servers) = document
            .get_mut("mcp_servers")
            .and_then(Item::as_array_of_tables_mut)
        else {
            return Ok((McpRemoval::NoEntry, TextFileMutation::Unchanged));
        };
        let Some(index) = servers
            .iter()
            .position(|server| server.get("name").and_then(Item::as_str) == Some("tracedecay"))
        else {
            return Ok((McpRemoval::NoEntry, TextFileMutation::Unchanged));
        };
        servers.remove(index);
        if servers.is_empty() {
            document.remove("mcp_servers");
        }
        if let Ok(bytes) = std::fs::read(&original)
            && toml::from_slice::<toml::Value>(&bytes).ok()
                == toml::from_str::<toml::Value>(&document.to_string()).ok()
        {
            let original = String::from_utf8(bytes).map_err(|error| TraceDecayError::Config {
                message: format!("{} is not valid UTF-8: {error}", original.display()),
            })?;
            return Ok((
                McpRemoval::RestoredOriginal,
                TextFileMutation::Write(original),
            ));
        }
        if document.is_empty() {
            return Ok((McpRemoval::RemovedFile, TextFileMutation::Remove));
        }
        Ok((
            McpRemoval::Rewritten,
            TextFileMutation::Write(document.to_string()),
        ))
    })?;
    if matches!(outcome, McpRemoval::RestoredOriginal) {
        super::safe_remove_host_file(&original).map_err(|error| TraceDecayError::Config {
            message: format!("failed to remove {}: {error}", original.display()),
        })?;
    }
    Ok(())
}

fn install_prompt(prompt: &Path, profile_home: &Path) -> Result<()> {
    let block = super::prompt_rules::standard_prompt_rules(
        PROMPT_RULE_MARKER,
        &PromptRulesOptions {
            extra_paragraphs: &[],
        },
    );
    super::prompt_rules::reconcile_prompt_rules(prompt, PROMPT_RULE_MARKER, &block)?;
    super::install_managed_skill_prompt_index(profile_home, prompt, SkillInstallTarget::Agents)
}

fn uninstall_prompt(prompt: &Path, profile_home: &Path) -> Result<()> {
    super::remove_managed_skill_prompt_index(profile_home, prompt, SkillInstallTarget::Agents)?;
    super::prompt_rules::remove_standard_prompt_rules(prompt)
}

fn activate_components(
    components: &[HostBundleComponentV1],
    config: &Path,
    prompt: &Path,
    ctx: &InstallContext,
) -> Result<()> {
    if components.contains(&HostBundleComponentV1::ContextMcp) {
        install_mcp(config, &ctx.tracedecay_bin)?;
    }
    if components.contains(&HostBundleComponentV1::Core) {
        install_prompt(prompt, &ctx.home)?;
    }
    Ok(())
}

fn deactivate_components(
    components: &[HostBundleComponentV1],
    config: &Path,
    prompt: &Path,
    profile_home: &Path,
) -> Result<()> {
    if components.contains(&HostBundleComponentV1::Core) {
        uninstall_prompt(prompt, profile_home)?;
    }
    if components.contains(&HostBundleComponentV1::ContextMcp) {
        uninstall_mcp(config)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install_context(home: &Path, binary: &str) -> InstallContext {
        InstallContext {
            home: home.to_path_buf(),
            tracedecay_bin: binary.to_string(),
            tool_permissions: Vec::new(),
            project_root: None,
            dashboard: false,
        }
    }

    #[test]
    fn vibe_mcp_lifecycle_preserves_foreign_servers_and_restores_original_bytes() {
        let home = tempfile::tempdir().unwrap();
        let config = vibe_config_path(home.path());
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        let original = b"# operator comment\n\
[[mcp_servers]]\nname = \"one\"\ncommand = \"one-bin\"\n\n\
[[mcp_servers]]\nname = \"two\"\ncommand = \"two-bin\"\n";
        std::fs::write(&config, original).unwrap();
        let install = install_context(home.path(), "/tmp/tracedecay");

        VibeIntegration
            .activate_deployed_host_component_registration(
                &[HostBundleComponentV1::ContextMcp],
                &install,
            )
            .unwrap();

        let installed = std::fs::read_to_string(&config).unwrap();
        assert!(installed.contains("name = \"one\"\ncommand = \"one-bin\""));
        assert!(installed.contains("name = \"two\"\ncommand = \"two-bin\""));
        assert_eq!(
            mcp_registration_state(&config, Some("/tmp/tracedecay")),
            HostBundleRegistrationStateV1::Current
        );

        VibeIntegration
            .deactivate_deployed_host_component_registration(
                &[HostBundleComponentV1::ContextMcp],
                &install,
            )
            .unwrap();

        assert_eq!(std::fs::read(&config).unwrap(), original);
    }

    #[test]
    fn vibe_context_mcp_and_core_are_independently_installable() {
        let home = tempfile::tempdir().unwrap();
        let config = vibe_config_path(home.path());
        let prompt = vibe_prompt_path(home.path());
        let install = install_context(home.path(), "/tmp/tracedecay");

        activate_components(&[HostBundleComponentV1::Core], &config, &prompt, &install).unwrap();
        assert_eq!(
            prompt_registration_state(&prompt),
            HostBundleRegistrationStateV1::Current
        );
        assert!(!config.exists());

        deactivate_components(
            &[HostBundleComponentV1::Core],
            &config,
            &prompt,
            home.path(),
        )
        .unwrap();
        activate_components(
            &[HostBundleComponentV1::ContextMcp],
            &config,
            &prompt,
            &install,
        )
        .unwrap();
        assert_eq!(
            mcp_registration_state(&config, Some("/tmp/tracedecay")),
            HostBundleRegistrationStateV1::Current
        );
        assert!(!prompt.exists());
    }

    #[test]
    fn vibe_uninstall_removes_files_created_by_tracedecay() {
        let home = tempfile::tempdir().unwrap();
        let config = vibe_config_path(home.path());
        let prompt = vibe_prompt_path(home.path());
        let install = install_context(home.path(), "/tmp/tracedecay");
        let components = [
            HostBundleComponentV1::ContextMcp,
            HostBundleComponentV1::Core,
        ];

        activate_components(&components, &config, &prompt, &install).unwrap();
        deactivate_components(&components, &config, &prompt, home.path()).unwrap();

        assert!(!config.exists());
        assert!(!prompt.exists());
    }

    #[test]
    fn vibe_readback_detects_foreign_modification() {
        let home = tempfile::tempdir().unwrap();
        let config = vibe_config_path(home.path());
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            &config,
            "[[mcp_servers]]\nname = \"tracedecay\"\ntransport = \"stdio\"\ncommand = \"/tmp/foreign\"\nargs = [\"serve\"]\n",
        )
        .unwrap();

        assert_eq!(
            mcp_registration_state(&config, Some("/tmp/tracedecay")),
            HostBundleRegistrationStateV1::Repairable
        );
    }

    #[test]
    fn vibe_project_scope_uses_project_local_documents() {
        let project = tempfile::tempdir().unwrap();
        let root = project_vibe_home(project.path());
        assert_eq!(
            root.join("config.toml"),
            project.path().join(".vibe/config.toml")
        );
        assert_eq!(
            root.join("prompts/cli.md"),
            project.path().join(".vibe/prompts/cli.md")
        );
    }
}
