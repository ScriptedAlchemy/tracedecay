use std::cell::RefCell;
use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tracedecay::storage::{default_profile_project_id, profile_sharded_data_root};

use crate::common;

pub(super) const PROCESS_TIMEOUT: Duration = Duration::from_secs(20);
pub(super) const CLIENT_COUNT: usize = 12;
pub(super) const CONCURRENT_CLIENTS_PER_PATH: usize = 4;

pub(super) struct ChildGuard(Child);

impl ChildGuard {
    pub(super) fn new(child: Child) -> Self {
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

pub(super) fn init_project(home: &Path, project: &Path, socket_path: &Path) -> PathBuf {
    std::fs::create_dir_all(project.join("src")).expect("create fixture source directory");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub fn broker_fixture() -> u32 { 42 }\n",
    )
    .expect("write fixture source");

    let output = common::tracedecay_command_with_home(home)
        .env("TRACEDECAY_DAEMON_SOCKET", socket_path)
        .arg("init")
        .current_dir(project)
        .output()
        .expect("tracedecay init should run");
    assert_command_success("tracedecay init", &output);

    let profile_root = home.join(".tracedecay");
    let data_root = profile_sharded_data_root(&profile_root, &default_profile_project_id(project));
    data_root.join(tracedecay::config::db_filename(&data_root))
}

pub(super) fn assert_command_success(label: &str, output: &std::process::Output) {
    assert!(
        output.status.success(),
        "{label} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn wait_for_socket(socket_path: &Path, child: &mut Child) {
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    common::poll_until(
        deadline,
        Duration::from_millis(25),
        || {
            if std::os::unix::net::UnixStream::connect(socket_path).is_ok() {
                return Some(());
            }
            if let Some(status) = child.try_wait().expect("read daemon status") {
                panic!("daemon exited before opening socket: {status}");
            }
            None
        },
        || "daemon socket did not become ready".to_string(),
    );
}

pub(super) fn spawn_daemon(home: &Path, socket_path: &Path) -> ChildGuard {
    let mut child = ChildGuard::new(
        common::tracedecay_command_with_home(home)
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

pub(super) fn spawn_daemon_with_stderr(
    home: &Path,
    socket_path: &Path,
    stderr: std::fs::File,
) -> ChildGuard {
    let mut child = ChildGuard::new(
        common::tracedecay_command_with_home(home)
            .args(["daemon", "run", "--socket"])
            .arg(socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .spawn()
            .expect("spawn daemon"),
    );
    wait_for_socket(socket_path, &mut child);
    child
}

pub(super) fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Replaces an unavailable daemon socket with a non-retryable endpoint.
///
/// Clients deliberately wait through an absent or refused socket to tolerate
/// a daemon restart. This test covers fail-closed behavior rather than that
/// restart grace, so it uses a self-referential symlink while preserving the
/// authoritative daemon record. The resulting `ELOOP` makes every client prove
/// it cannot fall back to a local writable store without serially consuming the
/// restart window.
pub(super) fn install_unavailable_socket_sentinel(socket_path: &Path) {
    match std::fs::remove_file(socket_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!(
            "remove stale daemon socket '{}': {error}",
            socket_path.display()
        ),
    }
    std::os::unix::fs::symlink(
        socket_path
            .file_name()
            .expect("daemon socket path must have a filename"),
        socket_path,
    )
    .unwrap_or_else(|error| {
        panic!(
            "write daemon socket sentinel '{}': {error}",
            socket_path.display()
        )
    });
    let error = std::os::unix::net::UnixStream::connect(socket_path)
        .expect_err("daemon socket sentinel must not accept connections");
    assert!(
        !matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
        ),
        "daemon socket sentinel must fail without restart grace: {error}"
    );
}

pub(super) fn wait_for_exit(child: &mut Child) -> Option<std::process::ExitStatus> {
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

enum ProxyOutput {
    Message(Value),
    InvalidJson { line: String, error: String },
    ReadError(String),
    Eof,
}

fn read_proxy_stdout(stdout: ChildStdout, output_tx: Sender<ProxyOutput>) {
    let mut stdout = BufReader::new(stdout);
    loop {
        let mut line = String::new();
        match stdout.read_line(&mut line) {
            Ok(0) => {
                let _ = output_tx.send(ProxyOutput::Eof);
                return;
            }
            Ok(_) => {
                let output = match serde_json::from_str(&line) {
                    Ok(message) => ProxyOutput::Message(message),
                    Err(error) => ProxyOutput::InvalidJson {
                        line,
                        error: error.to_string(),
                    },
                };
                if output_tx.send(output).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = output_tx.send(ProxyOutput::ReadError(error.to_string()));
                return;
            }
        }
    }
}

pub(super) struct McpProxy {
    child: ChildGuard,
    stdin: ChildStdin,
    output_rx: Receiver<ProxyOutput>,
    pending: BTreeMap<u64, Value>,
    stdout_reader: Option<JoinHandle<()>>,
}

impl McpProxy {
    pub(super) fn spawn(home: &Path, project: &Path, socket_path: &Path, ordinal: usize) -> Self {
        let mut child = ChildGuard::new(
            common::tracedecay_command_with_home(home)
                .env("TRACEDECAY_DAEMON_SOCKET", socket_path)
                .env(
                    "TRACEDECAY_CLIENT_INSTANCE_ID",
                    format!("broker-test-{ordinal}"),
                )
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
        let stdout = child.stdout.take().expect("proxy stdout");
        let (output_tx, output_rx) = std::sync::mpsc::channel();
        let stdout_reader = std::thread::spawn(move || read_proxy_stdout(stdout, output_tx));
        let mut proxy = Self {
            child,
            stdin,
            output_rx,
            pending: BTreeMap::new(),
            stdout_reader: Some(stdout_reader),
        };
        proxy.request(1, "initialize", json!({}));
        proxy.request(
            2,
            "tools/call",
            json!({"name": "tracedecay_status", "arguments": {"format": "json"}}),
        );
        proxy
    }

    #[cfg(target_os = "linux")]
    pub(super) fn pid(&self) -> u32 {
        self.child.id()
    }

    pub(super) fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
        .expect("write MCP request");
        self.stdin.flush().expect("flush MCP request");

        if let Some(response) = self.pending.remove(&id) {
            return assert_successful_response(id, response);
        }

        let deadline = Instant::now() + PROCESS_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.kill_for_timeout(id);
            }
            match self.output_rx.recv_timeout(remaining) {
                Ok(ProxyOutput::Message(response)) => {
                    let Some(response_id) = response.get("id").and_then(Value::as_u64) else {
                        continue;
                    };
                    if response_id == id {
                        return assert_successful_response(id, response);
                    }
                    self.pending.insert(response_id, response);
                }
                Ok(ProxyOutput::InvalidJson { line, error }) => {
                    panic!("invalid MCP response while waiting for {id}: {error}\nline: {line}")
                }
                Ok(ProxyOutput::ReadError(error)) => {
                    panic!("failed to read MCP response {id}: {error}")
                }
                Ok(ProxyOutput::Eof) => {
                    let status = self.child.try_wait().expect("read MCP proxy status");
                    panic!("MCP proxy exited before response {id}; status: {status:?}")
                }
                Err(RecvTimeoutError::Timeout) => self.kill_for_timeout(id),
                Err(RecvTimeoutError::Disconnected) => {
                    let status = self.child.try_wait().expect("read MCP proxy status");
                    panic!("MCP stdout reader stopped before response {id}; status: {status:?}")
                }
            }
        }
    }

    fn kill_for_timeout(&mut self, id: u64) -> ! {
        stop_child(&mut self.child);
        panic!("MCP request {id} exceeded {PROCESS_TIMEOUT:?}")
    }
}

impl Drop for McpProxy {
    fn drop(&mut self) {
        stop_child(&mut self.child);
        if let Some(stdout_reader) = self.stdout_reader.take() {
            let _ = stdout_reader.join();
        }
    }
}

fn assert_successful_response(id: u64, response: Value) -> Value {
    assert!(
        response.get("error").is_none(),
        "MCP request {id} failed: {response}"
    );
    response
}

#[cfg(target_os = "linux")]
pub(super) fn sqlite_handles(pid: u32, profile_root: &Path) -> Vec<PathBuf> {
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

pub(super) fn file_identity(path: &Path) -> Option<(u64, u64)> {
    std::fs::metadata(path)
        .ok()
        .map(|metadata| (metadata.dev(), metadata.ino()))
}

pub(super) fn storage_snapshot(db_path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut paths = vec![db_path.to_path_buf()];
    for suffix in ["-wal", "-shm"] {
        paths.push(PathBuf::from(format!("{}{suffix}", db_path.display())));
    }
    paths
        .into_iter()
        .filter_map(|path| std::fs::read(&path).ok().map(|bytes| (path, bytes)))
        .collect()
}

/// Compact digest of a storage family snapshot for assertion messages.
pub(super) fn storage_snapshot_digest(snapshot: &BTreeMap<PathBuf, Vec<u8>>) -> String {
    let mut hasher = DefaultHasher::new();
    for (path, bytes) in snapshot {
        path.hash(&mut hasher);
        bytes.hash(&mut hasher);
    }
    format!("{:016x}:{} files", hasher.finish(), snapshot.len())
}

pub(super) fn assert_storage_unchanged(
    label: &str,
    before: &BTreeMap<PathBuf, Vec<u8>>,
    db_path: &Path,
) {
    let after = storage_snapshot(db_path);
    assert_eq!(
        after,
        *before,
        "{label}: durable SQLite family changed\nbefore={}\nafter={}",
        storage_snapshot_digest(before),
        storage_snapshot_digest(&after),
    );
}

/// Blocks until the owner daemon's post-open maintenance (legacy memory
/// cutover receipts, repair passes) stops mutating the project store, so
/// byte-stability assertions measure only the window under test instead of
/// racing the owner's own startup writes.
pub(super) fn wait_for_quiescent_storage(db_path: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let previous = RefCell::new(storage_snapshot(db_path));
    let stable_samples = RefCell::new(0_u8);
    common::poll_until(
        deadline,
        Duration::ZERO,
        || {
            std::thread::sleep(Duration::from_millis(250));
            let current = storage_snapshot(db_path);
            if current == *previous.borrow() {
                let mut stable_samples = stable_samples.borrow_mut();
                *stable_samples += 1;
                (*stable_samples >= 8).then_some(current)
            } else {
                *previous.borrow_mut() = current;
                *stable_samples.borrow_mut() = 0;
                None
            }
        },
        || "project storage never became quiescent under the owner daemon".to_string(),
    )
}

pub(super) fn daemon_authority_record(home: &Path) -> Value {
    serde_json::from_slice(
        &std::fs::read(home.join(".tracedecay/daemon-authority.json"))
            .expect("read daemon authority record"),
    )
    .expect("parse daemon authority record")
}

pub(super) fn tool_status(home: &Path, project: &Path, socket_path: &Path) -> std::process::Output {
    let project_arg = project.to_string_lossy().to_string();
    common::tracedecay_command_with_home(home)
        .env("TRACEDECAY_DAEMON_SOCKET", socket_path)
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
