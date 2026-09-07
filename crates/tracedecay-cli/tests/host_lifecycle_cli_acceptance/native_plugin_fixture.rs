use std::fs;
use std::path::{Path, PathBuf};

pub fn copy_test_bundle(source: &Path, destination: &Path) {
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&destination_path).unwrap();
            copy_test_bundle(&source_path, &destination_path);
        } else {
            fs::create_dir_all(destination_path.parent().unwrap()).unwrap();
            fs::copy(source_path, destination_path).unwrap();
        }
    }
}

pub fn set_claude_native_activation(home: &Path, active: bool) {
    let settings_path = home.join(".claude/settings.json");
    let mut settings: serde_json::Value =
        serde_json::from_slice(&fs::read(&settings_path).unwrap()).unwrap();
    let enabled_plugins = settings
        .get_mut("enabledPlugins")
        .and_then(serde_json::Value::as_object_mut)
        .unwrap();
    if active {
        enabled_plugins.insert("tracedecay@tracedecay".to_string(), true.into());
    } else {
        enabled_plugins.remove("tracedecay@tracedecay");
    }
    fs::write(
        &settings_path,
        serde_json::to_vec_pretty(&settings).unwrap(),
    )
    .unwrap();

    let marketplaces_path = home.join(".claude/plugins/known_marketplaces.json");
    let mut marketplaces: serde_json::Value =
        serde_json::from_slice(&fs::read(&marketplaces_path).unwrap()).unwrap();
    let marketplaces = marketplaces.as_object_mut().unwrap();
    if active {
        let deploy_dir = home.join(".claude/plugins/marketplaces/tracedecay");
        marketplaces.insert(
            "tracedecay".to_string(),
            serde_json::json!({
                "source": { "source": "directory", "path": deploy_dir },
                "installLocation": deploy_dir
            }),
        );
    } else {
        marketplaces.remove("tracedecay");
    }
    fs::write(
        &marketplaces_path,
        serde_json::to_vec_pretty(&marketplaces).unwrap(),
    )
    .unwrap();

    let cache_root = home
        .join(".claude/plugins/cache/tracedecay/tracedecay")
        .join(tracedecay_agent_hosts::PRODUCT_VERSION);
    if active {
        fs::create_dir_all(&cache_root).unwrap();
        copy_test_bundle(
            &home.join(".claude/plugins/marketplaces/tracedecay"),
            &cache_root,
        );
    } else if cache_root.exists() {
        fs::remove_dir_all(cache_root).unwrap();
    }
}

pub fn apply_current_codex_plugin_remediation(home: &Path, command: &str) -> Result<(), String> {
    let ["codex", "plugin", "add", "tracedecay@personal"] =
        command.split_whitespace().collect::<Vec<_>>().as_slice()
    else {
        return Err(format!(
            "Codex 0.147.0 cannot execute lifecycle remediation `{command}`"
        ));
    };

    let config_path = home.join(".codex/config.toml");
    let mut config = fs::read_to_string(&config_path).unwrap_or_default();
    if !config.contains("[plugins.\"tracedecay@personal\"]") {
        config.push_str("\n[plugins.\"tracedecay@personal\"]\nenabled = true\n");
        fs::write(&config_path, config).unwrap();
    }

    let source = home.join(".codex/plugins/tracedecay");
    let cache = home
        .join(".codex/plugins/cache/personal/tracedecay")
        .join(tracedecay_agent_hosts::PRODUCT_VERSION);
    fs::create_dir_all(&cache).unwrap();
    copy_test_bundle(&source, &cache);
    Ok(())
}

pub fn remediation_command(stderr: &[u8]) -> &str {
    std::str::from_utf8(stderr)
        .unwrap()
        .split('`')
        .nth(1)
        .expect("lifecycle failure must provide one executable remediation command")
}

#[cfg(unix)]
pub fn install_current_claude_cli(home: &Path, bin_dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let cli = bin_dir.join("claude");
    fs::write(
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

if args == ["plugin", "uninstall", "tracedecay"]:
    settings_path = home / ".claude/settings.json"
    settings = json.loads(settings_path.read_text())
    settings["enabledPlugins"].pop("tracedecay@tracedecay", None)
    settings_path.write_text(json.dumps(settings, indent=2))
    shutil.rmtree(home / ".claude/plugins/cache/tracedecay", ignore_errors=True)
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
    let mut permissions = fs::metadata(&cli).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&cli, permissions).unwrap();
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
