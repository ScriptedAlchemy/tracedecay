use super::*;

#[tokio::test]
async fn fts_corruption_falls_back_without_rebuild_or_write() {
    let (db, _dir, db_path) = setup_db().await;

    // Insert data so FTS has content
    let nodes = vec![
        sample_node("e1", "important_handler"),
        sample_node("e2", "other_helper"),
    ];
    db.insert_nodes(&nodes).await.unwrap();

    // Verify search works
    let results = db.search_nodes("important_handler", 10).await.unwrap();
    assert_eq!(results[0].node.id, "e1");

    // Capture an FTS segment, then corrupt only its payload on disk. The nodes
    // table and primary database B-trees remain healthy.
    let mut rows = db
        .conn()
        .query(
            "SELECT block FROM nodes_fts_data WHERE id > 10 ORDER BY id DESC LIMIT 1",
            (),
        )
        .await
        .unwrap();
    let segment = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<Vec<u8>>(0)
        .unwrap();
    drop(rows);
    db.checkpoint().await.unwrap();
    db.close();

    // Corrupt both FTS and an unrelated table. Checking only `nodes` would
    // incorrectly permit the LIKE fallback because its B-tree is still sound.
    let mut bytes = std::fs::read(&db_path).unwrap();
    let offset = bytes
        .windows(segment.len())
        .position(|candidate| candidate == segment)
        .expect("FTS segment must be present in the checkpointed database");
    bytes[offset..offset + 8].fill(0xff);
    std::fs::write(&db_path, bytes).unwrap();

    let (db, _) = crate::common::open_test_database(&db_path).await.unwrap();
    assert!(
        !db.quick_check().await.unwrap(),
        "fixture must trigger SQLite's FTS integrity failure"
    );
    let changes_before = db.conn().total_changes();

    let results = db.search_nodes("important_handler", 10).await.unwrap();
    assert_eq!(results[0].node.id, "e1", "LIKE fallback must still match");
    assert_eq!(
        db.conn().total_changes(),
        changes_before,
        "search must not rebuild or otherwise write"
    );

    let mut rows = db
        .conn()
        .query(
            "SELECT rowid FROM nodes_fts WHERE nodes_fts MATCH '\"important_handler\"*'",
            (),
        )
        .await
        .unwrap();
    assert!(
        rows.next().await.is_err(),
        "the corrupt FTS index must remain untouched for offline repair"
    );
    drop(rows);
    close_db(db).await;
}

#[tokio::test]
async fn whole_database_corruption_propagates_without_write() {
    let (db, _dir, db_path) = setup_db().await;
    db.insert_nodes(&[sample_node("whole-db", "whole_db_probe")])
        .await
        .unwrap();

    let mut rows = db
        .conn()
        .query(
            "SELECT block FROM nodes_fts_data WHERE id > 10 ORDER BY id DESC LIMIT 1",
            (),
        )
        .await
        .unwrap();
    let segment = rows
        .next()
        .await
        .unwrap()
        .unwrap()
        .get::<Vec<u8>>(0)
        .unwrap();
    drop(rows);
    let mut rows = db
        .conn()
        .query(
            "SELECT rootpage FROM sqlite_schema WHERE name = 'edges'",
            (),
        )
        .await
        .unwrap();
    let root_page = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap() as u64;
    drop(rows);
    let mut rows = db.conn().query("PRAGMA page_size", ()).await.unwrap();
    let page_size = rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap() as u64;
    drop(rows);
    db.checkpoint().await.unwrap();
    db.close();

    let mut bytes = std::fs::read(&db_path).unwrap();
    let fts_offset = bytes
        .windows(segment.len())
        .position(|candidate| candidate == segment)
        .expect("FTS segment must be present in the checkpointed database");
    bytes[fts_offset..fts_offset + 8].fill(0xff);
    bytes[((root_page - 1) * page_size) as usize] = 0xff;
    std::fs::write(&db_path, bytes).unwrap();

    let (db, _) = crate::common::open_test_database(&db_path).await.unwrap();
    let changes_before = db.conn().total_changes();
    let error = db.search_nodes("whole_db_probe", 10).await.unwrap_err();
    assert!(
        Database::is_corruption_error(&error),
        "unexpected error: {error}"
    );
    assert_eq!(
        db.conn().total_changes(),
        changes_before,
        "search must not write while reporting whole-database corruption"
    );
    db.close();
}
