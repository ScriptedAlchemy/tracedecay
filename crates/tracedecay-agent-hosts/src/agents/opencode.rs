//! `OpenCode` (V2) agent integration.
//!
//! Handles `TraceDecay`'s MCP registration in `OpenCode`'s config, native
//! TypeScript plugin deployment, and prompt/managed-skill rules. `OpenCode`
//! uses interactive runtime approval rather than declarative tool
//! permissions, and V2 accepts but never runs `lsp` configuration, so no LSP
//! bridge is registered.
//!
//! Unlike the Claude and Kiro integrations, no half of this lifecycle is driven
//! through the host's own CLI: the plugin deployment already *is* `OpenCode`'s
//! own discovery contract, `opencode mcp add` is interactive, and the prompt
//! registration has no host command at all. `plugin_cli` is the decision
//! record, including why `opencode plugin add` does not apply to a local
//! plugin file.

mod plugin_cli;

use std::path::{Path, PathBuf};
use tracedecay_runtime_core::config::ProfileRoot;

use serde_json::{Value, json};

use tracedecay_domain::errors::{Result, TraceDecayError};

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, JsonConfigDialect,
    TextFileMutation, load_json_file, safe_write_text_file, update_text_file_transactionally,
};

use super::mcp_registration::McpRegistrationOutcome;
use super::prompt_rules::{PROMPT_RULE_MARKER, PromptRulesOptions};

pub struct OpenCodeIntegration;

const OPENCODE_PLUGIN_SOURCE: &str = include_str!("../../../../plugin/opencode/tracedecay.ts");
const OPENCODE_PLUGIN_MARKER: &str = "TraceDecayPlugin";
/// Deployed path of the managed plugin relative to the `OpenCode` config dir.
///
/// Load-bearing, not cosmetic: `OpenCode` scans `{plugin,plugins}/*.{ts,js}`
/// one level deep in each config directory, so a file here is loaded with no
/// registration step, while a sub-directory or another extension would leave a
/// configuration that still validates and a plugin that never loads. Guarded
/// by [`plugin_cli::is_host_discovered_plugin_path`].
pub(crate) const OPENCODE_PLUGIN_RELATIVE: &str = "plugins/tracedecay.ts";
/// Native V2 location of the managed MCP server inside `opencode.json`.
const MCP_SERVER_POINTER: &str = "/mcp/servers/tracedecay";
/// Where the V1 installer registered the same server; removed on the next
/// install so one config never names the server twice.
const LEGACY_MCP_SERVER_KEY: &str = "tracedecay";

impl AgentIntegration for OpenCodeIntegration {
    fn name(&self) -> &'static str {
        "OpenCode"
    }

    fn id(&self) -> &'static str {
        "opencode"
    }

    fn supports_local_install(&self) -> bool {
        true
    }

    #[hotpath::measure(label = "hosts.agent.opencode.project_install")]
    fn activate_project_host_component_registration(
        &self,
        _components: &[super::host_bundle::HostComponentV1],
        ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        let mcp_path = project_path.join("opencode.json");
        let plugin_path = project_path.join(".opencode/plugins/tracedecay.ts");
        let agents_md = project_path.join("AGENTS.md");
        super::ensure_project_local_safe_paths(
            project_path,
            [
                mcp_path.as_path(),
                plugin_path.as_path(),
                agents_md.as_path(),
            ],
        )?;
        install_mcp_server(&mcp_path, &ctx.tracedecay_bin)?;
        install_opencode_plugin(&plugin_path, &ctx.tracedecay_bin)?;
        install_prompt_rules(&agents_md)?;
        super::install_managed_skill_prompt_index(
            ctx.profile.data_dir(),
            &agents_md,
            tracedecay_automation_runtime::automation::skill_targets::SkillInstallTarget::OpenCode,
        )
    }

    fn project_host_component_registration_paths(
        &self,
        _components: &[super::host_bundle::HostComponentV1],
        _home: &Path,
        _profile_root: &Path,
        project_path: &Path,
    ) -> Result<Vec<PathBuf>> {
        Ok(vec![
            project_path.join("opencode.json"),
            project_path.join(".opencode/plugins/tracedecay.ts"),
            project_path.join("AGENTS.md"),
        ])
    }

    fn deactivate_project_host_component_registration(
        &self,
        _components: &[super::host_bundle::HostComponentV1],
        _ctx: &InstallContext,
        project_path: &Path,
    ) -> Result<()> {
        uninstall_mcp_server(&project_path.join("opencode.json"))?;
        remove_opencode_plugin(&project_path.join(".opencode/plugins/tracedecay.ts"))?;
        let agents_md = project_path.join("AGENTS.md");
        super::remove_managed_skill_prompt_index(
            &agents_md,
            tracedecay_automation_runtime::automation::skill_targets::SkillInstallTarget::OpenCode,
        )?;
        uninstall_prompt_rules(&agents_md)?;
        Ok(())
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        dc.section("OpenCode integration");
        doctor_check_config(dc, &ctx.home, &ctx.profile);
        doctor_check_prompt(dc, &ctx.home, &ctx.profile);
        doctor_check_plugin(dc, &ctx.home, &ctx.profile);
        super::doctor_check_managed_skill_prompt_indexes(
            dc,
            ctx.profile.data_dir(),
            &[
                opencode_prompt_path(&ctx.home, &ctx.profile),
                ctx.project_path.join("AGENTS.md"),
            ],
            tracedecay_automation_runtime::automation::skill_targets::SkillInstallTarget::OpenCode,
        );
    }

    fn host_component_registration(
        &self,
        component: super::host_bundle::HostComponentV1,
        ctx: &HealthcheckContext,
    ) -> super::host_bundle::HostBundleRegistrationStateV1 {
        use super::host_bundle::{HostBundleRegistrationStateV1 as State, HostComponentV1};

        let config_path = opencode_config_path(&ctx.home, &ctx.profile);
        let config = if matches!(
            component,
            HostComponentV1::Core | HostComponentV1::ContextMcp
        ) {
            match std::fs::read(&config_path) {
                Ok(config_bytes) => {
                    match serde_json::from_slice::<serde_json::Value>(&config_bytes) {
                        Ok(config) => Some(config),
                        Err(error) if component == HostComponentV1::Core => {
                            tracing::warn!(path = %config_path.display(), %error, "Cannot inspect retired OpenCode LSP registration; Core artifacts remain independent of MCP config");
                            None
                        }
                        Err(_) => return State::Corrupt,
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(_) => return State::Corrupt,
            }
        } else {
            None
        };
        let mcp_current = config
            .as_ref()
            .and_then(|config| config.pointer(&format!("{MCP_SERVER_POINTER}/command")))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|args| args.iter().any(|arg| arg.as_str() == Some("serve")));
        if component == HostComponentV1::ContextMcp {
            return if mcp_current {
                State::Current
            } else {
                State::Missing
            };
        }
        if component == HostComponentV1::OperatorMcp {
            return State::Missing;
        }
        let config_root = config_path.parent().unwrap_or(&ctx.home);
        if component == HostComponentV1::Agent {
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
        let plugin_path = opencode_plugin_path(&ctx.home, &ctx.profile);
        let plugin_current = std::fs::read_to_string(&plugin_path)
            .is_ok_and(|contents| contents.contains(OPENCODE_PLUGIN_MARKER));
        let prompt_current = std::fs::read_to_string(opencode_prompt_path(&ctx.home, &ctx.profile))
            .is_ok_and(|contents| contents.contains(PROMPT_RULE_MARKER));
        let legacy_lsp = config
            .as_ref()
            .is_some_and(|config| config.pointer("/lsp/tracedecay").is_some());
        if plugin_current && prompt_current && !legacy_lsp {
            State::Current
        } else if !plugin_path.exists() && !prompt_current && !legacy_lsp {
            State::Missing
        } else {
            State::Repairable
        }
    }

    fn is_detected(&self, home: &Path) -> bool {
        home.join(".config").join("opencode").is_dir()
    }

    fn primary_config_path(
        &self,
        home: &Path,
        profile: &ProfileRoot,
    ) -> Option<std::path::PathBuf> {
        Some(opencode_config_path(home, profile))
    }

    fn host_registration_paths(
        &self,
        home: &Path,
        profile: &ProfileRoot,
    ) -> Vec<std::path::PathBuf> {
        vec![
            opencode_config_path(home, profile),
            opencode_prompt_path(home, profile),
        ]
    }

    fn host_component_registration_paths(
        &self,
        components: &[super::host_bundle::HostComponentV1],
        home: &Path,
        profile: &ProfileRoot,
    ) -> Vec<std::path::PathBuf> {
        use super::host_bundle::HostComponentV1;

        let mut paths = Vec::new();
        if components.contains(&HostComponentV1::ContextMcp)
            || components.contains(&HostComponentV1::Core)
        {
            paths.push(opencode_config_path(home, profile));
        }
        if components.contains(&HostComponentV1::Core) {
            paths.push(opencode_prompt_path(home, profile));
        }
        paths.extend(external_opencode_asset_paths(home, components, profile));
        paths
    }

    fn activate_deployed_host_registration(&self, ctx: &InstallContext) -> Result<()> {
        install_mcp_server(
            &opencode_config_path(&ctx.home, &ctx.profile),
            &ctx.tracedecay_bin,
        )
    }

    fn activate_deployed_host_component_registration(
        &self,
        components: &[super::host_bundle::HostComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        use super::host_bundle::HostComponentV1;

        let core = components.contains(&HostComponentV1::Core);
        let context_mcp = components.contains(&HostComponentV1::ContextMcp);
        if context_mcp {
            install_mcp_server(
                &opencode_config_path(&ctx.home, &ctx.profile),
                &ctx.tracedecay_bin,
            )?;
        } else if core {
            remove_legacy_lsp_registration(&opencode_config_path(&ctx.home, &ctx.profile))?;
        }
        if core {
            let prompt = opencode_prompt_path(&ctx.home, &ctx.profile);
            install_prompt_rules(&prompt)?;
            super::install_managed_skill_prompt_index(
                ctx.profile.data_dir(),
                &prompt,
                tracedecay_automation_runtime::automation::skill_targets::SkillInstallTarget::OpenCode,
            )?;
        }
        mirror_external_opencode_assets(&ctx.home, components, &ctx.profile)?;
        Ok(())
    }

    fn deactivate_deployed_host_component_registration(
        &self,
        components: &[super::host_bundle::HostComponentV1],
        ctx: &InstallContext,
    ) -> Result<()> {
        use super::host_bundle::HostComponentV1;

        let core = components.contains(&HostComponentV1::Core);
        let context_mcp = components.contains(&HostComponentV1::ContextMcp);
        if context_mcp {
            uninstall_mcp_server(&opencode_config_path(&ctx.home, &ctx.profile))?;
        } else if core {
            remove_legacy_lsp_registration(&opencode_config_path(&ctx.home, &ctx.profile))?;
        }
        if core {
            let prompt = opencode_prompt_path(&ctx.home, &ctx.profile);
            super::remove_managed_skill_prompt_index(
                &prompt,
                tracedecay_automation_runtime::automation::skill_targets::SkillInstallTarget::OpenCode,
            )?;
            uninstall_prompt_rules(&prompt)?;
        }
        remove_external_opencode_assets(&ctx.home, components, &ctx.profile)?;
        Ok(())
    }

    fn has_tracedecay(&self, home: &Path, profile: &ProfileRoot) -> bool {
        let config_path = opencode_config_path(home, profile);
        if !config_path.exists() {
            return false;
        }
        config_has_tracedecay(&super::load_json_file(&config_path))
    }

    fn detected_host_surface(
        &self,
        home: &Path,
        profile: &ProfileRoot,
    ) -> Option<std::path::PathBuf> {
        let config_path = opencode_config_path(home, profile);
        config_path.exists().then_some(config_path)
    }

    fn export_managed_skills(
        &self,
        home: &Path,
        profile: &ProfileRoot,
    ) -> Result<Vec<tracedecay_automation_runtime::automation::skill_targets::SkillInstallSummary>>
    {
        let profile_root = profile.data_dir();
        let prompt_path = opencode_prompt_path(home, profile);
        if !self.has_tracedecay(home, profile) || !prompt_path.exists() {
            return Ok(Vec::new());
        }
        Ok(vec![
            tracedecay_automation_runtime::automation::skill_targets::install_managed_skills(
                &crate::host_io(),
                profile_root,
                tracedecay_automation_runtime::automation::skill_targets::SkillInstallTarget::OpenCode,
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
        let agents_md = project_root.join("AGENTS.md");
        if !local_config_has_tracedecay(project_root) || !agents_md.exists() {
            return Ok(Vec::new());
        }
        Ok(vec![
            tracedecay_automation_runtime::automation::skill_targets::install_managed_skills(
                &crate::host_io(),
                profile_root,
                tracedecay_automation_runtime::automation::skill_targets::SkillInstallTarget::OpenCode,
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
    config_has_tracedecay(&super::load_json_file(&config_path))
}

fn config_has_tracedecay(config: &Value) -> bool {
    config.pointer(MCP_SERVER_POINTER).is_some()
        || config
            .pointer(&format!("/mcp/{LEGACY_MCP_SERVER_KEY}"))
            .is_some()
}

// ---------------------------------------------------------------------------
// Config path resolution
// ---------------------------------------------------------------------------

/// Honors the profile's absolute `$XDG_CONFIG_HOME` only inside `home`.
///
/// A caller that names a root, a per-home sweep, a managed-skill export
/// destination scan, a test sandbox, must stay inside the root it named, so
/// an `$XDG_CONFIG_HOME` outside `home` never redirects OpenCode to another
/// user's `~/.config/opencode`. This is the same rule every other host-home
/// override follows (`host_home_override`).
fn opencode_config_path(home: &Path, profile: &ProfileRoot) -> std::path::PathBuf {
    opencode_config_path_for(home, profile_xdg_config_home(home, profile).as_deref())
}

fn profile_xdg_config_home(home: &Path, profile: &ProfileRoot) -> Option<std::ffi::OsString> {
    xdg_config_home_inside(
        home,
        profile
            .xdg_config_home()
            .map(|xdg| xdg.as_os_str().to_owned()),
    )
}

fn xdg_config_home_inside(
    home: &Path,
    xdg: Option<std::ffi::OsString>,
) -> Option<std::ffi::OsString> {
    xdg.filter(|xdg| Path::new(xdg).starts_with(home))
}

fn opencode_config_path_for(home: &Path, xdg: Option<&std::ffi::OsStr>) -> std::path::PathBuf {
    xdg.map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("opencode/opencode.json")
}

/// Resolution never depends on filesystem state. `~/.config/opencode` is
/// created by TraceDecay's own managed artifacts, which a component-set
/// transaction writes between the moment the registration authority confirms
/// a revision and the moment it applies; a path keyed on what exists would
/// move the hashed registration path list mid-transaction and roll every apply
/// back with `StalePreview`. The write path creates the parent on demand.
pub(super) fn opencode_prompt_path(home: &Path, profile: &ProfileRoot) -> std::path::PathBuf {
    profile_xdg_config_home(home, profile)
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("opencode/AGENTS.md")
}

fn opencode_asset_relative_paths(
    components: &[super::host_bundle::HostComponentV1],
) -> Vec<std::path::PathBuf> {
    use super::host_bundle::HostComponentV1;

    let mut paths = Vec::new();
    if components.contains(&HostComponentV1::Core)
        && let Ok(files) = rendered_plugin_files("tracedecay")
    {
        paths.extend(
            files
                .into_iter()
                .map(|(relative, _)| std::path::PathBuf::from(relative)),
        );
    }
    if components.contains(&HostComponentV1::Agent) {
        paths.extend(
            super::plugin_bundle::opencode_agent_files()
                .into_iter()
                .map(|(relative, _)| std::path::PathBuf::from(relative)),
        );
    }
    if components.contains(&HostComponentV1::ContextMcp) {
        paths.extend([
            std::path::PathBuf::from("plugins/tracedecay-mcp.ts"),
            std::path::PathBuf::from("tracedecay/opencode.registration.json"),
        ]);
    }
    paths.sort();
    paths.dedup();
    paths
}

fn external_opencode_asset_paths(
    home: &Path,
    components: &[super::host_bundle::HostComponentV1],
    profile: &ProfileRoot,
) -> Vec<std::path::PathBuf> {
    let root = opencode_config_path(home, profile)
        .parent()
        .unwrap_or(home)
        .to_path_buf();
    external_opencode_asset_paths_for(home, &root, components)
}

fn external_opencode_asset_paths_for(
    home: &Path,
    root: &Path,
    components: &[super::host_bundle::HostComponentV1],
) -> Vec<std::path::PathBuf> {
    if root == home.join(".config/opencode") {
        return Vec::new();
    }
    opencode_asset_relative_paths(components)
        .into_iter()
        .map(|relative| root.join(relative))
        .collect()
}

fn mirror_external_opencode_assets(
    home: &Path,
    components: &[super::host_bundle::HostComponentV1],
    profile: &ProfileRoot,
) -> Result<()> {
    let root = opencode_config_path(home, profile)
        .parent()
        .unwrap_or(home)
        .to_path_buf();
    mirror_external_opencode_assets_to(home, &root, components)
}

fn mirror_external_opencode_assets_to(
    home: &Path,
    root: &Path,
    components: &[super::host_bundle::HostComponentV1],
) -> Result<()> {
    let relative_paths = opencode_asset_relative_paths(components);
    let destinations = external_opencode_asset_paths_for(home, root, components);
    for (relative, destination) in relative_paths.iter().zip(destinations) {
        let source = home.join(".config/opencode").join(relative);
        let bytes = std::fs::read(&source).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to read deployed OpenCode asset {}: {error}",
                source.display()
            ),
        })?;
        super::safe_write_bytes_file(&destination, &bytes)?;
    }
    Ok(())
}

fn remove_external_opencode_assets(
    home: &Path,
    components: &[super::host_bundle::HostComponentV1],
    profile: &ProfileRoot,
) -> Result<()> {
    for path in external_opencode_asset_paths(home, components, profile) {
        match super::safe_remove_host_file(&path) {
            Ok(()) => tracedecay_private_fs::framed_log::sync_parent_directory(
                &path,
                tracedecay_private_fs::framed_log::DirectorySyncPolicy::TolerateUnsupported,
            )
            .map_err(|error| TraceDecayError::Config {
                message: format!("failed to durably remove {}: {error}", path.display()),
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(TraceDecayError::Config {
                    message: format!("failed to remove {}: {error}", path.display()),
                });
            }
        }
    }
    Ok(())
}

fn opencode_plugin_path(home: &Path, profile: &ProfileRoot) -> std::path::PathBuf {
    opencode_config_path(home, profile)
        .parent()
        .unwrap_or(home)
        .join("plugins/tracedecay.ts")
}

/// Rendered inventory of the managed `OpenCode` plugin files used by the
/// receipt-backed first-party catalog and explicit artifact refresh.
pub(crate) fn rendered_plugin_files(tracedecay_bin: &str) -> Result<Vec<(&'static str, String)>> {
    let encoded = serde_json::to_string(tracedecay_bin)?;
    let rendered = OPENCODE_PLUGIN_SOURCE.replace(
        &format!("\"{}\"", super::plugin_bundle::TRACEDECAY_BIN_PLACEHOLDER),
        &encoded,
    );
    super::plugin_bundle::reject_unresolved_placeholders(&rendered, "OpenCode plugin")?;
    Ok(vec![(OPENCODE_PLUGIN_RELATIVE, rendered)])
}

/// Deploy the managed plugin to a path `OpenCode`'s own loader discovers.
///
/// The write is the whole registration; `opencode plugin <module>` is
/// deliberately not driven afterwards, because it would add a *second* plugin
/// origin beside this file rather than replace it (see [`plugin_cli`]). The
/// destination is checked rather than assumed so a future refactor cannot
/// quietly deploy where the host never scans.
#[hotpath::measure(label = "hosts.agent.opencode.plugin_install")]
fn install_opencode_plugin(path: &Path, tracedecay_bin: &str) -> Result<()> {
    if !plugin_cli::is_host_discovered_plugin_path(path) {
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing to deploy the OpenCode plugin to {}: OpenCode only loads \
                 `{{plugin,plugins}}/*.{{ts,js}}` from a config directory, so the plugin \
                 would never be loaded there",
                path.display()
            ),
        });
    }
    for (_, rendered) in rendered_plugin_files(tracedecay_bin)? {
        safe_write_text_file(path, &rendered)?;
    }
    Ok(())
}

fn remove_opencode_plugin(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let contents = std::fs::read_to_string(path).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("failed to read {}: {error}", path.display()),
        }
    })?;
    if !contents.contains(OPENCODE_PLUGIN_MARKER) {
        return Err(tracedecay_domain::errors::TraceDecayError::Config {
            message: format!(
                "refusing to remove non-TraceDecay plugin {}",
                path.display()
            ),
        });
    }
    super::safe_remove_host_file(path).map_err(|error| {
        tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("failed to remove {}: {error}", path.display()),
        }
    })
}

// ---------------------------------------------------------------------------
// Install helpers
// ---------------------------------------------------------------------------

/// Merge TraceDecay's MCP registration into `opencode.json`.
///
/// Stays TraceDecay-written: `opencode mcp add [name]` exists but is an
/// interactive wizard with no non-interactive flags for the server type,
/// command, or arguments, so an unattended lifecycle cannot drive it. The key
/// is documented, operator-editable configuration rather than host-private
/// state, so writing it is not the emulation the host-capability doctrine
/// forbids. Strict JSON parsing means an existing file with invalid syntax is
/// never silently replaced with an empty object.
///
/// `plugins` is the one key here that *is* owned by a host command TraceDecay
/// declines to drive, so forging its effect is refused on both the install and
/// uninstall paths, see
/// [`plugin_cli::ensure_host_owned_plugin_registration_untouched`].
#[hotpath::measure(label = "hosts.agent.opencode.registration_install")]
fn install_mcp_server(config_path: &Path, tracedecay_bin: &str) -> Result<()> {
    let outcome = update_text_file_transactionally(config_path, |existing: &str| {
        let before = JsonConfigDialect::Json.parse_for_edit(config_path, existing)?;
        let config = merge_mcp_registration(config_path, before.clone(), tracedecay_bin)?;
        if config == before {
            return Ok((
                McpRegistrationOutcome::Unchanged,
                TextFileMutation::Unchanged,
            ));
        }
        let outcome = if before.pointer(MCP_SERVER_POINTER).is_some() {
            McpRegistrationOutcome::Updated
        } else {
            McpRegistrationOutcome::Added
        };
        Ok((
            outcome,
            JsonConfigDialect::Json.mutation(config_path, existing, config)?,
        ))
    })?;
    outcome.report(config_path);
    Ok(())
}

/// Merge TraceDecay's registration into the config parsed from the bytes
/// observed under the write lock, returning the replacement value. A V1-era
/// `mcp.tracedecay` entry is folded into the native `mcp.servers` map, while
/// the retired `lsp.tracedecay` registration is removed.
fn merge_mcp_registration(
    config_path: &Path,
    mut config: serde_json::Value,
    tracedecay_bin: &str,
) -> Result<serde_json::Value> {
    // Snapshot the host-recorded plugin registration before touching anything,
    // so the write below can be proven not to have created, altered, or
    // dropped the key `opencode plugin` owns.
    let host_plugin_before = plugin_cli::host_owned_plugin_registration(&config);

    let config_object = config
        .as_object_mut()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("{} must contain a JSON object", config_path.display()),
        })?;
    if let Some(lsp) = config_object
        .get_mut("lsp")
        .and_then(serde_json::Value::as_object_mut)
    {
        lsp.remove("tracedecay");
    }
    let mcp = config_object
        .entry("mcp")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("{}.mcp must be a JSON object", config_path.display()),
        })?;
    mcp.remove(LEGACY_MCP_SERVER_KEY);
    let servers = mcp
        .entry("servers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "{}.mcp.servers must be a JSON object",
                config_path.display()
            ),
        })?;
    servers.insert(
        "tracedecay".to_string(),
        json!({
            "type": "local",
            "command": [tracedecay_bin, "serve"]
        }),
    );

    plugin_cli::ensure_host_owned_plugin_registration_untouched(
        host_plugin_before.as_ref(),
        &config,
        config_path,
    )?;
    Ok(config)
}

/// Install-or-refresh prompt rules in AGENTS.md.
///
/// Stays TraceDecay-written: `OpenCode` has no command that edits instruction
/// files, and `AGENTS.md` is operator-editable Markdown discovered by
/// convention, no host-owned state to emulate. The block is marker-delimited
/// so a refresh replaces exactly what TraceDecay wrote.
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

/// Outcome of the uninstall transform, reported after publication.
enum OpenCodeRegistrationRemoval {
    NoEntry,
    RemovedFile,
    Rewritten,
}

/// Remove TraceDecay's MCP server and retired custom-LSP entry from
/// `opencode.json`.
fn uninstall_mcp_server(config_path: &Path) -> Result<()> {
    if !config_path.exists() {
        return Ok(());
    }
    let outcome = update_text_file_transactionally(config_path, |existing: &str| {
        strip_registration_entries(config_path, existing, true, true)
    })?;
    match outcome {
        OpenCodeRegistrationRemoval::NoEntry => {
            eprintln!(
                "  No tracedecay MCP registration in {}, skipping",
                config_path.display()
            );
        }
        OpenCodeRegistrationRemoval::RemovedFile => {
            eprintln!(
                "\x1b[32m✔\x1b[0m Removed {} (was empty)",
                config_path.display()
            );
        }
        OpenCodeRegistrationRemoval::Rewritten => {
            eprintln!(
                "\x1b[32m✔\x1b[0m Removed tracedecay MCP server from {}",
                config_path.display()
            );
        }
    }
    Ok(())
}

/// Remove only the retired custom-LSP entry during an explicit Core lifecycle.
///
/// Core stages `opencode.json` only to remove the key prior releases wrote.
/// Unparseable operator config is left untouched; MCP lifecycle edits remain strict.
fn remove_legacy_lsp_registration(config_path: &Path) -> Result<()> {
    if !config_path.exists() {
        return Ok(());
    }
    update_text_file_transactionally(config_path, |existing: &str| {
        if let Err(error) = JsonConfigDialect::Json.parse_for_edit(config_path, existing) {
            tracing::warn!(path = %config_path.display(), %error, "Skipped retired OpenCode LSP cleanup; operator config is unparseable");
            return Ok(((), TextFileMutation::Unchanged));
        }
        strip_registration_entries(config_path, existing, false, true)
            .map(|(_, mutation)| ((), mutation))
    })?;
    Ok(())
}

/// Strip TraceDecay's selected native or V1-era registrations from the config
/// bytes observed under the write lock, deciding between a rewrite and
/// removal of an emptied file. Containers the install created are pruned by
/// the creation ledger when they empty.
fn strip_registration_entries(
    config_path: &Path,
    existing: &str,
    remove_mcp: bool,
    remove_lsp: bool,
) -> Result<(OpenCodeRegistrationRemoval, TextFileMutation)> {
    let mut config = JsonConfigDialect::Json.parse_for_edit(config_path, existing)?;
    // Uninstall drops only what TraceDecay wrote. A plugin registration the
    // host recorded through `opencode plugin add` is not ours to remove.
    let host_plugin_before = plugin_cli::host_owned_plugin_registration(&config);
    let removed_legacy_mcp = remove_mcp
        && config
            .get_mut("mcp")
            .and_then(serde_json::Value::as_object_mut)
            .is_some_and(|mcp| mcp.remove(LEGACY_MCP_SERVER_KEY).is_some());
    let removed_native_mcp = remove_mcp
        && config
            .get_mut("mcp")
            .and_then(serde_json::Value::as_object_mut)
            .and_then(|mcp| mcp.get_mut("servers"))
            .and_then(serde_json::Value::as_object_mut)
            .is_some_and(|servers| servers.remove("tracedecay").is_some());
    let removed_lsp = remove_lsp
        && config
            .get_mut("lsp")
            .and_then(serde_json::Value::as_object_mut)
            .is_some_and(|lsp| lsp.remove("tracedecay").is_some());
    if !removed_legacy_mcp && !removed_native_mcp && !removed_lsp {
        return Ok((
            OpenCodeRegistrationRemoval::NoEntry,
            TextFileMutation::Unchanged,
        ));
    }
    plugin_cli::ensure_host_owned_plugin_registration_untouched(
        host_plugin_before.as_ref(),
        &config,
        config_path,
    )?;
    let mutation = JsonConfigDialect::Json.mutation(config_path, existing, config)?;
    let removal = match mutation {
        TextFileMutation::Remove => OpenCodeRegistrationRemoval::RemovedFile,
        TextFileMutation::Unchanged | TextFileMutation::Write(_) => {
            OpenCodeRegistrationRemoval::Rewritten
        }
    };
    Ok((removal, mutation))
}

fn uninstall_prompt_rules(prompt_path: &Path) -> Result<()> {
    super::prompt_rules::remove_standard_prompt_rules(prompt_path)
}

// ---------------------------------------------------------------------------
// Healthcheck helpers
// ---------------------------------------------------------------------------

fn doctor_check_config(dc: &mut DoctorCounters, home: &Path, profile: &ProfileRoot) {
    let config_path = opencode_config_path(home, profile);
    if !config_path.exists() {
        dc.warn(&format!(
            "{} not found, run `tracedecay install --agent opencode` if you use OpenCode",
            config_path.display()
        ));
        return;
    }

    let config = load_json_file(&config_path);
    if config
        .pointer(&format!("/mcp/{LEGACY_MCP_SERVER_KEY}"))
        .is_some()
    {
        dc.fail(
            "V1-era `mcp.tracedecay` registration still present, run `tracedecay install --agent opencode`",
        );
    }
    if config.pointer("/lsp/tracedecay").is_some() {
        dc.fail(
            "retired `lsp.tracedecay` registration still present, run `tracedecay install --agent opencode`",
        );
    }
    let Some(mcp_entry) = config
        .pointer(MCP_SERVER_POINTER)
        .filter(|entry| entry.is_object())
    else {
        dc.fail(&format!(
            "MCP server NOT registered in {}, run `tracedecay install --agent opencode`",
            config_path.display()
        ));
        return;
    };
    dc.pass(&format!(
        "MCP server registered in {}",
        config_path.display()
    ));

    let command = mcp_entry["command"].as_array();
    let has_serve = command.is_some_and(|arr| arr.iter().any(|v| v.as_str() == Some("serve")));
    if has_serve {
        dc.pass("MCP server args include \"serve\"");
    } else {
        dc.fail("MCP server args missing \"serve\", run `tracedecay install --agent opencode`");
    }
}

fn doctor_check_prompt(dc: &mut DoctorCounters, home: &Path, profile: &ProfileRoot) {
    super::doctor_check_prompt_contains_tracedecay(
        dc,
        &opencode_prompt_path(home, profile),
        "AGENTS.md",
        "opencode",
    );
}

fn doctor_check_plugin(dc: &mut DoctorCounters, home: &Path, profile: &ProfileRoot) {
    let plugin_path = opencode_plugin_path(home, profile);
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
            "native edit/idle plugin missing from {}, run `tracedecay install --agent opencode`",
            plugin_path.display()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_registration_ignores_corrupt_mcp_config() {
        use crate::agents::host_bundle::{HostBundleRegistrationStateV1 as State, HostComponentV1};

        let home = tempfile::tempdir().unwrap();
        let profile = ProfileRoot::under_home(home.path());
        let config = opencode_config_path(home.path(), &profile);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "{not-json").unwrap();
        let ctx = HealthcheckContext {
            home: home.path().to_path_buf(),
            profile: profile.clone(),
            project_path: home.path().to_path_buf(),
        };

        assert_eq!(
            OpenCodeIntegration.host_component_registration(HostComponentV1::Core, &ctx),
            State::Missing
        );
        assert_eq!(
            OpenCodeIntegration.host_component_registration(HostComponentV1::ContextMcp, &ctx),
            State::Corrupt
        );
        install_opencode_plugin(
            &opencode_plugin_path(home.path(), &profile),
            "/usr/bin/tracedecay",
        )
        .unwrap();
        install_prompt_rules(&opencode_prompt_path(home.path(), &profile)).unwrap();
        assert_eq!(
            OpenCodeIntegration.host_component_registration(HostComponentV1::Core, &ctx),
            State::Current
        );
        let install = InstallContext {
            home: ctx.home,
            profile,
            tracedecay_bin: "/usr/bin/tracedecay".to_string(),
            project_root: None,
            dashboard: false,
        };
        OpenCodeIntegration
            .activate_deployed_host_component_registration(&[HostComponentV1::Core], &install)
            .unwrap();
        OpenCodeIntegration
            .deactivate_deployed_host_component_registration(&[HostComponentV1::Core], &install)
            .unwrap();
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "{not-json");
    }

    #[test]
    fn existing_core_with_legacy_lsp_requires_activation() {
        use crate::agents::host_bundle::{HostBundleRegistrationStateV1 as State, HostComponentV1};

        let home = tempfile::tempdir().unwrap();
        let profile = ProfileRoot::under_home(home.path());
        let config = opencode_config_path(home.path(), &profile);
        install_opencode_plugin(
            &opencode_plugin_path(home.path(), &profile),
            "/usr/bin/tracedecay",
        )
        .unwrap();
        install_prompt_rules(&opencode_prompt_path(home.path(), &profile)).unwrap();
        std::fs::write(&config, serde_json::to_vec(&json!({
            "lsp": {"tracedecay": {"command": ["old-tracedecay", "lsp"]}, "operator": {"command": ["operator-lsp"]}},
            "mcp": {"servers": {"docs": {"type": "remote", "url": "https://mcp.example.com"}}}
        })).unwrap()).unwrap();
        let ctx = HealthcheckContext {
            home: home.path().to_path_buf(),
            profile: profile.clone(),
            project_path: home.path().to_path_buf(),
        };
        assert_eq!(
            OpenCodeIntegration.host_component_registration(HostComponentV1::Core, &ctx),
            State::Repairable
        );
        remove_legacy_lsp_registration(&config).unwrap();
        assert_eq!(
            OpenCodeIntegration.host_component_registration(HostComponentV1::Core, &ctx),
            State::Current
        );
        let config = crate::agents::load_json_file_strict(&config).unwrap();
        assert!(config["lsp"].get("tracedecay").is_none());
        assert_eq!(
            config["lsp"]["operator"]["command"],
            json!(["operator-lsp"])
        );
        assert_eq!(config["mcp"]["servers"]["docs"]["type"], "remote");
    }

    #[test]
    fn legacy_mcp_registration_remains_detectable_for_migration() {
        let home = tempfile::tempdir().unwrap();
        let profile = ProfileRoot::under_home(home.path());
        let config = opencode_config_path(home.path(), &profile);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            &config,
            serde_json::to_vec(&json!({
                "mcp": {
                    "tracedecay": {
                        "type": "local",
                        "command": ["tracedecay", "serve"]
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(OpenCodeIntegration.has_tracedecay(home.path(), &profile));

        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("opencode.json"),
            std::fs::read(&config).unwrap(),
        )
        .unwrap();
        assert!(local_config_has_tracedecay(project.path()));
    }

    #[test]
    fn doctor_reports_retired_lsp_registration() {
        let home = tempfile::tempdir().unwrap();
        let profile = ProfileRoot::under_home(home.path());
        let config = opencode_config_path(home.path(), &profile);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        for (mcp, legacy_mcp) in [
            (
                json!({
                    "servers": {
                        "tracedecay": {
                            "type": "local",
                            "command": ["tracedecay", "serve"]
                        }
                    }
                }),
                false,
            ),
            (
                json!({
                    "tracedecay": {
                        "type": "local",
                        "command": ["tracedecay", "serve"]
                    }
                }),
                true,
            ),
            (Value::Null, false),
        ] {
            std::fs::write(
                &config,
                serde_json::to_vec(&json!({
                    "mcp": mcp,
                    "lsp": {
                        "tracedecay": {
                            "command": ["tracedecay", "lsp"]
                        }
                    }
                }))
                .unwrap(),
            )
            .unwrap();

            let mut counters = DoctorCounters::new();
            doctor_check_config(&mut counters, home.path(), &profile);

            assert!(counters.checks.iter().any(|check| {
                check.level == crate::agents::DoctorCheckLevelV1::Issue
                    && check.message.contains("lsp.tracedecay")
            }));
            assert_eq!(
                counters.checks.iter().any(|check| {
                    check.level == crate::agents::DoctorCheckLevelV1::Issue
                        && check.message.contains("mcp.tracedecay")
                }),
                legacy_mcp
            );
        }
    }

    /// The profile's `$XDG_CONFIG_HOME` is honored only inside the home being
    /// resolved: otherwise a managed-skill export sweep handed a sandbox home
    /// resolves OpenCode to the operator's real `~/.config/opencode` and
    /// writes there.
    #[test]
    fn profile_xdg_never_redirects_a_home_outside_itself() {
        let home = tempfile::tempdir().unwrap();
        let inside = home.path().join("xdg");
        let outside = tempfile::tempdir().unwrap();

        let foreign = ProfileRoot::under_home(home.path()).with_xdg_config_home(outside.path());
        assert_eq!(
            opencode_config_path(home.path(), &foreign),
            home.path().join(".config/opencode/opencode.json")
        );
        assert_eq!(
            opencode_prompt_path(home.path(), &foreign),
            home.path().join(".config/opencode/AGENTS.md")
        );
        let own = ProfileRoot::under_home(home.path()).with_xdg_config_home(&inside);
        assert_eq!(
            opencode_config_path(home.path(), &own),
            inside.join("opencode/opencode.json")
        );
        assert_eq!(
            opencode_prompt_path(home.path(), &own),
            inside.join("opencode/AGENTS.md")
        );
    }

    #[test]
    fn mcp_registration_migrates_v1_entries_and_preserves_operator_lsp_servers() {
        let home = tempfile::tempdir().unwrap();
        let config_path = home.path().join("opencode.json");
        std::fs::write(
            &config_path,
            serde_json::to_vec_pretty(&json!({
                "mcp": {
                    "tracedecay": {"type": "local", "command": ["old-tracedecay", "serve"]},
                    "servers": {
                        "docs": {"type": "remote", "url": "https://mcp.example.com"}
                    }
                },
                "lsp": {
                    "tracedecay": {"command": ["old-tracedecay", "lsp", "bridge", "--stdio"]},
                    "operator": {"command": ["operator-lsp"]}
                },
                "plugins": ["opencode-acme-plugin"]
            }))
            .unwrap(),
        )
        .unwrap();

        install_mcp_server(&config_path, "/usr/bin/tracedecay").unwrap();

        let config = crate::agents::load_json_file_strict(&config_path).unwrap();
        assert_eq!(
            config["mcp"]["servers"]["tracedecay"],
            json!({"type": "local", "command": ["/usr/bin/tracedecay", "serve"]})
        );
        assert!(config["mcp"].get("tracedecay").is_none());
        assert_eq!(config["mcp"]["servers"]["docs"]["type"], "remote");
        assert!(config["lsp"].get("tracedecay").is_none());
        assert_eq!(
            config["lsp"]["operator"]["command"],
            json!(["operator-lsp"])
        );
        assert_eq!(config["plugins"], json!(["opencode-acme-plugin"]));

        uninstall_mcp_server(&config_path).unwrap();

        let config = crate::agents::load_json_file_strict(&config_path).unwrap();
        assert!(config["mcp"]["servers"].get("tracedecay").is_none());
        assert_eq!(config["mcp"]["servers"]["docs"]["type"], "remote");
        assert_eq!(
            config["lsp"]["operator"]["command"],
            json!(["operator-lsp"])
        );
        assert_eq!(config["plugins"], json!(["opencode-acme-plugin"]));
    }

    /// A recorded install lifecycle owns the file and the `mcp` / `mcp.servers`
    /// containers it created, so its uninstall removes all three.
    #[test]
    fn uninstall_removes_a_config_that_held_only_the_managed_server() {
        let home = tempfile::tempdir().unwrap();
        let config_path = home.path().join("opencode.json");
        let mut facts = Vec::new();
        crate::agents::recorded_lifecycle(home.path(), &mut facts, false, || {
            install_mcp_server(&config_path, "/usr/bin/tracedecay")
        })
        .unwrap();
        assert!(
            crate::agents::load_json_file_strict(&config_path)
                .unwrap()
                .pointer(MCP_SERVER_POINTER)
                .is_some()
        );

        crate::agents::recorded_lifecycle(home.path(), &mut facts, true, || {
            uninstall_mcp_server(&config_path)
        })
        .unwrap();

        assert!(!config_path.exists());
    }

    #[test]
    fn external_xdg_assets_are_mirrored_byte_for_byte() {
        use crate::agents::host_bundle::HostComponentV1;

        let home = tempfile::tempdir().unwrap();
        let xdg = tempfile::tempdir().unwrap();
        let components = [HostComponentV1::ContextMcp];
        for (index, relative) in opencode_asset_relative_paths(&components)
            .iter()
            .enumerate()
        {
            let source = home.path().join(".config/opencode").join(relative);
            std::fs::create_dir_all(source.parent().unwrap()).unwrap();
            std::fs::write(&source, format!("asset-{index}\n")).unwrap();
        }

        let external_root = xdg.path().join("opencode");
        mirror_external_opencode_assets_to(home.path(), &external_root, &components).unwrap();

        for relative in opencode_asset_relative_paths(&components) {
            assert_eq!(
                std::fs::read(external_root.join(&relative)).unwrap(),
                std::fs::read(home.path().join(".config/opencode").join(relative)).unwrap()
            );
        }
    }

    /// Install the rules as one recorded lifecycle, returning its creation
    /// facts.
    fn installed_prompt(
        path: &Path,
        operator_contents: Option<&[u8]>,
    ) -> Vec<tracedecay_host_integration::HostConfigCreationV1> {
        if let Some(contents) = operator_contents {
            std::fs::write(path, contents).unwrap();
        }
        let mut facts = Vec::new();
        crate::agents::recorded_lifecycle(path.parent().unwrap(), &mut facts, false, || {
            install_prompt_rules(path)
        })
        .unwrap();
        facts
    }

    fn recorded_uninstall(
        path: &Path,
        mut facts: Vec<tracedecay_host_integration::HostConfigCreationV1>,
    ) -> Result<()> {
        crate::agents::recorded_lifecycle(path.parent().unwrap(), &mut facts, true, || {
            uninstall_prompt_rules(path)
        })
    }

    fn start_paused_uninstall(
        path: &Path,
        facts: Vec<tracedecay_host_integration::HostConfigCreationV1>,
    ) -> (
        crate::agents::TestHostConfigWritePauseController,
        std::thread::JoinHandle<std::result::Result<(), String>>,
    ) {
        let pause = crate::agents::pause_next_host_config_write_at_publication(path);
        let writer_path = path.to_path_buf();
        let remover = std::thread::spawn(move || {
            recorded_uninstall(&writer_path, facts).map_err(|error| error.to_string())
        });
        pause.wait_until_reached();
        (pause, remover)
    }

    #[test]
    fn opencode_prompt_uninstall_refuses_a_concurrent_nonempty_rewrite() {
        let root = tempfile::tempdir().unwrap();
        let prompt = root.path().join("AGENTS.md");
        let facts = installed_prompt(&prompt, Some(b"operator rules\n"));
        let (pause, remover) = start_paused_uninstall(&prompt, facts);

        let foreign = b"foreign OpenCode edit\n";
        std::fs::write(&prompt, foreign).unwrap();
        pause.resume();
        let error = remover.join().unwrap().unwrap_err();

        assert!(error.contains("changed since it was read"), "{error}");
        assert_eq!(std::fs::read(&prompt).unwrap(), foreign);
    }

    #[test]
    fn opencode_prompt_uninstall_refuses_a_concurrent_empty_deletion() {
        let root = tempfile::tempdir().unwrap();
        let prompt = root.path().join("AGENTS.md");
        let facts = installed_prompt(&prompt, None);
        let (pause, remover) = start_paused_uninstall(&prompt, facts);

        let foreign = b"foreign OpenCode edit\n";
        std::fs::write(&prompt, foreign).unwrap();
        pause.resume();
        let error = remover.join().unwrap().unwrap_err();

        assert!(error.contains("changed since it was read"), "{error}");
        assert_eq!(std::fs::read(&prompt).unwrap(), foreign);
    }

    #[test]
    fn opencode_prompt_uninstall_rewrites_operator_content_and_deletes_an_empty_result() {
        let root = tempfile::tempdir().unwrap();
        let nonempty = root.path().join("nonempty.md");
        let facts = installed_prompt(&nonempty, Some(b"operator rules\n"));

        recorded_uninstall(&nonempty, facts).unwrap();

        assert_eq!(std::fs::read(&nonempty).unwrap(), b"operator rules\n");

        let empty = root.path().join("empty.md");
        let facts = installed_prompt(&empty, None);

        recorded_uninstall(&empty, facts).unwrap();

        assert!(!empty.exists());
    }

    #[cfg(unix)]
    #[test]
    fn opencode_prompt_uninstall_refuses_a_symlink_swap() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let prompt = root.path().join("AGENTS.md");
        let outside = root.path().join("outside.md");
        let facts = installed_prompt(&prompt, None);
        std::fs::write(&outside, b"outside OpenCode rules\n").unwrap();
        let (pause, remover) = start_paused_uninstall(&prompt, facts);

        std::fs::remove_file(&prompt).unwrap();
        symlink(&outside, &prompt).unwrap();
        pause.resume();
        let error = remover.join().unwrap().unwrap_err();

        assert!(error.contains("unsafe host metadata path"), "{error}");
        assert!(
            std::fs::symlink_metadata(&prompt)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read(&outside).unwrap(),
            b"outside OpenCode rules\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn opencode_prompt_uninstall_refuses_a_metadata_change() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        let prompt = root.path().join("AGENTS.md");
        let facts = installed_prompt(&prompt, Some(b"operator rules\n"));
        let before = std::fs::read(&prompt).unwrap();
        std::fs::set_permissions(&prompt, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (pause, remover) = start_paused_uninstall(&prompt, facts);

        std::fs::set_permissions(&prompt, std::fs::Permissions::from_mode(0o640)).unwrap();
        pause.resume();
        let error = remover.join().unwrap().unwrap_err();

        assert!(error.contains("changed since it was read"), "{error}");
        assert_eq!(std::fs::read(&prompt).unwrap(), before);
        assert_eq!(std::fs::metadata(&prompt).unwrap().mode() & 0o777, 0o640);
    }

    #[test]
    fn project_uninstall_propagates_registration_removal_errors() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let config = project.path().join("opencode.json");
        std::fs::write(&config, "{not-json").unwrap();
        let ctx = InstallContext {
            profile: tracedecay_runtime_core::config::ProfileRoot::under_home(home.path()),
            home: home.path().to_path_buf(),
            tracedecay_bin: "/usr/bin/tracedecay".to_string(),
            project_root: Some(project.path().to_path_buf()),
            dashboard: false,
        };

        let error = OpenCodeIntegration
            .deactivate_project_host_component_registration(&[], &ctx, project.path())
            .expect_err("corrupt project opencode.json must fail uninstall");

        assert!(error.to_string().contains("cannot parse"), "{error}");
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "{not-json");
    }

    #[test]
    fn opencode_prompt_uninstall_refuses_a_missing_file_race() {
        let root = tempfile::tempdir().unwrap();
        let prompt = root.path().join("AGENTS.md");
        let facts = installed_prompt(&prompt, None);
        let (pause, remover) = start_paused_uninstall(&prompt, facts);

        std::fs::remove_file(&prompt).unwrap();
        pause.resume();
        let error = remover.join().unwrap().unwrap_err();

        assert!(error.contains("failed to conditionally remove"), "{error}");
        assert!(!prompt.exists());
    }
}
