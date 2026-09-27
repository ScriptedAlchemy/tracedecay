//! Test-coverage and diagnostics workflow tool definitions.

use serde_json::Value;

use super::{def, def_rw};
use crate::ToolDefinition;

pub(super) fn def_test_map(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_test_map",
        "Test Map",
        "Which tests cover this, run tests for a symbol, test coverage. Map source symbols to their test functions by walking the call graph up to depth 3. A listed test may be a direct caller or a transitive caller reached through up to two intermediate functions; coverage here is static attribution (the symbol is reachable from a test), not executed line/branch coverage. Pair with tracedecay_test_risk to see the direct-vs-closure attribution_method distinction per symbol. Pass `file` or `node_id`.",
        input_schema,
    )
}

pub(super) fn def_test_risk(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_test_risk",
        "Test Risk",
        "Find high-risk source symbols with weak or no static test attribution. Reports both direct test-call coverage and conservative depth-3 closure attribution so integration-heavy repos do not look artificially uncovered. Each risk item carries an attribution_method (direct_unit vs closure); coverage_pct is a static attribution lower bound, not executed line/branch coverage. Answers: where should the next test go, and what only has broad behavioral evidence today?",
        input_schema,
    )
}

pub(super) fn def_diagnose(input_schema: Value) -> ToolDefinition {
    def(
        "tracedecay_diagnose",
        "Diagnose Cargo Output",
        "Parse raw `cargo check` / `cargo clippy` stderr text and map each \
         diagnostic to the smallest containing graph node, with callers \
         pre-attached so you can see what the failing code is reachable \
         from. Diagnostics without a `--> file:line:col` span are dropped. \
         Pass the full stderr capture; you do not need to pre-filter.",
        input_schema,
    )
}

pub(super) fn def_run_affected_tests(input_schema: Value) -> ToolDefinition {
    def_rw(
        "tracedecay_run_affected_tests",
        "Run Affected Tests",
        "Run `cargo test` for tests that cover the symbols in the explicit \
         `changed_paths` manifest. Closes the loop opened by \
         `tracedecay_test_map` / `tracedecay_test_risk`, emits pass/fail per \
         test alongside the source nodes each test covers. Output is the \
         libtest summary parsed into JSON.",
        input_schema,
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn affected_test_execution_requires_an_explicit_file_manifest() {
        let definition = crate::get_maximal_tool_definitions()
            .expect("tool definitions")
            .into_iter()
            .find(|definition| definition.name == "tracedecay_run_affected_tests")
            .expect("tracedecay_run_affected_tests definition");
        assert_eq!(
            definition.input_schema["required"],
            serde_json::json!(["changed_paths"])
        );
    }
}
