use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use tracedecay_runtime_core::test_executable::write_executable_script;

/// A stock-grammar `claude` that performs the plugin lifecycle the component
/// transaction drives: marketplace registration, cache install, removal.
#[cfg(unix)]
pub fn install_current_claude_cli(home: &Path, bin_dir: &Path) -> PathBuf {
    let cli = bin_dir.join("claude");
    write_executable_script(
        &cli,
        r##"#!/usr/bin/env python3
import json
import os
import pathlib
import shutil
import sys

home = pathlib.Path(os.environ["HOME"])
args = sys.argv[1:]
with (home / ".claude-test-invocations").open("a") as log:
    log.write(" ".join(args) + "\n")

deploy_dir = home / ".claude/plugins/marketplaces/tracedecay"
if args == ["plugin", "marketplace", "add", str(deploy_dir)]:
    marketplace_path = home / ".claude/plugins/known_marketplaces.json"
    marketplaces = json.loads(marketplace_path.read_text()) if marketplace_path.exists() else {}
    marketplaces["tracedecay"] = {
        "source": {"source": "directory", "path": str(deploy_dir)},
        "installLocation": str(deploy_dir),
    }
    marketplace_path.write_text(json.dumps(marketplaces, indent=2))
elif args == ["plugin", "install", "tracedecay@tracedecay"]:
    installed_path = home / ".claude/plugins/installed_plugins.json"
    installed = (
        json.loads(installed_path.read_text())
        if installed_path.exists()
        else {"version": 2, "plugins": {}}
    )
    plugins = installed.get("plugins")
    entry = plugins.get("tracedecay@tracedecay") if isinstance(plugins, dict) else None
    if isinstance(entry, list):
        already_installed = len(entry) > 0
    elif isinstance(entry, dict):
        already_installed = len(entry) > 0
    else:
        already_installed = entry is not None
    if already_installed:
        # Stock `plugin install` of a plugin Claude already records exits 0
        # and leaves that version's cache untouched.
        sys.exit(0)
    manifest = json.loads((deploy_dir / ".claude-plugin/plugin.json").read_text())
    cache = home / ".claude/plugins/cache/tracedecay/tracedecay" / manifest["version"]
    shutil.rmtree(cache, ignore_errors=True)
    shutil.copytree(deploy_dir, cache)
    settings_path = home / ".claude/settings.json"
    settings = json.loads(settings_path.read_text()) if settings_path.exists() else {}
    settings.setdefault("enabledPlugins", {})["tracedecay@tracedecay"] = True
    settings_path.write_text(json.dumps(settings, indent=2))
    if not isinstance(plugins, dict):
        plugins = {}
        installed["plugins"] = plugins
    installed["version"] = 2
    plugins["tracedecay@tracedecay"] = [
        {
            "scope": "user",
            "installPath": str(cache),
            "version": manifest["version"],
        }
    ]
    installed_path.parent.mkdir(parents=True, exist_ok=True)
    installed_path.write_text(json.dumps(installed, indent=2))
elif args == ["plugin", "uninstall", "tracedecay"]:
    settings_path = home / ".claude/settings.json"
    if settings_path.exists():
        settings = json.loads(settings_path.read_text())
        enabled = settings.get("enabledPlugins")
        if isinstance(enabled, dict):
            enabled.pop("tracedecay@tracedecay", None)
            settings_path.write_text(json.dumps(settings, indent=2))
    shutil.rmtree(home / ".claude/plugins/cache/tracedecay", ignore_errors=True)
    installed_path = home / ".claude/plugins/installed_plugins.json"
    if installed_path.exists():
        installed = json.loads(installed_path.read_text())
        plugins = installed.get("plugins")
        if isinstance(plugins, dict):
            plugins.pop("tracedecay@tracedecay", None)
            installed_path.write_text(json.dumps(installed, indent=2))
elif args == ["plugin", "marketplace", "remove", "tracedecay"]:
    marketplace_path = home / ".claude/plugins/known_marketplaces.json"
    marketplaces = json.loads(marketplace_path.read_text())
    marketplaces.pop("tracedecay", None)
    marketplace_path.write_text(json.dumps(marketplaces, indent=2))
else:
    print("unsupported fake Claude lifecycle command: " + " ".join(args), file=sys.stderr)
    sys.exit(2)
"##,
    )
    .unwrap();
    home.join(".claude-test-invocations")
}

#[cfg(unix)]
pub fn recorded_claude_invocations(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// A stock-grammar `codex` whose `plugin add` enables the plugin and copies
/// the catalog-deployed source into its versioned cache, and whose `plugin
/// remove` drops both again while leaving the marketplace cache directory, as
/// Codex 0.156 does.
#[cfg(unix)]
pub fn install_current_codex_cli(bin_dir: &Path) {
    let cli = bin_dir.join("codex");
    write_executable_script(
        &cli,
        r##"#!/usr/bin/env python3
import json
import os
import pathlib
import shutil
import sys

home = pathlib.Path(os.environ["HOME"])
args = sys.argv[1:]
if args[:2] == ["plugin", "add"] and len(args) >= 3 and args[2].startswith("tracedecay@") and args[3:] in ([], ["--json"]):
    marketplace = args[2].split("@", 1)[1]
    source = home / ".codex/plugins/tracedecay"
    manifest = json.loads((source / ".codex-plugin/plugin.json").read_text())
    cache = home / ".codex/plugins/cache" / marketplace / "tracedecay" / manifest["version"]
    shutil.rmtree(cache, ignore_errors=True)
    shutil.copytree(source, cache)
    config_path = home / ".codex/config.toml"
    config = config_path.read_text() if config_path.exists() else ""
    header = '[plugins."' + args[2] + '"]'
    if header not in config:
        config_path.write_text(config + "\n" + header + "\nenabled = true\n")
    print(json.dumps({"pluginId": args[2], "enabled": True}))
elif args[:2] == ["plugin", "remove"] and len(args) >= 3 and args[2].startswith("tracedecay@") and args[3:] in ([], ["--json"]):
    marketplace = args[2].split("@", 1)[1]
    shutil.rmtree(home / ".codex/plugins/cache" / marketplace / "tracedecay", ignore_errors=True)
    config_path = home / ".codex/config.toml"
    block = '\n[plugins."' + args[2] + '"]\nenabled = true\n'
    config_path.write_text(config_path.read_text().replace(block, ""))
    print(json.dumps({"pluginId": args[2], "enabled": False}))
else:
    print("unsupported fake Codex lifecycle command: " + " ".join(args), file=sys.stderr)
    sys.exit(2)
"##,
    )
    .unwrap();
}

/// A stock-grammar `droid` whose `mcp add` / `mcp remove` edit its own
/// `~/.factory/mcp.json` registry, as Factory Droid does.
#[cfg(unix)]
pub fn install_current_droid_cli(bin_dir: &Path) {
    let cli = bin_dir.join("droid");
    write_executable_script(
        &cli,
        r##"#!/usr/bin/env python3
import json
import os
import pathlib
import sys

home = pathlib.Path(os.environ["HOME"])
path = home / ".factory/mcp.json"
args = sys.argv[1:]
registry = json.loads(path.read_text()) if path.exists() else {"mcpServers": {}}
if args[:2] == ["mcp", "add"] and len(args) == 6 and args[4:] == ["--type", "stdio"]:
    command = args[3].split(" ")
    registry.setdefault("mcpServers", {})[args[2]] = {
        "type": "stdio",
        "command": command[0],
        "args": command[1:],
    }
elif args[:2] == ["mcp", "remove"] and len(args) == 3:
    registry.get("mcpServers", {}).pop(args[2], None)
else:
    print("unsupported fake Droid lifecycle command: " + " ".join(args), file=sys.stderr)
    sys.exit(2)
path.parent.mkdir(parents=True, exist_ok=True)
path.write_text(json.dumps(registry, indent=2) + "\n")
"##,
    )
    .unwrap();
}
