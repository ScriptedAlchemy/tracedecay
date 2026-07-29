//! Transaction rollback tests for failed batch writes.

use super::*;

#[tokio::test]
async fn test_insert_nodes_rolls_back_after_execute_failure() {
    let db = setup_db().await;
    db.execute_write_batch(
        "install failing node insert trigger fixture",
        "CREATE TRIGGER fail_nodes_insert BEFORE INSERT ON nodes BEGIN
                 SELECT RAISE(FAIL, 'forced node insert failure');
             END;",
    )
    .await
    .expect("failed to install node failure trigger");

    let err = db
        .insert_nodes(&[sample_node("rb-node", "failing_node", "src/lib.rs")])
        .await
        .expect_err("forced trigger should make insert_nodes fail");
    assert!(
        err.to_string().contains("forced node insert failure"),
        "unexpected error: {err}"
    );

    assert_can_start_new_transaction(&db).await;
}

#[tokio::test]
async fn test_insert_edges_rolls_back_after_execute_failure() {
    let db = setup_db().await;
    let nodes = vec![
        sample_node("rb-edge-a", "edge_a", "src/lib.rs"),
        sample_node("rb-edge-b", "edge_b", "src/lib.rs"),
    ];
    db.insert_nodes(&nodes).await.expect("insert_nodes failed");
    db.execute_write_batch(
        "install failing edge insert trigger fixture",
        "CREATE TRIGGER fail_edges_insert BEFORE INSERT ON edges BEGIN
                 SELECT RAISE(FAIL, 'forced edge insert failure');
             END;",
    )
    .await
    .expect("failed to install edge failure trigger");

    let err = db
        .insert_edges(&[sample_edge("rb-edge-a", "rb-edge-b", EdgeKind::Calls)])
        .await
        .expect_err("forced trigger should make insert_edges fail");
    assert!(
        err.to_string().contains("forced edge insert failure"),
        "unexpected error: {err}"
    );

    assert_can_start_new_transaction(&db).await;
}

#[tokio::test]
async fn test_upsert_files_rolls_back_after_execute_failure() {
    let db = setup_db().await;
    db.execute_write_batch(
        "install failing file insert trigger fixture",
        "CREATE TRIGGER fail_files_insert BEFORE INSERT ON files BEGIN
                 SELECT RAISE(FAIL, 'forced file insert failure');
             END;",
    )
    .await
    .expect("failed to install file failure trigger");

    let err = db
        .upsert_files(&[sample_file("src/lib.rs")])
        .await
        .expect_err("forced trigger should make upsert_files fail");
    assert!(
        err.to_string().contains("forced file insert failure"),
        "unexpected error: {err}"
    );

    assert_can_start_new_transaction(&db).await;
}

#[tokio::test]
async fn test_insert_unresolved_refs_rolls_back_after_execute_failure() {
    let db = setup_db().await;
    db.insert_nodes(&[sample_node("rb-ref", "ref_source", "src/lib.rs")])
        .await
        .expect("insert_nodes failed");
    db.execute_write_batch(
        "install failing unresolved reference trigger fixture",
        "CREATE TRIGGER fail_unresolved_refs_insert BEFORE INSERT ON unresolved_refs BEGIN
                 SELECT RAISE(FAIL, 'forced unresolved ref insert failure');
             END;",
    )
    .await
    .expect("failed to install unresolved ref failure trigger");

    let err = db
        .insert_unresolved_refs(&[UnresolvedRef {
            from_node_id: "rb-ref".to_string(),
            reference_name: "MissingType".to_string(),
            reference_kind: EdgeKind::Uses,
            line: 7,
            column: 12,
            file_path: "src/lib.rs".to_string(),
        }])
        .await
        .expect_err("forced trigger should make insert_unresolved_refs fail");
    assert!(
        err.to_string()
            .contains("forced unresolved ref insert failure"),
        "unexpected error: {err}"
    );

    assert_can_start_new_transaction(&db).await;
}
