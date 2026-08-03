use std::collections::{HashMap, HashSet};

use tracedecay::context::read_modes::{LineRange, ReadMode, estimate_tokens, render_lines};
use tracedecay::diagnose::parse_cargo_output;
use tracedecay::graph::{
    health::{HealthDimensions, compute_composite_health},
    scc,
};

#[test]
fn root_facades_match_extracted_operations() {
    assert_eq!(ReadMode::parse("lines"), Some(ReadMode::Lines));
    assert_eq!(
        ReadMode::parse("lines"),
        tracedecay_usecases::context::read_modes::ReadMode::parse("lines")
    );
    assert_eq!(
        render_lines("one\ntwo\nthree\n", LineRange { start: 2, end: 9 }),
        "two\nthree"
    );
    assert_eq!(estimate_tokens("abcdefgh"), 2);
    assert_eq!(
        tracedecay::context::read_cache::args_hash(&serde_json::json!({"a": 1})),
        tracedecay_usecases::context::read_cache::args_hash(&serde_json::json!({"a": 1}))
    );

    let parsed = parse_cargo_output("error[E0308]: mismatched types\n  --> src/lib.rs:42:10\n");
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].file, "src/lib.rs");

    let dimensions = HealthDimensions {
        acyclicity: 1.0,
        depth: 1.0,
        equality: 1.0,
        redundancy: 1.0,
        modularity: 1.0,
        coverage_discipline: 1.0,
    };
    assert_eq!(compute_composite_health(&dimensions), 10_000);

    let mut adjacency = HashMap::new();
    adjacency.insert("a", HashSet::from(["b"]));
    adjacency.insert("b", HashSet::from(["a"]));
    let components = scc::tarjan_scc(&adjacency);
    assert_eq!(components.len(), 1);
    assert!(scc::is_cyclic_scc(&components[0], &adjacency));

    let metrics = tracedecay::graph::NodeMetrics {
        incoming_edge_count: 1,
        outgoing_edge_count: 2,
        call_count: 1,
        caller_count: 1,
        child_count: 0,
        depth: 3,
    };
    let _: tracedecay_usecases::graph::queries::NodeMetrics = metrics;
}
