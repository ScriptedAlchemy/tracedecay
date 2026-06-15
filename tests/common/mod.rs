#![allow(dead_code)]

use std::ffi::{OsStr, OsString};
use std::net::TcpListener;
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;
use tracedecay::global_db::GlobalDb;
use tracedecay::sessions::{SessionMessageRecord, SessionRecord};

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
pub static GLOBAL_DB_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn tempdir_or_panic() -> TempDir {
    match TempDir::new() {
        Ok(dir) => dir,
        Err(err) => panic!("failed to create temp dir: {err}"),
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
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(4)))
        .build()
        .into()
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
    GlobalDb::open_at(&isolated_lcm_db_path(tmp))
        .await
        .expect("session db open")
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

pub fn python3_has_real_yaml() -> bool {
    static HAS_YAML: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HAS_YAML.get_or_init(|| {
        std::process::Command::new("python3")
            .args(["-c", "import yaml"])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    })
}

/// Returns a PYTHONPATH entry providing the `yaml` shim when the system
/// python3 has no real PyYAML, so config.yaml-dependent checks run on every
/// OS instead of failing on bare CI runners.
pub fn pyyaml_shim_pythonpath(scratch: &std::path::Path) -> Option<std::path::PathBuf> {
    if python3_has_real_yaml() {
        return None;
    }
    let shim_dir = scratch.join("pyyaml-shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    std::fs::write(shim_dir.join("yaml.py"), PYYAML_SHIM).unwrap();
    Some(shim_dir)
}
