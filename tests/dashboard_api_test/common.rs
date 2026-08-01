#![allow(dead_code)]

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
#[cfg(not(windows))]
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
#[cfg(not(windows))]
use tempfile::NamedTempFile;
use tempfile::TempDir;
use tracedecay::sessions::SessionMessageRecord;

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

pub const GLOBAL_DB_ENV: &str = "TRACEDECAY_GLOBAL_DB";
pub static GLOBAL_DB_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn tempdir_or_panic() -> TempDir {
    TempDir::new().unwrap_or_else(|error| panic!("failed to create temp dir: {error}"))
}

pub fn fake_codex_bin(temp: &Path) -> PathBuf {
    temp.join(if cfg!(windows) { "codex.cmd" } else { "codex" })
}

#[cfg(windows)]
pub fn install_fake_codex_launcher(_script: &Path, bin: &Path) {
    fs::write(bin, windows_python_launcher("codex.py")).unwrap_or_else(|error| {
        panic!(
            "failed to install fake codex launcher {}: {error}",
            bin.display()
        )
    });
}

#[cfg(not(windows))]
pub fn install_fake_codex_launcher(script: &Path, bin: &Path) {
    let script_name = script
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_else(|| panic!("invalid fake codex script path: {}", script.display()));
    let launcher = format!(
        "#!/bin/sh\n\
         SCRIPT_DIR=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\n\
         exec python3 \"$SCRIPT_DIR/{script_name}\" \"$@\"\n"
    );
    write_executable_atomically(bin, launcher.as_bytes()).unwrap_or_else(|error| {
        panic!(
            "failed to install fake codex launcher {} for {}: {error}",
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
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    make_executable_file(temporary.as_file())?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
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

fn windows_python_launcher(script_name: &str) -> String {
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

pub fn create_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap_or_else(|error| panic!("failed to create tokio runtime: {error}"))
}

pub fn pick_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .unwrap_or_else(|error| panic!("failed to reserve a local port: {error}"))
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

pub fn git_program() -> OsString {
    tracedecay::git::git_program().to_os_string()
}

pub fn response_to_json(mut response: ureq::http::Response<ureq::Body>) -> (u16, Value) {
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .unwrap_or_else(|error| panic!("failed to read response body: {error}"));
    let parsed = serde_json::from_str(&body)
        .unwrap_or_else(|error| panic!("failed to decode JSON body `{body}`: {error}"));
    (status, parsed)
}

fn is_transient_connection_error(error: &ureq::Error) -> bool {
    match error {
        ureq::Error::ConnectionFailed | ureq::Error::Io(_) => true,
        other => {
            let text = other.to_string().to_ascii_lowercase();
            text.contains("peer disconnected")
                || text.contains("connection refused")
                || text.contains("connection reset")
                || text.contains("broken pipe")
                || text.contains("timed out")
        }
    }
}

pub fn http_call_with_retry(
    label: &str,
    send: impl Fn() -> Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> ureq::http::Response<ureq::Body> {
    let mut last_error = None;
    for attempt in 0..12 {
        match send() {
            Ok(response) => return response,
            Err(error) if is_transient_connection_error(&error) => {
                last_error = Some(error);
                std::thread::sleep(Duration::from_millis(25 * (attempt + 1)));
            }
            Err(error) => panic!("{label} failed: {error}"),
        }
    }
    panic!("{label} failed after retries: {last_error:?}");
}

pub fn get_json(agent: &ureq::Agent, url: &str) -> (u16, Value) {
    response_to_json(http_call_with_retry(&format!("GET {url}"), || {
        agent.get(url).call()
    }))
}

pub async fn wait_for_dashboard(agent: &ureq::Agent, base_url: &str) {
    let probe = format!("{base_url}/api/capabilities");
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

    pub fn with_source(mut self, path: Option<&'a str>, offset: Option<i64>) -> Self {
        self.source_path = path;
        self.source_offset = offset;
        self
    }

    pub fn with_metadata(mut self, metadata_json: Option<&'a str>) -> Self {
        self.metadata_json = metadata_json;
        self
    }

    pub fn build(self) -> SessionMessageRecord {
        SessionMessageRecord {
            provider: self.provider.to_owned(),
            message_id: self.message_id.to_owned(),
            session_id: self.session_id.to_owned(),
            role: self.role.to_owned(),
            timestamp: self.timestamp,
            ordinal: self.ordinal,
            text: self.text.to_owned(),
            kind: Some(self.kind.to_owned()),
            model: self.model.map(str::to_owned),
            tool_names: self.tool_names.map(str::to_owned),
            source_path: self.source_path.map(str::to_owned),
            source_offset: self.source_offset,
            metadata_json: self.metadata_json.map(str::to_owned),
        }
    }
}
