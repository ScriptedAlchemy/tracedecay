//! The complexity walk is bounded by the nodes it actually visits and reports
//! a typed incomplete state when the bound stops it, instead of publishing the
//! counters of a partial walk as exact metrics.

use tracedecay_code_extraction::complexity::{
    RUST_COMPLEXITY, TRAVERSAL_BUDGET, count_complexity, count_complexity_bounded,
};
use tracedecay_code_extraction::{LanguageExtractor, RustExtractor};
use tracedecay_domain::{ComplexityAnalysisV1, NodeKind};
use tree_sitter::{Node, Parser, Tree};

fn parse_rust(source: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(
            &tracedecay_code_extraction::ts_provider::language("rust")
                .expect("bundled Rust grammar"),
        )
        .expect("configure Rust parser");
    parser.parse(source, None).expect("parse Rust source")
}

fn first_function(tree: &Tree) -> Node<'_> {
    let root = tree.root_node();
    let mut cursor = root.walk();
    root.children(&mut cursor)
        .find(|child| child.kind() == "function_item")
        .expect("fixture declares a function")
}

/// Every node below `node`, counted the same way the walk counts its visits.
fn descendant_count(node: Node<'_>) -> usize {
    let mut count = 0;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        count += 1 + descendant_count(child);
    }
    count
}

fn body_with_statements(statements: usize) -> String {
    let mut source = String::from("fn body(mut x: u64) -> u64 {\n");
    for _ in 0..statements {
        source.push_str("    x += 1;\n");
    }
    source.push_str("    if x > 3 { return x; }\n    x\n}\n");
    source
}

#[test]
fn budget_at_or_above_the_body_size_yields_complete_exact_metrics() {
    let source = body_with_statements(8);
    let tree = parse_rust(&source);
    let function = first_function(&tree);
    let nodes = descendant_count(function);

    let unbounded = count_complexity(function, &RUST_COMPLEXITY, source.as_bytes());
    let exact_boundary =
        count_complexity_bounded(function, &RUST_COMPLEXITY, source.as_bytes(), nodes);
    let generous =
        count_complexity_bounded(function, &RUST_COMPLEXITY, source.as_bytes(), nodes + 1);

    assert_eq!(unbounded.analysis, ComplexityAnalysisV1::Complete);
    assert_eq!(unbounded.branches, 1);
    assert_eq!(unbounded.returns, 1);
    assert_eq!(unbounded.max_nesting, 2);
    assert_eq!(
        exact_boundary, unbounded,
        "visiting exactly every node is complete"
    );
    assert_eq!(generous, unbounded);
}

#[test]
fn budget_below_the_body_size_is_reported_incomplete_with_visited_lower_bounds() {
    let source = body_with_statements(8);
    let tree = parse_rust(&source);
    let function = first_function(&tree);
    let nodes = descendant_count(function);

    let one_short =
        count_complexity_bounded(function, &RUST_COMPLEXITY, source.as_bytes(), nodes - 1);
    assert_eq!(
        one_short.analysis,
        ComplexityAnalysisV1::TraversalBudgetExhausted,
        "one unvisited node must not be reported as a complete analysis"
    );

    // The `if` sits at the end of the body: a walk cut off before it visits
    // the leading statements only and reports no branch or return.
    let early_cut = count_complexity_bounded(function, &RUST_COMPLEXITY, source.as_bytes(), 12);
    assert_eq!(
        early_cut.analysis,
        ComplexityAnalysisV1::TraversalBudgetExhausted
    );
    assert_eq!((early_cut.branches, early_cut.returns), (0, 0));
}

/// The bound counts visits, so a high-fanout body stops after `budget` nodes
/// even though its first level alone holds thousands of siblings.
#[test]
fn high_fanout_body_stops_after_the_budgeted_visits() {
    let source = body_with_statements(5_000);
    let tree = parse_rust(&source);
    let function = first_function(&tree);

    let metrics = count_complexity_bounded(function, &RUST_COMPLEXITY, source.as_bytes(), 64);
    assert_eq!(
        metrics.analysis,
        ComplexityAnalysisV1::TraversalBudgetExhausted
    );
    assert_eq!(
        (metrics.branches, metrics.returns),
        (0, 0),
        "64 visits cannot reach the branch after 5 000 statements"
    );
}

#[test]
fn deep_nesting_is_measured_rather_than_asserted_against() {
    let depth = 600;
    let mut source = String::from("fn deep() {\n");
    for _ in 0..depth {
        source.push('{');
    }
    source.push_str("let _x = 1;");
    for _ in 0..depth {
        source.push('}');
    }
    source.push_str("\n}\n");
    let tree = parse_rust(&source);
    let function = first_function(&tree);

    let metrics = count_complexity(function, &RUST_COMPLEXITY, source.as_bytes());
    assert_eq!(metrics.analysis, ComplexityAnalysisV1::Complete);
    assert_eq!(metrics.max_nesting, depth + 1);
}

/// The production budget reaches the extracted node: a body larger than the
/// budget yields an incomplete node, while ordinary bodies stay complete.
#[test]
fn extracted_nodes_carry_the_analysis_state() {
    let statements = TRAVERSAL_BUDGET / 4;
    let over_budget = body_with_statements(statements);
    let result = RustExtractor.extract("huge.rs", &over_budget);
    let body = result
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Function && node.name == "body")
        .expect("function node");
    assert_eq!(
        body.complexity_analysis,
        ComplexityAnalysisV1::TraversalBudgetExhausted
    );

    let small = RustExtractor.extract("small.rs", &body_with_statements(3));
    let body = small
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Function && node.name == "body")
        .expect("function node");
    assert_eq!(body.complexity_analysis, ComplexityAnalysisV1::Complete);
    assert_eq!((body.branches, body.returns), (1, 1));
}
