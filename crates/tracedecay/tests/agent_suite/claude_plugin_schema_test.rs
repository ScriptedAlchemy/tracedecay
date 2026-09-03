//! Validates the Claude Code side of the plugin bundle against vendored JSON
//! Schemas: `plugin/hooks/hooks-claude.json`, `plugin/.claude-plugin/plugin.json`,
//! and `plugin/.claude-plugin/marketplace.json`.
//!
//! Anthropic does not publish standalone machine-readable schemas for these
//! files (marketplace.json even references a schema URL that is not served),
//! so the schemas vendored at `tests/fixtures/claude-schemas/` are derived
//! from the Claude Code docs — see each schema's top-level `description` for
//! provenance. This closes the gap where the Cursor/Codex configs were
//! schema-validated (`plugin_config_schema_test.rs`) but the Claude configs
//! only had semantic spot checks.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use jsonschema::Validator;
use serde_json::{Value, json};

use crate::plugin_validation_support::{
    assert_schema_valid, compile_schema, read_json_file, repo_path,
};

const HOOKS_SCHEMA: &str =
    include_str!("../../../../tests/fixtures/claude-schemas/hooks.schema.json");
const PLUGIN_SCHEMA: &str =
    include_str!("../../../../tests/fixtures/claude-schemas/plugin.schema.json");
const MARKETPLACE_SCHEMA: &str =
    include_str!("../../../../tests/fixtures/claude-schemas/marketplace.schema.json");

fn compile(schema_body: &str) -> Validator {
    let schema: Value = serde_json::from_str(schema_body).expect("vendored schema parses");
    compile_schema(&schema)
}

fn assert_repo_file_valid(validator: &Validator, relative: &str) {
    let path = repo_path(relative);
    assert!(path.exists(), "{relative} must exist");
    assert_schema_valid(validator, &read_json_file(&path), &path);
}

#[test]
fn claude_bundle_hooks_config_matches_the_claude_hooks_schema() {
    assert_repo_file_valid(&compile(HOOKS_SCHEMA), "plugin/hooks/hooks-claude.json");
}

#[test]
fn claude_plugin_manifest_matches_the_claude_plugin_schema() {
    assert_repo_file_valid(&compile(PLUGIN_SCHEMA), "plugin/.claude-plugin/plugin.json");
}

#[test]
fn claude_marketplace_matches_the_claude_marketplace_schema() {
    assert_repo_file_valid(
        &compile(MARKETPLACE_SCHEMA),
        "plugin/.claude-plugin/marketplace.json",
    );
}

#[test]
fn claude_marketplace_includes_strict_validator_metadata() {
    let path = repo_path("plugin/.claude-plugin/marketplace.json");
    let marketplace = read_json_file(&path);
    let description = marketplace
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    assert!(
        !description.is_empty(),
        "{} must include a top-level description so `claude plugin validate --strict` stays clean",
        path.display()
    );
}

/// Guards against the vendored schemas degenerating into accept-everything:
/// each must reject representative malformed configs.
#[test]
fn vendored_claude_schemas_reject_malformed_configs() {
    let hooks_validator = compile(HOOKS_SCHEMA);
    let bad_hooks_configs = [
        // typo'd event name (Claude events are PascalCase)
        json!({ "hooks": { "preToolUse": [{ "hooks": [{ "type": "command", "command": "x" }] }] } }),
        // matcher group missing its hooks array
        json!({ "hooks": { "PreToolUse": [{ "matcher": "Agent" }] } }),
        // command hook missing its command
        json!({ "hooks": { "Stop": [{ "hooks": [{ "type": "command" }] }] } }),
        // unsupported hook type
        json!({ "hooks": { "Stop": [{ "hooks": [{ "type": "prompt", "command": "x" }] }] } }),
    ];
    for config in &bad_hooks_configs {
        assert!(
            !hooks_validator.is_valid(config),
            "claude hooks schema unexpectedly accepted: {config}"
        );
    }

    let plugin_validator = compile(PLUGIN_SCHEMA);
    let bad_manifests = [
        // missing required name
        json!({ "version": "1.0.0" }),
        // name not kebab-case
        json!({ "name": "Trace Decay" }),
        // unknown top-level field
        json!({ "name": "tracedecay", "entrypoint": "main.js" }),
    ];
    for manifest in &bad_manifests {
        assert!(
            !plugin_validator.is_valid(manifest),
            "claude plugin schema unexpectedly accepted: {manifest}"
        );
    }

    let marketplace_validator = compile(MARKETPLACE_SCHEMA);
    let bad_marketplaces = [
        // plugins must be an array
        json!({ "name": "m", "plugins": {} }),
        // plugin entry missing its source
        json!({ "name": "m", "plugins": [{ "name": "p" }] }),
        // owner missing its name
        json!({ "name": "m", "owner": {}, "plugins": [] }),
    ];
    for marketplace in &bad_marketplaces {
        assert!(
            !marketplace_validator.is_valid(marketplace),
            "claude marketplace schema unexpectedly accepted: {marketplace}"
        );
    }
}
