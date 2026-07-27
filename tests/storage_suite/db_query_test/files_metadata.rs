//! File records, metadata keys, and test-annotation query tests (split from
//! `db_query_test.rs`).

use super::*;

#[tokio::test]
async fn test_get_all_files() {
    let db = setup_db().await;

    let files = vec![sample_file("src/a.rs"), sample_file("src/b.rs")];
    db.upsert_files(&files).await.expect("upsert_files failed");

    let all = db.get_all_files().await.expect("get_all_files failed");
    assert_eq!(all.len(), 2);
    let paths: Vec<&str> = all.iter().map(|f| f.path.as_str()).collect();
    assert!(paths.contains(&"src/a.rs"));
    assert!(paths.contains(&"src/b.rs"));
}

#[tokio::test]
async fn test_get_all_file_paths_reads_only_logical_paths() {
    let db = setup_db().await;
    let mut first = sample_file("src/a.rs");
    first.content_hash = "x".repeat(1024 * 1024);
    let mut second = sample_file("src/b.rs");
    second.content_hash = "y".repeat(1024 * 1024);
    db.upsert_files(&[first, second])
        .await
        .expect("upsert_files failed");

    let paths = db
        .get_all_file_paths()
        .await
        .expect("get_all_file_paths failed");

    assert_eq!(paths, vec!["src/a.rs", "src/b.rs"]);
}

#[tokio::test]
async fn test_last_index_time_empty() {
    let db = setup_db().await;

    let time = db.last_index_time().await.expect("last_index_time failed");
    assert_eq!(time, 0);
}

#[tokio::test]
async fn test_last_index_time_with_files() {
    let db = setup_db().await;

    let mut f1 = sample_file("src/a.rs");
    f1.indexed_at = 1000;

    let mut f2 = sample_file("src/b.rs");
    f2.indexed_at = 5000;

    let mut f3 = sample_file("src/c.rs");
    f3.indexed_at = 3000;

    db.upsert_files(&[f1, f2, f3])
        .await
        .expect("upsert_files failed");

    let time = db.last_index_time().await.expect("last_index_time failed");
    assert_eq!(time, 5000);
}

#[tokio::test]
async fn test_metadata_get_set() {
    let db = setup_db().await;

    // Non-existent key returns None
    let val = db
        .get_metadata("nonexistent")
        .await
        .expect("get_metadata failed");
    assert!(val.is_none());

    // Set and get
    db.set_metadata("my_key", "my_value")
        .await
        .expect("set_metadata failed");

    let val = db
        .get_metadata("my_key")
        .await
        .expect("get_metadata failed")
        .expect("metadata should exist");
    assert_eq!(val, "my_value");

    // Overwrite
    db.set_metadata("my_key", "updated_value")
        .await
        .expect("set_metadata failed");

    let val = db
        .get_metadata("my_key")
        .await
        .expect("get_metadata failed")
        .expect("metadata should exist");
    assert_eq!(val, "updated_value");
}

#[tokio::test]
async fn test_metadata_multiple_keys() {
    let db = setup_db().await;

    db.set_metadata("key1", "val1")
        .await
        .expect("set_metadata failed");
    db.set_metadata("key2", "val2")
        .await
        .expect("set_metadata failed");

    let v1 = db
        .get_metadata("key1")
        .await
        .expect("get_metadata failed")
        .expect("key1 should exist");
    let v2 = db
        .get_metadata("key2")
        .await
        .expect("get_metadata failed")
        .expect("key2 should exist");

    assert_eq!(v1, "val1");
    assert_eq!(v2, "val2");
}

#[tokio::test]
async fn test_get_test_annotated_node_ids_finds_test_functions() {
    let db = setup_db().await;

    // A source function and a test function in the same file.
    let src_fn = sample_node("fn_prod", "production_code", "src/lib.rs");
    let mut test_fn = sample_node("fn_test", "test_production_code", "src/lib.rs");
    test_fn.start_line = 20;

    // The #[test] annotation node.
    let mut annot = sample_node("annot_test", "test", "src/lib.rs");
    annot.kind = NodeKind::AnnotationUsage;
    annot.start_line = 19;
    annot.signature = Some("#[test]".to_string());

    db.insert_nodes(&[src_fn, test_fn, annot])
        .await
        .expect("insert_nodes failed");

    // Annotates edge: #[test] -> test function.
    let edge = sample_edge("annot_test", "fn_test", EdgeKind::Annotates);
    db.insert_edges(&[edge]).await.expect("insert_edges failed");

    // Query with both candidates; only the annotated one should be returned.
    let candidates = vec!["fn_prod".to_string(), "fn_test".to_string()];
    let result = db
        .get_test_annotated_node_ids(&candidates)
        .await
        .expect("query failed");
    assert_eq!(result.len(), 1);
    assert!(result.contains("fn_test"));
    assert!(!result.contains("fn_prod"));
}

#[tokio::test]
async fn test_get_test_annotated_node_ids_empty_input() {
    let db = setup_db().await;
    let result = db
        .get_test_annotated_node_ids(&[])
        .await
        .expect("should not fail on empty input");
    assert!(result.is_empty());
}

#[tokio::test]
async fn test_get_test_annotated_node_ids_chunks_large_candidate_sets() {
    let db = setup_db().await;

    let test_fn = sample_node("fn_test_large", "test_large_candidate_set", "src/lib.rs");
    let mut annot = sample_node("annot_test_large", "test", "src/lib.rs");
    annot.kind = NodeKind::AnnotationUsage;

    db.insert_nodes(&[test_fn, annot])
        .await
        .expect("insert_nodes failed");
    db.insert_edges(&[sample_edge(
        "annot_test_large",
        "fn_test_large",
        EdgeKind::Annotates,
    )])
    .await
    .expect("insert_edges failed");

    let mut candidates: Vec<String> = (0..33_000)
        .map(|i| format!("missing_candidate_{i}"))
        .collect();
    candidates.push("fn_test_large".to_string());

    let result = db
        .get_test_annotated_node_ids(&candidates)
        .await
        .expect("query should chunk candidates below SQLite variable limits");

    assert_eq!(result.len(), 1);
    assert!(result.contains("fn_test_large"));
}

#[tokio::test]
async fn test_get_files_with_test_annotations() {
    let db = setup_db().await;

    // Two files: one with inline tests, one without.
    let src_fn = sample_node("fn1", "foo", "src/lib.rs");
    let mut test_fn = sample_node("fn2", "test_foo", "src/lib.rs");
    test_fn.start_line = 30;
    let other_fn = sample_node("fn3", "bar", "src/other.rs");

    let mut annot = sample_node("annot1", "test", "src/lib.rs");
    annot.kind = NodeKind::AnnotationUsage;
    annot.start_line = 29;

    db.insert_nodes(&[src_fn, test_fn, other_fn, annot])
        .await
        .expect("insert_nodes failed");

    let edge = sample_edge("annot1", "fn2", EdgeKind::Annotates);
    db.insert_edges(&[edge]).await.expect("insert_edges failed");

    let result = db
        .get_files_with_test_annotations()
        .await
        .expect("query failed");
    assert!(result.contains("src/lib.rs"));
    assert!(!result.contains("src/other.rs"));
}
