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
//! See <https://github.com/zed-industries/zed/discussions/58417>.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::errors::Result;

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, JsonConfigDialect,
    McpDoctorLabels, McpUninstallPolicy, config_backup_path, doctor_check_mcp_registration,
    install_mcp_server_entry, load_jsonc_file, uninstall_mcp_server_entry,
};
use super::host_bundle_v2::{HostBundleComponentV1, HostBundleRegistrationStateV1};

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
        let project = zed_project_settings_path(&ctx.project_path);
        if zed_settings_path(&ctx.home).exists() || !project.exists() {
            doctor_check_settings(dc, &zed_settings_path(&ctx.home), "Zed");
        }
        if project.exists() {
            doctor_check_settings(dc, &project, "Zed project");
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
        if component != HostBundleComponentV1::ContextMcp {
            return HostBundleRegistrationStateV1::Missing;
        }
        zed_registration_state(&zed_settings_path(&ctx.home), None)
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
        zed_registration_state(&zed_settings_path(&ctx.home), Some(&install.tracedecay_bin))
    }

    fn project_host_component_registration_for_lifecycle(
        &self,
        component: HostBundleComponentV1,
        ctx: &HealthcheckContext,
        install: &InstallContext,
    ) -> HostBundleRegistrationStateV1 {
        if component != HostBundleComponentV1::ContextMcp {
            return HostBundleRegistrationStateV1::Missing;
        }
        zed_registration_state(
            &zed_project_settings_path(&ctx.project_path),
            Some(&install.tracedecay_bin),
        )
    }

    fn is_detected(&self, home: &Path) -> bool {
        zed_config_dir(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(zed_settings_path(home))
    }

    fn host_component_registration_paths(
        &self,
        components: &[HostBundleComponentV1],
        home: &Path,
    ) -> Vec<PathBuf> {
        registration_paths(components, zed_settings_path(home))
    }

    fn project_host_component_registration_paths(
        &self,
        components: &[HostBundleComponentV1],
        _home: &Path,
        project_path: &Path,
    ) -> Result<Vec<PathBuf>> {
        Ok(registration_paths(
            components,
            zed_project_settings_path(project_path),
        ))
    }

    fn activate_deployed_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        install_zed_registration(components, &zed_settings_path(&ctx.home), ctx)
    }

    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        uninstall_zed_registration(components, &zed_settings_path(&ctx.home))
    }

    fn activate_project_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        let settings = zed_project_settings_path(project_path);
        let backup = config_backup_path(&settings);
        super::ensure_project_local_safe_paths(
            project_path,
            [settings.as_path(), backup.as_path()],
        )?;
        install_zed_registration(components, &settings, ctx)
    }

    fn deactivate_project_host_component_registration(
        &self,
        components: &[HostBundleComponentV1],
        _ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        let settings = zed_project_settings_path(project_path);
        let backup = config_backup_path(&settings);
        super::ensure_project_local_safe_paths(
            project_path,
            [settings.as_path(), backup.as_path()],
        )?;
        uninstall_zed_registration(components, &settings)
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

fn registration_paths(components: &[HostBundleComponentV1], settings: PathBuf) -> Vec<PathBuf> {
    if components == [HostBundleComponentV1::ContextMcp] {
        vec![settings.clone(), config_backup_path(&settings)]
    } else {
        Vec::new()
    }
}

fn zed_registration_state(
    settings_path: &Path,
    expected_binary: Option<&str>,
) -> HostBundleRegistrationStateV1 {
    let bytes = match std::fs::read_to_string(settings_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return HostBundleRegistrationStateV1::Missing;
        }
        Err(_) => return HostBundleRegistrationStateV1::Corrupt,
    };
    let settings = super::parse_jsonc(&bytes);
    if !settings.is_object() {
        return HostBundleRegistrationStateV1::Corrupt;
    }
    let Some(server) = settings.pointer("/context_servers/tracedecay") else {
        return HostBundleRegistrationStateV1::Missing;
    };
    let command_is_current = server
        .get("command")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|command| {
            expected_binary.map_or_else(|| !command.is_empty(), |expected| command == expected)
        });
    let serves = server
        .get("args")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|args| args.iter().any(|arg| arg.as_str() == Some("serve")));
    if command_is_current && serves {
        HostBundleRegistrationStateV1::Current
    } else {
        HostBundleRegistrationStateV1::Repairable
    }
}

fn install_zed_registration(
    components: &[HostBundleComponentV1],
    settings_path: &Path,
    ctx: &InstallContext,
) -> Result<()> {
    if components.contains(&HostBundleComponentV1::ContextMcp) {
        install_mcp_server_entry(
            settings_path,
            "context_servers",
            json!({
                "command": ctx.tracedecay_bin.clone(),
                "args": ["serve"],
            }),
            "Zed",
            JsonConfigDialect::Jsonc,
        )?;
    }
    Ok(())
}

fn uninstall_zed_registration(
    components: &[HostBundleComponentV1],
    settings_path: &Path,
) -> Result<()> {
    if components.contains(&HostBundleComponentV1::ContextMcp) {
        uninstall_mcp_server_entry(
            settings_path,
            "context_servers",
            JsonConfigDialect::Jsonc,
            McpUninstallPolicy::default(),
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

fn doctor_check_settings(dc: &mut DoctorCounters, settings_path: &Path, product: &str) {
    doctor_check_mcp_registration(
        dc,
        settings_path,
        "context_servers",
        load_jsonc_file,
        &McpDoctorLabels {
            agent_id: "zed",
            product,
            registered: "Context server registered",
            missing: "Context server NOT registered",
        },
    );
}
