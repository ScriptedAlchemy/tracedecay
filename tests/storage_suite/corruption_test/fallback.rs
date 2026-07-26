use super::*;
use std::path::Path;

async fn raw_quick_check_detects_corruption(db_path: &Path) -> bool {
    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open raw read-only database");
    conn.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
        .map_or(true, |result| result != "ok")
}

#[tokio::test]
async fn fts_corruption_propagates_without_rebuild_or_write() {
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

    // Desynchronize only the derivable FTS index. The nodes table and primary
    // database B-trees remain healthy.
    db.execute_write_batch(
        "clear FTS corruption fixture",
        "DELETE FROM nodes_fts_data WHERE id > 10;",
    )
    .await
    .unwrap();
    db.checkpoint().await.unwrap();
    db.close();
    truncate_fixture_wal(&db_path);

    assert!(
        raw_quick_check_detects_corruption(&db_path).await,
        "fixture must trigger SQLite's FTS integrity failure"
    );
    let corrupted_bytes = std::fs::read(&db_path).unwrap();

    let conn =
        rusqlite::Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open raw read-only database");
    assert_eq!(
        conn.query_row(
            "SELECT id FROM nodes WHERE name LIKE '%important_handler%'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
        "e1",
        "the intact nodes table must remain readable"
    );
    assert_eq!(
        conn.query_row("PRAGMA quick_check(nodes)", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap(),
        "ok",
        "the nodes table and its ordinary indexes must remain healthy"
    );
    let mut statement = conn
        .prepare("SELECT rowid FROM nodes_fts WHERE nodes_fts MATCH '\"important_handler\"*'")
        .unwrap();
    let mut rows = statement.query([]).unwrap();
    assert!(
        rows.next().is_err(),
        "the corrupt FTS index must remain untouched for offline repair"
    );
    drop(rows);
    drop(statement);
    drop(conn);

    let (reopened, _) = crate::common::open_test_database(&db_path)
        .await
        .expect("direct database open should retain read fallback access");
    let error = reopened
        .search_nodes("important_handler", 10)
        .await
        .expect_err("direct database access must fail closed on FTS corruption");
    assert!(
        error.to_string().contains("fts5: corruption found"),
        "unexpected direct database error: {error}"
    );
    reopened.close();
    assert_eq!(
        std::fs::read(&db_path).unwrap(),
        corrupted_bytes,
        "direct database open must not rebuild or otherwise write"
    );
}

#[tokio::test]
async fn whole_database_corruption_propagates_without_write() {
    let (db, _dir, db_path) = setup_db().await;
    db.insert_nodes(&[sample_node("whole-db", "whole_db_probe")])
        .await
        .unwrap();

    db.execute_write_batch(
        "clear whole-database corruption FTS fixture",
        "DELETE FROM nodes_fts_data WHERE id > 10;",
    )
    .await
    .unwrap();
    let root_page = db
        .query_scalar_i64(
            "read corruption fixture root page",
            "SELECT rootpage FROM sqlite_schema WHERE name = 'edges'",
        )
        .await
        .unwrap() as u64;
    let page_size = db
        .query_scalar_i64("read corruption fixture page size", "PRAGMA page_size")
        .await
        .unwrap() as u64;
    db.checkpoint().await.unwrap();
    db.close();
    truncate_fixture_wal(&db_path);

    let mut bytes = std::fs::read(&db_path).unwrap();
    bytes[((root_page - 1) * page_size) as usize] = 0xff;
    std::fs::write(&db_path, bytes).unwrap();

    let corrupted_bytes = std::fs::read(&db_path).unwrap();
    assert!(
        raw_quick_check_detects_corruption(&db_path).await,
        "fixture must fail SQLite's read-only quick_check"
    );

    let error = match crate::common::open_test_database(&db_path).await {
        Err(error) => error,
        Ok((db, _)) => {
            let error = db
                .search_nodes("whole_db_probe", 10)
                .await
                .expect_err("whole-database corruption must not use the LIKE fallback");
            db.close();
            error
        }
    };
    assert!(
        Database::is_corruption_error(&error),
        "unexpected error: {error}"
    );
    assert_eq!(
        std::fs::read(&db_path).unwrap(),
        corrupted_bytes,
        "failed open must not write while reporting whole-database corruption"
    );
}
