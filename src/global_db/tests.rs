use super::*;

#[test]
fn global_db_mmap_guard_matches_connection_platform_guard() {
    if cfg!(windows) {
        assert_eq!(global_db_mmap_size_guard(), Some(0));
    } else {
        assert_eq!(global_db_mmap_size_guard(), None);
    }
}

#[test]
fn explicit_project_path_selector_keeps_names_and_paths_separate() {
    assert!(!GlobalDb::is_explicit_project_path_selector("target"));
    assert!(!GlobalDb::is_explicit_project_path_selector(" proj_123 "));
    assert!(GlobalDb::is_explicit_project_path_selector("."));
    assert!(GlobalDb::is_explicit_project_path_selector(".."));
    assert!(GlobalDb::is_explicit_project_path_selector("./target"));
    assert!(GlobalDb::is_explicit_project_path_selector("../target"));
    assert!(GlobalDb::is_explicit_project_path_selector("/tmp/target"));
    assert!(GlobalDb::is_explicit_project_path_selector(r"..\target"));
}

#[tokio::test]
async fn session_column_migration_tolerates_duplicate_column_race() {
    // In-memory DB: the duplicate-column race only needs one connection,
    // so the on-disk sqlite file adds nothing but I/O.
    let db = Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                PRIMARY KEY(provider, session_id)
            );",
    )
    .await
    .unwrap();

    assert!(!session_column_exists(&conn, "parent_session_id").await);

    conn.execute("ALTER TABLE sessions ADD COLUMN parent_session_id TEXT", ())
        .await
        .unwrap();

    assert!(add_session_parent_column_after_missing_check(
        &conn,
        "parent_session_id",
        "ALTER TABLE sessions ADD COLUMN parent_session_id TEXT",
    )
    .await
    .is_some());
}

#[tokio::test]
async fn code_projects_seen_within_applies_window_and_limit() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("open global db");

    let now = crate::tracedecay::current_timestamp();
    // (project_id, last_seen_at)
    let rows = [
        ("proj_recent", now - 60),       // 1 min ago  -> in window
        ("proj_mid", now - 3 * 86_400),  // 3 days ago -> in window
        ("proj_old", now - 30 * 86_400), // 30 days ago-> outside 14d window
    ];
    for (project_id, last_seen) in rows {
        db.conn
            .execute(
                "INSERT INTO code_projects
                     (project_id, canonical_root, display_root, git_common_dir,
                      git_remote_url, default_branch, created_at, last_seen_at)
                     VALUES (?1, ?2, ?3, NULL, NULL, NULL, ?4, ?4)",
                params![
                    project_id,
                    format!("/root/{project_id}"),
                    project_id,
                    last_seen
                ],
            )
            .await
            .unwrap();
    }

    // 14-day window keeps the two recent projects, most-recent first.
    let within = db.code_projects_seen_within(14 * 86_400, 10).await;
    let ids: Vec<&str> = within.iter().map(|p| p.project_id.as_str()).collect();
    assert_eq!(ids, vec!["proj_recent", "proj_mid"]);

    // Limit caps the result even when more projects are in-window.
    let capped = db.code_projects_seen_within(14 * 86_400, 1).await;
    let capped_ids: Vec<&str> = capped.iter().map(|p| p.project_id.as_str()).collect();
    assert_eq!(capped_ids, vec!["proj_recent"]);
}
