//! Test-coverage and diagnostics workflow tool definitions.

use serde_json::json;

use super::{def, def_rw};
use crate::mcp::tools::ToolDefinition;

pub(super) fn def_test_map() -> ToolDefinition {
    def(
        "tracedecay_test_map",
        "Test Map",
        "Which tests cover this, run tests for a symbol, test coverage. Map source symbols to their test functions by walking the call graph up to depth 3. A listed test may be a direct caller or a transitive caller reached through up to two intermediate functions; coverage here is static attribution (the symbol is reachable from a test), not executed line/branch coverage. Pair with tracedecay_test_risk to see the direct-vs-closure attribution_method distinction per symbol.",
        json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "Source file path to find test coverage for"
                },
                "node_id": {
                    "type": "string",
                    "description": "Specific node ID to find test coverage for (alternative to file)"
                }
            }
        }),
    )
}

pub(super) fn def_test_risk() -> ToolDefinition {
    def(
        "tracedecay_test_risk",
        "Test Risk",
        "Find high-risk source symbols with weak or no static test attribution. Reports both direct test-call coverage and conservative depth-3 closure attribution so integration-heavy repos do not look artificially uncovered. Each risk item carries an attribution_method (direct_unit vs closure); coverage_pct is a static attribution lower bound, not executed line/branch coverage. Answers: where should the next test go, and what only has broad behavioral evidence today?",
        json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "number",
                    "description": "Maximum number of results to return (default: 20)"
                },
                "path": {
                    "type": "string",
                    "description": "Filter to files under this directory path"
                },
                "include_tested": {
                    "type": "boolean",
                    "description": "Include already-tested functions in results (default: false)"
                }
            }
        }),
    )
}

pub(super) fn def_diagnose() -> ToolDefinition {
    def(
        "tracedecay_diagnose",
        "Diagnose Cargo Output",
        "Parse raw `cargo check` / `cargo clippy` stderr text and map each \
         diagnostic to the smallest containing graph node, with callers \
         pre-attached so you can see what the failing code is reachable \
         from. Diagnostics without a `--> file:line:col` span are dropped. \
         Each mapped node also carries up to 3 `near_duplicates` — cached \
         functional-duplicate matches from the redundancy index, when present. \
         Pass the full stderr capture; you do not need to pre-filter.",
        json!({
            "type": "object",
            "properties": {
                "cargo_output": {
                    "type": "string",
                    "description": "Raw stderr text from `cargo check` / `cargo clippy` / `rustc`."
                },
                "severity": {
                    "type": "string",
                    "enum": ["error", "warning", "all"],
                    "description": "Filter by severity (default: all)."
                },
                "include_callers": {
                    "type": "boolean",
                    "description": "Attach up to 5 callers per diagnostic (default: true)."
                },
                "max_diagnostics": {
                    "type": "number",
                    "description": "Cap on diagnostics in the response (default: 50)."
                }
            },
            "required": ["cargo_output"]
        }),
    )
}

pub(super) fn def_run_affected_tests() -> ToolDefinition {
    def_rw(
        "tracedecay_run_affected_tests",
        "Run Affected Tests",
        "Run `cargo test` for tests that cover the symbols in the explicit \
         `changed_paths` manifest. Closes the loop opened by \
         `tracedecay_test_map` / `tracedecay_test_risk` — emits pass/fail per \
         test alongside the source nodes each test covers. Output is the \
         libtest summary parsed into JSON.",
        json!({
            "type": "object",
            "properties": {
                "changed_paths": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Explicit manifest of file paths used to compute affected tests."
                },
                "profile": {
                    "type": "string",
                    "enum": ["debug", "release"],
                    "description": "Cargo profile (default: debug)."
                },
                "timeout_secs": {
                    "type": "number",
                    "description": "Maximum wall time before the cargo subprocess is killed (default: 300)."
                },
                "max_tests": {
                    "type": "number",
                    "description": "Cap on tests dispatched in a single invocation (default: 100)."
                }
            },
            "required": ["changed_paths"]
        }),
    )
}

pub(super) fn def_diagnostics() -> ToolDefinition {
    def(
        "tracedecay_diagnostics",
        "Read Canonical Diagnostics",
        "Read the daemon-retained clean-generation diagnostic authority. This \
         compatibility name does not start an analyzer or execute a build; \
         configured producers publish new diagnostics through their owned \
         lifecycle.",
        json!({
            "type": "object",
            "properties": {
                "scope": {
                    "type": "string",
                    "enum": ["workspace", "file"],
                    "description": "Read scope. Default 'workspace'. 'file' requires `path`."
                },
                "path": {
                    "type": "string",
                    "description": "Project-relative file path when scope='file'."
                },
                "maximum_diagnostics": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 1000,
                    "description": "Maximum diagnostics returned in this page."
                },
                "cursor": {
                    "type": ["string", "null"],
                    "minLength": 1,
                    "description": "Opaque cursor returned by the prior page."
                }
            }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::def_run_affected_tests;

    #[test]
    fn affected_test_execution_requires_an_explicit_file_manifest() {
        let definition = def_run_affected_tests();
        assert_eq!(
            definition.input_schema["required"],
            serde_json::json!(["changed_paths"])
        );
    }
}
