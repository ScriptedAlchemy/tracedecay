#[test]
fn twelve_mcp_cli_and_hook_clients_share_one_daemon_sqlite_owner() {
    let home = TempDir::new().expect("temp home");
    let project = TempDir::new().expect("temp project");
    let home_path = common::canonical_existing_path(home.path());
    let project_path = common::canonical_existing_path(project.path());
    let profile_root = home_path.join(".tracedecay");
    let socket_path = common::daemon_socket_path(&home_path);
    let daemon_stderr_path = home_path.join("daemon.stderr.log");
    let daemon_stderr = std::fs::File::create(&daemon_stderr_path).expect("create daemon stderr");
    let mut daemon = spawn_daemon_with_stderr(&home_path, &socket_path, daemon_stderr);
    let db_path = init_project(&home_path, &project_path, &socket_path);

    let mut clients = (0..CLIENT_COUNT)
        .map(|ordinal| McpProxy::spawn(&home_path, &project_path, &socket_path, ordinal))
        .collect::<Vec<_>>();

    #[cfg(target_os = "linux")]
    {
        for client in &clients {
            assert_eq!(
                sqlite_handles(client.pid(), &profile_root),
                Vec::<PathBuf>::new(),
                "MCP proxy must not own any profile SQLite handle"
            );
        }
        let daemon_handles = sqlite_handles(daemon.id(), &profile_root);
        assert!(
            daemon_handles.iter().any(|path| path == &db_path),
            "daemon must own the graph DB; handles: {daemon_handles:?}"
        );
    }
    let authority_before = daemon_authority_record(&home_path);
    assert_eq!(
        authority_before["pid"],
        daemon.id(),
        "profile authority must name the daemon"
    );
    assert_eq!(
        authority_before["profile_root"].as_str(),
        profile_root.to_str(),
        "profile authority must use the canonical profile root"
    );
    assert!(
        authority_before["epoch"]
            .as_u64()
            .is_some_and(|epoch| epoch > 0),
        "profile authority must publish a nonzero epoch"
    );

    let db_identity = file_identity(&db_path).expect("graph DB identity");
    let hook_event = json!({
        "hook_event_name": "afterFileEdit",
        "file_path": project_path.join("src/lib.rs"),
        "workspace_roots": [&project_path],
    })
    .to_string();
    std::thread::scope(|scope| {
        let start = Arc::new(Barrier::new(3 * CONCURRENT_CLIENTS_PER_PATH + 1));
        let mut requests = Vec::new();
        for (ordinal, client) in clients
            .iter_mut()
            .take(CONCURRENT_CLIENTS_PER_PATH)
            .enumerate()
        {
            let start = Arc::clone(&start);
            requests.push(scope.spawn(move || {
                start.wait();
                client.request(
                    100 + ordinal as u64,
                    "tools/call",
                    json!({"name": "tracedecay_status", "arguments": {"format": "json"}}),
                );
            }));
        }
        for _ in 0..CONCURRENT_CLIENTS_PER_PATH {
            let home_path = &home_path;
            let project_path = &project_path;
            let socket_path = &socket_path;
            let start = Arc::clone(&start);
            requests.push(scope.spawn(move || {
                start.wait();
                let project_arg = project_path.to_string_lossy().to_string();
                let mut tool = ChildGuard::new(
                    common::tracedecay_command_with_home(home_path)
                        .env("TRACEDECAY_DAEMON_SOCKET", socket_path)
                        .current_dir(project_path)
                        .args([
                            "tool",
                            "--project",
                            &project_arg,
                            "status",
                            "--json",
                            "--format",
                            "json",
                        ])
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .expect("spawn brokered tool status"),
                );
                let status = wait_for_exit(&mut tool)
                    .unwrap_or_else(|| panic!("tool client exceeded {PROCESS_TIMEOUT:?}"));
                assert!(status.success(), "brokered tool status failed");
            }));
        }
        for _ in 0..CONCURRENT_CLIENTS_PER_PATH {
            let home_path = &home_path;
            let project_path = &project_path;
            let socket_path = &socket_path;
            let hook_event = &hook_event;
            let start = Arc::clone(&start);
            requests.push(scope.spawn(move || {
                start.wait();
                let mut hook = ChildGuard::new(
                    common::tracedecay_command_with_home(home_path)
                        .env("TRACEDECAY_DAEMON_SOCKET", socket_path)
                        .arg("hook-cursor-after-file-edit")
                        .current_dir(project_path)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                        .expect("spawn hook client"),
                );
                let mut stdin = hook.stdin.take().expect("hook stdin");
                stdin
                    .write_all(hook_event.as_bytes())
                    .expect("write hook event");
                drop(stdin);
                let status = wait_for_exit(&mut hook)
                    .unwrap_or_else(|| panic!("hook client exceeded {PROCESS_TIMEOUT:?}"));
                assert!(status.success(), "hook client failed");
            }));
        }
        start.wait();
        for request in requests {
            request.join().expect("concurrent broker client panicked");
        }
    });

    let doctor = common::tracedecay_command_with_home(&home_path)
        .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
        .arg("doctor")
        .args(["--agent", "claude"])
        .current_dir(&project_path)
        .output()
        .expect("run doctor probe");
    assert_command_success("brokered doctor", &doctor);
    assert_eq!(
        file_identity(&db_path),
        Some(db_identity),
        "client probes replaced graph DB inode"
    );
    assert_eq!(
        daemon_authority_record(&home_path),
        authority_before,
        "concurrent clients changed daemon owner or epoch"
    );
    #[cfg(target_os = "linux")]
    for client in &clients {
        assert_eq!(
            sqlite_handles(client.pid(), &profile_root),
            Vec::<PathBuf>::new(),
            "MCP proxy retained a profile SQLite handle after its request"
        );
    }
    stop_child(&mut daemon);
    let daemon_stderr = std::fs::read_to_string(&daemon_stderr_path).expect("read daemon stderr");
    assert!(
        !daemon_stderr.contains("database is locked"),
        "daemon encountered SQLite writer contention:\n{daemon_stderr}"
    );
}
