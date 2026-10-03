//! Typed per-host outcomes and exit codes of host lifecycle sweeps and the
//! post-update refresh.

use std::fs;
use std::path::Path;
use std::process::Output;

use tracedecay_agent_hosts::agents::host_bundle::{HostComponentV1, HostKindV1};
#[cfg(unix)]
use tracedecay_runtime_core::test_executable::write_executable_script;

#[cfg(unix)]
use super::install_kimi_cli;
use super::{
    IsolatedCli, VERIFY_FAILURE_ENV, assert_receipt_digests, assert_seeded_bytes, assert_success,
    host_case, latest_receipt, seed_host,
};

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

/// A Factory Droid MCP registry that still names TraceDecay, as an older
/// binary or a removed Droid install leaves behind.
fn seed_leftover_droid_registration(cli: &IsolatedCli) {
    let path = cli.home.path().join(".factory/mcp.json");
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
    seed_leftover_droid_registration(&cli);

    let output = cli.run(&["update-plugin"]);

    let stderr = stderr(&output);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("cline: refreshed"), "{stderr}");
    assert!(stderr.contains(DROID_NOT_INSTALLED), "{stderr}");
    assert!(
        !tracked_agents(&cli).contains("\"droid\""),
        "a skipped leftover host must not become tracked"
    );
}

/// Tracks `agents` beside the installed Cline, as an earlier install would.
fn track(cli: &IsolatedCli, agents: &[&str]) {
    let config = cli.profile.join("config.toml");
    let tracked = tracked_agents(cli);
    let listed = agents
        .iter()
        .map(|agent| format!(", \"{agent}\""))
        .collect::<String>();
    fs::write(
        &config,
        tracked.replace(
            "installed_agents = [\"cline\"]",
            &format!("installed_agents = [\"cline\"{listed}]"),
        ),
    )
    .unwrap();
}

const DROID_CLI_UNAVAILABLE: &str = "host CLI `droid` is unavailable for Factory Droid MCP \
     registry lifecycle; install it or add it to PATH and retry";

const DROID_NOT_INSTALLED: &str = "  droid: skipped, not installed (host CLI `droid` is \
     unavailable for Factory Droid MCP registry lifecycle; install it or add it to PATH and \
     retry)\n";

/// A host CLI `program` on the isolated `PATH` running the shell `body`.
#[cfg(unix)]
fn install_host_cli(cli: &IsolatedCli, program: &str, body: &str) {
    write_executable_script(&cli.bin_dir.join(program), format!("#!/bin/sh\n{body}\n")).unwrap();
}

/// Kiro's CLI while signed out, as #2374 found it: it refuses every command
/// and leaves a mark so a test can prove it never ran.
#[cfg(unix)]
const SIGNED_OUT_KIRO_CLI: &str = "touch \"$HOME/kiro-cli-ran\"\n\
     echo 'error: You are not logged in, please log in with kiro-cli login' >&2\nexit 1";

#[test]
fn update_plugin_skips_a_tracked_host_whose_cli_is_not_installed() {
    let cli = IsolatedCli::new();
    install_cline(&cli);
    seed_leftover_droid_registration(&cli);
    track(&cli, &["droid"]);

    let output = cli.run(&["update-plugin"]);

    let stderr = stderr(&output);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("  cline: refreshed\n"), "{stderr}");
    assert!(stderr.contains(DROID_NOT_INSTALLED), "{stderr}");
    let tracked = tracked_agents(&cli);
    assert!(
        tracked.contains("\"droid\""),
        "a skipped host stays tracked for when it is installed: {tracked}"
    );

    let untrack = cli.run(&["uninstall", "--agent", "droid"]);
    let untrack_stderr = self::stderr(&untrack);
    assert_eq!(untrack.status.code(), Some(0), "{untrack_stderr}");
    assert!(
        untrack_stderr.contains(&format!(
            "  droid: no longer tracked; the host is not installed, so its host-owned \
             registration was left in place ({DROID_CLI_UNAVAILABLE})\n"
        )),
        "{untrack_stderr}"
    );
    let tracked = tracked_agents(&cli);
    assert!(
        !tracked.contains("\"droid\"") && tracked.contains("\"cline\""),
        "the uninstall stops tracking only that host: {tracked}"
    );

    let after = cli.run(&["update-plugin"]);
    let after_stderr = self::stderr(&after);
    assert_eq!(after.status.code(), Some(0), "{after_stderr}");
    assert!(
        after_stderr.contains(DROID_NOT_INSTALLED),
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

    let output = cli.run(&["install", "--agent", "droid"]);

    let stderr = stderr(&output);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(DROID_NOT_INSTALLED), "{stderr}");
    assert!(
        !cli.profile.join("config.toml").exists() || !tracked_agents(&cli).contains("\"droid\""),
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
    track(&cli, &["droid"]);
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
    assert!(stderr.contains(DROID_NOT_INSTALLED), "{stderr}");
    assert!(
        stderr.contains("  kimi: pending operator action: `/plugins install"),
        "{stderr}"
    );
    assert!(!stderr.contains("reinstall failed for"), "{stderr}");
}

/// The operator's Kiro registry before TraceDecay touches it: a peer server
/// and formatting TraceDecay does not own.
const OPERATOR_KIRO_MCP: &str = "{\n  \"mcpServers\": {\n    \"operator\": {\n      \"command\": \"operator-mcp\",\n      \"args\": []\n    }\n  }\n}\n";

/// `OPERATOR_KIRO_MCP` with TraceDecay registered beside the peer server.
fn kiro_mcp_with_tracedecay(cli: &IsolatedCli) -> String {
    format!(
        "{{\n  \"mcpServers\": {{\n    \"operator\": {{\n      \"command\": \"operator-mcp\",\n      \"args\": []\n    }},\n    \"tracedecay\": {{\n      \"args\": [\n        \"serve\"\n      ],\n      \"command\": {},\n      \"disabled\": false\n    }}\n  }}\n}}\n",
        serde_json::to_string(&cli.installed_bin()).unwrap()
    )
}

/// #2374: Kiro's CLI installed but signed out. Kiro's MCP registry is the
/// documented `~/.kiro/settings/mcp.json`, so `install`, `update-plugin`, the
/// `post-update` refresh `update` runs, `doctor`, and `uninstall` all work on
/// that file, exit 0, and never run `kiro-cli`.
#[cfg(unix)]
#[test]
fn a_signed_out_kiro_cli_never_blocks_the_kiro_lifecycle() {
    let cli = IsolatedCli::new();
    install_cline(&cli);
    install_host_cli(&cli, "kiro-cli", SIGNED_OUT_KIRO_CLI);
    let mcp = cli.home.path().join(".kiro/settings/mcp.json");
    fs::create_dir_all(mcp.parent().unwrap()).unwrap();
    fs::write(&mcp, OPERATOR_KIRO_MCP).unwrap();

    let install = cli.run(&["install", "--agent", "kiro"]);
    let install_stderr = stderr(&install);
    assert_eq!(install.status.code(), Some(0), "{install_stderr}");
    assert_eq!(
        fs::read_to_string(&mcp).unwrap(),
        kiro_mcp_with_tracedecay(&cli)
    );

    for refresh in [&["update-plugin"][..], &["post-update"][..]] {
        let output = cli.run(refresh);
        let output_stderr = stderr(&output);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{refresh:?}: {output_stderr}"
        );
        assert!(
            output_stderr.contains("  kiro: refreshed\n"),
            "{output_stderr}"
        );
        assert_eq!(
            fs::read_to_string(&mcp).unwrap(),
            kiro_mcp_with_tracedecay(&cli)
        );
    }

    // `post-update` refuses while an unmanaged daemon listens; Doctor reads
    // the daemon's canonical report.
    {
        let _daemon = ProfileDaemon::start(&cli);
        let doctor = cli.run(&["doctor"]);
        let doctor_stderr = stderr(&doctor);
        assert_eq!(doctor.status.code(), Some(0), "{doctor_stderr}");
        assert!(
            doctor_stderr.contains(&format!("MCP server registered in {}\n", mcp.display())),
            "{doctor_stderr}"
        );
        assert!(!doctor_stderr.contains("kiro: skipped"), "{doctor_stderr}");
    }

    let uninstall = cli.run(&["uninstall", "--agent", "kiro"]);
    let uninstall_stderr = stderr(&uninstall);
    assert_eq!(uninstall.status.code(), Some(0), "{uninstall_stderr}");
    assert_eq!(fs::read_to_string(&mcp).unwrap(), OPERATOR_KIRO_MCP);
    assert!(
        !cli.home.path().join("kiro-cli-ran").exists(),
        "the Kiro lifecycle ran kiro-cli"
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
/// Kimi's plugin while Droid stays tracked and absent.
#[cfg(unix)]
#[test]
fn doctor_waits_only_on_real_operator_steps_and_skips_absent_hosts() {
    let cli = IsolatedCli::new();
    install_cline(&cli);
    track(&cli, &["droid"]);
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
        doctor_stderr.contains(&format!("  - {}", &DROID_NOT_INSTALLED[2..])),
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
        converged_stderr.contains(&format!("  - {}", &DROID_NOT_INSTALLED[2..])),
        "{converged_stderr}"
    );
    assert!(
        !converged_stderr.contains("pending operator action"),
        "{converged_stderr}"
    );
}

/// A tracked Droid whose CLI is absent is skipped and exits 0; a tracked Kiro,
/// which needs no CLI, with a malformed TraceDecay registration is an issue
/// and exits 1 while Droid stays skipped.
#[cfg(unix)]
#[test]
fn doctor_skips_an_absent_host_but_fails_a_reachable_hosts_malformed_registration() {
    let cli = IsolatedCli::new();
    install_cline(&cli);
    track(&cli, &["droid", "kiro"]);
    let _daemon = ProfileDaemon::start(&cli);

    let absent = cli.run(&["doctor"]);
    let absent_stderr = stderr(&absent);
    assert_eq!(absent.status.code(), Some(0), "{absent_stderr}");
    assert!(
        absent_stderr.contains(&format!("  - {}", &DROID_NOT_INSTALLED[2..])),
        "{absent_stderr}"
    );
    assert!(
        !absent_stderr.contains("pending operator action"),
        "{absent_stderr}"
    );

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
        !malformed_stderr.contains("kiro: skipped")
            && malformed_stderr.contains(&format!("  - {}", &DROID_NOT_INSTALLED[2..])),
        "{malformed_stderr}"
    );
}

/// A torn profile `config.toml` is an issue naming the file, the parse error
/// and its repair, not a warning while every stored entry reads as default.
#[cfg(unix)]
#[test]
fn doctor_fails_a_corrupt_profile_config_with_its_repair() {
    let cli = IsolatedCli::new();
    install_cline(&cli);
    let _daemon = ProfileDaemon::start(&cli);
    let config = cli.profile.join("config.toml");
    let installed = fs::read_to_string(&config).unwrap();
    fs::write(&config, "pending_upload = 0\n true").unwrap();

    let doctor = cli.run(&["doctor"]);
    let doctor_stderr = stderr(&doctor);
    assert_eq!(doctor.status.code(), Some(1), "{doctor_stderr}");
    assert!(
        doctor_stderr.contains(&format!(
            "Profile config is unusable: config file {} is corrupt at line 2: TOML parse error \
             at line 2, column 6\n  |\n2 |  true\n  |      ^\nkey with no value, expected `=`\n. \
             Fix it, or delete it to regenerate defaults",
            config.display()
        )),
        "{doctor_stderr}"
    );

    fs::write(&config, &installed).unwrap();
    let repaired = cli.run(&["doctor"]);
    let repaired_stderr = stderr(&repaired);
    assert_eq!(repaired.status.code(), Some(0), "{repaired_stderr}");
    assert!(
        !repaired_stderr.contains("Profile config is unusable"),
        "{repaired_stderr}"
    );

    fs::write(
        &config,
        format!(
            "{installed}\n[[github_review_sources]]\nowner = \"ScriptedAlchemy\"\n\
             repository = \"keyring-unnamed\"\naccess = \"os_keyring\"\n"
        ),
    )
    .unwrap();
    let unregistered = cli.run(&["doctor"]);
    let unregistered_stderr = stderr(&unregistered);
    assert_eq!(unregistered.status.code(), Some(1), "{unregistered_stderr}");
    assert!(
        unregistered_stderr.contains(&format!(
            "GitHub review source ScriptedAlchemy/keyring-unnamed uses os_keyring access without \
             keyring_service and keyring_account, so it is not registered; fix or remove its \
             github_review_sources entry in {}",
            config.display()
        )),
        "{unregistered_stderr}"
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

/// A reachable host CLI that refuses the registration: the install fails
/// with the host CLI's own words, not a TraceDecay filesystem failure
/// pointing at a source line. An absent host is skipped instead; this is a
/// host TraceDecay can reach.
#[cfg(unix)]
#[test]
fn install_reports_a_host_cli_refusal_in_the_host_clis_words() {
    let cli = IsolatedCli::new();
    install_host_cli(
        &cli,
        "droid",
        r#"case "$1 $2" in
  "mcp add") echo 'error: the MCP registry is locked by another droid' >&2; exit 1 ;;
esac
exit 0"#,
    );

    let output = cli.run(&["install", "--agent", "droid"]);

    let stderr = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    let droid_line = stderr
        .lines()
        .find(|line| line.starts_with("  droid: "))
        .unwrap_or_else(|| panic!("no droid summary line: {stderr}"));
    assert!(
        droid_line.starts_with("  droid: failed: ")
            && droid_line.ends_with("error: the MCP registry is locked by another droid"),
        "{droid_line}"
    );
    assert!(!stderr.contains("filesystem operation failed"), "{stderr}");
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

    // No daemon listens here, so `daemon_unavailable` is the one step left.
    let doctor = stderr(&cli.run(&["doctor"]));
    assert!(!doctor.contains("open Kimi Code and run"), "{doctor}");
    assert!(
        doctor.contains("daemon_unavailable: no TraceDecay daemon is listening")
            && doctor.contains("1 pending operator action(s), "),
        "{doctor}"
    );
    let converged = cli.run(&["update-plugin"]);
    assert_eq!(converged.status.code(), Some(0), "{}", stderr(&converged));
}

/// Staging converges without claiming host registration. Doctor reports the
/// unverifiable host step informationally and rejects an incomplete stage.
#[cfg(unix)]
#[test]
fn chatgpt_stages_an_unverifiable_registration_and_uninstalls_its_bundle() {
    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::ChatGpt);
    let originals = seed_host(case, &cli);
    let staged = cli
        .home
        .path()
        .join(".tracedecay/host-bundle-stage/chatgpt/tracedecay");

    let install = cli.run(&["install", "--agent", case.id]);
    let install_stderr = stderr(&install);
    assert_eq!(install.status.code(), Some(0), "{install_stderr}");
    assert!(
        install_stderr.contains("ChatGPT registration is unverifiable locally"),
        "{install_stderr}"
    );
    // The receipt-owned staged bundle is the operator's install payload.
    for relative in [
        "plugin.json",
        "mcp.json",
        "README.md",
        "chatgpt-extension/embedded/server.mjs",
        "chatgpt-extension/embedded/app.html",
        "chatgpt-extension/assets/icon.svg",
    ] {
        assert!(
            staged.join(relative).is_file(),
            "staged bundle missing {relative}"
        );
    }
    let install_receipt = latest_receipt(&cli, case.host);
    assert_receipt_digests(&cli, &install_receipt);
    assert_seeded_bytes(&cli, &originals);
    // The staged mcp.json launches the resolved tracedecay binary, not a
    // placeholder or a bare `tracedecay` the host cannot resolve.
    let mcp: serde_json::Value =
        serde_json::from_slice(&fs::read(staged.join("mcp.json")).unwrap()).unwrap();
    assert_eq!(
        mcp["mcpServers"]["graph"]["command"],
        serde_json::json!(cli.installed_bin()),
        "{mcp}"
    );

    let update = cli.run(&["update-plugin"]);
    let update_stderr = stderr(&update);
    assert_eq!(
        update.status.code(),
        Some(0),
        "the staged host stays tracked: {update_stderr}"
    );
    assert!(
        update_stderr.contains("ChatGPT registration is unverifiable locally"),
        "{update_stderr}"
    );

    {
        let _daemon = ProfileDaemon::start(&cli);
        let doctor_result = cli.run(&["doctor"]);
        let doctor = stderr(&doctor_result);
        assert_eq!(doctor_result.status.code(), Some(0), "{doctor}");
        assert!(
            doctor.contains("ChatGPT registration is unverifiable locally"),
            "{doctor}"
        );
    }

    // A bundle left without its manifest is a broken stage, not an absent
    // one: doctor fails it, and reinstalling converges the staged bytes the
    // receipt refuses to remove as foreign.
    fs::remove_file(staged.join("plugin.json")).unwrap();
    let partial = stderr(&cli.run(&["doctor"]));
    assert!(
        partial.contains("✘")
            && partial.contains("ChatGPT staged bundle")
            && partial.contains("incomplete"),
        "{partial}"
    );

    let reinstall = cli.run(&["install", "--agent", case.id]);
    let reinstall_stderr = stderr(&reinstall);
    assert_eq!(reinstall.status.code(), Some(0), "{reinstall_stderr}");
    assert!(
        staged.join("plugin.json").is_file(),
        "reinstall did not restore the staged manifest"
    );

    let uninstall = cli.run(&["uninstall", "--agent", case.id]);
    let uninstall_stderr = stderr(&uninstall);
    assert_eq!(uninstall.status.code(), Some(0), "{uninstall_stderr}");
    assert!(!staged.exists(), "uninstall left the staged bundle behind");
    assert!(install_receipt.component_receipts.iter().all(|component| {
        component
            .artifacts
            .iter()
            .all(|artifact| !cli.home.path().join(&artifact.relative_path).exists())
    }));
    assert_seeded_bytes(&cli, &originals);
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
    // ChatGPT's desktop app-data directory is its only local presence proof.
    fs::create_dir_all(cli.home.path().join("Library/Application Support/ChatGPT")).unwrap();
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
    assert!(
        doctor_stderr.contains(&format!(
            "ChatGPT detected ({}) but tracedecay is not integrated, run `tracedecay install \
             --agent chatgpt`\n",
            cli.home
                .path()
                .join("Library/Application Support/ChatGPT")
                .display()
        )),
        "{doctor_stderr}"
    );
    assert!(!doctor_stderr.contains("NOT registered"), "{doctor_stderr}");
}

/// An installed file whose bytes moved is drift the daemon reports; the
/// finding names the component and the command that converges it.
#[test]
fn doctor_names_the_drifted_host_component_and_its_remedy() {
    let cli = IsolatedCli::new();
    install_cline(&cli);
    let receipt = latest_receipt(&cli, HostKindV1::Cline);
    let artifact = cli.home.path().join(
        &receipt
            .component_receipts
            .iter()
            .find(|receipt| receipt.component == HostComponentV1::ContextMcp)
            .expect("cline installs its context MCP component")
            .artifacts[0]
            .relative_path,
    );
    let mut bytes = fs::read(&artifact).unwrap();
    bytes.push(b'\n');
    fs::write(&artifact, bytes).unwrap();
    let _daemon = ProfileDaemon::start(&cli);
    // Daemon findings are reported for an enrolled project.
    fs::write(cli.project.path().join("lib.rs"), "fn probe() {}\n").unwrap();
    assert_success("project", "init", cli.run(&["init"]));

    let doctor = cli.run(&["doctor"]);

    let doctor_stderr = stderr(&doctor);
    assert!(
        doctor_stderr.contains(
            "advisory: cline/context-mcp has drifted from its installed shape; run `tracedecay \
             reinstall --component context-mcp` (refreshes tracedecay-owned files) \
             (host.conformance.drifted)"
        ),
        "{doctor_stderr}"
    );
}

/// Gemini CLI and Pi are CLIs. `~/.gemini` is also Antigravity's directory and
/// `~/.pi/agent` outlives an uninstalled `pi`, so neither directory detects its
/// host; the host's own executable does.
#[cfg(unix)]
#[test]
fn doctor_detects_cli_hosts_by_their_executable_not_their_directory() {
    let cli = IsolatedCli::new();
    install_cline(&cli);
    fs::create_dir_all(cli.home.path().join(".gemini/antigravity")).unwrap();
    fs::create_dir_all(cli.home.path().join(".pi/agent")).unwrap();
    let _daemon = ProfileDaemon::start(&cli);

    let absent = cli.run(&["doctor"]);
    let absent_stderr = stderr(&absent);
    assert_eq!(absent.status.code(), Some(0), "{absent_stderr}");
    for name in ["Gemini CLI", "Pi"] {
        assert!(
            !absent_stderr.contains(&format!("{name} detected")),
            "a host whose CLI is absent is not detected:\n{absent_stderr}"
        );
    }

    install_host_cli(&cli, "gemini", "exit 0");
    install_host_cli(&cli, "pi", "exit 0");
    let present = cli.run(&["doctor"]);
    let present_stderr = stderr(&present);
    assert_eq!(present.status.code(), Some(0), "{present_stderr}");
    assert!(
        present_stderr.contains(
            "Gemini CLI detected but tracedecay is not integrated, run `tracedecay install \
             --agent gemini`\n"
        ),
        "{present_stderr}"
    );
    assert!(
        present_stderr.contains(&format!(
            "Pi detected ({}) but tracedecay is not integrated, run `tracedecay install --agent \
             pi`\n",
            cli.home.path().join(".pi/agent").display()
        )),
        "{present_stderr}"
    );
}
