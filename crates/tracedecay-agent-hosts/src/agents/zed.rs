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
//! Zed settings are JSONC, whose comments cannot survive a serde round-trip.
//! See <https://github.com/zed-industries/zed/discussions/58417>.

use std::path::{Path, PathBuf};
use tracedecay_runtime_core::config::ProfileRoot;

use serde_json::json;

use tracedecay_domain::errors::{Result, TraceDecayError};

use super::host_bundle::{HostBundleRegistrationStateV1, HostComponentV1};
use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, JsonConfigDialect,
    McpDoctorLabels, TextFileMutation, load_jsonc_file, report_mcp_registration,
    update_text_file_transactionally,
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
        component: HostComponentV1,
        ctx: &HealthcheckContext,
    ) -> HostBundleRegistrationStateV1 {
        if component != HostComponentV1::ContextMcp {
            return HostBundleRegistrationStateV1::Missing;
        }
        zed_mcp_registration_state(&zed_settings_path(&ctx.home), None)
    }

    fn host_component_registration_for_lifecycle(
        &self,
        component: HostComponentV1,
        ctx: &HealthcheckContext,
        install: &InstallContext,
    ) -> HostBundleRegistrationStateV1 {
        if component != HostComponentV1::ContextMcp {
            return HostBundleRegistrationStateV1::Missing;
        }
        zed_mcp_registration_state(&zed_settings_path(&ctx.home), Some(&install.tracedecay_bin))
    }

    fn is_detected(&self, home: &Path) -> bool {
        zed_config_dir(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path, _profile: &ProfileRoot) -> Option<PathBuf> {
        Some(zed_settings_path(home))
    }

    fn host_component_registration_paths(
        &self,
        components: &[HostComponentV1],
        home: &Path,
        _profile: &ProfileRoot,
    ) -> Vec<PathBuf> {
        if components != [HostComponentV1::ContextMcp] {
            return Vec::new();
        }
        vec![zed_settings_path(home)]
    }

    fn project_host_component_registration_paths(
        &self,
        components: &[HostComponentV1],
        _home: &Path,
        _profile_root: &Path,
        project_path: &Path,
    ) -> Result<Vec<PathBuf>> {
        if components != [HostComponentV1::ContextMcp] {
            return Ok(Vec::new());
        }
        Ok(vec![zed_project_settings_path(project_path)])
    }

    #[hotpath::measure(label = "zed_mcp_install")]
    fn activate_deployed_host_component_registration(
        &self,
        components: &[HostComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        install_mcp_if_selected(components, &zed_settings_path(&ctx.home), ctx)
    }

    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[HostComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        uninstall_mcp_if_selected(components, &zed_settings_path(&ctx.home))
    }

    fn activate_project_host_component_registration(
        &self,
        components: &[HostComponentV1],
        ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        let path = zed_project_settings_path(project_path);
        super::ensure_project_local_safe_path(project_path, &path)?;
        install_mcp_if_selected(components, &path, ctx)
    }

    fn deactivate_project_host_component_registration(
        &self,
        components: &[HostComponentV1],
        _ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        let path = zed_project_settings_path(project_path);
        super::ensure_project_local_safe_path(project_path, &path)?;
        uninstall_mcp_if_selected(components, &path)
    }

    fn reports_absence_to_doctor(&self) -> bool {
        true
    }

    fn has_tracedecay(&self, home: &Path, _profile: &ProfileRoot) -> bool {
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
            "{} not found, run `tracedecay install --agent zed` if you use {}",
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
    components: &[HostComponentV1],
    config: &Path,
    ctx: &InstallContext,
) -> Result<()> {
    if !components.contains(&HostComponentV1::ContextMcp) {
        return Ok(());
    }
    update_text_file_transactionally(config, |existing| {
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
        servers.insert(
            "tracedecay".to_string(),
            json!({
                "command": ctx.tracedecay_bin.clone(),
                "args": ["serve"],
            }),
        );
        Ok((
            (),
            TextFileMutation::Write(
                JsonConfigDialect::Jsonc.render_edit(config, existing, &settings)?,
            ),
        ))
    })?;
    Ok(())
}

fn uninstall_mcp_if_selected(components: &[HostComponentV1], config: &Path) -> Result<()> {
    if !components.contains(&HostComponentV1::ContextMcp) || !config.exists() {
        return Ok(());
    }
    update_text_file_transactionally(config, |existing| {
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
            return Ok(((), TextFileMutation::Unchanged));
        };
        if servers.remove("tracedecay").is_none() {
            return Ok(((), TextFileMutation::Unchanged));
        }
        if servers.is_empty() {
            root.remove("context_servers");
        }
        if root.is_empty() {
            return Ok(((), TextFileMutation::Remove));
        }
        Ok((
            (),
            TextFileMutation::Write(
                JsonConfigDialect::Jsonc.render_edit(config, existing, &settings)?,
            ),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::host_bundle::{HostBundleRegistrationStateV1, HostComponentV1};

    fn install_context(home: &Path, binary: &str) -> InstallContext {
        InstallContext {
            profile: tracedecay_runtime_core::config::ProfileRoot::under_home(home),
            home: home.to_path_buf(),
            tracedecay_bin: binary.to_string(),
            project_root: None,
            dashboard: false,
        }
    }

    #[test]
    fn zed_lifecycle_restores_operator_bytes_and_keeps_no_copy() {
        let home = tempfile::tempdir().unwrap();
        let config = zed_settings_path(home.path());
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        let original = br#"{
    // operator comment
    "theme": "dark", /* inline */
    "context_servers": {
        "foreign": {"command": "foreign-mcp",},
    },
}
"#;
        std::fs::write(&config, original).unwrap();
        let components = [HostComponentV1::ContextMcp];
        let install = install_context(home.path(), "/tmp/tracedecay");

        ZedIntegration
            .activate_deployed_host_component_registration(&components, &install)
            .unwrap();

        let installed_text = std::fs::read_to_string(&config).unwrap();
        assert!(
            installed_text
                .starts_with("{\n    // operator comment\n    \"theme\": \"dark\", /* inline */\n"),
            "{installed_text}"
        );
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

        ZedIntegration
            .deactivate_deployed_host_component_registration(&components, &install)
            .unwrap();

        assert_eq!(std::fs::read(&config).unwrap(), original);
        let siblings: Vec<_> = std::fs::read_dir(config.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(siblings, vec![std::ffi::OsString::from("settings.json")]);
    }

    #[test]
    fn zed_uninstall_removes_a_config_created_by_tracedecay() {
        let home = tempfile::tempdir().unwrap();
        let config = zed_settings_path(home.path());
        let components = [HostComponentV1::ContextMcp];
        let install = install_context(home.path(), "/tmp/tracedecay");

        ZedIntegration
            .activate_deployed_host_component_registration(&components, &install)
            .unwrap();
        assert!(config.is_file());

        ZedIntegration
            .deactivate_deployed_host_component_registration(&components, &install)
            .unwrap();

        assert!(!config.exists());
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
}
