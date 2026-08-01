#![allow(dead_code)]

pub mod fixture;

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::Read;
#[cfg(not(windows))]
use std::io::Write;
use std::net::TcpListener;
#[cfg(not(unix))]
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
#[cfg(not(windows))]
use tempfile::NamedTempFile;
use tempfile::TempDir;
use tokio::sync::OnceCell;
use tracedecay::application::host_admission::{
    HostAdmissionOutcome, HostAdmissionScope, HostAdmissionTestRuntimeV1,
};
use tracedecay::config::USER_DATA_DIR_ENV;
use tracedecay::db::{Database, DatabaseAuthority, TestDatabaseRuntimeMode};
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
use tracedecay::types::{Node, NodeKind, Visibility};

/// Host-installer source and template assets that live in
/// `crates/tracedecay-agent-hosts`. Tests assert over the *source* of the
/// generated guidance, so every suite must read the same authority; keeping the
/// `include_str!` sites here means one edit repoints them all after a move.
pub mod host_sources {
    pub const HERMES_PLUGIN_INIT_PY: &str = include_str!(
        "../../crates/tracedecay-agent-hosts/src/agents/hermes/templates/plugin_init.py"
    );
    pub const HERMES_SKILL_MD: &str =
        include_str!("../../crates/tracedecay-agent-hosts/src/agents/hermes/templates/skill.md");
}

static EMPTY_LCM_DB_TEMPLATE: OnceCell<Vec<u8>> = OnceCell::const_new();
static EMPTY_GLOBAL_DB_TEMPLATE: OnceCell<Vec<u8>> = OnceCell::const_new();
static EMPTY_GRAPH_DB_TEMPLATE: OnceCell<Vec<u8>> = OnceCell::const_new();

pub async fn initialize_test_database(path: &Path) -> tracedecay::errors::Result<(Database, bool)> {
    let authority = DatabaseAuthority::acquire_test(path, "integration test initialize")?;
    Database::publish_test_runtime(path, &authority, TestDatabaseRuntimeMode::Initialize).await
}

pub async fn open_test_database(path: &Path) -> tracedecay::errors::Result<(Database, bool)> {
    let authority = DatabaseAuthority::acquire_test(path, "integration test open")?;
    Database::publish_test_runtime(path, &authority, TestDatabaseRuntimeMode::Existing).await
}

pub async fn open_test_database_read_only(
    path: &Path,
) -> tracedecay::errors::Result<(Database, bool)> {
    let authority = DatabaseAuthority::acquire_test(path, "integration test read-only open")?;
    Database::publish_test_runtime(path, &authority, TestDatabaseRuntimeMode::ReadOnly).await
}

/// Sets (or removes) an environment variable for its lifetime, restoring the
/// previous value on drop.
pub struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    pub fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }

    /// Removes `key` for the guard's lifetime, so tests can exercise the
    /// no-override path.
    pub fn unset(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

/// Env var pinning the global DB path; tests that set it serialize on
/// [`GLOBAL_DB_ENV_LOCK`].
pub const GLOBAL_DB_ENV: &str = "TRACEDECAY_GLOBAL_DB";

/// Serializes tests within one binary that mutate process-wide env vars.
///
/// Prefer [`IsolatedEnv`], which bundles this serialization with a throwaway
/// home and [`TraceDecayStorageEnvGuard`]; reach for this raw lock only when
/// a test needs finer-grained control over which env vars it swaps.
pub static GLOBAL_DB_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquires `mutex`, recovering the guard even when a prior holder panicked.
pub fn lock_recovering_poison<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

/// Serializes tests that pin [`GLOBAL_DB_ENV`], tolerating a poisoned lock.
pub fn lock_global_db_env() -> std::sync::MutexGuard<'static, ()> {
    lock_recovering_poison(&GLOBAL_DB_ENV_LOCK)
}

/// Serializes [`IsolatedEnv`] users within one test binary: storage isolation
/// swaps process-wide env vars (`HOME`, `TRACEDECAY_DATA_DIR`, ...), so tests
/// must not overlap.
static ISOLATED_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The canonical way to isolate env-mutating tests: serializes tests within
/// one binary and keeps every test's project registration, store manifests,
/// and branch-meta writes inside a throwaway home instead of the developer's
/// real `~/.tracedecay` profile store.
///
/// Construct via [`IsolatedEnv::acquire`] (async tests) or
/// [`IsolatedEnv::acquire_blocking`] (sync tests); both return the guard plus
/// a ready-made `project` directory inside the temp home.
pub struct IsolatedEnv {
    // Field order matters: fields drop in declaration order, so the lock must
    // be declared last. Dropping it first would let the next waiting test
    // install its own isolated env, only for `storage`'s restore to clobber it.
    storage: TraceDecayStorageEnvGuard,
    dir: TempDir,
    _env_lock: tokio::sync::MutexGuard<'static, ()>,
}

impl IsolatedEnv {
    fn build(env_lock: tokio::sync::MutexGuard<'static, ()>) -> (Self, PathBuf) {
        let dir = tempdir_or_panic();
        let storage = TraceDecayStorageEnvGuard::for_tempdir(&dir);
        let project = dir.path().join("project");
        fs::create_dir_all(&project).unwrap_or_else(|err| {
            panic!(
                "failed to create isolated project directory '{}': {err}",
                project.display()
            )
        });
        (
            Self {
                storage,
                dir,
                _env_lock: env_lock,
            },
            project,
        )
    }

    pub async fn acquire() -> (Self, PathBuf) {
        Self::build(ISOLATED_ENV_LOCK.lock().await)
    }

    /// Sync counterpart of [`IsolatedEnv::acquire`] for plain `#[test]` fns.
    ///
    /// Warning: this uses `blocking_lock`, which panics if called from within
    /// an async context — use [`IsolatedEnv::acquire`] there instead.
    pub fn acquire_blocking() -> (Self, PathBuf) {
        Self::build(ISOLATED_ENV_LOCK.blocking_lock())
    }

    pub fn home(&self) -> &Path {
        self.storage.home()
    }

    /// The throwaway directory holding the isolated home and every checkout, so
    /// a fixture can place siblings of its project (a bare `origin`, a linked
    /// worktree) inside the same disposable tree.
    pub fn scratch(&self) -> &Path {
        self.dir.path()
    }
}

/// Sets [`GLOBAL_DB_ENV`] to a test DB path for the guard's lifetime.
pub struct GlobalDbEnvGuard {
    _env_guard: EnvVarGuard,
}

impl GlobalDbEnvGuard {
    pub fn set(db_path: impl AsRef<Path>) -> Self {
        let db_path = canonicalize_test_db_path(db_path.as_ref());
        Self {
            _env_guard: EnvVarGuard::set(GLOBAL_DB_ENV, db_path),
        }
    }
}

/// Isolates TraceDecay user/profile storage and the global DB under one test home.
///
/// Callers that may run concurrently with other env-mutating tests should hold
/// [`GLOBAL_DB_ENV_LOCK`] while this guard is alive.
pub struct TraceDecayStorageEnvGuard {
    home: PathBuf,
    profile_root: PathBuf,
    global_db_path: PathBuf,
    _home_guard: EnvVarGuard,
    _userprofile_guard: EnvVarGuard,
    _data_dir_guard: EnvVarGuard,
    _global_db_guard: GlobalDbEnvGuard,
    _holder_scan_guard: EnvVarGuard,
}

impl TraceDecayStorageEnvGuard {
    pub fn set(home: impl AsRef<Path>) -> Self {
        let home = canonicalize_test_dir(home.as_ref());
        let profile_root = canonicalize_test_dir(&home.join(".tracedecay"));
        let global_db_path = canonicalize_test_db_path(&profile_root.join("global.db"));

        Self {
            home: home.clone(),
            profile_root: profile_root.clone(),
            global_db_path: global_db_path.clone(),
            _home_guard: EnvVarGuard::set("HOME", &home),
            _userprofile_guard: EnvVarGuard::set("USERPROFILE", &home),
            _data_dir_guard: EnvVarGuard::set(USER_DATA_DIR_ENV, &profile_root),
            _global_db_guard: GlobalDbEnvGuard::set(&global_db_path),
            _holder_scan_guard: EnvVarGuard::set(
                "TRACEDECAY_TEST_ALLOW_INCOMPLETE_HOLDER_SCAN",
                "1",
            ),
        }
    }

    pub fn for_tempdir(tmp: &TempDir) -> Self {
        Self::set(tmp.path().join("home"))
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn profile_root(&self) -> &Path {
        &self.profile_root
    }

    pub fn global_db_path(&self) -> &Path {
        &self.global_db_path
    }
}

pub fn isolated_tracedecay_storage(tmp: &TempDir) -> TraceDecayStorageEnvGuard {
    TraceDecayStorageEnvGuard::for_tempdir(tmp)
}

/// Serializes in-process agent install/uninstall tests and pins
/// [`USER_DATA_DIR_ENV`] to that test's home.
///
/// Without this, concurrent `cargo test` cases can point each other at the same
/// managed-skill target file and race during atomic rewrites. Field order keeps
/// the env pin alive until just before the lock is released.
pub struct AgentEnvLock {
    _pin: EnvVarGuard,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

impl AgentEnvLock {
    /// Pins [`USER_DATA_DIR_ENV`] to `<home>/.tracedecay` while holding
    /// [`PROCESS_ENV_LOCK`].
    pub fn pin(home: impl AsRef<Path>) -> Self {
        let lock = PROCESS_ENV_LOCK.blocking_lock();
        let pin = EnvVarGuard::set(USER_DATA_DIR_ENV, home.as_ref().join(".tracedecay"));
        Self {
            _pin: pin,
            _lock: lock,
        }
    }
}

fn canonicalize_test_dir(path: &Path) -> PathBuf {
    fs::create_dir_all(path).unwrap_or_else(|err| {
        panic!(
            "failed to create test directory '{}': {err}",
            path.display()
        )
    });
    path.canonicalize().unwrap_or_else(|err| {
        panic!(
            "failed to canonicalize test directory '{}': {err}",
            path.display()
        )
    })
}

fn canonicalize_test_db_path(path: &Path) -> PathBuf {
    let parent = path
        .parent()
        .unwrap_or_else(|| panic!("test DB path '{}' has no parent", path.display()));
    canonicalize_test_dir(parent).join(
        path.file_name()
            .unwrap_or_else(|| panic!("test DB path '{}' has no file name", path.display())),
    )
}

pub fn canonical_existing_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

pub fn tempdir_or_panic() -> TempDir {
    match TempDir::new() {
        Ok(dir) => dir,
        Err(err) => panic!("failed to create temp dir: {err}"),
    }
}

pub fn fake_codex_bin(temp: &Path) -> PathBuf {
    temp.join(if cfg!(windows) { "codex.cmd" } else { "codex" })
}

#[cfg(windows)]
pub fn install_fake_codex_launcher(_script: &Path, bin: &Path) {
    fs::write(bin, windows_python_launcher("codex.py")).unwrap_or_else(|err| {
        panic!(
            "failed to install fake codex launcher {}: {err}",
            bin.display()
        )
    });
}

#[cfg(not(windows))]
pub fn install_fake_codex_launcher(script: &Path, bin: &Path) {
    let script_name = script
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_else(|| {
            panic!(
                "fake codex script has no valid file name: {}",
                script.display()
            )
        });
    let launcher = format!(
        "#!/bin/sh\n\
         SCRIPT_DIR=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\n\
         exec python3 \"$SCRIPT_DIR/{script_name}\" \"$@\"\n"
    );
    write_executable_atomically(bin, launcher.as_bytes()).unwrap_or_else(|err| {
        panic!(
            "failed to install fake codex launcher {} for {}: {err}",
            bin.display(),
            script.display()
        )
    });
}

#[cfg(not(windows))]
fn write_executable_atomically(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = NamedTempFile::new_in(parent)?;
    tmp.write_all(contents)?;
    make_executable_file(tmp.as_file())?;
    tmp.as_file_mut().sync_all()?;
    tmp.persist(path).map_err(|err| err.error)?;
    Ok(())
}

#[cfg(unix)]
fn make_executable_file(file: &File) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = file.metadata()?.permissions();
    permissions.set_mode(0o755);
    file.set_permissions(permissions)
}

#[cfg(not(unix))]
fn make_executable_file(_file: &File) -> std::io::Result<()> {
    Ok(())
}

pub fn windows_python_launcher(script_name: &str) -> String {
    format!(
        "@echo off\r\n\
setlocal\r\n\
if defined Python_ROOT_DIR if exist \"%Python_ROOT_DIR%\\python.exe\" (\r\n\
  \"%Python_ROOT_DIR%\\python.exe\" \"%~dp0{script_name}\" %*\r\n\
  exit /b %ERRORLEVEL%\r\n\
)\r\n\
if defined pythonLocation if exist \"%pythonLocation%\\python.exe\" (\r\n\
  \"%pythonLocation%\\python.exe\" \"%~dp0{script_name}\" %*\r\n\
  exit /b %ERRORLEVEL%\r\n\
)\r\n\
where python >nul 2>nul\r\n\
if not errorlevel 1 (\r\n\
  python \"%~dp0{script_name}\" %*\r\n\
  exit /b %ERRORLEVEL%\r\n\
)\r\n\
where python3 >nul 2>nul\r\n\
if not errorlevel 1 (\r\n\
  python3 \"%~dp0{script_name}\" %*\r\n\
  exit /b %ERRORLEVEL%\r\n\
)\r\n\
py -3 \"%~dp0{script_name}\" %*\r\n\
exit /b %ERRORLEVEL%\r\n"
    )
}

pub fn sample_node(id: &str, name: &str, file_path: &str) -> Node {
    Node {
        id: id.to_string(),
        kind: NodeKind::Function,
        name: name.to_string(),
        qualified_name: format!("crate::{name}"),
        file_path: file_path.to_string(),
        start_line: 1,
        attrs_start_line: 1,
        end_line: 3,
        start_column: 0,
        end_column: 1,
        signature: Some(format!("fn {name}()")),
        docstring: None,
        visibility: Visibility::Pub,
        is_async: false,
        branches: 0,
        loops: 0,
        returns: 0,
        max_nesting: 0,
        unsafe_blocks: 0,
        unchecked_calls: 0,
        assertions: 0,
        updated_at: 1_800_000_000,
        parent_id: None,
    }
}

/// Small multi-thread runtime for `#[test]`-driven async dashboard fixtures.
pub fn create_runtime() -> tokio::runtime::Runtime {
    match tokio::runtime::Builder::new_multi_thread()
        // Dashboard tests issue synchronous ureq calls while the in-process
        // server and database handlers use this same runtime. Two workers can
        // deadlock under load (one blocked in ureq, one awaiting DB work).
        .worker_threads(4)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => panic!("failed to create tokio runtime: {err}"),
    }
}

pub fn pick_free_port() -> u16 {
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(err) => panic!("failed to bind free local port: {err}"),
    };
    match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(err) => panic!("failed to read bound local address: {err}"),
    }
}

pub fn http_agent() -> ureq::Agent {
    http_agent_with_timeout(Duration::from_secs(4))
}

pub fn http_agent_with_timeout(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(timeout))
        .build()
        .into()
}

pub struct DaemonProcess {
    child: Child,
}

impl DaemonProcess {
    fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Force-stops the daemon and reaps its process before returning.
    ///
    /// `Child::kill` maps to `SIGKILL` on Unix and the platform termination
    /// primitive elsewhere, keeping fault-injection tests portable.
    pub fn kill_and_wait(&mut self) -> std::io::Result<ExitStatus> {
        terminate_and_reap(&mut self.child)
    }

    fn drain_stderr(&mut self) {
        let Some(mut stderr) = self.child.stderr.take() else {
            return;
        };
        std::thread::spawn(move || {
            if let Some(path) = std::env::var_os("TRACEDECAY_TEST_DAEMON_LOG")
                && let Ok(mut file) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
            {
                let _ = std::io::copy(&mut stderr, &mut file);
                return;
            }
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
        });
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = terminate_and_reap(&mut self.child);
    }
}

fn terminate_and_reap(child: &mut Child) -> std::io::Result<ExitStatus> {
    if let Some(status) = child.try_wait()? {
        return Ok(status);
    }

    if let Err(kill_err) = child.kill() {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        return Err(kill_err);
    }

    child.wait()
}

pub fn apply_tracedecay_home_env(command: &mut Command, home: &Path) {
    let home = canonical_existing_path(home);
    command
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env(USER_DATA_DIR_ENV, home.join(".tracedecay"))
        .env(GLOBAL_DB_ENV, home.join(".tracedecay/global.db"));
}

pub fn tracedecay_command_with_home(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tracedecay"));
    apply_tracedecay_home_env(&mut command, home);
    command
}

thread_local! {
    static TEST_DAEMONS: std::cell::RefCell<std::collections::HashMap<PathBuf, DaemonProcess>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Keeps one managed daemon alive for the current test thread and profile.
///
/// Nextest runs each test in its own process, while the standard test harness
/// runs each test on a dedicated thread. Thread-local ownership therefore
/// keeps command factories concise without leaking daemon children across
/// otherwise unrelated tests.
pub fn ensure_tracedecay_daemon(home: &Path) {
    let home = canonical_existing_path(home);
    TEST_DAEMONS.with(|daemons| {
        let mut daemons = daemons.borrow_mut();
        daemons.retain(|existing_home, daemon| existing_home == &home && daemon.is_running());
        daemons
            .entry(home.clone())
            .or_insert_with(|| spawn_tracedecay_daemon(&home));
    });
}

/// Resolves the `git` executable to an absolute path exactly once per process.
///
/// This delegates to the same cached authority the product spawns through, so
/// tests and production resolve one program. Under heavy parallel test load
/// (nextest spawns one process per test, each spawning several `git`
/// subprocesses) a bare `Command::new("git")` PATH lookup can transiently fail
/// the spawn with `ENOENT` even though git is installed; resolving to an
/// absolute path up front removes the per-spawn PATH walk.
pub fn git_program() -> std::ffi::OsString {
    tracedecay::git::git_program().to_os_string()
}

#[cfg(unix)]
pub fn daemon_socket_path(home: &Path) -> PathBuf {
    canonical_existing_path(home).join(".tracedecay/daemon.sock")
}

pub fn daemon_authority_path(profile_root: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        profile_root
            .join("daemon-authority")
            .join("daemon-authority.json")
    }
    #[cfg(not(windows))]
    {
        profile_root.join("daemon-authority.json")
    }
}

pub fn spawn_tracedecay_daemon(home: &Path) -> DaemonProcess {
    spawn_tracedecay_daemon_with(home, |_| {})
}

/// Spawns a test daemon after applying caller-supplied command customization.
///
/// The callback runs after the standard test environment, arguments, working
/// directory, and stdio have been installed, so fault tests can override or
/// extend them without duplicating daemon startup and readiness handling.
pub fn spawn_tracedecay_daemon_with(
    home: &Path,
    configure: impl FnOnce(&mut Command),
) -> DaemonProcess {
    let profile_root = canonical_existing_path(home).join(".tracedecay");
    std::fs::create_dir_all(&profile_root).expect("daemon profile should be created");
    #[cfg(unix)]
    let socket_path = daemon_socket_path(home);
    let authority_path = daemon_authority_path(&profile_root);
    #[cfg(not(unix))]
    let portable_daemon_connectable = || {
        std::fs::read(&authority_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|record| {
                (record["endpoint"]["kind"] == "loopback")
                    .then(|| record["endpoint"]["address"].as_str().map(str::to_owned))
                    .flatten()
            })
            .is_some_and(|address| TcpStream::connect(address).is_ok())
    };
    #[cfg(unix)]
    assert!(
        std::os::unix::net::UnixStream::connect(&socket_path).is_err(),
        "refusing to replace a live test daemon at {}",
        socket_path.display()
    );
    #[cfg(not(unix))]
    assert!(
        !portable_daemon_connectable(),
        "refusing to replace a live test daemon recorded at {}",
        authority_path.display()
    );

    let mut command = Command::new(env!("CARGO_BIN_EXE_tracedecay"));
    apply_tracedecay_home_env(&mut command, home);
    command
        .args(["daemon", "run"])
        .env("TRACEDECAY_TEST_ALLOW_INCOMPLETE_HOLDER_SCAN", "1")
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    configure(&mut command);
    let child = command.spawn().expect("tracedecay daemon should start");
    let mut daemon = DaemonProcess { child };

    let deadline = Instant::now() + Duration::from_secs(10);
    poll_until(
        deadline,
        Duration::from_millis(25),
        || {
            #[cfg(unix)]
            let ready = std::os::unix::net::UnixStream::connect(&socket_path).is_ok();
            #[cfg(not(unix))]
            let ready = portable_daemon_connectable();
            if ready {
                return Some(());
            }
            if let Some(status) = daemon
                .child
                .try_wait()
                .expect("daemon status should be readable")
            {
                let mut stderr = String::new();
                if let Some(mut child_stderr) = daemon.child.stderr.take() {
                    let _ = child_stderr.read_to_string(&mut stderr);
                }
                panic!(
                    "tracedecay daemon exited before accepting connections: {status}; stderr: {}",
                    stderr.trim()
                );
            }
            None
        },
        || {
            format!(
                "timed out waiting for daemon authority at {}",
                authority_path.display()
            )
        },
    );
    daemon.drain_stderr();
    daemon
}

pub fn response_to_json(mut response: ureq::http::Response<ureq::Body>) -> (u16, Value) {
    let status = response.status().as_u16();
    let body = match response.body_mut().read_to_string() {
        Ok(body) => body,
        Err(err) => panic!("failed to read response body: {err}"),
    };
    let parsed = match serde_json::from_str::<Value>(&body) {
        Ok(value) => value,
        Err(err) => panic!("failed to decode JSON body `{body}`: {err}"),
    };
    (status, parsed)
}

/// True when a `ureq` error is a connection-level failure that a freshly
/// started (or briefly overloaded) server can transiently raise before it is
/// steadily accepting requests: peer disconnected, connection refused/reset,
/// or a bare I/O error. These are safe to retry for idempotent test requests;
/// an HTTP status error is NOT one of these (the agent is built with
/// `http_status_as_error(false)`, so 4xx/5xx come back as `Ok`).
pub fn is_transient_connection_error(err: &ureq::Error) -> bool {
    match err {
        ureq::Error::ConnectionFailed => true,
        ureq::Error::Io(_) => true,
        other => {
            // Fall back to a message match so newer/renamed variants (e.g.
            // "Peer disconnected", "connection reset") still count as transient
            // without pinning to a specific ureq version's enum shape.
            let text = other.to_string().to_ascii_lowercase();
            text.contains("peer disconnected")
                || text.contains("connection refused")
                || text.contains("connection reset")
                || text.contains("broken pipe")
                || text.contains("timed out")
        }
    }
}

/// Issues an idempotent HTTP request, retrying transient connection-level
/// errors (the server racing its own readiness under parallel load) with a
/// short bounded backoff. `send` performs one attempt; a `ureq::Error` that
/// passes [`is_transient_connection_error`] is retried, any other error (or
/// exhausted retries) panics with `label`.
pub fn http_call_with_retry(
    label: &str,
    send: impl Fn() -> Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> ureq::http::Response<ureq::Body> {
    let mut last_err: Option<ureq::Error> = None;
    for attempt in 0..12 {
        match send() {
            Ok(response) => return response,
            Err(err) if is_transient_connection_error(&err) => {
                last_err = Some(err);
                std::thread::sleep(Duration::from_millis(25 * (attempt + 1)));
            }
            Err(err) => panic!("{label} failed: {err}"),
        }
    }
    panic!("{label} failed after retries: {last_err:?}");
}

pub fn get_json(agent: &ureq::Agent, url: &str) -> (u16, Value) {
    let response = http_call_with_retry(&format!("GET {url}"), || agent.get(url).call());
    response_to_json(response)
}

/// Polls `condition` until it returns `Some(value)` or `deadline` passes,
/// sleeping `interval` between unsuccessful attempts. Panics with the
/// message produced by `describe` if the deadline elapses first.
///
/// This is the canonical shape for the "compute state, check it, sleep and
/// retry, assert on timeout" idiom used throughout the integration suites.
/// Callers that need a fixed cadence between samples (rather than an
/// immediate first check) can perform the delay inside `condition` itself
/// and pass `Duration::ZERO` as `interval`.
pub fn poll_until<T>(
    deadline: Instant,
    interval: Duration,
    mut condition: impl FnMut() -> Option<T>,
    describe: impl Fn() -> String,
) -> T {
    loop {
        if let Some(value) = condition() {
            return value;
        }
        assert!(Instant::now() < deadline, "{}", describe());
        std::thread::sleep(interval);
    }
}

pub async fn wait_for_dashboard(agent: &ureq::Agent, base_url: &str) {
    let probe = format!("{base_url}/api/capabilities");
    // Poll until the server both accepts the connection AND returns a real
    // HTTP response (2xx). A bare connect success is not enough — the server
    // can accept then drop the socket during startup ("Peer disconnected").
    for _ in 0..160 {
        let probe_agent = agent.clone();
        let probe_url = probe.clone();
        let ready = tokio::task::spawn_blocking(move || {
            probe_agent
                .get(&probe_url)
                .call()
                .is_ok_and(|response| response.status().is_success())
        })
        .await
        .unwrap_or(false);
        if ready {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("dashboard server did not become ready at {base_url}");
}

pub fn isolated_lcm_db_path(tmp: &TempDir) -> std::path::PathBuf {
    tracedecay::sessions::user_sessions_db_path(&tmp.path().join(".tracedecay"))
}

pub fn isolated_global_db_path(tmp: &TempDir) -> std::path::PathBuf {
    tmp.path().join(".tracedecay").join("global.db")
}

/// Opaque retained profile-session runtime for integration fixtures.
///
/// Callers get typed operations only; the registered database handle and
/// physical path remain owned by the daemon-style session registry.
pub struct LcmTestRuntime {
    runtime: HostAdmissionTestRuntimeV1,
}

impl LcmTestRuntime {
    async fn open(profile_root: &Path) -> Self {
        Self {
            runtime: HostAdmissionTestRuntimeV1::profile(profile_root)
                .await
                .expect("registered LCM test runtime"),
        }
    }

    pub fn close(self) {}

    pub async fn upsert_session(&self, session: &SessionRecord) -> bool {
        self.runtime
            .upsert_session_for_test(HostAdmissionScope::Profile, session)
            .await
            .unwrap_or(false)
    }

    pub async fn upsert_session_message(&self, message: &SessionMessageRecord) -> bool {
        self.runtime
            .upsert_session_message_for_test(HostAdmissionScope::Profile, message)
            .await
            .unwrap_or(false)
    }

    pub async fn lcm_load_raw_message(
        &self,
        provider: &str,
        message_id: &str,
    ) -> Option<tracedecay::sessions::lcm::LcmRawMessage> {
        self.runtime
            .lcm_load_raw_message_for_test(provider, message_id)
            .await
    }

    pub async fn lcm_insert_summary_node(
        &self,
        draft: tracedecay::sessions::lcm::LcmSummaryNodeDraft,
    ) -> Result<tracedecay::sessions::lcm::LcmSummaryNode, tracedecay::sessions::lcm::LcmError>
    {
        self.runtime
            .lcm_insert_summary_node_for_test(HostAdmissionScope::Profile, draft)
            .await
    }

    pub async fn lcm_update_lifecycle(
        &self,
        update: tracedecay::sessions::lcm::LcmLifecycleUpdate,
    ) -> Result<tracedecay::sessions::lcm::LcmLifecycleState, tracedecay::sessions::lcm::LcmError>
    {
        self.runtime
            .lcm_update_lifecycle_for_test(HostAdmissionScope::Profile, update)
            .await
    }

    pub async fn lcm_lifecycle_state(
        &self,
        provider: &str,
        conversation_id: &str,
    ) -> Result<tracedecay::sessions::lcm::LcmLifecycleState, tracedecay::sessions::lcm::LcmError>
    {
        self.runtime
            .lcm_lifecycle_state_for_test(provider, conversation_id)
            .await
    }

    pub async fn lcm_preflight(
        &self,
        request: tracedecay::sessions::lcm::LcmPreflightRequest,
    ) -> Result<tracedecay::sessions::lcm::LcmPreflightResponse, tracedecay::sessions::lcm::LcmError>
    {
        self.runtime.lcm_preflight_for_test(request).await
    }

    pub async fn lcm_compress(
        &self,
        request: tracedecay::sessions::lcm::LcmCompressionRequest,
    ) -> Result<
        tracedecay::sessions::lcm::LcmCompressionResponse,
        tracedecay::sessions::lcm::LcmError,
    > {
        self.runtime.lcm_compress_for_test(request).await
    }

    pub async fn lcm_compress_for_test(
        &self,
        request: tracedecay::sessions::lcm::LcmCompressionRequest,
    ) -> Result<
        tracedecay::sessions::lcm::LcmCompressionResponse,
        tracedecay::sessions::lcm::LcmError,
    > {
        self.runtime.lcm_compress_for_test(request).await
    }

    pub async fn lcm_status(
        &self,
        provider: &str,
        session_id: Option<&str>,
    ) -> Result<tracedecay::sessions::lcm::LcmStatus, tracedecay::sessions::lcm::LcmError> {
        self.runtime.lcm_status_for_test(provider, session_id).await
    }

    pub async fn lcm_load_session(
        &self,
        request: tracedecay::sessions::lcm::LcmLoadSessionRequest,
    ) -> Result<tracedecay::sessions::lcm::LcmLoadSessionPage, tracedecay::sessions::lcm::LcmError>
    {
        self.runtime.lcm_load_session_for_test(request).await
    }

    pub async fn lcm_grep(
        &self,
        request: tracedecay::sessions::lcm::LcmGrepRequest,
    ) -> Result<tracedecay::sessions::lcm::LcmGrepOutcome, tracedecay::sessions::lcm::LcmError>
    {
        self.runtime.lcm_grep_for_test(request).await
    }

    pub async fn lcm_expand(
        &self,
        request: tracedecay::sessions::lcm::LcmExpandRequest,
    ) -> Result<tracedecay::sessions::lcm::LcmExpandResponse, tracedecay::sessions::lcm::LcmError>
    {
        self.runtime.lcm_expand_for_test(request).await
    }

    pub async fn lcm_expand_summary_node(
        &self,
        provider: &str,
        session_id: &str,
        node_id: &str,
    ) -> Result<tracedecay::sessions::lcm::LcmSummaryExpansion, tracedecay::sessions::lcm::LcmError>
    {
        self.runtime
            .lcm_expand_summary_node_for_test(provider, session_id, node_id)
            .await
    }

    pub async fn lcm_session_boundary(
        &self,
        request: tracedecay::sessions::lcm::LcmSessionBoundaryRequest,
    ) -> Result<
        tracedecay::sessions::lcm::LcmSessionBoundaryResponse,
        tracedecay::sessions::lcm::LcmError,
    > {
        self.runtime.lcm_session_boundary_for_test(request).await
    }

    pub async fn replace_lcm_summary_source_for_test(
        &self,
        scope: HostAdmissionScope,
        node_id: &str,
        source_node_id: &str,
    ) -> tracedecay::errors::Result<()> {
        self.runtime
            .replace_lcm_summary_source_for_test(scope, node_id, source_node_id)
            .await
    }

    pub fn lcm_store(&self, _storage_root: impl AsRef<Path>) -> LcmTestStore<'_> {
        LcmTestStore {
            runtime: &self.runtime,
        }
    }

    pub fn observation_store(
        &self,
    ) -> Result<tracedecay::store::GlobalDbObservationStore<'_>, HostAdmissionOutcome> {
        self.runtime.observation_store(HostAdmissionScope::Profile)
    }

    pub fn session_temporal_store(
        &self,
    ) -> Result<tracedecay::store::GlobalDbSessionTemporalStore<'_>, HostAdmissionOutcome> {
        self.runtime
            .session_temporal_store(HostAdmissionScope::Profile)
    }
}

pub struct LcmTestStore<'runtime> {
    runtime: &'runtime HostAdmissionTestRuntimeV1,
}

impl LcmTestStore<'_> {
    pub async fn ingest_raw_message(
        &self,
        message: &SessionMessageRecord,
    ) -> Result<(), tracedecay::sessions::lcm::LcmError> {
        self.runtime
            .lcm_ingest_raw_message_for_test(HostAdmissionScope::Profile, message)
            .await
    }

    pub async fn lcm_expand_payload(
        &self,
        provider: &str,
        session_id: &str,
        payload_ref: &str,
        offset: usize,
        limit: usize,
    ) -> Result<tracedecay::sessions::lcm::LcmExpandResponse, tracedecay::sessions::lcm::LcmError>
    {
        self.runtime
            .lcm_expand_for_test(tracedecay::sessions::lcm::LcmExpandRequest {
                provider: provider.to_string(),
                session_id: session_id.to_string(),
                target: tracedecay::sessions::lcm::LcmExpandTarget::ExternalPayload {
                    payload_ref: payload_ref.to_string(),
                },
                content_slice: Some(tracedecay::sessions::lcm::LcmContentSlice { offset, limit }),
                source_offset: 0,
                source_limit: None,
            })
            .await
    }
}

pub async fn open_lcm_db(tmp: &TempDir) -> LcmTestRuntime {
    let profile_root = tmp.path().join(".tracedecay");
    let db_path = tracedecay::sessions::user_sessions_db_path(&profile_root);
    if !db_path.exists() {
        seed_lcm_db_from_template(&db_path).await;
    }
    LcmTestRuntime::open(&profile_root).await
}

/// Writes an empty registered-global-schema store at `db_path` from the cached
/// per-process template, so later opens (fixture seeding, dashboard server
/// startup) find an existing DB and skip the full schema creation — a large
/// fixed cost on Windows. The first call in a process pays one real schema
/// creation to build the template; every further store is a file copy.
pub async fn write_empty_global_db_schema(db_path: &Path) {
    let bytes = if db_path.file_name() == Some(OsStr::new("global.db")) {
        empty_global_db_template().await
    } else {
        empty_lcm_db_template().await
    };
    seed_database_from_template(db_path, bytes, "global").await;
}

async fn seed_lcm_db_from_template(db_path: &Path) {
    seed_database_from_template(db_path, empty_lcm_db_template().await, "LCM").await;
}

async fn seed_database_from_template(db_path: &Path, bytes: &[u8], label: &str) {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|err| {
            panic!(
                "failed to create {label} test DB directory '{}': {err}",
                parent.display()
            )
        });
    }
    fs::write(db_path, bytes).unwrap_or_else(|err| {
        panic!(
            "failed to write {label} test DB template '{}': {err}",
            db_path.display()
        )
    });
}

/// Opens a fresh graph-schema [`Database`] at `db_path` from a cached
/// per-process template, skipping the full `create_schema` DDL run — a large
/// fixed cost on Windows when a suite creates one store per test. The first
/// call in a process pays one real `Database::initialize` to build the
/// template; every further store is a file copy plus `Database::open`.
pub async fn open_graph_db_from_template(db_path: &Path) -> Database {
    let bytes = EMPTY_GRAPH_DB_TEMPLATE
        .get_or_init(|| async {
            let tmp = tempdir_or_panic();
            let template_path = tmp.path().join("template-graph.db");
            let (db, _) = initialize_test_database(&template_path)
                .await
                .expect("template graph db initialize");
            db.checkpoint().await.expect("template graph db checkpoint");
            db.close();
            fs::read(&template_path).unwrap_or_else(|err| {
                panic!(
                    "failed to read graph test DB template '{}': {err}",
                    template_path.display()
                )
            })
        })
        .await;
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|err| {
            panic!(
                "failed to create graph test DB directory '{}': {err}",
                parent.display()
            )
        });
    }
    fs::write(db_path, bytes).unwrap_or_else(|err| {
        panic!(
            "failed to write graph test DB template '{}': {err}",
            db_path.display()
        )
    });
    let (db, _) = open_test_database(db_path)
        .await
        .unwrap_or_else(|err| panic!("failed to open templated graph db: {err}"));
    db
}

async fn empty_lcm_db_template() -> &'static [u8] {
    EMPTY_LCM_DB_TEMPLATE
        .get_or_init(|| async {
            let tmp = tempdir_or_panic();
            let profile_root = tmp.path().join(".tracedecay");
            let snapshot_path = tmp.path().join("template-session.db");
            let runtime = HostAdmissionTestRuntimeV1::profile(&profile_root)
                .await
                .expect("template session runtime open");
            runtime
                .snapshot_session_database_for_test(HostAdmissionScope::Profile, &snapshot_path)
                .await
                .expect("template session database snapshot");
            drop(runtime);
            fs::read(&snapshot_path).unwrap_or_else(|err| {
                panic!(
                    "failed to read LCM test DB template '{}': {err}",
                    snapshot_path.display()
                )
            })
        })
        .await
}

async fn empty_global_db_template() -> &'static [u8] {
    EMPTY_GLOBAL_DB_TEMPLATE
        .get_or_init(|| async {
            let tmp = tempdir_or_panic();
            let profile_root = tmp.path().join(".tracedecay");
            let snapshot_path = tmp.path().join("template-global.db");
            let runtime = HostAdmissionTestRuntimeV1::profile(&profile_root)
                .await
                .expect("template global runtime open");
            runtime
                .snapshot_profile_database_for_test(&snapshot_path)
                .await
                .expect("template global database snapshot");
            drop(runtime);
            fs::read(&snapshot_path).unwrap_or_else(|err| {
                panic!(
                    "failed to read global test DB template '{}': {err}",
                    snapshot_path.display()
                )
            })
        })
        .await
}

pub fn session_record(
    provider: &str,
    session_id: &str,
    project_key: &str,
    title: &str,
    transcript_path: Option<&str>,
    metadata_json: Option<&str>,
) -> SessionRecord {
    SessionRecord {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        project_key: project_key.to_string(),
        project_path: "/tmp/project".to_string(),
        title: Some(title.to_string()),
        started_at: Some(1_715_000_000),
        ended_at: None,
        transcript_path: transcript_path.map(str::to_string),
        metadata_json: metadata_json.map(str::to_string),
        parent_session_id: None,
        is_subagent: false,
        agent_id: None,
        parent_tool_use_id: None,
    }
}

pub fn lcm_payload_session(provider: &str, session_id: &str) -> SessionRecord {
    session_record(
        provider,
        session_id,
        "/tmp/project",
        "LCM payload test",
        None,
        None,
    )
}

pub fn lcm_dag_session(provider: &str, session_id: &str) -> SessionRecord {
    session_record(
        provider,
        session_id,
        "/tmp/project",
        "LCM DAG test",
        None,
        None,
    )
}

pub fn lcm_raw_session(provider: &str, session_id: &str, project_key: &str) -> SessionRecord {
    session_record(
        provider,
        session_id,
        project_key,
        "LCM raw test",
        Some("/tmp/project/transcript.jsonl"),
        None,
    )
}

pub fn global_session(provider: &str, session_id: &str, project_key: &str) -> SessionRecord {
    session_record(
        provider,
        session_id,
        project_key,
        "Initial title",
        Some("/tmp/project/transcript.jsonl"),
        Some(r#"{"source":"test"}"#),
    )
}

/// Builder for the shared `SessionMessageRecord` fixture.
pub struct MessageRecordBuilder<'a> {
    provider: &'a str,
    message_id: &'a str,
    session_id: &'a str,
    role: &'a str,
    ordinal: i64,
    text: &'a str,
    kind: &'a str,
    timestamp: Option<i64>,
    model: Option<&'a str>,
    tool_names: Option<&'a str>,
    source_path: Option<&'a str>,
    source_offset: Option<i64>,
    metadata_json: Option<&'a str>,
}

impl<'a> MessageRecordBuilder<'a> {
    pub fn new(
        provider: &'a str,
        message_id: &'a str,
        session_id: &'a str,
        role: &'a str,
        ordinal: i64,
        text: &'a str,
        kind: &'a str,
    ) -> Self {
        Self {
            provider,
            message_id,
            session_id,
            role,
            ordinal,
            text,
            kind,
            timestamp: Some(1_715_000_030),
            model: Some("test-model"),
            tool_names: None,
            source_path: None,
            source_offset: None,
            metadata_json: None,
        }
    }

    pub fn with_timestamp(mut self, timestamp: Option<i64>) -> Self {
        self.timestamp = timestamp;
        self
    }

    pub fn with_model(mut self, model: Option<&'a str>) -> Self {
        self.model = model;
        self
    }

    pub fn with_tool_names(mut self, tool_names: Option<&'a str>) -> Self {
        self.tool_names = tool_names;
        self
    }

    pub fn with_source(mut self, source_path: Option<&'a str>, source_offset: Option<i64>) -> Self {
        self.source_path = source_path;
        self.source_offset = source_offset;
        self
    }

    pub fn with_metadata(mut self, metadata_json: Option<&'a str>) -> Self {
        self.metadata_json = metadata_json;
        self
    }

    pub fn build(self) -> SessionMessageRecord {
        SessionMessageRecord {
            provider: self.provider.to_string(),
            message_id: self.message_id.to_string(),
            session_id: self.session_id.to_string(),
            role: self.role.to_string(),
            timestamp: self.timestamp,
            ordinal: self.ordinal,
            text: self.text.to_string(),
            kind: Some(self.kind.to_string()),
            model: self.model.map(str::to_string),
            tool_names: self.tool_names.map(str::to_string),
            source_path: self.source_path.map(str::to_string),
            source_offset: self.source_offset,
            metadata_json: self.metadata_json.map(str::to_string),
        }
    }
}

pub fn lcm_payload_message(
    provider: &str,
    message_id: &str,
    session_id: &str,
    role: &str,
    text: &str,
) -> SessionMessageRecord {
    MessageRecordBuilder::new(
        provider,
        message_id,
        session_id,
        role,
        1,
        text,
        "tool_result",
    )
    .build()
}

pub fn lcm_dag_message(
    provider: &str,
    message_id: &str,
    session_id: &str,
    ordinal: i64,
    text: &str,
) -> SessionMessageRecord {
    MessageRecordBuilder::new(
        provider,
        message_id,
        session_id,
        "assistant",
        ordinal,
        text,
        "message",
    )
    .with_timestamp(Some(1_715_000_000 + ordinal))
    .build()
}

pub fn lcm_raw_message(
    provider: &str,
    message_id: &str,
    session_id: &str,
    text: &str,
) -> SessionMessageRecord {
    MessageRecordBuilder::new(
        provider,
        message_id,
        session_id,
        "assistant",
        1,
        text,
        "message",
    )
    .with_source(Some("/tmp/project/transcript.jsonl"), Some(42))
    .build()
}

pub fn global_message(
    provider: &str,
    message_id: &str,
    session_id: &str,
    text: &str,
) -> SessionMessageRecord {
    MessageRecordBuilder::new(
        provider,
        message_id,
        session_id,
        "assistant",
        1,
        text,
        "message",
    )
    .with_tool_names(Some("tracedecay_context,tracedecay_search"))
    .with_source(Some("/tmp/project/transcript.jsonl"), Some(42))
    .with_metadata(Some(r#"{"finish_reason":"stop"}"#))
    .build()
}

/// Minimal PyYAML stand-in covering only the YAML subset the generated
/// Hermes configs use: nested block mappings, block lists of scalars, and
/// plain/quoted scalars. Hermes itself always ships PyYAML; CI's system
/// python3 on macOS/Windows has no third-party packages, so checks that
/// exercise the plugin's config.yaml paths get this shim via PYTHONPATH.
pub const PYYAML_SHIM: &str = r##""""Minimal PyYAML stand-in for tracedecay agent tests.

Implements safe_load/dump for the simple block-style YAML the generated
Hermes config files use. Only used when the system python3 lacks PyYAML.
"""

import json
import re

_PLAIN_SCALAR = re.compile(r"^[A-Za-z0-9_./~+-]+$")


def safe_load(stream):
    text = stream if isinstance(stream, str) else stream.read()
    items = []
    for raw in text.splitlines():
        stripped = raw.strip()
        if not stripped or stripped.startswith("#"):
            continue
        items.append((len(raw) - len(raw.lstrip(" ")), stripped))
    if not items:
        return None
    value, index = _parse_block(items, 0, items[0][0])
    if index != len(items):
        raise ValueError(f"unsupported yaml structure near: {items[index][1]!r}")
    return value


def _parse_scalar(token):
    if token in ("", "null", "~"):
        return None
    if token == "true":
        return True
    if token == "false":
        return False
    if len(token) >= 2 and token[0] == token[-1] and token[0] in "'\"":
        return token[1:-1]
    for parse in (int, float):
        try:
            return parse(token)
        except ValueError:
            pass
    return token


def _parse_block(items, index, indent):
    if items[index][1].startswith("- "):
        result = []
        while index < len(items) and items[index][0] == indent and items[index][1].startswith("- "):
            result.append(_parse_scalar(items[index][1][2:].strip()))
            index += 1
        return result, index
    mapping = {}
    while index < len(items) and items[index][0] == indent and not items[index][1].startswith("- "):
        line = items[index][1]
        if ":" not in line:
            raise ValueError(f"unsupported yaml line: {line!r}")
        key, _, rest = line.partition(":")
        index += 1
        rest = rest.strip()
        if rest:
            mapping[_parse_scalar(key.strip())] = _parse_scalar(rest)
            continue
        child = None
        if index < len(items) and items[index][0] > indent:
            child, index = _parse_block(items, index, items[index][0])
        elif index < len(items) and items[index][0] == indent and items[index][1].startswith("- "):
            child, index = _parse_block(items, index, indent)
        mapping[_parse_scalar(key.strip())] = child
    return mapping, index


def _dump_scalar(value):
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, (int, float)):
        return str(value)
    text = str(value)
    return text if _PLAIN_SCALAR.match(text) else json.dumps(text)


def _dump_lines(value, indent, lines):
    pad = " " * indent
    if isinstance(value, dict):
        for key, child in value.items():
            if isinstance(child, (dict, list)) and child:
                lines.append(f"{pad}{_dump_scalar(key)}:")
                _dump_lines(child, indent + 2, lines)
            else:
                child_repr = "{}" if child == {} else "[]" if child == [] else _dump_scalar(child)
                lines.append(f"{pad}{_dump_scalar(key)}: {child_repr}")
    elif isinstance(value, list):
        for item in value:
            lines.append(f"{pad}- {_dump_scalar(item)}")
    else:
        lines.append(f"{pad}{_dump_scalar(value)}")


def dump(data, stream=None, default_flow_style=False, **kwargs):
    lines = []
    _dump_lines(data, 0, lines)
    text = "\n".join(lines) + "\n"
    if stream is None:
        return text
    stream.write(text)
    return None
"##;

/// Python prelude that falls back to the bundled PyYAML shim (argv[2]) only
/// when the interpreter has no importable `yaml`, so config.yaml-dependent
/// checks run on bare CI runners without a separate `python3 -c "import
/// yaml"` probe process. Appending to sys.path keeps the precedence
/// identical: a real PyYAML always wins.
pub const PYYAML_FALLBACK_PRELUDE: &str = r#"
import importlib.util as _yaml_probe_util
import sys as _yaml_probe_sys

if _yaml_probe_util.find_spec("yaml") is None:
    _yaml_probe_sys.path.append(_yaml_probe_sys.argv[2])
"#;

/// Writes the PyYAML test shim next to the test home and returns its
/// directory, for scripts using [`PYYAML_FALLBACK_PRELUDE`].
pub fn write_pyyaml_shim(scratch: &Path) -> PathBuf {
    let shim_dir = scratch.join("pyyaml-shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    std::fs::write(shim_dir.join("yaml.py"), PYYAML_SHIM).unwrap();
    shim_dir
}

/// Serializes tests that mutate process-wide environment variables (HOME,
/// USER_DATA_DIR_ENV, HERMES_HOME, ...) across every module of a consolidated
/// test binary. Only matters for in-process runners like `cargo test`;
/// nextest runs one process per test. A tokio mutex so async tests can hold
/// the guard across `.await` (sync tests use `blocking_lock`), and unlike a
/// std mutex it cannot poison when a failing test panics while holding it.
pub static PROCESS_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
