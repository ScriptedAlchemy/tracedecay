use super::*;
use crate::support;
use tempfile::TempDir;

#[tokio::test]
async fn test_empty_db_template_cache_seeds_without_migration() {
    let dir = TempDir::new().expect("failed to create temp dir");
    let db_path = dir.path().join("test.db");
    support::seed_latest_graph_db(&db_path).await;

    let template_path = support::template_db_path("graph-empty", &[]);
    assert!(template_path.exists(), "template should be cached on disk");
    assert!(
        template_path.metadata().unwrap().len() > 0,
        "template database should not be empty"
    );

    let (_db, migrated) = crate::common::open_test_database(&db_path)
        .await
        .expect("failed to open template database");
    assert!(
        !migrated,
        "cached test database should not require migration"
    );
}

#[tokio::test]
async fn test_dependency_import_uses_query_use_nodes_without_unresolved_refs() {
    let db = setup_db().await;
    let mut import_node = sample_node("dep-import", "pkg", "src/app.ts");
    import_node.kind = NodeKind::Use;
    import_node.start_line = 4;
    import_node.signature = Some("import type { Foo, Bar as Baz } from \"pkg\";".to_string());
    let mut relative_import = sample_node("relative-import", "./local", "src/app.ts");
    relative_import.kind = NodeKind::Use;
    relative_import.start_line = 5;
    relative_import.signature = Some("import type { Foo } from \"./local\";".to_string());

    db.insert_nodes(&[import_node, relative_import])
        .await
        .expect("insert_nodes failed");

    let imports = db
        .dependency_import_uses("Foo", 5, None)
        .await
        .expect("dependency_import_uses failed");

    assert_eq!(imports.len(), 1);
    assert_eq!(imports[0].module, "pkg");
    assert_eq!(
        imports[0].signature,
        "import type { Foo, Bar as Baz } from \"pkg\";"
    );
    assert_eq!(imports[0].file_path, "src/app.ts");
    assert_eq!(imports[0].line, 4);
}

#[tokio::test]
async fn test_dependency_import_uses_applies_scope_before_limit() {
    let db = setup_db().await;
    let mut nodes = Vec::new();
    for index in 0..8 {
        let mut import_node = sample_node(
            &format!("dep-import-{index}"),
            "pkg",
            &format!("src/{index}.ts"),
        );
        import_node.kind = NodeKind::Use;
        import_node.start_line = index;
        import_node.signature = Some("import type { Foo } from \"pkg\";".to_string());
        nodes.push(import_node);
    }
    let mut scoped_import = sample_node("dep-import-scoped", "pkg", "tests/app.ts");
    scoped_import.kind = NodeKind::Use;
    scoped_import.start_line = 9;
    scoped_import.signature = Some("import type { Foo } from \"pkg\";".to_string());
    nodes.push(scoped_import);

    db.insert_nodes(&nodes).await.expect("insert_nodes failed");

    let imports = db
        .dependency_import_uses("Foo", 1, Some("tests"))
        .await
        .expect("dependency_import_uses failed");

    assert_eq!(imports.len(), 1);
    assert_eq!(imports[0].file_path, "tests/app.ts");
}

#[tokio::test]
async fn test_db_module_reexports_database_and_stored_fingerprint() {
    let db = setup_db().await;
    let node = sample_node("fp-node", "fingerprinted", "src/lib.rs");
    db.insert_node(&node).await.expect("insert_node failed");

    let fingerprint = Fingerprint {
        ast_hash: "ast-hash".to_string(),
        cfg_hash: "cfg-hash".to_string(),
        call_seq_hash: "call-seq-hash".to_string(),
        shingles: vec![0xabcddcba, 0x12345678],
        body_tokens: 42,
        source_hash: "source-hash".to_string(),
    };
    db.upsert_fingerprint("fp-node", &fingerprint)
        .await
        .expect("upsert_fingerprint failed");

    let stored: StoredFingerprint = db
        .get_fingerprint("fp-node")
        .await
        .expect("get_fingerprint failed")
        .expect("fingerprint should exist");
    assert_eq!(stored.node_id, "fp-node");
    assert_eq!(stored.ast_hash, fingerprint.ast_hash);
    assert_eq!(stored.cfg_hash, fingerprint.cfg_hash);
    assert_eq!(stored.call_seq_hash, fingerprint.call_seq_hash);
    assert_eq!(stored.shingles, fingerprint.shingles);
    assert_eq!(stored.body_tokens, fingerprint.body_tokens as u32);
    assert_eq!(stored.source_hash, fingerprint.source_hash);
}
