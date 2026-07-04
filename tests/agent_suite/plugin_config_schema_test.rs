//! Validates the plugin bundles' MCP and hooks configuration files against
//! Cursor's config schemas.
//!
//! Cursor's official cursor/plugins repository publishes JSON schemas plus an
//! ajv validation workflow, but only for `plugin.json` and `marketplace.json`
//! (its `plugin.schema.json` types the inline `hooks` / `mcpServers` fields
//! as bare objects). There is no standalone published schema for `mcp.json`
//! or `hooks.json`, so the schemas vendored at
//! `tests/fixtures/cursor-schemas/{mcp,hooks}.schema.json` are derived from
//! Cursor's official field references:
//!
//! - <https://cursor.com/docs/context/mcp> (mcp.json server fields)
//! - <https://cursor.com/docs/hooks> (hooks.json events and per-script options)
//!
//! cross-checked against the hooks.json files shipped by official plugins in
//! <https://github.com/cursor/plugins> (commit 0452e08). See each schema's
//! top-level `description` for provenance details. The `plugin.json`
//! manifests themselves are covered by `plugin_manifest_schema_test.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use jsonschema::Validator;
use serde_json::{json, Value};

use crate::plugin_validation_support::{
    assert_schema_valid, compile_schema, read_json_file, repo_path,
};

const MCP_SCHEMA: &str = include_str!("../fixtures/cursor-schemas/mcp.schema.json");
const HOOKS_SCHEMA: &str = include_str!("../fixtures/cursor-schemas/hooks.schema.json");

fn compile(schema_body: &str) -> Validator {
    let schema: Value = serde_json::from_str(schema_body).expect("vendored schema parses");
    compile_schema(&schema)
}

/// Validates the config at `relative` if it exists. Returns whether it did.
/// Required files assert existence at their call sites; optional bundle
/// equivalents (e.g. a future codex-plugin/mcp.json) get validated the moment
/// someone adds them.
fn validate_if_present(validator: &Validator, relative: &str) -> bool {
    let path = repo_path(relative);
    if !path.exists() {
        return false;
    }
    assert_schema_valid(validator, &read_json_file(&path), &path);
    true
}

#[test]
fn cursor_bundle_mcp_config_matches_the_mcp_schema() {
    let validator = compile(MCP_SCHEMA);
    assert!(
        validate_if_present(&validator, "plugin/mcp-cursor.json"),
        "plugin/mcp-cursor.json must exist"
    );
}

#[test]
fn cursor_bundle_hooks_config_matches_the_hooks_schema() {
    let validator = compile(HOOKS_SCHEMA);
    assert!(
        validate_if_present(&validator, "plugin/hooks/hooks-cursor.json"),
        "plugin/hooks/hooks-cursor.json must exist"
    );
}

/// The Codex bundle reuses the same hooks.json shape (its host hook file is
/// `plugin/hooks/hooks-codex.json`) and shares the Claude/Codex `.mcp.json`.
#[test]
fn codex_bundle_configs_match_the_schemas_when_present() {
    // `hooks-codex.json` ships an empty `hooks` object plus a top-level
    // `description` explaining that repo-local Codex bundles carry no hooks and
    // the global bundle populates them at install time. Cursor's vendored
    // hooks schema sets `additionalProperties: false`, so allow exactly that
    // one self-documenting key (Codex's hook loader reads only `hooks.*` and
    // ignores it). Same escape hatch as the Codex manifest's `interface` block.
    let mut hooks_schema: Value =
        serde_json::from_str(HOOKS_SCHEMA).expect("vendored hooks schema parses");
    hooks_schema["properties"]["description"] = json!({ "type": "string" });
    let hooks_validator = compile_schema(&hooks_schema);
    assert!(
        validate_if_present(&hooks_validator, "plugin/hooks/hooks-codex.json"),
        "plugin/hooks/hooks-codex.json must exist"
    );

    let mcp_validator = compile(MCP_SCHEMA);
    validate_if_present(&mcp_validator, "plugin/.mcp.json");
}

/// Guards against the vendored schemas degenerating into accept-everything:
/// each must reject representative malformed configs.
#[test]
fn vendored_schemas_reject_malformed_configs() {
    let mcp_validator = compile(MCP_SCHEMA);
    let bad_mcp_configs = [
        // stdio server missing its required command
        json!({ "mcpServers": { "s": { "args": ["serve"] } } }),
        // remote server with an unknown field
        json!({ "mcpServers": { "s": { "url": "https://example.com/mcp", "cmd": "x" } } }),
        // top-level key typo
        json!({ "mcpservers": {} }),
    ];
    for config in &bad_mcp_configs {
        assert!(
            !mcp_validator.is_valid(config),
            "mcp schema unexpectedly accepted: {config}"
        );
    }

    let hooks_validator = compile(HOOKS_SCHEMA);
    let bad_hooks_configs = [
        // typo'd hook event name (event enum is the main guard this schema adds)
        json!({ "version": 1, "hooks": { "afterShellExecutionn": [{ "command": "x" }] } }),
        // hook definition missing its command
        json!({ "version": 1, "hooks": { "stop": [{ "timeout": 5 }] } }),
        // unsupported config version
        json!({ "version": 2, "hooks": {} }),
    ];
    for config in &bad_hooks_configs {
        assert!(
            !hooks_validator.is_valid(config),
            "hooks schema unexpectedly accepted: {config}"
        );
    }
}
