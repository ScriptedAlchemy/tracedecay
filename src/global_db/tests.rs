use super::*;

#[cfg(unix)]
fn colliding_non_unicode_project_paths(root: &Path) -> (PathBuf, PathBuf) {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    (
        root.join(OsString::from_vec(vec![b'p', 0x80])),
        root.join(OsString::from_vec(vec![b'p', 0x81])),
    )
}

#[cfg(windows)]
fn colliding_non_unicode_project_paths(root: &Path) -> (PathBuf, PathBuf) {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt as _;

    (
        root.join(OsString::from_wide(&[u16::from(b'p'), 0xd800])),
        root.join(OsString::from_wide(&[u16::from(b'p'), 0xd801])),
    )
}

#[cfg(any(unix, windows))]
async fn replace_native_alias_with_legacy(db: &GlobalDb, project_path: &Path, project_id: &str) {
    let native_alias = project_path_alias_key(project_path);
    let legacy_alias = GlobalDb::canonical_project_key(project_path);
    assert_ne!(native_alias, legacy_alias);
    db.conn
        .execute(
            "DELETE FROM project_aliases WHERE alias_path = ?1",
            params![native_alias],
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO project_aliases (alias_path, project_id, last_seen_at)
             VALUES (?1, ?2, 1)
             ON CONFLICT(alias_path) DO UPDATE SET project_id = excluded.project_id",
            params![legacy_alias, project_id],
        )
        .await
        .unwrap();
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn unique_legacy_non_unicode_alias_migrates_to_native_key() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("open global db");
    let (project_path, _) = colliding_non_unicode_project_paths(dir.path());
    db.upsert_code_project("proj_legacy", &project_path, None, None, None)
        .await
        .expect("register legacy project");
    replace_native_alias_with_legacy(&db, &project_path, "proj_legacy").await;

    let context = db
        .project_registry_context_by_alias(&project_path)
        .await
        .expect("unique legacy owner should migrate");
    assert_eq!(context.project.project_id, "proj_legacy");
    assert_eq!(
        db.project_id_by_alias_key(&project_path_alias_key(&project_path))
            .await
            .as_deref(),
        Some("proj_legacy")
    );
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn colliding_legacy_non_unicode_alias_fails_closed() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("open global db");
    let (first, second) = colliding_non_unicode_project_paths(dir.path());
    db.upsert_code_project("proj_first", &first, None, None, None)
        .await
        .expect("register first project");
    db.upsert_code_project("proj_second", &second, None, None, None)
        .await
        .expect("register second project");
    replace_native_alias_with_legacy(&db, &first, "proj_first").await;
    db.conn
        .execute(
            "DELETE FROM project_aliases WHERE alias_path = ?1",
            params![project_path_alias_key(&second)],
        )
        .await
        .unwrap();

    assert!(db.project_registry_context_by_alias(&first).await.is_none());
    assert!(
        db.project_id_by_alias_key(&project_path_alias_key(&first))
            .await
            .is_none()
    );
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn unique_legacy_non_unicode_git_common_alias_migrates_to_native_key() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("open global db");
    let project_root = dir.path().join("project");
    let (git_common_dir, _) = colliding_non_unicode_project_paths(dir.path());
    db.upsert_code_project(
        "proj_common",
        &project_root,
        Some(&git_common_dir),
        None,
        None,
    )
    .await
    .expect("register project");
    let native_alias = format!("git-common-dir:{}", project_path_alias_key(&git_common_dir));
    let legacy_alias = format!(
        "git-common-dir:{}",
        GlobalDb::canonical_project_key(&git_common_dir)
    );
    db.conn
        .execute(
            "DELETE FROM project_aliases WHERE alias_path = ?1",
            params![native_alias.as_str()],
        )
        .await
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO project_aliases (alias_path, project_id, last_seen_at)
             VALUES (?1, 'proj_common', 1)",
            params![legacy_alias],
        )
        .await
        .unwrap();

    assert_eq!(
        db.project_id_by_git_common_dir_alias(&git_common_dir)
            .await
            .as_deref(),
        Some("proj_common")
    );
    assert_eq!(
        db.project_id_by_alias_key(&native_alias).await.as_deref(),
        Some("proj_common")
    );
}

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
async fn cancelled_authoritative_transaction_isolated_from_retained_connection_and_cleans_payload()
{
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("global.db");
    let db = Arc::new(GlobalDb::open_at(&path).await.expect("global DB open"));
    let session = SessionRecord {
        provider: "codex".to_string(),
        session_id: "cancelled-transaction".to_string(),
        project_key: "project".to_string(),
        project_path: dir.path().display().to_string(),
        title: None,
        started_at: None,
        ended_at: None,
        transcript_path: Some(dir.path().join("session.jsonl").display().to_string()),
        metadata_json: None,
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    };
    let (created_tx, created_rx) = tokio::sync::oneshot::channel();
    let task_db = Arc::clone(&db);
    let task_session = session.clone();
    let task = tokio::spawn(async move {
        let _writer = task_db.transaction.lock().await;
        let transaction = task_db.begin_authoritative_transaction().await.unwrap();
        assert!(GlobalDb::upsert_session_in_existing_tx(&transaction, &task_session).await);
        let mut payload_rollback =
            crate::sessions::lcm::payload::PayloadFileRollback::begin_cancellation_safe(
                &task_db.storage_root,
            );
        let payload = crate::sessions::lcm::payload::write_external_payload_tracked(
            &task_db.storage_root,
            crate::sessions::lcm::payload::ExternalPayloadWrite {
                provider: "codex",
                session_id: "cancelled-transaction",
                message_id: "cancelled-message",
                kind: "tool_output",
                content: "payload created inside a transaction that will be cancelled",
                metadata_json: None,
            },
            &mut payload_rollback,
        )
        .unwrap();
        created_tx.send(payload.payload_ref).unwrap();
        std::future::pending::<()>().await;
    });

    let payload_ref = created_rx.await.expect("payload creation signal");
    let payload_path =
        crate::sessions::lcm::payload::payload_dir(&db.storage_root).join(&payload_ref);
    assert!(payload_path.is_file());

    let mut rows = db
        .conn()
        .query(
            "SELECT 1 FROM sessions WHERE provider = ?1 AND session_id = ?2",
            params!["codex", "cancelled-transaction"],
        )
        .await
        .expect("retained read must not join the fresh transaction");
    assert!(rows.next().await.unwrap().is_none());
    drop(rows);

    db.conn()
        .execute_batch("PRAGMA busy_timeout = 0;")
        .await
        .unwrap();
    assert!(!GlobalDb::upsert_session_in_existing_tx(&db.conn, &session).await);

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(!payload_path.exists());
    db.conn()
        .execute_batch("PRAGMA busy_timeout = 5000;")
        .await
        .unwrap();
    assert!(
        db.get_session("codex", "cancelled-transaction")
            .await
            .is_none()
    );
    assert!(db.upsert_session(&session).await);
    assert!(
        db.get_session("codex", "cancelled-transaction")
            .await
            .is_some()
    );
}

#[tokio::test]
async fn cancelled_lcm_lifecycle_mutation_rolls_back_and_releases_writer() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Arc::new(
        GlobalDb::open_at(&dir.path().join("global.db"))
            .await
            .expect("global DB open"),
    );
    let update = crate::sessions::lcm::LcmLifecycleUpdate {
        provider: "cursor".to_string(),
        conversation_id: "cancelled-lifecycle".to_string(),
        current_session_id: "cancelled-lifecycle".to_string(),
        current_frontier_store_id: None,
        last_finalized_session_id: None,
        last_finalized_frontier_store_id: None,
        maintenance_debt: vec![crate::sessions::lcm::LcmMaintenanceDebt::RawBacklog {
            from_store_id: 1,
            to_store_id: 2,
        }],
    };
    let (written_tx, written_rx) = tokio::sync::oneshot::channel();
    let task_db = Arc::clone(&db);
    let task_update = update.clone();
    let task = tokio::spawn(async move {
        let _writer = task_db.transaction.lock().await;
        let transaction = task_db.begin_authoritative_transaction().await.unwrap();
        crate::sessions::lcm::compression::update_lifecycle(&transaction, task_update)
            .await
            .unwrap();
        written_tx.send(()).unwrap();
        std::future::pending::<()>().await;
    });

    written_rx.await.expect("lifecycle write signal");
    assert!(
        db.lcm_lifecycle_state("cursor", "cancelled-lifecycle")
            .await
            .is_err(),
        "retained reader must not observe the uncommitted lifecycle state"
    );

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(
        db.lcm_lifecycle_state("cursor", "cancelled-lifecycle")
            .await
            .is_err(),
        "cancellation must roll back lifecycle state and maintenance debt"
    );

    let state = db
        .lcm_update_lifecycle(update.clone())
        .await
        .expect("writer must be reusable after cancellation");
    assert_eq!(state.provider, update.provider);
    assert_eq!(state.conversation_id, update.conversation_id);
    assert_eq!(state.maintenance_debt, update.maintenance_debt);
}

#[tokio::test]
async fn analytics_batch_error_rolls_back_prior_rows_and_releases_writer() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("global DB open");
    db.conn()
        .execute_batch(
            "CREATE TRIGGER fail_analytics_batch
             BEFORE INSERT ON analytics_events
             WHEN NEW.event_kind = 'force_failure'
             BEGIN
                 SELECT RAISE(ABORT, 'forced analytics failure');
             END;",
        )
        .await
        .unwrap();

    let event = |event_kind: &str| AnalyticsEventInsert {
        provider: "codex".to_string(),
        project_id: "project".to_string(),
        session_id: Some("session".to_string()),
        timestamp: 1,
        event_kind: event_kind.to_string(),
        hook_name: None,
        tool_name: None,
        tool_category: None,
        skill_name: None,
        hint_category: None,
        hint_id: None,
        outcome: None,
        metadata_json: None,
    };
    assert!(
        db.append_analytics_events(&[event("valid"), event("force_failure")])
            .await
            .is_err()
    );

    let mut rows = db
        .conn()
        .query("SELECT COUNT(*) FROM analytics_events", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    drop(rows);

    db.conn()
        .execute("DROP TRIGGER fail_analytics_batch", ())
        .await
        .unwrap();
    assert_eq!(
        db.append_analytics_events(&[event("after_failure")])
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn turn_batch_error_rolls_back_prior_rows_and_releases_writer() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = GlobalDb::open_at(&dir.path().join("global.db"))
        .await
        .expect("global DB open");
    db.conn()
        .execute_batch(
            "CREATE TRIGGER fail_turn_batch
             BEFORE INSERT ON turns
             WHEN NEW.message_id = 'force-failure'
             BEGIN
                 SELECT RAISE(ABORT, 'forced turn failure');
             END;",
        )
        .await
        .unwrap();

    let turn = |message_id: &str| crate::types::CostTurn {
        message_id: message_id.to_string(),
        project_hash: "project".to_string(),
        session_id: "session".to_string(),
        model: "test-model".to_string(),
        timestamp: 1,
        input_tokens: 1,
        output_tokens: 1,
        cache_write_tokens: 0,
        cache_read_tokens: 0,
        cost_usd: 0.01,
        category: "test".to_string(),
        tool_names: String::new(),
    };
    assert_eq!(
        db.insert_turns(&[turn("valid"), turn("force-failure")])
            .await,
        0
    );

    let mut rows = db
        .conn()
        .query("SELECT COUNT(*) FROM turns", ())
        .await
        .unwrap();
    assert_eq!(
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
        0
    );
    drop(rows);

    db.conn()
        .execute("DROP TRIGGER fail_turn_batch", ())
        .await
        .unwrap();
    assert_eq!(db.insert_turns(&[turn("after-failure")]).await, 1);
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
