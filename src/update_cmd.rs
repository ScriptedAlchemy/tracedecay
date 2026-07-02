//! The `upgrade` / `update` / `post-update` / `update-plugin` flow: binary
//! upgrade via subprocess re-exec, generated-plugin refresh, daemon service
//! refresh, and the post-update health pass.

use std::path::PathBuf;

use tracedecay::upgrade::UpgradeOutcome;

pub(crate) fn refresh_generated_plugins() -> tracedecay::errors::Result<()> {
    let home = tracedecay_home_dir()?;
    let tracedecay_bin = tracedecay_bin_on_path()?;
    eprintln!("Refreshing tracedecay-generated plugin artifacts (agent configs are not touched)");

    // Detection-driven, not `installed_agents`-driven: each integration
    // decides whether generated artifacts exist on this machine, so stale
    // tracking state can neither skip a real install nor install anywhere new.
    let mut refreshed_any = false;
    let mut config_only_installed: Vec<&'static str> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for ag in tracedecay::agents::all_integrations() {
        let ctx = tracedecay::agents::InstallContext {
            home: home.clone(),
            tracedecay_bin: tracedecay_bin.clone(),
            tool_permissions: tracedecay::agents::expected_tool_perms(),
            profile: None,
            project_root: None,
            dashboard: true,
        };
        match ag.update_plugin(&ctx) {
            Ok(tracedecay::agents::UpdatePluginOutcome::Refreshed(paths)) => {
                refreshed_any = true;
                for path in paths {
                    eprintln!(
                        "  \x1b[32m✔\x1b[0m {}: refreshed {}",
                        ag.id(),
                        path.display()
                    );
                }
            }
            Ok(tracedecay::agents::UpdatePluginOutcome::NotInstalled) => {}
            Ok(tracedecay::agents::UpdatePluginOutcome::ConfigOnly) => {
                if ag.has_tracedecay(&home) {
                    config_only_installed.push(ag.id());
                }
            }
            Err(e) => failures.push(format!("{}: {e}", ag.id())),
        }
    }
    if !config_only_installed.is_empty() {
        eprintln!(
            "  Config-managed integrations left untouched: {} (run `tracedecay reinstall` to refresh their config entries)",
            config_only_installed.join(", ")
        );
    }
    if !refreshed_any {
        eprintln!("No generated plugin installs detected — nothing to update.");
    }
    if !failures.is_empty() {
        return Err(tracedecay::errors::TraceDecayError::Config {
            message: format!("update-plugin failed for {}", failures.join("; ")),
        });
    }

    Ok(())
}

/// Rewrites and restarts the installed daemon service, returning the service
/// path and its socket, or `None` when no service is installed.
fn refresh_daemon_service() -> tracedecay::errors::Result<Option<(PathBuf, PathBuf)>> {
    let tracedecay_bin = tracedecay_bin_on_path()?;
    let spec = tracedecay::daemon::service_spec(tracedecay_bin, None)?;
    let socket_path = tracedecay::daemon::installed_service_socket_path()?
        .unwrap_or_else(|| spec.socket_path.clone());
    Ok(tracedecay::daemon::refresh_installed_service(&spec)?
        .map(|service_path| (service_path, socket_path)))
}

fn refresh_daemon_service_after_update() -> tracedecay::errors::Result<()> {
    match refresh_daemon_service()? {
        Some((service_path, socket_path)) => {
            eprintln!(
                "\x1b[32m✔\x1b[0m Daemon service refreshed at {}",
                service_path.display()
            );
            eprintln!("Daemon socket: {}", socket_path.display());
        }
        None if tracedecay::daemon::daemon_reachable() => {
            eprintln!(
                "  \x1b[33mwarning:\x1b[0m a TraceDecay daemon is running without an installed service; \
                 it keeps serving the previous version until its `tracedecay daemon run` process is restarted."
            );
        }
        None => {
            eprintln!("TraceDecay daemon service is not installed; skipping daemon restart.");
        }
    }
    Ok(())
}

pub(crate) fn restart_daemon_service() -> tracedecay::errors::Result<()> {
    match refresh_daemon_service()? {
        Some((service_path, socket_path)) => {
            eprintln!(
                "\x1b[32m✔\x1b[0m Daemon service restarted at {}",
                service_path.display()
            );
            eprintln!("Daemon socket: {}", socket_path.display());
            Ok(())
        }
        None => Err(tracedecay::errors::TraceDecayError::Config {
            message: "no TraceDecay daemon service is installed — restart your `tracedecay daemon run` \
                      process manually, or run `tracedecay daemon install-service` to manage it as a service"
                .to_string(),
        }),
    }
}

fn tracedecay_home_dir() -> tracedecay::errors::Result<PathBuf> {
    tracedecay::agents::home_dir().ok_or_else(|| tracedecay::errors::TraceDecayError::Config {
        message: "could not determine home directory".to_string(),
    })
}

pub(crate) fn tracedecay_bin_on_path() -> tracedecay::errors::Result<String> {
    tracedecay::agents::which_tracedecay().ok_or_else(|| {
        tracedecay::errors::TraceDecayError::Config {
            message: "tracedecay not found on PATH".to_string(),
        }
    })
}

pub(crate) fn run_update_steps<U, P>(
    mut upgrade: U,
    mut post_update: P,
) -> tracedecay::errors::Result<()>
where
    U: FnMut() -> tracedecay::errors::Result<()>,
    P: FnMut() -> tracedecay::errors::Result<()>,
{
    upgrade()?;
    post_update()?;
    Ok(())
}

pub(crate) fn run_update_command(no_heal: bool) -> tracedecay::errors::Result<()> {
    run_update_steps(
        || tracedecay::upgrade::run_upgrade().map(|_| ()),
        || run_post_update_subcommand(no_heal),
    )
}

/// The `upgrade` flow: install the new binary, then — only when something was
/// actually installed — re-exec the NEW binary's `post-update` subcommand so
/// the plugin refresh, daemon refresh, and health pass run on the new version.
///
/// Unlike `update`, a refresh failure only warns: the binary upgrade itself
/// succeeded, so the command must not report failure (mirroring how the
/// health pass inside `post-update` is best-effort).
pub(crate) fn run_upgrade_steps<U, P>(
    mut upgrade: U,
    mut post_update: P,
) -> tracedecay::errors::Result<()>
where
    U: FnMut() -> tracedecay::errors::Result<UpgradeOutcome>,
    P: FnMut() -> tracedecay::errors::Result<()>,
{
    match upgrade()? {
        UpgradeOutcome::Installed => {
            if let Err(error) = post_update() {
                eprintln!(
                    "  \x1b[33mwarning:\x1b[0m post-upgrade refresh failed: {error}\n  \
                     The new binary is installed; run `tracedecay update` to retry the \
                     plugin refresh and health pass."
                );
            }
            Ok(())
        }
        UpgradeOutcome::AlreadyUpToDate => {
            eprintln!(
                "Nothing was installed, so plugins were left untouched — \
                 run `tracedecay update` to refresh generated plugins anyway."
            );
            Ok(())
        }
    }
}

pub(crate) fn run_upgrade_command(no_heal: bool) -> tracedecay::errors::Result<()> {
    run_upgrade_steps(tracedecay::upgrade::run_upgrade, || {
        run_post_update_subcommand(no_heal)
    })
}

fn run_post_update_subcommand(no_heal: bool) -> tracedecay::errors::Result<()> {
    let tracedecay_bin = tracedecay_bin_on_path()?;
    let mut command = std::process::Command::new(&tracedecay_bin);
    command.arg("post-update");
    if no_heal {
        command.arg("--no-heal");
    }
    let status = command
        .status()
        .map_err(|e| tracedecay::errors::TraceDecayError::Config {
            message: format!("failed to run post-update with '{tracedecay_bin}': {e}"),
        })?;
    if status.success() {
        return Ok(());
    }
    Err(tracedecay::errors::TraceDecayError::Config {
        message: format!("post-update failed with status: {status}"),
    })
}

pub(crate) async fn run_post_update_tasks(no_heal: bool) -> tracedecay::errors::Result<()> {
    refresh_generated_plugins()?;
    if let Err(error) = refresh_daemon_service_after_update() {
        eprintln!("  \x1b[33mwarning:\x1b[0m daemon service refresh failed: {error}");
    }
    if no_heal {
        eprintln!("Skipping post-update health pass (--no-heal).");
    } else {
        tracedecay::doctor::heal::run_post_update_health_pass().await;
    }
    Ok(())
}
