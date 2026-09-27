//! Typed per-host outcomes and exit codes of host lifecycle sweeps and the
//! post-update refresh.

use std::fs;
use std::path::Path;
use std::process::Output;

use tracedecay_agent_hosts::agents::host_bundle::HostKindV1;

use super::{IsolatedCli, VERIFY_FAILURE_ENV, assert_success, host_case, seed_host};

/// `EX_TEMPFAIL`: nothing failed, but a host needs an operator step.
const PENDING_OPERATOR_ACTION_EXIT: i32 = 75;

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn install_cline(cli: &IsolatedCli) {
    seed_host(host_case(HostKindV1::Cline), cli);
    assert_success(
        "cline",
        "install",
        cli.run(&["install", "--agent", "cline"]),
    );
}

/// A Kiro MCP registry that still names TraceDecay, as an older binary or a
/// removed Kiro install leaves behind.
fn seed_leftover_kiro_registration(cli: &IsolatedCli) {
    let path = cli.home.path().join(".kiro/settings/mcp.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        br#"{"mcpServers":{"tracedecay":{"command":"tracedecay","args":["serve"]}}}"#,
    )
    .unwrap();
}

fn tracked_agents(cli: &IsolatedCli) -> String {
    fs::read_to_string(cli.profile.join("config.toml")).unwrap()
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// What Kimi Code's interactive `/plugins install <staged>` writes: a managed
/// copy of the staged source and an enabled `installed.json` entry for it.
fn complete_kimi_plugins_install(cli: &IsolatedCli, staged: &Path) {
    let code_home = cli.home.path().join(".kimi-code");
    let managed = code_home.join("plugins/managed/tracedecay");
    copy_dir(staged, &managed);
    fs::write(
        code_home.join("plugins/installed.json"),
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "plugins": [{
                "id": "tracedecay",
                "root": managed,
                "source": "local-path",
                "originalSource": staged,
                "enabled": true
            }]
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn update_plugin_skips_a_leftover_host_whose_cli_is_not_installed() {
    let cli = IsolatedCli::new();
    install_cline(&cli);
    seed_leftover_kiro_registration(&cli);

    let output = cli.run_without_host_clis(&["update-plugin"]);

    let stderr = stderr(&output);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("cline: refreshed"), "{stderr}");
    assert!(
        stderr.contains("kiro: skipped, host CLI not installed"),
        "{stderr}"
    );
    assert!(
        !tracked_agents(&cli).contains("\"kiro\""),
        "a skipped leftover host must not become tracked"
    );
}

fn track_kiro(cli: &IsolatedCli) {
    let config = cli.profile.join("config.toml");
    let tracked = tracked_agents(cli);
    fs::write(
        &config,
        tracked.replace(
            "installed_agents = [\"cline\"]",
            "installed_agents = [\"cline\", \"kiro\"]",
        ),
    )
    .unwrap();
}

const KIRO_PENDING_HOST_CLI: &str = "  kiro: pending operator action: install the kiro CLI, or run \
     `tracedecay uninstall --agent kiro` to stop tracking it\n      config error: host bundle \
     lifecycle failed: Kiro host CLI is unavailable; install the host CLI or add it to PATH \
     before retrying\n";

#[test]
fn update_plugin_waits_on_the_operator_for_a_tracked_host_whose_cli_is_not_installed() {
    let cli = IsolatedCli::new();
    install_cline(&cli);
    seed_leftover_kiro_registration(&cli);
    track_kiro(&cli);

    let output = cli.run_without_host_clis(&["update-plugin"]);

    let stderr = stderr(&output);
    assert_eq!(
        output.status.code(),
        Some(PENDING_OPERATOR_ACTION_EXIT),
        "{stderr}"
    );
    assert!(stderr.contains("  cline: refreshed\n"), "{stderr}");
    assert!(stderr.contains(KIRO_PENDING_HOST_CLI), "{stderr}");
    let tracked = tracked_agents(&cli);
    assert!(
        tracked.contains("\"kiro\""),
        "the pending host stays tracked until the operator decides: {tracked}"
    );

    let untrack = cli.run_without_host_clis(&["uninstall", "--agent", "kiro"]);
    let untrack_stderr = self::stderr(&untrack);
    assert_eq!(untrack.status.code(), Some(0), "{untrack_stderr}");
    assert!(
        untrack_stderr.contains(
            "  kiro: no longer tracked; the host CLI is not installed, so its host-owned \
             registration was left in place (config error: host bundle lifecycle failed: Kiro \
             host CLI is unavailable; install the host CLI or add it to PATH before retrying)\n"
        ),
        "{untrack_stderr}"
    );
    let tracked = tracked_agents(&cli);
    assert!(
        !tracked.contains("\"kiro\"") && tracked.contains("\"cline\""),
        "the printed uninstall stops tracking only that host: {tracked}"
    );

    let after = cli.run_without_host_clis(&["update-plugin"]);
    let after_stderr = self::stderr(&after);
    assert_eq!(after.status.code(), Some(0), "{after_stderr}");
    assert!(
        after_stderr.contains("  kiro: skipped, host CLI not installed; only leftover"),
        "the untracked leftover is skipped, not pending: {after_stderr}"
    );
}

/// A refresh that finds a host's MCP registration already in place reports it
/// unchanged and leaves the operator's config alone: same bytes, same inode.
#[cfg(unix)]
#[test]
fn update_plugin_reports_registrations_already_in_place_as_unchanged() {
    use std::os::unix::fs::MetadataExt;

    let cli = IsolatedCli::new();
    let hosts = [
        (HostKindV1::Cline, ".cline/mcp.json"),
        (HostKindV1::Devin, ".config/devin/mcp_config.json"),
        (
            HostKindV1::RooCode,
            ".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json",
        ),
        (HostKindV1::Kilo, ".config/kilo/kilo.jsonc"),
        (HostKindV1::OpenCode, ".config/opencode/opencode.json"),
    ];
    let mut installed = Vec::new();
    for (host, relative) in hosts {
        let case = host_case(host);
        seed_host(case, &cli);
        let install = cli.run_without_host_clis(&["install", "--agent", case.id]);
        let install_stderr = stderr(&install);
        assert_eq!(install.status.code(), Some(0), "{install_stderr}");
        let path = cli.home.path().join(relative);
        assert!(
            install_stderr.contains(&format!(
                "Added tracedecay MCP server to {}\n",
                path.display()
            )),
            "{install_stderr}"
        );
        let metadata = fs::metadata(&path).unwrap();
        installed.push((path.clone(), fs::read(&path).unwrap(), metadata.ino()));
    }

    let refresh = cli.run_without_host_clis(&["update-plugin"]);

    let refresh_stderr = stderr(&refresh);
    assert_eq!(refresh.status.code(), Some(0), "{refresh_stderr}");
    for (path, bytes, inode) in installed {
        assert!(
            refresh_stderr.contains(&format!(
                "  tracedecay MCP server unchanged in {}\n",
                path.display()
            )),
            "{refresh_stderr}"
        );
        assert!(
            !refresh_stderr.contains(&format!("tracedecay MCP server to {}", path.display())),
            "{refresh_stderr}"
        );
        assert_eq!(fs::read(&path).unwrap(), bytes, "{}", path.display());
        assert_eq!(
            fs::metadata(&path).unwrap().ino(),
            inode,
            "{} was republished",
            path.display()
        );
    }
}

#[test]
fn install_fails_an_untracked_named_host_whose_cli_is_not_installed() {
    let cli = IsolatedCli::new();

    let output = cli.run_without_host_clis(&["install", "--agent", "kiro"]);

    let stderr = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(
            "  kiro: failed, host CLI not installed: config error: host bundle lifecycle \
             failed: Kiro host CLI is unavailable; install the host CLI or add it to PATH \
             before retrying\n"
        ),
        "{stderr}"
    );
}

/// The operator's `tracedecay update` journey: a tracked host whose CLI is
/// gone must not mask the pending Kimi step behind a failure exit.
#[test]
fn post_update_reports_a_tracked_host_without_its_cli_as_pending_beside_kimi() {
    let cli = IsolatedCli::new();
    install_cline(&cli);
    track_kiro(&cli);
    let _ = cli.run(&["install", "--agent", "kimi"]);

    // Post-update still needs the service manager, so only Kiro's CLI goes.
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(cli.bin_dir.clone())
            .chain(std::env::split_paths(&inherited).filter(|dir| {
                !dir.join("kiro-cli").exists() && !dir.join("kiro-cli.exe").exists()
            })),
    )
    .unwrap();
    let output = cli
        .command(&["post-update"])
        .env("PATH", path)
        .output()
        .unwrap();

    let stderr = stderr(&output);
    assert_eq!(
        output.status.code(),
        Some(PENDING_OPERATOR_ACTION_EXIT),
        "{stderr}"
    );
    assert!(stderr.contains("  cline: refreshed\n"), "{stderr}");
    assert!(stderr.contains(KIRO_PENDING_HOST_CLI), "{stderr}");
    assert!(
        stderr.contains("  kimi: pending operator action: `/plugins install"),
        "{stderr}"
    );
    assert!(!stderr.contains("reinstall failed for"), "{stderr}");
}

/// The daemon serving the isolated profile while it is held: Doctor reads its
/// canonical report from the sole daemon owner and counts its absence as an
/// issue.
#[cfg(unix)]
struct ProfileDaemon(std::process::Child);

#[cfg(unix)]
impl ProfileDaemon {
    fn start(cli: &IsolatedCli) -> Self {
        let mut command = cli.command(&["daemon", "run"]);
        command
            .env("TRACEDECAY_TEST_ALLOW_INCOMPLETE_HOLDER_SCAN", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let daemon = Self(command.spawn().unwrap());
        let socket = cli.profile.join("daemon.sock");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            assert!(
                std::time::Instant::now() < deadline,
                "the profile daemon never listened at {}",
                socket.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        daemon
    }
}

#[cfg(unix)]
impl Drop for ProfileDaemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Doctor classifies the same operator steps `update-plugin` does: Kimi's
/// pending `/plugins install` and a tracked host without its CLI exit 75,
/// and the run converges to 0 once the operator acts on both.
#[cfg(unix)]
#[test]
fn doctor_waits_on_the_operator_for_the_steps_update_plugin_reports() {
    let cli = IsolatedCli::new();
    install_cline(&cli);
    track_kiro(&cli);
    let _ = cli.run(&["install", "--agent", "kimi"]);
    let _daemon = ProfileDaemon::start(&cli);
    let staged = cli
        .home
        .path()
        .join(".tracedecay/host-bundle-stage/kimi/tracedecay");

    let doctor = cli.run_without_host_clis(&["doctor"]);
    let doctor_stderr = stderr(&doctor);
    assert_eq!(
        doctor.status.code(),
        Some(PENDING_OPERATOR_ACTION_EXIT),
        "{doctor_stderr}"
    );
    assert!(
        doctor_stderr.contains(
            "kiro: pending operator action: install the kiro CLI, or run `tracedecay \
             uninstall --agent kiro` to stop tracking it"
        ),
        "{doctor_stderr}"
    );
    assert!(
        doctor_stderr.contains(&format!(
            "pending operator action: open Kimi Code and run `/plugins install {}`",
            staged.display()
        )),
        "{doctor_stderr}"
    );

    complete_kimi_plugins_install(&cli, &staged);
    let untrack = cli.run_without_host_clis(&["uninstall", "--agent", "kiro"]);
    assert_eq!(untrack.status.code(), Some(0), "{}", stderr(&untrack));
    let converged = cli.run_without_host_clis(&["doctor"]);
    let converged_stderr = stderr(&converged);
    assert_eq!(converged.status.code(), Some(0), "{converged_stderr}");
    assert!(
        !converged_stderr.contains("pending operator action"),
        "{converged_stderr}"
    );
}

#[test]
fn update_plugin_exits_nonzero_when_an_attempted_host_fails() {
    let cli = IsolatedCli::new();
    install_cline(&cli);

    let output = cli.run_with_env(&["update-plugin"], VERIFY_FAILURE_ENV, "1");

    let stderr = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("cline: failed:"), "{stderr}");
}

#[test]
fn kimi_reports_pending_operator_action_until_its_plugins_install_runs() {
    let cli = IsolatedCli::new();
    let staged = cli
        .home
        .path()
        .join(".tracedecay/host-bundle-stage/kimi/tracedecay");
    let command = format!("/plugins install {}", staged.display());

    let install = cli.run(&["install", "--agent", "kimi"]);
    let install_stderr = stderr(&install);
    assert_eq!(
        install.status.code(),
        Some(PENDING_OPERATOR_ACTION_EXIT),
        "{install_stderr}"
    );
    assert!(
        install_stderr.contains(&format!("kimi: pending operator action: `{command}`")),
        "{install_stderr}"
    );

    let update = cli.run(&["update-plugin"]);
    let update_stderr = stderr(&update);
    assert_eq!(
        update.status.code(),
        Some(PENDING_OPERATOR_ACTION_EXIT),
        "the pending host stays tracked and the sweep keeps reporting it: {update_stderr}"
    );
    assert!(
        update_stderr.contains(&format!("kimi: pending operator action: `{command}`")),
        "{update_stderr}"
    );

    let doctor = stderr(&cli.run(&["doctor"]));
    assert!(
        doctor.contains(&format!(
            "pending operator action: open Kimi Code and run `{command}`"
        )),
        "{doctor}"
    );

    complete_kimi_plugins_install(&cli, &staged);

    let doctor = stderr(&cli.run(&["doctor"]));
    assert!(!doctor.contains("pending operator action"), "{doctor}");
    let converged = cli.run(&["update-plugin"]);
    assert_eq!(converged.status.code(), Some(0), "{}", stderr(&converged));
}

#[test]
fn post_update_exits_nonzero_when_a_host_refresh_fails() {
    let cli = IsolatedCli::new();
    install_cline(&cli);

    let output = cli.run_with_env(&["post-update"], VERIFY_FAILURE_ENV, "1");

    let stderr = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("cline: failed:"), "{stderr}");
    assert!(
        !stderr.contains("retried on the next tracedecay command"),
        "nothing retries a failed refresh implicitly: {stderr}"
    );
}

#[test]
fn post_update_reports_pending_operator_action_with_its_own_exit_code() {
    let cli = IsolatedCli::new();
    install_cline(&cli);
    let _ = cli.run(&["install", "--agent", "kimi"]);

    let output = cli.run(&["post-update"]);

    let stderr = stderr(&output);
    assert_eq!(
        output.status.code(),
        Some(PENDING_OPERATOR_ACTION_EXIT),
        "{stderr}"
    );
    assert!(stderr.contains("cline: refreshed"), "{stderr}");
    assert!(
        stderr.contains("kimi: pending operator action: `/plugins install"),
        "{stderr}"
    );
}
