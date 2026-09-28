//! Typed per-host outcomes and exit codes of host lifecycle sweeps and the
//! post-update refresh.

use std::fs;
use std::path::Path;
use std::process::Output;

use tracedecay_agent_hosts::agents::host_bundle::HostKindV1;

#[cfg(unix)]
use super::install_kimi_cli;
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
pub(super) fn complete_kimi_plugins_install(cli: &IsolatedCli, staged: &Path) {
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

    let output = cli.run(&["update-plugin"]);

    let stderr = stderr(&output);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("cline: refreshed"), "{stderr}");
    assert!(stderr.contains(KIRO_NOT_INSTALLED), "{stderr}");
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

const KIRO_CLI_UNAVAILABLE: &str = "host CLI `kiro-cli` is unavailable for kiro MCP registry \
     lifecycle; install it or add it to PATH and retry";

const KIRO_NOT_INSTALLED: &str = "  kiro: skipped, not installed (host CLI `kiro-cli` is \
     unavailable for kiro MCP registry lifecycle; install it or add it to PATH and retry)\n";

const KIRO_NOT_SIGNED_IN: &str = "kiro: skipped, not signed in (host CLI `kiro-cli` is not \
     signed in; run `kiro-cli login` to use it)\n";

/// A host CLI `program` on the isolated `PATH` running the shell `body`.
#[cfg(unix)]
fn install_host_cli(cli: &IsolatedCli, program: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    let path = cli.bin_dir.join(program);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Kiro's CLI while logged out: it refuses every `mcp` command.
#[cfg(unix)]
const LOGGED_OUT_KIRO_CLI: &str =
    "echo 'error: You are not logged in, please log in with kiro-cli login' >&2\nexit 1";

/// Kiro's CLI once logged in: `mcp list` answers and `mcp add` registers the
/// server it names in Kiro's global registry.
#[cfg(unix)]
const SIGNED_IN_KIRO_CLI: &str = r#"case "$1 $2" in
  "mcp add")
    /bin/mkdir -p "$HOME/.kiro/settings"
    printf '{"mcpServers":{"tracedecay":{"command":"%s","args":["serve"],"disabled":false}}}\n' "$6" > "$HOME/.kiro/settings/mcp.json"
    ;;
esac
exit 0"#;

#[test]
fn update_plugin_skips_a_tracked_host_whose_cli_is_not_installed() {
    let cli = IsolatedCli::new();
    install_cline(&cli);
    seed_leftover_kiro_registration(&cli);
    track_kiro(&cli);

    let output = cli.run(&["update-plugin"]);

    let stderr = stderr(&output);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("  cline: refreshed\n"), "{stderr}");
    assert!(stderr.contains(KIRO_NOT_INSTALLED), "{stderr}");
    let tracked = tracked_agents(&cli);
    assert!(
        tracked.contains("\"kiro\""),
        "a skipped host stays tracked for when it is installed: {tracked}"
    );

    let untrack = cli.run(&["uninstall", "--agent", "kiro"]);
    let untrack_stderr = self::stderr(&untrack);
    assert_eq!(untrack.status.code(), Some(0), "{untrack_stderr}");
    assert!(
        untrack_stderr.contains(&format!(
            "  kiro: no longer tracked; the host is not installed, so its host-owned \
             registration was left in place ({KIRO_CLI_UNAVAILABLE})\n"
        )),
        "{untrack_stderr}"
    );
    let tracked = tracked_agents(&cli);
    assert!(
        !tracked.contains("\"kiro\"") && tracked.contains("\"cline\""),
        "the uninstall stops tracking only that host: {tracked}"
    );

    let after = cli.run(&["update-plugin"]);
    let after_stderr = self::stderr(&after);
    assert_eq!(after.status.code(), Some(0), "{after_stderr}");
    assert!(
        after_stderr.contains(KIRO_NOT_INSTALLED),
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
        let install = cli.run(&["install", "--agent", case.id]);
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

    let refresh = cli.run(&["update-plugin"]);

    let refresh_stderr = stderr(&refresh);
    assert_eq!(refresh.status.code(), Some(0), "{refresh_stderr}");
    // Per config: what the refresh reported, whether its bytes and its inode
    // survived.
    let observed = installed
        .iter()
        .map(|(path, bytes, inode)| {
            let path_text = path.display().to_string();
            let reported = if refresh_stderr.contains(&format!(
                "  tracedecay MCP server unchanged in {path_text}\n"
            )) {
                "unchanged"
            } else if refresh_stderr
                .contains(&format!("Added tracedecay MCP server to {path_text}\n"))
            {
                "added"
            } else {
                "unreported"
            };
            (
                path_text,
                reported,
                fs::read(path).unwrap() == *bytes,
                fs::metadata(path).unwrap().ino() == *inode,
            )
        })
        .collect::<Vec<_>>();
    let expected = installed
        .iter()
        .map(|(path, _, _)| (path.display().to_string(), "unchanged", true, true))
        .collect::<Vec<_>>();
    assert_eq!(observed, expected, "{refresh_stderr}");
}

#[test]
fn install_skips_a_named_host_whose_cli_is_not_installed() {
    let cli = IsolatedCli::new();

    let output = cli.run(&["install", "--agent", "kiro"]);

    let stderr = stderr(&output);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(KIRO_NOT_INSTALLED), "{stderr}");
    assert!(
        !cli.profile.join("config.toml").exists() || !tracked_agents(&cli).contains("\"kiro\""),
        "a skipped host is not tracked"
    );
}

/// The operator's `tracedecay update` journey: a tracked host whose CLI is
/// gone is skipped beside the pending Kimi step, which alone sets the exit.
#[cfg(unix)]
#[test]
fn post_update_skips_a_tracked_host_without_its_cli_beside_pending_kimi() {
    let cli = IsolatedCli::new();
    install_cline(&cli);
    track_kiro(&cli);
    install_kimi_cli(&cli);
    let _ = cli.run(&["install", "--agent", "kimi"]);

    let output = cli.run(&["post-update"]);

    let stderr = stderr(&output);
    assert_eq!(
        output.status.code(),
        Some(PENDING_OPERATOR_ACTION_EXIT),
        "{stderr}"
    );
    assert!(stderr.contains("  cline: refreshed\n"), "{stderr}");
    assert!(stderr.contains(KIRO_NOT_INSTALLED), "{stderr}");
    assert!(
        stderr.contains("  kimi: pending operator action: `/plugins install"),
        "{stderr}"
    );
    assert!(!stderr.contains("reinstall failed for"), "{stderr}");
}

/// Kiro's CLI installed but logged out, as #2374 reported it: `update-plugin`,
/// the `post-update` refresh `update` runs, and `doctor` all report it
/// skipped as not signed in and exit 0; once the CLI admits its commands the
/// same sweep registers Kiro.
#[cfg(unix)]
#[test]
fn a_kiro_cli_that_is_not_signed_in_is_skipped_everywhere() {
    let cli = IsolatedCli::new();
    install_cline(&cli);
    seed_leftover_kiro_registration(&cli);
    track_kiro(&cli);
    install_host_cli(&cli, "kiro-cli", LOGGED_OUT_KIRO_CLI);

    let update = cli.run(&["update-plugin"]);
    let update_stderr = stderr(&update);
    assert_eq!(update.status.code(), Some(0), "{update_stderr}");
    assert!(
        update_stderr.contains(&format!("  {KIRO_NOT_SIGNED_IN}")),
        "{update_stderr}"
    );

    let post_update = cli.run(&["post-update"]);
    let post_update_stderr = stderr(&post_update);
    assert_eq!(post_update.status.code(), Some(0), "{post_update_stderr}");
    assert!(
        post_update_stderr.contains(&format!("  {KIRO_NOT_SIGNED_IN}")),
        "{post_update_stderr}"
    );

    // `post-update` refuses while an unmanaged daemon listens; Doctor reads
    // the daemon's canonical report.
    let _daemon = ProfileDaemon::start(&cli);
    let doctor = cli.run(&["doctor"]);
    let doctor_stderr = stderr(&doctor);
    assert_eq!(doctor.status.code(), Some(0), "{doctor_stderr}");
    assert!(
        doctor_stderr.contains(&format!("  - {KIRO_NOT_SIGNED_IN}")),
        "{doctor_stderr}"
    );

    install_host_cli(&cli, "kiro-cli", SIGNED_IN_KIRO_CLI);
    let signed_in = cli.run(&["update-plugin"]);
    let signed_in_stderr = stderr(&signed_in);
    assert_eq!(signed_in.status.code(), Some(0), "{signed_in_stderr}");
    assert!(
        signed_in_stderr.contains("  kiro: refreshed\n"),
        "{signed_in_stderr}"
    );
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

/// Doctor classifies hosts the way `update-plugin` does: Kimi's pending
/// `/plugins install` exits 75, a tracked host without its CLI is skipped as
/// not installed, and the run converges to 0 once the operator installs
/// Kimi's plugin while Kiro stays tracked and absent.
#[cfg(unix)]
#[test]
fn doctor_waits_only_on_real_operator_steps_and_skips_absent_hosts() {
    let cli = IsolatedCli::new();
    install_cline(&cli);
    track_kiro(&cli);
    install_kimi_cli(&cli);
    let _ = cli.run(&["install", "--agent", "kimi"]);
    let _daemon = ProfileDaemon::start(&cli);
    let staged = cli
        .home
        .path()
        .join(".tracedecay/host-bundle-stage/kimi/tracedecay");

    let doctor = cli.run(&["doctor"]);
    let doctor_stderr = stderr(&doctor);
    assert_eq!(
        doctor.status.code(),
        Some(PENDING_OPERATOR_ACTION_EXIT),
        "{doctor_stderr}"
    );
    assert!(
        doctor_stderr.contains(&format!("  - {}", &KIRO_NOT_INSTALLED[2..])),
        "{doctor_stderr}"
    );
    assert!(
        doctor_stderr.contains(&format!(
            "pending operator action: open Kimi Code and run `/plugins install {}`",
            staged.display()
        )),
        "{doctor_stderr}"
    );
    assert!(
        doctor_stderr.contains("1 pending operator action(s), "),
        "only Kimi's step is pending: {doctor_stderr}"
    );

    complete_kimi_plugins_install(&cli, &staged);
    let converged = cli.run(&["doctor"]);
    let converged_stderr = stderr(&converged);
    assert_eq!(converged.status.code(), Some(0), "{converged_stderr}");
    assert!(
        converged_stderr.contains(&format!("  - {}", &KIRO_NOT_INSTALLED[2..])),
        "{converged_stderr}"
    );
    assert!(
        !converged_stderr.contains("pending operator action"),
        "{converged_stderr}"
    );
}

/// A tracked Kiro whose CLI is absent is skipped and exits 0; the same Kiro
/// with a reachable, signed-in CLI and a malformed TraceDecay registration is
/// an issue and exits 1.
#[cfg(unix)]
#[test]
fn doctor_skips_an_absent_host_but_fails_a_reachable_hosts_malformed_registration() {
    let cli = IsolatedCli::new();
    install_cline(&cli);
    track_kiro(&cli);
    let _daemon = ProfileDaemon::start(&cli);

    let absent = cli.run(&["doctor"]);
    let absent_stderr = stderr(&absent);
    assert_eq!(absent.status.code(), Some(0), "{absent_stderr}");
    assert!(
        absent_stderr.contains(&format!("  - {}", &KIRO_NOT_INSTALLED[2..])),
        "{absent_stderr}"
    );
    assert!(
        !absent_stderr.contains("pending operator action"),
        "{absent_stderr}"
    );

    install_host_cli(&cli, "kiro-cli", SIGNED_IN_KIRO_CLI);
    let mcp = cli.home.path().join(".kiro/settings/mcp.json");
    fs::create_dir_all(mcp.parent().unwrap()).unwrap();
    fs::write(&mcp, "{ not valid JSON").unwrap();
    let malformed = cli.run(&["doctor"]);
    let malformed_stderr = stderr(&malformed);
    assert_eq!(malformed.status.code(), Some(1), "{malformed_stderr}");
    assert!(
        malformed_stderr.contains(&format!(
            "Kiro installation state is unreadable: config error: failed to parse Kiro MCP \
             config {}",
            mcp.display()
        )),
        "{malformed_stderr}"
    );
    assert!(
        !malformed_stderr.contains("kiro: skipped"),
        "{malformed_stderr}"
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

#[cfg(unix)]
#[test]
fn kimi_reports_pending_operator_action_until_its_plugins_install_runs() {
    let cli = IsolatedCli::new();
    install_kimi_cli(&cli);
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

#[cfg(unix)]
#[test]
fn post_update_reports_pending_operator_action_with_its_own_exit_code() {
    let cli = IsolatedCli::new();
    install_cline(&cli);
    install_kimi_cli(&cli);
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

/// Copilot's own registry as `copilot mcp add` maintains it.
#[cfg(unix)]
const REGISTERING_COPILOT_CLI: &str = r#"case "$1 $2" in
  "mcp add")
    /bin/mkdir -p "$HOME/.copilot"
    printf '{"mcpServers":{"tracedecay":{"command":"%s","args":["serve"]}}}\n' "$5" > "$HOME/.copilot/mcp-config.json"
    ;;
esac
exit 0"#;

/// No TraceDecay lifecycle writes VS Code's `settings.json`, so after a
/// completed Copilot install a VS Code profile without the server is an
/// observation, not an issue whose remedy is a TraceDecay command.
#[cfg(unix)]
#[test]
fn doctor_observes_copilots_vscode_settings_without_failing_on_them() {
    let cli = IsolatedCli::new();
    install_host_cli(&cli, "copilot", REGISTERING_COPILOT_CLI);
    let vscode_settings =
        tracedecay_agent_hosts::agents::vscode_data_dir(cli.home.path()).join("User/settings.json");
    fs::create_dir_all(vscode_settings.parent().unwrap()).unwrap();
    fs::write(&vscode_settings, "{}").unwrap();
    assert_success(
        "copilot",
        "install",
        cli.run(&["install", "--agent", "copilot"]),
    );
    let _daemon = ProfileDaemon::start(&cli);

    let doctor = cli.run(&["doctor"]);

    let doctor_stderr = stderr(&doctor);
    assert_eq!(doctor.status.code(), Some(0), "{doctor_stderr}");
    assert!(
        doctor_stderr.contains(&format!(
            "MCP server registered in {}\n",
            cli.home.path().join(".copilot/mcp-config.json").display()
        )),
        "{doctor_stderr}"
    );
    assert!(
        doctor_stderr.contains(&format!(
            "    VS Code: no tracedecay MCP server in {}; `tracedecay install --agent copilot` \
             registers Copilot CLI only\n",
            vscode_settings.display()
        )),
        "{doctor_stderr}"
    );
    assert!(!doctor_stderr.contains("NOT registered"), "{doctor_stderr}");
}

/// Zed and Antigravity present on the machine without TraceDecay get the same
/// "detected but not integrated" warning as every other host, never an issue.
#[cfg(target_os = "linux")]
#[test]
fn doctor_warns_on_detected_hosts_without_a_tracedecay_integration() {
    let cli = IsolatedCli::new();
    install_cline(&cli);
    fs::create_dir_all(cli.home.path().join(".config/zed")).unwrap();
    fs::create_dir_all(cli.home.path().join(".gemini/antigravity")).unwrap();
    let _daemon = ProfileDaemon::start(&cli);

    let doctor = cli.run(&["doctor"]);

    let doctor_stderr = stderr(&doctor);
    assert_eq!(doctor.status.code(), Some(0), "{doctor_stderr}");
    for (name, id) in [("Zed", "zed"), ("Antigravity", "antigravity")] {
        assert!(
            doctor_stderr.contains(&format!(
                "{name} detected but tracedecay is not integrated, run `tracedecay install \
                 --agent {id}`\n"
            )),
            "{doctor_stderr}"
        );
    }
    assert!(!doctor_stderr.contains("NOT registered"), "{doctor_stderr}");
}
