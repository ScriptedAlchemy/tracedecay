use std::io;
use std::path::{Path, PathBuf};

pub(crate) fn hermes_profile_targets(
    home: &Path,
) -> tracedecay::errors::Result<Vec<Option<String>>> {
    let mut targets = vec![None];
    let profiles_dir = home.join(".hermes/profiles");
    let entries = match std::fs::read_dir(&profiles_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(targets),
        Err(e) => {
            return Err(tracedecay::errors::TraceDecayError::Config {
                message: format!(
                    "failed to read Hermes profiles directory {}: {e}",
                    profiles_dir.display()
                ),
            });
        }
    };

    let mut profile_names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| tracedecay::errors::TraceDecayError::Config {
            message: format!(
                "failed to read Hermes profiles directory {}: {e}",
                profiles_dir.display()
            ),
        })?;
        let file_type =
            entry
                .file_type()
                .map_err(|e| tracedecay::errors::TraceDecayError::Config {
                    message: format!(
                        "failed to inspect Hermes profile {}: {e}",
                        entry.path().display()
                    ),
                })?;
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().into_string().map_err(|_| {
            tracedecay::errors::TraceDecayError::Config {
                message: format!(
                    "Hermes profile path is not valid UTF-8: {}",
                    entry.path().display()
                ),
            }
        })?;
        profile_names.push(name);
    }
    profile_names.sort();
    targets.extend(profile_names.into_iter().map(Some));
    Ok(targets)
}

pub(crate) fn validate_hermes_profile_flags(
    agent: Option<&str>,
    profile: &Option<String>,
    all_profiles: bool,
) -> tracedecay::errors::Result<()> {
    if profile.is_some() && agent != Some("hermes") {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "`--profile` is only supported with `--agent hermes`".to_string(),
        });
    }
    if all_profiles && agent != Some("hermes") {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "`--all-profiles` is only supported with `--agent hermes`".to_string(),
        });
    }
    Ok(())
}

pub(crate) fn validate_hermes_project_root_flag(
    agent: Option<&str>,
    project_root: &Option<String>,
) -> tracedecay::errors::Result<Option<PathBuf>> {
    let Some(project_root) = project_root else {
        return Ok(None);
    };
    if agent != Some("hermes") {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: "`--project-root` is only supported with `--agent hermes`".to_string(),
        });
    }
    let path = PathBuf::from(project_root);
    if !path.is_absolute() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!("`--project-root` must be an absolute path, got '{project_root}'"),
        });
    }
    Ok(Some(path))
}

fn validate_codex_automation_flags(
    agent: Option<&str>,
    automation: bool,
) -> tracedecay::errors::Result<()> {
    if !automation {
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
) -> tracedecay::errors::Result<PathBuf> {
    use tracedecay::automation::config::{
        effective_config, load_project_config, merge_project_config, project_config_path,
        save_project_config, AutomationBackend, AutomationConfigPatch, AutomationHostMode,
        AutomationTaskPatch,
    };

    let cg = open_or_init_codex_daemon_automation_project(project_path).await?;
    let dashboard_root = cg.store_layout().dashboard_root.clone();
    let existing = load_project_config(&dashboard_root).await?;
    let updated = merge_project_config(
        existing,
        AutomationConfigPatch {
            enabled: Some(true),
            backend: Some(AutomationBackend::CodexAppServer),
            host_mode: Some(AutomationHostMode::Standalone),
            model: Some(Some("gpt-5.5".to_string())),
            require_dashboard_approval: Some(false),
            auto_apply_memory_ops: Some(true),
            auto_enable_skills: Some(false),
            memory_curator: codex_daemon_interval_task(15 * 60),
            session_reflector: codex_daemon_interval_task(15 * 60),
            skill_writer: AutomationTaskPatch {
                min_idle_secs: Some(Some(15 * 60)),
                ..codex_daemon_interval_task(60 * 60)
            },
            ..AutomationConfigPatch::default()
        },
    );

    let global = tracedecay::user_config::UserConfig::load().automation;
    effective_config(&global, Some(&updated))?;
    save_project_config(&dashboard_root, &updated).await?;
    let path = project_config_path(&dashboard_root);
    eprintln!(
        "\x1b[32m✔\x1b[0m Enabled TraceDecay daemon automation loop at {}",
        path.display()
    );
    eprintln!(
        "  The daemon scheduler will run memory_curator, session_reflector, and skill_writer via the Codex app-server backend."
    );
    Ok(path)
}

async fn open_or_init_codex_daemon_automation_project(
    project_path: &Path,
) -> tracedecay::errors::Result<tracedecay::tracedecay::TraceDecay> {
    if tracedecay::tracedecay::TraceDecay::has_initialized_store(project_path).await {
        tracedecay::tracedecay::TraceDecay::open(project_path).await
    } else {
        tracedecay::tracedecay::TraceDecay::init(project_path).await
    }
}

fn codex_daemon_interval_task(
    interval_secs: u64,
) -> tracedecay::automation::config::AutomationTaskPatch {
    tracedecay::automation::config::AutomationTaskPatch {
        enabled: Some(true),
        schedule: Some(Some("interval".to_string())),
        interval_secs: Some(Some(interval_secs)),
        cooldown_secs: Some(Some(5 * 60)),
        ..tracedecay::automation::config::AutomationTaskPatch::default()
    }
}

pub(crate) fn hermes_selected_profile_targets(
    home: &Path,
    profile: &Option<String>,
    all_profiles: bool,
) -> tracedecay::errors::Result<Vec<Option<String>>> {
    if all_profiles {
        hermes_profile_targets(home)
    } else {
        Ok(vec![profile.clone()])
    }
}

pub(crate) async fn handle_install_command(
    agent: Option<String>,
    local: bool,
    profile: Option<String>,
    all_profiles: bool,
    project_root: Option<String>,
    no_dashboard: bool,
    automation: bool,
) -> tracedecay::errors::Result<()> {
    validate_hermes_profile_flags(agent.as_deref(), &profile, all_profiles)?;
    let pinned_project_root = validate_hermes_project_root_flag(agent.as_deref(), &project_root)?;
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
            profile: profile.clone(),
            project_root: pinned_project_root.clone(),
            dashboard: !no_dashboard,
        };
        let mut installed_names: Vec<String> = Vec::new();

        if let Some(id) = agent {
            let ag = tracedecay::agents::get_integration(&id)?;
            for target_profile in hermes_selected_profile_targets(&home, &profile, all_profiles)? {
                let ctx = tracedecay::agents::InstallContext {
                    home: home.clone(),
                    tracedecay_bin: tracedecay_bin.clone(),
                    tool_permissions: tracedecay::agents::expected_tool_perms(),
                    profile: target_profile,
                    project_root: pinned_project_root.clone(),
                    dashboard: !no_dashboard,
                };
                ag.install_local(&ctx, &project_path)?;
                ag.post_install(Some(&project_path)).await;
                if automation && id == "codex" {
                    let scoped_project_path = validate_codex_automation_project_path()?;
                    install_codex_daemon_automation(&scoped_project_path).await?;
                }
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

    let mut user_cfg = tracedecay::user_config::UserConfig::load();
    tracedecay::agents::migrate_installed_agents(&home, &mut user_cfg);

    let mut installed_names: Vec<String> = Vec::new();
    let mut removed_names: Vec<String> = Vec::new();
    let project_path = std::env::current_dir().ok();

    if let Some(id) = agent {
        let ag = tracedecay::agents::get_integration(&id)?;
        let name = ag.name().to_string();
        for target_profile in hermes_selected_profile_targets(&home, &profile, all_profiles)? {
            let ctx = tracedecay::agents::InstallContext {
                home: home.clone(),
                tracedecay_bin: tracedecay_bin.clone(),
                tool_permissions: tracedecay::agents::expected_tool_perms(),
                profile: target_profile,
                project_root: pinned_project_root.clone(),
                dashboard: !no_dashboard,
            };
            ag.install(&ctx)?;
            ag.post_install(project_path.as_deref()).await;
            if automation && id == "codex" {
                let scoped_project_path = validate_codex_automation_project_path()?;
                install_codex_daemon_automation(&scoped_project_path).await?;
            }
        }
        if !user_cfg.installed_agents.contains(&id) {
            user_cfg.installed_agents.push(id);
            installed_names.push(name);
        }
        user_cfg.save();
    } else {
        let (to_install, to_uninstall) =
            tracedecay::agents::pick_integrations_interactive(&home, &user_cfg.installed_agents)?;

        for id in &to_uninstall {
            let ag = tracedecay::agents::get_integration(id)?;
            let ctx = tracedecay::agents::InstallContext {
                home: home.clone(),
                tracedecay_bin: tracedecay_bin.clone(),
                tool_permissions: tracedecay::agents::expected_tool_perms(),
                profile: profile.clone(),
                project_root: pinned_project_root.clone(),
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
                profile: profile.clone(),
                project_root: pinned_project_root.clone(),
                dashboard: !no_dashboard,
            };
            ag.install(&ctx)?;
            ag.post_install(project_path.as_deref()).await;
            installed_names.push(ag.name().to_string());
            if !user_cfg.installed_agents.contains(id) {
                user_cfg.installed_agents.push(id.clone());
            }
        }
        user_cfg.save();
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
    user_cfg.save();

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
        let project_path = std::env::current_dir().ok();
        eprintln!(
            "Reinstalling {} agent(s): {}",
            agents.len(),
            agents.join(", ")
        );
        for id in &agents {
            let ag = tracedecay::agents::get_integration(id)?;
            let ctx = tracedecay::agents::InstallContext {
                home: home.clone(),
                tracedecay_bin: tracedecay_bin.clone(),
                tool_permissions: tracedecay::agents::expected_tool_perms(),
                profile: None,
                project_root: None,
                dashboard: true,
            };
            ag.install(&ctx)?;
            ag.post_install(project_path.as_deref()).await;
        }
        eprintln!("\x1b[32m✔\x1b[0m All agents reinstalled");
        user_cfg.last_installed_version = env!("CARGO_PKG_VERSION").to_string();
        user_cfg.save();
    }
    Ok(())
}

pub(crate) async fn handle_uninstall_command(
    agent: Option<String>,
    profile: Option<String>,
    all_profiles: bool,
) -> tracedecay::errors::Result<()> {
    validate_hermes_profile_flags(agent.as_deref(), &profile, all_profiles)?;
    let home = tracedecay::agents::home_dir().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "could not determine home directory".to_string(),
        }
    })?;
    let mut user_cfg = tracedecay::user_config::UserConfig::load();
    tracedecay::agents::migrate_installed_agents(&home, &mut user_cfg);

    if let Some(id) = agent {
        let ag = tracedecay::agents::get_integration(&id)?;
        for target_profile in hermes_selected_profile_targets(&home, &profile, all_profiles)? {
            let ctx = tracedecay::agents::InstallContext {
                home: home.clone(),
                tracedecay_bin: String::new(),
                tool_permissions: tracedecay::agents::expected_tool_perms(),
                profile: target_profile,
                project_root: None,
                dashboard: true,
            };
            ag.uninstall(&ctx)?;
        }
        user_cfg.installed_agents.retain(|a| a != &id);
        user_cfg.save();
    } else {
        for id in user_cfg.installed_agents.clone() {
            if let Ok(ag) = tracedecay::agents::get_integration(&id) {
                let ctx = tracedecay::agents::InstallContext {
                    home: home.clone(),
                    tracedecay_bin: String::new(),
                    tool_permissions: tracedecay::agents::expected_tool_perms(),
                    profile: None,
                    project_root: None,
                    dashboard: true,
                };
                ag.uninstall(&ctx).ok();
            }
        }
        user_cfg.installed_agents.clear();
        user_cfg.save();
        eprintln!("All agent integrations removed.");
    }
    Ok(())
}
