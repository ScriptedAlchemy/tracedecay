#![cfg(unix)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;
use tracedecay::db::{Database, DatabaseAuthority, SQLITE_UNSAFE_FAST_ENV};
use tracedecay::storage::{default_profile_project_id, profile_sharded_data_root};

use crate::common;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(20);
const CLIENT_COUNT: usize = 12;
const CONCURRENT_CLIENTS_PER_PATH: usize = 4;

struct ChildGuard(Child);

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self(child)
    }
}

impl std::ops::Deref for ChildGuard {
    type Target = Child;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        stop_child(&mut self.0);
    }
}

fn init_project(home: &Path, project: &Path, socket_path: &Path) -> PathBuf {
    std::fs::create_dir_all(project.join("src")).expect("create fixture source directory");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn broker_fixture() -> u32 { 42 }\n",
    )
    .expect("write fixture source");

    let output = common::tracedecay_command_with_home(home)
        .env("TRACEDECAY_DAEMON_SOCKET", socket_path)
        .env_remove(SQLITE_UNSAFE_FAST_ENV)
        .arg("init")
        .current_dir(project)
        .output()
        .expect("tracedecay init should run");
    assert_command_success("tracedecay init", &output);

    let profile_root = home.join(".tracedecay");
    let data_root = profile_sharded_data_root(&profile_root, &default_profile_project_id(project));
    data_root.join(tracedecay::config::db_filename(&data_root))
}

fn assert_command_success(label: &str, output: &std::process::Output) {
    assert!(
        output.status.success(),
        "{label} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn wait_for_socket(socket_path: &Path, child: &mut Child) {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        if std::os::unix::net::UnixStream::connect(socket_path).is_ok() {
            return;
        }
        if let Some(status) = child.try_wait().expect("read daemon status") {
            panic!("daemon exited before opening socket: {status}");
        }
        assert!(
            Instant::now() < deadline,
            "daemon socket did not become ready"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn spawn_daemon(home: &Path, socket_path: &Path) -> ChildGuard {
    let mut child = ChildGuard::new(
        common::tracedecay_command_with_home(home)
            .env_remove(SQLITE_UNSAFE_FAST_ENV)
            .args(["daemon", "run", "--socket"])
            .arg(socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn daemon"),
    );
    wait_for_socket(socket_path, &mut child);
    child
}

fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn wait_for_exit(child: &mut Child) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().expect("read child status") {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

struct McpProxy {
    child: ChildGuard,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl McpProxy {
    fn spawn(home: &Path, project: &Path, socket_path: &Path, ordinal: usize) -> Self {
        let mut child = ChildGuard::new(
            common::tracedecay_command_with_home(home)
                .env("TRACEDECAY_DAEMON_SOCKET", socket_path)
                .env(
                    "TRACEDECAY_CLIENT_INSTANCE_ID",
                    format!("broker-test-{ordinal}"),
                )
                .env_remove(SQLITE_UNSAFE_FAST_ENV)
                .args(["serve", "--path"])
                .arg(project)
                .current_dir(project)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn MCP proxy"),
        );
        let stdin = child.stdin.take().expect("proxy stdin");
        let stdout = BufReader::new(child.stdout.take().expect("proxy stdout"));
        let mut proxy = Self {
            child,
            stdin,
            stdout,
        };
        proxy.request(1, "initialize", json!({}));
        proxy.request(
            2,
            "tools/call",
            json!({"name": "tracedecay_status", "arguments": {"format": "json"}}),
        );
        proxy
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
        .expect("write MCP request");
        self.stdin.flush().expect("flush MCP request");

        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let proxy_pid = self.child.id();
        let watchdog = std::thread::spawn(move || {
            if matches!(
                done_rx.recv_timeout(PROCESS_TIMEOUT),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ) {
                let _ = Command::new("kill")
                    .args(["-KILL", &proxy_pid.to_string()])
                    .status();
            }
        });
        let mut matched = None;
        for _ in 0..32 {
            let mut line = String::new();
            let bytes = self.stdout.read_line(&mut line).expect("read MCP response");
            assert_ne!(bytes, 0, "MCP proxy exited before response {id}");
            let response: Value = serde_json::from_str(&line).expect("valid MCP response");
            if response.get("id").and_then(Value::as_u64) == Some(id) {
                matched = Some(response);
                break;
            }
        }
        let _ = done_tx.send(());
        watchdog.join().expect("MCP response watchdog panicked");
        let response = matched.unwrap_or_else(|| {
            panic!("MCP response {id} was hidden behind too many notifications")
        });
        assert!(
            response.get("error").is_none(),
            "MCP request {id} failed: {response}"
        );
        response
    }
}

#[cfg(target_os = "linux")]
fn sqlite_handles(pid: u32, profile_root: &Path) -> Vec<PathBuf> {
    let mut handles = Vec::new();
    let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return handles;
    };
    for entry in entries.flatten() {
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let rendered = target.to_string_lossy();
        if target.starts_with(profile_root)
            && (rendered.contains(".db")
                || rendered.ends_with("-wal")
                || rendered.ends_with("-shm"))
        {
            handles.push(target);
        }
    }
    handles.sort();
    handles
}

fn file_identity(path: &Path) -> Option<(u64, u64)> {
    std::fs::metadata(path)
        .ok()
        .map(|metadata| (metadata.dev(), metadata.ino()))
}

fn storage_snapshot(db_path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut paths = vec![db_path.to_path_buf()];
    for suffix in ["-wal", "-shm"] {
        paths.push(PathBuf::from(format!("{}{suffix}", db_path.display())));
    }
    paths
        .into_iter()
        .filter_map(|path| std::fs::read(&path).ok().map(|bytes| (path, bytes)))
        .collect()
}

fn daemon_authority_record(home: &Path) -> Value {
    serde_json::from_slice(
        &std::fs::read(home.join(".tracedecay/daemon-authority.json"))
            .expect("read daemon authority record"),
    )
    .expect("parse daemon authority record")
}

fn tool_status(home: &Path, project: &Path, socket_path: &Path) -> std::process::Output {
    let project_arg = project.to_string_lossy().to_string();
    common::tracedecay_command_with_home(home)
        .env("TRACEDECAY_DAEMON_SOCKET", socket_path)
        .env_remove(SQLITE_UNSAFE_FAST_ENV)
        .current_dir(project)
        .args([
            "tool",
            "--project",
            &project_arg,
            "status",
            "--json",
            "--format",
            "json",
        ])
        .output()
        .expect("run tool status")
}

#[test]
fn twelve_mcp_cli_and_hook_clients_share_one_daemon_sqlite_owner() {
    let home = TempDir::new().expect("temp home");
    let project = TempDir::new().expect("temp project");
    let home_path = common::canonical_existing_path(home.path());
    let project_path = common::canonical_existing_path(project.path());
    let profile_root = home_path.join(".tracedecay");
    let socket_path = common::daemon_socket_path(&home_path);
    let mut daemon = spawn_daemon(&home_path, &socket_path);
    let db_path = init_project(&home_path, &project_path, &socket_path);

    let mut clients = (0..CLIENT_COUNT)
        .map(|ordinal| McpProxy::spawn(&home_path, &project_path, &socket_path, ordinal))
        .collect::<Vec<_>>();

    #[cfg(target_os = "linux")]
    {
        for client in &clients {
            assert_eq!(
                sqlite_handles(client.child.id(), &profile_root),
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
                        .env_remove(SQLITE_UNSAFE_FAST_ENV)
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
                        .env_remove(SQLITE_UNSAFE_FAST_ENV)
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
        .env_remove(SQLITE_UNSAFE_FAST_ENV)
        .arg("doctor")
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
            sqlite_handles(client.child.id(), &profile_root),
            Vec::<PathBuf>::new(),
            "MCP proxy retained a profile SQLite handle after its request"
        );
    }
    stop_child(&mut daemon);
}

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
    let mut contender = ChildGuard::new(
        common::tracedecay_command_with_home(&home_path)
            .env_remove(SQLITE_UNSAFE_FAST_ENV)
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

    stop_child(&mut owner);
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
            .env_remove(SQLITE_UNSAFE_FAST_ENV)
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
        assert_eq!(
            storage_snapshot(&db_path),
            before,
            "{label} used a local SQLite fallback"
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
            .env_remove(SQLITE_UNSAFE_FAST_ENV)
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
    assert_eq!(
        storage_snapshot(&db_path),
        before,
        "hook used a local SQLite fallback while daemon was unavailable"
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
    let (db, _) = Database::open(&db_path, &authority)
        .await
        .expect("open fixture graph DB");
    db.conn()
        .execute_batch("PRAGMA wal_autocheckpoint = 0")
        .await
        .expect("disable WAL autocheckpoint");
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
            .env_remove(SQLITE_UNSAFE_FAST_ENV)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn writer fixture"),
    );
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    while !ready_path.exists() {
        assert!(
            writer.try_wait().expect("writer status").is_none(),
            "writer exited early"
        );
        assert!(
            Instant::now() < deadline,
            "writer fixture did not become ready"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
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
            .env_remove(SQLITE_UNSAFE_FAST_ENV)
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
    assert!(
        !failed.status.success(),
        "daemon must fail closed when killed-writer recovery cannot validate the database"
    );
    assert_eq!(
        storage_snapshot(&db_path),
        failed_family,
        "failed recovery changed DB, WAL, or SHM"
    );
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
