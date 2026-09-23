//! Cursor agent integration.
//!
//! Installs tracedecay's Cursor plugin bundle into Cursor's local plugin
//! directory. The plugin owns MCP, hooks, and rule configuration.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use tracedecay_domain::errors::{Result, TraceDecayError};

use super::host_bundle::{HostBundleRegistrationStateV1, HostComponentV1};
use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, load_json_file, load_jsonc_file_strict, safe_remove_host_file, safe_write_text_file,
};

pub struct CursorIntegration;

/// Model-invocable skills shipped by the Cursor plugin.
///
/// The plugin bundle owns this inventory. Host steering adapters can re-export
/// it without reaching back into a composition-root hook module.
pub const CURSOR_PLUGIN_SKILLS: &[&str] = &[
    "assessing-impact",
    "code-health",
    "diagnosing-analytics",
    "discovering-tracedecay",
    "editing-safely",
    "exploring-code",
    "fixing-build-and-type-errors",
    "inspecting-managed-skills",
    "investigating-unexpected-changes",
    "managing-session-context",
    "managing-work",
    "managing-workflows",
    "profiling-tracedecay-performance",
    "project-memory",
    "reviewing-changes",
    "tracing-functions",
    "using-the-cli",
];

impl AgentIntegration for CursorIntegration {
    fn name(&self) -> &'static str {
        "Cursor"
    }

    fn id(&self) -> &'static str {
        "cursor"
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn export_managed_skills(
        &self,
        home: &Path,
        profile_root: &Path,
    ) -> Result<Vec<tracedecay_automation_runtime::automation::skill_targets::SkillInstallSummary>>
    {
        if !cursor_plugin_manifest_path(home).exists() {
            return Ok(Vec::new());
        }
        Ok(vec![
            tracedecay_automation_runtime::automation::skill_targets::install_managed_skills(
                &crate::host_io(),
                profile_root,
                tracedecay_automation_runtime::automation::skill_targets::SkillInstallTarget::Cursor,
                &cursor_plugin_install_dir(home),
            )?,
        ])
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mCursor integration\x1b[0m");
        doctor_check_plugin(dc, &ctx.home);
        doctor_check_native_extension(dc, &ctx.home);
        super::cursor_diagnostics::report_cursor_mcp_log_findings(dc, &ctx.home);
    }

    fn healthcheck_with_daemon_status(
        &self,
        dc: &mut DoctorCounters,
        ctx: &HealthcheckContext,
        daemon_status: Option<&Value>,
    ) {
        self.healthcheck(dc, ctx);
        doctor_check_session_ingest(dc, &ctx.project_path, daemon_status);
    }

    fn host_component_registration(
        &self,
        component: HostComponentV1,
        ctx: &HealthcheckContext,
    ) -> HostBundleRegistrationStateV1 {
        if component == HostComponentV1::Agent {
            return cursor_native_extension_registration(&ctx.home);
        }
        let plugin_dir = cursor_plugin_install_dir(&ctx.home);
        let manifest_path = cursor_plugin_manifest_path(&ctx.home);
        let Ok(manifest_bytes) = std::fs::read(&manifest_path) else {
            return HostBundleRegistrationStateV1::Missing;
        };
        let Ok(manifest) = serde_json::from_slice::<Value>(&manifest_bytes) else {
            return HostBundleRegistrationStateV1::Corrupt;
        };
        if manifest.get("name").and_then(Value::as_str) != Some("tracedecay") {
            return HostBundleRegistrationStateV1::Corrupt;
        }
        let mcp = load_json_file(&plugin_dir.join("mcp.json"));
        let mcp_current = mcp
            .pointer("/mcpServers/tracedecay")
            .is_some_and(Value::is_object);
        if matches!(
            component,
            HostComponentV1::ContextMcp | HostComponentV1::OperatorMcp
        ) {
            return if mcp_current {
                HostBundleRegistrationStateV1::Current
            } else {
                HostBundleRegistrationStateV1::Repairable
            };
        }
        let hooks = load_json_file(&plugin_dir.join("hooks/hooks.json"));
        let native_hooks_current =
            cursor_plugin_hook_expectations()
                .iter()
                .all(|(event, command)| {
                    hooks["hooks"][event.as_str()]
                        .as_array()
                        .is_some_and(|entries| {
                            entries.iter().any(|entry| {
                                entry["command"]
                                    .as_str()
                                    .is_some_and(|value| value.contains(command))
                            })
                        })
                });
        if native_hooks_current && plugin_dir.join("rules/tracedecay.mdc").is_file() {
            HostBundleRegistrationStateV1::Current
        } else {
            HostBundleRegistrationStateV1::Repairable
        }
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".cursor").is_dir()
    }

    fn detected_host_surface(&self, home: &Path) -> Option<PathBuf> {
        let surface = home.join(".cursor");
        surface.is_dir().then_some(surface)
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(cursor_plugin_manifest_path(home))
    }

    fn host_registration_paths(&self, home: &Path) -> Vec<PathBuf> {
        vec![cursor_plugin_manifest_path(home)]
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        cursor_plugin_manifest_path(home).exists()
    }
}

// ---------------------------------------------------------------------------
// Plugin install helpers
// ---------------------------------------------------------------------------

/// The Cursor plugin's composed deploy set, sourced from the shared
/// `plugin/` tree via [`crate::agents::plugin_bundle::cursor_files`].
/// Each entry is `(deploy_relative_path, file_contents)`. The manifest,
/// `mcp.json`, and `hooks/hooks.json` entries are rendered through helpers at
/// install time to inject the package version and the absolute tracedecay
/// binary path.
fn embedded_plugin_files() -> Vec<(&'static str, &'static str)> {
    crate::agents::plugin_bundle::cursor_files()
}

pub fn cursor_plugin_install_dir(home: &Path) -> PathBuf {
    home.join(".cursor/plugins/local/tracedecay")
}

fn cursor_plugin_manifest_path(home: &Path) -> PathBuf {
    cursor_plugin_install_dir(home).join(".cursor-plugin/plugin.json")
}

/// Deploy directory of the native diagnostics extension, versioned exactly
/// like every VS Code-family extension install (`publisher.name-version`) and
/// stamped with the real release version, a `0.0.0` directory next to
/// otherwise-versioned components was an unstampable literal.
pub(super) fn cursor_native_extension_relative_dir() -> String {
    format!(
        ".cursor/extensions/tracedecay.cursor-native-{}",
        crate::PRODUCT_VERSION
    )
}

fn cursor_native_extension_install_dir(home: &Path) -> PathBuf {
    home.join(cursor_native_extension_relative_dir())
}

fn cursor_native_extension_registration(home: &Path) -> HostBundleRegistrationStateV1 {
    let install_dir = cursor_native_extension_install_dir(home);
    let manifest_path = install_dir.join("package.json");
    let Ok(manifest_bytes) = std::fs::read(&manifest_path) else {
        return HostBundleRegistrationStateV1::Missing;
    };
    let Ok(manifest) = serde_json::from_slice::<Value>(&manifest_bytes) else {
        return HostBundleRegistrationStateV1::Corrupt;
    };
    let expected_manifest = manifest.get("name").and_then(Value::as_str) == Some("cursor-native")
        && manifest.get("publisher").and_then(Value::as_str) == Some("tracedecay")
        && manifest.get("main").and_then(Value::as_str) == Some("./dist/extension.js");
    if !expected_manifest {
        return HostBundleRegistrationStateV1::Corrupt;
    }
    if install_dir.join("dist/extension.js").is_file() {
        HostBundleRegistrationStateV1::Current
    } else {
        HostBundleRegistrationStateV1::Repairable
    }
}

/// Doctor coverage for the deployed native diagnostics extension, the one
/// Cursor component the plugin-dir checks never touched, so a missing or
/// half-deployed extension was invisible. A wholly absent extension is
/// informational (the plugin-only install surface never claims it); a
/// stale-version or half-deployed one warns because an install claimed it
/// and it no longer loads current diagnostics.
fn doctor_check_native_extension(dc: &mut DoctorCounters, home: &Path) {
    let install_dir = cursor_native_extension_install_dir(home);
    match cursor_native_extension_registration(home) {
        HostBundleRegistrationStateV1::Current => dc.pass(&format!(
            "Cursor native diagnostics extension {} deployed at {}",
            crate::PRODUCT_VERSION,
            install_dir.display()
        )),
        HostBundleRegistrationStateV1::Missing => {
            let stale = stale_native_extension_dirs(home);
            if stale.is_empty() {
                dc.info(&format!(
                    "Cursor native diagnostics extension {} not deployed ({}), run \
                     `tracedecay install --agent cursor`",
                    crate::PRODUCT_VERSION,
                    install_dir.display()
                ));
            } else {
                dc.warn(&format!(
                    "Cursor native diagnostics extension is stale ({}) while {} is current. \
                     run `tracedecay install --agent cursor` to redeploy",
                    stale.join(", "),
                    crate::PRODUCT_VERSION
                ));
            }
        }
        HostBundleRegistrationStateV1::Repairable => dc.warn(&format!(
            "Cursor native diagnostics extension at {} is incomplete (dist/extension.js \
             missing), run `tracedecay install --agent cursor`",
            install_dir.display()
        )),
        HostBundleRegistrationStateV1::Corrupt => dc.fail(&format!(
            "package.json at {} is not the tracedecay cursor-native extension, inspect and \
             remove it, then run `tracedecay install --agent cursor`",
            install_dir.display()
        )),
    }
}

/// Names of `~/.cursor/extensions/tracedecay.cursor-native-*` directories left
/// by other product versions (e.g. the unstamped `0.0.0` deploys).
fn stale_native_extension_dirs(home: &Path) -> Vec<String> {
    let current = format!("tracedecay.cursor-native-{}", crate::PRODUCT_VERSION);
    let Ok(entries) = std::fs::read_dir(home.join(".cursor/extensions")) else {
        return Vec::new();
    };
    let mut stale: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| name.starts_with("tracedecay.cursor-native-") && *name != current)
        .collect();
    stale.sort();
    stale
}

fn write_embedded_plugin(install_dir: &Path, tracedecay_bin: &str) -> Result<()> {
    for (relative, rendered) in rendered_plugin_files(tracedecay_bin)? {
        safe_write_text_file(&install_dir.join(relative), &rendered)?;
    }
    Ok(())
}

/// Canonical rendered Cursor plugin inventory shared by explicit artifact
/// refresh and the receipt-backed first-party catalog.
pub(crate) fn rendered_plugin_files(tracedecay_bin: &str) -> Result<Vec<(&'static str, String)>> {
    embedded_plugin_files()
        .into_iter()
        .map(|(relative, contents)| {
            let rendered = match relative {
                ".cursor-plugin/plugin.json" => {
                    super::plugin_bundle::stamp_manifest_version(contents)?
                }
                "mcp.json" => super::plugin_bundle::set_mcp_command(contents, tracedecay_bin)?,
                "hooks/hooks.json" => cursor_plugin_hooks(contents, tracedecay_bin)?,
                _ => contents.to_string(),
            };
            Ok((relative, rendered))
        })
        .collect()
}

fn cursor_plugin_hooks(raw: &str, tracedecay_bin: &str) -> Result<String> {
    let mut hooks: serde_json::Value = serde_json::from_str(raw)?;
    if let Some(events) = hooks
        .get_mut("hooks")
        .and_then(|value| value.as_object_mut())
    {
        for entries in events.values_mut().filter_map(|value| value.as_array_mut()) {
            for entry in entries {
                if let Some(command_value) = entry.get_mut("command") {
                    let Some(command) = command_value.as_str() else {
                        continue;
                    };
                    if let Some(suffix) = command.strip_prefix("tracedecay ") {
                        *command_value = json!(super::hook_command(tracedecay_bin, suffix));
                    }
                }
            }
        }
    }
    let rendered = format!("{}\n", serde_json::to_string_pretty(&hooks)?);
    super::plugin_bundle::reject_unresolved_placeholders(&rendered, "Cursor hooks")?;
    Ok(rendered)
}

fn remove_cursor_plugin_install(install_dir: &Path) -> Result<()> {
    let Ok(metadata) = std::fs::symlink_metadata(install_dir) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        safe_remove_host_file(install_dir).map_err(|error| TraceDecayError::Config {
            message: format!("failed to remove {}: {error}", install_dir.display()),
        })?;
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing to replace non-directory Cursor plugin path {}",
                install_dir.display()
            ),
        });
    }
    if !cursor_plugin_dir_is_tracedecay(install_dir) {
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing to replace unmanaged Cursor plugin directory {}",
                install_dir.display()
            ),
        });
    }
    // The directory is tracedecay-owned: remove the managed skill overlay and
    // every file the current bundle ships. User-added files are preserved.
    remove_cursor_managed_skill_overlay(install_dir)?;
    for path in cursor_plugin_managed_paths(install_dir) {
        remove_cursor_plugin_file(&path)?;
    }
    if cursor_plugin_dir_has_only_managed_files(install_dir) {
        match std::fs::remove_dir_all(install_dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(TraceDecayError::Config {
                    message: format!("failed to remove {}: {error}", install_dir.display()),
                });
            }
        }
    }
    Ok(())
}

fn remove_cursor_plugin_file(path: &Path) -> Result<()> {
    match safe_remove_host_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(TraceDecayError::Config {
            message: format!("failed to remove {}: {error}", path.display()),
        }),
    }
}

fn remove_cursor_managed_skill_overlay(install_dir: &Path) -> Result<()> {
    let overlay = install_dir.join("skills/agent-managed");
    match super::collect_regular_files(&overlay) {
        Ok(files) => {
            for path in files {
                remove_cursor_plugin_file(&path)?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!("failed to inspect {}: {error}", overlay.display()),
            });
        }
    }
    match std::fs::remove_dir_all(&overlay) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(TraceDecayError::Config {
            message: format!("failed to remove {}: {error}", overlay.display()),
        }),
    }
}

fn cursor_plugin_dir_is_tracedecay(install_dir: &Path) -> bool {
    let manifest = load_json_file(&install_dir.join(".cursor-plugin/plugin.json"));
    matches!(
        manifest.get("name").and_then(|v| v.as_str()),
        Some("tracedecay")
    )
}

fn cursor_plugin_dir_has_only_managed_files(install_dir: &Path) -> bool {
    let Ok(entries) = super::collect_regular_files(install_dir) else {
        return false;
    };
    let managed = cursor_plugin_managed_paths(install_dir);
    entries.iter().all(|entry| managed.contains(entry))
}

fn cursor_plugin_managed_paths(install_dir: &Path) -> Vec<PathBuf> {
    embedded_plugin_files()
        .into_iter()
        .map(|(relative, _)| install_dir.join(relative))
        .collect()
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

fn doctor_check_plugin(dc: &mut DoctorCounters, home: &Path) {
    let plugin_dir = cursor_plugin_install_dir(home);
    let manifest_path = cursor_plugin_manifest_path(home);
    if !manifest_path.exists() {
        dc.warn(&format!(
            "{} not found, run `tracedecay install --agent cursor` if you use Cursor",
            manifest_path.display()
        ));
        return;
    }

    let manifest = load_json_file(&manifest_path);
    if manifest.get("name").and_then(|v| v.as_str()) == Some("tracedecay")
        && manifest.get("mcpServers").and_then(|v| v.as_str()) == Some("mcp.json")
        && manifest.get("hooks").and_then(|v| v.as_str()) == Some("hooks/hooks.json")
    {
        dc.pass(&format!(
            "Cursor plugin manifest active in {}",
            manifest_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Cursor tracedecay plugin manifest is incomplete in {}",
            manifest_path.display()
        ));
    }
    if let Some(message) =
        super::cursor_diagnostics::plugin_version_staleness(&manifest, crate::PRODUCT_VERSION)
    {
        dc.warn(&message);
    }
    doctor_check_plugin_mcp(dc, &plugin_dir.join("mcp.json"));
    doctor_check_plugin_hooks(dc, &plugin_dir.join("hooks/hooks.json"));
    doctor_check_plugin_rule(dc, &plugin_dir.join("rules/tracedecay.mdc"));
}

fn doctor_check_plugin_mcp(dc: &mut DoctorCounters, mcp_path: &Path) {
    if !mcp_path.exists() {
        dc.warn(&format!(
            "{} not found, run `tracedecay install --agent cursor`",
            mcp_path.display()
        ));
        return;
    }
    let settings = load_json_file(mcp_path);
    // Cursor Settings surfaces the MCP server key literally, so the Cursor
    // plugin registers `tracedecay` (not the Claude/Codex `graph` key).
    let server = &settings["mcpServers"]["tracedecay"];
    if server["command"]
        .as_str()
        .is_some_and(|command| !command.is_empty())
        && server["args"] == json!(["serve", "--path", "${workspaceFolder}"])
    {
        dc.pass(&format!(
            "Cursor plugin MCP registered in {}",
            mcp_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Cursor plugin MCP config is incomplete in {}, run `tracedecay install --agent cursor`",
            mcp_path.display()
        ));
    }
}

/// `(event, hook subcommand)` pairs parsed from the embedded plugin
/// `hooks/hooks.json` template, so the doctor check can never drift from the
/// hooks the bundle actually registers.
fn cursor_plugin_hook_expectations() -> Vec<(String, String)> {
    let files = embedded_plugin_files();
    let raw = files
        .iter()
        .find(|(relative, _)| *relative == "hooks/hooks.json")
        .map_or("{}", |&(_, contents)| contents);
    let template: serde_json::Value = serde_json::from_str(raw).unwrap_or_else(|_| json!({}));
    let Some(events) = template.get("hooks").and_then(|hooks| hooks.as_object()) else {
        return Vec::new();
    };
    events
        .iter()
        .flat_map(|(event, entries)| {
            entries
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| {
                    entry["command"]
                        .as_str()
                        .and_then(|command| command.strip_prefix("tracedecay "))
                        .map(|subcommand| (event.clone(), subcommand.to_string()))
                })
        })
        .collect()
}

fn doctor_check_plugin_hooks(dc: &mut DoctorCounters, hooks_path: &Path) {
    if !hooks_path.exists() {
        dc.warn(&format!(
            "{} not found, run `tracedecay install --agent cursor`",
            hooks_path.display()
        ));
        return;
    }
    let hooks = load_jsonc_file_strict(hooks_path).unwrap_or_else(|e| {
        dc.fail(&format!("{e}"));
        json!({})
    });
    let expected = cursor_plugin_hook_expectations();
    let missing: Vec<&str> = expected
        .iter()
        .filter_map(|(event, command)| {
            let has = hooks["hooks"][event.as_str()]
                .as_array()
                .is_some_and(|entries| {
                    entries.iter().any(|entry| {
                        entry["command"]
                            .as_str()
                            .is_some_and(|value| value.contains(command))
                    })
                });
            (!has).then_some(event.as_str())
        })
        .collect();
    if missing.is_empty() {
        dc.pass(&format!(
            "All {} Cursor plugin lifecycle hooks registered in {}",
            expected.len(),
            hooks_path.display()
        ));
    } else {
        dc.fail(&format!(
            "Cursor plugin hook(s) missing for {}, run `tracedecay install --agent cursor`",
            missing.join(", ")
        ));
    }
}

#[derive(serde::Deserialize)]
struct CursorSessionIngestHealth {
    tracked_transcripts: u64,
    pending_transcripts: u64,
    pending_bytes: u64,
    max_transcript_pending_bytes: u64,
}

#[derive(Debug, PartialEq, Eq)]
enum CursorPlaceholderPathsState {
    Available(Vec<String>),
    Unavailable(String),
}

fn cursor_placeholder_paths_state(status: &Value) -> Option<CursorPlaceholderPathsState> {
    let value = status.get("cursor_session_placeholder_paths")?;
    if let Some(paths) = value.as_array() {
        return Some(CursorPlaceholderPathsState::Available(
            paths
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
        ));
    }
    if value.get("status").and_then(Value::as_str) == Some("unavailable") {
        let reason = value
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        return Some(CursorPlaceholderPathsState::Unavailable(reason));
    }
    None
}

/// Flags a stalled Cursor transcript ingest using Doctor's daemon snapshot.
fn doctor_check_session_ingest(
    dc: &mut DoctorCounters,
    project_path: &Path,
    daemon_status: Option<&Value>,
) {
    let Some(status) = daemon_status else {
        return;
    };
    let placeholder_paths = cursor_placeholder_paths_state(status);
    if let Some(CursorPlaceholderPathsState::Unavailable(reason)) = &placeholder_paths {
        dc.warn(&format!(
            "Cursor transcript placeholder-path diagnostics unavailable from daemon ({reason}); \
             literal workspace placeholders could not be checked"
        ));
    }
    if status
        .pointer("/cursor_session_ingest/status")
        .and_then(Value::as_str)
        == Some("unavailable")
    {
        dc.warn("Cursor transcript ingest health unavailable from daemon session authority");
        return;
    }
    let Some(health) = status
        .get("cursor_session_ingest")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
    else {
        return;
    };
    let paths = match placeholder_paths {
        Some(CursorPlaceholderPathsState::Available(paths)) => paths,
        Some(CursorPlaceholderPathsState::Unavailable(_)) | None => Vec::new(),
    };
    report_cursor_session_ingest(dc, project_path, &health, paths.iter().map(String::as_str));
}

fn report_cursor_session_ingest<'a>(
    dc: &mut DoctorCounters,
    project_path: &Path,
    health: &CursorSessionIngestHealth,
    placeholder_paths: impl Iterator<Item = &'a str>,
) {
    let placeholder_paths = placeholder_paths.collect::<Vec<_>>();
    if !placeholder_paths.is_empty() {
        dc.warn(&format!(
            "Cursor transcript ingest has {} path(s) with a literal workspace placeholder; \
             Cursor did not expand `${{workspaceFolder}}`, so session recall will miss those transcripts",
            placeholder_paths.len(),
        ));
        for path in placeholder_paths {
            dc.info(&format!("  - {path}"));
        }
    }
    if health.max_transcript_pending_bytes > crate::hooks::CURSOR_CATCH_UP_INGEST_MAX_BYTES {
        dc.warn(&format!(
            "Cursor transcript ingest looks stalled: a transcript has {} un-ingested \
             byte(s) ({} byte(s) total across {} transcript(s)), exceeding the {} byte \
             per-transcript hook catch-up cap, it will not drain automatically and \
             session recall is missing those turns. Run `tracedecay sessions import \
             --project-path {}` to schedule bounded convergence",
            health.max_transcript_pending_bytes,
            health.pending_bytes,
            health.pending_transcripts,
            crate::hooks::CURSOR_CATCH_UP_INGEST_MAX_BYTES,
            project_path.display(),
        ));
    } else {
        dc.pass(&format!(
            "Cursor transcript ingest healthy ({} transcript(s) tracked, {} pending \
             byte(s), all within the per-transcript hook cap)",
            health.tracked_transcripts, health.pending_bytes
        ));
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CursorPluginRuleDoctorState {
    Missing,
    Unreadable,
    Incomplete,
    Active,
}

fn cursor_plugin_rule_doctor_state(rule_path: &Path) -> CursorPluginRuleDoctorState {
    if !rule_path.exists() {
        return CursorPluginRuleDoctorState::Missing;
    }
    // The installer deploys the embedded rule byte-for-byte, so an active
    // rule is one that still matches the bundle shipped with this binary; any
    // edit or stale version is a reinstall, not a prose check.
    match std::fs::read_to_string(rule_path) {
        Ok(contents)
            if embedded_plugin_files().iter().any(|(relative, embedded)| {
                *relative == "rules/tracedecay.mdc" && *embedded == contents
            }) =>
        {
            CursorPluginRuleDoctorState::Active
        }
        Ok(_) => CursorPluginRuleDoctorState::Incomplete,
        Err(_) => CursorPluginRuleDoctorState::Unreadable,
    }
}

fn doctor_check_plugin_rule(dc: &mut DoctorCounters, rule_path: &Path) {
    match cursor_plugin_rule_doctor_state(rule_path) {
        CursorPluginRuleDoctorState::Missing => dc.warn(&format!(
            "{} not found, run `tracedecay install --agent cursor`",
            rule_path.display()
        )),
        CursorPluginRuleDoctorState::Unreadable => dc.fail(&format!(
            "Cursor plugin tracedecay rule is unreadable in {}, run `tracedecay install --agent cursor`",
            rule_path.display()
        )),
        CursorPluginRuleDoctorState::Active => dc.pass(&format!(
            "Cursor plugin tracedecay rule active in {}",
            rule_path.display()
        )),
        CursorPluginRuleDoctorState::Incomplete => dc.fail(&format!(
            "Cursor plugin tracedecay rule is incomplete in {}, run `tracedecay install --agent cursor`",
            rule_path.display()
        )),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::agents::host_bundle::{
        HostBundleComponentDoctorStateV1, HostBundleError, HostBundleLifecycleOpV1,
        HostBundleRegistrationStateV1, HostBundleWriterV1, HostComponentSetExecutionRequestV1,
        HostComponentSetLifecycleRequestV1, HostComponentSetTransactionV1, HostKindV1,
    };
    use tempfile::TempDir;
    use tracedecay_host_integration::HostCapabilityUnavailableReasonV1;

    /// The doctor's expected-hooks list is parsed from the embedded bundle
    /// template; a parse regression would silently disable the hook checks.
    #[test]
    fn plugin_hook_expectations_cover_the_bundled_hooks() {
        let expectations = cursor_plugin_hook_expectations();
        assert_eq!(
            expectations.len(),
            8,
            "expected one entry per bundled lifecycle hook, got {expectations:?}"
        );
        assert!(expectations.contains(&(
            "sessionStart".to_string(),
            "hook-cursor-session-start".to_string()
        )));
        assert!(expectations.contains(&(
            "afterFileEdit".to_string(),
            "hook-cursor-after-file-edit".to_string()
        )));
    }

    #[test]
    fn write_embedded_plugin_writes_core_and_bundle_files() {
        let tmp = TempDir::new().unwrap();
        let install_dir = tmp.path().join("tracedecay");
        write_embedded_plugin(&install_dir, "tracedecay").expect("embedded install should succeed");

        // The four core files land, and the manifest is valid JSON carrying the
        // mcpServers key released binaries rely on.
        let manifest_path = install_dir.join(".cursor-plugin/plugin.json");
        let manifest: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["name"], "tracedecay");
        assert_eq!(manifest["mcpServers"], "mcp.json");
        assert!(install_dir.join("README.md").exists());
        assert!(install_dir.join("mcp.json").exists());
        assert!(install_dir.join("hooks/hooks.json").exists());
        assert!(install_dir.join("rules/tracedecay.mdc").exists());

        // Cursor Settings surfaces the MCP server key literally, so the
        // Cursor plugin must register `tracedecay` (not Claude/Codex `graph`).
        let mcp: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(install_dir.join("mcp.json")).unwrap())
                .unwrap();
        let server = &mcp["mcpServers"]["tracedecay"];
        assert!(
            server.is_object(),
            "Cursor mcp.json must declare mcpServers.tracedecay"
        );
        assert_eq!(server["command"], "tracedecay");
        assert_eq!(
            server["args"],
            serde_json::json!(["serve", "--path", "${workspaceFolder}"])
        );
        assert!(
            mcp["mcpServers"].get("graph").is_none(),
            "Cursor mcp.json must not keep the Claude/Codex graph key"
        );

        // A representative skill, the agent, and a native slash command also
        // ship, so released installs are no longer missing the bundle that the
        // symlink path provides.
        assert!(
            install_dir.join("skills/exploring-code/SKILL.md").exists(),
            "a representative skill should be embedded"
        );
        assert!(
            install_dir.join("agents/code-explorer.md").exists(),
            "the code-explorer agent should be embedded"
        );
        assert!(
            install_dir
                .join("commands/tracedecay-map-architecture.md")
                .exists(),
            "a representative native slash command should be embedded"
        );
        // Cursor no longer ships the `tracedecay-*` dispatcher *skills*, those
        // slugs are native commands now.
        assert!(
            !install_dir
                .join("skills/tracedecay-map-architecture/SKILL.md")
                .exists(),
            "the retired dispatcher skill must not ship"
        );

        // Every embedded file is also a managed path so uninstall can clean it.
        let managed = cursor_plugin_managed_paths(&install_dir);
        for (relative, _) in embedded_plugin_files() {
            assert!(
                managed.contains(&install_dir.join(relative)),
                "{relative} should be a managed path"
            );
        }
    }

    #[test]
    fn catalog_cursor_bundle_passes_doctor_and_preserves_user_config() {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let user_config = home.path().join(".cursor/mcp.json");
        std::fs::create_dir_all(user_config.parent().unwrap()).unwrap();
        let user_config_bytes = br#"{"mcpServers":{"operator":{"command":"other"}}}"#;
        std::fs::write(&user_config, user_config_bytes).unwrap();

        let integration = CursorIntegration;
        let plugin_dir = cursor_plugin_install_dir(home.path());
        write_embedded_plugin(&plugin_dir, "/opt/tracedecay-next").unwrap();
        let installed_mcp: serde_json::Value =
            serde_json::from_slice(&std::fs::read(plugin_dir.join("mcp.json")).unwrap()).unwrap();
        assert_eq!(
            installed_mcp["mcpServers"]["tracedecay"]["command"],
            "/opt/tracedecay-next"
        );
        assert!(plugin_dir.join("agents/code-explorer.md").is_file());
        assert_eq!(std::fs::read(&user_config).unwrap(), user_config_bytes);

        let mut doctor = DoctorCounters::new();
        integration.healthcheck(
            &mut doctor,
            &HealthcheckContext {
                home: home.path().to_path_buf(),
                project_path: project.path().to_path_buf(),
            },
        );
        assert_eq!(doctor.issues, 0);
        assert_eq!(doctor.warnings, 0);
    }

    fn cursor_component_set(
        tracedecay_bin: &str,
    ) -> crate::agents::host_bundle_registry::VerifiedEmbeddedHostComponentSetV1 {
        use crate::agents::host_bundle_registry::{
            default_components, verified_embedded_host_component_set_with_tracedecay_bin,
        };

        verified_embedded_host_component_set_with_tracedecay_bin(
            HostKindV1::CursorDesktop,
            &default_components(HostKindV1::CursorDesktop),
            0,
            tracedecay_bin,
            crate::agents::TEST_GENERATOR_COMMIT,
        )
        .expect("the packaged Cursor Desktop component set must verify")
    }

    fn cursor_component_request(
        operation: HostBundleLifecycleOpV1,
        operation_id: [u8; 16],
        explicit_confirmation: bool,
    ) -> HostComponentSetExecutionRequestV1 {
        HostComponentSetExecutionRequestV1 {
            lifecycle: HostComponentSetLifecycleRequestV1 {
                operation,
                expected_host: HostKindV1::CursorDesktop,
                expected_components: crate::agents::host_bundle_registry::default_components(
                    HostKindV1::CursorDesktop,
                ),
                explicit_confirmation,
                hermes_profile_bindings: 0,
                explicit_adoption: false,
            },
            operation_id,
        }
    }

    /// [`cursor_component_request`] carrying the operator's `--yes --adopt`.
    fn adopting_cursor_component_request(
        operation: HostBundleLifecycleOpV1,
        operation_id: [u8; 16],
    ) -> HostComponentSetExecutionRequestV1 {
        let mut request = cursor_component_request(operation, operation_id, true);
        request.lifecycle.explicit_adoption = true;
        request
    }

    #[test]
    fn cursor_component_transaction_updates_doctor_and_preserves_denied_state() {
        use crate::agents::host_bundle_registry::{
            HostBundleRegistryError, verified_embedded_default_host_component_set,
        };

        let home = TempDir::new().unwrap();
        let lifecycle = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let user_config = home.path().join(".cursor/mcp.json");
        let user_config_bytes = br#"{"mcpServers":{"operator":{"command":"other"}}}"#;
        std::fs::create_dir_all(user_config.parent().unwrap()).unwrap();
        std::fs::write(&user_config, user_config_bytes).unwrap();

        let previous = cursor_component_set("/opt/tracedecay-v1");
        let install = cursor_component_request(HostBundleLifecycleOpV1::Install, [61; 16], true);
        let mut writer =
            HostBundleWriterV1::open_with_lifecycle_root(home.path(), lifecycle.path()).unwrap();
        let mut registration = crate::agents::host_component_registration::CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin(
            "cursor",
            home.path(),
            install.lifecycle.operation,
            "/opt/tracedecay-v1".to_string(),
        )
        .unwrap();
        HostComponentSetTransactionV1::new(&mut writer)
            .execute(
                &previous.component_set,
                &install,
                &previous,
                &mut registration,
            )
            .expect("the packaged Cursor set must install through its production transaction");

        let mcp_path = cursor_plugin_install_dir(home.path()).join("mcp.json");
        let installed_mcp: Value =
            serde_json::from_slice(&std::fs::read(&mcp_path).unwrap()).unwrap();
        assert_eq!(
            installed_mcp["mcpServers"]["tracedecay"]["command"],
            "/opt/tracedecay-v1"
        );

        let current_bin =
            super::super::which_tracedecay().unwrap_or_else(|| "tracedecay".to_string());
        let current = cursor_component_set(&current_bin);
        let update = cursor_component_request(HostBundleLifecycleOpV1::Update, [62; 16], true);
        let mut registration = crate::agents::host_component_registration::CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin(
            "cursor",
            home.path(),
            update.lifecycle.operation,
            current_bin.clone(),
        )
        .unwrap();
        let update_receipt = HostComponentSetTransactionV1::new(&mut writer)
            .execute(&current.component_set, &update, &current, &mut registration)
            .expect("a newer packaged Cursor set must update through the transaction");
        assert_eq!(update_receipt.operation_id, update.operation_id);

        let updated_mcp: Value =
            serde_json::from_slice(&std::fs::read(&mcp_path).unwrap()).unwrap();
        assert_eq!(
            updated_mcp["mcpServers"]["tracedecay"]["command"],
            current_bin
        );
        let updated_bytes = std::fs::read(&mcp_path).unwrap();

        let mut repeat_registration = crate::agents::host_component_registration::CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin(
            "cursor",
            home.path(),
            update.lifecycle.operation,
            current_bin.clone(),
        )
        .unwrap();
        let repeated_receipt = HostComponentSetTransactionV1::new(&mut writer)
            .execute(
                &current.component_set,
                &update,
                &current,
                &mut repeat_registration,
            )
            .expect("repeating the same confirmed transaction must be idempotent");
        assert_eq!(repeated_receipt, update_receipt);
        assert_eq!(std::fs::read(&mcp_path).unwrap(), updated_bytes);

        let denied = cursor_component_request(HostBundleLifecycleOpV1::Update, [63; 16], false);
        let rejected = cursor_component_set("/opt/tracedecay-v3");
        let mut denied_registration = crate::agents::host_component_registration::CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin(
            "cursor",
            home.path(),
            denied.lifecycle.operation,
            "/opt/tracedecay-v3".to_string(),
        )
        .unwrap();
        let preview = HostComponentSetTransactionV1::new(&mut writer)
            .preview(
                &rejected.component_set,
                &denied,
                &rejected,
                &mut denied_registration,
            )
            .expect("a denied Cursor update must still produce its truthful preview");
        assert_eq!(
            HostComponentSetTransactionV1::new(&mut writer)
                .execute_confirmed(
                    &rejected.component_set,
                    &denied,
                    &preview,
                    &rejected,
                    &mut denied_registration,
                )
                .expect_err("an unconfirmed Cursor update must not mutate the installed release"),
            HostBundleError::ConfirmationRequired
        );
        assert_eq!(std::fs::read(&mcp_path).unwrap(), updated_bytes);

        let report = crate::agents::inspect_receipt_backed_host_components(
            &HealthcheckContext {
                home: home.path().to_path_buf(),
                project_path: project.path().to_path_buf(),
            },
            lifecycle.path(),
            crate::agents::TEST_GENERATOR_COMMIT,
        )
        .expect("Doctor must inspect the transaction receipts through production registration");
        assert_eq!(
            report.components.len(),
            current.component_set.components.len(),
            "Doctor must report exactly the Cursor Desktop component set: {report:#?}"
        );
        for expected_component in [
            HostComponentV1::Core,
            HostComponentV1::Agent,
            HostComponentV1::ContextMcp,
        ] {
            let component = report
                .components
                .iter()
                .find(|component| component.component == Some(expected_component))
                .unwrap_or_else(|| {
                    panic!("Doctor omitted Cursor Desktop {expected_component:?}: {report:#?}")
                });
            assert_eq!(
                component.host,
                Some(HostKindV1::CursorDesktop),
                "unexpected Doctor host for {expected_component:?}: {component:#?}; full report: {report:#?}"
            );
            assert_eq!(
                component.state,
                HostBundleComponentDoctorStateV1::Current,
                "unexpected Doctor state for {expected_component:?}: {component:#?}; full report: {report:#?}"
            );
            assert_eq!(
                component.registration,
                Some(HostBundleRegistrationStateV1::Current),
                "unexpected registration state for {expected_component:?}: {component:#?}; full report: {report:#?}"
            );
        }
        assert_eq!(std::fs::read(&user_config).unwrap(), user_config_bytes);

        assert_eq!(
            verified_embedded_default_host_component_set(
                HostKindV1::CursorCloud,
                0,
                crate::agents::TEST_GENERATOR_COMMIT
            ),
            Err(HostBundleRegistryError::HostComponentSetUnavailable {
                host: HostKindV1::CursorCloud,
                reason: HostCapabilityUnavailableReasonV1::HostRegistrationUnsupported,
            }),
            "Cursor Cloud remains excluded from the production component transaction"
        );
    }

    /// A cataloged deploy path proves nothing about ownership: bytes an
    /// operator parked at the plugin manifest path are refused untouched by
    /// Install and Update unless the operator explicitly adopts. Explicit
    /// adoption then takes them over.
    #[test]
    fn cursor_transaction_refuses_unrecognized_receiptless_bytes_without_adoption() {
        for operation in [
            HostBundleLifecycleOpV1::Install,
            HostBundleLifecycleOpV1::Update,
        ] {
            let home = TempDir::new().unwrap();
            let lifecycle = TempDir::new().unwrap();
            let manifest_path = cursor_plugin_manifest_path(home.path());
            std::fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
            let operator_bytes = b"my own plugin experiment, not a tracedecay bundle";
            std::fs::write(&manifest_path, operator_bytes).unwrap();

            let current_bin =
                super::super::which_tracedecay().unwrap_or_else(|| "tracedecay".to_string());
            let current = cursor_component_set(&current_bin);
            let mut writer =
                HostBundleWriterV1::open_with_lifecycle_root(home.path(), lifecycle.path())
                    .unwrap();

            let request = cursor_component_request(operation, [71; 16], true);
            let mut registration = crate::agents::host_component_registration::CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin(
                "cursor",
                home.path(),
                operation,
                current_bin.clone(),
            )
            .unwrap();
            let error = HostComponentSetTransactionV1::new(&mut writer)
                .execute(
                    &current.component_set,
                    &request,
                    &current,
                    &mut registration,
                )
                .expect_err("unrecognized receiptless bytes must refuse without adoption");
            assert!(
                matches!(error, HostBundleError::OwnershipConflict(_)),
                "{operation:?}: {error}"
            );
            assert!(
                error.to_string().contains("--yes --adopt"),
                "the refusal must name the explicit adoption remedy: {error}"
            );
            assert_eq!(
                std::fs::read(&manifest_path).unwrap(),
                operator_bytes,
                "{operation:?} must leave the refused file byte-for-byte untouched"
            );

            let adopt = adopting_cursor_component_request(operation, [72; 16]);
            let mut adopting_registration = crate::agents::host_component_registration::CatalogHostComponentRegistrationAuthority::new_with_tracedecay_bin(
                "cursor",
                home.path(),
                operation,
                current_bin.clone(),
            )
            .unwrap();
            HostComponentSetTransactionV1::new(&mut writer)
                .execute(
                    &current.component_set,
                    &adopt,
                    &current,
                    &mut adopting_registration,
                )
                .unwrap_or_else(|error| {
                    panic!("explicit adoption must take the file over: {error}")
                });
            let manifest: Value =
                serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
            assert_eq!(manifest["name"], "tracedecay");
        }
    }

    #[test]
    fn native_extension_registration_is_receipt_doctor_ready() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            cursor_native_extension_registration(tmp.path()),
            HostBundleRegistrationStateV1::Missing
        );

        let install_dir = cursor_native_extension_install_dir(tmp.path());
        std::fs::create_dir_all(install_dir.join("dist")).unwrap();
        std::fs::write(
            install_dir.join("package.json"),
            r#"{
                "name": "cursor-native",
                "publisher": "tracedecay",
                "main": "./dist/extension.js"
            }"#,
        )
        .unwrap();
        assert_eq!(
            cursor_native_extension_registration(tmp.path()),
            HostBundleRegistrationStateV1::Repairable
        );

        std::fs::write(
            install_dir.join("dist/extension.js"),
            "module.exports = {};",
        )
        .unwrap();
        assert_eq!(
            cursor_native_extension_registration(tmp.path()),
            HostBundleRegistrationStateV1::Current
        );
    }

    /// The skill index injected into Cursor `sessionStart` context must match
    /// the *model-invocable* skills shipped in the bundle, slash dispatchers
    /// (`disable-model-invocation: true`) are explicit-invoke-only and would
    /// be noise in steering context.
    #[test]
    fn session_context_skill_index_matches_bundle_skills() {
        let mut bundled: Vec<String> = embedded_plugin_files()
            .into_iter()
            .filter_map(|(relative, contents)| {
                let name = relative
                    .strip_prefix("skills/")
                    .and_then(|rest| rest.strip_suffix("/SKILL.md"))?;
                (!contents.contains("disable-model-invocation: true")).then(|| name.to_string())
            })
            .collect();
        bundled.sort();
        let mut listed: Vec<String> = CURSOR_PLUGIN_SKILLS
            .iter()
            .map(|skill| (*skill).to_string())
            .collect();
        listed.sort();
        assert_eq!(
            bundled, listed,
            "hooks::CURSOR_PLUGIN_SKILLS must list exactly the model-invocable bundled skills"
        );
    }

    #[test]
    fn embedded_install_uninstalls_completely() {
        let tmp = TempDir::new().unwrap();
        let install_dir = tmp.path().join("tracedecay");
        write_embedded_plugin(&install_dir, "tracedecay").expect("embedded install should succeed");
        assert!(install_dir.join("skills/exploring-code/SKILL.md").exists());

        // Because managed paths cover every embedded file, uninstall recognises a
        // tracedecay-only directory and removes it entirely.
        remove_cursor_plugin_install(&install_dir).expect("uninstall should succeed");
        assert!(
            !install_dir.exists(),
            "embedded install should be fully removed on uninstall"
        );
    }

    /// The clean replace must refuse to delete a directory tracedecay does not
    /// own (no tracedecay plugin manifest), so it never nukes an unrelated dir.
    #[test]
    fn clean_replace_refuses_unmanaged_dir() {
        let tmp = TempDir::new().unwrap();
        let install_dir = tmp.path().join("tracedecay");
        std::fs::create_dir_all(&install_dir).unwrap();
        std::fs::write(install_dir.join("user-file.txt"), "not tracedecay").unwrap();

        let err = remove_cursor_plugin_install(&install_dir)
            .expect_err("must refuse an unmanaged directory");
        assert!(
            err.to_string().contains("unmanaged"),
            "unexpected error: {err}"
        );
        assert!(
            install_dir.join("user-file.txt").exists(),
            "an unmanaged dir must be left untouched"
        );
    }

    #[test]
    fn uninstall_removes_managed_files_and_preserves_user_files() {
        let tmp = TempDir::new().unwrap();
        let install_dir = tmp.path().join("tracedecay");
        write_embedded_plugin(&install_dir, "tracedecay").expect("embedded install should succeed");
        std::fs::write(install_dir.join("user-keep.txt"), "keep").unwrap();

        remove_cursor_plugin_install(&install_dir).expect("uninstall should succeed");

        assert_eq!(
            std::fs::read_to_string(install_dir.join("user-keep.txt")).unwrap(),
            "keep"
        );
        assert!(
            !install_dir.join(".cursor-plugin/plugin.json").exists(),
            "managed plugin files must be removed beside operator files"
        );
        assert!(
            !install_dir.join("rules/tracedecay.mdc").exists(),
            "managed rule files must be removed beside operator files"
        );
    }

    #[test]
    fn leftover_managed_file_removal_propagates_errors() {
        let tmp = TempDir::new().unwrap();
        let install_dir = tmp.path().join("tracedecay");
        write_embedded_plugin(&install_dir, "tracedecay").expect("embedded install should succeed");
        std::fs::write(install_dir.join("user-keep.txt"), "keep").unwrap();
        let managed = install_dir.join("rules/tracedecay.mdc");
        std::fs::remove_file(&managed).unwrap();
        std::fs::create_dir(&managed).unwrap();
        std::fs::write(managed.join("nested"), "blocked").unwrap();

        let error = remove_cursor_plugin_install(&install_dir)
            .expect_err("a leftover managed path that is not a file must fail uninstall");
        assert!(error.to_string().contains("failed to remove"), "{error}");
        assert_eq!(
            std::fs::read_to_string(install_dir.join("user-keep.txt")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn doctor_plugin_rule_distinguishes_unreadable_from_incomplete() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("missing.mdc");
        assert_eq!(
            cursor_plugin_rule_doctor_state(&missing),
            CursorPluginRuleDoctorState::Missing
        );

        let incomplete = tmp.path().join("incomplete.mdc");
        std::fs::write(&incomplete, "alwaysApply: false\n").unwrap();
        assert_eq!(
            cursor_plugin_rule_doctor_state(&incomplete),
            CursorPluginRuleDoctorState::Incomplete
        );

        let (_, embedded) = embedded_plugin_files()
            .into_iter()
            .find(|(relative, _)| *relative == "rules/tracedecay.mdc")
            .expect("the Cursor bundle ships rules/tracedecay.mdc");
        let active = tmp.path().join("active.mdc");
        std::fs::write(&active, embedded).unwrap();
        assert_eq!(
            cursor_plugin_rule_doctor_state(&active),
            CursorPluginRuleDoctorState::Active
        );
        std::fs::write(&active, format!("{embedded}\n# local edit\n")).unwrap();
        assert_eq!(
            cursor_plugin_rule_doctor_state(&active),
            CursorPluginRuleDoctorState::Incomplete,
            "an edited rule is drift, not an active install"
        );

        let unreadable = tmp.path().join("unreadable.mdc");
        std::fs::create_dir(&unreadable).unwrap();
        assert_eq!(
            cursor_plugin_rule_doctor_state(&unreadable),
            CursorPluginRuleDoctorState::Unreadable
        );

        let mut counters = DoctorCounters::new();
        doctor_check_plugin_rule(&mut counters, &unreadable);
        assert_eq!(counters.issues, 1);
        assert_eq!(counters.warnings, 0);
    }

    fn session_ingest_status(placeholder_paths: Value) -> Value {
        json!({
            "cursor_session_ingest": {
                "tracked_transcripts": 1,
                "pending_transcripts": 0,
                "pending_bytes": 0,
                "max_transcript_pending_bytes": 0,
            },
            "cursor_session_placeholder_paths": placeholder_paths,
        })
    }

    #[test]
    fn cursor_placeholder_paths_empty_array_remains_available() {
        let status = session_ingest_status(json!([]));
        assert_eq!(
            cursor_placeholder_paths_state(&status),
            Some(CursorPlaceholderPathsState::Available(Vec::new()))
        );

        let mut counters = DoctorCounters::new();
        doctor_check_session_ingest(&mut counters, Path::new("/project"), Some(&status));
        assert_eq!(counters.warnings, 0);
    }

    #[test]
    fn cursor_placeholder_paths_nonempty_array_remains_available() {
        let status = session_ingest_status(json!(["${workspaceFolder}/cursor.jsonl"]));
        assert_eq!(
            cursor_placeholder_paths_state(&status),
            Some(CursorPlaceholderPathsState::Available(vec![
                "${workspaceFolder}/cursor.jsonl".to_owned()
            ]))
        );

        let mut counters = DoctorCounters::new();
        doctor_check_session_ingest(&mut counters, Path::new("/project"), Some(&status));
        assert_eq!(counters.warnings, 1);
    }

    #[test]
    fn cursor_placeholder_paths_typed_unavailable_is_warned() {
        let status = session_ingest_status(json!({
            "status": "unavailable",
            "reason": "cursor_session_placeholder_paths_query_failed",
        }));
        assert_eq!(
            cursor_placeholder_paths_state(&status),
            Some(CursorPlaceholderPathsState::Unavailable(
                "cursor_session_placeholder_paths_query_failed".to_owned()
            ))
        );

        let mut counters = DoctorCounters::new();
        doctor_check_session_ingest(&mut counters, Path::new("/project"), Some(&status));
        assert_eq!(counters.issues, 0);
        assert_eq!(counters.warnings, 1);
    }

    #[test]
    fn session_ingest_healthcheck_reports_daemon_snapshot() {
        let mut counters = DoctorCounters::new();
        let health = CursorSessionIngestHealth {
            tracked_transcripts: 2,
            pending_transcripts: 1,
            pending_bytes: crate::hooks::CURSOR_CATCH_UP_INGEST_MAX_BYTES + 1,
            max_transcript_pending_bytes: crate::hooks::CURSOR_CATCH_UP_INGEST_MAX_BYTES + 1,
        };

        report_cursor_session_ingest(
            &mut counters,
            Path::new("/project"),
            &health,
            ["${workspaceFolder}/cursor.jsonl"].into_iter(),
        );

        assert_eq!(counters.issues, 0);
        assert_eq!(counters.warnings, 2);
    }

    #[test]
    fn session_ingest_healthcheck_warns_when_daemon_authority_is_unavailable() {
        let mut counters = DoctorCounters::new();
        doctor_check_session_ingest(
            &mut counters,
            Path::new("/project"),
            Some(&serde_json::json!({
                "cursor_session_ingest": {
                    "status": "unavailable",
                    "message": "daemon project session authority is unavailable",
                }
            })),
        );

        assert_eq!(counters.issues, 0);
        assert_eq!(counters.warnings, 1);
    }
}
