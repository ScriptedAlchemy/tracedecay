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
//! `doctor` checks both locations and reports them separately.
//!
//! **Manual by necessity, not by preference (verified 2026-08-08).** The owner
//! policy is CLI-first, so these two config writes need a justification. The
//! `agy` CLI has a plugin/marketplace layer (`agy plugin list|install|disable`)
//! but no MCP command at all: Antigravity's own documentation directs users to
//! the interactive `/mcp` overlay or to editing `mcp_config.json` by hand, and
//! no `agy mcp add`/`remove` exists. The plugin commands cannot carry an MCP
//! server registration, so neither of the two files below has a command to
//! drive. See <https://antigravity.google/docs/mcp> and
//! <https://antigravity.google/docs/cli/plugins>.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::errors::{Result, TraceDecayError};

use super::host_bundle_v2::{HostBundleComponentV1, HostBundleRegistrationStateV1};
use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, JsonConfigDialect,
    McpDoctorLabels, TextFileMutation, config_backup_path, report_mcp_registration,
    update_two_config_files_transactionally,
};

pub struct AntigravityIntegration;

fn mcp_config_path(home: &Path) -> PathBuf {
    home.join(".gemini/antigravity/mcp_config.json")
}

/// Per-plugin file used by the Antigravity CLI. Holds the same shape as
/// the IDE config.
fn cli_plugin_path(home: &Path) -> PathBuf {
    home.join(".gemini/antigravity-cli/plugins/tracedecay.json")
}

fn original_config_path(config: &Path) -> PathBuf {
    PathBuf::from(format!("{}.tracedecay-original", config.display()))
}

impl AgentIntegration for AntigravityIntegration {
    fn name(&self) -> &'static str {
        "Antigravity"
    }

    fn id(&self) -> &'static str {
        "antigravity"
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mAntigravity integration\x1b[0m");
        doctor_check_registration(
            dc,
            &mcp_config_path(&ctx.home),
            "the Antigravity IDE",
            "IDE MCP server registered",
            "MCP server NOT registered",
        );
        doctor_check_registration(
            dc,
            &cli_plugin_path(&ctx.home),
            "the Antigravity CLI (#85)",
            "CLI plugin registered",
            "CLI plugin file exists but lacks `mcpServers.tracedecay`",
        );
    }

    fn host_component_registration(
        &self,
        component: HostBundleComponentV1,
        ctx: &HealthcheckContext,
    ) -> HostBundleRegistrationStateV1 {
        if component != HostBundleComponentV1::ContextMcp {
            return HostBundleRegistrationStateV1::Missing;
        }
        antigravity_registration_state(&ctx.home, None)
    }

    fn host_component_registration_for_lifecycle(
        &self,
        component: HostBundleComponentV1,
        ctx: &HealthcheckContext,
        install: &InstallContext,
    ) -> HostBundleRegistrationStateV1 {
        if component != HostBundleComponentV1::ContextMcp {
            return HostBundleRegistrationStateV1::Missing;
        }
        antigravity_registration_state(&ctx.home, Some(&install.tracedecay_bin))
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".gemini/antigravity").is_dir() || home.join(".gemini/antigravity-cli").is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<PathBuf> {
        Some(mcp_config_path(home))
    }

    fn host_component_registration_paths(
        &self,
        components: &[HostBundleComponentV1],
        home: &Path,
    ) -> Vec<PathBuf> {
        if components != [HostBundleComponentV1::ContextMcp] {
            return Vec::new();
        }
        registration_paths(home)
    }

    #[hotpath::measure(label = "antigravity_mcp_install")]
    fn activate_deployed_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        install_mcp_if_selected(components, ctx)
    }

    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        uninstall_mcp_if_selected(components, &ctx.home)
    }

    fn reports_absence_to_doctor(&self) -> bool {
        true
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        antigravity_registration_state(home, None) == HostBundleRegistrationStateV1::Current
    }
}

fn registration_paths(home: &Path) -> Vec<PathBuf> {
    let ide = mcp_config_path(home);
    let cli = cli_plugin_path(home);
    vec![
        ide.clone(),
        config_backup_path(&ide),
        original_config_path(&ide),
        cli.clone(),
        config_backup_path(&cli),
        original_config_path(&cli),
    ]
}

fn document_registration_state(
    config: &Path,
    expected_binary: Option<&str>,
) -> HostBundleRegistrationStateV1 {
    let Ok(bytes) = std::fs::read(config) else {
        return HostBundleRegistrationStateV1::Missing;
    };
    let Ok(settings) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return HostBundleRegistrationStateV1::Corrupt;
    };
    let Some(server) = settings.pointer("/mcpServers/tracedecay") else {
        return HostBundleRegistrationStateV1::Missing;
    };
    let command_matches = server
        .get("command")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|command| {
            expected_binary.map_or_else(|| !command.is_empty(), |expected| command == expected)
        });
    let serves_tracedecay = server
        .get("args")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|args| args.len() == 1 && args[0].as_str() == Some("serve"));
    if server.is_object() && command_matches && serves_tracedecay {
        HostBundleRegistrationStateV1::Current
    } else {
        HostBundleRegistrationStateV1::Repairable
    }
}

fn antigravity_registration_state(
    home: &Path,
    expected_binary: Option<&str>,
) -> HostBundleRegistrationStateV1 {
    let ide = document_registration_state(&mcp_config_path(home), expected_binary);
    let cli = document_registration_state(&cli_plugin_path(home), expected_binary);
    match (ide, cli) {
        (HostBundleRegistrationStateV1::Current, HostBundleRegistrationStateV1::Current) => {
            HostBundleRegistrationStateV1::Current
        }
        (HostBundleRegistrationStateV1::Missing, HostBundleRegistrationStateV1::Missing) => {
            HostBundleRegistrationStateV1::Missing
        }
        (HostBundleRegistrationStateV1::Corrupt, _)
        | (_, HostBundleRegistrationStateV1::Corrupt) => HostBundleRegistrationStateV1::Corrupt,
        (HostBundleRegistrationStateV1::Repairable, _)
        | (_, HostBundleRegistrationStateV1::Repairable)
        | (HostBundleRegistrationStateV1::Current, HostBundleRegistrationStateV1::Missing)
        | (HostBundleRegistrationStateV1::Missing, HostBundleRegistrationStateV1::Current) => {
            HostBundleRegistrationStateV1::Repairable
        }
    }
}

fn doctor_check_registration(
    dc: &mut DoctorCounters,
    config: &Path,
    product: &'static str,
    registered: &'static str,
    missing: &'static str,
) {
    report_mcp_registration(
        dc,
        config,
        document_registration_state(config, None) == HostBundleRegistrationStateV1::Current,
        &McpDoctorLabels {
            agent_id: "antigravity",
            product,
            registered,
            missing,
        },
    );
}

fn parse_document(config: &Path, existing: &str) -> Result<serde_json::Value> {
    let settings = JsonConfigDialect::Json.parse_for_edit(config, existing)?;
    if !settings.is_object() {
        return Err(TraceDecayError::Config {
            message: format!("{} must contain a JSON object", config.display()),
        });
    }
    if settings
        .get("mcpServers")
        .is_some_and(|value| !value.is_object())
    {
        return Err(TraceDecayError::Config {
            message: format!("{}.mcpServers must be a JSON object", config.display()),
        });
    }
    Ok(settings)
}

fn add_registration(config: &Path, existing: &str, binary: &str) -> Result<TextFileMutation> {
    let mut settings = parse_document(config, existing)?;
    settings["mcpServers"]["tracedecay"] = json!({
        "command": binary,
        "args": ["serve"],
        "env": {},
        "transport": "stdio",
    });
    Ok(TextFileMutation::Write(super::render_json_config(
        config, &settings,
    )?))
}

fn save_original_if_needed(config: &Path, existing: &str) -> Result<()> {
    let original = original_config_path(config);
    let settings = parse_document(config, existing)?;
    if settings.pointer("/mcpServers/tracedecay").is_none()
        && config.is_file()
        && !original.exists()
    {
        super::safe_write_bytes_file(&original, existing.as_bytes(), None)?;
    }
    Ok(())
}

fn install_mcp_if_selected(
    components: &[HostBundleComponentV1],
    ctx: &InstallContext,
) -> Result<()> {
    if !components.contains(&HostBundleComponentV1::ContextMcp) {
        return Ok(());
    }
    let ide = mcp_config_path(&ctx.home);
    let cli = cli_plugin_path(&ctx.home);
    let ide_original = original_config_path(&ide);
    let cli_original = original_config_path(&cli);
    let ide_original_existed = ide_original.exists();
    let cli_original_existed = cli_original.exists();
    let result =
        update_two_config_files_transactionally(&ide, &cli, |ide_existing, cli_existing| {
            save_original_if_needed(&ide, ide_existing)?;
            save_original_if_needed(&cli, cli_existing)?;
            Ok((
                (),
                add_registration(&ide, ide_existing, &ctx.tracedecay_bin)?,
                add_registration(&cli, cli_existing, &ctx.tracedecay_bin)?,
            ))
        });
    if result.is_err() {
        remove_new_original(&ide_original, ide_original_existed)?;
        remove_new_original(&cli_original, cli_original_existed)?;
    }
    result
}

fn remove_new_original(path: &Path, existed_before: bool) -> Result<()> {
    if !existed_before && path.exists() {
        super::safe_remove_host_file(path).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to remove {} after rollback: {error}",
                path.display()
            ),
        })?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum DocumentRemoval {
    NoEntry,
    RestoredOriginal,
    RemovedFile,
    Rewritten,
}

fn remove_registration(
    config: &Path,
    existing: &str,
) -> Result<(DocumentRemoval, TextFileMutation)> {
    let mut settings = parse_document(config, existing)?;
    let Some(root) = settings.as_object_mut() else {
        return Err(TraceDecayError::Config {
            message: format!("{} must contain a JSON object", config.display()),
        });
    };
    let Some(servers) = root
        .get_mut("mcpServers")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok((DocumentRemoval::NoEntry, TextFileMutation::Unchanged));
    };
    if servers.remove("tracedecay").is_none() {
        return Ok((DocumentRemoval::NoEntry, TextFileMutation::Unchanged));
    }
    if servers.is_empty() {
        root.remove("mcpServers");
    }
    let root_is_empty = root.is_empty();
    let original = original_config_path(config);
    if let Ok(bytes) = std::fs::read(&original)
        && serde_json::from_slice::<serde_json::Value>(&bytes).ok() == Some(settings.clone())
    {
        let bytes = String::from_utf8(bytes).map_err(|error| TraceDecayError::Config {
            message: format!("{} is not valid UTF-8: {error}", original.display()),
        })?;
        return Ok((
            DocumentRemoval::RestoredOriginal,
            TextFileMutation::Write(bytes),
        ));
    }
    if root_is_empty {
        return Ok((DocumentRemoval::RemovedFile, TextFileMutation::Remove));
    }
    Ok((
        DocumentRemoval::Rewritten,
        TextFileMutation::Write(super::render_json_config(config, &settings)?),
    ))
}

fn uninstall_mcp_if_selected(components: &[HostBundleComponentV1], home: &Path) -> Result<()> {
    if !components.contains(&HostBundleComponentV1::ContextMcp) {
        return Ok(());
    }
    let ide = mcp_config_path(home);
    let cli = cli_plugin_path(home);
    let (ide_outcome, cli_outcome) =
        update_two_config_files_transactionally(&ide, &cli, |ide_existing, cli_existing| {
            let (ide_outcome, ide_mutation) = remove_registration(&ide, ide_existing)?;
            let (cli_outcome, cli_mutation) = remove_registration(&cli, cli_existing)?;
            Ok(((ide_outcome, cli_outcome), ide_mutation, cli_mutation))
        })?;
    remove_restored_original(&ide, ide_outcome)?;
    remove_restored_original(&cli, cli_outcome)
}

fn remove_restored_original(config: &Path, outcome: DocumentRemoval) -> Result<()> {
    if matches!(outcome, DocumentRemoval::RestoredOriginal) {
        let original = original_config_path(config);
        super::safe_remove_host_file(&original).map_err(|error| TraceDecayError::Config {
            message: format!("failed to remove {}: {error}", original.display()),
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

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

    fn write_parent(path: &Path, contents: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn antigravity_lifecycle_preserves_peers_and_restores_both_documents() {
        let home = tempfile::tempdir().unwrap();
        let ide = mcp_config_path(home.path());
        let cli = cli_plugin_path(home.path());
        let ide_original = br#"{"theme":"dark","mcpServers":{"foreign":{"command":"peer"}}}"#;
        let cli_original = br#"{"enabled":true,"mcpServers":{"other":{"command":"peer"}}}"#;
        write_parent(&ide, ide_original);
        write_parent(&cli, cli_original);
        let components = [HostBundleComponentV1::ContextMcp];
        let install = install_context(home.path(), "/tmp/tracedecay");

        AntigravityIntegration
            .activate_deployed_host_component_registration(&components, &install)
            .unwrap();

        assert_eq!(
            antigravity_registration_state(home.path(), Some("/tmp/tracedecay")),
            HostBundleRegistrationStateV1::Current
        );
        assert_eq!(
            super::super::load_json_file(&ide)["mcpServers"]["foreign"]["command"],
            "peer"
        );
        assert_eq!(
            super::super::load_json_file(&cli)["mcpServers"]["other"]["command"],
            "peer"
        );

        AntigravityIntegration
            .deactivate_deployed_host_component_registration(&components, &install)
            .unwrap();

        assert_eq!(std::fs::read(&ide).unwrap(), ide_original);
        assert_eq!(std::fs::read(&cli).unwrap(), cli_original);
    }

    #[test]
    fn antigravity_uninstall_removes_both_documents_created_by_tracedecay() {
        let home = tempfile::tempdir().unwrap();
        let components = [HostBundleComponentV1::ContextMcp];
        let install = install_context(home.path(), "/tmp/tracedecay");

        AntigravityIntegration
            .activate_deployed_host_component_registration(&components, &install)
            .unwrap();
        AntigravityIntegration
            .deactivate_deployed_host_component_registration(&components, &install)
            .unwrap();

        assert!(!mcp_config_path(home.path()).exists());
        assert!(!cli_plugin_path(home.path()).exists());
    }

    #[test]
    fn antigravity_partial_and_foreign_registration_is_repairable() {
        let home = tempfile::tempdir().unwrap();
        let ide = mcp_config_path(home.path());
        write_parent(
            &ide,
            br#"{"mcpServers":{"tracedecay":{"command":"/tmp/foreign","args":["serve"]}}}"#,
        );

        assert_eq!(
            antigravity_registration_state(home.path(), Some("/tmp/tracedecay")),
            HostBundleRegistrationStateV1::Repairable
        );
    }

    #[test]
    fn antigravity_second_document_failure_rolls_first_back_exactly() {
        let home = tempfile::tempdir().unwrap();
        let ide = mcp_config_path(home.path());
        let cli = cli_plugin_path(home.path());
        let ide_original = br#"{"theme":"dark"}"#;
        let cli_original = br#"{"enabled":true}"#;
        write_parent(&ide, ide_original);
        write_parent(&cli, cli_original);
        let pause = super::super::pause_next_host_config_write_after_validation(&cli);
        let install = Arc::new(install_context(home.path(), "/tmp/tracedecay"));
        let worker_install = Arc::clone(&install);
        let worker = std::thread::spawn(move || {
            AntigravityIntegration.activate_deployed_host_component_registration(
                &[HostBundleComponentV1::ContextMcp],
                &worker_install,
            )
        });

        pause.wait_until_reached();
        std::fs::write(&cli, br#"{"foreign":"concurrent"}"#).unwrap();
        pause.resume();
        let error = worker.join().unwrap().unwrap_err();

        assert!(error.to_string().contains("failed to atomically replace"));
        assert_eq!(std::fs::read(&ide).unwrap(), ide_original);
        assert_eq!(std::fs::read(&cli).unwrap(), br#"{"foreign":"concurrent"}"#);
    }
}
