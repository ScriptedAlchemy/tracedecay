use tree_sitter::Node as TsNode;

use super::{ExtractionState, TypeScriptExtractor};
use crate::complexity::{TYPESCRIPT_COMPLEXITY, count_complexity};
use crate::traversal::find_direct_child_by_kind;
use tracedecay_domain::code_intelligence::{
    Edge, EdgeKind, Node, NodeKind, Visibility, generate_node_id,
};

/// Root callee names that mark a call as a test-framework construct whose
/// callback argument should be attributed as an executable test node.
const TEST_CALLEES: &[&str] = &[
    "describe",
    "it",
    "test",
    "suite",
    "bench",
    "context",
    "specify",
    "beforeEach",
    "afterEach",
    "beforeAll",
    "afterAll",
];

/// Resolve the root callee identifier of a `call_expression`, walking through
/// member accesses (`describe.only`, `it.each`) and curried calls
/// (`test.each([...])('t', fn)`). Returns the base identifier text, e.g.
/// `describe`, `it`, `test`.
fn test_call_root_callee(state: &ExtractionState, call: TsNode<'_>) -> Option<String> {
    // The callee is the first named child of the call_expression (the
    // "function" field); arguments follow.
    let mut callee = call.named_child(0)?;
    loop {
        match callee.kind() {
            "identifier" => return Some(state.node_text(callee)),
            // `describe.only`, `it.each`, `test.skip` — recurse into the
            // object side of the member access. Curried calls like
            // `test.each([...])(...)` are their own `call_expression`, so we
            // descend into that callee the same way.
            "member_expression" | "call_expression" => {
                callee = callee.named_child(0)?;
            }
            _ => return None,
        }
    }
}

/// Returns true if the given `call_expression` is a recognized test-framework
/// call (`describe`, `it`, `test`, …) based on its root callee.
pub(super) fn is_test_framework_call(state: &ExtractionState, call: TsNode<'_>) -> bool {
    test_call_root_callee(state, call).is_some_and(|root| TEST_CALLEES.contains(&root.as_str()))
}

/// Find the title argument (first string / template) of a test call's
/// argument list, stripped of quotes and truncated.
fn test_call_title(state: &ExtractionState, args: TsNode<'_>) -> Option<String> {
    let mut cursor = args.walk();
    if !cursor.goto_first_child() {
        return None;
    }
    loop {
        let child = cursor.node();
        match child.kind() {
            "string" => {
                let text = state.node_text(child);
                let title = text
                    .trim()
                    .trim_matches('\'')
                    .trim_matches('"')
                    .trim_matches('`');
                return Some(truncate_title(title));
            }
            "template_string" => {
                let text = state.node_text(child);
                let title = text.trim().trim_matches('`');
                return Some(truncate_title(title));
            }
            _ => {}
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
    None
}

/// Find the callback argument (last arrow/function expression) of a test
/// call's argument list.
fn test_call_callback(args: TsNode<'_>) -> Option<TsNode<'_>> {
    let mut cursor = args.walk();
    if !cursor.goto_first_child() {
        return None;
    }
    let mut found: Option<TsNode<'_>> = None;
    loop {
        let child = cursor.node();
        if matches!(
            child.kind(),
            "arrow_function" | "function_expression" | "function"
        ) {
            found = Some(child);
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
    found
}

/// Truncate a test title to a reasonable length for a node name.
fn truncate_title(title: &str) -> String {
    const MAX: usize = 120;
    let t = title.trim();
    if t.chars().count() <= MAX {
        t.to_string()
    } else {
        t.chars().take(MAX).collect()
    }
}

/// Extract a test-framework call (`describe`/`it`/`test`/…) as an executable
/// `Function` node named by its title, attributing all calls inside the
/// callback to that node so tests map back to the source they exercise.
///
/// The node is deliberately `NodeKind::Function` so it passes `is_callable`
/// filters and the Function|Method coverage universes. The framework callee
/// itself (e.g. `describe`) does NOT get a Calls ref.
pub(super) fn visit_test_call(state: &mut ExtractionState, call: TsNode<'_>) {
    // The arguments node holds the title and callback.
    let Some(args) = find_direct_child_by_kind(call, "arguments") else {
        return;
    };

    let root = test_call_root_callee(state, call);
    let title = test_call_title(state, args)
        .or(root)
        .unwrap_or_else(TypeScriptExtractor::anonymous_name);

    let start_line = call.start_position().row as u32;
    let end_line = call.end_position().row as u32;
    let start_column = call.start_position().column as u32;
    let end_column = call.end_position().column as u32;
    let qualified_name = format!("{}::{}", state.qualified_prefix(), title);
    let id = generate_node_id(&state.file_path, &NodeKind::Function, &title, start_line);

    let callback = test_call_callback(args);
    let is_async = callback.is_some_and(|cb| TypeScriptExtractor::has_child_kind(cb, "async"));
    let metrics = callback
        .map(|cb| count_complexity(cb, &TYPESCRIPT_COMPLEXITY, &state.source))
        .unwrap_or_default();

    let call_text = state.node_text(call);
    let signature = truncate_title(call_text.lines().next().unwrap_or_default());

    let graph_node = Node {
        id: id.clone(),
        kind: NodeKind::Function,
        name: title.clone(),
        qualified_name,
        file_path: state.file_path.clone(),
        start_line,
        attrs_start_line: start_line,
        end_line,
        start_column,
        end_column,
        signature: Some(signature),
        docstring: None,
        visibility: Visibility::Pub,
        is_async,
        branches: metrics.branches,
        loops: metrics.loops,
        returns: metrics.returns,
        max_nesting: metrics.max_nesting,
        unsafe_blocks: metrics.unsafe_blocks,
        unchecked_calls: metrics.unchecked_calls,
        assertions: metrics.assertions,
        updated_at: state.timestamp,
        parent_id: None,
    };
    state.nodes.push(graph_node);

    // Contains edge from the enclosing parent (File or outer describe).
    if let Some(parent_id) = state.parent_node_id() {
        state.edges.push(Edge {
            source: parent_id.to_string(),
            target: id.clone(),
            kind: EdgeKind::Contains,
            line: Some(start_line),
        });
    }

    // `it.todo('x')` and other callback-less forms produce no callback node
    // beyond the node above; nothing more to walk.
    let Some(callback) = callback else {
        return;
    };

    // Descend into the callback body under this test node.
    state.node_stack.push((title, id.clone()));
    if let Some(body) = find_direct_child_by_kind(callback, "statement_block") {
        visit_test_body(state, body, &id);
    } else {
        // Expression-bodied callback: `it('x', () => foo())`.
        TypeScriptExtractor::extract_call_sites(state, callback, &id);
    }
    state.node_stack.pop();
}

/// Whether a statement is a named function declaration whose body owns its
/// own call sites. `extract_call_sites` only skips nested arrow/function
/// *children*, so a top-level `function_declaration` statement would have
/// its body walked and double-attributed to the enclosing test — guard it.
/// Arrow/function-expression assignments (`const f = () => {}`) are already
/// skipped by `extract_call_sites` and need no guard here.
fn defines_own_callable(stmt: TsNode<'_>) -> bool {
    matches!(
        stmt.kind(),
        "function_declaration" | "generator_function_declaration"
    )
}

/// Walk the statement block of a test callback: recurse into nested
/// test-framework calls, and for every other statement both register nested
/// declarations (helpers/consts) AND attribute call sites to `test_id`.
fn visit_test_body(state: &mut ExtractionState, body: TsNode<'_>, test_id: &str) {
    let mut cursor = body.walk();
    if !cursor.goto_first_child() {
        return;
    }
    loop {
        let stmt = cursor.node();
        let mut handled = false;
        if stmt.kind() == "expression_statement" {
            if let Some(call) = find_direct_child_by_kind(stmt, "call_expression") {
                if is_test_framework_call(state, call) {
                    // Nested describe/it — recurse as its own test node.
                    visit_test_call(state, call);
                    handled = true;
                }
            }
        }
        if !handled {
            // Declarations inside describe (helpers, consts, nested classes)
            // become their own nodes.
            TypeScriptExtractor::visit_node(state, stmt);
            // Statements that define their own callable (a helper function
            // or arrow) own their call sites; attributing them again to the
            // test node would double-count. Only attribute non-declaration
            // statements (setup calls, assertions) to the test node.
            if !defines_own_callable(stmt) {
                TypeScriptExtractor::extract_call_sites(state, stmt, test_id);
            }
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}
