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
//! `tests/agent_suite/update_plugin_test.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use jsonschema::Validator;
use serde_json::{json, Value};

const PLUGIN_SCHEMA: &str = include_str!("fixtures/cursor-schemas/plugin.schema.json");

fn repo_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read_json(path: &Path) -> Value {
    let body = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
    serde_json::from_str(&body)
        .unwrap_or_else(|err| panic!("failed to parse JSON {}: {err}", path.display()))
}

fn compile(schema: &Value) -> Validator {
    jsonschema::options()
        .should_validate_formats(true)
        .build(schema)
        .expect("vendored Cursor plugin schema should compile")
}

fn assert_valid(validator: &Validator, manifest: &Value, manifest_path: &Path) {
    let errors = validator
        .iter_errors(manifest)
        .map(|err| format!("  {}: {err}", err.instance_path()))
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "{} violates the official Cursor plugin schema:\n{}",
        manifest_path.display(),
        errors.join("\n")
    );
}

/// Component paths declared in a manifest, with the manifest fields that
/// declared them. Only string and string-array fields are path references;
/// inline objects (`hooks`, `mcpServers`) carry their config in place.
fn declared_component_paths(manifest: &Value) -> Vec<(String, String)> {
    let mut paths = Vec::new();
    for field in ["rules", "agents", "skills", "commands", "hooks", "mcpServers"] {
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

#[test]
fn cursor_bundle_manifest_matches_the_official_cursor_plugin_schema() {
    let schema: Value = serde_json::from_str(PLUGIN_SCHEMA).expect("schema fixture parses");
    let validator = compile(&schema);

    let manifest_path = repo_path("cursor-plugin/.cursor-plugin/plugin.json");
    let manifest = read_json(&manifest_path);
    assert_valid(&validator, &manifest, &manifest_path);
    assert_component_paths_resolve(&manifest, &repo_path("cursor-plugin"), &manifest_path);
}

#[test]
fn codex_bundle_manifest_matches_the_cursor_schema_plus_interface_extension() {
    let mut schema: Value = serde_json::from_str(PLUGIN_SCHEMA).expect("schema fixture parses");
    // Codex marketplaces read an `interface` block (display metadata) that
    // Cursor's schema does not define; with `additionalProperties: false`
    // the stock schema would reject it, so allow exactly that one extra key.
    schema["properties"]["interface"] = json!({ "type": "object" });
    let validator = compile(&schema);

    let manifest_path = repo_path("codex-plugin/.codex-plugin/plugin.json");
    let manifest = read_json(&manifest_path);
    assert_valid(&validator, &manifest, &manifest_path);
    assert_component_paths_resolve(&manifest, &repo_path("codex-plugin"), &manifest_path);
}

/// The schema's `name` pattern is what the marketplace submission checklist
/// enforces; both bundles must agree on the plugin name so cross-bundle
/// tooling (marketplace entries, cache paths) can key on one identifier.
#[test]
fn bundle_manifests_share_the_plugin_name() {
    let cursor = read_json(&repo_path("cursor-plugin/.cursor-plugin/plugin.json"));
    let codex = read_json(&repo_path("codex-plugin/.codex-plugin/plugin.json"));
    assert_eq!(cursor["name"], "tracedecay");
    assert_eq!(codex["name"], "tracedecay");
}
