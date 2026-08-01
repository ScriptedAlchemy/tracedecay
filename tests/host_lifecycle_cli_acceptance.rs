use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay::agents::host_bundle_v2::{
    HostBundleComponentV1, HostComponentSetReceiptV1, HostKindV1, latest_host_component_receipt_at,
    latest_host_component_set_receipt_at,
};

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
const HERMES_CONFIGS: &[(&str, &[u8])] = &[(
    ".hermes/config.yaml",
    b"theme: dark\nplugins:\n  enabled:\n    - foreign\n",
)];
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

const HOSTS: &[HostCase] = &[
    HostCase {
        id: "claude",
        host: HostKindV1::ClaudeCode,
        configs: CLAUDE_CONFIGS,
    },
    HostCase {
        id: "cursor",
        host: HostKindV1::CursorDesktop,
        configs: CURSOR_CONFIGS,
    },
    HostCase {
        id: "codex",
        host: HostKindV1::Codex,
        configs: CODEX_CONFIGS,
    },
    HostCase {
        id: "hermes",
        host: HostKindV1::Hermes,
        configs: HERMES_CONFIGS,
    },
    HostCase {
        id: "kiro",
        host: HostKindV1::Kiro,
        configs: KIRO_CONFIGS,
    },
    HostCase {
        id: "opencode",
        host: HostKindV1::OpenCode,
        configs: OPENCODE_CONFIGS,
    },
];

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

fn pending_registration_backup(cli: &IsolatedCli, integration_id: &str) -> PathBuf {
    let root = cli
        .lifecycle_root()
        .join(".tracedecay-host-bundle-v1/registration-backups");
    let entries = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1, "expected one pending registration backup");
    entries.into_iter().next().unwrap().join(integration_id)
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

fn native_feedback(case: HostCase) -> Option<[(&'static str, Vec<u8>); 2]> {
    match case.host {
        HostKindV1::ClaudeCode => Some([
            (
                "hook-claude-post-tool-use",
                include_bytes!(
                    "../crates/tracedecay-hooks/fixtures/host_events/claude/post_tool_use_write.json"
                )
                .to_vec(),
            ),
            (
                "hook-stop",
                include_bytes!("../crates/tracedecay-hooks/fixtures/host_events/claude/stop.json")
                    .to_vec(),
            ),
        ]),
        HostKindV1::CursorDesktop => {
            let packet = include_str!("../crates/tracedecay-hooks/fixtures/host_events/cursor.json");
            Some([
                (
                    "hook-cursor-after-file-edit",
                    packet_request(packet, "saved_edit"),
                ),
                ("hook-cursor-stop", packet_request(packet, "stop")),
            ])
        }
        HostKindV1::Codex => {
            let packet = include_str!("../crates/tracedecay-hooks/fixtures/host_events/codex.json");
            Some([
                (
                    "hook-codex-post-tool-use",
                    packet_request(packet, "saved_edit"),
                ),
                (
                    "hook-codex-stop",
                    include_bytes!(
                        "../crates/tracedecay-hooks/fixtures/host_events/codex/stop.json"
                    )
                    .to_vec(),
                ),
            ])
        }
        HostKindV1::Hermes => Some([
            (
                "hook-hermes-terminal-receipt",
                include_bytes!(
                    "../crates/tracedecay-hooks/fixtures/host_events/hermes/saved-edit.json"
                )
                .to_vec(),
            ),
            (
                "hook-hermes-terminal-receipt",
                include_bytes!("../crates/tracedecay-hooks/fixtures/host_events/hermes/stop.json")
                    .to_vec(),
            ),
        ]),
        HostKindV1::OpenCode => {
            let packet =
                include_str!("../crates/tracedecay-hooks/fixtures/host_events/opencode/baseline.json");
            Some([
                (
                    "hook-opencode-event",
                    packet_request(packet, "saved_edit"),
                ),
                ("hook-opencode-event", packet_request(packet, "stop")),
            ])
        }
        HostKindV1::Kiro => None,
        _ => unreachable!("non-acceptance host"),
    }
}

#[test]
fn production_cli_completes_deterministic_lifecycle_for_config_native_hosts() {
    for case in HOSTS
        .iter()
        .filter(|case| !matches!(case.host, HostKindV1::Codex | HostKindV1::Kiro))
    {
        let cli = IsolatedCli::new();
        let originals = seed_host(*case, &cli);

        assert_success(
            case.id,
            "install",
            cli.run(&["install", "--agent", case.id]),
        );
        let install_receipt = latest_receipt(&cli, case.host);
        assert_receipt_digests(&cli, &install_receipt);
        if case.host == HostKindV1::ClaudeCode {
            let settings: serde_json::Value = serde_json::from_slice(
                &fs::read(cli.home.path().join(".claude/settings.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                settings
                    .pointer("/env/FOREIGN_SETTING")
                    .and_then(serde_json::Value::as_str),
                Some("preserved"),
                "Claude install dropped an unrelated native setting"
            );
        }

        assert_success(case.id, "update", cli.run(&["update-plugin"]));
        let update_receipt = latest_receipt(&cli, case.host);
        assert_receipt_digests(&cli, &update_receipt);

        if let Some(events) = native_feedback(*case) {
            for (phase, (entrypoint, fixture)) in ["edit", "stop"].into_iter().zip(events) {
                assert_success(case.id, phase, cli.run_with_stdin(&[entrypoint], &fixture));
            }
        }

        let repair_target = update_receipt
            .component_receipts
            .iter()
            .flat_map(|component| &component.artifacts)
            .find(|artifact| {
                !artifact.relative_path.ends_with(".json")
                    && !artifact.relative_path.ends_with(".toml")
                    && !artifact.relative_path.ends_with(".yaml")
                    && !artifact.relative_path.ends_with(".yml")
            })
            .expect("non-registration repair artifact");
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
fn codex_lifecycle_refuses_unavailable_noninteractive_activation() {
    let cli = IsolatedCli::new();
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::Codex)
        .unwrap();
    let originals = seed_host(case, &cli);

    let output = cli.run(&["install", "--agent", "codex"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unsupported") || stderr.contains("unavailable"),
        "Codex capability denial was not reported honestly: {stderr}"
    );
    assert!(
        stderr.contains("plugin UI")
            && stderr.contains("source was staged")
            && stderr.contains("not installed"),
        "Codex denial omitted manual typed remediation: {stderr}"
    );
    assert_seeded_bytes(&cli, &originals);
    assert!(
        cli.home
            .path()
            .join("plugins/tracedecay/.codex-plugin/plugin.json")
            .is_file(),
        "Codex manual remediation has no staged plugin source"
    );
    assert!(
        cli.home
            .path()
            .join(".agents/plugins/marketplace.json")
            .is_file(),
        "Codex manual remediation has no staged marketplace entry"
    );
    assert!(
        latest_host_component_set_receipt_at(&cli.lifecycle_root(), HostKindV1::Codex)
            .unwrap()
            .is_none()
    );
}

#[test]
fn killed_registration_mutation_recovers_exact_pre_effect_state() {
    let cli = IsolatedCli::new();
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::OpenCode)
        .unwrap();
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
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::OpenCode)
        .unwrap();
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
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::OpenCode)
        .unwrap();
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
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::OpenCode)
        .unwrap();
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
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::OpenCode)
        .unwrap();
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

#[test]
fn claude_nonempty_rewrite_recovers_after_real_write_kill() {
    let cli = IsolatedCli::new();
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::ClaudeCode)
        .unwrap();
    seed_host(case, &cli);
    assert_success(
        case.id,
        "initial install",
        cli.run(&["install", "--agent", case.id]),
    );
    let claude_md = cli.home.path().join(".claude/CLAUDE.md");
    let installed = fs::read(&claude_md).unwrap();
    let mut with_foreign = b"# Operator preface\n\n".to_vec();
    with_foreign.extend_from_slice(&installed);
    with_foreign.extend_from_slice(b"\n# Operator suffix\n");
    fs::write(&claude_md, &with_foreign).unwrap();

    let mut command = cli.command(&["uninstall", "--agent", case.id]);
    let killed = command
        .env(
            "TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE_PATH",
            &claude_md,
        )
        .output()
        .unwrap();
    assert!(
        !killed.status.success(),
        "Claude rewrite fault did not abort"
    );
    let after_kill = fs::read(&claude_md).unwrap();
    assert_ne!(after_kill, with_foreign);
    assert!(
        !after_kill.is_empty(),
        "fault crossed a removal, not rewrite"
    );

    assert_success(
        case.id,
        "Claude rewrite recovery",
        cli.run(&["host-bundle", "recover", "--agent", case.id, "--yes"]),
    );
    assert_eq!(fs::read(&claude_md).unwrap(), with_foreign);
}

#[test]
fn claude_global_install_recovers_project_config_mutations() {
    let cli = IsolatedCli::new();
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::ClaudeCode)
        .unwrap();
    seed_host(case, &cli);
    let mcp_path = cli.project.path().join(".mcp.json");
    let settings_path = cli.project.path().join(".claude/settings.local.json");
    let mcp_original =
        br#"{"mcpServers":{"tracedecay":{"command":"old"},"foreign":{"command":"keep"}}}
"#;
    let settings_original =
        br#"{"enabledMcpjsonServers":["tracedecay","foreign"],"operator":"keep"}
"#;
    fs::write(&mcp_path, mcp_original).unwrap();
    fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    fs::write(&settings_path, settings_original).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(
            cli.home.path().join(".claude"),
            fs::Permissions::from_mode(0o710),
        )
        .unwrap();
        fs::set_permissions(
            settings_path.parent().unwrap(),
            fs::Permissions::from_mode(0o750),
        )
        .unwrap();
    }

    let mut command = cli.command(&["install", "--agent", case.id]);
    let killed = command
        .env(
            "TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE_PATH",
            &settings_path,
        )
        .output()
        .unwrap();
    assert!(!killed.status.success());
    assert_ne!(
        fs::read(&mcp_path).unwrap(),
        mcp_original,
        "install stopped before project MCP mutation\nstderr:\n{}",
        String::from_utf8_lossy(&killed.stderr)
    );
    assert_ne!(fs::read(&settings_path).unwrap(), settings_original);

    let mut recovery = cli.command(&["host-bundle", "recover", "--agent", case.id, "--yes"]);
    let interrupted = recovery
        .env(
            "TRACEDECAY_TEST_ABORT_AFTER_REGISTRATION_ROLLBACK_WRITE_PATH",
            &settings_path,
        )
        .output()
        .unwrap();
    assert!(!interrupted.status.success());
    assert_success(
        case.id,
        "Claude interrupted rollback restart",
        cli.run(&["host-bundle", "recover", "--agent", case.id, "--yes"]),
    );
    assert_eq!(fs::read(&mcp_path).unwrap(), mcp_original);
    assert_eq!(fs::read(&settings_path).unwrap(), settings_original);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            fs::metadata(cli.home.path().join(".claude"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o710
        );
        assert_eq!(
            fs::metadata(settings_path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
    }
    assert_success(
        case.id,
        "Claude idempotent rollback restart",
        cli.run(&["host-bundle", "recover", "--agent", case.id, "--yes"]),
    );
    assert_eq!(fs::read(&mcp_path).unwrap(), mcp_original);
    assert_eq!(fs::read(&settings_path).unwrap(), settings_original);
}

#[test]
fn claude_recovery_recreates_vanished_directory_with_modified_config() {
    let cli = IsolatedCli::new();
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::ClaudeCode)
        .unwrap();
    let originals = seed_host(case, &cli);
    let settings_path = cli.home.path().join(".claude/settings.json");
    let killed = cli.run_with_env(
        &["install", "--agent", case.id],
        "TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE_PATH",
        settings_path.to_str().unwrap(),
    );
    assert!(!killed.status.success());
    assert_ne!(
        fs::read(&settings_path).unwrap(),
        originals[&PathBuf::from(".claude/settings.json")]
    );
    fs::remove_dir_all(cli.home.path().join(".claude")).unwrap();

    assert_success(
        case.id,
        "vanished modified directory recovery",
        cli.run(&["host-bundle", "recover", "--agent", case.id, "--yes"]),
    );
    assert_eq!(
        fs::read(&settings_path).unwrap(),
        originals[&PathBuf::from(".claude/settings.json")]
    );
}

#[cfg(unix)]
#[test]
fn claude_recovery_refuses_foreign_directory_metadata_drift() {
    use std::os::unix::fs::PermissionsExt;

    let cli = IsolatedCli::new();
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::ClaudeCode)
        .unwrap();
    seed_host(case, &cli);
    let claude_dir = cli.home.path().join(".claude");
    fs::set_permissions(&claude_dir, fs::Permissions::from_mode(0o710)).unwrap();
    let killed = cli.run_with_env(
        &["install", "--agent", case.id],
        "TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE",
        "1",
    );
    assert!(!killed.status.success());
    fs::set_permissions(&claude_dir, fs::Permissions::from_mode(0o777)).unwrap();

    let refused = cli.run(&["host-bundle", "recover", "--agent", case.id, "--yes"]);
    assert!(!refused.status.success());
    assert_eq!(
        fs::metadata(&claude_dir).unwrap().permissions().mode() & 0o777,
        0o777,
        "recovery must preserve foreign directory metadata drift"
    );
}

#[test]
fn claude_tracedecay_only_project_mcp_removal_recovers() {
    let cli = IsolatedCli::new();
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::ClaudeCode)
        .unwrap();
    seed_host(case, &cli);
    let mcp_path = cli.project.path().join(".mcp.json");
    let original = br#"{"mcpServers":{"tracedecay":{"command":"old"}}}
"#;
    fs::write(&mcp_path, original).unwrap();

    let mut command = cli.command(&["install", "--agent", case.id]);
    let killed = command
        .env(
            "TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_REMOVE_PATH",
            &mcp_path,
        )
        .output()
        .unwrap();
    assert!(!killed.status.success());
    assert!(
        !mcp_path.exists(),
        "install stopped before tracedecay-only MCP removal\nstderr:\n{}",
        String::from_utf8_lossy(&killed.stderr)
    );

    assert_success(
        case.id,
        "tracedecay-only project MCP recovery",
        cli.run(&["host-bundle", "recover", "--agent", case.id, "--yes"]),
    );
    assert_eq!(fs::read(&mcp_path).unwrap(), original);
}

#[test]
fn claude_old_directory_missing_backup_layout_recovers() {
    let cli = IsolatedCli::new();
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::ClaudeCode)
        .unwrap();
    seed_host(case, &cli);
    let settings_path = cli.home.path().join(".claude/settings.json");
    let killed = cli.run_with_env(
        &["install", "--agent", case.id],
        "TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE_PATH",
        settings_path.to_str().unwrap(),
    );
    assert!(!killed.status.success());

    let backup = pending_registration_backup(&cli, case.id);
    let identity_path = backup.join("identity.v1.json");
    let mut identity: serde_json::Value =
        serde_json::from_slice(&fs::read(&identity_path).unwrap()).unwrap();
    assert_eq!(identity["schema_version"], 2);
    identity["schema_version"] = 1.into();
    fs::write(&identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();

    let plan_path = backup.join("mutation-plan.v1.json");
    let mut plan: serde_json::Value =
        serde_json::from_slice(&fs::read(&plan_path).unwrap()).unwrap();
    assert_eq!(plan["schema_version"], 2);
    plan["schema_version"] = 1.into();
    let directories = plan["directories"].as_array_mut().unwrap();
    let absent_directory = cli.project.path().join(".claude");
    let index = directories
        .iter()
        .position(|path| path == &serde_json::to_value(&absent_directory).unwrap())
        .expect("project Claude directory must be journaled");
    fs::write(&plan_path, serde_json::to_vec(&plan).unwrap()).unwrap();
    assert!(
        absent_directory.is_dir(),
        "current apply must have created the originally absent directory"
    );
    fs::remove_file(backup.join(format!("directory-{index}.applied.metadata.json"))).unwrap();

    assert_success(
        case.id,
        "old directory backup recovery",
        cli.run(&["host-bundle", "recover", "--agent", case.id, "--yes"]),
    );
    assert!(!absent_directory.exists());
}

#[test]
fn claude_base_v1_plan_without_directories_key_recovers() {
    let cli = IsolatedCli::new();
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::ClaudeCode)
        .unwrap();
    seed_host(case, &cli);
    let settings_path = cli.home.path().join(".claude/settings.json");
    let killed = cli.run_with_env(
        &["install", "--agent", case.id],
        "TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE_PATH",
        settings_path.to_str().unwrap(),
    );
    assert!(!killed.status.success());

    let backup = pending_registration_backup(&cli, case.id);
    let identity_path = backup.join("identity.v1.json");
    let mut identity: serde_json::Value =
        serde_json::from_slice(&fs::read(&identity_path).unwrap()).unwrap();
    identity["schema_version"] = 1.into();
    fs::write(&identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();
    let plan_path = backup.join("mutation-plan.v1.json");
    let mut plan: serde_json::Value =
        serde_json::from_slice(&fs::read(&plan_path).unwrap()).unwrap();
    plan["schema_version"] = 1.into();
    plan.as_object_mut().unwrap().remove("directories");
    fs::write(&plan_path, serde_json::to_vec(&plan).unwrap()).unwrap();
    for entry in fs::read_dir(&backup).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("directory-"))
        {
            fs::remove_file(path).unwrap();
        }
    }

    assert_success(
        case.id,
        "base v1 registration recovery",
        cli.run(&["host-bundle", "recover", "--agent", case.id, "--yes"]),
    );
}

#[test]
fn claude_future_registration_backup_version_fails_truthfully() {
    let cli = IsolatedCli::new();
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::ClaudeCode)
        .unwrap();
    seed_host(case, &cli);
    let settings_path = cli.home.path().join(".claude/settings.json");
    let killed = cli.run_with_env(
        &["install", "--agent", case.id],
        "TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE_PATH",
        settings_path.to_str().unwrap(),
    );
    assert!(!killed.status.success());

    let backup = pending_registration_backup(&cli, case.id);
    let identity: serde_json::Value =
        serde_json::from_slice(&fs::read(backup.join("identity.v1.json")).unwrap()).unwrap();
    assert_eq!(identity["schema_version"], 2);
    let plan_path = backup.join("mutation-plan.v1.json");
    let mut plan: serde_json::Value =
        serde_json::from_slice(&fs::read(&plan_path).unwrap()).unwrap();
    assert_eq!(plan["schema_version"], 2);
    plan["schema_version"] = 99.into();
    fs::write(&plan_path, serde_json::to_vec(&plan).unwrap()).unwrap();

    let refused = cli.run(&["host-bundle", "recover", "--agent", case.id, "--yes"]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("host recovery backup format is unsupported"));
    assert!(stderr.contains("use the TraceDecay version that created it"));
}

#[test]
fn claude_future_identity_version_fails_truthfully() {
    let cli = IsolatedCli::new();
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::ClaudeCode)
        .unwrap();
    seed_host(case, &cli);
    let settings_path = cli.home.path().join(".claude/settings.json");
    let killed = cli.run_with_env(
        &["install", "--agent", case.id],
        "TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE_PATH",
        settings_path.to_str().unwrap(),
    );
    assert!(!killed.status.success());

    let identity_path = pending_registration_backup(&cli, case.id).join("identity.v1.json");
    let mut identity: serde_json::Value =
        serde_json::from_slice(&fs::read(&identity_path).unwrap()).unwrap();
    assert_eq!(identity["schema_version"], 2);
    identity["schema_version"] = 99.into();
    fs::write(&identity_path, serde_json::to_vec(&identity).unwrap()).unwrap();

    let refused = cli.run(&["host-bundle", "recover", "--agent", case.id, "--yes"]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("host recovery backup format is unsupported"));
    assert!(stderr.contains("use the TraceDecay version that created it"));
}

#[cfg(unix)]
#[test]
fn claude_install_rejects_empty_symlinked_config_directory() {
    use std::os::unix::fs::symlink;

    let cli = IsolatedCli::new();
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::ClaudeCode)
        .unwrap();
    seed_host(case, &cli);
    let claude_dir = cli.home.path().join(".claude");
    fs::remove_dir_all(&claude_dir).unwrap();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), &claude_dir).unwrap();

    let refused = cli.run(&["install", "--agent", case.id]);
    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(stderr.contains("Claude home configuration path ~/.claude is a symlink"));
    assert!(stderr.contains("replace it with a real directory"));
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[test]
fn claude_global_uninstall_removes_current_project_legacy_registration() {
    let cli = IsolatedCli::new();
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::ClaudeCode)
        .unwrap();
    seed_host(case, &cli);
    assert_success(
        case.id,
        "initial install",
        cli.run(&["install", "--agent", case.id]),
    );
    let mcp_path = cli.project.path().join(".mcp.json");
    let settings_path = cli.project.path().join(".claude/settings.local.json");
    fs::write(
        &mcp_path,
        br#"{"mcpServers":{"tracedecay":{"command":"old"},"foreign":{"command":"keep"}}}"#,
    )
    .unwrap();
    fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    fs::write(
        &settings_path,
        br#"{"enabledMcpjsonServers":["tracedecay","foreign"],"hooks":{"Stop":[{"hooks":[{"type":"command","command":"tracedecay hook"}]}]},"operator":"keep"}"#,
    )
    .unwrap();

    assert_success(
        case.id,
        "global uninstall",
        cli.run(&["uninstall", "--agent", case.id]),
    );
    assert!(
        !fs::read_to_string(&mcp_path)
            .unwrap()
            .contains("tracedecay")
    );
    assert!(
        !fs::read_to_string(&settings_path)
            .unwrap()
            .contains("tracedecay")
    );
    let mcp: serde_json::Value = serde_json::from_slice(&fs::read(&mcp_path).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"]["foreign"]["command"], "keep");
    let settings: serde_json::Value =
        serde_json::from_slice(&fs::read(&settings_path).unwrap()).unwrap();
    assert_eq!(settings["operator"], "keep");
    assert!(
        settings["enabledMcpjsonServers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "foreign")
    );
}

#[test]
fn killed_install_recovery_refuses_later_operator_edit() {
    let cli = IsolatedCli::new();
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::OpenCode)
        .unwrap();
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
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::OpenCode)
        .unwrap();
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
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::OpenCode)
        .unwrap();
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
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::OpenCode)
        .unwrap();
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
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::OpenCode)
        .unwrap();
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
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::OpenCode)
        .unwrap();
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
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::OpenCode)
        .unwrap();
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
    let case = *HOSTS
        .iter()
        .find(|case| case.host == HostKindV1::OpenCode)
        .unwrap();
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
