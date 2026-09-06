#[test]
fn twelve_mcp_cli_and_hook_clients_share_one_daemon_profile_store_owner() {
    let home = TempDir::new().expect("temp home");
    let project = TempDir::new().expect("temp project");
    let home_path = common::canonical_existing_path(home.path());
    let project_path = common::canonical_existing_path(project.path());
    let profile_root = home_path.join(".tracedecay");
    let socket_path = common::daemon_socket_path(&home_path);
    let daemon_stderr_path = home_path.join("daemon.stderr.log");
    let daemon_stderr = std::fs::File::create(&daemon_stderr_path).expect("create daemon stderr");
    let mut daemon =
        spawn_daemon_with_stderr(&home_path, &socket_path, &daemon_stderr_path, daemon_stderr);
    let profile_db_path = init_project(&home_path, &project_path, &socket_path);

    let mut clients = (0..CLIENT_COUNT)
        .map(|ordinal| McpProxy::spawn(&home_path, &project_path, &socket_path, ordinal))
        .collect::<Vec<_>>();

    #[cfg(target_os = "linux")]
    {
        for client in &clients {
            assert_eq!(
                sqlite_handles(client.pid(), &profile_root),
                Vec::<std::path::PathBuf>::new(),
                "MCP proxy must not own any profile SQLite handle"
            );
        }
        let daemon_handles = sqlite_handles(daemon.id(), &profile_root);
        assert!(
            daemon_handles.iter().any(|path| path == &profile_db_path),
            "daemon must own the canonical profile database; handles: {daemon_handles:?}"
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

    let db_identity = file_identity(&profile_db_path).expect("profile database identity");
    // `hook-cursor-after-file-edit` is a capture-only callback: the native
    // decoder reads Cursor's documented `afterFileEdit` shape and rejects an
    // identity subset as a malformed payload (exit 1), the contract
    // `cursor_after_file_edit_hook_captures_bound_spool_record` pins. Send the
    // recorded host shape, not just the fields this test reads.
    let hook_event = json!({
        "hook_event_name": "afterFileEdit",
        "conversation_id": "ownership-conversation",
        "generation_id": "ownership-generation",
        "model": "fixture-model",
        "file_path": project_path.join("src/lib.rs"),
        "edits": [{ "old_string": "", "new_string": "pub fn owned() {}\n" }],
        "session_id": "ownership-session",
        "cursor_version": "fixture",
        "workspace_roots": [&project_path],
        "transcript_path": null,
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
                let status = wait_for_exit(&mut tool).unwrap_or_else(|| {
                    panic!(
                        "tool client exceeded {PROCESS_TIMEOUT:?}\ndaemon stderr:\n{}",
                        daemon_stderr_tail()
                    )
                });
                assert!(
                    status.success(),
                    "brokered tool status failed\ndaemon stderr:\n{}",
                    daemon_stderr_tail()
                );
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
                        .stderr(Stdio::piped())
                        .spawn()
                        .expect("spawn hook client"),
                );
                let mut stdin = hook.stdin.take().expect("hook stdin");
                stdin
                    .write_all(hook_event.as_bytes())
                    .expect("write hook event");
                drop(stdin);
                let mut hook_stderr = hook.stderr.take().expect("hook stderr");
                let status = wait_for_exit(&mut hook).unwrap_or_else(|| {
                    panic!(
                        "hook client exceeded {PROCESS_TIMEOUT:?}\ndaemon stderr:\n{}",
                        daemon_stderr_tail()
                    )
                });
                let mut stderr = String::new();
                let _ = std::io::Read::read_to_string(&mut hook_stderr, &mut stderr);
                assert!(
                    status.success(),
                    "hook client failed with {:?}\nhook stderr:\n{stderr}\ndaemon stderr:\n{}",
                    status.code(),
                    daemon_stderr_tail()
                );
            }));
        }
        start.wait();
        for request in requests {
            request.join().expect("concurrent broker client panicked");
        }
    });

    let doctor = common::tracedecay_command_with_home(&home_path)
        .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
        // `doctor` takes no arguments: it checks every agent integration in one
        // pass, so the retired `--agent` selector is a hard parse failure.
        .arg("doctor")
        .current_dir(&project_path)
        .output()
        .expect("run doctor probe");
    assert!(
        doctor.status.success(),
        "brokered doctor failed\nstdout:\n{}\nstderr:\n{}\ndaemon stderr:\n{}",
        String::from_utf8_lossy(&doctor.stdout),
        String::from_utf8_lossy(&doctor.stderr),
        daemon_stderr_tail()
    );
    assert_eq!(
        file_identity(&profile_db_path),
        Some(db_identity),
        "client probes replaced the profile database inode"
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
            Vec::<std::path::PathBuf>::new(),
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
