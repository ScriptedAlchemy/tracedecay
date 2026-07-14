use super::*;

#[test]
fn global_db_disables_mmap_on_every_platform() {
    assert_eq!(global_db_mmap_size_guard(), 0);
}

#[tokio::test]
async fn concurrent_full_opens_singleflight_schema_but_use_independent_connections() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("global.db");
    let (first, second, third, fourth) = tokio::join!(
        GlobalDb::open_at(&path),
        GlobalDb::open_at(&path),
        GlobalDb::open_at(&path),
        GlobalDb::open_at(&path),
    );
    let first = first.expect("first open");
    let opened = [
        second.expect("second open"),
        third.expect("third open"),
        fourth.expect("fourth open"),
    ];
    for db in &opened {
        assert!(!Arc::ptr_eq(&first.inner, &db.inner));
    }

    first.conn().execute("BEGIN", ()).await.unwrap();
    for db in &opened {
        db.conn().execute("BEGIN", ()).await.unwrap();
    }
    first.conn().execute("ROLLBACK", ()).await.unwrap();
    for db in &opened {
        db.conn().execute("ROLLBACK", ()).await.unwrap();
    }
}

#[tokio::test]
async fn global_db_slot_uses_database_authority_canonical_identity() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("nested")).unwrap();
    let direct_path = dir.path().join("global.db");
    let alias_path = dir.path().join("nested").join("..").join("global.db");
    let direct = DatabaseAuthority::for_runtime(&direct_path, "direct slot identity").unwrap();
    let alias = DatabaseAuthority::for_runtime(&alias_path, "alias slot identity").unwrap();

    assert_eq!(
        direct.canonical_database_path(),
        alias.canonical_database_path()
    );
    assert!(Arc::ptr_eq(
        &global_db_slot(&direct),
        &global_db_slot(&alias)
    ));
}

#[tokio::test]
async fn assuming_schema_open_cannot_poison_full_schema_ensure() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("global.db");
    std::fs::File::create(&path).unwrap();

    let raw = GlobalDb::open_at_assuming_schema(&path)
        .await
        .expect("raw assuming-schema open");
    let mut rows = raw
        .conn()
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'projects'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    drop(rows);
    let raw_inner = Arc::downgrade(&raw.inner);
    raw.close();

    let ensured = GlobalDb::open_at(&path).await.expect("full schema open");
    let mut rows = ensured
        .conn()
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'projects'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        1
    );
    assert!(raw_inner.upgrade().is_none());
}

#[tokio::test]
async fn distinct_global_db_paths_do_not_share_an_initialization_lock() {
    let dir = tempfile::TempDir::new().unwrap();
    let first_path = dir.path().join("first.db");
    let second_path = dir.path().join("second.db");
    let first_authority =
        DatabaseAuthority::for_runtime(&first_path, "hold first global DB slot").unwrap();
    let first_slot = global_db_slot(&first_authority);
    let _first_guard = first_slot.lock().await;

    let second = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        GlobalDb::open_at_without_structured_backfill(&second_path),
    )
    .await
    .expect("unrelated global DB path waited on the first path's slot")
    .expect("open unrelated global DB path");
    let second_authority =
        DatabaseAuthority::for_runtime(&second_path, "verify second global DB path").unwrap();
    assert_eq!(second.db_path(), second_authority.canonical_database_path());
}

#[tokio::test]
async fn read_only_open_is_independent_and_cannot_poison_writer() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("global.db");
    let seed = GlobalDb::open_at(&path).await.expect("seed writable open");
    drop(seed);
    let reader = GlobalDb::open_read_only_at(&path)
        .await
        .expect("read-only open");
    let writable = GlobalDb::open_at(&path).await.expect("writable open");
    assert!(!Arc::ptr_eq(&writable.inner, &reader.inner));
    assert!(
        reader
            .conn()
            .execute("CREATE TABLE forbidden_reader_write (id INTEGER)", ())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn runtime_open_without_authority_scope_fails_closed() {
    let path = std::env::current_dir()
        .unwrap()
        .join("target/global-db-no-authority/global.db");
    if path.starts_with(std::env::temp_dir()) {
        return;
    }
    assert!(GlobalDb::open_at(&path).await.is_none());
}

#[tokio::test]
async fn try_open_at_preserves_authority_error() {
    let path = std::env::current_dir()
        .unwrap()
        .join("target/global-db-authority-error/global.db");
    if path.starts_with(std::env::temp_dir()) {
        return;
    }
    let Err(error) = GlobalDb::try_open_at(&path).await else {
        panic!("unauthorized global DB open unexpectedly succeeded");
    };
    let message = error.to_string();
    assert!(
        message
            .contains("database access requires managed-daemon or exclusive-maintenance authority"),
        "{message}"
    );
    assert!(message.contains("open global database"), "{message}");
    let displayed = path.display().to_string();
    #[cfg(windows)]
    assert!(
        message
            .replace('\\', "/")
            .contains(&displayed.replace('\\', "/")),
        "{message}"
    );
    #[cfg(not(windows))]
    assert!(message.contains(&displayed), "{message}");
}

#[tokio::test]
async fn isolated_temp_database_opens_without_ambient_daemon() {
    let dir = tempfile::TempDir::new().unwrap();
    let _db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("temp test open");
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

    assert!(
        add_session_parent_column_after_missing_check(
            &conn,
            "parent_session_id",
            "ALTER TABLE sessions ADD COLUMN parent_session_id TEXT",
        )
        .await
        .is_some()
    );
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

#[tokio::test]
async fn search_code_projects_matches_any_whitespace_term() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("open global db");

    for (project_id, root) in [
        ("proj_rsbuild", "/repos/rsbuild-plugin-react-router"),
        ("proj_rspack", "/repos/rspack"),
        ("proj_unrelated", "/repos/unrelated"),
    ] {
        db.upsert_code_project(project_id, Path::new(root), None, None, Some("main"))
            .await
            .expect("code project should upsert");
    }
    db.upsert_code_project(
        "proj_remote_only",
        Path::new("/repos/remote-only"),
        None,
        Some("https://token:secret@example.test/remote-only.git"),
        Some("main"),
    )
    .await
    .expect("code project with remote should upsert");

    let matches = db.search_code_projects("rsbuild rspack", 10).await;
    let ids: Vec<&str> = matches
        .iter()
        .map(|project| project.project_id.as_str())
        .collect();

    assert!(ids.contains(&"proj_rsbuild"), "ids: {ids:?}");
    assert!(ids.contains(&"proj_rspack"), "ids: {ids:?}");
    assert!(!ids.contains(&"proj_unrelated"), "ids: {ids:?}");

    let remote_name_matches = db.search_code_projects("remote-only.git", 10).await;
    let remote_name_ids: Vec<&str> = remote_name_matches
        .iter()
        .map(|project| project.project_id.as_str())
        .collect();
    assert!(
        remote_name_ids.contains(&"proj_remote_only"),
        "remote_name_ids: {remote_name_ids:?}"
    );

    let remote_matches = db.search_code_projects("secret", 10).await;
    assert!(
        remote_matches.is_empty(),
        "remote credential text must not be searchable: {remote_matches:?}"
    );
}
