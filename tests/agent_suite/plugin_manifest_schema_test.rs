//! Validates the source plugin bundle manifests against Cursor's official
//! published JSON schema.
//!
//! The schema is vendored at `tests/fixtures/cursor-schemas/` from the
//! cursor/plugins repository (commit 4a91a6e, "Add plugin validation
//! workflow") so validation runs offline in `cargo test`:
//! <https://github.com/cursor/plugins/commit/4a91a6e2665f559f61877f03e36b54886eef359e>
//!
//! The Codex bundle manifest follows the same shape plus a Codex-specific
//! `interface` marketplace block, so it is checked against the Cursor schema
//! extended with that one key. Rendered (installed) manifests are covered by
//! the receipt-backed host lifecycle acceptance suite.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use serde_json::{Value, json};

use crate::plugin_validation_support::{
    assert_schema_valid, compile_schema, read_json_file, repo_path,
};

const PLUGIN_SCHEMA: &str = include_str!("../fixtures/cursor-schemas/plugin.schema.json");

/// Component paths declared in a manifest, with the manifest fields that
/// declared them. Only string and string-array fields are path references;
/// inline objects (`hooks`, `mcpServers`) carry their config in place.
fn declared_component_paths(manifest: &Value) -> Vec<(String, String)> {
    let mut paths = Vec::new();
    for field in [
        "rules",
        "agents",
        "skills",
        "commands",
        "hooks",
        "mcpServers",
    ] {
        match manifest.get(field) {
            None => {}
            Some(Value::String(path)) => paths.push((field.to_string(), path.clone())),
            Some(Value::Array(items)) => {
                for item in items {
                    if let Value::String(path) = item {
                        paths.push((field.to_string(), path.clone()));
                    }
                }
            }
            Some(_) => {} // inline hooks / mcpServers objects
        }
    }
    paths
}

fn assert_component_paths_resolve(manifest: &Value, bundle_root: &Path, manifest_path: &Path) {
    for (field, declared) in declared_component_paths(manifest) {
        assert!(
            !declared.starts_with('/') && !declared.split('/').any(|part| part == ".."),
            "{} field `{field}` declares `{declared}`; the marketplace submission \
             checklist requires relative paths without `..`",
            manifest_path.display()
        );
        let resolved = bundle_root.join(declared.trim_start_matches("./"));
        assert!(
            resolved.exists(),
            "{} field `{field}` declares `{declared}` but {} does not exist",
            manifest_path.display(),
            resolved.display()
        );
    }
}

/// The manifests now live in the single shared `plugin/` tree, but their
/// component pointers are *deploy-relative* (e.g. `mcp.json`, `hooks/hooks.json`
/// — sourced from `mcp-cursor.json` / `hooks/hooks-cursor.json`). Stage the
/// per-host deploy layout into a temp dir so `assert_component_paths_resolve`
/// checks against the tree each host actually installs.
fn stage_host_deploy(copies: &[(&str, &str)]) -> tempfile::TempDir {
    let src = repo_path("plugin");
    let staged = tempfile::tempdir().expect("temp dir");
    for (source, deploy) in copies {
        let target = staged.path().join(deploy);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        let src_path = src.join(source);
        if src_path.is_dir() {
            copy_dir(&src_path, &target);
        } else {
            std::fs::copy(&src_path, &target).unwrap();
        }
    }
    staged
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

#[test]
fn cursor_bundle_manifest_matches_the_official_cursor_plugin_schema() {
    let schema: Value = serde_json::from_str(PLUGIN_SCHEMA).expect("schema fixture parses");
    let validator = compile_schema(&schema);

    let manifest_path = repo_path("plugin/.cursor-plugin/plugin.json");
    let manifest = read_json_file(&manifest_path);
    assert_schema_valid(&validator, &manifest, &manifest_path);

    let staged = stage_host_deploy(&[
        (".cursor-plugin/plugin.json", ".cursor-plugin/plugin.json"),
        ("mcp-cursor.json", "mcp.json"),
        ("hooks/hooks-cursor.json", "hooks/hooks.json"),
        ("rules", "rules"),
        ("overlays/cursor/commands", "commands"),
        ("skills", "skills"),
        ("agents", "agents"),
    ]);
    assert_component_paths_resolve(&manifest, staged.path(), &manifest_path);
}

#[test]
fn codex_bundle_manifest_matches_the_cursor_schema_plus_interface_extension() {
    let mut schema: Value = serde_json::from_str(PLUGIN_SCHEMA).expect("schema fixture parses");
    // Codex marketplaces read an `interface` block (display metadata) that
    // Cursor's schema does not define; with `additionalProperties: false`
    // the stock schema would reject it, so allow exactly that one extra key.
    schema["properties"]["interface"] = json!({ "type": "object" });
    let validator = compile_schema(&schema);

    let manifest_path = repo_path("plugin/.codex-plugin/plugin.json");
    let manifest = read_json_file(&manifest_path);
    assert_schema_valid(&validator, &manifest, &manifest_path);

    let staged = stage_host_deploy(&[
        (".codex-plugin/plugin.json", ".codex-plugin/plugin.json"),
        (".mcp.json", ".mcp.json"),
        ("hooks/hooks-codex.json", "hooks/hooks.json"),
        ("skills", "skills"),
    ]);
    assert_component_paths_resolve(&manifest, staged.path(), &manifest_path);
}

/// The schema's `name` pattern is what the marketplace submission checklist
/// enforces; both host manifests must agree on the plugin name so cross-host
/// tooling (marketplace entries, cache paths) can key on one identifier.
#[test]
fn bundle_manifests_share_the_plugin_name() {
    let cursor = read_json_file(&repo_path("plugin/.cursor-plugin/plugin.json"));
    let codex = read_json_file(&repo_path("plugin/.codex-plugin/plugin.json"));
    let claude = read_json_file(&repo_path("plugin/.claude-plugin/plugin.json"));
    assert_eq!(cursor["name"], "tracedecay");
    assert_eq!(codex["name"], "tracedecay");
    assert_eq!(claude["name"], "tracedecay");
}
