//! Hook-side views of the intent action-fact predicates, which live in
//! [`tracedecay_policy::intent_action_facts`].

#[cfg(test)]
use super::ToolHintInput;

#[cfg(test)]
pub(in crate::hooks) use tracedecay_policy::intent_action_facts::is_harness_memory_path;

/// True when a Write/Edit/MultiEdit event adds a new function-shaped body of
/// meaningful size. See
/// [`tracedecay_policy::intent_action_facts::is_redundancy_candidate_edit`].
#[cfg(test)]
fn is_redundancy_candidate_edit(input: &ToolHintInput) -> bool {
    tracedecay_policy::intent_action_facts::is_redundancy_candidate_edit(&input.action_fact_input())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(tool: &str, path: &str, text: &str) -> ToolHintInput {
        ToolHintInput {
            tool_name: Some(tool.to_string()),
            file_path: Some(path.to_string()),
            edit_text: Some(text.to_string()),
            ..ToolHintInput::default()
        }
    }

    fn rust_fn_body() -> String {
        // A new function-sized Rust body (>= REDUNDANCY_EDIT_MIN_LINES lines).
        [
            "fn compute_widget_total(items: &[Item]) -> u64 {",
            "    let mut total = 0;",
            "    for item in items {",
            "        if item.active {",
            "            total += item.count;",
            "        }",
            "    }",
            "    total",
            "}",
        ]
        .join("\n")
    }

    #[test]
    fn short_or_non_function_edits_are_not_candidates() {
        // Under the line threshold, even with a `fn`.
        assert!(!is_redundancy_candidate_edit(&edit(
            "Write",
            "src/widgets.rs",
            "fn tiny() -> u8 { 1 }",
        )));
        // Long enough, but no function definition shape (a data/const block).
        let data = (0..12)
            .map(|i| format!("    const VALUE_{i}: u32 = {i};"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!is_redundancy_candidate_edit(&edit(
            "Write",
            "src/widgets.rs",
            &data,
        )));
    }

    #[test]
    fn requires_edit_tool_path_and_text() {
        let body = rust_fn_body();
        // Non-edit tool.
        assert!(!is_redundancy_candidate_edit(&edit(
            "Read",
            "src/widgets.rs",
            &body
        )));
        // Missing file_path.
        assert!(!is_redundancy_candidate_edit(&ToolHintInput {
            tool_name: Some("Write".to_string()),
            edit_text: Some(body.clone()),
            ..ToolHintInput::default()
        }));
        // Missing edit_text.
        assert!(!is_redundancy_candidate_edit(&ToolHintInput {
            tool_name: Some("Write".to_string()),
            file_path: Some("src/widgets.rs".to_string()),
            ..ToolHintInput::default()
        }));
    }

    #[test]
    fn language_keywords_are_recognized() {
        let cases = [
            (
                "mod.py",
                "def build_report(rows):\n    total = 0\n    for r in rows:\n        if r.ok:\n            total += r.n\n        else:\n            total -= 1\n    return total\n",
            ),
            (
                "server.go",
                "func Handle(w Writer, r Request) {\n    a := 1\n    b := 2\n    c := a + b\n    d := c * 2\n    e := d - 1\n    f := e + 3\n    _ = f\n}\n",
            ),
            (
                "app.ts",
                "export function render(state: State) {\n    const a = 1;\n    const b = 2;\n    const c = a + b;\n    const d = c * 2;\n    const e = d - 1;\n    const f = e + 3;\n    return f;\n}\n",
            ),
            (
                "view.jsx",
                "const handler = (event) => {\n    const a = 1;\n    const b = 2;\n    const c = a + b;\n    const d = c * 2;\n    const e = d - 1;\n    const f = e + 3;\n    return f;\n};\n",
            ),
        ];
        for (path, body) in cases {
            assert!(
                is_redundancy_candidate_edit(&edit("Write", path, body)),
                "{path} body should be recognized as a new function"
            );
        }
    }

    #[test]
    fn brace_method_signature_excludes_control_flow() {
        // A Java method signature qualifies.
        let method = [
            "public int total(List<Item> items) {",
            "    int total = 0;",
            "    for (Item i : items) {",
            "        if (i.active) {",
            "            total += i.count;",
            "        }",
            "    }",
            "    return total;",
            "}",
        ]
        .join("\n");
        assert!(is_redundancy_candidate_edit(&edit(
            "Write",
            "Totals.java",
            &method
        )));
        // A block of only control-flow (no signature) does not qualify.
        let control_only = [
            "if (ready) {",
            "    step();",
            "} else if (waiting) {",
            "    wait();",
            "}",
            "while (running) {",
            "    tick();",
            "}",
            "for (int i = 0; i < 3; i++) {",
            "    poll();",
            "}",
        ]
        .join("\n");
        assert!(!is_redundancy_candidate_edit(&edit(
            "Write",
            "Totals.java",
            &control_only,
        )));
    }

    #[test]
    fn unknown_extension_never_matches() {
        let body = rust_fn_body();
        assert!(!is_redundancy_candidate_edit(&edit(
            "Write", "notes.md", &body
        )));
        // No extension at all.
        assert!(!is_redundancy_candidate_edit(&edit(
            "Write", "Makefile", &body
        )));
    }
}
