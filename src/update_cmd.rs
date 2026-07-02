//! The `upgrade` / `update` / `post-update` / `update-plugin` flow: binary
//! upgrade via subprocess re-exec, generated-plugin refresh, daemon service
//! refresh, and the post-update health pass.

use std::path::{Path, PathBuf};

use tracedecay::upgrade::UpgradeOutcome;
use tracedecay::user_config::UserConfig;

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

/// How the `post-update` re-exec reacts to the binary-upgrade outcome.
pub(crate) enum RefreshPolicy {
    /// `update`: refresh even when nothing was installed, and a refresh
    /// failure fails the command.
    Always,
    /// `upgrade`: refresh only after a real install, and a refresh failure
    /// only warns — the binary upgrade itself already succeeded (mirroring
    /// how the health pass inside `post-update` is best-effort).
    AfterInstall,
}

/// The shared `update` / `upgrade` flow: install the new binary, then re-exec
/// the NEW binary's `post-update` subcommand — passed the freshly installed
/// binary path, when known — so the plugin refresh, daemon refresh, and
/// health pass run on the new version. `policy` decides whether the refresh
/// runs on a no-op upgrade and whether a refresh failure is fatal.
pub(crate) fn run_update_steps<U, P>(
    policy: RefreshPolicy,
    upgrade: U,
    post_update: P,
) -> tracedecay::errors::Result<()>
where
    U: FnOnce() -> tracedecay::errors::Result<UpgradeOutcome>,
    P: FnOnce(Option<&Path>) -> tracedecay::errors::Result<()>,
{
    let outcome = upgrade()?;
    match policy {
        RefreshPolicy::Always => {
            let binary = match &outcome {
                UpgradeOutcome::Installed { binary } => binary.as_deref(),
                UpgradeOutcome::AlreadyCurrent => None,
            };
            post_update(binary)
        }
        RefreshPolicy::AfterInstall => match outcome {
            UpgradeOutcome::Installed { binary } => {
                if let Err(error) = post_update(binary.as_deref()) {
                    // Point the retry at the installed binary when we know
                    // where it lives — a bare `tracedecay` may not be on PATH.
                    let retry = match &binary {
                        Some(path) => format!("`{} update`", path.display()),
                        None => "`tracedecay update`".to_string(),
                    };
                    eprintln!(
                        "  \x1b[33mwarning:\x1b[0m post-upgrade refresh failed: {error}\n  \
                         The new binary is installed; run {retry} to retry the \
                         plugin refresh and health pass."
                    );
                }
                Ok(())
            }
            UpgradeOutcome::AlreadyCurrent => {
                eprintln!(
                    "Nothing was installed, so plugins were left untouched — \
                     run `tracedecay update` to refresh generated plugins anyway."
                );
                Ok(())
            }
        },
    }
}

pub(crate) fn run_update_command(no_heal: bool) -> tracedecay::errors::Result<()> {
    run_update_steps(
        RefreshPolicy::Always,
        tracedecay::upgrade::run_upgrade,
        |binary| run_post_update_subcommand(no_heal, binary),
    )
}

pub(crate) fn run_upgrade_command(no_heal: bool) -> tracedecay::errors::Result<()> {
    run_update_steps(
        RefreshPolicy::AfterInstall,
        tracedecay::upgrade::run_upgrade,
        |binary| run_post_update_subcommand(no_heal, binary),
    )
}

/// The binary to re-exec for `post-update`: the freshly installed one when
/// the upgrade reported where it landed, otherwise the usual resolution.
/// Never `which_tracedecay()` alone — its current-exe-first order can point
/// at the OLD binary (e.g. a stale Homebrew keg) right after an upgrade.
fn post_update_binary(installed: Option<&Path>) -> tracedecay::errors::Result<String> {
    match installed.filter(|path| path.exists()) {
        Some(path) => Ok(path.to_string_lossy().into_owned()),
        None => tracedecay_bin_on_path(),
    }
}

fn run_post_update_subcommand(
    no_heal: bool,
    installed: Option<&Path>,
) -> tracedecay::errors::Result<()> {
    let tracedecay_bin = post_update_binary(installed)?;
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

/// Marks `running` as fully installed in the version markers, returning
/// whether anything changed. Run at the end of a successful `post-update` so
/// the next ordinary command's startup maintenance does not re-run the
/// plugin refresh (silent reinstall) that `post-update` just performed.
pub(crate) fn mark_running_version_installed(config: &mut UserConfig, running: &str) -> bool {
    if config.previous_version == running && config.last_installed_version == running {
        return false;
    }
    config.previous_version = running.to_string();
    config.last_installed_version = running.to_string();
    true
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

    let mut config = UserConfig::load();
    if mark_running_version_installed(&mut config, env!("CARGO_PKG_VERSION")) {
        config.save();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::{Path, PathBuf};

    use super::{post_update_binary, run_update_steps, RefreshPolicy};
    use tracedecay::upgrade::UpgradeOutcome;

    fn config_err(message: &str) -> tracedecay::errors::TraceDecayError {
        tracedecay::errors::TraceDecayError::Config {
            message: message.to_string(),
        }
    }

    /// Closure factory for the upgrade step: records `label`, returns `result`.
    fn record_upgrade<'a>(
        calls: &'a RefCell<Vec<&'static str>>,
        label: &'static str,
        result: tracedecay::errors::Result<UpgradeOutcome>,
    ) -> impl FnOnce() -> tracedecay::errors::Result<UpgradeOutcome> + 'a {
        move || {
            calls.borrow_mut().push(label);
            result
        }
    }

    /// Closure factory for the post-update step: records `label` and the
    /// binary path it was handed, returns `result`.
    fn record_post_update<'a>(
        calls: &'a RefCell<Vec<&'static str>>,
        label: &'static str,
        seen_binary: &'a RefCell<Option<Option<PathBuf>>>,
        result: tracedecay::errors::Result<()>,
    ) -> impl FnOnce(Option<&Path>) -> tracedecay::errors::Result<()> + 'a {
        move |binary| {
            calls.borrow_mut().push(label);
            *seen_binary.borrow_mut() = Some(binary.map(Path::to_path_buf));
            result
        }
    }

    #[test]
    fn update_policy_runs_post_update_after_upgrade() {
        let calls = RefCell::new(Vec::new());
        let seen_binary = RefCell::new(None);

        run_update_steps(
            RefreshPolicy::Always,
            record_upgrade(&calls, "upgrade", Ok(UpgradeOutcome::AlreadyCurrent)),
            record_post_update(&calls, "post-update", &seen_binary, Ok(())),
        )
        .expect("update steps should succeed");

        assert_eq!(calls.into_inner(), vec!["upgrade", "post-update"]);
        // Nothing was installed, so no installed-binary path to prefer.
        assert_eq!(seen_binary.into_inner(), Some(None));
    }

    #[test]
    fn update_policy_stops_after_upgrade_failure() {
        let calls = RefCell::new(Vec::new());
        let seen_binary = RefCell::new(None);

        let result = run_update_steps(
            RefreshPolicy::Always,
            record_upgrade(&calls, "upgrade", Err(config_err("upgrade failed"))),
            record_post_update(&calls, "post-update", &seen_binary, Ok(())),
        );

        assert!(result.is_err());
        assert_eq!(calls.into_inner(), vec!["upgrade"]);
    }

    #[test]
    fn update_policy_treats_post_update_failure_as_fatal() {
        let calls = RefCell::new(Vec::new());
        let seen_binary = RefCell::new(None);

        let result = run_update_steps(
            RefreshPolicy::Always,
            record_upgrade(&calls, "upgrade", Ok(UpgradeOutcome::AlreadyCurrent)),
            record_post_update(
                &calls,
                "post-update",
                &seen_binary,
                Err(config_err("plugin refresh failed")),
            ),
        );

        assert!(result.is_err());
        assert_eq!(calls.into_inner(), vec!["upgrade", "post-update"]);
    }

    #[test]
    fn upgrade_policy_forwards_installed_binary_to_post_update() {
        let calls = RefCell::new(Vec::new());
        let seen_binary = RefCell::new(None);
        let installed = PathBuf::from("/opt/homebrew/bin/tracedecay");

        run_update_steps(
            RefreshPolicy::AfterInstall,
            record_upgrade(
                &calls,
                "upgrade",
                Ok(UpgradeOutcome::Installed {
                    binary: Some(installed.clone()),
                }),
            ),
            record_post_update(&calls, "post-update", &seen_binary, Ok(())),
        )
        .expect("upgrade steps should succeed");

        assert_eq!(calls.into_inner(), vec!["upgrade", "post-update"]);
        // The refresh must re-exec the binary the upgrade just installed,
        // never a re-resolved (possibly stale) one.
        assert_eq!(seen_binary.into_inner(), Some(Some(installed)));
    }

    #[test]
    fn upgrade_policy_skips_post_update_when_already_current() {
        let calls = RefCell::new(Vec::new());
        let seen_binary = RefCell::new(None);

        run_update_steps(
            RefreshPolicy::AfterInstall,
            record_upgrade(&calls, "upgrade", Ok(UpgradeOutcome::AlreadyCurrent)),
            record_post_update(&calls, "post-update", &seen_binary, Ok(())),
        )
        .expect("an up-to-date upgrade should stay a successful no-op");

        assert_eq!(calls.into_inner(), vec!["upgrade"]);
        assert_eq!(seen_binary.into_inner(), None);
    }

    #[test]
    fn upgrade_policy_tolerates_post_update_failure() {
        let calls = RefCell::new(Vec::new());
        let seen_binary = RefCell::new(None);

        let result = run_update_steps(
            RefreshPolicy::AfterInstall,
            record_upgrade(
                &calls,
                "upgrade",
                Ok(UpgradeOutcome::Installed { binary: None }),
            ),
            record_post_update(
                &calls,
                "post-update",
                &seen_binary,
                Err(config_err("plugin refresh failed")),
            ),
        );

        // The binary upgrade itself succeeded — a refresh failure only warns.
        assert!(result.is_ok());
        assert_eq!(calls.into_inner(), vec!["upgrade", "post-update"]);
    }

    #[test]
    fn upgrade_policy_stops_after_upgrade_failure() {
        let calls = RefCell::new(Vec::new());
        let seen_binary = RefCell::new(None);

        let result = run_update_steps(
            RefreshPolicy::AfterInstall,
            record_upgrade(&calls, "upgrade", Err(config_err("upgrade failed"))),
            record_post_update(&calls, "post-update", &seen_binary, Ok(())),
        );

        assert!(result.is_err());
        assert_eq!(calls.into_inner(), vec!["upgrade"]);
    }

    #[test]
    fn post_update_binary_prefers_the_freshly_installed_path() {
        let temp = tempfile::tempdir().expect("tempdir should exist");
        let installed = temp.path().join("tracedecay");
        std::fs::write(&installed, b"new-binary").expect("binary should be writable");

        let resolved = post_update_binary(Some(&installed)).expect("installed path should resolve");

        assert_eq!(resolved, installed.to_string_lossy());
    }

    #[test]
    fn post_update_binary_ignores_a_missing_installed_path() {
        let temp = tempfile::tempdir().expect("tempdir should exist");
        let missing = temp.path().join("does-not-exist/tracedecay");

        // A dangling path (e.g. brew cleaned the keg) must fall back to the
        // normal resolution instead of re-execing a nonexistent file.
        let resolved = post_update_binary(Some(&missing));

        if let Ok(resolved) = resolved {
            assert_ne!(resolved, missing.to_string_lossy());
        }
    }
}
