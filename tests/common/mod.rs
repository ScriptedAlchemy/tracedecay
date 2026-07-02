#![allow(dead_code)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(unix)]
use std::process::{Child, Stdio};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::OnceCell;
use tracedecay::config::USER_DATA_DIR_ENV;
use tracedecay::global_db::GlobalDb;
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};
use tracedecay::types::{Node, NodeKind, Visibility};

static EMPTY_LCM_DB_TEMPLATE: OnceCell<Vec<u8>> = OnceCell::const_new();

/// Sets (or removes) an environment variable for its lifetime, restoring the
/// previous value on drop.
pub struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    pub fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }

    /// Removes `key` for the guard's lifetime, so tests can exercise the
    /// no-override path.
    pub fn unset(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
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
    _dir: TempDir,
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
                _dir: dir,
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
    fs::copy(script, bin).unwrap_or_else(|err| {
        panic!(
            "failed to install fake codex launcher {} from {}: {err}",
            bin.display(),
            script.display()
        )
    });
    make_executable(bin);
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .unwrap_or_else(|err| panic!("failed to stat {}: {err}", path.display()))
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
        .unwrap_or_else(|err| panic!("failed to chmod {}: {err}", path.display()));
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

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
        .worker_threads(2)
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

#[cfg(unix)]
pub struct DaemonProcess {
    child: Child,
}

#[cfg(unix)]
impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
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

#[cfg(unix)]
pub fn daemon_socket_path(home: &Path) -> PathBuf {
    canonical_existing_path(home).join(".tracedecay/daemon.sock")
}

#[cfg(unix)]
pub fn spawn_tracedecay_daemon(home: &Path) -> DaemonProcess {
    let socket_path = daemon_socket_path(home);
    let _ = std::fs::remove_file(&socket_path);

    let mut child = tracedecay_command_with_home(home)
        .arg("daemon")
        .arg("run")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("tracedecay daemon should start");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if std::os::unix::net::UnixStream::connect(&socket_path).is_ok() {
            return DaemonProcess { child };
        }
        if let Some(status) = child.try_wait().expect("daemon status should be readable") {
            panic!("tracedecay daemon exited before opening socket: {status}");
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for daemon socket at {}",
            socket_path.display()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
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

pub fn get_json(agent: &ureq::Agent, url: &str) -> (u16, Value) {
    let response = match agent.get(url).call() {
        Ok(response) => response,
        Err(err) => panic!("GET {url} failed: {err}"),
    };
    response_to_json(response)
}

pub async fn wait_for_dashboard(agent: &ureq::Agent, base_url: &str) {
    let probe = format!("{base_url}/api/capabilities");
    for _ in 0..80 {
        if agent.get(&probe).call().is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("dashboard server did not become ready at {base_url}");
}

pub fn isolated_lcm_db_path(tmp: &TempDir) -> std::path::PathBuf {
    tmp.path().join(".tracedecay").join("sessions.db")
}

pub fn isolated_global_db_path(tmp: &TempDir) -> std::path::PathBuf {
    tmp.path().join(".tracedecay").join("global.db")
}

pub async fn open_lcm_db(tmp: &TempDir) -> GlobalDb {
    let db_path = isolated_lcm_db_path(tmp);
    if !db_path.exists() {
        seed_lcm_db_from_template(&db_path).await;
        return GlobalDb::open_at_assuming_schema(&db_path)
            .await
            .expect("session db open");
    }
    GlobalDb::open_at(&db_path).await.expect("session db open")
}

/// Writes an empty `GlobalDb`-schema store at `db_path` from the cached
/// per-process template, so later opens (fixture seeding, dashboard server
/// startup) find an existing DB and skip the full schema creation — a large
/// fixed cost on Windows. The first call in a process pays one real schema
/// creation to build the template; every further store is a file copy.
pub async fn write_empty_global_db_schema(db_path: &Path) {
    seed_lcm_db_from_template(db_path).await;
}

async fn seed_lcm_db_from_template(db_path: &Path) {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|err| {
            panic!(
                "failed to create LCM test DB directory '{}': {err}",
                parent.display()
            )
        });
    }
    fs::write(db_path, empty_lcm_db_template().await).unwrap_or_else(|err| {
        panic!(
            "failed to write LCM test DB template '{}': {err}",
            db_path.display()
        )
    });
}

async fn empty_lcm_db_template() -> &'static [u8] {
    EMPTY_LCM_DB_TEMPLATE
        .get_or_init(|| async {
            let tmp = tempdir_or_panic();
            let db_path = isolated_lcm_db_path(&tmp);
            let db = GlobalDb::open_at(&db_path)
                .await
                .expect("template session db open");
            db.checkpoint().await;
            db.close();
            fs::read(&db_path).unwrap_or_else(|err| {
                panic!(
                    "failed to read LCM test DB template '{}': {err}",
                    db_path.display()
                )
            })
        })
        .await
}

pub async fn open_global_db(tmp: &TempDir) -> GlobalDb {
    GlobalDb::open_at(&isolated_global_db_path(tmp))
        .await
        .expect("global db open")
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

/// The shared `SessionMessageRecord` builder every test fixture routes
/// through. `timestamp`/`model` are explicit because the dashboard fixtures
/// vary them; the convenience wrappers below fill the common defaults.
#[allow(clippy::too_many_arguments)]
pub fn message_record_at(
    provider: &str,
    message_id: &str,
    session_id: &str,
    role: &str,
    ordinal: i64,
    timestamp: Option<i64>,
    text: &str,
    kind: &str,
    model: Option<&str>,
    tool_names: Option<&str>,
    source_path: Option<&str>,
    source_offset: Option<i64>,
    metadata_json: Option<&str>,
) -> SessionMessageRecord {
    SessionMessageRecord {
        provider: provider.to_string(),
        message_id: message_id.to_string(),
        session_id: session_id.to_string(),
        role: role.to_string(),
        timestamp,
        ordinal,
        text: text.to_string(),
        kind: Some(kind.to_string()),
        model: model.map(str::to_string),
        tool_names: tool_names.map(str::to_string),
        source_path: source_path.map(str::to_string),
        source_offset,
        metadata_json: metadata_json.map(str::to_string),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn message_record(
    provider: &str,
    message_id: &str,
    session_id: &str,
    role: &str,
    ordinal: i64,
    text: &str,
    kind: &str,
    tool_names: Option<&str>,
    source_path: Option<&str>,
    source_offset: Option<i64>,
    metadata_json: Option<&str>,
) -> SessionMessageRecord {
    message_record_at(
        provider,
        message_id,
        session_id,
        role,
        ordinal,
        Some(1_715_000_030),
        text,
        kind,
        Some("test-model"),
        tool_names,
        source_path,
        source_offset,
        metadata_json,
    )
}

pub fn lcm_payload_message(
    provider: &str,
    message_id: &str,
    session_id: &str,
    role: &str,
    text: &str,
) -> SessionMessageRecord {
    message_record(
        provider,
        message_id,
        session_id,
        role,
        1,
        text,
        "tool_result",
        None,
        None,
        None,
        None,
    )
}

pub fn lcm_dag_message(
    provider: &str,
    message_id: &str,
    session_id: &str,
    ordinal: i64,
    text: &str,
) -> SessionMessageRecord {
    let mut message = message_record(
        provider,
        message_id,
        session_id,
        "assistant",
        ordinal,
        text,
        "message",
        None,
        None,
        None,
        None,
    );
    message.timestamp = Some(1_715_000_000 + ordinal);
    message
}

pub fn lcm_raw_message(
    provider: &str,
    message_id: &str,
    session_id: &str,
    text: &str,
) -> SessionMessageRecord {
    message_record(
        provider,
        message_id,
        session_id,
        "assistant",
        1,
        text,
        "message",
        None,
        Some("/tmp/project/transcript.jsonl"),
        Some(42),
        None,
    )
}

pub fn global_message(
    provider: &str,
    message_id: &str,
    session_id: &str,
    text: &str,
) -> SessionMessageRecord {
    message_record(
        provider,
        message_id,
        session_id,
        "assistant",
        1,
        text,
        "message",
        Some("tracedecay_context,tracedecay_search"),
        Some("/tmp/project/transcript.jsonl"),
        Some(42),
        Some(r#"{"finish_reason":"stop"}"#),
    )
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
