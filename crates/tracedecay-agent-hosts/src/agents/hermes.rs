//! Hermes agent integration.
//!
//! Installs a Hermes profile plugin that exposes tracedecay tools as
//! Hermes-native plugin tools.

mod dashboard_wrapper;
mod lifecycle;
mod profile_config;

use std::collections::{BTreeMap, BTreeSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::errors::{Result, TraceDecayError};
pub use profile_config::read_config_pinned_project_root;
use profile_config::{disable_plugin, enable_plugin};

use super::{AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext};

mod templates;

/// Hermes agent.
pub struct HermesIntegration;

impl AgentIntegration for HermesIntegration {
    fn name(&self) -> &'static str {
        "Hermes"
    }

    fn id(&self) -> &'static str {
        "hermes"
    }

    fn activate_deployed_host_registration(&self, ctx: &InstallContext) -> Result<()> {
        lifecycle::activate_catalog_plugin_profiles(ctx)
    }

    fn deactivate_deployed_host_registration(&self, ctx: &InstallContext) -> Result<()> {
        lifecycle::deactivate_catalog_plugin_profiles(ctx)
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mHermes integration\x1b[0m");
        doctor_check_plugin(dc, &ctx.home);
    }

    fn host_component_registration(
        &self,
        _component: super::host_bundle_v2::HostBundleComponentV1,
        ctx: &HealthcheckContext,
    ) -> super::host_bundle_v2::HostBundleRegistrationStateV1 {
        hermes_registration_state(&ctx.home, None)
    }

    fn host_component_registration_for_lifecycle(
        &self,
        _component: super::host_bundle_v2::HostBundleComponentV1,
        ctx: &HealthcheckContext,
        install: &InstallContext,
    ) -> super::host_bundle_v2::HostBundleRegistrationStateV1 {
        hermes_registration_state(&ctx.home, Some(install.dashboard))
    }

    fn is_detected(&self, home: &Path) -> bool {
        hermes_home(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<PathBuf> {
        Some(hermes_home(home).join("config.yaml"))
    }

    fn host_registration_paths(&self, home: &Path) -> Vec<PathBuf> {
        let default_plugin = hermes_home(home).join("plugins/tracedecay");
        let mut paths = Vec::new();
        for plugin_dir in profile_plugin_dirs(home) {
            let Some(profile_dir) = plugin_dir.parent().and_then(Path::parent) else {
                continue;
            };
            let config = profile_dir.join("config.yaml");
            paths.push(config.clone());
            paths.push(profile_config::original_config_path(&config));
            paths.extend(dashboard_wrapper::managed_paths(&plugin_dir));
            if plugin_dir != default_plugin {
                paths.extend(managed_plugin_paths(&plugin_dir));
            }
        }
        paths.sort();
        paths.dedup();
        paths
    }

    fn host_component_registration_paths_checked(
        &self,
        _components: &[super::host_bundle_v2::HostBundleComponentV1],
        home: &Path,
    ) -> Result<Vec<PathBuf>> {
        let mut paths = self.host_registration_paths(home);
        let profile_root = crate::automation::skill_targets::profile_root_for_agent_home(home);
        for plugin_dir in profile_plugin_dirs(home) {
            paths.extend(managed_skill_overlay_paths(&profile_root, &plugin_dir)?);
        }
        paths.sort();
        paths.dedup();
        Ok(paths)
    }

    fn has_tracedecay(&self, home: &Path) -> bool {
        detected_plugin_dirs(home)
            .into_iter()
            .any(|dir| dir.is_dir())
    }

    fn export_managed_skills(
        &self,
        home: &Path,
        profile_root: &Path,
    ) -> Result<Vec<crate::automation::skill_targets::SkillInstallSummary>> {
        let mut exports = Vec::new();
        for plugin_dir in detected_plugin_dirs(home) {
            exports.push(crate::automation::skill_targets::install_managed_skills(
                profile_root,
                crate::automation::skill_targets::SkillInstallTarget::Hermes,
                &plugin_dir,
            )?);
        }
        Ok(exports)
    }
}

fn hermes_registration_state(
    home: &Path,
    expected_dashboard: Option<bool>,
) -> super::host_bundle_v2::HostBundleRegistrationStateV1 {
    use super::host_bundle_v2::HostBundleRegistrationStateV1 as State;

    let plugin_dirs = profile_plugin_dirs(home);
    let default_plugin = hermes_home(home).join("plugins/tracedecay");
    if !default_plugin.join("plugin.yaml").is_file() {
        return State::Missing;
    }
    if !managed_plugin_paths(&default_plugin)
        .into_iter()
        .all(|path| path.is_file())
    {
        return State::Repairable;
    }
    let dashboard_enabled = match expected_dashboard {
        Some(enabled) => enabled,
        None if dashboard_wrapper::is_current(&default_plugin) => true,
        None if dashboard_wrapper::is_absent(&default_plugin) => false,
        None => return State::Repairable,
    };
    if !dashboard_wrapper::matches_policy(&default_plugin, dashboard_enabled) {
        return State::Repairable;
    }
    let profile_root = crate::automation::skill_targets::profile_root_for_agent_home(home);
    for plugin_dir in plugin_dirs {
        let Some(profile_dir) = plugin_dir.parent().and_then(Path::parent) else {
            return State::Corrupt;
        };
        let overlay_current = match managed_skill_overlay_is_current(&profile_root, &plugin_dir) {
            Ok(current) => current,
            Err(_) => return State::Corrupt,
        };
        if profile_config::registration_state(&profile_dir.join("config.yaml")) != State::Current
            || !dashboard_wrapper::matches_policy(&plugin_dir, dashboard_enabled)
            || !managed_profile_files_match(&default_plugin, &plugin_dir)
            || !overlay_current
        {
            return State::Repairable;
        }
    }
    State::Current
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod registration_tests {
    use super::*;

    #[test]
    fn registration_inventory_owns_existing_managed_skill_overlay_files() {
        let home = tempfile::tempdir().unwrap();
        let overlay = home
            .path()
            .join(".hermes/plugins/tracedecay/skills/agent-managed/example");
        std::fs::create_dir_all(&overlay).unwrap();
        let skill = overlay.join("SKILL.md");
        std::fs::write(&skill, "managed").unwrap();

        let paths = HermesIntegration
            .host_component_registration_paths_checked(
                &[super::super::host_bundle_v2::HostBundleComponentV1::Core],
                home.path(),
            )
            .unwrap();

        assert!(paths.contains(&skill));
    }
}

fn hermes_home(home: &Path) -> PathBuf {
    home.join(".hermes")
}

fn doctor_check_plugin(dc: &mut DoctorCounters, home: &Path) {
    let candidates = hermes_healthcheck_plugin_paths(home);
    let existing: Vec<&PathBuf> = candidates.iter().filter(|plugin| plugin.exists()).collect();
    let Some(first) = existing.first() else {
        if let Some(plugin) = candidates.first() {
            dc.warn(&format!(
                "{} not found — run `tracedecay install --agent hermes` if you use Hermes",
                plugin.display()
            ));
        } else {
            dc.warn("Hermes tracedecay plugin not found — run `tracedecay install --agent hermes` if you use Hermes");
        }
        return;
    };
    dc.pass(&format!(
        "Hermes tracedecay plugin found at {}",
        first.display()
    ));

    for manifest_path in &existing {
        // Stale generated plugins keep working but miss new tools/config
        // surfaces; `hermes plugins list` shows the same manifest version.
        match read_manifest_version(manifest_path) {
            Some(version) if version == crate::PRODUCT_VERSION => {}
            Some(version) => dc.warn(&format!(
                "{} was generated by tracedecay {version} (installed binary is {}) — re-run `tracedecay install --agent hermes` to refresh it",
                manifest_path.display(),
                crate::PRODUCT_VERSION,
            )),
            None => dc.warn(&format!(
                "{} has no manifest version — re-run `tracedecay install --agent hermes` to refresh it",
                manifest_path.display(),
            )),
        }
    }
}

fn hermes_healthcheck_plugin_paths(home: &Path) -> Vec<PathBuf> {
    vec![hermes_home(home).join("plugins/tracedecay/plugin.yaml")]
}

fn read_manifest_version(manifest_path: &Path) -> Option<String> {
    let manifest = std::fs::read_to_string(manifest_path).ok()?;
    manifest
        .lines()
        .find_map(|line| line.strip_prefix("version:"))
        .map(|version| version.trim().to_string())
        .filter(|version| !version.is_empty())
}

pub(super) fn install_plugin(
    plugin_dir: &Path,
    tracedecay_bin: &str,
    deploy_dashboard: bool,
    profile_root: &Path,
) -> Result<()> {
    write_plugin_files(plugin_dir, tracedecay_bin)?;
    dashboard_wrapper::apply_install_policy(plugin_dir, tracedecay_bin, deploy_dashboard)?;
    reconcile_managed_skill_overlay(profile_root, plugin_dir)?;
    if let Some(profile_dir) = plugin_dir.parent().and_then(Path::parent) {
        let config_path = profile_dir.join("config.yaml");
        enable_plugin(&config_path)?;
    }

    tracing::debug!(
        plugin_dir = %plugin_dir.display(),
        "wrote Hermes tracedecay plugin"
    );
    Ok(())
}

/// Writes the generated agent-plugin files (manifest, schemas, tools,
/// entrypoint, skill). Shared by install and the config-preserving update
/// lifecycle path; never touches config.yaml.
pub(super) fn write_plugin_files(plugin_dir: &Path, tracedecay_bin: &str) -> Result<()> {
    std::fs::create_dir_all(plugin_dir).map_err(|e| TraceDecayError::Config {
        message: format!("failed to create {}: {e}", plugin_dir.display()),
    })?;
    for (relative_path, contents) in rendered_plugin_files(tracedecay_bin)? {
        write_text_file(&plugin_dir.join(relative_path), &contents)?;
    }
    Ok(())
}

/// Canonical rendered Hermes plugin inventory used by the receipt-backed
/// first-party catalog. Callers must pass the installed binary path, never the
/// running executable path.
pub(crate) fn rendered_plugin_files(tracedecay_bin: &str) -> Result<Vec<(&'static str, String)>> {
    Ok(vec![
        ("plugin.yaml", templates::plugin_manifest()),
        ("schemas.py", templates::plugin_schemas()),
        ("schemas.json", templates::plugin_schemas_json()?),
        ("tools.py", templates::plugin_tools(tracedecay_bin)),
        ("__init__.py", templates::plugin_init()),
        ("cli.py", templates::PLUGIN_CLI_PY.to_string()),
        (
            "skills/tracedecay/SKILL.md",
            templates::HERMES_SKILL.to_string(),
        ),
    ])
}

/// Generated plugin locations for the default Hermes profile and every named
/// profile that already exists. Hermes resolves each profile to an independent
/// `HERMES_HOME`, so each one needs the same stock plugin package and provider
/// selections in its own config.yaml.
pub(super) fn profile_plugin_dirs(home: &Path) -> Vec<PathBuf> {
    let root = hermes_home(home);
    let mut profile_roots = vec![root.clone()];
    if let Ok(entries) = std::fs::read_dir(root.join("profiles")) {
        let mut profiles = entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                entry.file_type().ok()?.is_dir().then(|| entry.path())
            })
            .collect::<Vec<_>>();
        profiles.sort();
        profile_roots.extend(profiles);
    }
    profile_roots
        .into_iter()
        .map(|profile_root| profile_root.join("plugins/tracedecay"))
        .collect()
}

pub(super) fn detected_plugin_dirs(home: &Path) -> Vec<PathBuf> {
    profile_plugin_dirs(home)
        .into_iter()
        .filter(|plugin_dir| plugin_dir.join("plugin.yaml").is_file())
        .collect()
}

fn managed_plugin_paths(plugin_dir: &Path) -> Vec<PathBuf> {
    [
        "plugin.yaml",
        "schemas.py",
        "schemas.json",
        "tools.py",
        "__init__.py",
        "cli.py",
        "skills/tracedecay/SKILL.md",
    ]
    .into_iter()
    .map(|relative| plugin_dir.join(relative))
    .collect()
}

fn managed_profile_files_match(default_plugin: &Path, profile_plugin: &Path) -> bool {
    managed_plugin_paths(default_plugin)
        .into_iter()
        .zip(managed_plugin_paths(profile_plugin))
        .chain(
            dashboard_wrapper::managed_paths(default_plugin)
                .into_iter()
                .zip(dashboard_wrapper::managed_paths(profile_plugin)),
        )
        .all(|(expected, observed)| std::fs::read(expected).ok() == std::fs::read(observed).ok())
}

pub(super) fn uninstall_plugin(plugin_dir: &Path) -> Result<()> {
    if let Some(profile_dir) = plugin_dir.parent().and_then(Path::parent) {
        disable_plugin(&profile_dir.join("config.yaml"))?;
    }
    remove_generated_plugin_files(plugin_dir)
}

pub(super) fn remove_generated_plugin_files(plugin_dir: &Path) -> Result<()> {
    if !plugin_dir.exists() {
        tracing::debug!(
            plugin_dir = %plugin_dir.display(),
            "Hermes tracedecay plugin not found; skipping removal"
        );
        return Ok(());
    }

    remove_generated_file(&plugin_dir.join("plugin.yaml"))?;
    remove_generated_file(&plugin_dir.join("schemas.py"))?;
    remove_generated_file(&plugin_dir.join("schemas.json"))?;
    remove_generated_file(&plugin_dir.join("tools.py"))?;
    remove_generated_file(&plugin_dir.join("__init__.py"))?;
    remove_generated_file(&plugin_dir.join("cli.py"))?;
    remove_generated_file(&plugin_dir.join("skills/tracedecay/SKILL.md"))?;
    remove_empty_dir(&plugin_dir.join("skills/tracedecay"))?;
    remove_managed_skill_overlay(plugin_dir)?;
    remove_empty_dir(&plugin_dir.join("skills"))?;
    dashboard_wrapper::uninstall(plugin_dir)?;

    if remove_empty_dir(plugin_dir)? {
        tracing::debug!(
            plugin_dir = %plugin_dir.display(),
            "removed Hermes tracedecay plugin"
        );
    } else {
        tracing::warn!(
            plugin_dir = %plugin_dir.display(),
            "left Hermes plugin directory in place because it contains files not generated by tracedecay"
        );
    }
    Ok(())
}

fn managed_skill_overlay_root(plugin_dir: &Path) -> PathBuf {
    plugin_dir.join("skills/agent-managed")
}

fn existing_managed_skill_overlay_paths(plugin_dir: &Path) -> Result<Vec<PathBuf>> {
    let root = managed_skill_overlay_root(plugin_dir);
    for ancestor in [plugin_dir.to_path_buf(), plugin_dir.join("skills")] {
        match std::fs::symlink_metadata(&ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "refusing unsafe Hermes managed skill parent {}",
                        ancestor.display()
                    ),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "failed to inspect Hermes managed skill parent {}: {error}",
                        ancestor.display()
                    ),
                });
            }
        }
    }
    match std::fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "refusing unsafe Hermes managed skill overlay {}",
                    root.display()
                ),
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "failed to inspect Hermes managed skill overlay {}: {error}",
                    root.display()
                ),
            });
        }
    }
    fn collect(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(directory).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to inventory Hermes managed skill directory {}: {error}",
                directory.display()
            ),
        })? {
            let entry = entry.map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to inventory Hermes managed skill directory {}: {error}",
                    directory.display()
                ),
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to inspect Hermes managed skill path {}: {error}",
                    path.display()
                ),
            })?;
            if file_type.is_symlink() {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "refusing symlink in Hermes managed skill overlay {}",
                        path.display()
                    ),
                });
            }
            if file_type.is_dir() {
                collect(&path, paths)?;
            } else if file_type.is_file() {
                paths.push(path);
            } else {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "refusing non-file in Hermes managed skill overlay {}",
                        path.display()
                    ),
                });
            }
        }
        Ok(())
    }
    let mut paths = Vec::new();
    collect(&root, &mut paths)?;
    paths.sort();
    Ok(paths)
}

fn managed_skill_overlay_paths(profile_root: &Path, plugin_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = existing_managed_skill_overlay_paths(plugin_dir)?;
    paths.extend(
        crate::automation::skill_targets::rendered_native_skill_overlay_files(
            profile_root,
            crate::automation::skill_targets::SkillInstallTarget::Hermes,
            plugin_dir,
        )?
        .into_iter()
        .map(|(path, _)| path),
    );
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn managed_skill_overlay_is_current(profile_root: &Path, plugin_dir: &Path) -> Result<bool> {
    let desired = crate::automation::skill_targets::rendered_native_skill_overlay_files(
        profile_root,
        crate::automation::skill_targets::SkillInstallTarget::Hermes,
        plugin_dir,
    )?
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    let existing = existing_managed_skill_overlay_paths(plugin_dir)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    if existing != desired.keys().cloned().collect() {
        return Ok(false);
    }
    Ok(desired
        .into_iter()
        .all(|(path, expected)| std::fs::read(path).is_ok_and(|observed| observed == expected)))
}

fn reconcile_managed_skill_overlay(profile_root: &Path, plugin_dir: &Path) -> Result<()> {
    let desired = crate::automation::skill_targets::rendered_native_skill_overlay_files(
        profile_root,
        crate::automation::skill_targets::SkillInstallTarget::Hermes,
        plugin_dir,
    )?
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    let existing = existing_managed_skill_overlay_paths(plugin_dir)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    for (path, bytes) in &desired {
        super::safe_write_bytes_file(path, bytes, None)?;
    }
    for path in existing {
        if !desired.contains_key(&path) {
            remove_generated_file(&path)?;
        }
    }
    prune_empty_managed_skill_overlay_dirs(plugin_dir)
}

fn remove_managed_skill_overlay(plugin_dir: &Path) -> Result<()> {
    for path in existing_managed_skill_overlay_paths(plugin_dir)? {
        remove_generated_file(&path)?;
    }
    prune_empty_managed_skill_overlay_dirs(plugin_dir)
}

fn prune_empty_managed_skill_overlay_dirs(plugin_dir: &Path) -> Result<()> {
    let root = managed_skill_overlay_root(plugin_dir);
    if !root.exists() {
        return Ok(());
    }
    let mut directories = Vec::new();
    let mut pending = vec![root.clone()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to inspect Hermes managed skill directory {}: {error}",
                directory.display()
            ),
        })? {
            let entry = entry.map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to inspect Hermes managed skill directory {}: {error}",
                    directory.display()
                ),
            })?;
            if entry
                .file_type()
                .map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "failed to inspect Hermes managed skill path {}: {error}",
                        entry.path().display()
                    ),
                })?
                .is_dir()
            {
                pending.push(entry.path());
            }
        }
        directories.push(directory);
    }
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        remove_empty_dir(&directory)?;
    }
    Ok(())
}

pub(super) fn write_text_file(path: &Path, contents: &str) -> Result<()> {
    let current = std::fs::read_to_string(path).unwrap_or_default();
    if current == contents {
        return Ok(());
    }
    super::safe_write_text_file(path, contents, None)
}

pub(super) fn remove_generated_file(path: &Path) -> Result<()> {
    match super::safe_remove_host_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(TraceDecayError::Config {
            message: format!("failed to remove {}: {e}", path.display()),
        }),
    }
}

pub(super) fn remove_empty_dir(path: &Path) -> Result<bool> {
    match std::fs::remove_dir(path) {
        Ok(()) => Ok(true),
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty) => {
            Ok(false)
        }
        Err(e) => Err(TraceDecayError::Config {
            message: format!("failed to remove {}: {e}", path.display()),
        }),
    }
}
