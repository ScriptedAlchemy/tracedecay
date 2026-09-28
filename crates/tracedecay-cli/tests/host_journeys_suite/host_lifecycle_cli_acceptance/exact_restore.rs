//! Uninstall inverts exactly what install changed, on every receipt-backed
//! host: the isolated home (outside TraceDecay's own profile and staging)
//! ends with the same paths, file bytes, modes, and symlinks it started with.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use sha2::{Digest, Sha256};
use tracedecay_agent_hosts::agents::host_bundle::HostKindV1;
use tracedecay_agent_hosts::agents::host_bundle_registry::RECEIPT_BACKED_HOST_KINDS;
use tracedecay_agent_hosts::agents::integration_id_for_host;
use tracedecay_runtime_core::test_executable::write_executable_script;

use super::IsolatedCli;

const PENDING_OPERATOR_ACTION_EXIT: i32 = 75;

/// Stock-grammar stand-in for every host CLI whose own registry TraceDecay
/// drives. Like the real hosts, a removal that leaves a file semantically as
/// it was before the matching add restores that file's original bytes, and a
/// file the add created is deleted again, so any residue is TraceDecay's.
const FAKE_NATIVE_HOST: &str = r##"#!/usr/bin/env python3
import json
import os
import pathlib
import shutil
import sys

name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
home = pathlib.Path(os.environ["HOME"])
state = pathlib.Path(__file__).resolve().parent / ".fake-host-state"


def fail():
    print(f"unsupported fake {name} command: {' '.join(args)}", file=sys.stderr)
    sys.exit(2)


def mark(path):
    return state / str(path.relative_to(home)).replace("/", "__")


def load(path):
    text = path.read_text() if path.exists() else ""
    return json.loads(text) if text.strip() else {}


def edit(path, mutate):
    saved = mark(path)
    if not saved.exists():
        state.mkdir(parents=True, exist_ok=True)
        saved.write_bytes(b"A" + path.read_bytes() if path.exists() else b"N")
    value = load(path)
    mutate(value)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2) + "\n")


def unedit(path, mutate):
    if not path.exists():
        return
    value = load(path)
    mutate(value)
    saved = mark(path)
    original = saved.read_bytes() if saved.exists() else None
    if original == b"N" and all(not v for v in value.values()):
        path.unlink()
    elif original and original[:1] == b"A" and json.loads(original[1:] or b"{}") == value:
        path.write_bytes(original[1:])
    else:
        path.write_text(json.dumps(value, indent=2) + "\n")
    saved.unlink(missing_ok=True)


def set_server(server, command, server_args):
    return lambda value: value.setdefault("mcpServers", {}).__setitem__(
        server, {"command": command, "args": server_args}
    )


def pop_server(server):
    return lambda value: value.get("mcpServers", {}).pop(server, None)


if args[:1] in (["--version"], ["-v"], ["version"]):
    print(f"{name} 99.0.0")
elif name == "claude":
    deploy = home / ".claude/plugins/marketplaces/tracedecay"
    marketplaces = home / ".claude/plugins/known_marketplaces.json"
    installed = home / ".claude/plugins/installed_plugins.json"
    settings = home / ".claude/settings.json"
    if args == ["plugin", "marketplace", "add", str(deploy)]:
        edit(marketplaces, lambda v: v.__setitem__("tracedecay", {
            "source": {"source": "directory", "path": str(deploy)},
            "installLocation": str(deploy),
        }))
    elif args == ["plugin", "install", "tracedecay@tracedecay"]:
        if load(installed).get("plugins", {}).get("tracedecay@tracedecay"):
            sys.exit(0)
        version = json.loads((deploy / ".claude-plugin/plugin.json").read_text())["version"]
        cache = home / ".claude/plugins/cache/tracedecay/tracedecay" / version
        shutil.rmtree(cache, ignore_errors=True)
        state.mkdir(parents=True, exist_ok=True)
        if not (state / "claude-cache-root").exists():
            (state / "claude-cache-root").write_text(
                "existed" if (home / ".claude/plugins/cache").exists() else "created"
            )
        shutil.copytree(deploy, cache)
        edit(settings, lambda v: v.setdefault("enabledPlugins", {}).__setitem__("tracedecay@tracedecay", True))
        edit(installed, lambda v: (v.__setitem__("version", 2), v.setdefault("plugins", {}).__setitem__(
            "tracedecay@tracedecay", [{"scope": "user", "installPath": str(cache), "version": version}])))
    elif args == ["plugin", "uninstall", "tracedecay"]:
        def drop_enabled(value):
            value.get("enabledPlugins", {}).pop("tracedecay@tracedecay", None)
            if value.get("enabledPlugins") == {}:
                value.pop("enabledPlugins")
        unedit(settings, drop_enabled)
        shutil.rmtree(home / ".claude/plugins/cache/tracedecay", ignore_errors=True)
        cache_root = state / "claude-cache-root"
        if cache_root.exists() and cache_root.read_text() == "created":
            shutil.rmtree(home / ".claude/plugins/cache", ignore_errors=True)
            cache_root.unlink()
        def drop_installed(value):
            value.get("plugins", {}).pop("tracedecay@tracedecay", None)
            if value.get("plugins") == {}:
                value.pop("plugins")
                value.pop("version", None)
        unedit(installed, drop_installed)
    elif args == ["plugin", "marketplace", "remove", "tracedecay"]:
        unedit(marketplaces, lambda v: v.pop("tracedecay", None))
    else:
        fail()
elif name == "codex":
    config = home / ".codex/config.toml"
    if args[:2] in (["plugin", "add"], ["plugin", "remove"]) and len(args) >= 3 and args[2].startswith("tracedecay@"):
        market = args[2].split("@", 1)[1]
        block = f'\n[plugins."{args[2]}"]\nenabled = true\n'
        text = config.read_text() if config.exists() else ""
        if args[1] == "add":
            source = home / ".codex/plugins/tracedecay"
            version = json.loads((source / ".codex-plugin/plugin.json").read_text())["version"]
            cache = home / ".codex/plugins/cache" / market / "tracedecay" / version
            shutil.rmtree(cache, ignore_errors=True)
            shutil.copytree(source, cache)
            if block not in text:
                config.write_text(text + block)
            print(json.dumps({"pluginId": args[2], "enabled": True}))
        else:
            shutil.rmtree(home / ".codex/plugins/cache" / market / "tracedecay", ignore_errors=True)
            config.write_text(text.replace(block, ""))
            print(json.dumps({"pluginId": args[2], "enabled": False}))
    else:
        fail()
elif name == "gemini":
    extension = home / ".gemini/extensions/tracedecay"
    if args[:2] == ["extensions", "install"] and len(args) == 3:
        shutil.rmtree(extension, ignore_errors=True)
        shutil.copytree(args[2], extension)
    elif args == ["extensions", "uninstall", "tracedecay"]:
        shutil.rmtree(extension, ignore_errors=True)
        extensions = home / ".gemini/extensions"
        if extensions.exists() and not any(extensions.iterdir()):
            extensions.rmdir()
    elif args == ["extensions", "list"]:
        if extension.exists():
            print("tracedecay")
    else:
        fail()
elif name == "copilot":
    registry = home / ".copilot/mcp-config.json"
    if args[:3] == ["mcp", "add", "tracedecay"] and "--" in args:
        rest = args[args.index("--") + 1:]
        edit(registry, set_server("tracedecay", rest[0], rest[1:]))
    elif args == ["mcp", "remove", "tracedecay"]:
        unedit(registry, pop_server("tracedecay"))
    else:
        fail()
elif name == "kiro-cli":
    registry = home / ".kiro/settings/mcp.json"
    if args[:2] == ["mcp", "add"] and "--name" in args:
        command = args[args.index("--command") + 1]
        server_args = [args[i + 1] for i, arg in enumerate(args) if arg == "--args"]
        edit(registry, set_server(args[args.index("--name") + 1], command, server_args))
    elif args[:2] == ["mcp", "remove"] and "--name" in args:
        unedit(registry, pop_server(args[args.index("--name") + 1]))
    elif args[:2] == ["mcp", "list"]:
        print(json.dumps(load(registry).get("mcpServers", {})))
    else:
        fail()
elif name == "droid":
    registry = home / ".factory/mcp.json"
    if args[:3] == ["mcp", "add", "tracedecay"] and args[4:] == ["--type", "stdio"]:
        words = args[3].split()
        edit(registry, lambda v: v.setdefault("mcpServers", {}).__setitem__(
            "tracedecay", {"type": "stdio", "command": words[0], "args": words[1:]}))
    elif args == ["mcp", "remove", "tracedecay"]:
        unedit(registry, pop_server("tracedecay"))
    else:
        fail()
else:
    fail()
"##;

/// Operator content in every host's config surface, including the two shapes
/// a heuristic uninstall gets wrong: a pre-existing registry that already
/// held an empty server map, and a hand-formatted settings file.
const OPERATOR_HOME: &[(&str, &[u8], u32)] = &[
    (
        ".claude/settings.json",
        b"{\n  \"theme\": \"dark\",\n  \"permissions\": { \"allow\": [\"Bash(ls:*)\"] }\n}\n",
        0o644,
    ),
    (
        ".claude.json",
        br#"{"numStartups":3,"mcpServers":{"other":{"command":"other-mcp","args":[]}}}"#,
        0o600,
    ),
    (
        ".codex/config.toml",
        b"model = \"gpt-5\"\n\n[mcp_servers.other]\ncommand = \"other-mcp\"\n",
        0o644,
    ),
    (
        ".cursor/mcp.json",
        b"{\n  \"mcpServers\": {\n    \"other\": { \"command\": \"other-mcp\" }\n  }\n}\n",
        0o644,
    ),
    (
        ".config/opencode/opencode.json",
        b"{\"$schema\":\"https://opencode.ai/config.json\",\"theme\":\"system\"}\n",
        0o644,
    ),
    (".gemini/settings.json", b"{\"theme\":\"Default\"}\n", 0o644),
    (
        ".gemini/antigravity/mcp_config.json",
        b"{\"mcpServers\":{}}\n",
        0o644,
    ),
    (
        ".cline/data/settings.json",
        b"{\"telemetry\":false}\n",
        0o644,
    ),
    (".copilot/config.json", b"{\"theme\":\"dark\"}\n", 0o644),
    (
        ".config/Code/User/settings.json",
        b"{\n  // user comment kept verbatim\n  \"editor.fontSize\": 13,\n}\n",
        0o644,
    ),
    (
        ".config/Code/User/globalStorage/rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json",
        b"{\"mcpServers\":{\"other\":{\"command\":\"other-mcp\"}}}\n",
        0o644,
    ),
    (".config/devin/config.json", b"{}\n", 0o600),
    (".factory/settings.json", b"{\"model\":\"x\"}\n", 0o644),
    (".hermes/config.yaml", b"model: x\n", 0o644),
    (
        ".config/kilo/kilo.jsonc",
        b"// user kilo config\n{\"theme\":\"dark\"}\n",
        0o644,
    ),
    (".kimi-code/config.toml", b"default_model = \"k2\"\n", 0o644),
    (".kiro/settings/mcp.json", b"{\"mcpServers\":{}}\n", 0o644),
    (
        ".pi/agent/settings.json",
        b"{\"defaultModel\":\"x\"}\n",
        0o644,
    ),
    (".vibe/config.toml", b"active_model = \"devstral\"\n", 0o644),
    (
        ".config/zed/settings.json",
        b"// zed user settings\n{\"theme\":\"One Dark\"}\n",
        0o644,
    ),
    (
        ".gitconfig",
        b"[user]\n\tname = Sweep Operator\n\temail = sweep@example.invalid\n",
        0o644,
    ),
];

pub(super) fn seed_operator_home(home: &Path) {
    for (relative, bytes, mode) in OPERATOR_HOME {
        let path = home.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(*mode)).unwrap();
    }
}

pub(super) fn install_fake_native_hosts(bin_dir: &Path) {
    for name in [
        "claude", "codex", "gemini", "copilot", "kiro-cli", "droid", "kimi",
    ] {
        write_executable_script(&bin_dir.join(name), FAKE_NATIVE_HOST).unwrap();
    }
}

/// Every path under the isolated home with its type, permission bits, and
/// content digest or link target, except TraceDecay's own profile, staging
/// root, and the fake host binaries.
pub(super) fn home_snapshot(cli: &IsolatedCli) -> BTreeMap<String, String> {
    let home = cli.home.path();
    let skipped = [
        cli.profile.clone(),
        cli.bin_dir.clone(),
        home.join(".tracedecay"),
    ];
    let mut snapshot = BTreeMap::new();
    let mut pending = vec![home.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if skipped.contains(&path) {
                continue;
            }
            let relative = path
                .strip_prefix(home)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let mode = metadata.permissions().mode() & 0o7777;
            let entry = if metadata.file_type().is_symlink() {
                format!("symlink -> {}", fs::read_link(&path).unwrap().display())
            } else if metadata.is_dir() {
                pending.push(path);
                format!("dir {mode:o}")
            } else {
                format!(
                    "file {mode:o} {}",
                    hex::encode(Sha256::digest(fs::read(&path).unwrap()))
                )
            };
            snapshot.insert(relative, entry);
        }
    }
    snapshot
}

pub(super) fn snapshot_diff(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut diff = Vec::new();
    for (path, entry) in before {
        match after.get(path) {
            None => diff.push(format!("  - {path}: {entry}")),
            Some(other) if other != entry => {
                diff.push(format!("  ~ {path}: {entry} => {other}"));
            }
            Some(_) => {}
        }
    }
    for (path, entry) in after {
        if !before.contains_key(path) {
            diff.push(format!("  + {path}: {entry}"));
        }
    }
    diff
}

fn run_phase(cli: &IsolatedCli, id: &str, args: &[&str], pending_allowed: bool) {
    let output = cli.run(args);
    let code = output.status.code();
    let accepted =
        code == Some(0) || (pending_allowed && code == Some(PENDING_OPERATOR_ACTION_EXIT));
    assert!(
        accepted,
        "{id} `{}` exited {code:?}\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// What each install must visibly have done to the surfaces a heuristic
/// uninstall used to get wrong, so an unchanged tree after uninstall proves an
/// exact inverse rather than an install that never happened.
fn assert_install_changed_surface(host: HostKindV1, home: &Path) {
    let installed = |relative: &str| fs::read_to_string(home.join(relative)).unwrap();
    match host {
        HostKindV1::Devin => assert!(
            installed(".config/devin/mcp_config.json").contains("\"tracedecay\""),
            "devin install did not create its MCP registry"
        ),
        HostKindV1::Cline => assert!(installed(".cline/mcp.json").contains("\"tracedecay\"")),
        HostKindV1::Kilo => assert!(installed(".config/kilo/kilo.jsonc").contains("\"mcp\"")),
        HostKindV1::Hermes => assert!(installed(".hermes/config.yaml").contains("- tracedecay")),
        HostKindV1::Antigravity => {
            assert!(installed(".gemini/antigravity/mcp_config.json").contains("\"tracedecay\""))
        }
        HostKindV1::ClaudeCode => {
            assert!(installed(".claude/settings.json").contains("mcp__plugin_tracedecay_graph__*"))
        }
        _ => {}
    }
}

/// Rewrite every receipt as a release that predates creation records wrote
/// it: the same records without `created_directories` or `created_config`.
fn strip_creation_records(cli: &IsolatedCli) {
    fn strip(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(members) => {
                members.remove("created_directories");
                members.remove("created_config");
                members.values_mut().for_each(strip);
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(strip),
            _ => {}
        }
    }
    let control = cli.lifecycle_root().join(".tracedecay-host-bundle-v1");
    for entry in fs::read_dir(control).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let mut receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        strip(&mut receipt);
        fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    }
}

const LEGACY_CURSOR_EXTENSION: &str =
    ".cursor/extensions/tracedecay.cursor-native-1.0.0-beta.52/dist/extension.js";

/// The native extension beta releases deployed as a receipt-owned Cursor
/// artifact, which the current catalog no longer ships.
fn deposit_legacy_cursor_extension(cli: &IsolatedCli) {
    let bytes = b"legacy cursor-native extension\n";
    let path = cli.home.path().join(LEGACY_CURSOR_EXTENSION);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    let control = cli.lifecycle_root().join(".tracedecay-host-bundle-v1");
    let receipt_path = fs::read_dir(control)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            let name = path.file_name().unwrap().to_string_lossy();
            name.starts_with("receipt.")
                && fs::read_to_string(path)
                    .unwrap()
                    .contains("\"relative_path\":\".cursor/plugins/local/tracedecay/")
        })
        .expect("cursor component receipt");
    let mut receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    let artifacts = receipt["artifacts"].as_array_mut().unwrap();
    let marker = artifacts[0]["ownership_marker"].clone();
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    artifacts.push(serde_json::json!({
        "relative_path": LEGACY_CURSOR_EXTENSION,
        "artifact_digest": digest,
        "ownership_marker": marker,
    }));
    fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
}

/// An install an earlier release made, refreshed and then removed by this
/// build: every directory in TraceDecay's own namespace goes, including the
/// retired extension the refresh drops, while structure nothing proves the
/// old install created stays exactly as the uninstall left it.
#[test]
fn uninstall_after_an_upgrade_removes_tracedecay_residue_and_keeps_unprovable_structure() {
    let cli = IsolatedCli::new();
    seed_operator_home(cli.home.path());
    let foreign_extension = cli
        .home
        .path()
        .join(".cursor/extensions/foreign.ext-1.0.0/package.json");
    fs::create_dir_all(foreign_extension.parent().unwrap()).unwrap();
    fs::write(&foreign_extension, b"{\"name\":\"ext\"}\n").unwrap();
    install_fake_native_hosts(&cli.bin_dir);
    let before = home_snapshot(&cli);

    for id in ["cline", "cursor"] {
        run_phase(&cli, id, &["install", "--agent", id], false);
    }
    strip_creation_records(&cli);
    deposit_legacy_cursor_extension(&cli);
    let upgraded = home_snapshot(&cli);
    for residue in [
        ".cline/tracedecay",
        ".cursor/plugins/local/tracedecay",
        LEGACY_CURSOR_EXTENSION,
    ] {
        assert!(upgraded.contains_key(residue), "old layout lacks {residue}");
    }

    run_phase(&cli, "cline+cursor", &["update-plugin"], false);
    assert!(
        !cli.home
            .path()
            .join(".cursor/extensions/tracedecay.cursor-native-1.0.0-beta.52")
            .exists()
    );
    for id in ["cline", "cursor"] {
        run_phase(&cli, id, &["uninstall", "--agent", id], false);
    }

    let skeleton = b"{\n  \"mcpServers\": {}\n}\n";
    assert_eq!(
        fs::read(cli.home.path().join(".cline/mcp.json")).unwrap(),
        skeleton
    );
    assert_eq!(
        snapshot_diff(&before, &home_snapshot(&cli)),
        [
            format!(
                "  + .cline/mcp.json: file 600 {}",
                hex::encode(Sha256::digest(skeleton))
            ),
            "  + .cursor/plugins: dir 755".to_string(),
            "  + .cursor/plugins/local: dir 755".to_string(),
        ]
    );
}

#[test]
fn uninstall_restores_the_exact_pre_install_home_on_every_host() {
    let mut residue = Vec::new();
    for host in RECEIPT_BACKED_HOST_KINDS {
        let id = integration_id_for_host(host);
        let pending_allowed = host == HostKindV1::KimiCode;
        let cli = IsolatedCli::new();
        seed_operator_home(cli.home.path());
        install_fake_native_hosts(&cli.bin_dir);
        let before = home_snapshot(&cli);

        run_phase(&cli, id, &["install", "--agent", id], pending_allowed);
        assert_install_changed_surface(host, cli.home.path());
        // Kimi defers activation to the operator's own `/plugins install`,
        // so its install stages under `.tracedecay` and nothing else yet.
        if host != HostKindV1::KimiCode {
            assert_ne!(home_snapshot(&cli), before, "{id} install changed nothing");
        }
        run_phase(&cli, id, &["update-plugin"], pending_allowed);
        let _ = cli.run(&["doctor"]);
        run_phase(&cli, id, &["uninstall", "--agent", id], false);

        let diff = snapshot_diff(&before, &home_snapshot(&cli));
        if !diff.is_empty() {
            residue.push(format!("{id}:\n{}", diff.join("\n")));
        }
    }
    assert!(
        residue.is_empty(),
        "uninstall left the home different from before install:\n{}",
        residue.join("\n")
    );
}
