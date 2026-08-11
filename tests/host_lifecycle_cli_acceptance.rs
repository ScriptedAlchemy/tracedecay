use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay::agents::host_bundle_registry::{
    default_components, unsupported_host_component_set_reason,
};
use tracedecay::agents::host_bundle_v2::{
    HostBundleComponentV1, HostComponentSetReceiptV1, HostKindV1, latest_host_component_receipt_at,
    latest_host_component_set_receipt_at,
};

#[path = "host_lifecycle_cli_acceptance/native_plugin_fixture.rs"]
mod native_plugin_fixture;
use native_plugin_fixture::{
    apply_current_codex_plugin_remediation, remediation_command, set_claude_native_activation,
};
#[cfg(unix)]
use native_plugin_fixture::{install_current_claude_cli, recorded_claude_invocations};

const VERIFY_FAILURE_ENV: &str = "TRACEDECAY_TEST_FAIL_HOST_REGISTRATION_VERIFY";

#[derive(Clone, Copy)]
struct HostCase {
    id: &'static str,
    host: HostKindV1,
    configs: &'static [(&'static str, &'static [u8])],
}

const CLAUDE_CONFIGS: &[(&str, &[u8])] = &[
    (
        ".claude/settings.json",
        br#"{"env":{"FOREIGN_SETTING":"preserved"},"permissions":{"allow":["Read"]},"enabledPlugins":{"foreign@market":true}}
"#,
    ),
    (
        ".claude/plugins/known_marketplaces.json",
        br#"{"foreign":{"source":{"source":"directory","path":"/opt/foreign"},"installLocation":"/opt/foreign","lastUpdated":"2026-07-01T00:00:00Z"}}
"#,
    ),
];
const CURSOR_CONFIGS: &[(&str, &[u8])] = &[(
    ".cursor/mcp.json",
    br#"{"mcpServers":{"foreign":{"url":"https://example.invalid/mcp"}},"ui":{"theme":"dark"}}
"#,
)];
const CODEX_CONFIGS: &[(&str, &[u8])] = &[(
    ".codex/config.toml",
    b"# operator comment\nmodel = \"o4-mini\" # keep inline\napproval_policy = \"on-failure\"\n\n[mcp_servers.foreign]\ncommand = \"foreign-bin\"\nargs = [\"--stdio\"]\n",
)];
const HERMES_CONFIGS: &[(&str, &[u8])] = &[
    (
        ".hermes/config.yaml",
        b"theme: dark\nplugins:\n  enabled:\n    - foreign\n",
    ),
    (
        ".hermes/profiles/review/config.yaml",
        b"theme: light\nplugins:\n  enabled:\n    - foreign\n",
    ),
];
const KIRO_CONFIGS: &[(&str, &[u8])] = &[
    (
        ".kiro/settings/mcp.json",
        br#"{"mcpServers":{"foreign":{"command":"foreign-bin","args":["serve"]}},"ui":{"theme":"dark"}}
"#,
    ),
    (
        ".kiro/settings/cli.json",
        br#"{"chat":{"defaultAgent":"custom-agent"},"telemetry":false}
"#,
    ),
];
const OPENCODE_CONFIGS: &[(&str, &[u8])] = &[(
    ".config/opencode/opencode.json",
    br#"{"$schema":"https://opencode.ai/config.json","mcp":{"foreign":{"type":"local","command":["foreign-bin"]}},"theme":"dark"}
"#,
)];
/// Gemini's shared settings file is host-owned under the extension model:
/// TraceDecay only observes it so a lifecycle can roll back whatever the host
/// CLI changed there. It is seeded with a foreign server so a run that started
/// merging it again would be caught.
const GEMINI_CONFIGS: &[(&str, &[u8])] = &[(
    ".gemini/settings.json",
    br#"{"mcpServers":{"foreign":{"command":"foreign-bin","args":["serve"]}},"theme":"dark"}
"#,
)];
/// Copilot's MCP registry is host-owned: `copilot mcp add|remove` is its only
/// writer. TraceDecay reads it to guard operator-owned peers and to record the
/// bytes rollback restores. Seeding a foreign server means a run that started
/// merging the document itself would be caught.
const COPILOT_CONFIGS: &[(&str, &[u8])] = &[(
    ".copilot/mcp-config.json",
    br#"{"mcpServers":{"foreign":{"command":"foreign-bin","args":["serve"]}}}
"#,
)];
const CLINE_CONFIGS: &[(&str, &[u8])] = &[(
    ".cline/mcp.json",
    br#"{
  "mcpServers": {
    "foreign": {
      "args": [
        "serve"
      ],
      "command": "foreign-bin"
    }
  },
  "ui": {
    "theme": "dark"
  }
}
"#,
)];
const ROO_CONFIGS: &[(&str, &[u8])] = &[(
    ".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json",
    br#"{
  "mcpServers": {
    "foreign": {
      "args": [
        "serve"
      ],
      "command": "foreign-bin"
    }
  },
  "ui": {
    "theme": "dark"
  }
}
"#,
)];
const KILO_CONFIGS: &[(&str, &[u8])] = &[(
    ".config/kilo/kilo.jsonc",
    br#"{
  "mcp": {
    "foreign": {
      "command": [
        "foreign-bin"
      ],
      "type": "local"
    }
  },
  "theme": "dark"
}
"#,
)];

fn host_case(host: HostKindV1) -> HostCase {
    let configs = match host {
        HostKindV1::ClaudeCode => CLAUDE_CONFIGS,
        HostKindV1::CursorDesktop => CURSOR_CONFIGS,
        HostKindV1::Codex => CODEX_CONFIGS,
        HostKindV1::Hermes => HERMES_CONFIGS,
        HostKindV1::Kiro => KIRO_CONFIGS,
        HostKindV1::KimiCode => &[],
        HostKindV1::OpenCode => OPENCODE_CONFIGS,
        HostKindV1::Gemini => GEMINI_CONFIGS,
        HostKindV1::Copilot => COPILOT_CONFIGS,
        HostKindV1::Cline => CLINE_CONFIGS,
        HostKindV1::RooCode => ROO_CONFIGS,
        HostKindV1::Kilo => KILO_CONFIGS,
        unsupported => panic!("no production lifecycle case for unsupported host {unsupported:?}"),
    };
    HostCase {
        id: tracedecay::agents::integration_id_for_host(host),
        host,
        configs,
    }
}

fn supported_host_cases() -> impl Iterator<Item = HostCase> {
    HostKindV1::ALL
        .into_iter()
        .filter_map(|host| (!default_components(host).is_empty()).then(|| host_case(host)))
}

struct IsolatedCli {
    home: TempDir,
    project: TempDir,
    profile: PathBuf,
    bin_dir: PathBuf,
}

impl IsolatedCli {
    fn new() -> Self {
        let home = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let profile = home.path().join(".tracedecay-test-profile");
        let bin_dir = home.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let shim = bin_dir.join(if cfg!(windows) {
            "tracedecay.exe"
        } else {
            "tracedecay"
        });
        if fs::hard_link(env!("CARGO_BIN_EXE_tracedecay"), &shim).is_err() {
            fs::copy(env!("CARGO_BIN_EXE_tracedecay"), &shim).unwrap();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(&shim).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&shim, permissions).unwrap();
        }
        Self {
            home,
            project,
            profile,
            bin_dir,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tracedecay"));
        let inherited_path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(self.bin_dir.clone()).chain(std::env::split_paths(&inherited_path)),
        )
        .unwrap();
        command
            .args(args)
            .current_dir(self.project.path())
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("XDG_CONFIG_HOME", self.home.path().join(".config"))
            .env("TRACEDECAY_DATA_DIR", &self.profile)
            .env("TRACEDECAY_GLOBAL_DB", self.profile.join("global.db"))
            .env("PATH", path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn run_with_env(&self, args: &[&str], key: &str, value: &str) -> Output {
        let mut command = self.command(args);
        command.env(key, value);
        command.output().unwrap()
    }

    fn run_with_stdin(&self, args: &[&str], input: &[u8]) -> Output {
        let mut command = self.command(args);
        command.stdin(Stdio::piped());
        let mut child = command.spawn().unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap()
    }

    fn lifecycle_root(&self) -> PathBuf {
        self.profile.join("host-components")
    }
}
fn assert_success(host: &str, phase: &str, output: Output) {
    assert!(
        output.status.success(),
        "{host} {phase} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_documented_mcp_registration(case: HostCase, cli: &IsolatedCli) {
    let (relative, root) = match case.host {
        HostKindV1::Cline => (".cline/mcp.json", "mcpServers"),
        HostKindV1::RooCode => (
            ".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json",
            "mcpServers",
        ),
        HostKindV1::Kilo => (".config/kilo/kilo.jsonc", "mcp"),
        _ => return,
    };
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(cli.home.path().join(relative)).unwrap()).unwrap();
    assert!(
        config[root].get("foreign").is_some(),
        "{} install discarded a sibling MCP server",
        case.id
    );
    let theme = match case.host {
        HostKindV1::Cline | HostKindV1::RooCode => &config["ui"]["theme"],
        HostKindV1::Kilo => &config["theme"],
        _ => unreachable!(),
    };
    assert_eq!(theme, "dark", "{} config after install: {config}", case.id);
    let entry = &config[root]["tracedecay"];
    match case.host {
        HostKindV1::Cline => {
            assert_eq!(
                entry["command"],
                serde_json::json!(cli.bin_dir.join("tracedecay"))
            );
            assert_eq!(entry["args"], serde_json::json!(["serve"]));
            assert_eq!(entry["disabled"], false);
            assert_eq!(entry["autoApprove"], serde_json::json!([]));
        }
        HostKindV1::RooCode => {
            assert_eq!(
                entry["command"],
                serde_json::json!(cli.bin_dir.join("tracedecay"))
            );
            assert_eq!(entry["args"], serde_json::json!(["serve"]));
            assert_eq!(entry["disabled"], false);
            assert_eq!(entry["alwaysAllow"], serde_json::json!([]));
        }
        HostKindV1::Kilo => {
            assert_eq!(entry["type"], "local");
            assert_eq!(
                entry["command"],
                serde_json::json!([cli.bin_dir.join("tracedecay"), "serve"])
            );
            assert_eq!(entry["enabled"], true);
        }
        _ => unreachable!(),
    }
}

fn seed_host(case: HostCase, cli: &IsolatedCli) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut originals = BTreeMap::new();
    for (relative, bytes) in case.configs {
        let path = cli.home.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes = bytes.to_vec();
        fs::write(&path, &bytes).unwrap();
        originals.insert(PathBuf::from(relative), bytes);
    }
    let native_extension = PathBuf::from(format!(".{}/plugins/foreign/manifest.json", case.id));
    let native_extension_path = cli.home.path().join(&native_extension);
    fs::create_dir_all(native_extension_path.parent().unwrap()).unwrap();
    let extension_bytes = format!(
        "{{\"name\":\"foreign-{}\",\"version\":\"1.0.0\"}}\n",
        case.id
    )
    .into_bytes();
    fs::write(&native_extension_path, &extension_bytes).unwrap();
    originals.insert(native_extension, extension_bytes);
    originals
}

fn assert_seeded_bytes(cli: &IsolatedCli, originals: &BTreeMap<PathBuf, Vec<u8>>) {
    for (relative, expected) in originals {
        assert_eq!(
            fs::read(cli.home.path().join(relative)).unwrap(),
            *expected,
            "native host file changed: {}",
            relative.display()
        );
    }
}

fn latest_receipt(cli: &IsolatedCli, host: HostKindV1) -> HostComponentSetReceiptV1 {
    latest_host_component_set_receipt_at(&cli.lifecycle_root(), host)
        .unwrap()
        .expect("host lifecycle receipt")
}

fn assert_receipt_digests(cli: &IsolatedCli, receipt: &HostComponentSetReceiptV1) {
    assert!(
        receipt
            .confirmed_plan_digest
            .is_some_and(|digest| digest != [0; 32])
    );
    for component in &receipt.component_receipts {
        let manifest = receipt
            .component_manifests
            .iter()
            .find(|manifest| manifest.component == component.component)
            .unwrap();
        assert_eq!(
            manifest.canonical_digest().unwrap(),
            component.manifest_digest
        );
        for artifact in &component.artifacts {
            let bytes = fs::read(cli.home.path().join(&artifact.relative_path)).unwrap();
            let digest: [u8; 32] = Sha256::digest(bytes).into();
            assert_eq!(digest, artifact.artifact_digest);
        }
    }
}

fn owned_bytes(
    cli: &IsolatedCli,
    receipt: &HostComponentSetReceiptV1,
    configs: &BTreeMap<PathBuf, Vec<u8>>,
) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut snapshot = BTreeMap::new();
    for component in &receipt.component_receipts {
        for artifact in &component.artifacts {
            let relative = PathBuf::from(&artifact.relative_path);
            snapshot.insert(
                relative.clone(),
                fs::read(cli.home.path().join(relative)).unwrap(),
            );
        }
    }
    for relative in configs.keys() {
        snapshot.insert(
            relative.clone(),
            fs::read(cli.home.path().join(relative)).unwrap(),
        );
    }
    snapshot
}

fn assert_snapshot_eq(
    host: &str,
    phase: &str,
    observed: &BTreeMap<PathBuf, Vec<u8>>,
    expected: &BTreeMap<PathBuf, Vec<u8>>,
) {
    assert_eq!(
        observed.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>()
    );
    for (path, expected) in expected {
        let observed = &observed[path];
        assert_eq!(
            observed,
            expected,
            "{host} {phase} changed {} (observed {}, expected {})",
            path.display(),
            hex::encode(Sha256::digest(observed)),
            hex::encode(Sha256::digest(expected))
        );
    }
}

fn packet_request(packet: &str, identity: &str) -> Vec<u8> {
    let packet: serde_json::Value = serde_json::from_str(packet).unwrap();
    let request = packet["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|event| event["identity"] == identity)
        .unwrap()["request"]
        .clone();
    serde_json::to_vec(&request).unwrap()
}

fn native_feedback(case: HostCase) -> Vec<(&'static str, &'static str, Vec<u8>)> {
    match case.host {
        HostKindV1::ClaudeCode => vec![
            (
                "edit",
                "hook-claude-post-tool-use",
                include_bytes!(
                    "../crates/tracedecay-hooks/fixtures/host_events/claude/post_tool_use_write.json"
                )
                .to_vec(),
            ),
            (
                "stop",
                "hook-stop",
                include_bytes!("../crates/tracedecay-hooks/fixtures/host_events/claude/stop.json")
                    .to_vec(),
            ),
        ],
        HostKindV1::CursorDesktop => {
            let packet = include_str!("../crates/tracedecay-hooks/fixtures/host_events/cursor.json");
            vec![(
                "edit",
                "hook-cursor-after-file-edit",
                packet_request(packet, "saved_edit"),
            )]
        }
        HostKindV1::Codex => {
            vec![(
                "stop",
                "hook-codex-stop",
                include_bytes!("../crates/tracedecay-hooks/fixtures/host_events/codex/stop.json")
                    .to_vec(),
            )]
        }
        HostKindV1::Hermes => vec![
            (
                "edit",
                "hook-hermes-terminal-receipt",
                include_bytes!(
                    "../crates/tracedecay-hooks/fixtures/host_events/hermes/saved-edit.json"
                )
                .to_vec(),
            ),
            (
                "stop",
                "hook-hermes-terminal-receipt",
                include_bytes!("../crates/tracedecay-hooks/fixtures/host_events/hermes/stop.json")
                    .to_vec(),
            ),
        ],
        HostKindV1::KimiCode => vec![
            (
                "edit",
                "hook-kimi-event",
                include_bytes!(
                    "../crates/tracedecay-hooks/fixtures/host_events/kimi/post-tool-use-edit.json"
                )
                .to_vec(),
            ),
            (
                "stop",
                "hook-kimi-event",
                include_bytes!("../crates/tracedecay-hooks/fixtures/host_events/kimi/stop.json")
                    .to_vec(),
            ),
        ],
        HostKindV1::OpenCode => {
            let packet =
                include_str!("../crates/tracedecay-hooks/fixtures/host_events/opencode/baseline.json");
            vec![
                (
                    "edit",
                    "hook-opencode-event",
                    packet_request(packet, "saved_edit"),
                ),
                (
                    "stop",
                    "hook-opencode-event",
                    packet_request(packet, "stop"),
                ),
            ]
        }
        HostKindV1::Kiro
        | HostKindV1::Gemini
        | HostKindV1::Copilot
        | HostKindV1::Cline
        | HostKindV1::RooCode
        | HostKindV1::Kilo => Vec::new(),
        _ => unreachable!("non-acceptance host"),
    }
}

/// Hosts whose install/uninstall lifecycle drives a *host-owned* binary
/// (`claude plugin`, `codex mcp`, `kiro-cli mcp`, `gemini extensions`,
/// `copilot mcp`) or defers activation to an interactive host flow (Kimi).
///
/// This suite runs the production CLI against an isolated `HOME` that contains
/// no host binaries at all, so for these hosts the lifecycle correctly refuses
/// with the typed missing-binary error rather than emulating the registration.
/// Their lifecycles are covered by the per-host unit suites, which inject a
/// fake launcher.
fn lifecycle_requires_absent_host_binary(host: HostKindV1) -> bool {
    matches!(
        host,
        HostKindV1::ClaudeCode
            | HostKindV1::Codex
            | HostKindV1::KimiCode
            | HostKindV1::Kiro
            | HostKindV1::Gemini
            | HostKindV1::Copilot
    )
}

#[test]
fn production_cli_completes_deterministic_lifecycle_for_config_native_hosts() {
    for case in
        supported_host_cases().filter(|case| !lifecycle_requires_absent_host_binary(case.host))
    {
        let cli = IsolatedCli::new();
        let originals = seed_host(case, &cli);

        assert_success(
            case.id,
            "install",
            cli.run(&["install", "--agent", case.id]),
        );
        assert_documented_mcp_registration(case, &cli);
        let install_receipt = latest_receipt(&cli, case.host);
        assert_receipt_digests(&cli, &install_receipt);

        assert_success(case.id, "update", cli.run(&["update-plugin"]));
        let update_receipt = latest_receipt(&cli, case.host);
        assert_receipt_digests(&cli, &update_receipt);

        for (phase, entrypoint, fixture) in native_feedback(case) {
            assert_success(case.id, phase, cli.run_with_stdin(&[entrypoint], &fixture));
        }

        let repair_target = update_receipt
            .component_receipts
            .iter()
            .flat_map(|component| &component.artifacts)
            .next()
            .expect("managed repair artifact");
        fs::write(
            cli.home.path().join(&repair_target.relative_path),
            b"operator-corrupted managed artifact",
        )
        .unwrap();
        assert_success(case.id, "repair", cli.run(&["reinstall"]));
        let repaired_receipt = latest_receipt(&cli, case.host);
        assert_receipt_digests(&cli, &repaired_receipt);

        let pending_mutation = repaired_receipt
            .component_receipts
            .iter()
            .flat_map(|component| &component.artifacts)
            .next()
            .expect("managed artifact for interrupted repair");
        fs::write(
            cli.home.path().join(&pending_mutation.relative_path),
            b"operator state that the failed repair must restore",
        )
        .unwrap();
        let before_interruption = owned_bytes(&cli, &repaired_receipt, &originals);
        let receipt_before_interruption =
            serde_json::to_vec(&latest_receipt(&cli, case.host)).unwrap();
        let mut interrupted = cli.command(&["reinstall"]);
        interrupted.env(VERIFY_FAILURE_ENV, "1");
        let interrupted = interrupted.output().unwrap();
        assert!(
            !interrupted.status.success(),
            "{} injected interruption unexpectedly succeeded",
            case.id
        );
        assert_eq!(
            owned_bytes(&cli, &repaired_receipt, &originals),
            before_interruption,
            "{} interrupted repair did not restore configs/artifacts byte-for-byte",
            case.id
        );
        assert_eq!(
            serde_json::to_vec(&latest_receipt(&cli, case.host)).unwrap(),
            receipt_before_interruption,
            "{} interrupted repair did not preserve its durable receipt",
            case.id
        );
        assert_success(
            case.id,
            "interruption recovery",
            cli.run(&["host-bundle", "recover", "--agent", case.id, "--yes"]),
        );
        assert_eq!(
            owned_bytes(&cli, &repaired_receipt, &originals),
            before_interruption,
            "{} recovery did not preserve rolled-back configs/artifacts",
            case.id
        );
        assert_eq!(
            serde_json::to_vec(&latest_receipt(&cli, case.host)).unwrap(),
            receipt_before_interruption,
            "{} recovery did not preserve the pre-interruption receipt",
            case.id
        );
        assert_success(case.id, "post-recovery repair", cli.run(&["reinstall"]));
        let repaired_receipt = latest_receipt(&cli, case.host);
        assert_receipt_digests(&cli, &repaired_receipt);

        let state = cli
            .home
            .path()
            .join(format!("{}-feedback-rollback.json", case.id));
        let dry_run = cli.run_with_env(
            &["feedback-rollback", "dry-run", "--agent", case.id],
            "TRACEDECAY_TEST_FEEDBACK_ROUTE_REVISION",
            "next",
        );
        assert!(
            !String::from_utf8_lossy(&dry_run.stdout).contains(" 0 mutation(s)"),
            "{} feedback route planned no real byte mutation",
            case.id
        );
        assert_success(case.id, "feedback rollback dry-run", dry_run);
        let before_feedback = owned_bytes(&cli, &repaired_receipt, &originals);
        assert_success(
            case.id,
            "feedback rollback apply",
            cli.run_with_env(
                &[
                    "feedback-rollback",
                    "apply",
                    "--agent",
                    case.id,
                    "--state",
                    state.to_str().unwrap(),
                    "--yes",
                ],
                "TRACEDECAY_TEST_FEEDBACK_ROUTE_REVISION",
                "next",
            ),
        );
        let applied_feedback = owned_bytes(&cli, &repaired_receipt, &originals);
        assert_ne!(
            applied_feedback, before_feedback,
            "{} feedback apply did not change any owned bytes",
            case.id
        );
        assert_success(
            case.id,
            "feedback rollback restore",
            cli.run(&[
                "feedback-rollback",
                "restore",
                "--state",
                state.to_str().unwrap(),
                "--yes",
            ]),
        );
        assert_snapshot_eq(
            case.id,
            "feedback rollback",
            &owned_bytes(&cli, &latest_receipt(&cli, case.host), &originals),
            &before_feedback,
        );

        assert_success(
            case.id,
            "uninstall",
            cli.run(&["uninstall", "--agent", case.id]),
        );
        assert_seeded_bytes(&cli, &originals);
        let uninstall_receipt = latest_receipt(&cli, case.host);
        assert!(
            uninstall_receipt
                .component_receipts
                .iter()
                .all(|component| {
                    component
                        .artifacts
                        .iter()
                        .all(|artifact| !cli.home.path().join(&artifact.relative_path).exists())
                })
        );
    }
}

#[test]
fn hermes_dashboard_opt_out_survives_install_update_and_reinstall() {
    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::Hermes);
    seed_host(case, &cli);

    let assert_dashboard_absent = || {
        for plugin in [
            cli.home.path().join(".hermes/plugins/tracedecay"),
            cli.home
                .path()
                .join(".hermes/profiles/review/plugins/tracedecay"),
        ] {
            assert!(!plugin.join("dashboard/manifest.json").exists());
            assert!(!plugin.join("dashboard/plugin_api.py").exists());
            assert!(!plugin.join("dashboard/dist/index.js").exists());
        }
    };

    assert_success(
        case.id,
        "install --no-dashboard",
        cli.run(&["install", "--agent", case.id, "--no-dashboard"]),
    );
    assert_dashboard_absent();
    assert_success(
        case.id,
        "reinstall preflight",
        cli.run(&["reinstall", "--dry-run"]),
    );
    assert_dashboard_absent();
    assert_success(case.id, "update", cli.run(&["update-plugin"]));
    assert_dashboard_absent();
    assert_success(case.id, "reinstall", cli.run(&["reinstall"]));
    assert_dashboard_absent();
}

#[test]
fn feedback_policy_failure_precedes_apply_and_restore_mutations() {
    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::Hermes);
    let originals = seed_host(case, &cli);
    assert_success(
        case.id,
        "install --no-dashboard",
        cli.run(&["install", "--agent", case.id, "--no-dashboard"]),
    );
    let receipt = latest_receipt(&cli, case.host);
    let before_apply = owned_bytes(&cli, &receipt, &originals);
    let config_path = cli.profile.join("config.toml");
    let valid_config = fs::read(&config_path).unwrap();
    let corrupt_config =
        b"installed_agents = [\"hermes\"]\nagent_dashboard_enabled = { hermes = false\n";
    let state = cli.home.path().join("hermes-policy-feedback.json");
    let apply_args = [
        "feedback-rollback",
        "apply",
        "--agent",
        case.id,
        "--state",
        state.to_str().unwrap(),
        "--yes",
    ];

    fs::write(&config_path, corrupt_config).unwrap();
    let refused_apply = cli.run_with_env(
        &apply_args,
        "TRACEDECAY_TEST_FEEDBACK_ROUTE_REVISION",
        "next",
    );
    assert!(!refused_apply.status.success());
    assert!(!state.exists());
    assert_eq!(owned_bytes(&cli, &receipt, &originals), before_apply);

    fs::write(&config_path, &valid_config).unwrap();
    assert_success(
        case.id,
        "feedback apply",
        cli.run_with_env(
            &apply_args,
            "TRACEDECAY_TEST_FEEDBACK_ROUTE_REVISION",
            "next",
        ),
    );
    let applied = owned_bytes(&cli, &receipt, &originals);
    assert_ne!(applied, before_apply);
    assert!(
        applied
            .values()
            .any(|bytes| bytes.ends_with(b"\nfeedback-route:next\n")),
        "feedback activation replaced the exact applied target bytes"
    );

    fs::write(&config_path, corrupt_config).unwrap();
    let refused_restore = cli.run(&[
        "feedback-rollback",
        "restore",
        "--state",
        state.to_str().unwrap(),
        "--yes",
    ]);
    assert!(!refused_restore.status.success());
    assert_eq!(owned_bytes(&cli, &receipt, &originals), applied);

    fs::write(&config_path, valid_config).unwrap();
    assert_success(
        case.id,
        "feedback restore",
        cli.run(&[
            "feedback-rollback",
            "restore",
            "--state",
            state.to_str().unwrap(),
            "--yes",
        ]),
    );
    assert_eq!(owned_bytes(&cli, &receipt, &originals), before_apply);
}

#[test]
fn codex_stale_cache_remediation_executes_on_the_current_stock_cli_and_converges_update() {
    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::Codex);
    let originals = seed_host(case, &cli);

    let staged = cli.run(&["install", "--agent", case.id]);
    assert!(!staged.status.success());
    assert_seeded_bytes(&cli, &originals);
    assert!(
        cli.home
            .path()
            .join(".codex/plugins/tracedecay/.codex-plugin/plugin.json")
            .is_file(),
        "Codex remediation has no staged plugin source"
    );
    assert!(
        cli.home
            .path()
            .join(".agents/plugins/marketplace.json")
            .is_file(),
        "Codex remediation has no staged marketplace entry"
    );
    assert!(
        latest_host_component_set_receipt_at(&cli.lifecycle_root(), case.host)
            .unwrap()
            .is_none(),
        "staging Codex activation published a lifecycle receipt"
    );
    apply_current_codex_plugin_remediation(cli.home.path(), remediation_command(&staged.stderr))
        .unwrap();
    assert_success(
        case.id,
        "receipt-backed install after native activation",
        cli.run(&["install", "--agent", case.id]),
    );

    let cache_manifest = cli
        .home
        .path()
        .join(".codex/plugins/cache/personal/tracedecay")
        .join(tracedecay_agent_hosts::PRODUCT_VERSION)
        .join(".codex-plugin/plugin.json");
    fs::write(
        &cache_manifest,
        br#"{"name":"tracedecay","version":"stale"}"#,
    )
    .unwrap();

    let stale_update = cli.run(&["update-plugin"]);
    assert!(!stale_update.status.success());
    apply_current_codex_plugin_remediation(
        cli.home.path(),
        remediation_command(&stale_update.stderr),
    )
    .unwrap();
    assert_success(
        case.id,
        "update after current stock remediation",
        cli.run(&["update-plugin"]),
    );
}

#[cfg(unix)]
#[test]
fn claude_lifecycle_tracks_assets_only_after_native_activation() {
    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::ClaudeCode);
    let originals = seed_host(case, &cli);

    let deferred = cli.run(&["install", "--agent", case.id]);
    assert!(!deferred.status.success());
    let stderr = String::from_utf8_lossy(&deferred.stderr);
    assert!(
        stderr.contains("Claude Code owns marketplace registration"),
        "Claude deferral omitted its native activation boundary: {stderr}"
    );
    assert!(
        cli.home
            .path()
            .join(".claude/plugins/marketplaces/tracedecay/.claude-plugin/marketplace.json")
            .is_file(),
        "Claude deferral did not stage the verified marketplace source"
    );
    assert_seeded_bytes(&cli, &originals);
    assert!(
        latest_host_component_set_receipt_at(&cli.lifecycle_root(), case.host)
            .unwrap()
            .is_none(),
        "staging native activation published a lifecycle receipt"
    );

    set_claude_native_activation(cli.home.path(), true);
    let settings_path = cli.home.path().join(".claude/settings.json");
    let marketplaces_path = cli
        .home
        .path()
        .join(".claude/plugins/known_marketplaces.json");
    let active_native_state = [
        fs::read(&settings_path).unwrap(),
        fs::read(&marketplaces_path).unwrap(),
    ];
    assert_success(
        case.id,
        "receipt-backed install after native activation",
        cli.run(&["install", "--agent", case.id]),
    );
    let install_receipt = latest_receipt(&cli, case.host);
    assert_receipt_digests(&cli, &install_receipt);
    assert_eq!(
        [
            fs::read(&settings_path).unwrap(),
            fs::read(&marketplaces_path).unwrap(),
        ],
        active_native_state,
        "catalog install rewrote Claude-owned activation state"
    );

    let cache_manifest = cli
        .home
        .path()
        .join(".claude/plugins/cache/tracedecay/tracedecay")
        .join(tracedecay_agent_hosts::PRODUCT_VERSION)
        .join(".claude-plugin/plugin.json");
    fs::write(
        &cache_manifest,
        br#"{"name":"tracedecay","version":"stale"}"#,
    )
    .unwrap();
    let before_stale_update = serde_json::to_vec(&latest_receipt(&cli, case.host)).unwrap();
    let stale_update = cli.run(&["update-plugin"]);
    assert!(!stale_update.status.success());
    assert!(
        String::from_utf8_lossy(&stale_update.stderr).contains("loaded TraceDecay cache is stale"),
        "Claude stale cache did not produce native-update remediation: {}",
        String::from_utf8_lossy(&stale_update.stderr)
    );
    assert_eq!(
        serde_json::to_vec(&latest_receipt(&cli, case.host)).unwrap(),
        before_stale_update,
        "stale native cache changed the component receipt"
    );
    fs::copy(
        cli.home
            .path()
            .join(".claude/plugins/marketplaces/tracedecay/.claude-plugin/plugin.json"),
        &cache_manifest,
    )
    .unwrap();
    assert_success(
        case.id,
        "catalog update after native cache refresh",
        cli.run(&["update-plugin"]),
    );
    assert_success(case.id, "catalog repair", cli.run(&["reinstall"]));
    for (phase, entrypoint, fixture) in native_feedback(case) {
        assert_success(case.id, phase, cli.run_with_stdin(&[entrypoint], &fixture));
    }
    assert_eq!(
        [
            fs::read(&settings_path).unwrap(),
            fs::read(&marketplaces_path).unwrap(),
        ],
        active_native_state,
        "catalog maintenance rewrote Claude-owned activation state"
    );

    let claude_invocations = install_current_claude_cli(cli.home.path(), &cli.bin_dir);
    assert_success(
        case.id,
        "stock CLI-backed uninstall",
        cli.run(&["uninstall", "--agent", case.id]),
    );
    assert_eq!(
        recorded_claude_invocations(&claude_invocations),
        [
            "plugin uninstall tracedecay",
            "plugin marketplace remove tracedecay",
        ],
        "Claude removal must use the current stock plugin lifecycle grammar"
    );
    let removed_settings: serde_json::Value =
        serde_json::from_slice(&fs::read(&settings_path).unwrap()).unwrap();
    let removed_marketplaces: serde_json::Value =
        serde_json::from_slice(&fs::read(&marketplaces_path).unwrap()).unwrap();
    assert_eq!(removed_settings["enabledPlugins"]["foreign@market"], true);
    assert!(
        removed_settings["enabledPlugins"]
            .get("tracedecay@tracedecay")
            .is_none()
    );
    assert!(removed_marketplaces.get("foreign").is_some());
    assert!(removed_marketplaces.get("tracedecay").is_none());
    let uninstall_receipt = latest_receipt(&cli, case.host);
    assert!(
        uninstall_receipt
            .component_receipts
            .iter()
            .all(|component| {
                component
                    .artifacts
                    .iter()
                    .all(|artifact| !cli.home.path().join(&artifact.relative_path).exists())
            })
    );
}

#[test]
fn kimi_lifecycle_reports_official_activation_deferral() {
    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::KimiCode);

    let output = cli.run(&["install", "--agent", case.id]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Kimi") && stderr.contains("plugin"),
        "Kimi deferral omitted its official activation boundary: {stderr}"
    );
    assert!(
        latest_host_component_set_receipt_at(&cli.lifecycle_root(), case.host)
            .unwrap()
            .is_none()
    );
}

#[test]
fn unadmitted_catalog_hosts_never_fall_back_to_direct_installers() {
    for host in HostKindV1::ALL {
        let Some(reason) = unsupported_host_component_set_reason(host) else {
            continue;
        };
        if matches!(host, HostKindV1::CursorCloud | HostKindV1::ClineFamily) {
            continue;
        }
        let cli = IsolatedCli::new();
        let agent = tracedecay::agents::integration_id_for_host(host);

        let output = cli.run(&["install", "--agent", agent]);

        assert!(
            !output.status.success(),
            "{host:?} bypassed typed component unavailability through a direct installer"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!("{reason:?}")),
            "{host:?} did not report its catalog reason {reason:?}: {stderr}"
        );
        assert!(
            latest_host_component_set_receipt_at(&cli.lifecycle_root(), host)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn killed_registration_mutation_recovers_exact_pre_effect_state() {
    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::OpenCode);
    let originals = seed_host(case, &cli);
    assert_success(
        case.id,
        "initial install",
        cli.run(&["install", "--agent", case.id]),
    );
    let receipt = latest_receipt(&cli, case.host);
    let config_path = cli.home.path().join(".config/opencode/opencode.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    config["mcp"]["tracedecay"]["command"] = serde_json::json!(["operator-owned", "pending"]);
    fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o640)).unwrap();
    }
    let before = owned_bytes(&cli, &receipt, &originals);
    let receipt_before = serde_json::to_vec(&latest_receipt(&cli, case.host)).unwrap();

    let killed = cli.run_with_env(
        &["reinstall"],
        "TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE",
        "1",
    );
    assert!(!killed.status.success(), "fault subprocess did not abort");
    assert_ne!(
        fs::read(&config_path).unwrap(),
        before[&PathBuf::from(".config/opencode/opencode.json")],
        "fault boundary did not cross a real host-config mutation"
    );

    assert_success(
        case.id,
        "restart recovery",
        cli.run(&["host-bundle", "recover", "--agent", case.id, "--yes"]),
    );
    assert_eq!(owned_bytes(&cli, &receipt, &originals), before);
    assert_eq!(
        serde_json::to_vec(&latest_receipt(&cli, case.host)).unwrap(),
        receipt_before
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            fs::metadata(&config_path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
}

#[test]
fn killed_install_recovers_with_original_journal_operation() {
    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::OpenCode);
    let originals = seed_host(case, &cli);
    let config_path = cli.home.path().join(".config/opencode/opencode.json");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&config_path, fs::Permissions::from_mode(0o640)).unwrap();
    }
    let killed = cli.run_with_env(
        &["install", "--agent", case.id],
        "TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE",
        "1",
    );
    assert!(
        !killed.status.success(),
        "install fault subprocess did not abort"
    );
    assert_ne!(
        fs::read(&config_path).unwrap(),
        originals[&PathBuf::from(".config/opencode/opencode.json")]
    );
    assert_success(
        case.id,
        "install restart recovery",
        cli.run(&["host-bundle", "recover", "--agent", case.id, "--yes"]),
    );
    assert_seeded_bytes(&cli, &originals);
    assert!(
        latest_host_component_set_receipt_at(&cli.lifecycle_root(), case.host)
            .unwrap()
            .is_none()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            fs::metadata(&config_path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn recovery_rejects_foreign_metadata_drift_with_unchanged_bytes() {
    use std::os::unix::fs::PermissionsExt;

    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::OpenCode);
    seed_host(case, &cli);
    let config_path = cli.home.path().join(".config/opencode/opencode.json");
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o640)).unwrap();
    let mut original_acl = 2_u32.to_le_bytes().to_vec();
    for (tag, permissions, id) in [
        (0x01_u16, 0x06_u16, u32::MAX),
        (0x02, 0x04, 65_534),
        (0x04, 0x04, u32::MAX),
        (0x10, 0x04, u32::MAX),
        (0x20, 0x00, u32::MAX),
    ] {
        original_acl.extend_from_slice(&tag.to_le_bytes());
        original_acl.extend_from_slice(&permissions.to_le_bytes());
        original_acl.extend_from_slice(&id.to_le_bytes());
    }
    xattr::set(&config_path, "system.posix_acl_access", &original_acl).unwrap();
    let killed = cli.run_with_env(
        &["install", "--agent", case.id],
        "TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE",
        "1",
    );
    assert!(!killed.status.success());
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).unwrap();

    let bytes_after_kill = fs::read(&config_path).unwrap();
    let acl_after_drift = xattr::get(&config_path, "system.posix_acl_access").unwrap();
    let refused = cli.run(&["host-bundle", "recover", "--agent", case.id, "--yes"]);
    assert!(!refused.status.success());
    assert_eq!(fs::read(&config_path).unwrap(), bytes_after_kill);
    assert_eq!(
        fs::metadata(&config_path).unwrap().permissions().mode() & 0o777,
        0o600,
        "recovery must not overwrite foreign metadata drift"
    );
    assert_eq!(
        xattr::get(&config_path, "system.posix_acl_access").unwrap(),
        acl_after_drift
    );
    assert_ne!(acl_after_drift, Some(original_acl));
}

#[test]
fn interrupted_registration_rollback_converges_across_two_restarts() {
    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::OpenCode);
    let originals = seed_host(case, &cli);
    let config_path = cli.home.path().join(".config/opencode/opencode.json");
    let killed = cli.run_with_env(
        &["install", "--agent", case.id],
        "TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE",
        "1",
    );
    assert!(!killed.status.success());

    let mut recovery = cli.command(&["host-bundle", "recover", "--agent", case.id, "--yes"]);
    let interrupted = recovery
        .env(
            "TRACEDECAY_TEST_ABORT_AFTER_REGISTRATION_ROLLBACK_WRITE_PATH",
            &config_path,
        )
        .output()
        .unwrap();
    assert!(!interrupted.status.success());
    assert_seeded_bytes(&cli, &originals);

    assert_success(
        case.id,
        "rollback restart",
        cli.run(&["host-bundle", "recover", "--agent", case.id, "--yes"]),
    );
    assert_seeded_bytes(&cli, &originals);
    assert_success(
        case.id,
        "idempotent rollback restart",
        cli.run(&["host-bundle", "recover", "--agent", case.id, "--yes"]),
    );
    assert_seeded_bytes(&cli, &originals);
}

#[cfg(target_os = "linux")]
#[test]
fn successful_atomic_replacement_preserves_mode_and_extended_acl() {
    use std::os::unix::fs::PermissionsExt;

    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::OpenCode);
    let originals = seed_host(case, &cli);
    let config_path = cli.home.path().join(".config/opencode/opencode.json");
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o640)).unwrap();
    let mut original_acl = 2_u32.to_le_bytes().to_vec();
    for (tag, permissions, id) in [
        (0x01_u16, 0x06_u16, u32::MAX),
        (0x02, 0x04, 65_534),
        (0x04, 0x04, u32::MAX),
        (0x10, 0x04, u32::MAX),
        (0x20, 0x00, u32::MAX),
    ] {
        original_acl.extend_from_slice(&tag.to_le_bytes());
        original_acl.extend_from_slice(&permissions.to_le_bytes());
        original_acl.extend_from_slice(&id.to_le_bytes());
    }
    xattr::set(&config_path, "system.posix_acl_access", &original_acl).unwrap();

    assert_success(
        case.id,
        "ACL-preserving install",
        cli.run(&["install", "--agent", case.id]),
    );
    assert_ne!(
        fs::read(&config_path).unwrap(),
        originals[&PathBuf::from(".config/opencode/opencode.json")]
    );
    assert_eq!(
        fs::metadata(&config_path).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(
        xattr::get(&config_path, "system.posix_acl_access").unwrap(),
        Some(original_acl)
    );
}

#[cfg(unix)]
#[test]
fn claude_install_rejects_empty_symlinked_config_directory() {
    use std::os::unix::fs::symlink;

    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::ClaudeCode);
    seed_host(case, &cli);
    let claude_dir = cli.home.path().join(".claude");
    fs::remove_dir_all(&claude_dir).unwrap();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), &claude_dir).unwrap();

    let refused = cli.run(&["install", "--agent", case.id]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("Claude home configuration path ~/.claude is a symlink"),
        "Claude symlink refusal omitted the typed path boundary: {stderr}"
    );
    assert!(
        stderr.contains("replace it with a real directory"),
        "Claude symlink refusal omitted actionable remediation: {stderr}"
    );
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[test]
fn killed_install_recovery_refuses_later_operator_edit() {
    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::OpenCode);
    let originals = seed_host(case, &cli);
    let killed = cli.run_with_env(
        &["install", "--agent", case.id],
        "TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE",
        "1",
    );
    assert!(!killed.status.success());

    let config_path = cli.home.path().join(".config/opencode/opencode.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    config["operatorAfterKill"] = serde_json::json!(true);
    fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    let config_before = fs::read(&config_path).unwrap();
    let receipt_before =
        latest_host_component_set_receipt_at(&cli.lifecycle_root(), case.host).unwrap();

    let refused = cli.run(&["host-bundle", "recover", "--agent", case.id, "--yes"]);
    assert!(!refused.status.success());
    assert_eq!(fs::read(&config_path).unwrap(), config_before);
    assert_eq!(
        latest_host_component_set_receipt_at(&cli.lifecycle_root(), case.host).unwrap(),
        receipt_before
    );
    for relative in originals.keys() {
        if relative != &PathBuf::from(".config/opencode/opencode.json") {
            assert_eq!(
                fs::read(cli.home.path().join(relative)).unwrap(),
                originals[relative]
            );
        }
    }
}

#[test]
fn stale_feedback_registration_refuses_before_artifact_or_receipt_effects() {
    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::OpenCode);
    let originals = seed_host(case, &cli);
    assert_success(
        case.id,
        "initial install",
        cli.run(&["install", "--agent", case.id]),
    );
    let state = cli.home.path().join("opencode-feedback-rollback.json");
    assert_success(
        case.id,
        "feedback apply",
        cli.run_with_env(
            &[
                "feedback-rollback",
                "apply",
                "--agent",
                case.id,
                "--state",
                state.to_str().unwrap(),
                "--yes",
            ],
            "TRACEDECAY_TEST_FEEDBACK_ROUTE_REVISION",
            "next",
        ),
    );

    let config_path = cli.home.path().join(".config/opencode/opencode.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    config["operatorConcurrentEdit"] = serde_json::json!({"preserve": true});
    fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    let active_receipt = latest_receipt(&cli, case.host);
    let artifact_snapshot = owned_bytes(&cli, &active_receipt, &originals);
    let receipt_snapshot = serde_json::to_vec(&active_receipt).unwrap();
    let config_snapshot = fs::read(&config_path).unwrap();
    let state_snapshot = fs::read(&state).unwrap();

    let refused = cli.run(&[
        "feedback-rollback",
        "restore",
        "--state",
        state.to_str().unwrap(),
        "--yes",
    ]);
    assert!(
        !refused.status.success(),
        "stale restore unexpectedly succeeded"
    );
    assert_eq!(
        owned_bytes(&cli, &active_receipt, &originals),
        artifact_snapshot,
        "stale registration changed managed artifacts"
    );
    assert_eq!(
        serde_json::to_vec(&latest_receipt(&cli, case.host)).unwrap(),
        receipt_snapshot,
        "stale registration changed the durable receipt"
    );
    assert_eq!(
        fs::read(&config_path).unwrap(),
        config_snapshot,
        "stale registration was overwritten"
    );
    assert_eq!(
        fs::read(&state).unwrap(),
        state_snapshot,
        "stale restore mutated its recovery state"
    );
}

#[test]
fn killed_feedback_switch_recovers_from_durable_effect_identity() {
    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::OpenCode);
    let originals = seed_host(case, &cli);
    assert_success(
        case.id,
        "initial install",
        cli.run(&["install", "--agent", case.id]),
    );
    let before_receipt = latest_receipt(&cli, case.host);
    #[cfg(unix)]
    let permission_path = cli.home.path().join(
        &before_receipt
            .component_receipts
            .iter()
            .find(|receipt| receipt.component == HostBundleComponentV1::Core)
            .unwrap()
            .artifacts[0]
            .relative_path,
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&permission_path, fs::Permissions::from_mode(0o640)).unwrap();
    }
    let before = owned_bytes(&cli, &before_receipt, &originals);
    let before_core = latest_host_component_receipt_at(
        &cli.lifecycle_root(),
        case.host,
        HostBundleComponentV1::Core,
    )
    .unwrap()
    .unwrap();
    let state = cli.home.path().join("killed-feedback-rollback.json");
    let mut command = cli.command(&[
        "feedback-rollback",
        "apply",
        "--agent",
        case.id,
        "--state",
        state.to_str().unwrap(),
        "--yes",
    ]);
    let killed = command
        .env("TRACEDECAY_TEST_FEEDBACK_ROUTE_REVISION", "killed")
        .env("TRACEDECAY_TEST_ABORT_AFTER_FEEDBACK_SWITCH", "1")
        .output()
        .unwrap();
    assert!(
        !killed.status.success(),
        "feedback fault subprocess did not abort"
    );
    assert_ne!(
        owned_bytes(&cli, &before_receipt, &originals),
        before,
        "feedback fault boundary did not cross an artifact mutation"
    );
    assert_ne!(
        latest_host_component_receipt_at(
            &cli.lifecycle_root(),
            case.host,
            HostBundleComponentV1::Core
        )
        .unwrap()
        .unwrap(),
        before_core,
        "feedback fault boundary did not publish its component receipt"
    );

    assert_success(
        case.id,
        "feedback restart recovery",
        cli.run(&[
            "feedback-rollback",
            "restore",
            "--state",
            state.to_str().unwrap(),
            "--yes",
        ]),
    );
    assert_eq!(owned_bytes(&cli, &before_receipt, &originals), before);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            fs::metadata(permission_path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
}

#[test]
fn newer_feedback_receipt_refuses_before_restore_effects() {
    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::OpenCode);
    let originals = seed_host(case, &cli);
    assert_success(
        case.id,
        "initial install",
        cli.run(&["install", "--agent", case.id]),
    );
    let state = cli.home.path().join("receipt-cas-feedback-rollback.json");
    assert_success(
        case.id,
        "feedback apply",
        cli.run_with_env(
            &[
                "feedback-rollback",
                "apply",
                "--agent",
                case.id,
                "--state",
                state.to_str().unwrap(),
                "--yes",
            ],
            "TRACEDECAY_TEST_FEEDBACK_ROUTE_REVISION",
            "receipt-cas",
        ),
    );
    let receipt = latest_receipt(&cli, case.host);
    let receipt_path = cli
        .lifecycle_root()
        .join(".tracedecay-host-bundle-v1")
        .join(format!(
            "component-set-receipt.{}.v1.json",
            hex::encode(receipt.operation_id)
        ));
    let mut receipt_json: serde_json::Value =
        serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    receipt_json["operation_id"] = serde_json::to_value([91_u8; 16]).unwrap();
    fs::write(&receipt_path, serde_json::to_vec(&receipt_json).unwrap()).unwrap();
    let before = owned_bytes(&cli, &receipt, &originals);
    let receipt_before = fs::read(&receipt_path).unwrap();
    let state_before = fs::read(&state).unwrap();

    let refused = cli.run(&[
        "feedback-rollback",
        "restore",
        "--state",
        state.to_str().unwrap(),
        "--yes",
    ]);
    assert!(!refused.status.success(), "newer receipt was overwritten");
    assert_eq!(owned_bytes(&cli, &receipt, &originals), before);
    assert_eq!(fs::read(&receipt_path).unwrap(), receipt_before);
    assert_eq!(fs::read(&state).unwrap(), state_before);
}

#[test]
fn killed_feedback_registration_recovers_without_applied_marker() {
    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::OpenCode);
    let originals = seed_host(case, &cli);
    assert_success(
        case.id,
        "initial install",
        cli.run(&["install", "--agent", case.id]),
    );
    let receipt = latest_receipt(&cli, case.host);
    let before = owned_bytes(&cli, &receipt, &originals);
    let state = cli.home.path().join("killed-registration-feedback.json");
    let mut command = cli.command(&[
        "feedback-rollback",
        "apply",
        "--agent",
        case.id,
        "--state",
        state.to_str().unwrap(),
        "--yes",
    ]);
    let killed = command
        .env(
            "TRACEDECAY_TEST_FEEDBACK_ROUTE_REVISION",
            "registration-kill",
        )
        .env("TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE", "1")
        .output()
        .unwrap();
    assert!(
        !killed.status.success(),
        "registration fault subprocess did not abort"
    );
    assert_ne!(owned_bytes(&cli, &receipt, &originals), before);

    assert_success(
        case.id,
        "registration restart recovery",
        cli.run(&[
            "feedback-rollback",
            "restore",
            "--state",
            state.to_str().unwrap(),
            "--yes",
        ]),
    );
    assert_eq!(owned_bytes(&cli, &receipt, &originals), before);
}

#[test]
fn killed_feedback_registration_rejects_later_operator_edit() {
    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::OpenCode);
    let originals = seed_host(case, &cli);
    assert_success(
        case.id,
        "initial install",
        cli.run(&["install", "--agent", case.id]),
    );
    let state = cli.home.path().join("killed-registration-stale.json");
    let mut command = cli.command(&[
        "feedback-rollback",
        "apply",
        "--agent",
        case.id,
        "--state",
        state.to_str().unwrap(),
        "--yes",
    ]);
    let killed = command
        .env(
            "TRACEDECAY_TEST_FEEDBACK_ROUTE_REVISION",
            "registration-stale",
        )
        .env("TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE", "1")
        .output()
        .unwrap();
    assert!(!killed.status.success());

    let config_path = cli.home.path().join(".config/opencode/opencode.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    config["operatorAfterKill"] = serde_json::json!(true);
    fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    let receipt = latest_receipt(&cli, case.host);
    let before = owned_bytes(&cli, &receipt, &originals);
    let state_before = fs::read(&state).unwrap();

    let refused = cli.run(&[
        "feedback-rollback",
        "restore",
        "--state",
        state.to_str().unwrap(),
        "--yes",
    ]);
    assert!(!refused.status.success());
    assert_eq!(owned_bytes(&cli, &receipt, &originals), before);
    assert_eq!(fs::read(&state).unwrap(), state_before);
}

#[cfg(unix)]
#[test]
fn killed_feedback_registration_rejects_metadata_only_drift() {
    use std::os::unix::fs::PermissionsExt;

    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::OpenCode);
    seed_host(case, &cli);
    assert_success(
        case.id,
        "initial install",
        cli.run(&["install", "--agent", case.id]),
    );
    let state = cli.home.path().join("killed-registration-metadata.json");
    let mut command = cli.command(&[
        "feedback-rollback",
        "apply",
        "--agent",
        case.id,
        "--state",
        state.to_str().unwrap(),
        "--yes",
    ]);
    let killed = command
        .env(
            "TRACEDECAY_TEST_FEEDBACK_ROUTE_REVISION",
            "registration-metadata",
        )
        .env("TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE", "1")
        .output()
        .unwrap();
    assert!(!killed.status.success());

    let config_path = cli.home.path().join(".config/opencode/opencode.json");
    let current_mode = fs::metadata(&config_path).unwrap().permissions().mode() & 0o777;
    let drifted_mode = if current_mode == 0o600 { 0o640 } else { 0o600 };
    fs::set_permissions(&config_path, fs::Permissions::from_mode(drifted_mode)).unwrap();
    let bytes_after_kill = fs::read(&config_path).unwrap();
    let refused = cli.run(&[
        "feedback-rollback",
        "restore",
        "--state",
        state.to_str().unwrap(),
        "--yes",
    ]);
    assert!(!refused.status.success());
    assert_eq!(fs::read(&config_path).unwrap(), bytes_after_kill);
    assert_eq!(
        fs::metadata(&config_path).unwrap().permissions().mode() & 0o777,
        drifted_mode
    );
}

#[test]
fn killed_feedback_restore_converges_on_restart() {
    let cli = IsolatedCli::new();
    let case = host_case(HostKindV1::OpenCode);
    let originals = seed_host(case, &cli);
    assert_success(
        case.id,
        "initial install",
        cli.run(&["install", "--agent", case.id]),
    );
    let receipt = latest_receipt(&cli, case.host);
    let before = owned_bytes(&cli, &receipt, &originals);
    let state = cli.home.path().join("killed-restore-feedback.json");
    assert_success(
        case.id,
        "feedback apply",
        cli.run_with_env(
            &[
                "feedback-rollback",
                "apply",
                "--agent",
                case.id,
                "--state",
                state.to_str().unwrap(),
                "--yes",
            ],
            "TRACEDECAY_TEST_FEEDBACK_ROUTE_REVISION",
            "restore-kill",
        ),
    );
    let killed = cli.run_with_env(
        &[
            "feedback-rollback",
            "restore",
            "--state",
            state.to_str().unwrap(),
            "--yes",
        ],
        "TRACEDECAY_TEST_ABORT_AFTER_FEEDBACK_RESTORE",
        "1",
    );
    assert!(
        !killed.status.success(),
        "restore fault subprocess did not abort"
    );
    assert_eq!(
        owned_bytes(&cli, &receipt, &originals),
        before,
        "restore receipt boundary was not reached after artifact restoration"
    );

    assert_success(
        case.id,
        "restore restart recovery",
        cli.run(&[
            "feedback-rollback",
            "restore",
            "--state",
            state.to_str().unwrap(),
            "--yes",
        ]),
    );
    assert_eq!(owned_bytes(&cli, &receipt, &originals), before);
}
