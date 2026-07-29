//! Edge insertion, deletion, and constraint tests.

use super::*;

#[tokio::test]
async fn test_insert_edges_batch() {
    let db = setup_db().await;

    let nodes: Vec<Node> = (0..4)
        .map(|i| sample_node(&format!("be-{i}"), &format!("f{i}"), "src/lib.rs"))
        .collect();
    db.insert_nodes(&nodes).await.expect("insert_nodes failed");

    let edges = vec![
        sample_edge("be-0", "be-1", EdgeKind::Calls),
        sample_edge("be-1", "be-2", EdgeKind::Uses),
        sample_edge("be-2", "be-3", EdgeKind::Contains),
    ];
    db.insert_edges(&edges).await.expect("insert_edges failed");

    // Contains is denormalized into nodes.parent_id since v9, so only the
    // two non-Contains edges actually land in the edges table.
    let all = db.get_all_edges().await.expect("get_all_edges failed");
    assert_eq!(all.len(), 2);
    let be_3 = db
        .get_node_by_id("be-3")
        .await
        .expect("get_node_by_id failed")
        .expect("be-3 should exist");
    assert_eq!(be_3.parent_id.as_deref(), Some("be-2"));
}

#[tokio::test]
async fn test_insert_edges_empty() {
    let db = setup_db().await;
    db.insert_edges(&[])
        .await
        .expect("insert_edges with empty slice should succeed");
    let all = db.get_all_edges().await.expect("get_all_edges failed");
    assert!(all.is_empty());
}

/// Both source and target are missing — edge must be silently skipped.
#[tokio::test]
async fn test_insert_edges_both_endpoints_missing() {
    let db = setup_db().await;

    let edges = vec![sample_edge("ghost-a", "ghost-b", EdgeKind::Calls)];
    db.insert_edges(&edges)
        .await
        .expect("insert_edges should not fail for missing endpoints");

    let all = db.get_all_edges().await.expect("get_all_edges failed");
    assert!(
        all.is_empty(),
        "edge with two missing endpoints must be skipped"
    );
}

/// Source exists but target is missing — edge must be skipped.
#[tokio::test]
async fn test_insert_edges_missing_target() {
    let db = setup_db().await;

    let node = sample_node("src-ok", "func_a", "src/lib.rs");
    db.insert_nodes(&[node]).await.expect("insert_nodes failed");

    let edges = vec![sample_edge("src-ok", "no-such-target", EdgeKind::Calls)];
    db.insert_edges(&edges)
        .await
        .expect("insert_edges should not fail for missing target");

    let all = db.get_all_edges().await.expect("get_all_edges failed");
    assert!(all.is_empty(), "edge with missing target must be skipped");
}

/// Target exists but source is missing — edge must be skipped.
#[tokio::test]
async fn test_insert_edges_missing_source() {
    let db = setup_db().await;

    let node = sample_node("tgt-ok", "func_b", "src/lib.rs");
    db.insert_nodes(&[node]).await.expect("insert_nodes failed");

    let edges = vec![sample_edge("no-such-source", "tgt-ok", EdgeKind::Uses)];
    db.insert_edges(&edges)
        .await
        .expect("insert_edges should not fail for missing source");

    let all = db.get_all_edges().await.expect("get_all_edges failed");
    assert!(all.is_empty(), "edge with missing source must be skipped");
}

/// Mixed batch: some edges valid, some with missing endpoints.
/// Valid edges must be inserted; invalid ones silently skipped.
#[tokio::test]
async fn test_insert_edges_mixed_valid_and_missing() {
    let db = setup_db().await;

    let nodes = vec![
        sample_node("mx-a", "fa", "src/a.rs"),
        sample_node("mx-b", "fb", "src/a.rs"),
        sample_node("mx-c", "fc", "src/b.rs"),
    ];
    db.insert_nodes(&nodes).await.expect("insert_nodes failed");

    let edges = vec![
        sample_edge("mx-a", "mx-b", EdgeKind::Calls),   // valid
        sample_edge("mx-a", "ghost-1", EdgeKind::Uses), // missing target
        sample_edge("ghost-2", "mx-c", EdgeKind::Contains), // missing source
        sample_edge("mx-b", "mx-c", EdgeKind::Calls),   // valid
        sample_edge("ghost-3", "ghost-4", EdgeKind::Uses), // both missing
    ];
    db.insert_edges(&edges)
        .await
        .expect("insert_edges should not fail for mixed batch");

    let all = db.get_all_edges().await.expect("get_all_edges failed");
    assert_eq!(
        all.len(),
        2,
        "only edges with both endpoints present must be inserted"
    );
}

/// Singular insert_edge also skips when target is missing.
#[tokio::test]
async fn test_insert_edge_singular_missing_target() {
    let db = setup_db().await;

    let node = sample_node("se-a", "fa", "src/lib.rs");
    db.insert_nodes(&[node]).await.expect("insert_nodes failed");

    let edge = sample_edge("se-a", "missing", EdgeKind::Calls);
    db.insert_edge(&edge)
        .await
        .expect("insert_edge should not fail for missing target");

    let all = db.get_all_edges().await.expect("get_all_edges failed");
    assert!(all.is_empty());
}

/// Singular insert_edge also skips when source is missing.
#[tokio::test]
async fn test_insert_edge_singular_missing_source() {
    let db = setup_db().await;

    let node = sample_node("se-b", "fb", "src/lib.rs");
    db.insert_nodes(&[node]).await.expect("insert_nodes failed");

    let edge = sample_edge("missing", "se-b", EdgeKind::Uses);
    db.insert_edge(&edge)
        .await
        .expect("insert_edge should not fail for missing source");

    let all = db.get_all_edges().await.expect("get_all_edges failed");
    assert!(all.is_empty());
}

/// Singular insert_edge works normally when both endpoints exist.
#[tokio::test]
async fn test_insert_edge_singular_valid() {
    let db = setup_db().await;

    let nodes = vec![
        sample_node("sv-a", "fa", "src/lib.rs"),
        sample_node("sv-b", "fb", "src/lib.rs"),
    ];
    db.insert_nodes(&nodes).await.expect("insert_nodes failed");

    let edge = sample_edge("sv-a", "sv-b", EdgeKind::Calls);
    db.insert_edge(&edge).await.expect("insert_edge failed");

    let all = db.get_all_edges().await.expect("get_all_edges failed");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].source, "sv-a");
    assert_eq!(all[0].target, "sv-b");
}

/// Duplicate edges (same source/target/kind) are ignored via INSERT OR IGNORE.
#[tokio::test]
async fn test_insert_edges_duplicate_ignored() {
    let db = setup_db().await;

    let nodes = vec![
        sample_node("dup-a", "fa", "src/lib.rs"),
        sample_node("dup-b", "fb", "src/lib.rs"),
    ];
    db.insert_nodes(&nodes).await.expect("insert_nodes failed");

    let edges = vec![
        sample_edge("dup-a", "dup-b", EdgeKind::Calls),
        sample_edge("dup-a", "dup-b", EdgeKind::Calls), // duplicate
    ];
    db.insert_edges(&edges).await.expect("insert_edges failed");

    let all = db.get_all_edges().await.expect("get_all_edges failed");
    assert_eq!(all.len(), 1, "duplicate edge must be ignored");
}

/// Cross-file edges inserted after all nodes are present succeed.
/// This simulates the incremental sync reordering fix.
#[tokio::test]
async fn test_insert_edges_cross_file_after_all_nodes() {
    let db = setup_db().await;

    // Simulate phase 1: insert nodes from two different files
    let nodes_file_a = vec![sample_node("cf-a1", "func_a", "src/a.rs")];
    let nodes_file_b = vec![sample_node("cf-b1", "func_b", "src/b.rs")];
    db.insert_nodes(&nodes_file_a)
        .await
        .expect("insert_nodes a failed");
    db.insert_nodes(&nodes_file_b)
        .await
        .expect("insert_nodes b failed");

    // Simulate phase 2: insert cross-file edges after all nodes are in
    let edges = vec![
        sample_edge("cf-a1", "cf-b1", EdgeKind::Calls), // a.rs -> b.rs
        sample_edge("cf-b1", "cf-a1", EdgeKind::Uses),  // b.rs -> a.rs
    ];
    db.insert_edges(&edges).await.expect("insert_edges failed");

    let all = db.get_all_edges().await.expect("get_all_edges failed");
    assert_eq!(
        all.len(),
        2,
        "cross-file edges must succeed when nodes are present"
    );
}

/// Large batch with many missing endpoints does not abort the transaction.
#[tokio::test]
async fn test_insert_edges_large_batch_with_missing() {
    let db = setup_db().await;

    // Only insert one node
    let node = sample_node("lb-0", "f0", "src/lib.rs");
    db.insert_nodes(&[node]).await.expect("insert_nodes failed");

    // Create 100 edges, all referencing missing targets
    let edges: Vec<Edge> = (0..100)
        .map(|i| sample_edge("lb-0", &format!("missing-{i}"), EdgeKind::Calls))
        .collect();
    db.insert_edges(&edges)
        .await
        .expect("insert_edges should not abort on large batch with missing endpoints");

    let all = db.get_all_edges().await.expect("get_all_edges failed");
    assert!(
        all.is_empty(),
        "all edges with missing targets must be skipped"
    );
}

/// Edges with None line values and missing endpoints are handled correctly.
#[tokio::test]
async fn test_insert_edges_null_line_with_missing() {
    let db = setup_db().await;

    let nodes = vec![
        sample_node("nl-a", "fa", "src/lib.rs"),
        sample_node("nl-b", "fb", "src/lib.rs"),
    ];
    db.insert_nodes(&nodes).await.expect("insert_nodes failed");

    let edges = vec![
        Edge {
            source: "nl-a".to_string(),
            target: "nl-b".to_string(),
            kind: EdgeKind::Calls,
            line: None, // valid, null line
        },
        Edge {
            source: "nl-a".to_string(),
            target: "missing".to_string(),
            kind: EdgeKind::Uses,
            line: None, // missing target, null line
        },
    ];
    db.insert_edges(&edges).await.expect("insert_edges failed");

    let all = db.get_all_edges().await.expect("get_all_edges failed");
    assert_eq!(all.len(), 1, "only the valid edge should be inserted");
    assert!(all[0].line.is_none());
}

#[tokio::test]
async fn test_delete_edges_by_source() {
    let db = setup_db().await;

    let nodes: Vec<Node> = ["ds-a", "ds-b", "ds-c"]
        .iter()
        .map(|id| sample_node(id, id, "src/lib.rs"))
        .collect();
    db.insert_nodes(&nodes).await.expect("insert_nodes failed");

    let edges = vec![
        sample_edge("ds-a", "ds-b", EdgeKind::Calls),
        sample_edge("ds-a", "ds-c", EdgeKind::Uses),
        sample_edge("ds-b", "ds-c", EdgeKind::Calls),
    ];
    db.insert_edges(&edges).await.expect("insert_edges failed");

    db.delete_edges_by_source("ds-a")
        .await
        .expect("delete_edges_by_source failed");

    let all = db.get_all_edges().await.expect("get_all_edges failed");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].source, "ds-b");
    assert_eq!(all[0].target, "ds-c");
}

#[tokio::test]
async fn test_edge_line_none_and_some() {
    let db = setup_db().await;

    let n1 = sample_node("eln-1", "f1", "src/lib.rs");
    let n2 = sample_node("eln-2", "f2", "src/lib.rs");
    db.insert_nodes(&[n1, n2])
        .await
        .expect("insert_nodes failed");

    // Insert edge with line = None
    let edge_no_line = Edge {
        source: "eln-1".to_string(),
        target: "eln-2".to_string(),
        kind: EdgeKind::Calls,
        line: None,
    };
    db.insert_edge(&edge_no_line)
        .await
        .expect("insert_edge failed");

    // Insert edge with line = Some(42)
    let edge_with_line = Edge {
        source: "eln-1".to_string(),
        target: "eln-2".to_string(),
        kind: EdgeKind::Calls,
        line: Some(42),
    };
    db.insert_edge(&edge_with_line)
        .await
        .expect("insert_edge failed");

    let all = db.get_all_edges().await.expect("get_all_edges failed");
    // Both should exist (unique constraint includes line)
    assert_eq!(all.len(), 2);

    let lines: Vec<Option<u32>> = all.iter().map(|e| e.line).collect();
    assert!(lines.contains(&None));
    assert!(lines.contains(&Some(42)));
}

#[tokio::test]
async fn test_edge_unique_constraint_dedup() {
    let db = setup_db().await;

    let n1 = sample_node("euc-1", "f1", "src/lib.rs");
    let n2 = sample_node("euc-2", "f2", "src/lib.rs");
    db.insert_nodes(&[n1, n2])
        .await
        .expect("insert_nodes failed");

    // Insert the exact same edge twice — should be deduplicated by INSERT OR IGNORE
    let edge = Edge {
        source: "euc-1".to_string(),
        target: "euc-2".to_string(),
        kind: EdgeKind::Calls,
        line: Some(10),
    };
    db.insert_edge(&edge).await.expect("insert_edge failed");
    db.insert_edge(&edge)
        .await
        .expect("second insert should not fail");

    let all = db.get_all_edges().await.expect("get_all_edges failed");
    assert_eq!(all.len(), 1, "duplicate edge should be ignored");
}
