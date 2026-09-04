//! Zed agent integration.
//!
//! Handles registration of the tracedecay MCP server in Zed's `settings.json`
//! under the `context_servers.tracedecay` key.
//!
//! **Manual by necessity, not by preference (verified 2026-08-08).** The owner
//! policy is CLI-first, so this config write needs a justification. Zed ships
//! no non-interactive extension or context-server installation command at all:
//! that capability is an open feature request, not an implemented one, and
//! extensions are installed through the Command Palette and the Agent Panel.
//! There is nothing to drive, so the settings merge below is the only route.
//! Zed settings are JSONC, whose comments cannot survive a serde round-trip;
//! the byte-exact `.tracedecay-original` snapshot is therefore the peer-safety
//! authority and is restored when no later foreign edit prevents it.
//! See <https://github.com/zed-industries/zed/discussions/58417>.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::errors::{Result, TraceDecayError};

use super::host_bundle_v2::{HostBundleComponentV1, HostBundleRegistrationStateV1};
use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, JsonConfigDialect,
    McpDoctorLabels, TextFileMutation, config_backup_path, load_jsonc_file,
    report_mcp_registration, update_config_file_transactionally,
};

pub struct ZedIntegration;

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

    fn supports_local_install(&self) -> bool {
        true
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mZed integration\x1b[0m");
        doctor_check_registration(
            dc,
            &zed_settings_path(&ctx.home),
            "Zed user configuration",
            "Context server registered",
            "Context server NOT registered",
        );
        let project = zed_project_settings_path(&ctx.project_path);
        if project.exists() {
            doctor_check_registration(
                dc,
                &project,
                "Zed project configuration",
                "project context server registered",
                "project context server NOT registered",
            );
        }
    }

    fn host_component_registration(
        &self,
        component: HostBundleComponentV1,
        ctx: &HealthcheckContext,
    ) -> HostBundleRegistrationStateV1 {
        if component != HostBundleComponentV1::ContextMcp {
            return HostBundleRegistrationStateV1::Missing;
        }
        zed_mcp_registration_state(&zed_settings_path(&ctx.home), None)
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
        zed_mcp_registration_state(&zed_settings_path(&ctx.home), Some(&install.tracedecay_bin))
    }

    fn is_detected(&self, home: &Path) -> bool {
        zed_config_dir(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<PathBuf> {
        Some(zed_settings_path(home))
    }

    fn host_component_registration_paths(
        &self,
        components: &[HostBundleComponentV1],
        home: &Path,
    ) -> Vec<PathBuf> {
        if components != [HostBundleComponentV1::ContextMcp] {
            return Vec::new();
        }
        zed_registration_paths(&zed_settings_path(home))
    }

    fn project_host_component_registration_paths(
        &self,
        components: &[HostBundleComponentV1],
        _home: &Path,
        project_path: &Path,
    ) -> Result<Vec<PathBuf>> {
        if components != [HostBundleComponentV1::ContextMcp] {
            return Ok(Vec::new());
        }
        Ok(zed_registration_paths(&zed_project_settings_path(
            project_path,
        )))
    }

    #[hotpath::measure(label = "zed_mcp_install")]
    fn activate_deployed_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        install_mcp_if_selected(components, &zed_settings_path(&ctx.home), ctx)
    }

    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        uninstall_mcp_if_selected(components, &zed_settings_path(&ctx.home))
    }

    fn activate_project_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        let path = zed_project_settings_path(project_path);
        let original = zed_original_config_path(&path);
        super::ensure_project_local_safe_paths(project_path, [path.as_path(), original.as_path()])?;
        install_mcp_if_selected(components, &path, ctx)
    }

    fn deactivate_project_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        _ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        let path = zed_project_settings_path(project_path);
        let original = zed_original_config_path(&path);
        super::ensure_project_local_safe_paths(project_path, [path.as_path(), original.as_path()])?;
        uninstall_mcp_if_selected(components, &path)
    }

    fn reports_absence_to_doctor(&self) -> bool {
        true
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        super::mcp_config_has_tracedecay(
            &zed_settings_path(home),
            "context_servers",
            load_jsonc_file,
        )
    }
}

fn zed_settings_path(home: &Path) -> PathBuf {
    zed_config_dir(home).join("settings.json")
}

fn zed_project_settings_path(project: &Path) -> PathBuf {
    project.join(".zed/settings.json")
}

fn zed_original_config_path(config: &Path) -> PathBuf {
    PathBuf::from(format!("{}.tracedecay-original", config.display()))
}

fn zed_registration_paths(config: &Path) -> Vec<PathBuf> {
    vec![
        config.to_path_buf(),
        config_backup_path(config),
        zed_original_config_path(config),
    ]
}

fn zed_mcp_registration_state(
    config: &Path,
    expected_binary: Option<&str>,
) -> HostBundleRegistrationStateV1 {
    let Ok(existing) = std::fs::read_to_string(config) else {
        return HostBundleRegistrationStateV1::Missing;
    };
    let Ok(settings) = JsonConfigDialect::Jsonc.parse_for_edit(config, &existing) else {
        return HostBundleRegistrationStateV1::Corrupt;
    };
    let Some(server) = settings.pointer("/context_servers/tracedecay") else {
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

/// A host that is not installed is an informational finding, exactly as the
/// shared `doctor_check_mcp_registration` treats every other host: only a
/// present-but-foreign or unparsable registration is an issue. Grading an
/// absent Zed settings file as a failure made `tracedecay doctor` exit 1 on
/// every machine without Zed (the stock Hermes integration job included).
fn doctor_check_registration(
    dc: &mut DoctorCounters,
    config: &Path,
    product: &'static str,
    registered: &'static str,
    missing: &'static str,
) {
    if !config.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent zed` if you use {}",
            config.display(),
            product
        ));
        return;
    }
    report_mcp_registration(
        dc,
        config,
        zed_mcp_registration_state(config, None) == HostBundleRegistrationStateV1::Current,
        &McpDoctorLabels {
            agent_id: "zed",
            product,
            registered,
            missing,
        },
    );
}

fn install_mcp_if_selected(
    components: &[HostBundleComponentV1],
    config: &Path,
    ctx: &InstallContext,
) -> Result<()> {
    if !components.contains(&HostBundleComponentV1::ContextMcp) {
        return Ok(());
    }
    let original = zed_original_config_path(config);
    update_config_file_transactionally(config, |existing| {
        let mut settings = JsonConfigDialect::Jsonc.parse_for_edit(config, existing)?;
        let root = settings
            .as_object_mut()
            .ok_or_else(|| TraceDecayError::Config {
                message: format!("{} must contain a JSON object", config.display()),
            })?;
        let servers = root
            .entry("context_servers")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| TraceDecayError::Config {
                message: format!("{}.context_servers must be a JSON object", config.display()),
            })?;
        if !servers.contains_key("tracedecay") && config.is_file() && !original.exists() {
            super::safe_write_bytes_file(&original, existing.as_bytes(), None)?;
        }
        servers.insert(
            "tracedecay".to_string(),
            json!({
                "command": ctx.tracedecay_bin.clone(),
                "args": ["serve"],
            }),
        );
        Ok((
            (),
            TextFileMutation::Write(super::render_json_config(config, &settings)?),
        ))
    })?;
    Ok(())
}

#[derive(Clone, Copy)]
enum ZedMcpRemoval {
    NoEntry,
    RestoredOriginal,
    RemovedFile,
    Rewritten,
}

fn uninstall_mcp_if_selected(components: &[HostBundleComponentV1], config: &Path) -> Result<()> {
    if !components.contains(&HostBundleComponentV1::ContextMcp) || !config.exists() {
        return Ok(());
    }
    let original = zed_original_config_path(config);
    let outcome = update_config_file_transactionally(config, |existing| {
        let mut settings = JsonConfigDialect::Jsonc.parse_for_edit(config, existing)?;
        let Some(root) = settings.as_object_mut() else {
            return Err(TraceDecayError::Config {
                message: format!("{} must contain a JSON object", config.display()),
            });
        };
        let Some(servers) = root
            .get_mut("context_servers")
            .and_then(serde_json::Value::as_object_mut)
        else {
            return Ok((ZedMcpRemoval::NoEntry, TextFileMutation::Unchanged));
        };
        if servers.remove("tracedecay").is_none() {
            return Ok((ZedMcpRemoval::NoEntry, TextFileMutation::Unchanged));
        }
        if servers.is_empty() {
            root.remove("context_servers");
        }
        let root_is_empty = root.is_empty();
        if let Ok(bytes) = std::fs::read(&original)
            && serde_json::from_slice::<serde_json::Value>(
                super::strip_jsonc_comments(&String::from_utf8_lossy(&bytes)).as_bytes(),
            )
            .ok()
                == Some(settings.clone())
        {
            let bytes = String::from_utf8(bytes).map_err(|error| TraceDecayError::Config {
                message: format!("{} is not valid UTF-8: {error}", original.display()),
            })?;
            return Ok((
                ZedMcpRemoval::RestoredOriginal,
                TextFileMutation::Write(bytes),
            ));
        }
        if root_is_empty {
            return Ok((ZedMcpRemoval::RemovedFile, TextFileMutation::Remove));
        }
        Ok((
            ZedMcpRemoval::Rewritten,
            TextFileMutation::Write(super::render_json_config(config, &settings)?),
        ))
    })?;
    if matches!(outcome, ZedMcpRemoval::RestoredOriginal) {
        super::safe_remove_host_file(&original).map_err(|error| TraceDecayError::Config {
            message: format!("failed to remove {}: {error}", original.display()),
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::host_bundle_v2::{HostBundleComponentV1, HostBundleRegistrationStateV1};

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
    fn zed_lifecycle_preserves_jsonc_peers_and_restores_original_bytes() {
        let home = tempfile::tempdir().unwrap();
        let config = zed_settings_path(home.path());
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        let original = br#"{
  // operator comment cannot survive serde rendering
  "context_servers": {
    "foreign": {"command": "foreign-mcp"}
  },
  "theme": "dark"
}
"#;
        std::fs::write(&config, original).unwrap();
        let components = [HostBundleComponentV1::ContextMcp];
        let install = install_context(home.path(), "/tmp/tracedecay");

        ZedIntegration
            .activate_deployed_host_component_registration(&components, &install)
            .unwrap();

        let installed = load_jsonc_file(&config);
        assert_eq!(
            installed["context_servers"]["foreign"]["command"],
            "foreign-mcp"
        );
        assert_eq!(installed["theme"], "dark");
        assert_eq!(
            installed["context_servers"]["tracedecay"]["command"],
            "/tmp/tracedecay"
        );
        assert_eq!(
            zed_mcp_registration_state(&config, Some("/tmp/tracedecay")),
            HostBundleRegistrationStateV1::Current
        );
        assert_eq!(
            std::fs::read(zed_original_config_path(&config)).unwrap(),
            original
        );

        ZedIntegration
            .deactivate_deployed_host_component_registration(&components, &install)
            .unwrap();

        assert_eq!(std::fs::read(&config).unwrap(), original);
        assert!(!zed_original_config_path(&config).exists());
    }

    #[test]
    fn zed_uninstall_removes_a_config_created_by_tracedecay() {
        let home = tempfile::tempdir().unwrap();
        let config = zed_settings_path(home.path());
        let components = [HostBundleComponentV1::ContextMcp];
        let install = install_context(home.path(), "/tmp/tracedecay");

        ZedIntegration
            .activate_deployed_host_component_registration(&components, &install)
            .unwrap();
        assert!(config.is_file());

        ZedIntegration
            .deactivate_deployed_host_component_registration(&components, &install)
            .unwrap();

        assert!(!config.exists());
        assert!(!zed_original_config_path(&config).exists());
    }

    #[test]
    fn zed_readback_detects_foreign_modification_of_its_entry() {
        let home = tempfile::tempdir().unwrap();
        let config = zed_settings_path(home.path());
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            &config,
            r#"{"context_servers":{"tracedecay":{"command":"/tmp/foreign","args":["serve"]}}}"#,
        )
        .unwrap();

        assert_eq!(
            zed_mcp_registration_state(&config, Some("/tmp/tracedecay")),
            HostBundleRegistrationStateV1::Repairable
        );
    }

    #[test]
    fn zed_project_scope_uses_the_project_settings_document() {
        let project = tempfile::tempdir().unwrap();
        assert_eq!(
            zed_project_settings_path(project.path()),
            project.path().join(".zed/settings.json")
        );
    }
}
