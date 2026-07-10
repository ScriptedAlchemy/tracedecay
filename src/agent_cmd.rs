use std::path::{Path, PathBuf};

use tracedecay::automation::config::{
    AutomationBackend, AutomationConfigPatch, AutomationHostMode, AutomationTaskPatch,
    apply_project_config_patch, project_config_path,
};

/// How `install --agent codex --automation` should configure the daemon loop.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CodexAutomationInstall {
    /// Apply accepted memory-curation ops without dashboard approval
    /// (`--auto-apply`).
    pub(crate) auto_apply: bool,
}

fn validate_codex_automation_flags(
    agent: Option<&str>,
    automation: Option<CodexAutomationInstall>,
) -> tracedecay::errors::Result<()> {
    if automation.is_none() {
        return Ok(());
    }
    if agent != Some("codex") {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "`--automation` is only supported with `--agent codex`".to_string(),
        });
    }
    Ok(())
}

fn validate_codex_automation_project_path() -> tracedecay::errors::Result<PathBuf> {
    let project_path =
        std::env::current_dir().map_err(|e| tracedecay::errors::TraceDecayError::Config {
            message: format!("could not determine current project directory: {e}"),
        })?;
    std::fs::canonicalize(&project_path).map_err(|e| tracedecay::errors::TraceDecayError::Config {
        message: format!(
            "could not canonicalize project directory {}: {e}",
            project_path.display()
        ),
    })
}

async fn install_codex_daemon_automation(
    project_path: &Path,
    home: &Path,
    options: CodexAutomationInstall,
) -> tracedecay::errors::Result<PathBuf> {
    let auto_apply = options.auto_apply;
    if tracedecay::agents::codex::remove_legacy_codex_native_automation(home)? {
        eprintln!(
            "\x1b[32m✔\x1b[0m Removed the legacy Codex-native scheduled automation; the TraceDecay daemon loop replaces it."
        );
    }

    let cg = open_or_init_codex_daemon_automation_project(project_path).await?;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let patch = AutomationConfigPatch {
        enabled: Some(true),
        backend: Some(AutomationBackend::CodexAppServer),
        host_mode: Some(AutomationHostMode::Standalone),
        // Unattended memory-op apply is opt-in: without --auto-apply these
        // stays unset, and re-running the installer never weakens stricter
        // settings a user already chose.
        auto_apply_memory_ops: auto_apply.then_some(true),
        memory_curator: codex_daemon_interval_task(15 * 60),
        session_reflector: codex_daemon_interval_task(15 * 60),
        skill_writer: AutomationTaskPatch {
            min_idle_secs: Some(Some(15 * 60)),
            ..codex_daemon_interval_task(60 * 60)
        },
        ..AutomationConfigPatch::default()
    };

    let global = tracedecay::user_config::UserConfig::load().automation;
    apply_project_config_patch(&dashboard_root, &global, patch).await?;
    let path = project_config_path(&dashboard_root);
    eprintln!(
        "\x1b[32m✔\x1b[0m Enabled TraceDecay daemon automation loop at {}",
        path.display()
    );
    eprintln!(
        "  The daemon scheduler will run memory_curator, session_reflector, and skill_writer via the Codex app-server backend."
    );
    if auto_apply {
        eprintln!(
            "\x1b[33m⚠\x1b[0m --auto-apply: accepted memory-curation ops (deletes and merges) will be applied without dashboard approval. There is no archive; removals are permanent."
        );
    }
    if !tracedecay::daemon::daemon_reachable() {
        eprintln!(
            "\x1b[33m⚠\x1b[0m The TraceDecay daemon is not running, so the automation loop will stay idle. Enable it with `tracedecay daemon install-service`."
        );
    }
    Ok(path)
}

async fn open_or_init_codex_daemon_automation_project(
    project_path: &Path,
) -> tracedecay::errors::Result<tracedecay::tracedecay::TraceDecay> {
    if tracedecay::tracedecay::TraceDecay::has_initialized_store(project_path).await {
        tracedecay::tracedecay::TraceDecay::open(project_path).await
    } else {
        eprintln!(
            "No TraceDecay store found for {}; initializing one (equivalent to `tracedecay init`).",
            project_path.display()
        );
        let cg = tracedecay::tracedecay::TraceDecay::init_with_options(
            project_path,
            tracedecay::tracedecay::TraceDecayOpenOptions::default(),
        )
        .await?;
        cg.index_all().await?;
        Ok(cg)
    }
}

fn codex_daemon_interval_task(interval_secs: u64) -> AutomationTaskPatch {
    AutomationTaskPatch {
        enabled: Some(true),
        schedule: Some(Some("interval".to_string())),
        interval_secs: Some(Some(interval_secs)),
        cooldown_secs: Some(Some(5 * 60)),
        ..AutomationTaskPatch::default()
    }
}

/// Moves provable historical Hermes-local session data before any install can
/// remove its legacy project pin. Unresolved or failed sources block only the
/// Hermes cutover; the source store and its provenance remain untouched.
pub(crate) async fn migrate_legacy_hermes_data(home: &Path) -> tracedecay::errors::Result<()> {
    let report = tracedecay::migrate::hermes::migrate_legacy_hermes_stores(home).await;
    for migration in report.migrated {
        eprintln!(
            "  \x1b[32m✔\x1b[0m Migrated legacy Hermes session store {} -> {} ({} rows)",
            migration.source_db.display(),
            migration.target_project.display(),
            migration.rows_copied
        );
    }
    if report.unresolved.is_empty() && report.failed.is_empty() {
        return Ok(());
    }
    let issues = report
        .unresolved
        .into_iter()
        .chain(report.failed)
        .map(|issue| format!("{}: {}", issue.source_db.display(), issue.reason))
        .collect::<Vec<_>>()
        .join("; ");
    Err(tracedecay::errors::TraceDecayError::Config {
        message: format!(
            "legacy Hermes session data could not be migrated; source data and project pins were preserved: {issues}"
        ),
    })
}

pub(crate) async fn handle_install_command(
    agent: Option<String>,
    local: bool,
    no_dashboard: bool,
    automation: Option<CodexAutomationInstall>,
) -> tracedecay::errors::Result<()> {
    validate_codex_automation_flags(agent.as_deref(), automation)?;
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let tracedecay_bin = tracedecay::agents::which_tracedecay().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "tracedecay not found on PATH. Install it from this repo first:\n  \
                          cargo binstall --git https://github.com/ScriptedAlchemy/tracedecay tracedecay\n  \
                          cargo install --git https://github.com/ScriptedAlchemy/tracedecay tracedecay"
                .to_string(),
        }
    })?;
    if local {
        let project_path =
            std::env::current_dir().map_err(|e| tracedecay::errors::TraceDecayError::Config {
                message: format!("could not determine current project directory: {e}"),
            })?;
        let ctx = tracedecay::agents::InstallContext {
            home: home.clone(),
            tracedecay_bin: tracedecay_bin.clone(),
            tool_permissions: tracedecay::agents::expected_tool_perms(),
            project_root: None,
            dashboard: !no_dashboard,
        };
        let mut installed_names: Vec<String> = Vec::new();

        if let Some(id) = agent {
            let ag = tracedecay::agents::get_integration(&id)?;
            ag.install_local(&ctx, &project_path)?;
            ag.post_install(Some(&project_path)).await;
            if let Some(options) = automation.filter(|_| id == "codex") {
                let scoped_project_path = validate_codex_automation_project_path()?;
                install_codex_daemon_automation(&scoped_project_path, &home, options).await?;
            }
            installed_names.push(ag.name().to_string());
        } else {
            let (to_install, _) = tracedecay::agents::pick_integrations_interactive(&home, &[])?;
            for id in &to_install {
                let ag = tracedecay::agents::get_integration(id)?;
                if ag.supports_local_install() {
                    ag.install_local(&ctx, &project_path)?;
                    ag.post_install(Some(&project_path)).await;
                    installed_names.push(ag.name().to_string());
                } else {
                    eprintln!(
                        "Skipping {}: project-local install is not supported",
                        ag.name()
                    );
                }
            }
        }

        eprintln!();
        if installed_names.is_empty() {
            eprintln!("No local changes.");
        } else {
            for name in &installed_names {
                eprintln!("\x1b[32m+\x1b[0m {name} (local)");
            }
        }
        return Ok(());
    }

    if agent.as_deref() == Some("hermes") {
        migrate_legacy_hermes_data(&home).await?;
    }

    let mut user_cfg = tracedecay::user_config::UserConfig::load();
    tracedecay::agents::migrate_installed_agents(&home, &mut user_cfg);

    let mut installed_names: Vec<String> = Vec::new();
    let mut removed_names: Vec<String> = Vec::new();
    let project_path = std::env::current_dir().ok();

    if let Some(id) = agent {
        let ag = tracedecay::agents::get_integration(&id)?;
        let name = ag.name().to_string();
        let ctx = tracedecay::agents::InstallContext {
            home: home.clone(),
            tracedecay_bin: tracedecay_bin.clone(),
            tool_permissions: tracedecay::agents::expected_tool_perms(),
            project_root: None,
            dashboard: !no_dashboard,
        };
        ag.install(&ctx)?;
        ag.post_install(project_path.as_deref()).await;
        if let Some(options) = automation.filter(|_| id == "codex") {
            let scoped_project_path = validate_codex_automation_project_path()?;
            install_codex_daemon_automation(&scoped_project_path, &home, options).await?;
        }
        if !user_cfg.installed_agents.contains(&id) {
            user_cfg.installed_agents.push(id);
            installed_names.push(name);
        }
        user_cfg
            .save()
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
    } else {
        let (to_install, to_uninstall) =
            tracedecay::agents::pick_integrations_interactive(&home, &user_cfg.installed_agents)?;

        if to_install.iter().any(|id| id == "hermes") {
            migrate_legacy_hermes_data(&home).await?;
        }

        for id in &to_uninstall {
            let ag = tracedecay::agents::get_integration(id)?;
            let ctx = tracedecay::agents::InstallContext {
                home: home.clone(),
                tracedecay_bin: tracedecay_bin.clone(),
                tool_permissions: tracedecay::agents::expected_tool_perms(),
                project_root: None,
                dashboard: !no_dashboard,
            };
            ag.uninstall(&ctx)?;
            removed_names.push(ag.name().to_string());
            user_cfg.installed_agents.retain(|a| a != id);
        }
        for id in &to_install {
            let ag = tracedecay::agents::get_integration(id)?;
            let ctx = tracedecay::agents::InstallContext {
                home: home.clone(),
                tracedecay_bin: tracedecay_bin.clone(),
                tool_permissions: tracedecay::agents::expected_tool_perms(),
                project_root: None,
                dashboard: !no_dashboard,
            };
            ag.install(&ctx)?;
            ag.post_install(project_path.as_deref()).await;
            installed_names.push(ag.name().to_string());
            if !user_cfg.installed_agents.contains(id) {
                user_cfg.installed_agents.push(id.clone());
            }
        }
        user_cfg
            .save()
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
    }

    eprintln!();
    if installed_names.is_empty() && removed_names.is_empty() {
        eprintln!("No changes.");
    } else {
        for name in &installed_names {
            eprintln!("\x1b[32m+\x1b[0m {name}");
        }
        for name in &removed_names {
            eprintln!("\x1b[31m-\x1b[0m {name}");
        }
    }

    user_cfg.last_installed_version = env!("CARGO_PKG_VERSION").to_string();
    user_cfg
        .save()
        .map_err(|err| tracedecay::errors::TraceDecayError::Config {
            message: format!("failed to save user config: {err}"),
        })?;

    tracedecay::agents::offer_git_post_commit_hook(&tracedecay_bin);
    Ok(())
}

pub(crate) async fn handle_reinstall_command() -> tracedecay::errors::Result<()> {
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let tracedecay_bin = tracedecay::agents::which_tracedecay().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "tracedecay not found on PATH".to_string(),
        }
    })?;
    let mut user_cfg = tracedecay::user_config::UserConfig::load();
    tracedecay::agents::migrate_installed_agents(&home, &mut user_cfg);

    if user_cfg.installed_agents.is_empty() {
        eprintln!("No installed agents found. Run `tracedecay install` first.");
    } else {
        let agents = user_cfg.installed_agents.clone();
        eprintln!(
            "Reinstalling {} agent(s): {}",
            agents.len(),
            agents.join(", ")
        );
        let results = reinstall_agent_integrations(&agents, &home, &tracedecay_bin).await;
        let failed: Vec<String> = results
            .iter()
            .filter_map(|(id, result)| result.as_ref().err().map(|_| id.clone()))
            .collect();
        if !failed.is_empty() {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to reinstall agent(s): {}", failed.join(", ")),
            });
        }
        eprintln!("\x1b[32m✔\x1b[0m All agents reinstalled");
        user_cfg.last_installed_version = env!("CARGO_PKG_VERSION").to_string();
        user_cfg
            .save()
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
    }
    Ok(())
}

/// Re-runs `install()` + `post_install()` for each tracked agent id, returning
/// only the ids that resolve to a real integration paired with their install
/// result.
///
/// An id that does NOT resolve to an integration (a later release renamed or
/// removed it, or a typo landed in `installed_agents`) is SKIPPED, not failed:
/// it is logged as a warning and left out of the returned results entirely.
/// Gating version-marker advancement on such an id would wedge the reinstall
/// loop forever — `migrate_installed_agents` only ever adds ids, never prunes,
/// so a stale id would never resolve and the markers would never advance. Only
/// genuine `install()` failures are reported as `Err` so they still gate
/// markers.
pub(crate) async fn reinstall_agent_integrations(
    agent_ids: &[String],
    home: &Path,
    tracedecay_bin: &str,
) -> Vec<(String, tracedecay::errors::Result<()>)> {
    let project_path = std::env::current_dir().ok();
    let mut results = Vec::new();
    let hermes_migration_error = if agent_ids.iter().any(|id| id == "hermes") {
        migrate_legacy_hermes_data(home)
            .await
            .err()
            .map(|error| error.to_string())
    } else {
        None
    };
    for id in agent_ids {
        let ag = match tracedecay::agents::get_integration(id) {
            Ok(ag) => ag,
            Err(_) => {
                eprintln!(
                    "  \x1b[33mwarning:\x1b[0m skipping unknown tracked agent id \"{id}\" \
                     (no such integration); it will not gate the version-marker refresh."
                );
                continue;
            }
        };
        if id == "hermes"
            && let Some(message) = hermes_migration_error.as_ref()
        {
            results.push((
                id.clone(),
                Err(tracedecay::errors::TraceDecayError::Config {
                    message: message.clone(),
                }),
            ));
            continue;
        }
        let ctx = tracedecay::agents::InstallContext {
            home: home.to_path_buf(),
            tracedecay_bin: tracedecay_bin.to_string(),
            tool_permissions: tracedecay::agents::expected_tool_perms(),
            project_root: None,
            dashboard: true,
        };
        let result = match ag.install(&ctx) {
            Ok(()) => {
                ag.post_install(project_path.as_deref()).await;
                Ok(())
            }
            Err(e) => Err(e),
        };
        results.push((id.clone(), result));
    }
    results
}

pub(crate) async fn handle_uninstall_command(
    agent: Option<String>,
) -> tracedecay::errors::Result<()> {
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let mut user_cfg = tracedecay::user_config::UserConfig::load();
    tracedecay::agents::migrate_installed_agents(&home, &mut user_cfg);

    if agent.as_deref() == Some("hermes")
        || (agent.is_none() && user_cfg.installed_agents.iter().any(|id| id == "hermes"))
    {
        migrate_legacy_hermes_data(&home).await?;
    }

    if let Some(id) = agent {
        let ag = tracedecay::agents::get_integration(&id)?;
        let ctx = tracedecay::agents::InstallContext {
            home: home.clone(),
            tracedecay_bin: String::new(),
            tool_permissions: tracedecay::agents::expected_tool_perms(),
            project_root: None,
            dashboard: true,
        };
        ag.uninstall(&ctx)?;
        user_cfg.installed_agents.retain(|a| a != &id);
        user_cfg
            .save()
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
    } else {
        for id in user_cfg.installed_agents.clone() {
            if let Ok(ag) = tracedecay::agents::get_integration(&id) {
                let ctx = tracedecay::agents::InstallContext {
                    home: home.clone(),
                    tracedecay_bin: String::new(),
                    tool_permissions: tracedecay::agents::expected_tool_perms(),
                    project_root: None,
                    dashboard: true,
                };
                ag.uninstall(&ctx).ok();
            }
        }
        user_cfg.installed_agents.clear();
        user_cfg
            .save()
            .map_err(|err| tracedecay::errors::TraceDecayError::Config {
                message: format!("failed to save user config: {err}"),
            })?;
        eprintln!("All agent integrations removed.");
    }
    Ok(())
}
