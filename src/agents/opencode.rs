// Rust guideline compliant 2025-10-17
//! `OpenCode` agent integration.
//!
//! Handles `TraceDecay`'s MCP and custom LSP registration in `OpenCode`'s config,
//! native TypeScript plugin deployment, and prompt/managed-skill rules.
//! `OpenCode` uses interactive runtime approval rather than declarative tool
//! permissions.

use std::path::Path;

use serde_json::json;

use crate::errors::{Result, TraceDecayError};

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, UpdatePluginOutcome,
    backup_config_file, load_json_file, load_json_file_strict, safe_write_json_file,
    safe_write_text_file,
};

use super::prompt_rules::{PROMPT_RULE_MARKER, PromptRulesOptions};

/// `OpenCode` agent.
pub struct OpenCodeIntegration;

const OPENCODE_PLUGIN_SOURCE: &str = include_str!("../../plugin/opencode/tracedecay.ts");
const OPENCODE_PLUGIN_MARKER: &str = "TraceDecayPlugin";
/// Deployed path of the managed plugin relative to the `OpenCode` config dir.
pub(crate) const OPENCODE_PLUGIN_RELATIVE: &str = "plugins/tracedecay.ts";
pub(crate) const TRACEDECAY_LSP_EXTENSIONS: &[&str] = &[
    ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".c", ".h", ".cc", ".cpp", ".cxx", ".hh",
    ".hpp", ".hxx", ".m", ".mm", ".zig", ".lua", ".php",
];

impl AgentIntegration for OpenCodeIntegration {
    fn name(&self) -> &'static str {
        "OpenCode"
    }

    fn id(&self) -> &'static str {
        "opencode"
    }

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        let config_path = opencode_config_path(&ctx.home);
        install_mcp_server(&config_path, &ctx.tracedecay_bin)?;
        install_opencode_plugin(&opencode_plugin_path(&ctx.home), &ctx.tracedecay_bin)?;

        let global_prompt = opencode_prompt_path(&ctx.home);
        install_prompt_rules(&global_prompt)?;
        super::install_managed_skill_prompt_index(
            &ctx.home,
            &global_prompt,
            crate::automation::skill_targets::SkillInstallTarget::OpenCode,
        )?;

        eprintln!();
        eprintln!("Setup complete. Next steps:");
        eprintln!("  1. cd into your project and run: tracedecay init");
        eprintln!("  2. Start a new OpenCode session — tracedecay tools are now available");
        eprintln!("  3. OpenCode will prompt for approval on first use of each tool");
        Ok(())
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    fn install_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        let mcp_path = project_path.join("opencode.json");
        let agents_md = project_path.join("AGENTS.md");
        super::ensure_project_local_safe_paths(
            project_path,
            [mcp_path.as_path(), agents_md.as_path()],
        )?;
        install_mcp_server(&mcp_path, &ctx.tracedecay_bin)?;
        install_opencode_plugin(
            &project_path.join(".opencode/plugins/tracedecay.ts"),
            &ctx.tracedecay_bin,
        )?;
        install_prompt_rules(&agents_md)?;
        super::install_managed_skill_prompt_index(
            &ctx.home,
            &agents_md,
            crate::automation::skill_targets::SkillInstallTarget::OpenCode,
        )
    }

    fn uninstall_local(&self, ctx: &InstallContext, project_path: &Path) -> Result<()> {
        uninstall_mcp_server(&project_path.join("opencode.json"));
        remove_opencode_plugin(&project_path.join(".opencode/plugins/tracedecay.ts"))?;
        let agents_md = project_path.join("AGENTS.md");
        super::remove_managed_skill_prompt_index(
            &ctx.home,
            &agents_md,
            crate::automation::skill_targets::SkillInstallTarget::OpenCode,
        )?;
        uninstall_prompt_rules(&agents_md);
        Ok(())
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        let config_path = opencode_config_path(&ctx.home);
        uninstall_mcp_server(&config_path);
        remove_opencode_plugin(&opencode_plugin_path(&ctx.home))?;

        let global_prompt = opencode_prompt_path(&ctx.home);
        super::remove_managed_skill_prompt_index(
            &ctx.home,
            &global_prompt,
            crate::automation::skill_targets::SkillInstallTarget::OpenCode,
        )?;
        uninstall_prompt_rules(&global_prompt);

        eprintln!();
        eprintln!("Uninstall complete. Tracedecay has been removed from OpenCode.");
        eprintln!("Start a new OpenCode session for changes to take effect.");
        Ok(())
    }

    fn update_plugin(&self, ctx: &InstallContext) -> Result<UpdatePluginOutcome> {
        let plugin_path = opencode_plugin_path(&ctx.home);
        if !plugin_path.exists() {
            return Ok(UpdatePluginOutcome::NotInstalled);
        }
        install_opencode_plugin(&plugin_path, &ctx.tracedecay_bin)?;
        Ok(UpdatePluginOutcome::Refreshed(vec![plugin_path]))
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mOpenCode integration\x1b[0m");
        doctor_check_config(dc, &ctx.home);
        doctor_check_prompt(dc, &ctx.home);
        doctor_check_plugin(dc, &ctx.home);
    }

    fn host_component_registration(
        &self,
        component: super::host_bundle_v2::HostBundleComponentV1,
        ctx: &HealthcheckContext,
    ) -> super::host_bundle_v2::HostBundleRegistrationStateV1 {
        use super::host_bundle_v2::{
            HostBundleComponentV1, HostBundleRegistrationStateV1 as State,
        };

        let config_path = opencode_config_path(&ctx.home);
        let config = match std::fs::read(&config_path) {
            Ok(config_bytes) => {
                let Ok(config) = serde_json::from_slice::<serde_json::Value>(&config_bytes) else {
                    return State::Corrupt;
                };
                Some(config)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return State::Corrupt,
        };
        let mcp_current = config
            .as_ref()
            .and_then(|config| config.pointer("/mcp/tracedecay/command"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|args| args.iter().any(|arg| arg.as_str() == Some("serve")));
        let lsp_current = config
            .as_ref()
            .and_then(|config| config.pointer("/lsp/tracedecay/command"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|args| {
                ["lsp", "bridge", "--stdio"]
                    .iter()
                    .all(|expected| args.iter().any(|arg| arg.as_str() == Some(expected)))
            });
        if component == HostBundleComponentV1::ContextMcp {
            return if mcp_current {
                State::Current
            } else {
                State::Missing
            };
        }
        if component == HostBundleComponentV1::OperatorMcp {
            return State::Missing;
        }
        let config_root = config_path.parent().unwrap_or(&ctx.home);
        if component == HostBundleComponentV1::Agent {
            let assets = super::plugin_bundle::opencode_agent_files()
                .into_iter()
                .map(|(relative, _)| config_root.join(relative).is_file())
                .collect::<Vec<_>>();
            return if assets.iter().all(|current| *current) {
                State::Current
            } else if assets.iter().any(|current| *current) {
                State::Repairable
            } else {
                State::Missing
            };
        }
        let plugin_path = opencode_plugin_path(&ctx.home);
        let plugin_current = std::fs::read_to_string(&plugin_path)
            .is_ok_and(|contents| contents.contains(OPENCODE_PLUGIN_MARKER));
        let prompt_current = std::fs::read_to_string(opencode_prompt_path(&ctx.home))
            .is_ok_and(|contents| contents.contains(PROMPT_RULE_MARKER));
        if plugin_current && lsp_current && prompt_current {
            State::Current
        } else if !plugin_path.exists() && !lsp_current && !prompt_current {
            State::Missing
        } else {
            State::Repairable
        }
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".config").join("opencode").is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<std::path::PathBuf> {
        Some(opencode_config_path(home))
    }

    fn host_registration_paths(&self, home: &Path) -> Vec<std::path::PathBuf> {
        let profile_root = crate::automation::skill_targets::profile_root_for_agent_home(home);
        vec![
            opencode_config_path(home),
            opencode_prompt_path(home),
            crate::automation::memory_digest::digest_targets_path(&profile_root),
        ]
    }

    fn host_component_registration_paths(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        home: &Path,
    ) -> Vec<std::path::PathBuf> {
        use super::host_bundle_v2::HostBundleComponentV1;

        let mut paths = Vec::new();
        if components.contains(&HostBundleComponentV1::Core)
            || components.contains(&HostBundleComponentV1::ContextMcp)
        {
            paths.push(opencode_config_path(home));
        }
        if components.contains(&HostBundleComponentV1::Core) {
            let profile_root = crate::automation::skill_targets::profile_root_for_agent_home(home);
            paths.push(opencode_prompt_path(home));
            paths.push(crate::automation::memory_digest::digest_targets_path(
                &profile_root,
            ));
        }
        paths
    }

    fn activate_deployed_host_registration(&self, ctx: &InstallContext) -> Result<()> {
        install_mcp_server(&opencode_config_path(&ctx.home), &ctx.tracedecay_bin)
    }

    fn activate_deployed_host_component_registration(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        use super::host_bundle_v2::HostBundleComponentV1;

        let core = components.contains(&HostBundleComponentV1::Core);
        let mcp = components.contains(&HostBundleComponentV1::ContextMcp);
        install_registration_entries(
            &opencode_config_path(&ctx.home),
            &ctx.tracedecay_bin,
            mcp,
            core,
            false,
        )?;
        if core {
            let prompt = opencode_prompt_path(&ctx.home);
            install_prompt_rules(&prompt)?;
            super::install_managed_skill_prompt_index(
                &ctx.home,
                &prompt,
                crate::automation::skill_targets::SkillInstallTarget::OpenCode,
            )?;
        }
        Ok(())
    }

    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[super::host_bundle_v2::HostBundleComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        use super::host_bundle_v2::HostBundleComponentV1;

        let core = components.contains(&HostBundleComponentV1::Core);
        let mcp = components.contains(&HostBundleComponentV1::ContextMcp);
        remove_registration_entries(&opencode_config_path(&ctx.home), mcp, core, false)?;
        if core {
            let prompt = opencode_prompt_path(&ctx.home);
            super::remove_managed_skill_prompt_index(
                &ctx.home,
                &prompt,
                crate::automation::skill_targets::SkillInstallTarget::OpenCode,
            )?;
            uninstall_prompt_rules(&prompt);
        }
        Ok(())
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        let config_path = opencode_config_path(home);
        if !config_path.exists() {
            return false;
        }
        let json = super::load_json_file(&config_path);
        let mcp = json.get("mcp");
        mcp.and_then(|v| v.get("tracedecay")).is_some()
    }

    fn export_managed_skills(
        &self,
        home: &Path,
        profile_root: &Path,
    ) -> Result<Vec<crate::automation::skill_targets::SkillInstallSummary>> {
        let prompt_path = opencode_prompt_path(home);
        if !self.has_tracedecay(home) || !prompt_path.exists() {
            return Ok(Vec::new());
        }
        Ok(vec![
            crate::automation::skill_targets::install_managed_skills(
                profile_root,
                crate::automation::skill_targets::SkillInstallTarget::OpenCode,
                &prompt_path,
            )?,
        ])
    }

    fn export_managed_skills_local(
        &self,
        project_root: &Path,
        profile_root: &Path,
    ) -> Result<Vec<crate::automation::skill_targets::SkillInstallSummary>> {
        let agents_md = project_root.join("AGENTS.md");
        if !local_config_has_tracedecay(project_root) || !agents_md.exists() {
            return Ok(Vec::new());
        }
        Ok(vec![
            crate::automation::skill_targets::install_managed_skills(
                profile_root,
                crate::automation::skill_targets::SkillInstallTarget::OpenCode,
                &agents_md,
            )?,
        ])
    }
}

fn local_config_has_tracedecay(project_root: &Path) -> bool {
    let config_path = project_root.join("opencode.json");
    if !config_path.exists() {
        return false;
    }
    let json = super::load_json_file(&config_path);
    json.get("mcp")
        .and_then(|servers| servers.get("tracedecay"))
        .is_some()
}

// ---------------------------------------------------------------------------
// Config path resolution
// ---------------------------------------------------------------------------

/// Returns the path to opencode config (global).
/// Prefers `$HOME/.config/opencode/opencode.json`. Falls back to
/// `$XDG_CONFIG_HOME/opencode/opencode.json` only when the XDG path
/// is under `home` (so tests with temp-dir homes are never polluted by
/// the real user's environment).
fn opencode_config_path(home: &Path) -> std::path::PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let xdg_path = std::path::PathBuf::from(&xdg);
        if xdg_path.starts_with(home) {
            return xdg_path.join("opencode/opencode.json");
        }
    }
    home.join(".config/opencode/opencode.json")
}

/// Returns the path to the global AGENTS.md prompt file.
fn opencode_prompt_path(home: &Path) -> std::path::PathBuf {
    let modern = home.join(".config/opencode/AGENTS.md");
    if modern.exists() || home.join(".config/opencode").exists() {
        return modern;
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let xdg_path = std::path::PathBuf::from(&xdg);
        if xdg_path.starts_with(home) {
            let xdg_dir = xdg_path.join("opencode");
            if xdg_dir.exists() {
                return xdg_dir.join("AGENTS.md");
            }
        }
    }
    home.join("AGENTS.md")
}

fn opencode_plugin_path(home: &Path) -> std::path::PathBuf {
    opencode_config_path(home)
        .parent()
        .unwrap_or(home)
        .join("plugins/tracedecay.ts")
}

/// Rendered inventory of the managed `OpenCode` plugin files. This installer and
/// the receipt-backed first-party host-bundle catalog must produce
/// byte-identical files: the component-set transaction verifies installed
/// artifact digests after the compatibility registration adapter re-runs this
/// installer, so any rendering drift between the two writers fails installs
/// with `ArtifactContentMismatch` — and, before the convergent rollback rules,
/// wedged the shared component-set journal.
pub(crate) fn rendered_plugin_files(tracedecay_bin: &str) -> Result<Vec<(&'static str, String)>> {
    let encoded = serde_json::to_string(tracedecay_bin)?;
    Ok(vec![(
        OPENCODE_PLUGIN_RELATIVE,
        OPENCODE_PLUGIN_SOURCE.replace("\"__TRACEDECAY_BIN__\"", &encoded),
    )])
}

fn install_opencode_plugin(path: &Path, tracedecay_bin: &str) -> Result<()> {
    for (_, rendered) in rendered_plugin_files(tracedecay_bin)? {
        safe_write_text_file(path, &rendered, None)?;
    }
    Ok(())
}

fn remove_opencode_plugin(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let contents =
        std::fs::read_to_string(path).map_err(|error| crate::errors::TraceDecayError::Config {
            message: format!("failed to read {}: {error}", path.display()),
        })?;
    if !contents.contains(OPENCODE_PLUGIN_MARKER) {
        return Err(crate::errors::TraceDecayError::Config {
            message: format!(
                "refusing to remove non-TraceDecay plugin {}",
                path.display()
            ),
        });
    }
    std::fs::remove_file(path).map_err(|error| crate::errors::TraceDecayError::Config {
        message: format!("failed to remove {}: {error}", path.display()),
    })
}

// ---------------------------------------------------------------------------
// Install helpers
// ---------------------------------------------------------------------------

/// Register MCP server in opencode.json.
///
/// Safety: creates a `.bak` backup before writing and restores it on any
/// error. Uses strict JSON parsing so an existing file with invalid syntax
/// is never silently replaced with an empty object.
fn install_mcp_server(config_path: &Path, tracedecay_bin: &str) -> Result<()> {
    install_registration_entries(config_path, tracedecay_bin, true, true, true)
}

fn install_registration_entries(
    config_path: &Path,
    tracedecay_bin: &str,
    install_mcp: bool,
    install_lsp: bool,
    preserve_backup: bool,
) -> Result<()> {
    if !install_mcp && !install_lsp {
        return Ok(());
    }
    let backup = preserve_backup
        .then(|| backup_config_file(config_path))
        .transpose()?
        .flatten();
    let mut config = match load_json_file_strict(config_path) {
        Ok(v) => v,
        Err(e) => {
            if let Some(ref b) = backup {
                eprintln!("  Backup preserved at: {}", b.display());
            }
            return Err(e);
        }
    };

    let config_object = config
        .as_object_mut()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("{} must contain a JSON object", config_path.display()),
        })?;
    if install_mcp {
        let mcp = config_object
            .entry("mcp")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(|| TraceDecayError::Config {
                message: format!("{}.mcp must be a JSON object", config_path.display()),
            })?;
        mcp.insert(
            "tracedecay".to_string(),
            json!({
                "type": "local",
                "command": [tracedecay_bin, "serve"]
            }),
        );
    }
    if install_lsp {
        let retained_analyzer_owners = config_object
            .get("lsp")
            .and_then(serde_json::Value::as_object)
            .into_iter()
            .flat_map(|servers| servers.iter())
            .filter(|(name, registration)| {
                name.as_str() != "tracedecay"
                    && registration
                        .get("disabled")
                        .and_then(serde_json::Value::as_bool)
                        != Some(true)
            })
            .flat_map(|(name, registration)| {
                registration
                    .get("extensions")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .filter(|extension| TRACEDECAY_LSP_EXTENSIONS.contains(extension))
                    .map(move |extension| (extension.to_owned(), name.clone()))
            })
            .fold(
                std::collections::BTreeMap::<String, Vec<String>>::new(),
                |mut owners, (extension, owner)| {
                    owners.entry(extension).or_default().push(owner);
                    owners
                },
            );
        let lsp_value = config_object.entry("lsp").or_insert_with(|| json!({}));
        if lsp_value == &json!(true) {
            // OpenCode documents object-form `lsp` as retaining built-in servers
            // while allowing custom entries, so this preserves `lsp: true`.
            *lsp_value = json!({});
        }
        if lsp_value != &json!(false) {
            let lsp = lsp_value
                .as_object_mut()
                .ok_or_else(|| TraceDecayError::Config {
                    message: format!(
                        "{}.lsp must be a boolean or JSON object",
                        config_path.display()
                    ),
                })?;
            lsp.insert(
                "tracedecay".to_string(),
                json!({
                    "command": [tracedecay_bin, "lsp", "bridge", "--stdio"],
                    "extensions": TRACEDECAY_LSP_EXTENSIONS,
                    "env": {
                        "TRACEDECAY_LSP_BROKER_UPSTREAM": "0"
                    },
                    "initialization": {
                        "tracedecay": {
                            "brokerUpstream": false,
                            "duplicateAnalyzerAvoidance": true,
                            "analyzerOwnership": {
                                "mode": "projection_only",
                                "retainedByExtension": retained_analyzer_owners
                            }
                        }
                    }
                }),
            );
        }
    }

    safe_write_json_file(config_path, &config, backup.as_deref())?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Added tracedecay MCP server to {}",
        config_path.display()
    );
    Ok(())
}

/// Install-or-refresh prompt rules in AGENTS.md.
fn install_prompt_rules(prompt_path: &Path) -> Result<()> {
    let block = super::prompt_rules::standard_prompt_rules(
        PROMPT_RULE_MARKER,
        &PromptRulesOptions {
            extra_paragraphs: &[],
        },
    );
    super::prompt_rules::reconcile_prompt_rules(prompt_path, PROMPT_RULE_MARKER, &block)
}

// ---------------------------------------------------------------------------
// Uninstall helpers
// ---------------------------------------------------------------------------

/// Remove MCP server from opencode.json.
fn uninstall_mcp_server(config_path: &Path) {
    if let Err(error) = remove_registration_entries(config_path, true, true, true) {
        eprintln!("  Could not remove OpenCode registration: {error}");
    }
}

fn remove_registration_entries(
    config_path: &Path,
    remove_mcp: bool,
    remove_lsp: bool,
    preserve_backup: bool,
) -> Result<()> {
    if !config_path.exists() {
        return Ok(());
    }
    let mut config = load_json_file_strict(config_path)?;
    let removed_mcp = remove_mcp
        && config
            .get_mut("mcp")
            .and_then(|value| value.as_object_mut())
            .is_some_and(|mcp| mcp.remove("tracedecay").is_some());
    if removed_mcp
        && config
            .get("mcp")
            .and_then(serde_json::Value::as_object)
            .is_some_and(serde_json::Map::is_empty)
    {
        config.as_object_mut().map(|object| object.remove("mcp"));
    }
    let removed_lsp = remove_lsp
        && config
            .get_mut("lsp")
            .and_then(|value| value.as_object_mut())
            .is_some_and(|lsp| lsp.remove("tracedecay").is_some());
    if removed_lsp
        && config
            .get("lsp")
            .and_then(serde_json::Value::as_object)
            .is_some_and(serde_json::Map::is_empty)
    {
        config.as_object_mut().map(|object| object.remove("lsp"));
    }
    if !removed_mcp && !removed_lsp {
        eprintln!(
            "  No tracedecay MCP/LSP registration in {}, skipping",
            config_path.display()
        );
        return Ok(());
    }
    let backup = preserve_backup
        .then(|| backup_config_file(config_path))
        .transpose()?
        .flatten();
    let is_empty = config.as_object().is_some_and(serde_json::Map::is_empty);
    if is_empty {
        std::fs::remove_file(config_path).map_err(|error| TraceDecayError::Config {
            message: format!("failed to remove {}: {error}", config_path.display()),
        })?;
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed {} (was empty)",
            config_path.display()
        );
    } else {
        safe_write_json_file(config_path, &config, backup.as_deref())?;
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed tracedecay MCP server from {}",
            config_path.display()
        );
    }
    Ok(())
}

/// Remove tracedecay rules from AGENTS.md.
fn uninstall_prompt_rules(prompt_path: &Path) {
    super::prompt_rules::remove_prompt_rules(prompt_path, PROMPT_RULE_MARKER);
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

/// Check opencode.json has tracedecay registered.
fn doctor_check_config(dc: &mut DoctorCounters, home: &Path) {
    let config_path = opencode_config_path(home);
    if !config_path.exists() {
        dc.warn(&format!(
            "{} not found — run `tracedecay install --agent opencode` if you use OpenCode",
            config_path.display()
        ));
        return;
    }

    let config = load_json_file(&config_path);
    let mcp_entry = &config["mcp"]["tracedecay"];
    if !mcp_entry.is_object() {
        dc.fail(&format!(
            "MCP server NOT registered in {} — run `tracedecay install --agent opencode`",
            config_path.display()
        ));
        return;
    }
    dc.pass(&format!(
        "MCP server registered in {}",
        config_path.display()
    ));

    let command = mcp_entry["command"].as_array();
    let has_serve = command.is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some("serve")));
    if has_serve {
        dc.pass("MCP server args include \"serve\"");
    } else {
        dc.fail("MCP server args missing \"serve\" — run `tracedecay install --agent opencode`");
    }
    let lsp = &config["lsp"]["tracedecay"];
    let lsp_command = lsp["command"].as_array();
    let has_bridge = lsp_command.is_some_and(|args| {
        ["lsp", "bridge", "--stdio"]
            .iter()
            .all(|expected| args.iter().any(|arg| arg.as_str() == Some(expected)))
    });
    let has_extensions = lsp["extensions"]
        .as_array()
        .is_some_and(|extensions| !extensions.is_empty());
    let duplicate_avoidance =
        lsp["initialization"]["tracedecay"]["duplicateAnalyzerAvoidance"].as_bool() == Some(true);
    if has_bridge && has_extensions && duplicate_avoidance {
        dc.pass("custom TraceDecay LSP bridge configured with duplicate-analyzer avoidance");
    } else {
        dc.fail(
            "custom TraceDecay LSP config is stale — run `tracedecay install --agent opencode`",
        );
    }
}

/// Check AGENTS.md contains tracedecay rules.
fn doctor_check_prompt(dc: &mut DoctorCounters, home: &Path) {
    let prompt_path = opencode_prompt_path(home);
    if prompt_path.exists() {
        let has_rules = std::fs::read_to_string(&prompt_path)
            .unwrap_or_default()
            .contains("tracedecay");
        if has_rules {
            dc.pass("AGENTS.md contains tracedecay rules");
        } else {
            dc.fail(
                "AGENTS.md missing tracedecay rules — run `tracedecay install --agent opencode`",
            );
        }
    } else {
        dc.warn("AGENTS.md does not exist");
    }
}

fn doctor_check_plugin(dc: &mut DoctorCounters, home: &Path) {
    let plugin_path = opencode_plugin_path(home);
    let installed = std::fs::read_to_string(&plugin_path)
        .ok()
        .is_some_and(|contents| contents.contains(OPENCODE_PLUGIN_MARKER));
    if installed {
        dc.pass(&format!(
            "native edit/idle plugin registered in {}",
            plugin_path.display()
        ));
    } else {
        dc.fail(&format!(
            "native edit/idle plugin missing from {} — run `tracedecay install --agent opencode`",
            plugin_path.display()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsp_registration_records_retained_analyzers_per_extension() {
        let home = tempfile::tempdir().unwrap();
        let config_path = home.path().join("opencode.json");
        std::fs::write(
            &config_path,
            serde_json::to_vec_pretty(&json!({
                "lsp": {
                    "rust-analyzer": {
                        "command": ["rust-analyzer"],
                        "extensions": [".rs"]
                    },
                    "disabled-typescript": {
                        "disabled": true,
                        "extensions": [".ts"]
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        install_mcp_server(&config_path, "/usr/bin/tracedecay").unwrap();

        let config = load_json_file_strict(&config_path).unwrap();
        assert_eq!(
            config["lsp"]["tracedecay"]["initialization"]["tracedecay"]["analyzerOwnership"]["mode"],
            "projection_only"
        );
        assert_eq!(
            config["lsp"]["tracedecay"]["initialization"]["tracedecay"]["analyzerOwnership"]["retainedByExtension"]
                [".rs"],
            json!(["rust-analyzer"])
        );
        assert!(
            config["lsp"]["tracedecay"]["initialization"]["tracedecay"]
                ["analyzerOwnership"]["retainedByExtension"]
                .get(".ts")
                .is_none()
        );
        assert_eq!(
            config["lsp"]["rust-analyzer"]["command"],
            json!(["rust-analyzer"])
        );
    }
}
