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
    let mut hook_event: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tracedecay-hooks/fixtures/host_events/cursor/after-file-edit.json"
    )))
    .expect("recorded Cursor native fixture");
    hook_event["file_path"] = json!(project_path.join("src/lib.rs"));
    hook_event["workspace_roots"] = json!([&project_path]);
    hook_event["edits"] = json!([{
        "old_string": "pub fn broker_fixture() -> u32 { 41 }",
        "new_string": "pub fn broker_fixture() -> u32 { 42 }",
    }]);
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
        for ordinal in 0..CONCURRENT_CLIENTS_PER_PATH {
            let home_path = &home_path;
            let project_path = &project_path;
            let socket_path = &socket_path;
            let mut hook_event = hook_event.clone();
            hook_event["conversation_id"] = json!(format!("conversation-{ordinal}"));
            hook_event["generation_id"] = json!(format!("generation-{ordinal}"));
            hook_event["session_id"] = json!(format!("session-{ordinal}"));
            hook_event["transcript_path"] =
                json!(home_path.join(format!("transcripts/session-{ordinal}.jsonl")));
            let hook_event = hook_event.to_string();
            let start = Arc::clone(&start);
            requests.push(scope.spawn(move || {
                start.wait();
                let stdout_path = home_path.join(format!("hook-{ordinal}.stdout.log"));
                let stderr_path = home_path.join(format!("hook-{ordinal}.stderr.log"));
                let stdout = std::fs::File::create(&stdout_path).expect("create hook stdout");
                let stderr = std::fs::File::create(&stderr_path).expect("create hook stderr");
                let mut hook = ChildGuard::new(
                    common::tracedecay_command_with_home(home_path)
                        .env("TRACEDECAY_DAEMON_SOCKET", socket_path)
                        .arg("hook-cursor-after-file-edit")
                        .current_dir(project_path)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::from(stdout))
                        .stderr(Stdio::from(stderr))
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
                let stdout = std::fs::read_to_string(stdout_path).expect("read hook stdout");
                let stderr = std::fs::read_to_string(stderr_path).expect("read hook stderr");
                assert!(
                    status.success(),
                    "hook client {ordinal} failed: {status}; stdout: {stdout}; stderr: {stderr}"
                );
                assert_eq!(stdout.trim(), "{}", "native capture transport response");
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
    let project_id = tracedecay_runtime_core::storage::default_profile_project_id(&project_path);
    let data_root =
        tracedecay_runtime_core::storage::profile_sharded_data_root(&profile_root, &project_id);
    let host = tracedecay_hooks::HookHostV1::CursorDesktop;
    let (_, capture_report) = tracedecay_hooks::HookSpoolV1::open(
        data_root.join("hook-v2-spool").join(host.hook_key()),
        tracedecay_hooks::HookSpoolConfigV1::stock(host),
        tracedecay_contracts::now_micros(),
    )
    .expect("open native capture receipt");
    // Sequence survives daemon replay/acknowledgement, so this proves each
    // callback was captured even if the owner already drained its envelope.
    assert_eq!(
        capture_report.next_sequence,
        1 + CONCURRENT_CLIENTS_PER_PATH as u64,
        "every native callback must be captured, not merely accepted as unbound"
    );
    let daemon_stderr = std::fs::read_to_string(&daemon_stderr_path).expect("read daemon stderr");
    assert!(
        !daemon_stderr.contains("database is locked"),
        "daemon encountered SQLite writer contention:\n{daemon_stderr}"
    );
}
