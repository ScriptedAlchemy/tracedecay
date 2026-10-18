//! `install --agent <id> --catalog-profile <NAME>` records the choice in the
//! host's own installed configuration, and every later lifecycle command keeps
//! it until the operator resets it.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use tracedecay_agent_hosts::agents::host_bundle::HostKindV1;
use tracedecay_agent_hosts::agents::host_bundle_registry::RECEIPT_BACKED_HOST_KINDS;
use tracedecay_agent_hosts::agents::integration_id_for_host;

use super::IsolatedCli;
use super::exact_restore::{
    home_snapshot, install_fake_native_hosts, run_phase, seed_operator_home, snapshot_diff,
};
use super::sweep_outcomes::ProfileDaemon;

const COMPACT_FLAG: &str = "\"--catalog-profile=compact\"";

/// Every quoted `"serve"` launch argument written under the home (outside the
/// profile and the fake host binaries), by file, and whether the compact
/// profile flag is the very next argument.
fn serve_launches(cli: &IsolatedCli) -> Vec<(PathBuf, bool)> {
    let mut launches = Vec::new();
    for (path, text) in written_host_files(cli) {
        for (at, serve) in text.match_indices("\"serve\"") {
            let flagged = text[at + serve.len()..]
                .trim_start()
                .strip_prefix(',')
                .is_some_and(|rest| rest.trim_start().starts_with(COMPACT_FLAG));
            launches.push((path.clone(), flagged));
        }
    }
    launches
}

fn written_host_files(cli: &IsolatedCli) -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    let mut pending = vec![cli.home.path().to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path == cli.profile || path == cli.bin_dir {
                continue;
            }
            let file_type = fs::symlink_metadata(&path).unwrap().file_type();
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file()
                && matches!(
                    path.extension().and_then(|extension| extension.to_str()),
                    Some("json" | "jsonc" | "toml" | "ts")
                )
            {
                let text = fs::read_to_string(&path).unwrap();
                files.push((path, text));
            }
        }
    }
    files
}

fn flagged_count(cli: &IsolatedCli) -> usize {
    serve_launches(cli)
        .iter()
        .filter(|(_, flagged)| *flagged)
        .count()
}

fn assert_every_launch_is_compact(cli: &IsolatedCli, phase: &str) {
    let launches = serve_launches(cli);
    let bare = launches
        .iter()
        .filter(|(_, flagged)| !flagged)
        .map(|(path, _)| path.display().to_string())
        .collect::<Vec<_>>();
    assert!(!launches.is_empty(), "{phase}: no serve launch was written");
    assert!(
        bare.is_empty(),
        "{phase}: these launches lost --catalog-profile=compact:\n{}",
        bare.join("\n")
    );
}

/// Doctor's issues and host-conformance drift lines, which must not change
/// when an install only switched catalog profiles.
fn doctor_findings(cli: &IsolatedCli) -> BTreeSet<String> {
    let _daemon = ProfileDaemon::start(cli);
    let output = cli.run(&["doctor", "--json"]);
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "doctor --json printed no report\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let mut findings = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|check| check["level"] == "issue")
        .map(|check| check["message"].as_str().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    let rendered = report["daemon_findings"].to_string();
    for marker in [
        "host.conformance.drifted",
        "host.conformance.protocol-drift",
    ] {
        if rendered.contains(marker) {
            findings.insert(rendered.clone());
        }
    }
    findings
}

fn saved_profiles(cli: &IsolatedCli) -> Option<toml::Value> {
    let config: toml::Value =
        toml::from_str(&fs::read_to_string(cli.profile.join("config.toml")).unwrap()).unwrap();
    config.get("agent_catalog_profile").cloned()
}

#[test]
fn catalog_profile_survives_every_lifecycle_on_every_host() {
    let cli = IsolatedCli::new();
    seed_operator_home(cli.home.path());
    install_fake_native_hosts(&cli.bin_dir);
    // Hermes and Pi register no MCP launch, so they have no profile to carry.
    let hosts = RECEIPT_BACKED_HOST_KINDS
        .into_iter()
        .filter(|host| !matches!(host, HostKindV1::Hermes | HostKindV1::Pi))
        .map(integration_id_for_host)
        .collect::<Vec<_>>();

    for id in &hosts {
        run_phase(&cli, id, &["install", "--agent", id], *id == "kimi");
    }
    assert_eq!(flagged_count(&cli), 0, "a default install wrote a profile");
    let default_doctor = doctor_findings(&cli);

    for id in &hosts {
        let before = flagged_count(&cli);
        run_phase(
            &cli,
            id,
            &["install", "--agent", id, "--catalog-profile", "compact"],
            *id == "kimi",
        );
        // Kimi waits on the operator's activation step, so this pass writes
        // no launch for it.
        assert!(
            *id == "kimi" || flagged_count(&cli) > before,
            "{id}: switching an installed default to compact wrote no compact launch"
        );
    }
    assert_every_launch_is_compact(&cli, "install --catalog-profile compact");

    for phase in [&["update-plugin"][..], &["reinstall"][..], &["install"][..]] {
        run_phase(&cli, &phase.join(" "), phase, true);
        assert_every_launch_is_compact(&cli, &phase.join(" "));
    }
    assert_eq!(
        doctor_findings(&cli),
        default_doctor,
        "doctor reads a compact install differently from a default one"
    );

    for id in &hosts {
        run_phase(
            &cli,
            id,
            &["install", "--agent", id, "--catalog-profile", "default"],
            *id == "kimi",
        );
    }
    let leftover = written_host_files(&cli)
        .into_iter()
        .filter(|(_, text)| text.contains("--catalog-profile"))
        .map(|(path, _)| path.display().to_string())
        .collect::<Vec<_>>();
    assert!(
        leftover.is_empty(),
        "reset to default left a profile flag in:\n{}",
        leftover.join("\n")
    );
    assert_eq!(saved_profiles(&cli), None, "reset left a saved choice");
}

#[test]
fn install_rejects_a_catalog_profile_before_writing() {
    let cli = IsolatedCli::new();
    seed_operator_home(cli.home.path());
    install_fake_native_hosts(&cli.bin_dir);
    let before = home_snapshot(&cli);

    for (args, expected) in [
        (
            &["install", "--agent", "cursor", "--catalog-profile", "tiny"][..],
            "unknown catalog profile \"tiny\"; valid: ",
        ),
        (
            &["install", "--catalog-profile", "compact"][..],
            "--agent <AGENT>",
        ),
        (
            &[
                "install",
                "--agent",
                "zed",
                "--local",
                "--catalog-profile",
                "compact",
            ][..],
            "'--local' cannot be used with '--catalog-profile",
        ),
        (
            &[
                "install",
                "--agent",
                "hermes",
                "--catalog-profile",
                "compact",
            ][..],
            "hermes registers no MCP server, so --catalog-profile does not apply",
        ),
    ] {
        let output = cli.run(args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "`{}` succeeded:\n{stderr}",
            args.join(" ")
        );
        assert!(
            stderr.contains(expected),
            "`{}` did not say {expected:?}:\n{stderr}",
            args.join(" ")
        );
    }
    assert!(
        String::from_utf8_lossy(
            &cli.run(&["install", "--agent", "cursor", "--catalog-profile", "tiny"])
                .stderr
        )
        .contains("compact"),
        "the rejection must list the valid profiles"
    );
    assert_eq!(
        snapshot_diff(&before, &home_snapshot(&cli)),
        Vec::<String>::new()
    );
    assert!(
        !cli.profile.join("config.toml").exists() || saved_profiles(&cli).is_none(),
        "a rejected install saved a catalog profile"
    );
}
