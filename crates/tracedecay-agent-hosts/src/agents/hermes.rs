//! Hermes agent integration.
//!
//! Installs a Hermes profile plugin that exposes tracedecay tools as
//! Hermes-native plugin tools.

mod dashboard_wrapper;
mod lifecycle;
pub mod profile_config;

use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
};

use crate::errors::{Result, TraceDecayError};

use super::{
    AgentIntegration, DoctorCounters, HealthcheckContext, InstallContext, UpdatePluginOutcome,
};

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

    fn install(&self, ctx: &InstallContext) -> Result<()> {
        lifecycle::install(ctx)?;
        self.reconcile_managed_skills(ctx)?;
        Ok(())
    }

    fn update_plugin(&self, ctx: &InstallContext) -> Result<UpdatePluginOutcome> {
        let outcome = lifecycle::update_plugin(ctx)?;
        if matches!(outcome, UpdatePluginOutcome::Refreshed(_)) {
            self.reconcile_managed_skills(ctx)?;
        }
        Ok(outcome)
    }

    fn uninstall(&self, ctx: &InstallContext) -> Result<()> {
        lifecycle::uninstall(ctx)?;
        Ok(())
    }

    fn healthcheck(&self, dc: &mut DoctorCounters, ctx: &HealthcheckContext) {
        eprintln!("\n\x1b[1mHermes integration\x1b[0m");
        doctor_check_plugin(dc, &ctx.home);
    }

    fn is_detected(&self, home: &Path) -> bool {
        hermes_home(home).is_dir()
    }

    fn primary_config_path(&self, home: &Path) -> Option<PathBuf> {
        Some(hermes_home(home).join("config.yaml"))
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

impl HermesIntegration {
    fn reconcile_managed_skills(&self, ctx: &InstallContext) -> Result<()> {
        let profile_root = crate::automation::skill_targets::profile_root_for_agent_home(&ctx.home);
        self.export_managed_skills(&ctx.home, &profile_root)?;
        Ok(())
    }
}

fn hermes_home(home: &Path) -> PathBuf {
    home.join(".hermes")
}

fn enable_plugin(config_path: &Path) -> Result<bool> {
    let existing = std::fs::read_to_string(config_path).unwrap_or_default();
    let updated = profile_config::enable_plugin_config(&existing).map_err(|message| {
        TraceDecayError::Config {
            message: format!(
                "{message} in {}.\nFix the config by hand, then re-run: tracedecay install --agent hermes",
                config_path.display()
            ),
        }
    })?;
    if updated != existing {
        write_config_file(config_path, &updated)?;
    }
    Ok(true)
}

fn disable_plugin(config_path: &Path) -> Result<()> {
    let Ok(existing) = std::fs::read_to_string(config_path) else {
        return Ok(());
    };
    let updated = profile_config::disable_plugin_config(&existing).map_err(|message| {
        TraceDecayError::Config {
            message: format!(
                "{message} in {}; leaving Hermes plugin files in place",
                config_path.display()
            ),
        }
    })?;
    if updated != existing {
        write_config_file(config_path, &updated)?;
    }
    Ok(())
}

fn write_config_file(path: &Path, contents: &str) -> Result<()> {
    let current = match std::fs::read_to_string(path) {
        Ok(current) => Some(current),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!("failed to read {}: {error}", path.display()),
            });
        }
    };
    if current.as_deref() == Some(contents) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| TraceDecayError::Config {
            message: format!("failed to create {}: {error}", parent.display()),
        })?;
    }
    let backup = super::backup_config_file(path)?;
    let new_path = PathBuf::from(format!("{}.new", path.display()));
    if let Err(error) = std::fs::write(&new_path, contents) {
        std::fs::remove_file(&new_path).ok();
        return Err(TraceDecayError::Config {
            message: format!("failed to write {}: {error}", new_path.display()),
        });
    }
    if let Err(error) = std::fs::rename(&new_path, path) {
        std::fs::remove_file(&new_path).ok();
        let backup_hint = backup
            .as_ref()
            .map(|path| format!(" Backup is at {}.", path.display()))
            .unwrap_or_default();
        return Err(TraceDecayError::Config {
            message: format!(
                "failed to replace {} with {}: {error}.{backup_hint}",
                path.display(),
                new_path.display(),
            ),
        });
    }
    Ok(())
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
            Some(version) if version == env!("CARGO_PKG_VERSION") => {}
            Some(version) => dc.warn(&format!(
                "{} was generated by tracedecay {version} (installed binary is {}) — re-run `tracedecay install --agent hermes` to refresh it",
                manifest_path.display(),
                env!("CARGO_PKG_VERSION"),
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
) -> Result<()> {
    write_plugin_files(plugin_dir, tracedecay_bin)?;
    dashboard_wrapper::apply_install_policy(plugin_dir, tracedecay_bin, deploy_dashboard)?;
    if let Some(profile_dir) = plugin_dir.parent().and_then(Path::parent) {
        let config_path = profile_dir.join("config.yaml");
        enable_plugin(&config_path)?;
    }

    eprintln!(
        "\x1b[32m✔\x1b[0m Wrote Hermes tracedecay plugin to {}",
        plugin_dir.display()
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
    std::fs::create_dir_all(plugin_dir.join("skills/tracedecay")).map_err(|e| {
        TraceDecayError::Config {
            message: format!(
                "failed to create {}: {e}",
                plugin_dir.join("skills/tracedecay").display()
            ),
        }
    })?;

    write_text_file(
        &plugin_dir.join("plugin.yaml"),
        &templates::plugin_manifest(),
    )?;
    write_text_file(&plugin_dir.join("schemas.py"), &templates::plugin_schemas())?;
    write_text_file(
        &plugin_dir.join("schemas.json"),
        &templates::plugin_schemas_json()?,
    )?;
    write_text_file(
        &plugin_dir.join("tools.py"),
        &templates::plugin_tools(tracedecay_bin),
    )?;
    write_text_file(&plugin_dir.join("__init__.py"), &templates::plugin_init())?;
    write_text_file(&plugin_dir.join("cli.py"), templates::PLUGIN_CLI_PY)?;
    write_text_file(
        &plugin_dir.join("skills/tracedecay/SKILL.md"),
        templates::HERMES_SKILL,
    )
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

pub(super) fn uninstall_plugin(plugin_dir: &Path) -> Result<()> {
    if let Some(profile_dir) = plugin_dir.parent().and_then(Path::parent) {
        disable_plugin(&profile_dir.join("config.yaml"))?;
    }
    remove_generated_plugin_files(plugin_dir)
}

pub(super) fn remove_generated_plugin_files(plugin_dir: &Path) -> Result<()> {
    if !plugin_dir.exists() {
        eprintln!("  {} not found, skipping", plugin_dir.display());
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
    let managed_overlay = plugin_dir.join("skills/agent-managed");
    if managed_overlay
        .join(".tracedecay-managed-skills.json")
        .is_file()
    {
        std::fs::remove_dir_all(&managed_overlay).map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to remove generated Hermes skill overlay {}: {e}",
                managed_overlay.display()
            ),
        })?;
    }
    remove_empty_dir(&plugin_dir.join("skills"))?;
    dashboard_wrapper::uninstall(plugin_dir)?;

    if remove_empty_dir(plugin_dir)? {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed Hermes tracedecay plugin from {}",
            plugin_dir.display()
        );
    } else {
        eprintln!(
            "  Left {} in place because it contains files not generated by tracedecay",
            plugin_dir.display()
        );
    }
    Ok(())
}

pub(super) fn write_text_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| TraceDecayError::Config {
            message: format!("failed to create {}: {e}", parent.display()),
        })?;
    }
    let current = std::fs::read_to_string(path).unwrap_or_default();
    if current == contents {
        return Ok(());
    }
    // Write-to-.new-then-rename so a mid-write crash can never leave a
    // truncated/corrupt generated file behind (same pattern as
    // write_config_file, minus the backup — these files are regenerable).
    let new_path = PathBuf::from(format!("{}.new", path.display()));
    if let Err(e) = std::fs::write(&new_path, contents) {
        std::fs::remove_file(&new_path).ok();
        return Err(TraceDecayError::Config {
            message: format!("failed to write {}: {e}", new_path.display()),
        });
    }
    if let Err(e) = std::fs::rename(&new_path, path) {
        std::fs::remove_file(&new_path).ok();
        return Err(TraceDecayError::Config {
            message: format!(
                "failed to replace {} with {}: {e}",
                path.display(),
                new_path.display()
            ),
        });
    }
    Ok(())
}

pub(super) fn remove_generated_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
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
