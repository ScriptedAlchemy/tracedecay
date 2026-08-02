//! Ranking/analytics query tests (ranked nodes, largest, coupling, inheritance,
//! distribution, complexity, doc coverage, god classes, call counts).

use super::*;

#[tokio::test]
async fn test_get_ranked_nodes_by_edge_kind_incoming() {
    let db = setup_db().await;

    // Create target nodes that receive calls
    let target_a = sample_node("rt-a", "popular", "src/lib.rs");
    let target_b = sample_node("rt-b", "less_popular", "src/lib.rs");
    let caller1 = sample_node("rt-c1", "caller1", "src/lib.rs");
    let caller2 = sample_node("rt-c2", "caller2", "src/lib.rs");
    let caller3 = sample_node("rt-c3", "caller3", "src/lib.rs");

    db.insert_nodes(&[target_a, target_b, caller1, caller2, caller3])
        .await
        .expect("insert_nodes failed");

    // rt-a gets called by 3 callers, rt-b by 1
    let edges = vec![
        sample_edge("rt-c1", "rt-a", EdgeKind::Calls),
        sample_edge("rt-c2", "rt-a", EdgeKind::Calls),
        sample_edge("rt-c3", "rt-a", EdgeKind::Calls),
        sample_edge("rt-c1", "rt-b", EdgeKind::Calls),
    ];
    db.insert_edges(&edges).await.expect("insert_edges failed");

    let ranked = db
        .get_ranked_nodes_by_edge_kind(&EdgeKind::Calls, None, true, None, 10)
        .await
        .expect("get_ranked_nodes_by_edge_kind failed");

    assert_eq!(ranked.len(), 2);
    // Most called first
    assert_eq!(ranked[0].0.id, "rt-a");
    assert_eq!(ranked[0].1, 3);
    assert_eq!(ranked[1].0.id, "rt-b");
    assert_eq!(ranked[1].1, 1);
}

#[tokio::test]
async fn test_get_ranked_nodes_by_edge_kind_outgoing() {
    let db = setup_db().await;

    let caller = sample_node("ro-caller", "big_caller", "src/lib.rs");
    let target1 = sample_node("ro-t1", "t1", "src/lib.rs");
    let target2 = sample_node("ro-t2", "t2", "src/lib.rs");
    db.insert_nodes(&[caller, target1, target2])
        .await
        .expect("insert_nodes failed");

    let edges = vec![
        sample_edge("ro-caller", "ro-t1", EdgeKind::Calls),
        sample_edge("ro-caller", "ro-t2", EdgeKind::Calls),
    ];
    db.insert_edges(&edges).await.expect("insert_edges failed");

    let ranked = db
        .get_ranked_nodes_by_edge_kind(&EdgeKind::Calls, None, false, None, 10)
        .await
        .expect("get_ranked_nodes_by_edge_kind failed");

    assert!(!ranked.is_empty());
    assert_eq!(ranked[0].0.id, "ro-caller");
    assert_eq!(ranked[0].1, 2);
}

#[tokio::test]
async fn test_get_ranked_nodes_by_edge_kind_with_node_filter() {
    let db = setup_db().await;

    let mut func_node = sample_node("rnf-1", "func1", "src/lib.rs");
    func_node.kind = NodeKind::Function;

    let mut struct_node = sample_node("rnf-2", "MyStruct", "src/lib.rs");
    struct_node.kind = NodeKind::Struct;

    let caller = sample_node("rnf-c", "caller", "src/lib.rs");

    db.insert_nodes(&[func_node, struct_node, caller])
        .await
        .expect("insert_nodes failed");

    let edges = vec![
        sample_edge("rnf-c", "rnf-1", EdgeKind::Calls),
        sample_edge("rnf-c", "rnf-2", EdgeKind::Calls),
    ];
    db.insert_edges(&edges).await.expect("insert_edges failed");

    // Filter to only Function nodes
    let ranked = db
        .get_ranked_nodes_by_edge_kind(&EdgeKind::Calls, Some(&NodeKind::Function), true, None, 10)
        .await
        .expect("get_ranked_nodes_by_edge_kind failed");

    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].0.kind, NodeKind::Function);
}

#[tokio::test]
async fn test_get_largest_nodes() {
    let db = setup_db().await;

    let mut small = sample_node("ln-small", "small_fn", "src/lib.rs");
    small.start_line = 1;
    small.end_line = 5; // 5 lines

    let mut medium = sample_node("ln-medium", "medium_fn", "src/lib.rs");
    medium.start_line = 10;
    medium.end_line = 30; // 21 lines

    let mut large = sample_node("ln-large", "large_fn", "src/lib.rs");
    large.start_line = 50;
    large.end_line = 150; // 101 lines

    db.insert_nodes(&[small, medium, large])
        .await
        .expect("insert_nodes failed");

    let largest = db
        .get_largest_nodes(None, None, 10)
        .await
        .expect("get_largest_nodes failed");

    assert_eq!(largest.len(), 3);
    // Largest first
    assert_eq!(largest[0].0.id, "ln-large");
    assert_eq!(largest[0].1, 101);
    assert_eq!(largest[1].0.id, "ln-medium");
    assert_eq!(largest[1].1, 21);
    assert_eq!(largest[2].0.id, "ln-small");
    assert_eq!(largest[2].1, 5);
}

#[tokio::test]
async fn test_get_largest_nodes_with_kind_filter() {
    let db = setup_db().await;

    let mut func = sample_node("lk-func", "big_fn", "src/lib.rs");
    func.kind = NodeKind::Function;
    func.start_line = 1;
    func.end_line = 100;

    let mut strct = sample_node("lk-struct", "BigStruct", "src/lib.rs");
    strct.kind = NodeKind::Struct;
    strct.start_line = 1;
    strct.end_line = 200;

    db.insert_nodes(&[func, strct])
        .await
        .expect("insert_nodes failed");

    let largest = db
        .get_largest_nodes(Some(&NodeKind::Function), None, 10)
        .await
        .expect("get_largest_nodes failed");

    assert_eq!(largest.len(), 1);
    assert_eq!(largest[0].0.id, "lk-func");
}

#[tokio::test]
async fn test_get_largest_nodes_respects_limit() {
    let db = setup_db().await;

    let nodes: Vec<Node> = (0..10)
        .map(|i| {
            let mut n = sample_node(&format!("ll-{i}"), &format!("f{i}"), "src/lib.rs");
            n.start_line = 1;
            n.end_line = (i + 1) * 10;
            n
        })
        .collect();
    db.insert_nodes(&nodes).await.expect("insert_nodes failed");

    let largest = db
        .get_largest_nodes(None, None, 3)
        .await
        .expect("get_largest_nodes failed");

    assert_eq!(largest.len(), 3);
}

#[tokio::test]
async fn test_get_file_coupling_fan_in() {
    let db = setup_db().await;

    // Nodes in different files
    let n1 = sample_node("fc-1", "f1", "src/a.rs");
    let n2 = sample_node("fc-2", "f2", "src/b.rs");
    let n3 = sample_node("fc-3", "f3", "src/c.rs");
    let n4 = sample_node("fc-4", "f4", "src/a.rs");
    db.insert_nodes(&[n1, n2, n3, n4])
        .await
        .expect("insert_nodes failed");

    // Cross-file edges: b -> a, c -> a (a has fan-in of 2)
    let edges = vec![
        sample_edge("fc-2", "fc-1", EdgeKind::Calls),
        sample_edge("fc-3", "fc-4", EdgeKind::Uses),
    ];
    db.insert_edges(&edges).await.expect("insert_edges failed");

    let coupling = db
        .get_file_coupling(true, None, 10)
        .await
        .expect("get_file_coupling failed");

    // src/a.rs should have fan-in of 2 (from b and c)
    assert!(!coupling.is_empty());
    assert_eq!(coupling[0].0, "src/a.rs");
    assert_eq!(coupling[0].1, 2);
}

#[tokio::test]
async fn test_get_file_coupling_fan_out() {
    let db = setup_db().await;

    let n1 = sample_node("fco-1", "f1", "src/a.rs");
    let n2 = sample_node("fco-2", "f2", "src/b.rs");
    let n3 = sample_node("fco-3", "f3", "src/c.rs");
    db.insert_nodes(&[n1, n2, n3])
        .await
        .expect("insert_nodes failed");

    // a calls b and c => a has fan-out of 2
    let edges = vec![
        sample_edge("fco-1", "fco-2", EdgeKind::Calls),
        sample_edge("fco-1", "fco-3", EdgeKind::Uses),
    ];
    db.insert_edges(&edges).await.expect("insert_edges failed");

    let coupling = db
        .get_file_coupling(false, None, 10)
        .await
        .expect("get_file_coupling failed");

    assert!(!coupling.is_empty());
    assert_eq!(coupling[0].0, "src/a.rs");
    assert_eq!(coupling[0].1, 2);
}

#[tokio::test]
async fn test_get_file_coupling_binds_path_prefix() {
    let db = setup_db().await;

    let n1 = sample_node("fc-sql-1", "f1", "src/a.rs");
    let n2 = sample_node("fc-sql-2", "f2", "src/b.rs");
    db.insert_nodes(&[n1, n2])
        .await
        .expect("insert_nodes failed");
    db.insert_edges(&[sample_edge("fc-sql-2", "fc-sql-1", EdgeKind::Calls)])
        .await
        .expect("insert_edges failed");

    let coupling = db
        .get_file_coupling(true, Some("src/missing' OR 1=1 --"), 10)
        .await
        .expect("get_file_coupling should bind path_prefix");

    assert!(
        coupling.is_empty(),
        "quoted path_prefix must be treated as a literal prefix, not SQL"
    );
}

#[tokio::test]
async fn test_get_inheritance_depth() {
    let db = setup_db().await;

    // Create a chain: Child extends Parent extends GrandParent
    let mut grandparent = sample_node("ih-gp", "GrandParent", "src/lib.rs");
    grandparent.kind = NodeKind::Class;

    let mut parent = sample_node("ih-p", "Parent", "src/lib.rs");
    parent.kind = NodeKind::Class;

    let mut child = sample_node("ih-c", "Child", "src/lib.rs");
    child.kind = NodeKind::Class;

    db.insert_nodes(&[grandparent, parent, child])
        .await
        .expect("insert_nodes failed");

    let edges = vec![
        Edge {
            source: "ih-c".to_string(),
            target: "ih-p".to_string(),
            kind: EdgeKind::Extends,
            line: None,
        },
        Edge {
            source: "ih-p".to_string(),
            target: "ih-gp".to_string(),
            kind: EdgeKind::Extends,
            line: None,
        },
    ];
    db.insert_edges(&edges).await.expect("insert_edges failed");

    let depths = db
        .get_inheritance_depth(None, 10)
        .await
        .expect("get_inheritance_depth failed");

    // Child has depth 2 (Child -> Parent -> GrandParent)
    // Parent has depth 1 (Parent -> GrandParent)
    assert_eq!(depths.len(), 2);
    assert_eq!(depths[0].0.id, "ih-c");
    assert_eq!(depths[0].1, 2);
    assert_eq!(depths[1].0.id, "ih-p");
    assert_eq!(depths[1].1, 1);
}

#[tokio::test]
async fn test_get_inheritance_depth_binds_path_prefix() {
    let db = setup_db().await;

    let mut parent = sample_node("ih-sql-p", "Parent", "src/lib.rs");
    parent.kind = NodeKind::Class;
    let mut child = sample_node("ih-sql-c", "Child", "src/lib.rs");
    child.kind = NodeKind::Class;

    db.insert_nodes(&[parent, child])
        .await
        .expect("insert_nodes failed");
    db.insert_edges(&[sample_edge("ih-sql-c", "ih-sql-p", EdgeKind::Extends)])
        .await
        .expect("insert_edges failed");

    let depths = db
        .get_inheritance_depth(Some("src/missing' OR 1=1 --"), 10)
        .await
        .expect("get_inheritance_depth should bind path_prefix");

    assert!(
        depths.is_empty(),
        "quoted path_prefix must be treated as a literal prefix, not SQL"
    );
}

/// Regression: `get_inheritance_depth` previously had no cycle detection.
/// Rust trait bounds + generics can create supertrait cycles (or huge
/// near-cycles) — on the polkadot-sdk codebase (959 `extends` edges) the
/// recursive CTE would explode and never finish within 60 s. Test that
/// a 2-node cycle does not hang and reports finite depths.
#[tokio::test]
async fn test_get_inheritance_depth_terminates_on_cycle() {
    let db = setup_db().await;

    // A and B extend each other (cycle).
    let mut a = sample_node("ih-cy-a", "A", "src/lib.rs");
    a.kind = NodeKind::Trait;
    let mut b = sample_node("ih-cy-b", "B", "src/lib.rs");
    b.kind = NodeKind::Trait;
    // C extends A — should still be reported with finite depth despite A↔B cycle.
    let mut c = sample_node("ih-cy-c", "C", "src/lib.rs");
    c.kind = NodeKind::Trait;

    db.insert_nodes(&[a, b, c])
        .await
        .expect("insert_nodes failed");

    let edges = vec![
        Edge {
            source: "ih-cy-a".into(),
            target: "ih-cy-b".into(),
            kind: EdgeKind::Extends,
            line: None,
        },
        Edge {
            source: "ih-cy-b".into(),
            target: "ih-cy-a".into(),
            kind: EdgeKind::Extends,
            line: None,
        },
        Edge {
            source: "ih-cy-c".into(),
            target: "ih-cy-a".into(),
            kind: EdgeKind::Extends,
            line: None,
        },
    ];
    db.insert_edges(&edges).await.expect("insert_edges failed");

    let start = std::time::Instant::now();
    let depths = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        db.get_inheritance_depth(None, 10),
    )
    .await
    .expect("get_inheritance_depth must not hang on a cycle")
    .expect("get_inheritance_depth returned an error");
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "cycle case should be fast; took {:?}",
        elapsed
    );

    // All three nodes participate in the extends graph; each must appear
    // exactly once with a finite, bounded depth.
    assert_eq!(depths.len(), 3, "expected A, B, C each once");
    for (node, depth) in &depths {
        assert!(
            *depth < 50,
            "depth must be bounded for node {}: got {}",
            node.id,
            depth
        );
    }
}

#[tokio::test]
async fn test_get_node_distribution_no_prefix() {
    let db = setup_db().await;

    let mut n1 = sample_node("nd-1", "f1", "src/a.rs");
    n1.kind = NodeKind::Function;

    let mut n2 = sample_node("nd-2", "f2", "src/a.rs");
    n2.kind = NodeKind::Function;

    let mut n3 = sample_node("nd-3", "S1", "src/a.rs");
    n3.kind = NodeKind::Struct;

    let mut n4 = sample_node("nd-4", "f3", "src/b.rs");
    n4.kind = NodeKind::Function;

    db.insert_nodes(&[n1, n2, n3, n4])
        .await
        .expect("insert_nodes failed");

    let dist = db
        .get_node_distribution(None)
        .await
        .expect("get_node_distribution failed");

    // Should have entries for (src/a.rs, function, 2), (src/a.rs, struct, 1), (src/b.rs, function, 1)
    assert_eq!(dist.len(), 3);
}

#[tokio::test]
async fn test_get_node_distribution_with_prefix() {
    let db = setup_db().await;

    let mut n1 = sample_node("ndp-1", "f1", "src/a/foo.rs");
    n1.kind = NodeKind::Function;

    let mut n2 = sample_node("ndp-2", "f2", "src/b/bar.rs");
    n2.kind = NodeKind::Function;

    db.insert_nodes(&[n1, n2])
        .await
        .expect("insert_nodes failed");

    let dist = db
        .get_node_distribution(Some("src/a/"))
        .await
        .expect("get_node_distribution failed");

    assert_eq!(dist.len(), 1);
    assert_eq!(dist[0].0, "src/a/foo.rs");
}

#[tokio::test]
async fn test_get_call_edges() {
    let db = setup_db().await;

    let n1 = sample_node("ce-1", "f1", "src/lib.rs");
    let n2 = sample_node("ce-2", "f2", "src/lib.rs");
    let n3 = sample_node("ce-3", "f3", "src/lib.rs");
    db.insert_nodes(&[n1, n2, n3])
        .await
        .expect("insert_nodes failed");

    let edges = vec![
        sample_edge("ce-1", "ce-2", EdgeKind::Calls),
        sample_edge("ce-2", "ce-3", EdgeKind::Calls),
        sample_edge("ce-1", "ce-3", EdgeKind::Uses), // not a call edge
    ];
    db.insert_edges(&edges).await.expect("insert_edges failed");

    let call_edges = db
        .get_call_edges(None)
        .await
        .expect("get_call_edges failed");

    assert_eq!(call_edges.len(), 2);
    // Should only return calls edges
    let sources: Vec<&str> = call_edges.iter().map(|(s, _)| s.as_str()).collect();
    assert!(sources.contains(&"ce-1"));
    assert!(sources.contains(&"ce-2"));
}

#[tokio::test]
async fn test_get_complexity_ranked_no_filter() {
    let db = setup_db().await;

    // Returns (Node, lines, fan_out, fan_in, score)
    // score = lines + fan_out*3 + fan_in
    let mut n1 = sample_node("cx-1", "complex_fn", "src/lib.rs");
    n1.kind = NodeKind::Function;
    n1.start_line = 1;
    n1.end_line = 50; // 50 lines

    let mut n2 = sample_node("cx-2", "simple_fn", "src/lib.rs");
    n2.kind = NodeKind::Method;
    n2.start_line = 1;
    n2.end_line = 5; // 5 lines

    let mut target = sample_node("cx-t", "target", "src/lib.rs");
    target.kind = NodeKind::Struct; // Not function/method so it's excluded from default filter

    db.insert_nodes(&[n1, n2, target])
        .await
        .expect("insert_nodes failed");

    // cx-1 calls cx-t (fan_out = 1)
    let edges = vec![sample_edge("cx-1", "cx-t", EdgeKind::Calls)];
    db.insert_edges(&edges).await.expect("insert_edges failed");

    // No node_kind filter -> defaults to function + method
    let ranked = db
        .get_complexity_ranked(None, None, 10)
        .await
        .expect("get_complexity_ranked failed");

    assert_eq!(ranked.len(), 2);
    // cx-1: score = 50 + 1*3 + 0 = 53
    // cx-2: score = 5 + 0 + 0 = 5
    assert_eq!(ranked[0].0.id, "cx-1");
    assert_eq!(ranked[0].1, 50); // lines
    assert_eq!(ranked[0].2, 1); // fan_out
    assert_eq!(ranked[0].3, 0); // fan_in
    assert_eq!(ranked[0].4, 53); // score
}

#[tokio::test]
async fn test_get_complexity_ranked_with_filter() {
    let db = setup_db().await;

    let mut n1 = sample_node("cxf-1", "fn1", "src/lib.rs");
    n1.kind = NodeKind::Function;
    n1.start_line = 1;
    n1.end_line = 20;

    let mut n2 = sample_node("cxf-2", "method1", "src/lib.rs");
    n2.kind = NodeKind::Method;
    n2.start_line = 1;
    n2.end_line = 40;

    db.insert_nodes(&[n1, n2])
        .await
        .expect("insert_nodes failed");

    let ranked = db
        .get_complexity_ranked(Some(&NodeKind::Function), None, 10)
        .await
        .expect("get_complexity_ranked failed");

    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].0.kind, NodeKind::Function);
}

#[tokio::test]
async fn test_get_undocumented_public_symbols() {
    let db = setup_db().await;

    // Undocumented public function
    let mut undoc_pub = sample_node("udp-1", "undoc_fn", "src/lib.rs");
    undoc_pub.kind = NodeKind::Function;
    undoc_pub.visibility = Visibility::Pub;
    undoc_pub.docstring = None;

    // Documented public function
    let mut doc_pub = sample_node("udp-2", "doc_fn", "src/lib.rs");
    doc_pub.kind = NodeKind::Function;
    doc_pub.visibility = Visibility::Pub;
    doc_pub.docstring = Some("This is documented".to_string());

    // Undocumented private function (should not appear)
    let mut undoc_priv = sample_node("udp-3", "priv_fn", "src/lib.rs");
    undoc_priv.kind = NodeKind::Function;
    undoc_priv.visibility = Visibility::Private;
    undoc_priv.docstring = None;

    // Undocumented public struct
    let mut undoc_struct = sample_node("udp-4", "MyStruct", "src/lib.rs");
    undoc_struct.kind = NodeKind::Struct;
    undoc_struct.visibility = Visibility::Pub;
    undoc_struct.docstring = None;

    // Undocumented public with empty string docstring
    let mut undoc_empty = sample_node("udp-5", "empty_doc_fn", "src/lib.rs");
    undoc_empty.kind = NodeKind::Function;
    undoc_empty.visibility = Visibility::Pub;
    undoc_empty.docstring = Some(String::new());

    db.insert_nodes(&[undoc_pub, doc_pub, undoc_priv, undoc_struct, undoc_empty])
        .await
        .expect("insert_nodes failed");

    let undoc = db
        .get_undocumented_public_symbols(None, 100)
        .await
        .expect("get_undocumented_public_symbols failed");

    // Should include undoc_fn, MyStruct, empty_doc_fn but NOT doc_fn or priv_fn
    assert_eq!(undoc.len(), 3);
    let ids: Vec<&str> = undoc.iter().map(|n| n.id.as_str()).collect();
    assert!(ids.contains(&"udp-1"));
    assert!(ids.contains(&"udp-4"));
    assert!(ids.contains(&"udp-5"));
}

#[tokio::test]
async fn test_get_undocumented_public_symbols_with_prefix() {
    let db = setup_db().await;

    let mut n1 = sample_node("udpp-1", "f1", "src/a/foo.rs");
    n1.kind = NodeKind::Function;
    n1.visibility = Visibility::Pub;
    n1.docstring = None;

    let mut n2 = sample_node("udpp-2", "f2", "src/b/bar.rs");
    n2.kind = NodeKind::Function;
    n2.visibility = Visibility::Pub;
    n2.docstring = None;

    db.insert_nodes(&[n1, n2])
        .await
        .expect("insert_nodes failed");

    let undoc = db
        .get_undocumented_public_symbols(Some("src/a/"), 100)
        .await
        .expect("get_undocumented_public_symbols failed");

    assert_eq!(undoc.len(), 1);
    assert_eq!(undoc[0].file_path, "src/a/foo.rs");
}

/// Regression: doc_coverage previously excluded `field`, `enum_variant`,
/// `const`, `static`, and `type_alias`, so a Rust file full of `pub`
/// undocumented struct fields reported total_undocumented: 0. Those kinds
/// must be reported when public and undocumented.
#[tokio::test]
async fn test_get_undocumented_public_symbols_includes_fields_and_variants() {
    let db = setup_db().await;

    let mut field = sample_node("udpa-1", "freq", "src/lib.rs");
    field.kind = NodeKind::Field;
    field.visibility = Visibility::Pub;
    field.docstring = None;

    let mut variant = sample_node("udpa-2", "Lowpass", "src/lib.rs");
    variant.kind = NodeKind::EnumVariant;
    variant.visibility = Visibility::Pub;
    variant.docstring = None;

    let mut const_node = sample_node("udpa-3", "DEFAULT_Q", "src/lib.rs");
    const_node.kind = NodeKind::Const;
    const_node.visibility = Visibility::Pub;
    const_node.docstring = None;

    let mut static_node = sample_node("udpa-4", "GLOBAL", "src/lib.rs");
    static_node.kind = NodeKind::Static;
    static_node.visibility = Visibility::Pub;
    static_node.docstring = None;

    let mut alias = sample_node("udpa-5", "Peq", "src/lib.rs");
    alias.kind = NodeKind::TypeAlias;
    alias.visibility = Visibility::Pub;
    alias.docstring = None;

    // Documented field — should not appear.
    let mut documented = sample_node("udpa-6", "documented_field", "src/lib.rs");
    documented.kind = NodeKind::Field;
    documented.visibility = Visibility::Pub;
    documented.docstring = Some("good docs".to_string());

    db.insert_nodes(&[field, variant, const_node, static_node, alias, documented])
        .await
        .expect("insert_nodes failed");

    let undoc = db
        .get_undocumented_public_symbols(None, 100)
        .await
        .expect("get_undocumented_public_symbols failed");

    let ids: Vec<&str> = undoc.iter().map(|n| n.id.as_str()).collect();
    assert!(ids.contains(&"udpa-1"), "field should be reported");
    assert!(ids.contains(&"udpa-2"), "enum_variant should be reported");
    assert!(ids.contains(&"udpa-3"), "const should be reported");
    assert!(ids.contains(&"udpa-4"), "static should be reported");
    assert!(ids.contains(&"udpa-5"), "type_alias should be reported");
    assert!(
        !ids.contains(&"udpa-6"),
        "documented field must be filtered"
    );
}

#[tokio::test]
async fn test_get_god_classes() {
    let db = setup_db().await;

    // A struct with many contained members
    let mut class_node = sample_node("gc-class", "GodClass", "src/lib.rs");
    class_node.kind = NodeKind::Class;

    let mut method1 = sample_node("gc-m1", "method1", "src/lib.rs");
    method1.kind = NodeKind::Method;

    let mut method2 = sample_node("gc-m2", "method2", "src/lib.rs");
    method2.kind = NodeKind::Method;

    let mut field1 = sample_node("gc-f1", "field1", "src/lib.rs");
    field1.kind = NodeKind::Field;

    let mut constructor = sample_node("gc-ctor", "new", "src/lib.rs");
    constructor.kind = NodeKind::Constructor;

    db.insert_nodes(&[class_node, method1, method2, field1, constructor])
        .await
        .expect("insert_nodes failed");

    // "contains" edges from class to its members
    let edges = vec![
        Edge {
            source: "gc-class".to_string(),
            target: "gc-m1".to_string(),
            kind: EdgeKind::Contains,
            line: None,
        },
        Edge {
            source: "gc-class".to_string(),
            target: "gc-m2".to_string(),
            kind: EdgeKind::Contains,
            line: None,
        },
        Edge {
            source: "gc-class".to_string(),
            target: "gc-f1".to_string(),
            kind: EdgeKind::Contains,
            line: None,
        },
        Edge {
            source: "gc-class".to_string(),
            target: "gc-ctor".to_string(),
            kind: EdgeKind::Contains,
            line: None,
        },
    ];
    db.insert_edges(&edges).await.expect("insert_edges failed");

    let god_classes = db
        .get_god_classes(None, 10)
        .await
        .expect("get_god_classes failed");

    assert_eq!(god_classes.len(), 1);
    let (node, methods, fields, total) = &god_classes[0];
    assert_eq!(node.id, "gc-class");
    // methods: method1, method2, constructor = 3
    assert_eq!(*methods, 3);
    // fields: field1 = 1
    assert_eq!(*fields, 1);
    // total: 4
    assert_eq!(*total, 4);
}

#[tokio::test]
async fn test_get_god_classes_binds_path_prefix() {
    let db = setup_db().await;

    let mut class_node = sample_node("gc-sql-class", "GodClass", "src/lib.rs");
    class_node.kind = NodeKind::Class;
    let mut method = sample_node("gc-sql-method", "method", "src/lib.rs");
    method.kind = NodeKind::Method;

    db.insert_nodes(&[class_node, method])
        .await
        .expect("insert_nodes failed");
    db.insert_edges(&[sample_edge(
        "gc-sql-class",
        "gc-sql-method",
        EdgeKind::Contains,
    )])
    .await
    .expect("insert_edges failed");

    let god_classes = db
        .get_god_classes(Some("src/missing' OR 1=1 --"), 10)
        .await
        .expect("get_god_classes should bind path_prefix");

    assert!(
        god_classes.is_empty(),
        "quoted path_prefix must be treated as a literal prefix, not SQL"
    );
}

#[tokio::test]
async fn test_get_complexity_ranked_by_branches_and_nesting() {
    let db = setup_db().await;

    // High complexity: many branches + deep nesting + large body
    let mut complex = sample_node("cxb-1", "complex_fn", "src/lib.rs");
    complex.kind = NodeKind::Function;
    complex.start_line = 1;
    complex.end_line = 100;
    complex.branches = 15;
    complex.max_nesting = 5;
    complex.loops = 3;

    // Medium complexity
    let mut medium = sample_node("cxb-2", "medium_fn", "src/lib.rs");
    medium.kind = NodeKind::Function;
    medium.start_line = 200;
    medium.end_line = 230;
    medium.branches = 5;
    medium.max_nesting = 2;
    medium.loops = 1;

    // Simple: no branching
    let mut simple = sample_node("cxb-3", "simple_fn", "src/lib.rs");
    simple.kind = NodeKind::Function;
    simple.start_line = 300;
    simple.end_line = 305;
    simple.branches = 0;
    simple.max_nesting = 0;

    db.insert_nodes(&[complex, medium, simple])
        .await
        .expect("insert_nodes failed");

    let ranked = db
        .get_complexity_ranked(None, None, 10)
        .await
        .expect("get_complexity_ranked failed");

    assert_eq!(ranked.len(), 3);
    // Highest score first (100 lines > 31 lines > 6 lines, plus fan_out/fan_in = 0)
    assert_eq!(ranked[0].0.id, "cxb-1");
    assert_eq!(ranked[0].1, 100); // lines
    assert_eq!(ranked[1].0.id, "cxb-2");
    assert_eq!(ranked[2].0.id, "cxb-3");
}

#[tokio::test]
async fn test_get_god_classes_multiple_classes() {
    let db = setup_db().await;

    // Big class with many members
    let mut big_class = sample_node("gcm-big", "BigClass", "src/lib.rs");
    big_class.kind = NodeKind::Class;

    let mut m1 = sample_node("gcm-m1", "m1", "src/lib.rs");
    m1.kind = NodeKind::Method;
    let mut m2 = sample_node("gcm-m2", "m2", "src/lib.rs");
    m2.kind = NodeKind::Method;
    let mut m3 = sample_node("gcm-m3", "m3", "src/lib.rs");
    m3.kind = NodeKind::Method;
    let mut f1 = sample_node("gcm-f1", "field1", "src/lib.rs");
    f1.kind = NodeKind::Field;
    let mut f2 = sample_node("gcm-f2", "field2", "src/lib.rs");
    f2.kind = NodeKind::Field;

    // Small class with one member
    let mut small_class = sample_node("gcm-small", "SmallClass", "src/lib.rs");
    small_class.kind = NodeKind::Struct;

    let mut sm1 = sample_node("gcm-sm1", "sm_method", "src/lib.rs");
    sm1.kind = NodeKind::Method;

    db.insert_nodes(&[big_class, m1, m2, m3, f1, f2, small_class, sm1])
        .await
        .expect("insert_nodes failed");

    let edges = vec![
        Edge {
            source: "gcm-big".into(),
            target: "gcm-m1".into(),
            kind: EdgeKind::Contains,
            line: None,
        },
        Edge {
            source: "gcm-big".into(),
            target: "gcm-m2".into(),
            kind: EdgeKind::Contains,
            line: None,
        },
        Edge {
            source: "gcm-big".into(),
            target: "gcm-m3".into(),
            kind: EdgeKind::Contains,
            line: None,
        },
        Edge {
            source: "gcm-big".into(),
            target: "gcm-f1".into(),
            kind: EdgeKind::Contains,
            line: None,
        },
        Edge {
            source: "gcm-big".into(),
            target: "gcm-f2".into(),
            kind: EdgeKind::Contains,
            line: None,
        },
        Edge {
            source: "gcm-small".into(),
            target: "gcm-sm1".into(),
            kind: EdgeKind::Contains,
            line: None,
        },
    ];
    db.insert_edges(&edges).await.expect("insert_edges failed");

    let god = db
        .get_god_classes(None, 10)
        .await
        .expect("get_god_classes failed");

    // BigClass should be first (5 total), SmallClass second (1 total)
    assert_eq!(god.len(), 2);
    assert_eq!(god[0].0.id, "gcm-big");
    assert_eq!(god[0].0.name, "BigClass");
    assert_eq!(god[0].1, 3); // methods
    assert_eq!(god[0].2, 2); // fields
    assert_eq!(god[0].3, 5); // total

    assert_eq!(god[1].0.id, "gcm-small");
    assert_eq!(god[1].1, 1); // methods
    assert_eq!(god[1].2, 0); // fields
    assert_eq!(god[1].3, 1); // total
}

#[tokio::test]
async fn test_batch_incoming_call_counts() {
    let dir = tempfile::TempDir::new().unwrap();
    let (db, _) = crate::common::initialize_test_database(&dir.path().join("test.db"))
        .await
        .unwrap();

    let nodes = [("fn:a", "alpha"), ("fn:b", "beta"), ("fn:c", "gamma")]
        .into_iter()
        .map(|(id, name)| Node {
            id: id.to_string(),
            kind: NodeKind::Function,
            name: name.to_string(),
            qualified_name: format!("src/lib.rs::{name}"),
            file_path: "src/lib.rs".to_string(),
            start_line: 1,
            attrs_start_line: 1,
            end_line: 5,
            start_column: 0,
            end_column: 1,
            signature: None,
            docstring: None,
            visibility: Visibility::Pub,
            is_async: false,
            branches: 0,
            loops: 0,
            returns: 0,
            max_nesting: 0,
            unsafe_blocks: 0,
            unchecked_calls: 0,
            assertions: 0,
            updated_at: 0,
            parent_id: None,
        })
        .collect::<Vec<_>>();
    db.insert_nodes(&nodes).await.unwrap();

    // alpha has 2 callers, beta has 1, gamma has 0
    let edges = [("fn:b", "fn:a"), ("fn:c", "fn:a"), ("fn:c", "fn:b")]
        .into_iter()
        .map(|(src, tgt)| Edge {
            source: src.to_string(),
            target: tgt.to_string(),
            kind: EdgeKind::Calls,
            line: None,
        })
        .collect::<Vec<_>>();
    db.insert_edges(&edges).await.unwrap();

    let counts = db
        .batch_incoming_call_counts(&["fn:a".to_string(), "fn:b".to_string(), "fn:c".to_string()])
        .await
        .unwrap();
    assert_eq!(*counts.get("fn:a").unwrap_or(&0), 2);
    assert_eq!(*counts.get("fn:b").unwrap_or(&0), 1);
    assert_eq!(
        counts.get("fn:c"),
        None,
        "gamma has 0 callers so should be absent"
    );
}

// ---------------------------------------------------------------------------
// Health/gini SQL aggregate pushdown — byte-identical-to-Rust-fold proofs.
//
// Each test folds a fixture graph with the exact pre-pushdown Rust algorithm
// and asserts the SQL aggregate produces the identical result, so the health
// and gini reports keep their numeric output while dropping the whole-table
// `Vec<Node>` / `Vec<Edge>` materializations.
// ---------------------------------------------------------------------------

/// Installs the root-owned registered-schema port so `setup_db` can publish a
/// test profile runtime. The fail-closed port (added with the S11 local-storage
/// migration) refuses to open a shard until the root crate registers the
/// installer; the dashboard fixtures expose this idempotent seam for exactly
/// that. Only available with the `test-transport` feature (how CI builds this
/// suite); without it the shared harness cannot open a database at all.
#[cfg(feature = "test-transport")]
fn ensure_schema_installer() {
    tracedecay::dashboard::register_test_schema_installer();
}

#[cfg(not(feature = "test-transport"))]
fn ensure_schema_installer() {}

/// Builds a fresh, isolated graph database directly (no cached template), so
/// these proofs do not depend on the shared `setup_db` template/`!migrated`
/// assertion, which is entangled with the in-flight S11 storage migration.
async fn fresh_graph_db() -> (TempDir, Database) {
    ensure_schema_installer();
    let dir = TempDir::new().expect("failed to create temp dir");
    let path = dir.path().join("graph.db");
    let (db, _migrated) = crate::common::initialize_test_database(&path)
        .await
        .expect("failed to initialize fresh graph database");
    (dir, db)
}

/// Applies the exact `path_matches_scope` predicate used by the handlers.
fn in_scope(path: &str, prefix: Option<&str>) -> bool {
    match prefix {
        None => true,
        Some(p) => {
            let with_slash = if p.ends_with('/') {
                p.to_string()
            } else {
                format!("{p}/")
            };
            path.starts_with(&with_slash) || path == p
        }
    }
}

/// A varied fixture: several files, mixed kinds, non-trivial metric columns,
/// and `skip-test-coverage` markers on some function/method docstrings.
fn metric_fixture_nodes() -> Vec<Node> {
    let mut nodes = Vec::new();

    let mut set = |id: &str,
                   name: &str,
                   file: &str,
                   kind: NodeKind,
                   branches: u32,
                   loops: u32,
                   returns: u32,
                   max_nesting: u32,
                   start: u32,
                   end: u32,
                   parent: Option<&str>,
                   skip: bool| {
        let mut n = sample_node(id, name, file);
        n.kind = kind;
        n.branches = branches;
        n.loops = loops;
        n.returns = returns;
        n.max_nesting = max_nesting;
        n.start_line = start;
        n.end_line = end;
        n.parent_id = parent.map(str::to_string);
        n.docstring = if skip {
            Some(format!("does things // skip-test-coverage for {name}"))
        } else {
            Some(format!("Documentation for {name}"))
        };
        nodes.push(n);
    };

    // src/a.rs
    set(
        "a-f1",
        "alpha",
        "src/a.rs",
        NodeKind::Function,
        3,
        1,
        2,
        4,
        1,
        20,
        None,
        false,
    );
    set(
        "a-m1",
        "beta",
        "src/a.rs",
        NodeKind::Method,
        1,
        0,
        1,
        1,
        5,
        8,
        None,
        true,
    );
    set(
        "a-s1",
        "Widget",
        "src/a.rs",
        NodeKind::Struct,
        0,
        0,
        0,
        0,
        22,
        40,
        None,
        false,
    );

    // src/b.rs
    set(
        "b-f1",
        "gamma",
        "src/b.rs",
        NodeKind::Function,
        7,
        2,
        3,
        5,
        1,
        60,
        None,
        false,
    );
    set(
        "b-c1",
        "Service",
        "src/b.rs",
        NodeKind::Class,
        0,
        0,
        0,
        0,
        1,
        80,
        None,
        false,
    );
    set(
        "b-m1",
        "handle",
        "src/b.rs",
        NodeKind::Method,
        2,
        1,
        1,
        2,
        10,
        30,
        Some("b-c1"),
        false,
    );
    set(
        "b-m2",
        "start",
        "src/b.rs",
        NodeKind::Method,
        0,
        0,
        0,
        0,
        32,
        34,
        Some("b-c1"),
        true,
    );

    // src/nested/c.rs
    set(
        "c-f1",
        "delta",
        "src/nested/c.rs",
        NodeKind::Function,
        1,
        3,
        0,
        1,
        1,
        9,
        None,
        true,
    );

    nodes
}

fn fixture_edges() -> Vec<Edge> {
    // Mix of cross-file and same-file edges, varied kinds, and a duplicate
    // cross-file pair (two node pairs mapping to the same file pair).
    [
        ("a-f1", "b-f1", EdgeKind::Calls), // src/a.rs -> src/b.rs
        ("a-f1", "b-m1", EdgeKind::Uses),  // src/a.rs -> src/b.rs (same file pair)
        ("a-f1", "a-m1", EdgeKind::Calls), // same file (ignored)
        ("b-f1", "a-s1", EdgeKind::Uses),  // src/b.rs -> src/a.rs
        ("b-m1", "c-f1", EdgeKind::Calls), // src/b.rs -> src/nested/c.rs
        ("c-f1", "a-f1", EdgeKind::Calls), // src/nested/c.rs -> src/a.rs
    ]
    .into_iter()
    .map(|(s, t, k)| Edge {
        source: s.to_string(),
        target: t.to_string(),
        kind: k,
        line: Some(1),
    })
    .collect()
}

#[tokio::test]
async fn complexity_sum_by_file_matches_rust_fold() {
    let (_dir, db) = fresh_graph_db().await;
    let nodes = metric_fixture_nodes();
    db.insert_nodes(&nodes).await.expect("insert nodes");

    for prefix in [None, Some("src"), Some("src/nested")] {
        let mut expected: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        for n in nodes.iter().filter(|n| in_scope(&n.file_path, prefix)) {
            let c = f64::from(n.branches + n.loops + n.returns + n.max_nesting);
            *expected.entry(n.file_path.clone()).or_insert(0.0) += c;
        }
        let got: std::collections::HashMap<String, f64> = db
            .complexity_sum_by_file()
            .await
            .expect("complexity_sum_by_file")
            .into_iter()
            .filter(|(f, _)| in_scope(f, prefix))
            .collect();
        assert_eq!(got, expected, "complexity mismatch for prefix {prefix:?}");
    }
}

#[tokio::test]
async fn line_span_sum_by_file_matches_rust_fold() {
    let (_dir, db) = fresh_graph_db().await;
    let nodes = metric_fixture_nodes();
    db.insert_nodes(&nodes).await.expect("insert nodes");

    let mut expected: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for n in &nodes {
        let lines = f64::from(n.end_line.saturating_sub(n.start_line) + 1);
        *expected.entry(n.file_path.clone()).or_insert(0.0) += lines;
    }
    let got: std::collections::HashMap<String, f64> = db
        .line_span_sum_by_file()
        .await
        .expect("line_span_sum_by_file")
        .into_iter()
        .collect();
    assert_eq!(got, expected);
}

#[tokio::test]
async fn health_file_aggregates_match_rust_fold() {
    let (_dir, db) = fresh_graph_db().await;
    let nodes = metric_fixture_nodes();
    db.insert_nodes(&nodes).await.expect("insert nodes");

    // Old snapshot fold, verbatim.
    let mut per_file_complexity: std::collections::HashMap<String, f64> =
        std::collections::HashMap::new();
    for n in &nodes {
        let c = f64::from(n.branches) * 2.0
            + f64::from(n.loops) * 2.0
            + f64::from(n.max_nesting) * 3.0
            + f64::from(n.end_line.saturating_sub(n.start_line) + 1);
        *per_file_complexity
            .entry(n.file_path.clone())
            .or_insert(0.0) += c;
    }
    let total_fns = nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
        .count();
    let skip_ids: std::collections::HashSet<&str> = nodes
        .iter()
        .filter(|n| {
            n.docstring
                .as_deref()
                .is_some_and(|d| d.contains("skip-test-coverage"))
        })
        .map(|n| n.id.as_str())
        .collect();
    let skipped = nodes
        .iter()
        .filter(|n| {
            matches!(n.kind, NodeKind::Function | NodeKind::Method)
                && skip_ids.contains(n.id.as_str())
        })
        .count();

    let aggs = db
        .health_file_aggregates()
        .await
        .expect("health_file_aggregates");
    let got_complexity: std::collections::HashMap<String, f64> = aggs
        .iter()
        .map(|a| (a.file_path.clone(), a.complexity))
        .collect();
    let got_total_fns: usize = aggs.iter().map(|a| a.function_methods).sum();
    let got_skipped: usize = aggs.iter().map(|a| a.skipped_function_methods).sum();

    assert_eq!(got_complexity, per_file_complexity, "weighted complexity");
    assert_eq!(got_total_fns, total_fns, "function/method count");
    assert_eq!(got_skipped, skipped, "skip-test-coverage count");
}

#[tokio::test]
async fn cross_file_fan_matches_rust_fold() {
    let (_dir, db) = fresh_graph_db().await;
    let nodes = metric_fixture_nodes();
    let edges = fixture_edges();
    db.insert_nodes(&nodes).await.expect("insert nodes");
    db.insert_edges(&edges).await.expect("insert edges");

    for &fan_in in &[true, false] {
        // Old fold: node -> file map + whole-edge-table walk.
        let node_to_file: std::collections::HashMap<String, String> = nodes
            .iter()
            .map(|n| (n.id.clone(), n.file_path.clone()))
            .collect();
        let mut expected: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        for n in &nodes {
            expected.entry(n.file_path.clone()).or_insert(0.0);
        }
        for e in &edges {
            if let (Some(sf), Some(tf)) = (node_to_file.get(&e.source), node_to_file.get(&e.target))
                && sf != tf
            {
                let key = if fan_in { tf.clone() } else { sf.clone() };
                *expected.entry(key).or_insert(0.0) += 1.0;
            }
        }

        // New: SQL pair counts + distinct node files.
        let mut got: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        for file in db.distinct_node_file_paths().await.expect("files") {
            got.entry(file).or_insert(0.0);
        }
        for (src, tgt, count) in db.cross_file_edge_pair_counts().await.expect("pair counts") {
            if src != tgt {
                let key = if fan_in { tgt } else { src };
                *got.entry(key).or_insert(0.0) += count as f64;
            }
        }

        assert_eq!(got, expected, "fan_in={fan_in}");
    }
}

#[tokio::test]
async fn symbol_complexity_matches_rust_fold() {
    let (_dir, db) = fresh_graph_db().await;
    let nodes = metric_fixture_nodes();
    db.insert_nodes(&nodes).await.expect("insert nodes");

    // Old symbol-scope projection, in insertion (rowid) order.
    let expected: Vec<(String, f64)> = nodes
        .iter()
        .filter(|n| matches!(n.kind, NodeKind::Function | NodeKind::Method))
        .map(|n| {
            let c = f64::from(n.branches + n.loops + n.returns + n.max_nesting);
            (format!("{}:{}", n.file_path, n.name), c)
        })
        .collect();

    let got: Vec<(String, f64)> = db
        .symbol_complexity()
        .await
        .expect("symbol_complexity")
        .into_iter()
        .map(|(file, name, value)| (format!("{file}:{name}"), value))
        .collect();

    assert_eq!(got, expected);
}

/// Measures the health-aggregate pushdown on a multi-thousand-node graph and
/// proves the structural allocation win: the SQL path returns one row per file
/// (a few hundred) instead of the whole `Vec<Node>` (several thousand `Node`
/// structs, each with six owned `String`s). Also confirms the two paths agree.
/// Run with `--nocapture` to see the before/after timings.
#[tokio::test]
async fn health_aggregate_pushdown_avoids_whole_table_fold() {
    use std::collections::HashMap;
    use std::time::Instant;

    let (_dir, db) = fresh_graph_db().await;

    const FILES: usize = 250;
    const PER_FILE: usize = 16; // 4000 nodes total
    let mut nodes = Vec::with_capacity(FILES * PER_FILE);
    for f in 0..FILES {
        for i in 0..PER_FILE {
            let file = format!("src/pkg{:03}/mod{:03}.rs", f / 10, f);
            let mut n = sample_node(&format!("n-{f}-{i}"), &format!("fn_{f}_{i}"), &file);
            n.kind = if i % 4 == 0 {
                NodeKind::Method
            } else {
                NodeKind::Function
            };
            n.branches = (i as u32) % 5;
            n.loops = (i as u32) % 3;
            n.returns = (i as u32) % 2;
            n.max_nesting = (i as u32) % 4;
            n.start_line = (i as u32) * 10 + 1;
            n.end_line = (i as u32) * 10 + 9;
            n.docstring = if i % 8 == 0 {
                Some(format!("// skip-test-coverage fn_{f}_{i}"))
            } else {
                Some(format!("doc {f}/{i}"))
            };
            nodes.push(n);
        }
    }
    db.insert_nodes(&nodes).await.expect("insert nodes");

    // Old path: materialize the whole node table, fold in Rust.
    let old_start = Instant::now();
    let all_nodes = db.get_all_nodes().await.expect("get_all_nodes");
    let mut per_file: HashMap<String, f64> = HashMap::new();
    let mut old_fns = 0usize;
    let mut old_skipped = 0usize;
    for n in &all_nodes {
        let c = f64::from(n.branches) * 2.0
            + f64::from(n.loops) * 2.0
            + f64::from(n.max_nesting) * 3.0
            + f64::from(n.end_line.saturating_sub(n.start_line) + 1);
        *per_file.entry(n.file_path.clone()).or_insert(0.0) += c;
        if matches!(n.kind, NodeKind::Function | NodeKind::Method) {
            old_fns += 1;
            if n.docstring
                .as_deref()
                .is_some_and(|d| d.contains("skip-test-coverage"))
            {
                old_skipped += 1;
            }
        }
    }
    let old_nodes_materialized = all_nodes.len();
    let old_elapsed = old_start.elapsed();

    // New path: fold inside SQLite, one row per file.
    let new_start = Instant::now();
    let aggregates = db
        .health_file_aggregates()
        .await
        .expect("health_file_aggregates");
    let new_per_file: HashMap<String, f64> = aggregates
        .iter()
        .map(|a| (a.file_path.clone(), a.complexity))
        .collect();
    let new_fns: usize = aggregates.iter().map(|a| a.function_methods).sum();
    let new_skipped: usize = aggregates.iter().map(|a| a.skipped_function_methods).sum();
    let new_rows_materialized = aggregates.len();
    let new_elapsed = new_start.elapsed();

    // Byte-identical results.
    assert_eq!(new_per_file, per_file);
    assert_eq!(new_fns, old_fns);
    assert_eq!(new_skipped, old_skipped);

    // Structural allocation win: SQL returns one row per file, not per node,
    // and never builds a `Vec<Node>`.
    assert_eq!(new_rows_materialized, FILES);
    assert_eq!(old_nodes_materialized, FILES * PER_FILE);
    assert!(
        new_rows_materialized * 4 < old_nodes_materialized,
        "SQL aggregate must materialize far fewer rows than the node fold"
    );

    eprintln!(
        "[health_aggregate_pushdown] nodes={} files={} | old(get_all_nodes+fold): {:?} materializing {} Node structs | new(SQL GROUP BY): {:?} materializing {} file rows",
        nodes.len(),
        FILES,
        old_elapsed,
        old_nodes_materialized,
        new_elapsed,
        new_rows_materialized,
    );
}
