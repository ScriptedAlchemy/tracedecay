#[test]
fn split_brain_is_rejected_and_unavailable_daemon_fails_closed_until_restart() {
    let home = TempDir::new().expect("temp home");
    let project = TempDir::new().expect("temp project");
    let home_path = common::canonical_existing_path(home.path());
    let project_path = common::canonical_existing_path(project.path());
    let socket_path = common::daemon_socket_path(&home_path);
    let mut owner = spawn_daemon(&home_path, &socket_path);
    let db_path = init_project(&home_path, &project_path, &socket_path);
    assert_command_success(
        "owner daemon status",
        &tool_status(&home_path, &project_path, &socket_path),
    );

    let socket_before = file_identity(&socket_path).expect("owner socket identity");
    let authority_before = daemon_authority_record(&home_path);
    let storage_before_contender = wait_for_quiescent_storage(&db_path);
    let mut contender = ChildGuard::new(
        common::tracedecay_command_with_home(&home_path)
            .args(["daemon", "run", "--socket"])
            .arg(&socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn contender daemon"),
    );
    let contender_status = wait_for_exit(&mut contender).unwrap_or_else(|| {
        stop_child(&mut contender);
        panic!("second daemon remained alive and created split-brain ownership")
    });
    assert!(
        !contender_status.success(),
        "second daemon must be rejected"
    );
    assert_eq!(
        file_identity(&socket_path),
        Some(socket_before),
        "contender replaced owner socket"
    );
    assert!(
        owner.try_wait().expect("owner status").is_none(),
        "owner daemon exited"
    );
    assert_eq!(
        daemon_authority_record(&home_path),
        authority_before,
        "rejected contender changed daemon authority generation"
    );
    assert_storage_unchanged(
        "rejected contender wrote through a competing or fallback database owner",
        &storage_before_contender,
        &db_path,
    );
    // Contender must be gone before the unavailable-daemon client probes; a
    // surviving second owner would still race the byte snapshot.
    assert!(
        contender.try_wait().expect("contender status").is_some(),
        "rejected contender is still running after fail-closed rejection"
    );

    stop_child(&mut owner);
    install_unavailable_socket_sentinel(&socket_path);
    let before = storage_snapshot(&db_path);
    for (label, mut command) in [
        ("tool", {
            let project_arg = project_path.to_string_lossy().to_string();
            let mut command = common::tracedecay_command_with_home(&home_path);
            command.env("TRACEDECAY_DAEMON_SOCKET", &socket_path).args([
                "tool",
                "--project",
                &project_arg,
                "status",
                "--json",
            ]);
            command
        }),
        ("sync", {
            let mut command = common::tracedecay_command_with_home(&home_path);
            command
                .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
                .arg("sync");
            command
        }),
        ("doctor", {
            let mut command = common::tracedecay_command_with_home(&home_path);
            command
                .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
                .arg("doctor");
            command
        }),
    ] {
        command
            .current_dir(&project_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = command
            .output()
            .unwrap_or_else(|error| panic!("run {label}: {error}"));
        assert!(
            !output.status.success(),
            "{label} must fail closed while daemon is unavailable\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert_storage_unchanged(
            &format!("{label} used a local SQLite fallback"),
            &before,
            &db_path,
        );
    }
    let hook_event = json!({
        "hook_event_name": "afterFileEdit",
        "file_path": project_path.join("src/lib.rs"),
        "workspace_roots": [&project_path],
    })
    .to_string();
    let mut hook = ChildGuard::new(
        common::tracedecay_command_with_home(&home_path)
            .env("TRACEDECAY_DAEMON_SOCKET", &socket_path)
            .arg("hook-cursor-after-file-edit")
            .current_dir(&project_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn unavailable-daemon hook client"),
    );
    let mut stdin = hook.stdin.take().expect("hook stdin");
    stdin
        .write_all(hook_event.as_bytes())
        .expect("write hook event");
    drop(stdin);
    assert!(
        wait_for_exit(&mut hook).is_some(),
        "unavailable-daemon hook client exceeded {PROCESS_TIMEOUT:?}"
    );
    assert_storage_unchanged(
        "hook used a local SQLite fallback while daemon was unavailable",
        &before,
        &db_path,
    );

    let mut restarted = spawn_daemon(&home_path, &socket_path);
    assert_command_success(
        "restarted daemon status",
        &tool_status(&home_path, &project_path, &socket_path),
    );
    let authority_after = daemon_authority_record(&home_path);
    assert_eq!(authority_after["pid"], restarted.id());
    assert_ne!(
        authority_after["process_run_id"], authority_before["process_run_id"],
        "restart must publish a new process identity"
    );
    assert_ne!(
        authority_after["auth_token"], authority_before["auth_token"],
        "restart must invalidate the prior generation's authentication token"
    );
    assert!(
        authority_after["epoch"].as_u64() > authority_before["epoch"].as_u64(),
        "restart must advance daemon authority epoch"
    );
    stop_child(&mut restarted);
}
