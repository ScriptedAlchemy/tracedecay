//! Node/file bulk insert, upsert, and payload-fidelity tests.

use super::*;

#[tokio::test]
async fn test_insert_all_bulk() {
    let db = setup_db().await;

    let nodes = vec![
        sample_node("bulk-1", "func_a", "src/a.rs"),
        sample_node("bulk-2", "func_b", "src/a.rs"),
        sample_node("bulk-3", "func_c", "src/b.rs"),
    ];

    let edges = vec![
        sample_edge("bulk-1", "bulk-2", EdgeKind::Calls),
        sample_edge("bulk-2", "bulk-3", EdgeKind::Uses),
    ];

    let files = vec![sample_file("src/a.rs"), sample_file("src/b.rs")];

    db.insert_all(&nodes, &edges, &files)
        .await
        .expect("insert_all failed");

    let all_nodes = db.get_all_nodes().await.expect("get_all_nodes failed");
    assert_eq!(all_nodes.len(), 3);

    let all_edges = db.get_all_edges().await.expect("get_all_edges failed");
    assert_eq!(all_edges.len(), 2);

    let all_files = db.get_all_files().await.expect("get_all_files failed");
    assert_eq!(all_files.len(), 2);
}

#[tokio::test]
async fn test_insert_all_sql_literal_helpers_preserve_payloads() {
    let db = setup_db().await;

    let payload = "quoted ' value; -- comment\nnext line */";
    let mut node = sample_node("bulk-sql-1", payload, "src/bulk'sql.rs");
    node.qualified_name = format!("crate::{payload}");
    node.docstring = Some(payload.to_string());
    node.signature = Some(format!("fn {payload}()"));

    let file = FileRecord {
        path: "src/bulk'sql.rs".to_string(),
        content_hash: payload.to_string(),
        size: 42,
        modified_at: 1000,
        indexed_at: 2000,
        node_count: 1,
    };

    db.insert_all(&[node], &[], &[file])
        .await
        .expect("insert_all should safely quote value literals");

    let nodes = db.get_all_nodes().await.expect("get_all_nodes failed");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].name, payload);
    assert_eq!(nodes[0].docstring.as_deref(), Some(payload));

    let files = db.get_all_files().await.expect("get_all_files failed");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].content_hash, payload);
}

#[tokio::test]
async fn test_upsert_files_batch() {
    let db = setup_db().await;

    let files = vec![
        sample_file("src/a.rs"),
        sample_file("src/b.rs"),
        sample_file("src/c.rs"),
    ];

    db.upsert_files(&files).await.expect("upsert_files failed");

    let all = db.get_all_files().await.expect("get_all_files failed");
    assert_eq!(all.len(), 3);

    // Verify upsert replaces existing
    let updated_files = vec![FileRecord {
        path: "src/a.rs".to_string(),
        content_hash: "new_hash".to_string(),
        size: 9999,
        modified_at: 5000,
        indexed_at: 6000,
        node_count: 99,
    }];

    db.upsert_files(&updated_files)
        .await
        .expect("upsert_files failed");

    let fetched = db
        .get_file("src/a.rs")
        .await
        .expect("get_file failed")
        .expect("file should exist");
    assert_eq!(fetched.content_hash, "new_hash");
    assert_eq!(fetched.size, 9999);
}

#[tokio::test]
async fn test_upsert_files_empty() {
    let db = setup_db().await;
    db.upsert_files(&[])
        .await
        .expect("upsert_files with empty slice should succeed");
}

#[tokio::test]
async fn test_delete_file() {
    let db = setup_db().await;

    let file = sample_file("src/target.rs");
    db.upsert_file(&file).await.expect("upsert_file failed");

    // Also insert a node so we verify cascading
    let node = sample_node("df-1", "fn_in_target", "src/target.rs");
    db.insert_node(&node).await.expect("insert_node failed");

    // Verify file exists before delete
    let before = db.get_file("src/target.rs").await.expect("get_file failed");
    assert!(before.is_some());

    db.delete_file("src/target.rs")
        .await
        .expect("delete_file failed");

    // File record should be gone
    let after = db.get_file("src/target.rs").await.expect("get_file failed");
    assert!(after.is_none());

    // Associated nodes should also be gone
    let nodes = db
        .get_nodes_by_file("src/target.rs")
        .await
        .expect("get_nodes_by_file failed");
    assert!(nodes.is_empty());
}

#[tokio::test]
async fn test_insert_all_comprehensive() {
    let db = setup_db().await;

    let nodes = vec![
        sample_node("ia-1", "alpha", "src/a.rs"),
        sample_node("ia-2", "beta", "src/a.rs"),
        sample_node("ia-3", "gamma", "src/b.rs"),
        sample_node("ia-4", "delta", "src/c.rs"),
    ];

    let edges = vec![
        sample_edge("ia-1", "ia-2", EdgeKind::Calls),
        sample_edge("ia-2", "ia-3", EdgeKind::Uses),
        sample_edge("ia-3", "ia-4", EdgeKind::Contains),
    ];

    let files = vec![
        sample_file("src/a.rs"),
        sample_file("src/b.rs"),
        sample_file("src/c.rs"),
    ];

    db.insert_all(&nodes, &edges, &files)
        .await
        .expect("insert_all failed");

    // Verify nodes
    let all_nodes = db.get_all_nodes().await.expect("get_all_nodes failed");
    assert_eq!(all_nodes.len(), 4);
    let node_ids: Vec<&str> = all_nodes.iter().map(|n| n.id.as_str()).collect();
    assert!(node_ids.contains(&"ia-1"));
    assert!(node_ids.contains(&"ia-4"));

    // Verify edges. The Contains edge ia-3 → ia-4 is denormalized into
    // ia-4's parent_id since v9, so the edges table holds only the two
    // remaining kinds.
    let all_edges = db.get_all_edges().await.expect("get_all_edges failed");
    assert_eq!(all_edges.len(), 2);
    let edge_pairs: Vec<(&str, &str)> = all_edges
        .iter()
        .map(|e| (e.source.as_str(), e.target.as_str()))
        .collect();
    assert!(edge_pairs.contains(&("ia-1", "ia-2")));
    assert!(edge_pairs.contains(&("ia-2", "ia-3")));
    let ia_4 = db
        .get_node_by_id("ia-4")
        .await
        .expect("get_node_by_id failed")
        .expect("ia-4 should exist");
    assert_eq!(ia_4.parent_id.as_deref(), Some("ia-3"));

    // Verify files
    let all_files = db.get_all_files().await.expect("get_all_files failed");
    assert_eq!(all_files.len(), 3);
    let file_paths: Vec<&str> = all_files.iter().map(|f| f.path.as_str()).collect();
    assert!(file_paths.contains(&"src/a.rs"));
    assert!(file_paths.contains(&"src/c.rs"));

    // Verify individual node retrieval
    let node = db
        .get_node_by_id("ia-2")
        .await
        .expect("get_node_by_id failed")
        .expect("node should exist");
    assert_eq!(node.name, "beta");
}

/// Regression test: a node whose signature contains non-UTF-8 bytes (e.g. from
/// a Latin-1 encoded source file) must not crash `row_to_node`. The lossy
/// fallback replaces invalid bytes with U+FFFD.
#[tokio::test]
async fn test_non_utf8_signature_does_not_crash() {
    let db = setup_db().await;

    // Insert a node with a BLOB signature containing 0xFF (invalid UTF-8)
    // via raw SQL — the Rust insert_node API only accepts valid Strings.
    db.execute_write(
        "insert non-UTF-8 signature fixture",
        "INSERT INTO nodes (id, kind, name, qualified_name, file_path, \
             start_line, end_line, start_column, end_column, \
             docstring, signature, visibility, is_async, \
             branches, loops, returns, max_nesting, \
             unsafe_blocks, unchecked_calls, assertions, updated_at) \
             VALUES (
                 'function:bad_utf8', 'function', 'render',
                 'src/view.cpp::render', 'src/view.cpp',
                 1, 10, 0, 50,
                 X'52656e6465727320746865207363e86e65207769746820a92065666665637473',
                 X'766f69642072656e64657228636f6e7374207374643a3a737472696e6726207363e86e6529',
                 'public', 0, 0, 0, 0, 0, 0, 0, 0, 0
             )",
        (),
    )
    .await
    .unwrap();

    // This used to fail with "invalid utf-8 sequence of 1 bytes from index N"
    let node = db.get_node_by_id("function:bad_utf8").await;
    assert!(
        node.is_ok(),
        "get_node_by_id should not fail on non-UTF-8: {:?}",
        node.err()
    );
    let node = node.unwrap();
    assert!(node.is_some(), "node should exist");
    let node = node.unwrap();
    assert_eq!(node.name, "render");
    // The invalid bytes are replaced with U+FFFD (replacement character)
    assert!(node.signature.is_some());
    assert!(node.docstring.is_some());
}
