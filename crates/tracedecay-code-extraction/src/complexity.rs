//! Generic complexity counting for tree-sitter AST nodes.
//!
//! Walks descendants of a function/method node and counts branches,
//! loops, early-exit statements, and maximum nesting depth. The counts
//! are language-agnostic — each extractor supplies the node type names
//! that correspond to each category.

use tracedecay_domain::ComplexityAnalysisV1;
use tree_sitter::Node as TsNode;

/// Nodes one body walk may visit before it stops and reports itself
/// incomplete. Every visited node counts, so the bound is the work performed.
pub const TRAVERSAL_BUDGET: usize = 500_000;

/// Configuration mapping tree-sitter node type names to complexity categories.
pub struct ComplexityConfig {
    /// Node types that count as branches (if, match/switch arm, ternary).
    pub branch_types: &'static [&'static str],
    /// Node types that count as loops (for, while, loop, do).
    pub loop_types: &'static [&'static str],
    /// Node types that count as early exits (return, break, continue, throw).
    pub return_types: &'static [&'static str],
    /// Node types that introduce a new nesting level (block, `compound_statement`).
    pub nesting_types: &'static [&'static str],
    /// Node types representing unsafe blocks (e.g. `unsafe_block` in Rust, `unsafe_statement` in C#).
    pub unsafe_types: &'static [&'static str],
    /// Node types that are inherently unchecked operations (e.g. `non_null_assertion_expression`).
    pub unchecked_types: &'static [&'static str],
    /// Method names that represent unchecked/force-unwrap calls (e.g. `unwrap`, `get`).
    /// Matched against the method name in call expressions.
    pub unchecked_methods: &'static [&'static str],
    /// Node types representing method/function call expressions, used for `unchecked_methods` matching.
    pub call_expression_types: &'static [&'static str],
    /// Field name used to extract the method name from a call expression node.
    /// e.g. "function" for TS, "method" for Rust. Empty to skip.
    pub call_method_field: &'static str,
    /// Macro/function names that count as assertions (e.g. `assert`, `assert_eq`, `assertEquals`).
    /// Matched against macro invocation names and function/method call names.
    pub assertion_names: &'static [&'static str],
    /// Node types representing macro invocations (e.g. `macro_invocation` in Rust).
    pub macro_invocation_types: &'static [&'static str],
}

/// Complexity metrics extracted from a function body.
///
/// When `analysis` is not [`ComplexityAnalysisV1::Complete`], every counter is
/// a lower bound over the nodes the walk reached before its budget ran out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ComplexityMetrics {
    pub branches: u32,
    pub loops: u32,
    pub returns: u32,
    pub max_nesting: u32,
    /// Number of unsafe blocks/statements.
    pub unsafe_blocks: u32,
    /// Number of unchecked/force-unwrap calls or assertions.
    pub unchecked_calls: u32,
    /// Number of assertion calls (assert, `debug_assert`, assertEquals, etc.).
    pub assertions: u32,
    /// Whether the walk covered the whole body.
    pub analysis: ComplexityAnalysisV1,
}

/// Counts complexity metrics over every descendant of `node`, visiting at most
/// [`TRAVERSAL_BUDGET`] nodes; see [`count_complexity_bounded`].
pub fn count_complexity(
    node: TsNode<'_>,
    config: &ComplexityConfig,
    source: &[u8],
) -> ComplexityMetrics {
    count_complexity_bounded(node, config, source, TRAVERSAL_BUDGET)
}

/// Counts complexity metrics by walking the descendants of `node` in source
/// order with a `TreeCursor`, so the pending state is one cursor plus the
/// nesting depth rather than a stack of every enqueued sibling. The walk
/// visits at most `budget` nodes; reaching the bound with nodes still
/// unvisited yields [`ComplexityAnalysisV1::TraversalBudgetExhausted`] and the
/// counters accumulated so far. The nesting depth is the number of
/// nesting-type ancestors enclosing each node.
///
/// `source` is needed to extract method/macro names for unchecked-call and
/// assertion detection. Pass an empty slice to skip name-based matching.
pub fn count_complexity_bounded(
    node: TsNode<'_>,
    config: &ComplexityConfig,
    source: &[u8],
    budget: usize,
) -> ComplexityMetrics {
    debug_assert!(
        !config.branch_types.is_empty() || !config.loop_types.is_empty(),
        "count_complexity called with config that has no branch or loop types"
    );
    debug_assert!(
        node.child_count() > 0,
        "count_complexity called on a node with no children"
    );
    let mut metrics = ComplexityMetrics::default();
    let root = node.id();
    let mut cursor = node.walk();
    if !cursor.goto_first_child() {
        return metrics;
    }

    // Nesting-type ancestors of the cursor's node, excluding the root.
    let mut depth: u32 = 0;
    let mut visited: usize = 0;
    loop {
        if visited == budget {
            metrics.analysis = ComplexityAnalysisV1::TraversalBudgetExhausted;
            return metrics;
        }
        visited += 1;

        let current = cursor.node();
        classify(current, config, source, &mut metrics);
        let nesting = u32::from(config.nesting_types.contains(&current.kind()));
        let current_depth = depth + nesting;
        metrics.max_nesting = metrics.max_nesting.max(current_depth);

        if cursor.goto_first_child() {
            depth = current_depth;
            continue;
        }
        // Leaf: advance to the next sibling, ascending until one exists or
        // the walk returns to the root.
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() || cursor.node().id() == root {
                return metrics;
            }
            depth -= u32::from(config.nesting_types.contains(&cursor.node().kind()));
        }
    }
}

/// Adds `current`'s contribution to every counter but nesting.
fn classify(
    current: TsNode<'_>,
    config: &ComplexityConfig,
    source: &[u8],
    metrics: &mut ComplexityMetrics,
) {
    let kind = current.kind();
    if config.branch_types.contains(&kind) {
        metrics.branches += 1;
    }
    if config.loop_types.contains(&kind) {
        metrics.loops += 1;
    }
    if config.return_types.contains(&kind) {
        metrics.returns += 1;
    }
    if config.unsafe_types.contains(&kind) {
        metrics.unsafe_blocks += 1;
    }
    // Unchecked operator types (e.g. non_null_assertion_expression, `!!`).
    if config.unchecked_types.contains(&kind) {
        metrics.unchecked_calls += 1;
    }
    if source.is_empty() {
        return;
    }
    // Name-based detection for call expressions (unchecked methods + assertions).
    if config.call_expression_types.contains(&kind)
        && let Some(name) = extract_call_name(current, config.call_method_field, source)
    {
        if config.unchecked_methods.contains(&name) {
            metrics.unchecked_calls += 1;
        }
        if config.assertion_names.contains(&name) {
            metrics.assertions += 1;
        }
    }
    // Name-based detection for macro invocations (Rust assert!, debug_assert!, etc.).
    if config.macro_invocation_types.contains(&kind)
        && let Some(name) = extract_macro_name(current, source)
    {
        if config.assertion_names.contains(&name) {
            metrics.assertions += 1;
        }
        if config.unchecked_methods.contains(&name) {
            metrics.unchecked_calls += 1;
        }
    }
}

/// Extracts the method/function name from a call expression node.
///
/// Tries the configured `method_field` first (e.g. "function", "method"),
/// then falls back to common child patterns: last identifier before `(`,
/// or a `field_expression`/`member_expression` selector.
///
/// Returns a `&str` borrowed from `source`: this runs for every call
/// expression in the per-node loop, so it must not allocate.
fn extract_call_name<'s>(
    node: TsNode<'_>,
    method_field: &str,
    source: &'s [u8],
) -> Option<&'s str> {
    // Try the configured field name first.
    if !method_field.is_empty()
        && let Some(field_node) = node.child_by_field_name(method_field)
    {
        // For chained calls like `x.unwrap()`, the field may be a
        // field_expression / member_expression — grab the rightmost identifier.
        let text = rightmost_identifier(field_node, source);
        if !text.is_empty() {
            return Some(text);
        }
    }

    // Fallback: scan direct children via cursor (O(N), not O(N²)).
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            let ck = child.kind();
            if (ck == "identifier" || ck == "field_identifier" || ck == "property_identifier")
                && let Ok(text) = child.utf8_text(source)
            {
                return Some(text);
            }
            // member_expression / field_expression: grab the property/field child.
            if ck.contains("member_expression") || ck.contains("field_expression") {
                let text = rightmost_identifier(child, source);
                if !text.is_empty() {
                    return Some(text);
                }
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    None
}

/// Extracts the macro name from a macro invocation node (e.g. `assert!`).
///
/// Looks for the first identifier child, stripping a trailing `!` if present.
/// Returns a `&str` borrowed from `source` — see `extract_call_name`.
fn extract_macro_name<'s>(node: TsNode<'_>, source: &'s [u8]) -> Option<&'s str> {
    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            let ck = child.kind();
            if (ck == "identifier" || ck == "scoped_identifier")
                && let Ok(text) = child.utf8_text(source)
            {
                return Some(text.trim_end_matches('!'));
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    None
}

/// Returns the text of the rightmost identifier-like child of `node`,
/// borrowed from `source` (empty when no identifier child exists).
fn rightmost_identifier<'s>(node: TsNode<'_>, source: &'s [u8]) -> &'s str {
    // If node itself is a simple identifier, return it.
    let nk = node.kind();
    if nk == "identifier" || nk == "field_identifier" || nk == "property_identifier" {
        return node.utf8_text(source).unwrap_or("");
    }
    // Walk children via cursor and remember the rightmost match — `node.child(i)`
    // would be O(N²) for the right-to-left scan the previous revision did.
    let mut cursor = node.walk();
    let mut found = "";
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            let ck = child.kind();
            if (ck == "identifier" || ck == "field_identifier" || ck == "property_identifier")
                && let Ok(text) = child.utf8_text(source)
            {
                found = text;
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
    found
}

pub static RUST_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &["if_expression", "match_arm", "else_clause"],
    loop_types: &["for_expression", "while_expression", "loop_expression"],
    return_types: &[
        "return_expression",
        "break_expression",
        "continue_expression",
    ],
    nesting_types: &["block"],
    unsafe_types: &["unsafe_block"],
    unchecked_types: &[],
    unchecked_methods: &["unwrap", "expect"],
    call_expression_types: &["call_expression"],
    call_method_field: "function",
    assertion_names: &[
        "assert",
        "assert_eq",
        "assert_ne",
        "debug_assert",
        "debug_assert_eq",
        "debug_assert_ne",
    ],
    macro_invocation_types: &["macro_invocation"],
};

pub static JAVA_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &[
        "if_statement",
        "switch_block_statement_group",
        "ternary_expression",
        "catch_clause",
        "else",
    ],
    loop_types: &[
        "for_statement",
        "enhanced_for_statement",
        "while_statement",
        "do_statement",
    ],
    return_types: &[
        "return_statement",
        "break_statement",
        "continue_statement",
        "throw_statement",
    ],
    nesting_types: &["block"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &["get"],
    call_expression_types: &["method_invocation"],
    call_method_field: "name",
    assertion_names: &[
        "assert",
        "assertEquals",
        "assertNotEquals",
        "assertTrue",
        "assertFalse",
        "assertNull",
        "assertNotNull",
        "assertThrows",
        "assertThat",
        "assertArrayEquals",
    ],
    macro_invocation_types: &[],
};

pub static GO_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &[
        "if_statement",
        "expression_case",
        "type_case",
        "default_case",
    ],
    loop_types: &["for_statement"],
    return_types: &["return_statement", "break_statement", "continue_statement"],
    nesting_types: &["block"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &[],
    call_expression_types: &["call_expression"],
    call_method_field: "function",
    assertion_names: &[
        "assert", "require", "Equal", "NotEqual", "True", "False", "Nil", "NotNil", "Error",
        "NoError",
    ],
    macro_invocation_types: &[],
};

pub static PYTHON_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &[
        "if_statement",
        "elif_clause",
        "else_clause",
        "conditional_expression",
        "except_clause",
    ],
    loop_types: &["for_statement", "while_statement"],
    return_types: &[
        "return_statement",
        "break_statement",
        "continue_statement",
        "raise_statement",
    ],
    nesting_types: &["block"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &[],
    call_expression_types: &["call"],
    call_method_field: "function",
    assertion_names: &[
        "assert",
        "assertEqual",
        "assertNotEqual",
        "assertTrue",
        "assertFalse",
        "assertIs",
        "assertIsNone",
        "assertIsNotNone",
        "assertIn",
        "assertRaises",
        "assertAlmostEqual",
    ],
    macro_invocation_types: &[],
};

pub static TYPESCRIPT_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &[
        "if_statement",
        "switch_case",
        "ternary_expression",
        "catch_clause",
        "else_clause",
    ],
    loop_types: &[
        "for_statement",
        "for_in_statement",
        "while_statement",
        "do_statement",
    ],
    return_types: &[
        "return_statement",
        "break_statement",
        "continue_statement",
        "throw_statement",
    ],
    nesting_types: &["statement_block"],
    unsafe_types: &[],
    unchecked_types: &["non_null_assertion_expression"],
    unchecked_methods: &[],
    call_expression_types: &["call_expression"],
    call_method_field: "function",
    assertion_names: &[
        "assert",
        "expect",
        "assertEquals",
        "assertStrictEquals",
        "deepEqual",
        "strictEqual",
        "ok",
        "notOk",
    ],
    macro_invocation_types: &[],
};

pub static C_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &[
        "if_statement",
        "case_statement",
        "conditional_expression",
        "else_clause",
    ],
    loop_types: &["for_statement", "while_statement", "do_statement"],
    return_types: &["return_statement", "break_statement", "continue_statement"],
    nesting_types: &["compound_statement"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &[],
    call_expression_types: &["call_expression"],
    call_method_field: "function",
    assertion_names: &[
        "assert",
        "assert_true",
        "assert_false",
        "assert_int_equal",
        "assert_string_equal",
        "assert_null",
        "assert_non_null",
        "CU_ASSERT",
        "CU_ASSERT_EQUAL",
    ],
    macro_invocation_types: &[],
};

pub static CPP_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &[
        "if_statement",
        "case_statement",
        "conditional_expression",
        "catch_clause",
        "else_clause",
    ],
    loop_types: &[
        "for_statement",
        "while_statement",
        "do_statement",
        "for_range_loop",
    ],
    return_types: &[
        "return_statement",
        "break_statement",
        "continue_statement",
        "throw_statement",
    ],
    nesting_types: &["compound_statement"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &[],
    call_expression_types: &["call_expression"],
    call_method_field: "function",
    assertion_names: &[
        "assert",
        "ASSERT_TRUE",
        "ASSERT_FALSE",
        "ASSERT_EQ",
        "ASSERT_NE",
        "ASSERT_LT",
        "ASSERT_GT",
        "EXPECT_TRUE",
        "EXPECT_FALSE",
        "EXPECT_EQ",
        "EXPECT_NE",
        "static_assert",
    ],
    macro_invocation_types: &[],
};

pub static KOTLIN_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &["if_expression", "when_entry", "catch_block", "else"],
    loop_types: &["for_statement", "while_statement", "do_while_statement"],
    return_types: &["jump_expression"],
    nesting_types: &["statements"],
    unsafe_types: &[],
    unchecked_types: &["postfix_expression"],
    unchecked_methods: &[],
    call_expression_types: &["call_expression"],
    call_method_field: "",
    assertion_names: &[
        "assert",
        "assertEquals",
        "assertNotEquals",
        "assertTrue",
        "assertFalse",
        "assertNull",
        "assertNotNull",
        "assertIs",
        "assertIsNot",
    ],
    macro_invocation_types: &[],
};

pub static SCALA_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &["if_expression", "case_clause", "catch_clause"],
    loop_types: &["for_expression", "while_expression"],
    return_types: &["return_expression"],
    nesting_types: &["block"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &["get"],
    call_expression_types: &["call_expression"],
    call_method_field: "function",
    assertion_names: &["assert", "assertEquals", "assertResult", "assertThrows"],
    macro_invocation_types: &[],
};

#[cfg(feature = "lang-dart")]
pub static DART_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &[
        "if_statement",
        "switch_statement_case",
        "catch_clause",
        "conditional_expression",
    ],
    loop_types: &["for_statement", "while_statement", "do_statement"],
    return_types: &[
        "return_statement",
        "break_statement",
        "continue_statement",
        "throw_statement",
    ],
    nesting_types: &["block"],
    unsafe_types: &[],
    unchecked_types: &["postfix_expression"],
    unchecked_methods: &[],
    call_expression_types: &["call_expression"],
    call_method_field: "function",
    assertion_names: &["assert", "expect", "expectLater", "expectAsync"],
    macro_invocation_types: &[],
};

pub static CSHARP_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &[
        "if_statement",
        "switch_section",
        "conditional_expression",
        "catch_clause",
    ],
    loop_types: &[
        "for_statement",
        "for_each_statement",
        "while_statement",
        "do_statement",
    ],
    return_types: &[
        "return_statement",
        "break_statement",
        "continue_statement",
        "throw_statement",
    ],
    nesting_types: &["block"],
    unsafe_types: &["unsafe_statement"],
    unchecked_types: &[],
    unchecked_methods: &[],
    call_expression_types: &["invocation_expression"],
    call_method_field: "function",
    assertion_names: &[
        "Assert",
        "AreEqual",
        "AreNotEqual",
        "IsTrue",
        "IsFalse",
        "IsNull",
        "IsNotNull",
        "ThrowsException",
    ],
    macro_invocation_types: &[],
};

#[cfg(feature = "lang-pascal")]
pub static PASCAL_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &["if_statement", "case_item", "else_clause"],
    loop_types: &["for_statement", "while_statement", "repeat_statement"],
    return_types: &["raise_statement"],
    nesting_types: &["begin_end_block"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &[],
    call_expression_types: &["call_statement"],
    call_method_field: "",
    assertion_names: &["Assert", "CheckEquals", "CheckTrue", "CheckFalse"],
    macro_invocation_types: &[],
};

#[cfg(feature = "lang-php")]
pub static PHP_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &[
        "if_statement",
        "case_statement",
        "catch_clause",
        "else_clause",
        "else_if_clause",
    ],
    loop_types: &[
        "for_statement",
        "foreach_statement",
        "while_statement",
        "do_statement",
    ],
    return_types: &[
        "return_statement",
        "break_statement",
        "continue_statement",
        "throw_expression",
    ],
    nesting_types: &["compound_statement"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &[],
    call_expression_types: &["function_call_expression", "member_call_expression"],
    call_method_field: "name",
    assertion_names: &[
        "assert",
        "assertEquals",
        "assertNotEquals",
        "assertTrue",
        "assertFalse",
        "assertNull",
        "assertNotNull",
        "assertSame",
        "assertInstanceOf",
    ],
    macro_invocation_types: &[],
};

#[cfg(feature = "lang-ruby")]
pub static RUBY_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &["if", "elsif", "when", "rescue", "conditional"],
    loop_types: &["for", "while", "until"],
    return_types: &["return", "break", "next"],
    nesting_types: &["body_statement", "do_block", "block"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &["fetch"],
    call_expression_types: &["call", "method_call"],
    call_method_field: "method",
    assertion_names: &[
        "assert",
        "assert_equal",
        "assert_not_equal",
        "assert_nil",
        "assert_not_nil",
        "assert_raises",
        "assert_match",
        "refute",
    ],
    macro_invocation_types: &[],
};

pub static SWIFT_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &[
        "if_statement",
        "switch_entry",
        "guard_statement",
        "catch_keyword",
    ],
    loop_types: &[
        "for_in_statement",
        "while_statement",
        "repeat_while_statement",
    ],
    return_types: &["control_transfer_statement"],
    nesting_types: &["code_block"],
    unsafe_types: &[],
    unchecked_types: &["force_unwrap_expression"],
    unchecked_methods: &[],
    call_expression_types: &["call_expression"],
    call_method_field: "",
    assertion_names: &[
        "assert",
        "precondition",
        "assertionFailure",
        "XCTAssert",
        "XCTAssertEqual",
        "XCTAssertTrue",
        "XCTAssertFalse",
        "XCTAssertNil",
        "XCTAssertNotNil",
    ],
    macro_invocation_types: &[],
};

#[cfg(feature = "lang-bash")]
pub static BASH_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &["if_statement", "elif_clause", "else_clause", "case_item"],
    loop_types: &["for_statement", "while_statement", "c_style_for_statement"],
    return_types: &["return_statement"],
    nesting_types: &["compound_statement", "subshell"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &[],
    call_expression_types: &["command"],
    call_method_field: "name",
    assertion_names: &[],
    macro_invocation_types: &[],
};

#[cfg(feature = "lang-lua")]
pub static LUA_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &["if_statement", "elseif_statement", "else_statement"],
    loop_types: &[
        "for_statement",
        "for_in_statement",
        "while_statement",
        "repeat_statement",
    ],
    return_types: &["return_statement", "break_statement"],
    nesting_types: &["block"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &[],
    call_expression_types: &["function_call"],
    call_method_field: "",
    assertion_names: &["assert", "assert_equal", "assert_true", "assert_false"],
    macro_invocation_types: &[],
};

#[cfg(feature = "lang-zig")]
pub static ZIG_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &[
        "if_expression",
        "switch_expression",
        "else_expression",
        "catch",
    ],
    loop_types: &["for_expression", "while_expression"],
    return_types: &[
        "return_expression",
        "break_expression",
        "continue_expression",
    ],
    nesting_types: &["block"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &["orelse"],
    call_expression_types: &["call_expression"],
    call_method_field: "",
    assertion_names: &["expect", "expectEqual", "expectEqualStrings", "expectError"],
    macro_invocation_types: &[],
};

#[cfg(feature = "lang-nix")]
pub static NIX_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &["if_expression"],
    loop_types: &[],
    return_types: &[],
    nesting_types: &["attrset_expression", "let_expression"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &[],
    call_expression_types: &["apply_expression"],
    call_method_field: "",
    assertion_names: &[],
    macro_invocation_types: &[],
};

#[cfg(feature = "lang-powershell")]
pub static POWERSHELL_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &[
        "if_statement",
        "elseif_clause",
        "else_clause",
        "switch_statement",
        "catch_clause",
    ],
    loop_types: &[
        "for_statement",
        "foreach_statement",
        "while_statement",
        "do_while_statement",
    ],
    return_types: &[
        "return_statement",
        "break_statement",
        "continue_statement",
        "throw_statement",
    ],
    nesting_types: &["script_block"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &[],
    call_expression_types: &["command_expression"],
    call_method_field: "",
    assertion_names: &["Should", "Assert"],
    macro_invocation_types: &[],
};

#[cfg(feature = "lang-perl")]
pub static PERL_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &[
        "if_statement",
        "elsif_clause",
        "else_clause",
        "unless_statement",
        "conditional_expression",
    ],
    loop_types: &[
        "for_statement",
        "foreach_statement",
        "while_statement",
        "until_statement",
    ],
    return_types: &["return_expression", "last_expression", "next_expression"],
    nesting_types: &["block"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &[],
    call_expression_types: &["call_expression", "method_call_expression"],
    call_method_field: "",
    assertion_names: &["ok", "is", "isnt", "like", "unlike", "cmp_ok", "is_deeply"],
    macro_invocation_types: &[],
};

#[cfg(feature = "lang-objc")]
pub static OBJC_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &[
        "if_statement",
        "case_statement",
        "conditional_expression",
        "catch_clause",
        "else_clause",
    ],
    loop_types: &[
        "for_statement",
        "while_statement",
        "do_statement",
        "for_in_statement",
    ],
    return_types: &["return_statement", "break_statement", "continue_statement"],
    nesting_types: &["compound_statement"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &[],
    call_expression_types: &["call_expression", "message_expression"],
    call_method_field: "",
    assertion_names: &[
        "NSAssert",
        "NSCAssert",
        "XCTAssert",
        "XCTAssertTrue",
        "XCTAssertFalse",
        "XCTAssertEqual",
        "XCTAssertNil",
        "XCTAssertNotNil",
    ],
    macro_invocation_types: &[],
};

#[cfg(feature = "lang-fortran")]
pub static FORTRAN_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &[
        "if_statement",
        "elseif_clause",
        "else_clause",
        "case_statement",
        "where_statement",
    ],
    loop_types: &["do_loop_statement", "forall_statement"],
    return_types: &[
        "return_statement",
        "stop_statement",
        "exit_statement",
        "cycle_statement",
    ],
    nesting_types: &["block"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &[],
    call_expression_types: &["call_expression"],
    call_method_field: "",
    assertion_names: &[],
    macro_invocation_types: &[],
};

#[cfg(feature = "lang-qbasic")]
pub static QBASIC_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &["block_if_statement"],
    loop_types: &["for_statement", "while_statement", "do_loop_statement"],
    return_types: &["exit_statement"],
    nesting_types: &[],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &[],
    call_expression_types: &["call_statement"],
    call_method_field: "",
    assertion_names: &[],
    macro_invocation_types: &[],
};

#[cfg(feature = "lang-r")]
pub static R_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &["if_statement"],
    loop_types: &["for_statement", "while_statement", "repeat_statement"],
    return_types: &["return"],
    nesting_types: &["braced_expression"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &[],
    call_expression_types: &["call"],
    call_method_field: "",
    assertion_names: &[
        "stopifnot",
        "assert_that",
        "expect_equal",
        "expect_true",
        "expect_false",
    ],
    macro_invocation_types: &[],
};

#[cfg(feature = "lang-julia")]
pub static JULIA_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &["if_statement", "elseif_clause", "ternary_expression"],
    loop_types: &["for_statement", "while_statement"],
    return_types: &["return_statement", "break_statement", "continue_statement"],
    nesting_types: &["block", "compound_statement"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &[],
    call_expression_types: &["call_expression"],
    call_method_field: "",
    assertion_names: &["@assert", "assert", "@test", "@test_throws"],
    macro_invocation_types: &["macro_expression"],
};

#[cfg(feature = "lang-ocaml")]
pub static OCAML_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &["if_expression", "match_case"],
    loop_types: &["for_expression", "while_expression"],
    return_types: &[],
    nesting_types: &["let_binding"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &[],
    call_expression_types: &["application_expression"],
    call_method_field: "",
    assertion_names: &[
        "assert",
        "assert_equal",
        "assert_string_equal",
        "assert_bool",
        "check_bool",
    ],
    macro_invocation_types: &[],
};

#[cfg(feature = "lang-fsharp")]
pub static FSHARP_COMPLEXITY: ComplexityConfig = ComplexityConfig {
    branch_types: &["if_expression", "elif_expression", "match_expression"],
    loop_types: &["for_expression", "while_expression"],
    return_types: &[],
    nesting_types: &["sequential_expression"],
    unsafe_types: &[],
    unchecked_types: &[],
    unchecked_methods: &[],
    call_expression_types: &["application_expression"],
    call_method_field: "",
    assertion_names: &["Assert", "assertEqual", "assertTrue", "assertFalse"],
    macro_invocation_types: &[],
};
