//! Node lookup, directory, qualified-name, and search-ranking query tests.

use super::*;

#[tokio::test]
async fn test_get_nodes_by_kind() {
    let db = setup_db().await;

    let mut func_node = sample_node("n1", "my_func", "src/lib.rs");
    func_node.kind = NodeKind::Function;

    let mut struct_node = sample_node("n2", "MyStruct", "src/lib.rs");
    struct_node.kind = NodeKind::Struct;

    let mut method_node = sample_node("n3", "my_method", "src/lib.rs");
    method_node.kind = NodeKind::Method;

    let mut func_node2 = sample_node("n4", "other_func", "src/other.rs");
    func_node2.kind = NodeKind::Function;

    db.insert_nodes(&[func_node, struct_node, method_node, func_node2])
        .await
        .expect("insert_nodes failed");

    let functions = db
        .get_nodes_by_kind(NodeKind::Function)
        .await
        .expect("get_nodes_by_kind failed");
    assert_eq!(functions.len(), 2);
    assert!(functions.iter().all(|n| n.kind == NodeKind::Function));

    let structs = db
        .get_nodes_by_kind(NodeKind::Struct)
        .await
        .expect("get_nodes_by_kind failed");
    assert_eq!(structs.len(), 1);
    assert_eq!(structs[0].name, "MyStruct");

    let methods = db
        .get_nodes_by_kind(NodeKind::Method)
        .await
        .expect("get_nodes_by_kind failed");
    assert_eq!(methods.len(), 1);
    assert_eq!(methods[0].name, "my_method");

    let traits = db
        .get_nodes_by_kind(NodeKind::Trait)
        .await
        .expect("get_nodes_by_kind failed");
    assert!(traits.is_empty());
}

#[tokio::test]
async fn test_get_all_nodes() {
    let db = setup_db().await;

    let nodes: Vec<Node> = (0..5)
        .map(|i| sample_node(&format!("all-{i}"), &format!("func_{i}"), "src/lib.rs"))
        .collect();

    db.insert_nodes(&nodes).await.expect("insert_nodes failed");

    let all = db.get_all_nodes().await.expect("get_all_nodes failed");
    assert_eq!(all.len(), 5);
}

/// The SQLite runtime refuses to materialize a whole-table read of this size in
/// one query, so `get_all_nodes` has to page. The scan must still be complete.
#[tokio::test]
async fn test_get_all_nodes_pages_beyond_runtime_materialization_limit() {
    const NODES: usize = 10_001;
    let db = setup_db().await;

    let nodes: Vec<Node> = (0..NODES)
        .map(|i| sample_node(&format!("paged-{i:06}"), &format!("func_{i}"), "src/lib.rs"))
        .collect();
    db.insert_nodes(&nodes).await.expect("insert_nodes failed");

    let all = db.get_all_nodes().await.expect("get_all_nodes failed");

    assert_eq!(all.len(), NODES);
    let ids: std::collections::HashSet<&str> = all.iter().map(|node| node.id.as_str()).collect();
    assert_eq!(ids.len(), NODES, "paging must not duplicate or drop nodes");
}

#[tokio::test]
async fn test_get_all_edges() {
    let db = setup_db().await;

    let n1 = sample_node("ea", "fa", "src/lib.rs");
    let n2 = sample_node("eb", "fb", "src/lib.rs");
    let n3 = sample_node("ec", "fc", "src/lib.rs");
    db.insert_nodes(&[n1, n2, n3])
        .await
        .expect("insert_nodes failed");

    let e1 = sample_edge("ea", "eb", EdgeKind::Calls);
    let e2 = sample_edge("eb", "ec", EdgeKind::Uses);
    db.insert_edge(&e1).await.expect("insert_edge failed");
    db.insert_edge(&e2).await.expect("insert_edge failed");

    let all = db.get_all_edges().await.expect("get_all_edges failed");
    assert_eq!(all.len(), 2);
}

#[tokio::test]
async fn test_get_nodes_by_dir() {
    let db = setup_db().await;

    let mut n1 = sample_node("dir-1", "f1", "src/a/foo.rs");
    n1.kind = NodeKind::Function;

    let mut n2 = sample_node("dir-2", "f2", "src/a/bar.rs");
    n2.kind = NodeKind::Function;

    let mut n3 = sample_node("dir-3", "f3", "src/b/baz.rs");
    n3.kind = NodeKind::Function;

    let mut n4 = sample_node("dir-4", "S1", "src/a/foo.rs");
    n4.kind = NodeKind::Struct;

    db.insert_nodes(&[n1, n2, n3, n4])
        .await
        .expect("insert_nodes failed");

    // Query src/a/ with Function kind
    let results = db
        .get_nodes_by_dir("src/a/", &[NodeKind::Function])
        .await
        .expect("get_nodes_by_dir failed");

    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|n| n.file_path.starts_with("src/a/")));
    assert!(results.iter().all(|n| n.kind == NodeKind::Function));
}

#[tokio::test]
async fn test_get_nodes_by_dir_multiple_kinds() {
    let db = setup_db().await;

    let mut n1 = sample_node("dirk-1", "f1", "src/a/foo.rs");
    n1.kind = NodeKind::Function;

    let mut n2 = sample_node("dirk-2", "S1", "src/a/foo.rs");
    n2.kind = NodeKind::Struct;

    let mut n3 = sample_node("dirk-3", "m1", "src/a/foo.rs");
    n3.kind = NodeKind::Method;

    db.insert_nodes(&[n1, n2, n3])
        .await
        .expect("insert_nodes failed");

    let results = db
        .get_nodes_by_dir("src/a/", &[NodeKind::Function, NodeKind::Struct])
        .await
        .expect("get_nodes_by_dir failed");

    assert_eq!(results.len(), 2);
}

#[tokio::test]
async fn test_get_nodes_by_dir_empty_kinds() {
    let db = setup_db().await;

    let n1 = sample_node("dire-1", "f1", "src/a/foo.rs");
    db.insert_node(&n1).await.expect("insert_node failed");

    // Empty kinds should return empty
    let results = db
        .get_nodes_by_dir("src/a/", &[])
        .await
        .expect("get_nodes_by_dir failed");

    assert!(results.is_empty());
}

#[tokio::test]
async fn test_search_nodes_ranking_order() {
    let db = setup_db().await;

    // Node whose name exactly contains the query term should rank higher
    let mut exact_match = sample_node("sr-1", "process_data", "src/lib.rs");
    exact_match.qualified_name = "crate::process_data".to_string();
    exact_match.signature = Some("fn process_data()".to_string());

    // Node with partial match in qualified_name only
    let mut partial_match = sample_node("sr-2", "helper", "src/lib.rs");
    partial_match.qualified_name = "crate::data_module::helper".to_string();
    partial_match.signature = Some("fn helper()".to_string());

    // Node with no match at all
    let mut no_match = sample_node("sr-3", "unrelated", "src/lib.rs");
    no_match.qualified_name = "crate::unrelated".to_string();
    no_match.docstring = None;

    db.insert_nodes(&[exact_match, partial_match, no_match])
        .await
        .expect("insert_nodes failed");

    let results = db
        .search_nodes("process_data", 10)
        .await
        .expect("search_nodes failed");

    // The exact name match should appear first (highest score)
    assert!(!results.is_empty());
    assert_eq!(results[0].node.id, "sr-1");
    // Score should be positive
    assert!(results[0].score > 0.0, "score should be positive");
}

#[tokio::test]
async fn test_get_nodes_by_kind_same_file_multiple_kinds() {
    let db = setup_db().await;

    let mut func = sample_node("kmf-1", "func_a", "src/mixed.rs");
    func.kind = NodeKind::Function;

    let mut strct = sample_node("kmf-2", "StructA", "src/mixed.rs");
    strct.kind = NodeKind::Struct;

    let mut method = sample_node("kmf-3", "method_a", "src/mixed.rs");
    method.kind = NodeKind::Method;

    let mut trait_node = sample_node("kmf-4", "TraitA", "src/mixed.rs");
    trait_node.kind = NodeKind::Trait;

    let mut enum_node = sample_node("kmf-5", "EnumA", "src/mixed.rs");
    enum_node.kind = NodeKind::Enum;

    db.insert_nodes(&[func, strct, method, trait_node, enum_node])
        .await
        .expect("insert_nodes failed");

    // Verify each kind is returned correctly
    let functions = db.get_nodes_by_kind(NodeKind::Function).await.unwrap();
    assert_eq!(functions.len(), 1);
    assert_eq!(functions[0].name, "func_a");

    let structs = db.get_nodes_by_kind(NodeKind::Struct).await.unwrap();
    assert_eq!(structs.len(), 1);
    assert_eq!(structs[0].name, "StructA");

    let traits = db.get_nodes_by_kind(NodeKind::Trait).await.unwrap();
    assert_eq!(traits.len(), 1);
    assert_eq!(traits[0].name, "TraitA");

    let enums = db.get_nodes_by_kind(NodeKind::Enum).await.unwrap();
    assert_eq!(enums.len(), 1);
    assert_eq!(enums[0].name, "EnumA");

    // All in same file
    let all = db.get_nodes_by_file("src/mixed.rs").await.unwrap();
    assert_eq!(all.len(), 5);
}

#[tokio::test]
async fn test_fts_name_match_outranks_docstring_match() {
    let db = setup_db().await;

    // Node A: search term in name
    let node_a = Node {
        id: "function:aaa".to_string(),
        kind: NodeKind::Function,
        name: "sync_data".to_string(),
        qualified_name: "src/lib.rs::sync_data".to_string(),
        file_path: "src/lib.rs".to_string(),
        start_line: 1,
        attrs_start_line: 1,
        end_line: 5,
        start_column: 0,
        end_column: 1,
        signature: Some("pub fn sync_data()".to_string()),
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
    };
    db.insert_node(&node_a).await.unwrap();

    // Node B: search term only in docstring
    let node_b = Node {
        id: "function:bbb".to_string(),
        kind: NodeKind::Function,
        name: "upload_report".to_string(),
        qualified_name: "src/lib.rs::upload_report".to_string(),
        file_path: "src/lib.rs".to_string(),
        start_line: 10,
        attrs_start_line: 10,
        end_line: 15,
        start_column: 0,
        end_column: 1,
        signature: Some("pub fn upload_report()".to_string()),
        docstring: Some(
            "This function runs after sync completes to upload the sync report".to_string(),
        ),
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
    };
    db.insert_node(&node_b).await.unwrap();

    let results = db.search_nodes("sync", 10).await.unwrap();
    assert!(results.len() >= 2, "both nodes should match 'sync'");
    assert_eq!(
        results[0].node.id, "function:aaa",
        "name match should rank first"
    );
}

#[tokio::test]
async fn test_get_nodes_by_qualified_name_returns_all_matches() {
    let db = setup_db().await;

    // Two nodes with the same qualified name (e.g. overloaded methods or
    // multiple impl blocks). Both should come back.
    let mut a = sample_node("a", "render", "src/foo.rs");
    a.qualified_name = "crate::foo::render".to_string();
    let mut b = sample_node("b", "render", "src/bar.rs");
    b.qualified_name = "crate::foo::render".to_string();
    let mut c = sample_node("c", "other", "src/foo.rs");
    c.qualified_name = "crate::foo::other".to_string();

    db.insert_nodes(&[a, b, c])
        .await
        .expect("insert_nodes failed");

    let hits = db
        .get_nodes_by_qualified_name("crate::foo::render")
        .await
        .expect("query failed");
    assert_eq!(hits.len(), 2);
    assert!(
        hits.iter()
            .all(|n| n.qualified_name == "crate::foo::render")
    );

    let none = db
        .get_nodes_by_qualified_name("crate::missing")
        .await
        .expect("query failed");
    assert!(none.is_empty());
}

#[tokio::test]
async fn test_get_nodes_by_qualified_name_normalizes_module_and_crate_forms() {
    let db = setup_db().await;
    let mut node = sample_node("worktree-root", "git_worktree_root", "src/worktree.rs");
    node.qualified_name = "src/worktree.rs::git_worktree_root".to_string();
    db.insert_node(&node).await.expect("insert failed");

    for query in [
        "git_worktree_root",
        "worktree::git_worktree_root",
        "crate::worktree::git_worktree_root",
        "src/worktree.rs::git_worktree_root",
        r"src\worktree.rs::git_worktree_root",
    ] {
        let hits = db
            .get_nodes_by_qualified_name(query)
            .await
            .unwrap_or_else(|error| panic!("query {query:?} failed: {error}"));
        assert_eq!(hits.len(), 1, "query {query:?} returned {hits:?}");
        assert_eq!(hits[0].id, "worktree-root", "query {query:?}");
    }

    let wrong_module = db
        .get_nodes_by_qualified_name("other::git_worktree_root")
        .await
        .expect("query failed");
    assert!(
        wrong_module.is_empty(),
        "wrong module must not fall back to the bare callable: {wrong_module:?}"
    );
}

#[tokio::test]
async fn test_attrs_start_line_round_trips_through_db() {
    let db = setup_db().await;

    let mut n = sample_node("n", "documented_fn", "src/lib.rs");
    n.start_line = 10;
    // Doc-comment block starts 4 lines above the function signature.
    n.attrs_start_line = 6;
    db.insert_node(&n).await.expect("insert failed");

    let fetched = db
        .get_node_by_id("n")
        .await
        .expect("query failed")
        .expect("node missing");
    assert_eq!(fetched.start_line, 10);
    assert_eq!(fetched.attrs_start_line, 6);
}

#[tokio::test]
async fn test_attrs_start_line_zero_survives_round_trip() {
    // An item documented at the very top of a file has its doc/attr block start
    // at row 0 while the item itself starts at row 1 — exactly what RustExtractor
    // emits for "/// doc\nfn foo() {}". A stored 0 is a *legitimate* value and
    // must survive the DB round-trip strictly below start_line. Regression:
    // `row_to_node` used to treat a stored 0 as "unset" and substitute
    // start_line, which orphaned the leading doc/attr block for first-in-file
    // items.
    let db = setup_db().await;

    let mut n = sample_node("first_in_file", "documented_fn", "src/lib.rs");
    n.start_line = 1;
    n.attrs_start_line = 0;
    db.insert_node(&n).await.expect("insert failed");

    let by_id = db
        .get_node_by_id("first_in_file")
        .await
        .expect("query failed")
        .expect("node missing");
    assert_eq!(by_id.start_line, 1);
    assert_eq!(
        by_id.attrs_start_line, 0,
        "a legitimate attrs_start_line=0 must not be rewritten to start_line"
    );
    assert!(
        by_id.attrs_start_line < by_id.start_line,
        "leading doc/attr block must remain above the item"
    );

    // The qualified-name path used by the edit/derive resolvers must agree.
    let hits = db
        .get_nodes_by_qualified_name("crate::documented_fn")
        .await
        .expect("query failed");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].attrs_start_line, 0);
    assert!(hits[0].attrs_start_line < hits[0].start_line);
}

#[tokio::test]
async fn test_attrs_start_line_null_falls_back_to_start_line() {
    // A legacy row whose attrs_start_line is SQL NULL (a row predating the
    // column, or an older writer that never set it) must fall back to start_line
    // on read. This is now the *only* case the fallback covers, since a stored 0
    // is trusted verbatim.
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("test.db");
    support::seed_latest_graph_db(&db_path).await;

    // The fresh schema declares attrs_start_line nullable, so a raw connection
    // can persist an explicit NULL for this row.
    let conn = rusqlite::Connection::open(&db_path).expect("open offline fixture database");
    conn.execute(
        "INSERT INTO nodes (id, kind, name, qualified_name, file_path,
                            start_line, end_line, start_column, end_column,
                            updated_at, attrs_start_line)
         VALUES ('legacy', 'function', 'legacy_fn', 'crate::legacy_fn', 'src/lib.rs',
                 12, 20, 0, 1, 1000, NULL)",
        [],
    )
    .expect("insert legacy row");
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").ok();
    drop(conn);

    let (db, _migrated) = crate::common::open_test_database(&db_path)
        .await
        .expect("open db");
    let fetched = db
        .get_node_by_id("legacy")
        .await
        .expect("query failed")
        .expect("node missing");
    assert_eq!(fetched.start_line, 12);
    assert_eq!(
        fetched.attrs_start_line, fetched.start_line,
        "a NULL attrs_start_line must fall back to start_line"
    );
}
