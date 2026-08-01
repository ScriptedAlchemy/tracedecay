#[tokio::test]
#[ignore]
async fn killed_writer_fixture() {
    if std::env::var("TRACEDECAY_BROKER_FIXTURE").as_deref() != Ok("killed-writer") {
        return;
    }
    let db_path = PathBuf::from(std::env::var_os("TRACEDECAY_FIXTURE_DB").expect("fixture DB"));
    let dirty_path = PathBuf::from(
        std::env::var_os("TRACEDECAY_FIXTURE_DIRTY").expect("fixture dirty sentinel"),
    );
    let ready_path =
        PathBuf::from(std::env::var_os("TRACEDECAY_FIXTURE_READY").expect("fixture ready path"));
    let authority = DatabaseAuthority::acquire_test(&db_path, "killed writer fixture")
        .expect("acquire fixture database authority");
    let (db, _) = Database::publish_test_runtime(
        &db_path,
        &authority,
        tracedecay::db::TestDatabaseRuntimeMode::Existing,
    )
    .await
    .expect("open fixture graph DB");
    db.insert_nodes(&[common::sample_node(
        "broker-recovery-node",
        "broker_recovery_node",
        "src/recovery.rs",
    )])
    .await
    .expect("commit recovery node");
    std::fs::write(
        &dirty_path,
        format!("pid={}\nversion=test", std::process::id()),
    )
    .expect("write dirty sentinel");
    std::fs::write(&ready_path, "ready").expect("publish fixture readiness");
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

#[test]
fn daemon_recovers_killed_writer_dirty_wal_before_serving_clients() {
    let home = TempDir::new().expect("temp home");
    let project = TempDir::new().expect("temp project");
    let fixture = TempDir::new().expect("temp fixture state");
    let home_path = common::canonical_existing_path(home.path());
    let project_path = common::canonical_existing_path(project.path());
    let socket_path = common::daemon_socket_path(&home_path);
    let mut initializer = spawn_daemon(&home_path, &socket_path);
    let db_path = init_project(&home_path, &project_path, &socket_path);
    stop_child(&mut initializer);
    let data_root = db_path.parent().expect("graph data root");
    let dirty_path = data_root.join("dirty");
    let ready_path = fixture.path().join("ready");

    let mut writer = ChildGuard::new(
        Command::new(std::env::current_exe().expect("current test binary"))
            .args([
                "--ignored",
                "--exact",
                "multi_connection_test::killed_writer_fixture",
                "--nocapture",
            ])
            .env("TRACEDECAY_BROKER_FIXTURE", "killed-writer")
            .env("TRACEDECAY_FIXTURE_DB", &db_path)
            .env("TRACEDECAY_FIXTURE_DIRTY", &dirty_path)
            .env("TRACEDECAY_FIXTURE_READY", &ready_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn writer fixture"),
    );
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    common::poll_until(
        deadline,
        Duration::from_millis(25),
        || {
            if ready_path.exists() {
                return Some(());
            }
            if let Some(status) = writer.try_wait().expect("writer status") {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut out) = writer.stdout.take() {
                    let _ = out.read_to_string(&mut stdout);
                }
                if let Some(mut err) = writer.stderr.take() {
                    let _ = err.read_to_string(&mut stderr);
                }
                panic!(
                    "writer exited early ({status}); db={}; stdout={stdout}; stderr={stderr}",
                    db_path.display()
                );
            }
            None
        },
        || "writer fixture did not become ready".to_string(),
    );
    assert!(
        PathBuf::from(format!("{}-wal", db_path.display())).exists(),
        "writer fixture must leave committed WAL frames"
    );
    stop_child(&mut writer);

    let wal_path = PathBuf::from(format!("{}-wal", db_path.display()));
    let shm_path = PathBuf::from(format!("{}-shm", db_path.display()));
    let committed_family = storage_snapshot(&db_path);
    for path in [&db_path, &wal_path, &shm_path] {
        assert!(
            committed_family.contains_key(path),
            "killed writer must leave SQLite family member '{}'",
            path.display()
        );
    }
    let mut corrupted = committed_family[&db_path].clone();
    corrupted[..16].copy_from_slice(b"not-a-sqlite-db!");
    std::fs::write(&db_path, corrupted).expect("corrupt fixture database header");
    let failed_family = storage_snapshot(&db_path);
    let failed_identities = [&db_path, &wal_path, &shm_path].map(|path| {
        (
            path.to_path_buf(),
            file_identity(path).expect("SQLite family identity before failed recovery"),
        )
    });

    let mut daemon = spawn_daemon(&home_path, &socket_path);
    let project_arg = project_path.to_string_lossy().to_string();
    let search_recovered_node = || {
        common::tracedecay_command_with_home(&home_path)
            .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
            .current_dir(&project_path)
            .args([
                "tool",
                "--project",
                &project_arg,
                "search",
                "broker_recovery_node",
                "--json",
            ])
            .output()
            .expect("search recovered node through daemon")
    };
    let failed = search_recovered_node();
    if failed.status.success() {
        assert!(
            String::from_utf8_lossy(&failed.stdout).contains("broker_recovery_node"),
            "successful SQLite WAL recovery must retain the committed row"
        );
        assert!(
            !dirty_path.exists(),
            "successful SQLite WAL recovery must clear the dirty sentinel"
        );
        stop_child(&mut daemon);
        return;
    }
    assert!(
        !failed.status.success(),
        "daemon must fail closed when killed-writer recovery cannot validate the database"
    );
    let after_failed_recovery = storage_snapshot(&db_path);
    for path in [&db_path, &wal_path] {
        assert_eq!(
            after_failed_recovery.get(path),
            failed_family.get(path),
            "failed recovery changed durable SQLite family member '{}'",
            path.display()
        );
    }
    // SQLite's SHM file contains volatile coordination state, so opening a
    // recovery probe may change its bytes. The identity checks below still
    // prove that failed recovery preserved rather than replaced every member.
    for (path, identity) in failed_identities {
        assert_eq!(
            file_identity(&path),
            Some(identity),
            "failed recovery replaced SQLite family member '{}'",
            path.display()
        );
    }
    assert!(
        dirty_path.exists(),
        "failed recovery must preserve the dirty sentinel"
    );
    stop_child(&mut daemon);

    std::fs::write(&db_path, &committed_family[&db_path])
        .expect("restore fixture database for successful WAL recovery");
    daemon = spawn_daemon(&home_path, &socket_path);
    let output = search_recovered_node();
    assert_command_success("daemon WAL recovery search", &output);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("broker_recovery_node"),
        "committed WAL row was lost during daemon recovery"
    );
    assert!(
        !dirty_path.exists(),
        "daemon must clear dirty sentinel after recovery"
    );
    stop_child(&mut daemon);
}
