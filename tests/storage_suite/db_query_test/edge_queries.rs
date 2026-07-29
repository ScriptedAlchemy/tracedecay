//! Edge traversal query tests (internal edges, incoming-edge bulk, unresolved
//! refs).

use super::*;

#[tokio::test]
async fn test_get_internal_edges() {
    let db = setup_db().await;

    let n1 = sample_node("ie-1", "f1", "src/lib.rs");
    let n2 = sample_node("ie-2", "f2", "src/lib.rs");
    let n3 = sample_node("ie-3", "f3", "src/lib.rs");
    let n4 = sample_node("ie-4", "f4", "src/lib.rs"); // outside the subset
    db.insert_nodes(&[n1, n2, n3, n4])
        .await
        .expect("insert_nodes failed");

    let edges = vec![
        sample_edge("ie-1", "ie-2", EdgeKind::Calls), // internal
        sample_edge("ie-2", "ie-3", EdgeKind::Calls), // internal
        sample_edge("ie-1", "ie-4", EdgeKind::Calls), // external (target not in subset)
        sample_edge("ie-4", "ie-1", EdgeKind::Calls), // external (source not in subset)
    ];
    db.insert_edges(&edges).await.expect("insert_edges failed");

    let subset = vec!["ie-1".to_string(), "ie-2".to_string(), "ie-3".to_string()];

    let internal = db
        .get_internal_edges(&subset)
        .await
        .expect("get_internal_edges failed");

    assert_eq!(internal.len(), 2);
    let pairs: Vec<(&str, &str)> = internal
        .iter()
        .map(|e| (e.source.as_str(), e.target.as_str()))
        .collect();
    assert!(pairs.contains(&("ie-1", "ie-2")));
    assert!(pairs.contains(&("ie-2", "ie-3")));
}

#[tokio::test]
async fn test_get_internal_edges_empty() {
    let db = setup_db().await;

    let result = db
        .get_internal_edges(&[])
        .await
        .expect("get_internal_edges failed");
    assert!(result.is_empty());
}

#[tokio::test]
async fn test_insert_unresolved_refs_batch() {
    let db = setup_db().await;

    let node = sample_node("ur-node", "my_func", "src/lib.rs");
    db.insert_node(&node).await.expect("insert_node failed");

    let refs = vec![
        UnresolvedRef {
            from_node_id: "ur-node".to_string(),
            reference_name: "HashMap".to_string(),
            reference_kind: EdgeKind::Uses,
            line: 10,
            column: 5,
            file_path: "src/lib.rs".to_string(),
        },
        UnresolvedRef {
            from_node_id: "ur-node".to_string(),
            reference_name: "Vec".to_string(),
            reference_kind: EdgeKind::Uses,
            line: 15,
            column: 10,
            file_path: "src/lib.rs".to_string(),
        },
        UnresolvedRef {
            from_node_id: "ur-node".to_string(),
            reference_name: "other_fn".to_string(),
            reference_kind: EdgeKind::Calls,
            line: 20,
            column: 0,
            file_path: "src/lib.rs".to_string(),
        },
    ];

    db.insert_unresolved_refs(&refs)
        .await
        .expect("insert_unresolved_refs failed");

    let fetched = db
        .get_unresolved_refs()
        .await
        .expect("get_unresolved_refs failed");
    assert_eq!(fetched.len(), 3);
}

#[tokio::test]
async fn test_insert_unresolved_refs_empty() {
    let db = setup_db().await;
    db.insert_unresolved_refs(&[])
        .await
        .expect("insert_unresolved_refs with empty slice should succeed");
}

#[tokio::test]
async fn test_get_internal_edges_larger_set() {
    let db = setup_db().await;

    // Create 10 nodes
    let nodes: Vec<Node> = (0..10)
        .map(|i| sample_node(&format!("iel-{i}"), &format!("fn_{i}"), "src/lib.rs"))
        .collect();
    db.insert_nodes(&nodes).await.expect("insert_nodes failed");

    // Create a chain of calls: 0->1->2->...->9, plus some edges to nodes outside subset
    let mut edges = Vec::new();
    for i in 0..9 {
        edges.push(sample_edge(
            &format!("iel-{i}"),
            &format!("iel-{}", i + 1),
            EdgeKind::Calls,
        ));
    }
    db.insert_edges(&edges).await.expect("insert_edges failed");

    // Subset: nodes 0-4 (5 nodes) — should have 4 internal edges (0->1, 1->2, 2->3, 3->4)
    let subset: Vec<String> = (0..5).map(|i| format!("iel-{i}")).collect();
    let internal = db
        .get_internal_edges(&subset)
        .await
        .expect("get_internal_edges failed");

    assert_eq!(internal.len(), 4);

    // Edge 4->5 should NOT be in internal because 5 is not in subset
    let has_external = internal.iter().any(|e| e.target == "iel-5");
    assert!(
        !has_external,
        "edge to node outside subset should be excluded"
    );
}

#[tokio::test]
async fn test_get_incoming_edges_bulk_returns_all_targets() {
    let db = setup_db().await;

    let nodes = vec![
        sample_node("caller_a", "caller_a", "src/lib.rs"),
        sample_node("caller_b", "caller_b", "src/lib.rs"),
        sample_node("target_x", "target_x", "src/lib.rs"),
        sample_node("target_y", "target_y", "src/lib.rs"),
        sample_node("isolated", "isolated", "src/lib.rs"),
    ];
    db.insert_nodes(&nodes).await.expect("insert_nodes failed");

    let edges = vec![
        sample_edge("caller_a", "target_x", EdgeKind::Calls),
        sample_edge("caller_b", "target_x", EdgeKind::Calls),
        sample_edge("caller_a", "target_y", EdgeKind::Calls),
        sample_edge("caller_a", "isolated", EdgeKind::Uses),
    ];
    db.insert_edges(&edges).await.expect("insert_edges failed");

    let target_ids = vec!["target_x".to_string(), "target_y".to_string()];
    let result = db
        .get_incoming_edges_bulk(&target_ids, &[EdgeKind::Calls])
        .await
        .expect("get_incoming_edges_bulk failed");

    // 2 callers of target_x + 1 caller of target_y = 3 edges via Calls.
    assert_eq!(result.len(), 3);
    assert!(result.iter().all(|e| e.kind == EdgeKind::Calls));
    assert!(
        result
            .iter()
            .any(|e| e.target == "target_x" && e.source == "caller_a")
    );
    assert!(
        result
            .iter()
            .any(|e| e.target == "target_x" && e.source == "caller_b")
    );
    assert!(
        result
            .iter()
            .any(|e| e.target == "target_y" && e.source == "caller_a")
    );
}

#[tokio::test]
async fn test_get_incoming_edges_bulk_empty_kinds_returns_all_kinds() {
    let db = setup_db().await;

    let nodes = vec![
        sample_node("a", "a", "src/lib.rs"),
        sample_node("b", "b", "src/lib.rs"),
    ];
    db.insert_nodes(&nodes).await.expect("insert_nodes failed");

    let edges = vec![
        sample_edge("a", "b", EdgeKind::Calls),
        sample_edge("a", "b", EdgeKind::Uses),
    ];
    db.insert_edges(&edges).await.expect("insert_edges failed");

    let result = db
        .get_incoming_edges_bulk(&["b".to_string()], &[])
        .await
        .expect("get_incoming_edges_bulk failed");

    // Empty kinds should return both edges.
    assert_eq!(result.len(), 2);
}

#[tokio::test]
async fn test_get_incoming_edges_bulk_empty_input() {
    let db = setup_db().await;
    let result = db
        .get_incoming_edges_bulk(&[], &[])
        .await
        .expect("should not fail on empty input");
    assert!(result.is_empty());
}
