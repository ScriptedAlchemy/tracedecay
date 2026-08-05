use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::{Duration, Instant, SystemTime};

use tempfile::TempDir;
use tracedecay::agents::{AgentIntegration, HermesIntegration, InstallContext};
use tracedecay::external_tools::ast_grep_command;
use tracedecay::sessions::lcm::{LcmCompressionRequest, LcmSummarizerMode};

use crate::common::{PYYAML_FALLBACK_PRELUDE, host_sources, write_pyyaml_shim};

// Compiles the generated plugin sources with py_compile (argv[1] is the
// plugin dir). Only `generated_python_sources_compile` runs this: loading the
// plugin via `PLUGIN_LOAD_PRELUDE` already compiles every module, so a
// per-test compile pass would just re-parse ~150KB of generated Python in
// each of the ~50 checks — measurable on Windows CI where these tests are a
// runtime hotspot.
const PYTHON_COMPILE_CHECK: &str = r#"
import pathlib as _compile_pathlib
import py_compile as _py_compile
import sys as _compile_sys

for _name in ("tools.py", "schemas.py", "__init__.py"):
    try:
        _py_compile.compile(
            str(_compile_pathlib.Path(_compile_sys.argv[1]) / _name), doraise=True
        )
    except _py_compile.PyCompileError as _exc:
        print(f"generated Python should compile: {_name}: {_exc}", file=_compile_sys.stderr)
        _compile_sys.exit(1)
"#;

const PLUGIN_LOAD_PRELUDE: &str = r#"
import importlib.machinery
import importlib.util
import os
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
# The runner pins HOME to the temp install. A conflicting Hermes override must
# not redirect generated plugin configuration or TraceDecay storage.
os.environ["HERMES_HOME"] = "/ignored/hermes-home"
parent_name = "_hermes_user_shared_prelude"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)
"#;

/// Binary path baked into the generated `tools.py` for the shared install.
/// Part of the bundle fingerprint: changing it changes the rendered plugin.
const FIXTURE_TRACEDECAY_BIN: &str = "/usr/local/bin/tracedecay";

fn make_install_ctx(home: &Path) -> InstallContext {
    InstallContext {
        home: home.to_path_buf(),
        tracedecay_bin: FIXTURE_TRACEDECAY_BIN.to_string(),
        tool_permissions: Vec::new(),
        project_root: None,
        // The LCM bridge checks never touch the dashboard deploy, and
        // skipping it avoids writing ~430KB of embedded bundles per install;
        // dashboard deployment has dedicated coverage in `dashboard.rs`.
        dashboard: false,
    }
}

/// One unpinned Hermes install shared by every check.
///
/// `HermesIntegration::install` regenerates the full plugin from embedded
/// templates, so the output is identical for every unpinned install — there
/// is no reason to redo it per test. Each check writes its own uniquely
/// named script into the shared plugin dir and only mutates state inside its
/// own python interpreter, so tests stay independent. Checks that need a
/// different `InstallContext` (e.g. a pinned project root) keep a private
/// `TempDir` install.
///
/// The bundle is also shared across *processes* (see [`cached_install_home`]):
/// nextest runs one process per test, so a `LazyLock` alone re-renders the
/// bundle 70+ times per suite. Rendering is not cheap — `get_tool_definitions`
/// probes the host `ast-grep` with `--version` and `outline --help`, two real
/// subprocess spawns, and Windows CI resolves `ast-grep` through an npm shim.
struct SharedInstall {
    home: PathBuf,
    plugin_dir: PathBuf,
    fake_tools_dir: PathBuf,
    /// Set only on the fallback path, where the bundle could not be shared
    /// and this process rendered a private copy that it must clean up.
    _tempdir: Option<TempDir>,
}

/// Written last, so its presence means the bundle beside it is complete.
const INSTALL_READY_MARKER: &str = ".tracedecay-hermes-install-ready";
/// How long a process waits for a peer that is already rendering the bundle
/// before giving up and rendering a private copy.
const INSTALL_WAIT_TIMEOUT: Duration = Duration::from_secs(60);
/// A lock older than this outlived the run that took it, so it is stale.
const INSTALL_LOCK_STALE_AFTER: Duration = Duration::from_secs(300);

static SHARED_INSTALL: LazyLock<SharedInstall> = LazyLock::new(|| match cached_install_home() {
    Some(home) => SharedInstall {
        plugin_dir: plugin_dir_for(&home),
        fake_tools_dir: fake_tools_dir_for(&home),
        home,
        _tempdir: None,
    },
    None => {
        let tempdir = TempDir::new().unwrap();
        render_install(tempdir.path()).unwrap();
        SharedInstall {
            home: tempdir.path().to_path_buf(),
            plugin_dir: plugin_dir_for(tempdir.path()),
            fake_tools_dir: fake_tools_dir_for(tempdir.path()),
            _tempdir: Some(tempdir),
        }
    }
});

fn plugin_dir_for(home: &Path) -> PathBuf {
    home.join(".hermes/plugins/tracedecay")
}

fn fake_tools_dir_for(home: &Path) -> PathBuf {
    home.join("fake-tools")
}

/// Renders everything the checks read out of a shared install: the generated
/// Hermes plugin plus the immutable fake `tracedecay` binaries the subprocess
/// failure-mode check executes.
fn render_install(home: &Path) -> std::io::Result<()> {
    HermesIntegration
        .install(&make_install_ctx(home))
        .map_err(std::io::Error::other)?;
    write_fake_tracedecay_binaries(&fake_tools_dir_for(home))
}

/// Path of the machine-shared bundle for the current inputs, rendering it
/// first if no complete bundle exists yet.
///
/// Returns `None` when the bundle cannot be shared (unwritable temp dir, a
/// peer still rendering after [`INSTALL_WAIT_TIMEOUT`]); the caller then
/// renders a private copy, which is exactly the old per-process behaviour.
fn cached_install_home() -> Option<PathBuf> {
    let root = std::env::temp_dir().join(format!("tracedecay-hermes-suite-{}", install_key()));
    let ready = root.join(INSTALL_READY_MARKER);
    if ready.is_file() {
        return Some(root);
    }

    let lock = root.with_extension("lock");
    if lock_is_stale(&lock) {
        let _ = std::fs::remove_file(&lock);
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
    {
        Ok(_) => {
            // A leftover directory here is a partial render from a run that
            // died before publishing the marker; nobody can be reading it,
            // because readers only ever follow the marker.
            let _ = std::fs::remove_dir_all(&root);
            let rendered =
                render_install(&root).and_then(|()| std::fs::write(&ready, install_key()));
            let _ = std::fs::remove_file(&lock);
            rendered.ok()?;
            Some(root)
        }
        Err(_) => wait_for_ready(&ready, &lock).then_some(root),
    }
}

/// Every input that can change the rendered bundle: the generator build (this
/// executable embeds the plugin templates), the binary path baked into
/// `tools.py`, and the `ast-grep` image whose presence decides which tool
/// definitions are rendered. Metadata only — resolving the key must not spawn
/// the subprocesses the shared bundle exists to avoid.
fn install_key() -> String {
    let mut hasher = DefaultHasher::new();
    FIXTURE_TRACEDECAY_BIN.hash(&mut hasher);
    let ast_grep = PathBuf::from(ast_grep_command().get_program());
    for path in [std::env::current_exe().ok(), Some(ast_grep)]
        .into_iter()
        .flatten()
    {
        path.hash(&mut hasher);
        file_identity(&path).hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

fn file_identity(path: &Path) -> Option<(u64, Duration)> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?;
    Some((metadata.len(), modified))
}

fn lock_is_stale(lock: &Path) -> bool {
    file_identity(lock)
        .and_then(|(_, modified)| SystemTime::UNIX_EPOCH.checked_add(modified)?.elapsed().ok())
        .is_some_and(|age| age > INSTALL_LOCK_STALE_AFTER)
}

/// Blocks until the peer holding `lock` publishes the ready marker.
///
/// The holder writes the marker before releasing the lock, so a lock that
/// disappears with no marker means the render failed and waiting longer is
/// pointless. The timeout only covers a holder killed outright.
fn wait_for_ready(ready: &Path, lock: &Path) -> bool {
    let deadline = Instant::now() + INSTALL_WAIT_TIMEOUT;
    while Instant::now() < deadline {
        if ready.is_file() {
            return true;
        }
        if !lock.exists() {
            return ready.is_file();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// Fake `tracedecay` binaries for the subprocess failure-mode check, written
/// once as part of the shared bundle.
///
/// `(file stem, POSIX body, Windows body)`. cmd.exe `echo` always appends a
/// newline that POSIX `printf` does not, and a plain `echo text 1>&2` would
/// emit a trailing space before the redirect — hence the redirect-first form
/// (`>&2 echo`).
const FAKE_TRACEDECAY_BINARIES: &[(&str, &str, &str)] = &[
    (
        // Dies mid-handshake: nonzero exit with partial stdout and stderr.
        "fake-tracedecay-crash",
        "printf '{\"content'\nprintf 'handshake aborted' >&2\nexit 3\n",
        "echo {\"content\n>&2 echo handshake aborted\nexit /b 3\n",
    ),
    (
        // Exit 0 with malformed JSON on stdout.
        "fake-tracedecay-badjson",
        "printf 'not-json-at-all'\nexit 0\n",
        "echo not-json-at-all\nexit /b 0\n",
    ),
    (
        // Exit 0 with empty stdout.
        "fake-tracedecay-empty",
        "exit 0\n",
        "exit /b 0\n",
    ),
];

fn write_fake_tracedecay_binaries(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for (name, posix_body, windows_body) in FAKE_TRACEDECAY_BINARIES {
        if cfg!(windows) {
            // CRLF matches what cmd.exe batch files are written with.
            let body = format!("@echo off\n{windows_body}").replace('\n', "\r\n");
            std::fs::write(dir.join(format!("{name}.cmd")), body)?;
            continue;
        }
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{posix_body}"))?;
        #[cfg(unix)]
        {
            let mut permissions = std::fs::metadata(&path)?.permissions();
            permissions.set_mode(permissions.mode() | 0o111);
            std::fs::set_permissions(&path, permissions)?;
        }
    }
    Ok(())
}

/// Command for the test python interpreter.
///
/// On Windows CI, launching plain `python3` resolves through PATH shims on
/// every spawn; `actions/setup-python` exports the real interpreter root in
/// `Python_ROOT_DIR`/`pythonLocation` (same preference order as
/// `common::windows_python_launcher`), so use that executable directly.
fn python_command() -> Command {
    static PYTHON: LazyLock<PathBuf> = LazyLock::new(|| {
        if cfg!(windows) {
            for var in ["Python_ROOT_DIR", "pythonLocation"] {
                if let Some(root) = std::env::var_os(var) {
                    let exe = Path::new(&root).join("python.exe");
                    if exe.is_file() {
                        return exe;
                    }
                }
            }
        }
        PathBuf::from("python3")
    });
    Command::new(&*PYTHON)
}

/// Writes `script` into the shared plugin dir and runs it with a hermetic
/// environment (isolated HOME, no HERMES_HOME/HERMES_PROFILE, no ambient
/// LCM_* knobs), passing the plugin dir as argv[1].
fn run_python_check(script_name: &str, script: &str, failure_message: &str) {
    let install = &*SHARED_INSTALL;
    let script_path = install.plugin_dir.join(script_name);
    // Script names are unique per check and their bodies are constants, so a
    // matching file in a reused bundle is already this exact script. Skipping
    // the rewrite keeps the shared plugin dir immutable across processes.
    let already_written =
        std::fs::read(&script_path).is_ok_and(|existing| existing == script.as_bytes());
    if !already_written {
        std::fs::write(&script_path, script).unwrap();
    }

    let mut command = python_command();
    command
        .arg(&script_path)
        .arg(&install.plugin_dir)
        // Isolate from the developer's real ~/.hermes.
        .env("HOME", &install.home)
        .env("TRACEDECAY_TEST_FAKE_TOOLS", &install.fake_tools_dir)
        .env_remove("HERMES_HOME")
        .env_remove("HERMES_PROFILE");
    // Ambient LCM_* vars from the worker shell must not leak into the
    // generated-plugin subprocess: many scripts set their own knobs and expect
    // a hermetic starting point.
    for (key, _) in std::env::vars() {
        if key.starts_with("LCM_") {
            command.env_remove(key);
        }
    }
    let output = command
        .output()
        .expect("python3 should run generated Hermes plugin check");
    assert!(
        output.status.success(),
        "{failure_message}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Runs a check script that starts from [`PLUGIN_LOAD_PRELUDE`].
fn run_generated_plugin_script(script_name: &str, script: &str, failure_message: &str) {
    run_python_check(
        script_name,
        &format!("{PLUGIN_LOAD_PRELUDE}\n{script}"),
        failure_message,
    );
}

/// Every generated Python module must compile standalone. Loading the plugin
/// package (as all other checks do) exercises the same parse, but a syntax
/// error in a lazily imported module would slip through — this is the one
/// place that still runs an explicit `py_compile` pass.
#[test]
fn generated_python_sources_compile() {
    run_python_check(
        "check_generated_sources_compile.py",
        PYTHON_COMPILE_CHECK,
        "generated Hermes plugin Python sources should compile",
    );
}

#[test]
fn generated_registration_degrades_without_register_tool() {
    run_generated_plugin_script(
        "check_registration_without_register_tool.py",
        r#"
class NoToolCtx:
    def __init__(self):
        self.hooks = []
        self.memory_providers = []
        self.context_engines = []

    def register_hook(self, name, handler):
        self.hooks.append((name, handler))

    def register_memory_provider(self, provider):
        self.memory_providers.append(provider)

    def register_context_engine(self, engine):
        self.context_engines.append(engine)

ctx = NoToolCtx()
plugin.register(ctx)

assert [name for name, _ in ctx.hooks] == [
    "pre_llm_call", "post_tool_call", "on_session_end"
]
assert len(ctx.memory_providers) == 1
assert len(ctx.context_engines) == 1
assert isinstance(ctx.context_engines[0], plugin.TraceDecayContextEngine)
"#,
        "generated plugin registration should continue when host lacks register_tool",
    );
}

#[test]
fn generated_registration_continues_when_register_tool_raises() {
    run_generated_plugin_script(
        "check_registration_register_tool_raises.py",
        r#"
class RaisingToolCtx:
    context_engine_tool_handlers_receive_messages = True

    def __init__(self):
        self.tool_calls = []
        self.hooks = []
        self.memory_providers = []
        self.context_engines = []

    def register_tool(self, **kwargs):
        self.tool_calls.append(kwargs["name"])
        raise RuntimeError("host register_tool failed")

    def register_hook(self, name, handler):
        self.hooks.append((name, handler))

    def register_memory_provider(self, provider):
        self.memory_providers.append(provider)

    def register_context_engine(self, engine):
        self.context_engines.append(engine)

ctx = RaisingToolCtx()
plugin.register(ctx)

assert ctx.tool_calls
assert [name for name, _ in ctx.hooks] == [
    "pre_llm_call", "post_tool_call", "on_session_end"
]
assert len(ctx.memory_providers) == 1
assert len(ctx.context_engines) == 1
"#,
        "generated plugin registration should continue when register_tool raises",
    );
}

#[test]
fn generated_native_callbacks_forward_content_free_edit_and_stop_events() {
    run_generated_plugin_script(
        "check_native_hook_forwarding.py",
        r#"
import copy

captured = []
plugin._notify_host_receipt = (
    lambda event, thread_name: captured.append((copy.deepcopy(event), thread_name)) or True
)
plugin._code_project_root = lambda **_kwargs: "/workspace/project"

plugin._post_tool_call(
    hook_event_name="post_tool_call",
    session_id="session-hermes-native",
    turn_id="turn-hermes-native",
    tool_call_id="call-hermes-native",
    tool_name="write_file",
    tool_input={"path": "/workspace/project/secret.txt", "content": "secret"},
    result={"files_modified": ["/workspace/project/secret.txt"]},
    cwd="/workspace/project",
    status="ok",
)
plugin._on_session_end(
    hook_event_name="on_session_end",
    session_id="session-hermes-native",
    turn_id="turn-hermes-native",
    task_id="task-hermes-native",
    cwd="/workspace/project",
    completed=True,
    interrupted=False,
    model="test-model",
    platform="cli",
)

assert len(captured) == 2, captured
saved_edit, edit_thread = captured[0]
assert edit_thread == "tracedecay-native-post-tool"
assert saved_edit == {
    "hook_event_name": "post_tool_call",
    "cwd": "/workspace/project",
    "session_id": "session-hermes-native",
    "tool_input": {},
    "tool_name": "write_file",
    "extra": {
        "status": "ok",
        "tool_call_id": "call-hermes-native",
        "turn_id": "turn-hermes-native",
    },
    "_project_candidate": "/workspace/project",
    "_trusted_project": False,
    "_hermes_home": None,
}
stop, stop_thread = captured[1]
assert stop_thread == "tracedecay-native-session-end"
assert stop["hook_event_name"] == "on_session_end"
assert stop["cwd"] == "/workspace/project"
assert stop["session_id"] == "session-hermes-native"
assert stop["tool_input"] is None and stop["tool_name"] is None
assert stop["extra"]["turn_id"] == "turn-hermes-native"
assert stop["extra"]["task_id"] == "task-hermes-native"
assert "content" not in repr(captured)
assert "secret.txt" not in repr(captured)

class Ctx:
    def __init__(self):
        self.hooks = []
    def register_hook(self, name, handler):
        self.hooks.append((name, handler))

ctx = Ctx()
plugin.register(ctx)
assert [name for name, _ in ctx.hooks] == [
    "pre_llm_call",
    "post_tool_call",
    "on_session_end",
]
"#,
        "generated Hermes callbacks must forward native edit/stop identity without content",
    );
}

#[test]
fn generated_native_callback_queue_applies_explicit_backpressure() {
    run_generated_plugin_script(
        "check_native_hook_queue_bound.py",
        r#"
class DormantThread:
    def __init__(self, *args, **kwargs):
        pass
    def start(self):
        pass

plugin.threading.Thread = DormantThread
plugin._HOST_RECEIPT_QUEUE.clear()
plugin._HOST_RECEIPT_WORKER_ACTIVE = False

outcomes = [
    plugin._notify_host_receipt({"sequence": sequence}, "test-native-hook")
    for sequence in range(plugin._HOST_RECEIPT_QUEUE_LIMIT + 1)
]
assert outcomes[:-1] == [True] * plugin._HOST_RECEIPT_QUEUE_LIMIT
assert outcomes[-1] is False
assert len(plugin._HOST_RECEIPT_QUEUE) == plugin._HOST_RECEIPT_QUEUE_LIMIT
assert [event["sequence"] for event in plugin._HOST_RECEIPT_QUEUE] == list(
    range(plugin._HOST_RECEIPT_QUEUE_LIMIT)
)
"#,
        "generated Hermes callback queue must reject overflow without evicting admitted events",
    );
}

#[test]
fn generated_registration_skips_tools_without_message_forwarding_capability() {
    run_generated_plugin_script(
        "check_registration_capability_gate.py",
        r#"
class UnsafeRegisteredToolCtx:
    context_engine_tool_handlers_receive_messages = False

    def __init__(self):
        self.tools = []
        self.context_engines = []

    def register_tool(self, **kwargs):
        self.tools.append(kwargs["name"])

    def register_hook(self, name, handler):
        pass

    def register_memory_provider(self, provider):
        pass

    def register_context_engine(self, engine):
        self.context_engines.append(engine)

ctx = UnsafeRegisteredToolCtx()
plugin.register(ctx)

# Code-graph / memory / transcript tools register even without message
# forwarding; only the live-ingest LCM verbs whose schemas carry the
# in-memory messages list (and the context-engine tool mirrors) are gated.
assert "tracedecay_search" in ctx.tools
assert "tracedecay_context" in ctx.tools
assert "tracedecay_lcm_compress" not in ctx.tools
assert "tracedecay_lcm_preflight" not in ctx.tools
assert "lcm_grep" not in ctx.tools
assert len(ctx.context_engines) == 1
engine = ctx.context_engines[0]
assert engine.name == "tracedecay"
assert "lcm_grep" in {schema["name"] for schema in engine.get_tool_schemas()}
"#,
        "generated plugin should gate only message-dependent tools when host does not forward messages",
    );
}

#[test]
fn generated_context_engine_exposes_native_lcm_surface_and_dispatch() {
    run_generated_plugin_script(
        "check_context_engine_native_surface.py",
        r#"
import json

plugin._resolved_project_scope = lambda path, *_args: path
engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project")

assert engine.name == "tracedecay"

schemas = engine.get_tool_schemas()
schemas_by_name = {schema["name"]: schema for schema in schemas}
schema_names = {schema["name"] for schema in schemas}
expected_native = {
    "lcm_grep",
    "lcm_load_session",
    "lcm_describe",
    "lcm_expand",
    "lcm_expand_query",
    "lcm_status",
    "lcm_doctor",
}
assert expected_native.issubset(schema_names)
assert "tracedecay_lcm_preflight" not in schema_names
assert "tracedecay_lcm_compress" not in schema_names
assert all(name.startswith("lcm_") for name in schema_names)

grep_params = schemas_by_name["lcm_grep"]["parameters"]
assert "session_scope" in grep_params["properties"]
assert "scope" not in grep_params["properties"]
assert grep_params["properties"]["session_scope"]["enum"] == ["current", "all", "session"]
assert grep_params["required"] == ["query"]

load_params = schemas_by_name["lcm_load_session"]["parameters"]
assert "max_content_chars" in load_params["properties"]
assert "roles" in load_params["properties"]
assert "time_from" in load_params["properties"]
assert "time_to" in load_params["properties"]
assert "role" not in load_params["properties"]
assert "start_time" not in load_params["properties"]
assert "end_time" not in load_params["properties"]
assert "content_limit" not in load_params["properties"]
assert "after_store_id" not in load_params["properties"]
assert load_params["required"] == ["session_id"]

describe_params = schemas_by_name["lcm_describe"]["parameters"]
assert "node_id" in describe_params["properties"]
assert "externalized_ref" in describe_params["properties"]
assert "session_id" not in describe_params["properties"]
assert describe_params.get("required") == []

expand_params = schemas_by_name["lcm_expand"]["parameters"]
assert "node_id" in expand_params["properties"]
assert "store_id" in expand_params["properties"]
assert "externalized_ref" in expand_params["properties"]
assert "session_id" in expand_params["properties"]
assert "source_offset" in expand_params["properties"]
assert "source_limit" in expand_params["properties"]
assert "target" not in expand_params["properties"]
assert expand_params.get("required") == []

status_params = schemas_by_name["lcm_status"]["parameters"]
doctor_params = schemas_by_name["lcm_doctor"]["parameters"]
assert status_params["properties"] == {}
assert doctor_params["properties"] == {}

status = engine.get_status()
assert status["engine"] == "tracedecay"
assert status["session_id"] == "session-1"
assert status["active_session_id"] == "session-1"
assert "storage_scope" not in status
assert "hermes_home" not in status
assert "lcm_project_root" not in status
assert status["project_root"] == "/tmp/project"
assert status["tracedecay_binary_path"] == plugin.tools.TRACEDECAY_BIN
assert isinstance(status["tracedecay_binary_available"], bool)
assert status["context_engine_tool_names"] == sorted(schema_names)
assert status["route_failures"] == {}
assert status["cooldown_routes"] == []
assert status["route_cooldowns"] == {}
assert status["last_compress_result"] == {"status": "never_ran"}
assert status["live_ingest"]["registered_tool_names"] == []
assert status["live_ingest"]["context_tool_names"] == []
assert status["live_ingest"]["host_forwards_messages"] is None
assert status["live_ingest"]["message_dependent_tools_registered"] is False
assert status["live_ingest"]["gate_reason"] == "not_registered"

calls = []

def fake_call_tracedecay_tool(name, args, **kwargs):
    calls.append((name, args, kwargs))
    return json.dumps({"ok": True, "tool": name})

plugin.tools.call_tracedecay_tool = fake_call_tracedecay_tool
native_result = engine.handle_tool_call(
    "lcm_grep",
    {
        "query": "orchard",
        "session_scope": "current",
        "sort": "relevance",
        "source": "cli",
        "role": "assistant",
        "time_from": 1,
        "time_to": 2,
    },
    messages=[{"role": "user", "content": "current turn"}],
)
load_result = engine.handle_tool_call(
    "lcm_load_session",
    {"session_id": "session-1", "max_content_chars": 123, "roles": ["user", "tool"], "time_from": 1, "time_to": 2},
    messages=[{"role": "assistant", "content": "load turn"}],
)
describe_node_result = engine.handle_tool_call("lcm_describe", {"node_id": 7})
describe_payload_result = engine.handle_tool_call("lcm_describe", {"externalized_ref": "payload_123.payload"})
expand_result = engine.handle_tool_call(
    "lcm_expand",
    {"store_id": 42, "session_id": "session-foreign", "max_tokens": 77, "source_offset": 3, "source_limit": 2},
)
direct_result = engine.handle_tool_call("tracedecay_lcm_grep", {"query": "direct", "session_scope": "all"})
implicit_current_result = engine.handle_tool_call("lcm_grep", {"query": "implicit"})

assert json.loads(native_result) == {"ok": True, "tool": "tracedecay_lcm_grep"}
assert json.loads(load_result) == {"ok": True, "tool": "tracedecay_lcm_load_session"}
assert json.loads(describe_node_result) == {"ok": True, "tool": "tracedecay_lcm_describe"}
assert json.loads(describe_payload_result) == {"ok": True, "tool": "tracedecay_lcm_describe"}
assert json.loads(expand_result) == {"ok": True, "tool": "tracedecay_lcm_expand"}
assert json.loads(direct_result) == {"ok": True, "tool": "tracedecay_lcm_grep"}
assert json.loads(implicit_current_result) == {"ok": True, "tool": "tracedecay_lcm_grep"}
assert calls[0][0] == "tracedecay_lcm_preflight"
assert calls[0][1]["messages"] == [{"role": "user", "content": "current turn"}]
assert calls[0][1]["session_id"] == "session-1"
assert "project_root" not in calls[0][1]
assert calls[0][2] == {"project_root": "/tmp/project"}
assert calls[1][0] == "tracedecay_lcm_grep"
assert calls[1][1]["query"] == "orchard"
assert calls[1][1]["scope"] == "current"
assert calls[1][1]["sort"] == "relevance"
assert calls[1][1]["source"] == "cli"
assert calls[1][1]["role"] == "assistant"
assert calls[1][1]["start_time"] == 1
assert calls[1][1]["end_time"] == 2
assert "session_scope" not in calls[1][1]
assert "time_from" not in calls[1][1]
assert "time_to" not in calls[1][1]
assert "messages" not in calls[1][1]
assert "project_root" not in calls[1][1]
assert calls[1][1]["session_id"] == "session-1"
assert calls[1][2] == {"project_root": "/tmp/project"}
assert calls[2][0] == "tracedecay_lcm_preflight"
assert calls[2][1]["messages"] == [{"role": "assistant", "content": "load turn"}]
assert calls[3][0] == "tracedecay_lcm_load_session"
assert calls[3][1]["content_limit"] == 123
assert calls[3][1]["roles"] == ["user", "tool"]
assert calls[3][1]["start_time"] == 1
assert calls[3][1]["end_time"] == 2
assert "max_content_chars" not in calls[3][1]
assert "role" not in calls[3][1]
assert "time_from" not in calls[3][1]
assert "time_to" not in calls[3][1]
assert calls[4][0] == "tracedecay_lcm_describe"
assert calls[4][1]["target"] == {"kind": "summary_node", "node_id": "7"}
assert "node_id" not in calls[4][1]
assert calls[5][0] == "tracedecay_lcm_describe"
assert calls[5][1]["target"] == {"kind": "external_payload", "payload_ref": "payload_123.payload"}
assert "externalized_ref" not in calls[5][1]
assert calls[6][0] == "tracedecay_lcm_expand"
assert calls[6][1]["target"] == {"kind": "raw_message", "store_id": 42}
assert calls[6][1]["session_id"] == "session-foreign"
assert calls[6][1]["content_limit"] == 308
assert "source_offset" not in calls[6][1]
assert "source_limit" not in calls[6][1]
assert "store_id" not in calls[6][1]
assert "max_tokens" not in calls[6][1]
assert calls[7][0] == "tracedecay_lcm_grep"
assert calls[7][1]["query"] == "direct"
assert calls[7][1]["scope"] == "all"
assert "session_scope" not in calls[7][1]
assert "project_root" not in calls[7][1]
assert calls[7][2] == {"project_root": "/tmp/project"}
assert calls[7][1]["session_id"] == "session-1"
assert calls[8][0] == "tracedecay_lcm_grep"
assert calls[8][1]["query"] == "implicit"
assert calls[8][1]["scope"] == "current"
assert "session_scope" not in calls[8][1]
assert "project_root" not in calls[8][1]
assert calls[8][2] == {"project_root": "/tmp/project"}
assert calls[8][1]["session_id"] == "session-1"
"#,
        "generated context engine should expose Hermes-style native LCM surface",
    );
}

#[test]
fn generated_context_engine_exposes_optional_hermes_hooks() {
    run_generated_plugin_script(
        "check_context_engine_optional_hooks.py",
        r#"
import json

engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="old-session", project_root="/tmp/project")
engine.threshold_tokens = 2000

calls = []

def envelope(payload):
    return json.dumps({"content": [{"type": "text", "text": json.dumps(payload)}]})

def fake_call_tracedecay_tool(name, args, **kwargs):
    calls.append((name, args, kwargs))
    if name == "tracedecay_lcm_preflight":
        return envelope({"should_compress": True})
    if name == "tracedecay_lcm_compress":
        return envelope({
            "status": "compressed",
            "replay_messages": [{"role": "system", "content": "summary"}],
            "summary_node_count": 1,
            "raw_message_count": 2,
        })
    if name == "tracedecay_lcm_session_boundary":
        return json.dumps({"ok": True})
    raise AssertionError(f"unexpected tool call: {name}")

plugin.tools.call_tracedecay_tool = fake_call_tracedecay_tool

assert engine.has_content_to_compress([]) is False
assert engine.has_content_to_compress([
    {"role": "system", "content": ""},
    {"role": "assistant", "content": "   "},
]) is False
assert calls == []
assert engine.has_content_to_compress([
    {"role": "user", "content": "single message stays protected"},
]) is False

eligible = [
    {"role": "user", "content": "older content"},
    {"role": "assistant", "content": "newer content"},
]
assert engine.has_content_to_compress(eligible, current_tokens=123) is True
assert calls == []

compressed = engine.compress(
    [{"role": "user", "content": "one"}, {"role": "assistant", "content": "two"}],
    current_tokens=2100,
)
assert compressed == [{"role": "system", "content": "summary"}]
assert engine.should_defer_preflight_to_real_usage(rough_tokens=2200) is False
status = engine.get_status()
assert status["awaiting_real_usage_after_compression"] is True
assert status["last_compress_result"]["status"] == "compressed"
assert status["last_compress_result"]["summary_node_count"] == 1
assert status["last_compress_result"]["raw_message_count"] == 2

engine.update_from_response({"prompt_tokens": 321, "completion_tokens": 7})
assert engine.should_defer_preflight_to_real_usage(rough_tokens=2200) is True
assert engine.should_defer_preflight_to_real_usage(rough_tokens=7000) is False
assert engine.should_defer_preflight_to_real_usage(rough_tokens=2200) is True
assert engine.last_real_prompt_tokens == 321

engine.carry_over_new_session_context("old-session", "new-session")
boundary_calls = [call for call in calls if call[0] == "tracedecay_lcm_session_boundary"]
assert len(boundary_calls) == 1
assert boundary_calls[0][1]["old_session_id"] == "old-session"
assert boundary_calls[0][1]["session_id"] == "new-session"
assert boundary_calls[0][1]["boundary_reason"] == "compression"
assert engine.active_session_id == "new-session"
"#,
        "generated context engine should expose optional Hermes hook shims",
    );
}

#[test]
fn generated_context_engine_never_uses_hermes_home_as_storage_identity() {
    run_generated_plugin_script(
        "check_context_engine_env_home.py",
        r#"
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_env_home"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

os.environ["HERMES_HOME"] = "/tmp/hermes-from-env"

calls = []

def fake_call_tracedecay_tool(name, args, **kwargs):
    calls.append((name, args, kwargs))
    return json.dumps({"content": [{"type": "text", "text": json.dumps({"status": "ok"})}]})

plugin.tools.call_tracedecay_tool = fake_call_tracedecay_tool
plugin._resolved_project_scope = lambda path, *_args: (
    str(path) if path and str(path) == "/tmp/project" else None
)

engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1")
assert os.path.normcase(os.path.realpath(engine.hermes_home)) == os.path.normcase(
    os.path.realpath(str(plugin_dir.parent.parent))
)
status = engine.get_status()
assert "storage_scope" not in status
assert "hermes_home" not in status
assert "lcm_project_root" not in status
assert status["project_root"] is None

engine.handle_tool_call(
    "lcm_grep",
    {"query": "orchard"},
    messages=[{"role": "user", "content": "profile current turn"}],
)

assert calls[0][0] == "tracedecay_lcm_preflight"
assert "project_root" not in calls[0][1]
assert calls[0][1]["storage_scope"] == "user"
assert calls[0][2] == {}
assert calls[0][1]["messages"] == [{"role": "user", "content": "profile current turn"}]
assert calls[1][0] == "tracedecay_lcm_grep"
assert "project_root" not in calls[1][1]
assert calls[1][1]["storage_scope"] == "user"
assert calls[1][2] == {}

os.environ["HERMES_HOME"] = "/tmp/another-hermes-home"
other = plugin.TraceDecayContextEngine()
other.initialize(session_id="session-2")
other.status()
assert calls[-1][1]["storage_scope"] == "user"
assert calls[-1][2] == {}
"#,
        "generated context engine must not use HERMES_HOME as a TraceDecay storage identity",
    );
}

#[test]
fn context_engine_debounces_current_turn_preflight_when_messages_unchanged() {
    run_generated_plugin_script(
        "check_preflight_debounce.py",
        r#"
import json

calls = []

def fake_call_tracedecay_tool(name, args, **kwargs):
    calls.append((name, dict(args), dict(kwargs)))
    return json.dumps({"status": "ok", "tool": name})

plugin.tools.call_tracedecay_tool = fake_call_tracedecay_tool
engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project")

same_messages = [{"role": "user", "content": "unchanged context"}]
changed_messages = [{"role": "user", "content": "changed context"}]

for _ in range(3):
    engine.handle_tool_call("lcm_status", {}, messages=same_messages)
engine.handle_tool_call("lcm_status", {}, messages=changed_messages)

preflight_calls = [call for call in calls if call[0] == "tracedecay_lcm_preflight"]
status_calls = [call for call in calls if call[0] == "tracedecay_lcm_status"]

assert len(status_calls) == 4
assert len(preflight_calls) == 2
assert preflight_calls[0][1]["messages"] == same_messages
assert preflight_calls[1][1]["messages"] == changed_messages
"#,
        "generated context engine should debounce unchanged preflight message arrays",
    );
}

#[test]
fn context_engine_wraps_extraction_result_into_provided_summarizer_route() {
    run_generated_plugin_script(
        "check_auxiliary_extraction_route.py",
        r#"
import json
import os

os.environ["LCM_EXTRACTION_ENABLED"] = "true"
os.environ["LCM_EXTRACTION_MODEL"] = "openai/gpt-5.4-mini"
os.environ["LCM_EXTRACTION_OUTPUT_PATH"] = "/tmp/extractions"
os.environ["LCM_SUMMARY_TIMEOUT_MS"] = "45000"

compress_calls = []

def fake_call_tracedecay_json(name, args, **kwargs):
    if name != "tracedecay_lcm_compress":
        raise AssertionError(name)
    compress_calls.append((name, dict(args)))
    summarizer_mode = (args.get("summarizer") or {}).get("mode")
    if summarizer_mode == "hermes_auxiliary":
        return {
            "status": "needs_summary",
            "summary_request": {
                "focus_topic": "billing",
                "source_range": {"from_store_id": 11, "to_store_id": 11},
                "source_messages": [
                    {"store_id": 11, "role": "user", "content": "We decided to rotate keys weekly."}
                ],
                "extraction_request": {
                    "session_id": "session-1",
                    "source_range": {"from_store_id": 11, "to_store_id": 11},
                    "source_messages": [
                        {"store_id": 11, "role": "user", "content": "We decided to rotate keys weekly."}
                    ],
                    "serialized_messages": "[USER]: We decided to rotate keys weekly.",
                    "prompt": "extract decisions"
                },
            },
            "replay_messages": [{"role": "user", "content": "fresh"}],
        }
    if summarizer_mode == "provided":
        return {
            "status": "ok",
            "reason": "compressed_backlog",
            "summary_nodes_created": 1,
            "summary_nodes": [],
            "replay_messages": [{"role": "system", "content": "summary"}],
            "frontier": {
                "provider": "cursor",
                "conversation_id": "session-1",
                "current_session_id": "session-1",
                "current_frontier_store_id": 11,
                "last_finalized_session_id": None,
                "last_finalized_frontier_store_id": None,
                "maintenance_debt": [],
            },
        }
    raise AssertionError(f"unexpected summarizer mode: {summarizer_mode}")

plugin.call_tracedecay_json = fake_call_tracedecay_json

class _FakeMessage:
    def __init__(self, content):
        self.content = content

class _FakeChoice:
    def __init__(self, content):
        self.message = _FakeMessage(content)

class _FakeResponse:
    def __init__(self, content):
        self.choices = [_FakeChoice(content)]

class _AuxClient:
    def __init__(self):
        self.calls = []
    def call_llm(self, **kwargs):
        self.calls.append(dict(kwargs))
        if kwargs.get("task") == "extraction":
            return _FakeResponse("<think>hidden reasoning</think>- Decision: rotate keys weekly")
        if kwargs.get("task") == "compression":
            return _FakeResponse("Compact summary text")
        raise AssertionError(kwargs.get("task"))

agent = type("Agent", (), {"auxiliary_client": _AuxClient()})()

engine = plugin.TraceDecayContextEngine()
engine.agent = agent
engine.initialize(session_id="session-1", project_root="/tmp/project")

result = engine._compress_to_result([{"role": "user", "content": "current"}], current_tokens=700)

assert result["status"] == "ok"
assert len(compress_calls) == 2
provided_args = compress_calls[1][1]
assert provided_args["summarizer"]["mode"] == "provided"
route_payload = json.loads(provided_args["summarizer"]["route"])
assert route_payload["pre_compaction_extraction"]["status"] == "ok"
assert route_payload["pre_compaction_extraction"]["items"] == ["Decision: rotate keys weekly"]
assert route_payload["pre_compaction_extraction"]["model"] == "openai/gpt-5.4-mini"
assert route_payload["pre_compaction_extraction"]["output_path"] == "/tmp/extractions"
assert route_payload["route"] == "default"

tasks = [call["task"] for call in agent.auxiliary_client.calls]
assert tasks == ["extraction", "compression"]
"#,
        "generated context engine should attach extraction results to provided route envelope",
    );
}

#[test]
fn context_engine_compress_continues_when_extraction_call_fails() {
    run_generated_plugin_script(
        "check_auxiliary_extraction_failure_non_blocking.py",
        r#"
import json
import os

os.environ["LCM_EXTRACTION_ENABLED"] = "true"

compress_calls = []

def fake_call_tracedecay_json(name, args, **kwargs):
    compress_calls.append(dict(args))
    mode = (args.get("summarizer") or {}).get("mode")
    if mode == "hermes_auxiliary":
        return {
            "status": "needs_summary",
            "summary_request": {
                "source_messages": [{"store_id": 1, "role": "user", "content": "hello"}],
                "source_range": {"from_store_id": 1, "to_store_id": 1},
                "extraction_request": {
                    "session_id": "session-1",
                    "source_range": {"from_store_id": 1, "to_store_id": 1},
                    "prompt": "extract decisions"
                },
            },
            "replay_messages": [],
        }
    return {
        "status": "ok",
        "reason": "compressed_backlog",
        "summary_nodes_created": 1,
        "summary_nodes": [],
        "replay_messages": [],
        "frontier": {
            "provider": "cursor",
            "conversation_id": "session-1",
            "current_session_id": "session-1",
            "current_frontier_store_id": 1,
            "last_finalized_session_id": None,
            "last_finalized_frontier_store_id": None,
            "maintenance_debt": [],
        },
    }

plugin.call_tracedecay_json = fake_call_tracedecay_json

class _FakeMessage:
    def __init__(self, content):
        self.content = content

class _FakeChoice:
    def __init__(self, content):
        self.message = _FakeMessage(content)

class _FakeResponse:
    def __init__(self, content):
        self.choices = [_FakeChoice(content)]

class _AuxClient:
    def call_llm(self, **kwargs):
        if kwargs.get("task") == "extraction":
            raise RuntimeError("extraction backend unavailable")
        return _FakeResponse("summary text")

agent = type("Agent", (), {"auxiliary_client": _AuxClient()})()

engine = plugin.TraceDecayContextEngine()
engine.agent = agent
engine.initialize(session_id="session-1", project_root="/tmp/project")

result = engine._compress_to_result([{"role": "user", "content": "current"}], current_tokens=700)
assert result["status"] == "ok"
provided = compress_calls[1]
payload = json.loads(provided["summarizer"]["route"])
assert payload["pre_compaction_extraction"]["status"] == "failed_non_blocking"
assert "extraction backend unavailable" in payload["pre_compaction_extraction"]["error"]
"#,
        "generated context engine should never block compression when extraction fails",
    );
}

#[test]
fn generated_context_engine_home_default_uses_installed_profile() {
    run_generated_plugin_script(
        "check_context_engine_default_home.py",
        r#"
import os
import pathlib
import tempfile

os.environ.pop("HERMES_HOME", None)
with tempfile.TemporaryDirectory() as tmp:
    home = pathlib.Path(tmp) / "isolated-home"
    home.mkdir()
    # expanduser reads HOME on POSIX and USERPROFILE on Windows.
    os.environ["HOME"] = str(home)
    os.environ["USERPROFILE"] = str(home)
    expected = str(plugin_dir.parent.parent)

    engine = plugin.TraceDecayContextEngine()
    engine.initialize(session_id="session-1")

    def normalized(path):
        return os.path.normcase(os.path.realpath(path))

    assert normalized(engine.hermes_home) == normalized(expected), engine.hermes_home
    status = engine.get_status()
    assert "storage_scope" not in status
    assert "hermes_home" not in status
    assert "lcm_project_root" not in status
    assert status["project_root"] is None, status
"#,
        "Hermes home defaults to the installed profile but never TraceDecay storage",
    );
}

#[test]
fn generated_tools_bridge_preserves_message_kwargs_in_json_args() {
    run_generated_plugin_script(
        "check_tools_message_kwargs.py",
        r#"
import importlib.util
import json
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
tools_path = plugin_dir / "tools.py"
spec = importlib.util.spec_from_file_location("tracedecay_hermes_tools_kwargs", tools_path)
tools = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tools)

calls = []

class Result:
    returncode = 0
    stderr = ""
    stdout = json.dumps({"content": [{"type": "text", "text": "{}"}]})

def fake_run(argv, **kwargs):
    calls.append(argv)
    return Result()

tools.subprocess.run = fake_run
tools.call_tracedecay_tool(
    "tracedecay_lcm_grep",
    {"query": "orchard"},
    messages=[{"role": "user", "content": "current turn"}],
)

args = json.loads(calls[0][calls[0].index("--args") + 1])
assert args["query"] == "orchard"
assert args["messages"] == [{"role": "user", "content": "current turn"}]

tools.call_tracedecay_tool("tracedecay_status", {})
args = json.loads(calls[1][calls[1].index("--args") + 1])
assert args["format"] == "json"
"#,
        "generated subprocess bridge should preserve messages kwargs in JSON args",
    );
}

#[test]
fn generated_context_engine_resolves_configured_hermes_home_on_registration() {
    run_generated_plugin_script(
        "check_context_engine_config_home.py",
        r#"
import importlib.machinery
import importlib.util
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_config_home"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

class Ctx:
    def __init__(self):
        self.config = {"hermes_home": "/tmp/hermes-from-config"}
        self.context_engines = []
    def register_hook(self, name, handler):
        pass
    def register_context_engine(self, engine):
        self.context_engines.append(engine)

ctx = Ctx()
plugin.register(ctx)

assert len(ctx.context_engines) == 1
engine = ctx.context_engines[0]
assert engine.name == "tracedecay"
assert engine.hermes_home == "/tmp/hermes-from-config"
"#,
        "generated registration should resolve configured hermes_home",
    );
}

#[test]
fn generated_context_engine_registers_when_supported() {
    run_python_check(
        "check_context_engine.py",
        r#"
import importlib.machinery
import importlib.util
import os
import pathlib
import sys
import types

plugin_dir = pathlib.Path(sys.argv[1])

parent_name = "_hermes_user_context"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

class ContextEngine:
    pass

agent_module = types.ModuleType("agent")
agent_module.__path__ = []
context_engine_module = types.ModuleType("agent.context_engine")
context_engine_module.ContextEngine = ContextEngine
agent_module.context_engine = context_engine_module
sys.modules["agent"] = agent_module
sys.modules["agent.context_engine"] = context_engine_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

class FullCtx:
    def __init__(self):
        self.tools = []
        self.hooks = []
        self.context_engines = []

    def register_tool(self, **kwargs):
        self.tools.append(kwargs)

    def register_hook(self, name, handler):
        self.hooks.append((name, handler))

    def register_context_engine(self, engine):
        self.context_engines.append(engine)

ctx = FullCtx()
plugin.register(ctx)
assert len(ctx.context_engines) == 1
engine = ctx.context_engines[0]
assert isinstance(engine, plugin.TraceDecayContextEngine)
assert isinstance(engine, ContextEngine)

plugin._resolved_project_scope = lambda path, *_args: path
engine.initialize(
    session_id="session-123",
    hermes_home="/tmp/hermes-profile",
    project_root="/tmp/project",
)
assert engine.active_session_id == "session-123"
assert engine.hermes_home == "/tmp/hermes-profile"
assert engine.project_root == "/tmp/project"

# Project routing is transport-only and never derived from Hermes home.
assert not hasattr(plugin, "_storage_args")
assert plugin._project_call_kwargs("/tmp/project") == {
    "project_root": "/tmp/project",
}

calls = []

def fake_call_tracedecay_tool(name, args, **kwargs):
    calls.append((name, args, kwargs))
    return "{}"

plugin.tools.call_tracedecay_tool = fake_call_tracedecay_tool
plugin._resolved_project_scope = lambda path, *_args: (
    str(path) if path and str(path) == "/tmp/project" else None
)

profile_engine = plugin.TraceDecayContextEngine()
profile_engine.on_session_start(session_id="session-1", hermes_home="/tmp/hermes")
profile_engine.should_compress_preflight(messages=[], current_tokens=123)
name, args, kwargs = calls.pop()
assert name == "tracedecay_lcm_preflight"
assert args["session_id"] == "session-1"
assert "project_root" not in args
assert args["storage_scope"] == "user"
assert kwargs == {}

project_engine = plugin.TraceDecayContextEngine()
project_engine.on_session_start(
    session_id="session-2",
    hermes_home="/tmp/hermes",
    project_root="/tmp/project",
)
project_engine.should_compress_preflight(messages=[], current_tokens=456)
name, args, kwargs = calls.pop()
assert name == "tracedecay_lcm_preflight"
assert args["session_id"] == "session-2"
assert "project_root" not in args
assert kwargs == {"project_root": "/tmp/project"}

project_engine = plugin.TraceDecayContextEngine()
project_engine.initialize(session_id="initial", project_root="/tmp/project")
project_engine.on_session_start(session_id="next", project_root="/tmp/project")
project_engine.should_compress_preflight(messages=[], current_tokens=789)
name, args, kwargs = calls.pop()
assert name == "tracedecay_lcm_preflight"
assert args["session_id"] == "next"
assert "project_root" not in args
assert kwargs == {"project_root": "/tmp/project"}

profile_engine = plugin.TraceDecayContextEngine()
profile_engine.initialize(session_id="initial", hermes_home="/tmp/hermes")
profile_engine.on_session_start(session_id="next")
profile_engine.should_compress_preflight(messages=[], current_tokens=321)
name, args, kwargs = calls.pop()
assert name == "tracedecay_lcm_preflight"
assert args["session_id"] == "next"
assert "project_root" not in args
assert args["storage_scope"] == "user"
assert kwargs == {}

class LegacyCtx:
    def register_tool(self, *args, **kwargs):
        pass

    def register_hook(self, *args, **kwargs):
        pass

legacy = LegacyCtx()
plugin.register(legacy)
"#,
        "generated plugin should register a Hermes context engine when supported",
    );
}

#[test]
fn context_engine_preflight_uses_tracedecay_tool_json_args() {
    run_python_check(
        "check_preflight_bridge.py",
        r#"
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import sys

for key in [name for name in os.environ if name.startswith("LCM_")]:
    del os.environ[key]

plugin_dir = pathlib.Path(sys.argv[1])

parent_name = "_hermes_user_context"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

calls = []

class Result:
    def __init__(self, returncode, stdout, stderr):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

def fake_run(argv, check, capture_output, text, timeout, shell):
    calls.append(argv)
    inner = {
        "status": "ok",
        "should_compress": False,
        "messages": [],
    }
    outer = {
        "content": [
            {
                "type": "text",
                "text": json.dumps(inner),
            }
        ]
    }
    return Result(0, json.dumps(outer), "")

plugin.tools.subprocess.run = fake_run

engine = plugin.TraceDecayContextEngine()
engine.initialize(hermes_home="/tmp/hermes-profile")
engine.on_session_start(session_id="session-1", project_root="/tmp/project")
result = engine._preflight_probe(
    [{"role": "user", "content": "hello"}],
    current_tokens=987,
)

assert result["status"] == "ok"
assert result["should_compress"] is False
assert result["messages"] == []

assert len(calls) == 1
argv = calls[0]
assert argv[0] == plugin.tools.TRACEDECAY_BIN
assert argv[1:6] == ["tool", "--project", "/tmp/project", "tracedecay_lcm_preflight", "--json"]
args_index = argv.index("--args")
args = json.loads(argv[args_index + 1])
assert args == {
    "format": "json",
    "provider": "hermes",
    "fresh_tail_count": 64,
    "leaf_chunk_tokens": 20000,
    "dynamic_leaf_chunk_enabled": False,
    "dynamic_leaf_chunk_max": 40000,
    "max_assembly_tokens": 0,
    "reserve_tokens_floor": 0,
    "summary_fan_in": 4,
    "incremental_max_depth": 1,
    "session_id": "session-1",
    "messages": [{"role": "user", "content": "hello"}],
    "current_tokens": 987,
}
"#,
        "generated context engine should call tracedecay_lcm_preflight through the JSON bridge",
    );
}

#[test]
fn context_engine_session_start_reports_compression_boundary() {
    run_python_check(
        "check_session_boundary_bridge.py",
        r#"
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])

parent_name = "_hermes_user_boundary"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

calls = []

class Result:
    def __init__(self, returncode, stdout, stderr):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

def fake_run(argv, check, capture_output, text, timeout, shell):
    calls.append(argv)
    inner = {"status": "ok", "recorded": True, "reason": "compression_boundary_skip_recorded"}
    outer = {"content": [{"type": "text", "text": json.dumps(inner)}]}
    return Result(0, json.dumps(outer), "")

plugin.tools.subprocess.run = fake_run

engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-a", project_root="/tmp/project")

# Plain session starts must not call the boundary tool.
engine.on_session_start(session_id="session-a")
assert calls == []

# Compression boundary session starts hand bound/old session ids to tracedecay
# so the Rust side can decide whether the boundary skipped carry-over.
engine.on_session_start(
    session_id="session-b",
    old_session_id="session-c",
    boundary_reason="compression",
)
assert len(calls) == 1
argv = calls[0]
assert argv[0] == plugin.tools.TRACEDECAY_BIN
assert argv[1:6] == ["tool", "--project", "/tmp/project", "tracedecay_lcm_session_boundary", "--json"]
args = json.loads(argv[argv.index("--args") + 1])
assert args == {
    "format": "json",
    "provider": "hermes",
    "session_id": "session-b",
    "old_session_id": "session-c",
    "boundary_reason": "compression",
    "bound_session_id": "session-a",
}

# The engine binds the new session even when the boundary tool was called.
assert engine.active_session_id == "session-b"

# A non-compression boundary must not call the tool.
engine.on_session_start(session_id="session-d", old_session_id="session-b", boundary_reason="manual")
assert len(calls) == 1
"#,
        "generated context engine should report compression boundaries through tracedecay_lcm_session_boundary",
    );
}

#[test]
fn context_engine_compress_uses_tracedecay_tool_json_args() {
    run_python_check(
        "check_compress_bridge.py",
        r#"
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import sys

for key in [name for name in os.environ if name.startswith("LCM_")]:
    del os.environ[key]

plugin_dir = pathlib.Path(sys.argv[1])

parent_name = "_hermes_user_context"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

calls = []

class Result:
    def __init__(self, returncode, stdout, stderr):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

def fake_run(argv, check, capture_output, text, timeout, shell):
    calls.append(argv)
    inner = {
        "status": "not_implemented",
        "message": "placeholder parsed",
    }
    outer = {
        "content": [
            {
                "type": "text",
                "text": json.dumps(inner),
            }
        ]
    }
    return Result(0, json.dumps(outer), "")

plugin.tools.subprocess.run = fake_run

engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-2", project_root="/tmp/project")
messages = [{"role": "assistant", "content": "hello"}]
result = engine.compress(
    list(messages),
    current_tokens=1200,
    focus_topic="handoff",
)

# Host ABC contract: compress() returns a message LIST (here: the input,
# since the placeholder result carries no usable replay window); the raw
# tracedecay result stays on the engine for diagnostics.
assert result == messages
assert engine.last_compress_result == {"status": "not_implemented", "message": "placeholder parsed"}

assert len(calls) == 1
argv = calls[0]
assert argv[0] == plugin.tools.TRACEDECAY_BIN
assert argv[1:4] == ["tool", "--project", "/tmp/project"]
tool_idx = argv.index("tracedecay_lcm_compress")
assert argv[tool_idx + 1] == "--json"
assert argv[tool_idx + 2] == "--args"
args_ref = argv[argv.index("--args") + 1]
if args_ref.startswith("@"):
    with open(args_ref[1:], encoding="utf-8") as args_file:
        args = json.load(args_file)
else:
    args = json.loads(args_ref)
# expanduser matches the plugin's fallback byte-for-byte on Windows too.
assert args == {
    "format": "json",
    "response_handle_project_root": "/tmp/project",
    "provider": "hermes",
    "fresh_tail_count": 64,
    "leaf_chunk_tokens": 20000,
    "dynamic_leaf_chunk_enabled": False,
    "dynamic_leaf_chunk_max": 40000,
    "max_assembly_tokens": 0,
    "reserve_tokens_floor": 0,
    "summary_fan_in": 4,
    "incremental_max_depth": 1,
    "session_id": "session-2",
    "messages": messages,
    "current_tokens": 1200,
    "focus_topic": "handoff",
    "summarizer": {"mode": "hermes_auxiliary"},
}
"#,
        "generated context engine should call tracedecay_lcm_compress through the JSON bridge",
    );
}

#[test]
fn context_engine_projects_config_defaults_into_preflight_and_compress_args() {
    run_python_check(
        "check_context_engine_config_defaults.py",
        r#"
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import sys

for key in [name for name in os.environ if name.startswith("LCM_")]:
    del os.environ[key]

plugin_dir = pathlib.Path(sys.argv[1])

parent_name = "_hermes_user_config_defaults"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

calls = []

class Result:
    def __init__(self, returncode, stdout, stderr):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

def fake_run(argv, check, capture_output, text, timeout, shell):
    calls.append(argv)
    outer = {"content": [{"type": "text", "text": json.dumps({"status": "ok"})}]}
    return Result(0, json.dumps(outer), "")

plugin.tools.subprocess.run = fake_run

config = {
    "threshold_tokens": 777,
    "fresh_tail_count": 5,
    "leaf_chunk_tokens": 123,
    "dynamic_leaf_chunk_enabled": True,
    "dynamic_leaf_chunk_max": 456,
    "max_assembly_tokens": 999,
    "condensation_fanin": 3,
    "context_length": 200000,
    "reserve_tokens_floor": 4096,
    "incremental_max_depth": 2,
}
engine = plugin.TraceDecayContextEngine(config=config)
engine.initialize(session_id="session-1", project_root="/tmp/project")

engine.should_compress_preflight(
    [{"role": "user", "content": "hello"}],
    current_tokens=800,
)
engine.compress(
    [{"role": "assistant", "content": "compress me"}],
    current_tokens=800,
)

assert len(calls) == 2
preflight_args = json.loads(calls[0][calls[0].index("--args") + 1])
compress_args = json.loads(calls[1][calls[1].index("--args") + 1])

for args in (preflight_args, compress_args):
    assert args["threshold_tokens"] == 777
    assert args["fresh_tail_count"] == 5
    assert args["leaf_chunk_tokens"] == 123
    assert args["dynamic_leaf_chunk_enabled"] is True
    assert args["dynamic_leaf_chunk_max"] == 456
    assert args["max_assembly_tokens"] == 999
    assert args["summary_fan_in"] == 3
    assert args["context_length"] == 200000
    assert args["reserve_tokens_floor"] == 4096
    assert args["incremental_max_depth"] == 2

assert preflight_args["session_id"] == "session-1"
assert preflight_args["current_tokens"] == 800
assert preflight_args["messages"] == [{"role": "user", "content": "hello"}]
assert compress_args["summarizer"] == {"mode": "hermes_auxiliary"}
"#,
        "generated context engine should project configured Hermes defaults into preflight/compress args",
    );
}

#[test]
fn context_engine_project_routing_never_uses_hermes_home() {
    run_python_check(
        "check_project_flag_bridge.py",
        r#"
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])

parent_name = "_hermes_user_context"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

calls = []

class Result:
    def __init__(self, returncode, stdout, stderr):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

def mcp_response(inner):
    return json.dumps({"content": [{"type": "text", "text": json.dumps(inner)}]})

run_cwds = []

def fake_run(argv, check, capture_output, text, timeout, shell, cwd=None):
    calls.append(argv)
    run_cwds.append(cwd)
    tool_name = argv[4] if "--project" in argv else argv[2]
    if tool_name == "tracedecay_lcm_expand_query":
        inner = {
            "status": "ok",
            "prompt": "What changed?",
            "query": "orchard",
            "needs_synthesis": False,
            "answer": "orchard summary",
        }
    else:
        inner = {"status": "ok", "messages": []}
    return Result(0, mcp_response(inner), "")

plugin.tools.subprocess.run = fake_run

project_engine = plugin.TraceDecayContextEngine()
project_engine.initialize(session_id="session-1", project_root="/tmp/project")
answer = project_engine.expand_query(prompt="What changed?", query="orchard")
assert answer["status"] == "ok"
project_argv = calls.pop()
assert project_argv[0] == plugin.tools.TRACEDECAY_BIN
# LCM session state uses the unified user-level store for the project.
assert project_argv[1:6] == ["tool", "--project", "/tmp/project", "tracedecay_lcm_expand_query", "--json"]
project_args = json.loads(project_argv[project_argv.index("--args") + 1])
assert "project_root" not in project_args

profile_engine = plugin.TraceDecayContextEngine()
profile_engine.initialize(session_id="session-2", hermes_home="/tmp/hermes-profile")
profile_result = profile_engine._preflight_probe(messages=[], current_tokens=100)
assert profile_result["status"] == "ok"
assert profile_engine.should_compress_preflight([], current_tokens=100) is False
profile_argv = calls.pop()
profile_cwd = run_cwds.pop()
assert profile_argv[0] == plugin.tools.TRACEDECAY_BIN
assert profile_argv[1:3] == ["tool", "tracedecay_lcm_preflight"]
assert "--project" not in profile_argv
assert profile_cwd == os.path.abspath(os.sep)
profile_args = json.loads(profile_argv[profile_argv.index("--args") + 1])
assert "project_root" not in profile_args
assert profile_args["storage_scope"] == "user"

explicit = plugin.tools.call_tracedecay_tool(
    "tracedecay_lcm_status",
    {"storage_scope": "hermes_profile", "hermes_home": "/tmp/hermes-profile"},
    project_root="/tmp/project",
)
assert json.loads(explicit)["content"]
explicit_argv = calls.pop()
assert explicit_argv[1:6] == ["tool", "--project", "/tmp/project", "tracedecay_lcm_status", "--json"]
explicit_args = json.loads(explicit_argv[explicit_argv.index("--args") + 1])
assert explicit_args["storage_scope"] == "hermes_profile"
assert explicit_args["hermes_home"] == "/tmp/hermes-profile"
"#,
        "generated bridge must route only by real projects and isolate explicit user scope",
    );
}

#[test]
fn auxiliary_summary_strips_reasoning_tags() {
    run_python_check(
        "check_auxiliary_summary.py",
        r#"
import importlib.machinery
import importlib.util
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])

parent_name = "_hermes_user_context"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

assert plugin._strip_reasoning("<think>x</think>Useful") == "Useful"
assert plugin._strip_reasoning("<THINKING>x</THINKING>\nUseful") == "Useful"
assert plugin._strip_reasoning("<reasoning>x</reasoning>\nUseful") == "Useful"
assert plugin._strip_reasoning("<thought>x</thought>\nUseful") == "Useful"
assert plugin._strip_reasoning("<REASONING_SCRATCHPAD>x</REASONING_SCRATCHPAD>\nUseful") == "Useful"

class Aux:
    def __init__(self):
        self.calls = []

    def call_llm(self, **kwargs):
        self.calls.append(kwargs)
        return "<think>hidden chain</think>\nUseful compact summary"

agent = type("Agent", (), {"auxiliary_client": Aux()})()
engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project", agent=agent)
summary = engine._call_auxiliary_summary(
    "Summarize",
    [{"role": "user", "content": "raw"}],
)

assert summary["status"] == "ok"
assert summary["text"] == "Useful compact summary"
assert summary["route"] == "default"
assert summary["model"] is None
assert agent.auxiliary_client.calls[0]["task"] == "compression"
# Hermes _call_llm_for_summary sends the full prompt as one user message.
assert agent.auxiliary_client.calls[0]["messages"] == [
    {"role": "user", "content": "Summarize"},
]
assert agent.auxiliary_client.calls[0]["temperature"] == 0.3
"#,
        "generated context engine should strip reasoning from auxiliary summaries",
    );
}

#[test]
fn context_engine_expand_query_synthesizes_and_degrades() {
    run_python_check(
        "check_expand_query_synthesis.py",
        r#"
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
os.environ["HERMES_HOME"] = "/ignored/hermes-home"

parent_name = "_hermes_user_context"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

responses = []

def mcp_response(inner):
    return json.dumps({"content": [{"type": "text", "text": json.dumps(inner)}]})

def needs_synthesis():
    return {
        "status": "ok",
        "prompt": "What changed?",
        "query": "orchard",
        "needs_synthesis": True,
        "max_tokens": 32,
        "context_max_tokens": 256,
        "context_truncated": False,
        "context_pagination": [],
        "node_ids": ["sum_1"],
        "matches": [{"kind": "summary_node", "node_id": "sum_1", "snippet": "orchard summary"}],
        "context_blocks": [{"kind": "summary", "node_id": "sum_1", "content": "orchard summary"}],
        "synthesis_prompt": {
            "system": "Use expanded LCM context.",
            "user": "QUESTION:\nWhat changed?\n\nEXPANDED CONTEXT:\n[]",
        },
    }

def fake_call_tracedecay_tool(name, args, **kwargs):
    assert name == "tracedecay_lcm_expand_query"
    assert args["session_id"] == "session-1"
    assert "project_root" not in args
    assert kwargs == {"project_root": "/tmp/project"}
    assert args["prompt"] == "What changed?"
    assert args["query"] == "orchard"
    return mcp_response(responses.pop(0))

plugin.tools.call_tracedecay_tool = fake_call_tracedecay_tool

class Aux:
    def __init__(self):
        self.mode = "ok"
        self.calls = []

    def call_llm(self, **kwargs):
        self.calls.append(kwargs)
        if self.mode == "timeout":
            raise TimeoutError("slow route")
        if self.mode == "unexpected":
            raise RuntimeError("schema bug")
        if self.mode == "empty":
            return "<reasoning>hidden</reasoning>   "
        return "<reasoning>hidden</reasoning>Final answer from context"

agent = type("Agent", (), {"auxiliary_client": Aux()})()
engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project", agent=agent)

responses.append(needs_synthesis())
answer = engine.expand_query(prompt="What changed?", query="orchard")
assert answer["status"] == "ok"
assert answer["needs_synthesis"] is False
assert answer["answer"] == "Final answer from context"
assert "hidden" not in answer["answer"]
assert answer["node_ids"] == ["sum_1"]
assert agent.auxiliary_client.calls[0]["task"] == "compression"
assert agent.auxiliary_client.calls[0]["messages"][0] == {
    "role": "system",
    "content": "Use expanded LCM context.",
}
assert "EXPANDED CONTEXT" in agent.auxiliary_client.calls[0]["messages"][1]["content"]

agent.auxiliary_client.mode = "timeout"
responses.append(needs_synthesis())
timeout_payload = engine.expand_query(prompt="What changed?", query="orchard")
assert timeout_payload["degraded"] is True
assert "timed out" in timeout_payload["error"]
assert timeout_payload["timeout_seconds"] > 0
assert timeout_payload["needs_synthesis"] is False

agent.auxiliary_client.mode = "empty"
responses.append(needs_synthesis())
empty_payload = engine.expand_query(prompt="What changed?", query="orchard")
assert empty_payload["degraded"] is True
assert "empty answer" in empty_payload["error"]
assert empty_payload["needs_synthesis"] is False

# Non-timeout synthesis failures (RuntimeError / provider SDK / httpx)
# must degrade with the retrieval intact, never escape as a handler
# exception that loses the retrieval behind a generic registry error.
agent.auxiliary_client.mode = "unexpected"
responses.append(needs_synthesis())
failed_payload = engine.expand_query(prompt="What changed?", query="orchard")
assert failed_payload["degraded"] is True
assert "schema bug" in failed_payload["error"]
assert failed_payload["needs_synthesis"] is False
assert failed_payload["matches"], "retrieval must survive synthesis failures"
"#,
        "generated context engine should synthesize and degrade expand-query answers",
    );
}

#[test]
fn context_engine_compress_provides_auxiliary_summary_after_needs_summary() {
    run_python_check(
        "check_auxiliary_compress_flow.py",
        r#"
import importlib.machinery
import importlib.util
import json
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])

parent_name = "_hermes_user_context"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

calls = []
old_one = "old one " * 20
old_two = "old two " * 20

class Result:
    def __init__(self, returncode, stdout, stderr):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

def mcp_response(inner):
    return Result(0, json.dumps({"content": [{"type": "text", "text": json.dumps(inner)}]}), "")

def fake_run(argv, check, capture_output, text, timeout, shell):
    args = json.loads(argv[argv.index("--args") + 1])
    calls.append(args)
    if len(calls) == 1:
        assert args["summarizer"] == {"mode": "hermes_auxiliary"}
        return mcp_response({
            "status": "needs_summary",
            "reason": "hermes_auxiliary_not_available_in_task_9",
            "summary_nodes_created": 0,
            "summary_nodes": [],
            "replay_messages": [{"role": "user", "content": "fresh"}],
            "frontier": {"current_frontier_store_id": None},
            "summary_request": {
                "provider": "cursor",
                "session_id": "session-1",
                "focus_topic": "handoff",
                "prompt": "Summarize backlog",
                "source_range": {"from_store_id": 1, "to_store_id": 2},
                "source_messages": [
                    {"store_id": 1, "role": "user", "content": old_one},
                    {"store_id": 2, "role": "assistant", "content": old_two},
                ],
            },
        })
    assert args["summarizer"] == {
        "mode": "provided",
        "summary_text": "Useful compact summary",
        "route": "default",
    }
    return mcp_response({
        "status": "ok",
        "reason": "compressed_backlog",
        "summary_nodes_created": 1,
        "summary_nodes": [],
        "replay_messages": [{"role": "system", "content": "Useful compact summary"}],
        "frontier": {"current_frontier_store_id": 2},
        "summary_request": None,
    })

plugin.tools.subprocess.run = fake_run

class Aux:
    def __init__(self):
        self.calls = []

    def call_llm(self, **kwargs):
        self.calls.append(kwargs)
        return "<thinking>hidden chain</thinking>\nUseful compact summary"

agent = type("Agent", (), {"auxiliary_client": Aux()})()
engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project", agent=agent)
result = engine._compress_to_result(
    [{"role": "user", "content": old_one}],
    current_tokens=1200,
    focus_topic="handoff",
)

assert result["status"] == "ok"
assert result["reason"] == "compressed_backlog"
assert len(calls) == 2
assert agent.auxiliary_client.calls[0]["task"] == "compression"
# Hermes escalation L1 contract: a single user message carrying the depth-0
# prompt with focus brief, token budget, and serialized CONTENT block.
assert len(agent.auxiliary_client.calls[0]["messages"]) == 1
assert agent.auxiliary_client.calls[0]["messages"][0]["role"] == "user"
prompt = agent.auxiliary_client.calls[0]["messages"][0]["content"]
assert prompt.startswith("Summarize this conversation segment for future turns.")
assert "Preserve decisions, rationale, constraints, active tasks, file paths, commands, and specific values." in prompt
assert 'End with: "Expand for details about: <what was compressed>"' in prompt
assert "Focus brief:" in prompt
assert "Primary focus: handoff" in prompt
assert "Target ~2000 tokens." in prompt
assert "CONTENT:" in prompt
assert f"[USER]: {old_one}" in prompt
assert f"[ASSISTANT]: {old_two}" in prompt
assert agent.auxiliary_client.calls[0]["temperature"] == 0.3
assert agent.auxiliary_client.calls[0]["max_tokens"] == 4000
"#,
        "generated context engine should provide auxiliary summaries back to Rust",
    );
}

#[test]
fn context_engine_marks_no_usable_replay_as_aborted() {
    run_generated_plugin_script(
        "check_no_usable_replay_abort.py",
        r#"
import json

class Result:
    def __init__(self, returncode, stdout, stderr):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

def mcp_response(inner):
    return Result(0, json.dumps({"content": [{"type": "text", "text": json.dumps(inner)}]}), "")

def fake_run(argv, check, capture_output, text, timeout, shell):
    return mcp_response({
        "status": "ok",
        "reason": "noop_summarizer",
        "summary_nodes_created": 0,
        "summary_nodes": [],
        "replay_messages": [],
        "replay_token_estimate": 0,
        "replay_over_budget": False,
    })

plugin.tools.subprocess.run = fake_run

engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project")
messages = [{"role": "user", "content": "oversized live transcript"}]
compressed = engine.compress(messages, current_tokens=292666)

assert compressed == messages
assert engine._last_compress_aborted is True
assert engine._last_summary_error == "noop_summarizer"
assert engine.last_compress_result["reason"] == "noop_summarizer"
"#,
        "generated context engine should mark no usable replay as an aborted compression",
    );
}

#[test]
fn context_engine_rejects_orphan_heavy_replay_at_host_boundary() {
    run_generated_plugin_script(
        "check_orphan_heavy_replay_rejected.py",
        r#"
messages = [
    {"role": "user", "content": "original valid transcript " * 80},
    {"role": "assistant", "content": "still valid"},
]
replay = [
    {"role": "system", "content": "Useful compact summary"},
    {"role": "tool", "tool_call_id": "orphan-result", "content": "unmatched output"},
    {
        "role": "assistant",
        "content": "",
        "tool_calls": [{
            "id": "orphan-call",
            "type": "function",
            "function": {"name": "lookup", "arguments": "{}"},
        }],
    },
]

engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project")
engine.last_real_prompt_tokens = 100
engine._compress_to_result = lambda *args, **kwargs: {
    "status": "ok",
    "reason": "compressed_backlog",
    "replay_messages": replay,
    "replay_token_estimate": 40,
}

compressed = engine.compress(list(messages), current_tokens=107)

assert compressed == messages
assert engine._last_compress_aborted is True
assert engine._last_summary_error == "invalid_replay_tool_pairs"
assert engine.compression_count == 0
assert engine.last_compress_result["status"] == "aborted"
assert engine.last_compress_result["replay_messages"] == []
diagnostic = engine.last_compress_result["compression_diagnostic"]
assert diagnostic["type"] == "replay_boundary_rejection"
assert diagnostic["code"] == "invalid_replay_tool_pairs"
assert {issue["tool_call_id"] for issue in diagnostic["issues"]} == {"orphan-result"}
assert engine.should_defer_preflight_to_real_usage(rough_tokens=107) is True
"#,
        "generated context engine should reject orphan-heavy replay before host adoption",
    );
}

#[test]
fn context_engine_rejects_reported_107_to_201_token_expansion() {
    run_generated_plugin_script(
        "check_expanding_replay_rejected.py",
        r#"
messages = [{"role": "user", "content": "small live input"}]
replay = [{"role": "system", "content": "expanded replay " * 100}]

engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project")
engine.last_real_prompt_tokens = 100
engine._compress_to_result = lambda *args, **kwargs: {
    "status": "ok",
    "reason": "compressed_backlog",
    "replay_messages": replay,
    "replay_token_estimate": 201,
}

compressed = engine.compress(list(messages), current_tokens=107)

assert compressed == messages
assert engine._last_compress_aborted is True
assert engine.compression_count == 0
diagnostic = engine.last_compress_result["compression_diagnostic"]
assert diagnostic["type"] == "replay_boundary_rejection"
assert diagnostic["code"] in ("non_shrinking_replay", "non_shrinking_reported_replay")
assert diagnostic["reported_source_tokens"] == 107
assert diagnostic["reported_replay_tokens"] == 201
assert engine.last_compress_result["replay_messages"] == []
assert engine.should_defer_preflight_to_real_usage(rough_tokens=107) is True
"#,
        "generated context engine should reject the observed 107-to-201 token expansion",
    );
}

#[test]
fn context_engine_prefers_reported_shrink_over_host_estimator_tie() {
    run_generated_plugin_script(
        "check_reported_shrink_wins.py",
        r#"
messages = [
    {"role": "user", "content": "first"},
    {"role": "assistant", "content": "second"},
]
replay = [
    {"role": "system", "content": "short"},
    {"role": "user", "content": "tail"},
]

engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project")
engine._compress_to_result = lambda *args, **kwargs: {
    "status": "ok",
    "reason": "compressed_backlog",
    "replay_messages": replay,
    "replay_token_estimate": 3,
}

compressed = engine.compress(list(messages), current_tokens=50)

assert compressed == replay
assert engine._last_compress_aborted is False
assert engine.compression_count == 1
"#,
        "backend shrink estimates should override a tied secondary host estimate",
    );
}

#[test]
fn context_engine_preserves_valid_tool_pairs_and_summary() {
    run_generated_plugin_script(
        "check_valid_tool_pair_replay.py",
        r#"
messages = [{"role": "user", "content": "old backlog " * 300}]
replay = [
    {"role": "system", "content": "Useful compact summary"},
    {
        "role": "assistant",
        "content": "",
        "tool_calls": [{
            "id": "call-1",
            "type": "function",
            "function": {"name": "lookup", "arguments": "{}"},
        }],
    },
    {"role": "tool", "tool_call_id": "call-1", "content": "bounded result"},
]

engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project")
engine._compress_to_result = lambda *args, **kwargs: {
    "status": "ok",
    "reason": "compressed_backlog",
    "replay_messages": replay,
    "replay_token_estimate": 30,
}

compressed = engine.compress(list(messages), current_tokens=500)

assert compressed == replay
assert compressed[0]["content"] == "Useful compact summary"
assert compressed[1]["tool_calls"][0]["id"] == "call-1"
assert compressed[2]["tool_call_id"] == "call-1"
assert engine._last_compress_aborted is False
assert engine.compression_count == 1
"#,
        "generated context engine should preserve valid summaries and paired tool events",
    );
}

#[test]
fn context_engine_preserves_trailing_open_tool_call() {
    run_generated_plugin_script(
        "check_trailing_open_tool_call.py",
        r#"
messages = [{"role": "user", "content": "old backlog " * 200}]
replay = [
    {"role": "system", "content": "Useful compact summary"},
    {
        "role": "assistant",
        "content": "",
        "tool_calls": [{
            "id": "call-open",
            "type": "function",
            "function": {"name": "lookup", "arguments": "{}"},
        }],
    },
]

engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project")
engine._compress_to_result = lambda *args, **kwargs: {
    "status": "ok",
    "reason": "compressed_backlog",
    "replay_messages": replay,
    "replay_token_estimate": 20,
}

compressed = engine.compress(list(messages), current_tokens=500)

assert compressed == replay
assert compressed[-1]["tool_calls"][0]["id"] == "call-open"
assert engine._last_compress_aborted is False
assert engine.compression_count == 1
"#,
        "generated context engine should preserve a trailing call awaiting its tool result",
    );
}

#[test]
fn context_engine_treats_identical_replay_as_clean_noop() {
    run_generated_plugin_script(
        "check_identical_replay_noop.py",
        r#"
import json

messages = [{"role": "user", "content": "same replay should not count as progress"}]

class Result:
    def __init__(self, returncode, stdout, stderr):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

def mcp_response(inner):
    return Result(0, json.dumps({"content": [{"type": "text", "text": json.dumps(inner)}]}), "")

def fake_run(argv, check, capture_output, text, timeout, shell):
    return mcp_response({
        "status": "ok",
        "reason": "no_backlog_to_compress",
        "summary_nodes_created": 0,
        "summary_nodes": [],
        "replay_messages": messages,
        "replay_token_estimate": 10,
        "replay_over_budget": False,
    })

plugin.tools.subprocess.run = fake_run

engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project")
compressed = engine.compress(list(messages), current_tokens=292666)

assert compressed == messages
assert engine._last_compress_aborted is False
assert engine._last_summary_error is None
assert engine.compression_count == 0
"#,
        "generated context engine should treat identical valid replay as a clean no-op",
    );
}

#[test]
fn context_engine_adopts_decoded_replay_even_when_should_compress_false() {
    run_generated_plugin_script(
        "check_decoded_replay_compression.py",
        r#"
import json

messages = [
    {"role": "user", "content": "old oversized context"},
    {"role": "assistant", "content": "old acknowledgement"},
]
replay = [{"role": "system", "content": "compressed replay"}]
calls = []

def envelope(payload):
    return json.dumps({"content": [{"type": "text", "text": json.dumps(payload)}]})

def fake_call_tracedecay_tool(name, args, **kwargs):
    calls.append((name, dict(args), dict(kwargs)))
    if name == "tracedecay_lcm_preflight":
        return envelope({
            "status": "ok",
            "should_compress": False,
            "reason": "replay_only",
            "replay_messages": replay,
        })
    if name == "tracedecay_lcm_compress":
        return envelope({"truncated": True, "handle": "compress-payload"})
    if name == "tracedecay_retrieve":
        assert args == {"handle": "compress-payload"}
        assert kwargs.get("project_root") == "/tmp/project", kwargs
        return envelope({
            "content": json.dumps({
                "status": "ok",
                "reason": "compressed_backlog",
                "replay_messages": replay,
            })
        })
    raise AssertionError(f"unexpected tool call: {name}")

plugin.tools.call_tracedecay_tool = fake_call_tracedecay_tool

engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project")

assert engine.should_compress_preflight(list(messages), current_tokens=292666) is True
compressed = engine.compress(list(messages), current_tokens=292666)

assert compressed == replay
assert engine._last_compress_aborted is False
assert engine.compression_count == 1
assert engine.last_compress_result["status"] == "ok"
assert engine.last_compress_result["reason"] == "compressed_backlog"
assert [call[0] for call in calls] == [
    "tracedecay_lcm_preflight",
    "tracedecay_lcm_compress",
    "tracedecay_retrieve",
]
"#,
        "generated context engine should adopt decoded replay payloads as compression progress",
    );
}

#[test]
fn projectless_context_engine_compression_uses_user_session_store() {
    run_generated_plugin_script(
        "check_projectless_compression_user_scope.py",
        r#"
import json

calls = []

def fake_call_tracedecay_tool(name, args, **kwargs):
    calls.append((name, dict(args), dict(kwargs)))
    return json.dumps({"content": [{"type": "text", "text": json.dumps({
        "status": "ok",
        "reason": "compressed_backlog",
        "replay_messages": [{"role": "user", "content": "compressed"}],
    })}]})

plugin.tools.call_tracedecay_tool = fake_call_tracedecay_tool

engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="general-chat")
compressed = engine.compress(
    [{"role": "user", "content": "oversized general chat"}],
    current_tokens=350000,
)

assert compressed == [{"role": "user", "content": "compressed"}]
name, args, kwargs = calls.pop()
assert name == "tracedecay_lcm_compress"
assert args["storage_scope"] == "user"
assert "project_root" not in kwargs
"#,
        "projectless Hermes compression must use the profile-level user session store",
    );
}

#[test]
fn project_resolution_uses_registry_and_rejects_hermes_descendants() {
    run_generated_plugin_script(
        "check_registry_project_resolution.py",
        r#"
import json
import os
import pathlib
import tempfile

calls = []
temp = tempfile.TemporaryDirectory()
base = pathlib.Path(temp.name)
canonical = base / "repo"
alias = base / "repo-feature"
hermes_home = base / "hermes"
for path in (canonical, alias, hermes_home / "hermes-agent"):
    path.mkdir(parents=True)

def fake_call_tracedecay_tool(name, args, **kwargs):
    calls.append((name, dict(args), dict(kwargs)))
    assert name == "tracedecay_project_context"
    assert args == {"project_path": str(alias)}
    return json.dumps({"content": [{"type": "text", "text": json.dumps({
        "project": {"project_root": str(canonical)},
        "aliases": [{"alias_path": str(alias)}],
    })}]})

plugin.tools.call_tracedecay_tool = fake_call_tracedecay_tool

state, root = plugin._project_scope_resolution(str(alias), str(hermes_home))
assert state == "registered"
assert root == str(canonical)
assert [call[0] for call in calls] == ["tracedecay_project_context"]

for path in (hermes_home, hermes_home / "hermes-agent"):
    assert plugin._code_project_root(explicit=str(path), hermes_home=str(hermes_home)) is None
    assert plugin.tools.code_project_root(explicit=str(path), hermes_home=str(hermes_home)) is None
"#,
        "Hermes project resolution must use the registry and reject host-owned descendants",
    );
}

#[test]
fn context_engine_rejects_compacted_compression_replay() {
    run_generated_plugin_script(
        "check_compacted_replay_abort.py",
        r#"
import json

messages = [
    {"role": "user", "content": "old oversized context"},
    {"role": "assistant", "content": "old acknowledgement"},
]
compacted_replay = [{"role": "system", "content": "truncated replay"}]

def envelope(payload):
    return json.dumps({"content": [{"type": "text", "text": json.dumps(payload)}]})

def fake_call_tracedecay_tool(name, args, **kwargs):
    assert name == "tracedecay_lcm_compress"
    return envelope({
        "status": "ok",
        "reason": "compressed_backlog",
        "contract_truncated": True,
        "mcp_truncation_reason": "lcm-compress response compacted to preserve Hermes bridge contract",
        "replay_messages": compacted_replay,
        "replay_messages_truncated_for_mcp": True,
    })

plugin.tools.call_tracedecay_tool = fake_call_tracedecay_tool

engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project")
compressed = engine.compress(list(messages), current_tokens=292666)

assert compressed == messages
assert engine._last_compress_aborted is True
assert "compacted" in engine._last_summary_error
assert engine.compression_count == 0
"#,
        "generated context engine should never adopt compacted MCP replay as live transcript",
    );
}

#[test]
fn memory_provider_sync_turn_assigns_unique_fallback_message_ids() {
    run_generated_plugin_script(
        "check_sync_turn_unique_ids.py",
        r#"
calls = []

def fake_call_tracedecay_tool(name, args, **kwargs):
    calls.append((name, args, kwargs))
    return '{"content":[{"type":"text","text":"{\\"status\\":\\"ok\\"}"}]}'

plugin.tools.call_tracedecay_tool = fake_call_tracedecay_tool
plugin._project_scope_available = lambda root, *_args: True

provider = plugin.TracedecayMemoryProvider()
provider.initialize(session_id="session-1", hermes_home="/tmp/hermes", project_root="/tmp/project")
provider.sync_turn("repeat", "same", session_id="session-1")
provider.sync_turn("repeat", "same", session_id="session-1")
provider.sync_turn("repeat", "same", session_id="session-1", messages=[])
provider.sync_turn("repeat", "same", session_id="session-1", messages=[])

assert provider.project_root == "/tmp/project"
assert len(calls) == 8
for index in range(0, len(calls), 2):
    user_call, project_call = calls[index:index + 2]
    assert user_call[0] == "tracedecay_lcm_preflight"
    assert user_call[1]["storage_scope"] == "user"
    assert user_call[2] == {}
    assert project_call[0] == "tracedecay_lcm_preflight"
    assert "storage_scope" not in project_call[1]
    assert project_call[2]["project_root"] == "/tmp/project"
    assert user_call[1]["messages"] == project_call[1]["messages"]

first_messages = calls[0][1]["messages"]
second_messages = calls[2][1]["messages"]
empty_list_first_messages = calls[4][1]["messages"]
empty_list_second_messages = calls[6][1]["messages"]
assert [message["role"] for message in first_messages] == ["user", "assistant"]
assert all(message.get("id") for message in first_messages)
assert all(message.get("id") for message in second_messages)
assert first_messages[0]["id"] != second_messages[0]["id"]
assert first_messages[1]["id"] != second_messages[1]["id"]
assert [message["role"] for message in empty_list_first_messages] == ["user", "assistant"]
assert all(message.get("id") for message in empty_list_first_messages)
assert all(message.get("id") for message in empty_list_second_messages)
assert empty_list_first_messages[0]["id"] != empty_list_second_messages[0]["id"]
assert empty_list_first_messages[1]["id"] != empty_list_second_messages[1]["id"]

fallback = plugin.TracedecayMemoryProvider()
fallback.initialize(session_id="session-2", hermes_home="/tmp/hermes")
fallback.sync_turn("user", "assistant", session_id="session-2")
assert fallback.project_root is None
assert "project_root" not in calls[-1][1]
assert calls[-1][1]["storage_scope"] == "user"
assert "project_root" not in calls[-1][2]
"#,
        "sync_turn fallback messages should not collapse repeated identical turns",
    );
}

#[test]
fn memory_provider_projects_one_turn_to_user_and_every_touched_project() {
    run_generated_plugin_script(
        "check_sync_turn_multi_project_projection.py",
        r#"
calls = []
completed = []
ingested = []

def fake_call_tracedecay_tool(name, args, **kwargs):
    calls.append((name, dict(args), dict(kwargs)))
    return '{"content":[{"type":"text","text":"{\\"status\\":\\"ok\\"}"}]}'

plugin.tools.call_tracedecay_tool = fake_call_tracedecay_tool
def fake_project_scope_resolution(path, *_args):
    normalized = str(path).replace("\\", "/")
    marker = "/repos/"
    marker_index = normalized.find(marker)
    if marker_index < 0:
        return ("unregistered", None)
    return ("registered", normalized[marker_index:])

plugin._project_scope_resolution = fake_project_scope_resolution
plugin._notify_turn_completed = lambda sid, root, watermark: completed.append((sid, root, watermark))
plugin._notify_turn_ingested = lambda sid, root, watermark: ingested.append((sid, root, watermark))

messages = [
    {"role": "user", "content": "compare both repositories"},
    {
        "role": "assistant",
        "tool_calls": [
            {"function": {"name": "terminal", "arguments": '{"workdir":"/repos/alpha"}'}},
            {"function": {"name": "terminal", "arguments": '{"workdir":"/repos/beta"}'}},
        ],
    },
]

provider = plugin.TracedecayMemoryProvider()
provider.initialize(session_id="telegram-dm")
provider.sync_turn(
    "compare both repositories",
    "done",
    session_id="telegram-dm",
    messages=messages,
)

assert len(calls) == 3, calls
user_call, alpha_call, beta_call = calls
assert user_call[1]["storage_scope"] == "user"
assert user_call[2] == {}
assert alpha_call[2] == {"project_root": "/repos/alpha"}
assert beta_call[2] == {"project_root": "/repos/beta"}
assert "storage_scope" not in alpha_call[1]
assert "storage_scope" not in beta_call[1]

message_sets = [call[1]["messages"] for call in calls]
assert message_sets[0] == message_sets[1] == message_sets[2]
assert message_sets[0][0]["associated_project_roots"] == [
    "/repos/alpha",
    "/repos/beta",
]
assert [root for _, root, _ in completed] == [None, "/repos/alpha", "/repos/beta"]
assert ingested == completed
assert provider.project_root is None
"#,
        "one Hermes turn must keep a user canonical copy and project into every touched repository",
    );
}

#[test]
fn context_engine_records_all_forwarded_native_lcm_tools() {
    run_generated_plugin_script(
        "check_context_tool_names_bookkeeping.py",
        r#"
class Ctx:
    context_engine_tool_handlers_receive_messages = True

    def __init__(self):
        self.registered = []

    def register_hook(self, *args, **kwargs):
        pass

    def register_context_engine(self, engine):
        self.engine = engine

    def register_tool(self, **kwargs):
        self.registered.append(kwargs["name"])

    def register_memory_provider(self, provider):
        self.memory_provider = provider

    def register_command(self, *args, **kwargs):
        pass

ctx = Ctx()
plugin.register(ctx)

expected = sorted(schema["name"] for schema in plugin.LCM_NATIVE_SCHEMAS)
assert sorted(plugin._CONTEXT_TOOL_NAMES) == expected
assert set(expected).issubset(set(ctx.registered))
"#,
        "generated plugin should record every forwarded native LCM context tool",
    );
}

#[test]
fn native_describe_and_expand_schemas_are_closed_and_conditional() {
    run_generated_plugin_script(
        "check_closed_lcm_target_schemas.py",
        r#"
schemas = {schema["name"]: schema["parameters"] for schema in plugin.LCM_NATIVE_SCHEMAS}

describe = schemas["lcm_describe"]
assert describe["additionalProperties"] is False
assert len(describe["oneOf"]) == 3
assert describe["properties"]["node_id"]["type"] == "string"
assert describe["properties"]["externalized_ref"]["type"] == "string"

expand = schemas["lcm_expand"]
assert expand["additionalProperties"] is False
assert len(expand["oneOf"]) == 3
assert expand["properties"]["node_id"]["type"] == "string"
assert expand["properties"]["source_limit"]["minimum"] == 1
assert expand["properties"]["source_limit"]["maximum"] == 100
assert expand["properties"]["source_limit"]["default"] == 50
assert expand["properties"]["cursor"]["type"] == "string"

target, error = plugin._native_expand_target({"store_id": 7})
assert error is None
assert target == {"kind": "raw_message", "store_id": 7}
target, error = plugin._native_expand_target({"store_id": 7, "node_id": 8})
assert target is None
assert error == "lcm_expand expects exactly one of node_id, store_id, or externalized_ref"

summary = plugin._translate_lcm_args(
    "lcm_expand",
    {
        "node_id": "summary-v1:abc",
        "source_offset": 4,
        "source_limit": 7,
        "content_offset": 3,
        "cursor": "opaque-summary-page",
    },
)
assert summary["target"] == {"kind": "summary_node", "node_id": "summary-v1:abc"}
assert summary["source_offset"] == 4
assert summary["source_limit"] == 7
assert summary["content_offset"] == 3
assert summary["cursor"] == "opaque-summary-page"

raw = plugin._translate_lcm_args(
    "lcm_expand",
    {
        "store_id": 7,
        "source_offset": 4,
        "source_limit": 7,
        "content_offset": 3,
        "cursor": "wrong-target-page",
    },
)
assert raw["target"] == {"kind": "raw_message", "store_id": 7}
assert raw["content_offset"] == 3
assert "source_offset" not in raw
assert "source_limit" not in raw
assert "cursor" not in raw
"#,
        "generated Hermes schemas should close each compatibility target branch",
    );
}

#[test]
fn context_engine_lcm_expand_query_tolerates_forwarded_agent_kwarg() {
    run_generated_plugin_script(
        "check_expand_query_forwarded_agent.py",
        r#"
import json

def fake_call_tracedecay_json(name, args, **kwargs):
    assert name == "tracedecay_lcm_expand_query"
    return {
        "status": "ok",
        "prompt": args["prompt"],
        "needs_synthesis": False,
        "answer": "retrieval-only answer",
    }

plugin.call_tracedecay_json = fake_call_tracedecay_json

class Aux:
    def call_llm(self, **kwargs):
        raise AssertionError("synthesis should not run")

forwarded_agent = type("ForwardedAgent", (), {"auxiliary_client": Aux()})()
engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project", agent=forwarded_agent)

raw = engine.handle_tool_call(
    "lcm_expand_query",
    {"prompt": "What changed?"},
    agent=forwarded_agent,
)
payload = json.loads(raw)
assert payload["status"] == "ok"
assert payload["answer"] == "retrieval-only answer"
"#,
        "generated context engine should not pass duplicate agent kwargs through lcm_expand_query",
    );
}

#[test]
fn context_engine_compress_rejects_oversized_auxiliary_summary() {
    run_python_check(
        "check_auxiliary_oversized_fallback.py",
        r#"
import importlib.machinery
import importlib.util
import json
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])

parent_name = "_hermes_user_context"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

calls = []
source_content = "source text " * 600
huge_summary = "non-compressing summary " * 600

class Result:
    def __init__(self, returncode, stdout, stderr):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

def mcp_response(inner):
    return Result(0, json.dumps({"content": [{"type": "text", "text": json.dumps(inner)}]}), "")

def fake_run(argv, check, capture_output, text, timeout, shell):
    args = json.loads(argv[argv.index("--args") + 1])
    calls.append(args)
    if len(calls) == 1:
        return mcp_response({
            "status": "needs_summary",
            "reason": "summary_required",
            "summary_nodes_created": 0,
            "summary_nodes": [],
            "replay_messages": [{"role": "user", "content": "fresh"}],
            "frontier": {"current_frontier_store_id": None},
            "summary_request": {
                "provider": "cursor",
                "session_id": "session-1",
                "focus_topic": "handoff",
                "prompt": "Summarize backlog",
                "source_range": {"from_store_id": 1, "to_store_id": 1},
                "source_messages": [
                    {"store_id": 1, "role": "user", "content": source_content},
                ],
            },
        })
    summary = args["summarizer"]
    assert summary["mode"] == "provided"
    assert summary["summary_text"] != huge_summary
    assert len(summary["summary_text"]) < len(source_content)
    assert summary["summary_text"].endswith("[deterministic compression fallback]")
    assert summary["route"] == "deterministic_fallback"
    return mcp_response({
        "status": "ok",
        "reason": "compressed_backlog",
        "summary_nodes_created": 1,
        "summary_nodes": [],
        "replay_messages": [{"role": "system", "content": summary["summary_text"]}],
        "frontier": {"current_frontier_store_id": 1},
        "summary_request": None,
    })

plugin.tools.subprocess.run = fake_run

class OversizedAux:
    def __init__(self):
        self.calls = []

    def call_llm(self, **kwargs):
        self.calls.append(kwargs)
        return huge_summary

agent = type("Agent", (), {"auxiliary_client": OversizedAux()})()
engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project", agent=agent)
result = engine._compress_to_result(
    [{"role": "user", "content": source_content}],
    current_tokens=1200,
    focus_topic="handoff",
)

assert result["status"] == "ok"
assert result["fallback_used"] is True
assert len(calls) == 2
# Hermes escalation: oversized L1 retries once with the aggressive L2
# bullet prompt at half budget before deterministic (L3) fallback.
assert len(agent.auxiliary_client.calls) == 2
l1_prompt = agent.auxiliary_client.calls[0]["messages"][0]["content"]
l2_prompt = agent.auxiliary_client.calls[1]["messages"][0]["content"]
assert l1_prompt.startswith("Summarize this conversation segment for future turns.")
assert l2_prompt.startswith("Compress this into bullet points. Maximum 1000 tokens.")
assert "Keep only: decisions made, files changed, errors hit, current state." in l2_prompt
assert agent.auxiliary_client.calls[0]["max_tokens"] == 4000
assert agent.auxiliary_client.calls[1]["max_tokens"] == 2000
"#,
        "generated context engine should reject oversized auxiliary summaries",
    );
}

#[test]
fn retry_worthy_auxiliary_failures_fall_through_escalation_rungs() {
    run_generated_plugin_script(
        "check_auxiliary_retry_shrinks_chunk.py",
        r#"
import importlib.machinery
import importlib.util
import json
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_context"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

calls = []
source_messages = [
    {"store_id": 1, "role": "user", "content": "old one " * 20},
    {"store_id": 2, "role": "assistant", "content": "old two " * 20},
]

class Result:
    def __init__(self, returncode, stdout, stderr):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

def mcp_response(inner):
    return Result(0, json.dumps({"content": [{"type": "text", "text": json.dumps(inner)}]}), "")

def needs_summary(messages):
    return {
        "status": "needs_summary",
        "reason": "summary_required",
        "summary_nodes_created": 0,
        "summary_nodes": [],
        "replay_messages": [{"role": "user", "content": "fresh"}],
        "frontier": {"current_frontier_store_id": None, "maintenance_debt": []},
        "summary_request": {
            "provider": "cursor",
            "session_id": "session-1",
            "focus_topic": "handoff",
            "prompt": "Summarize backlog",
            "source_range": {
                "from_store_id": messages[0]["store_id"],
                "to_store_id": messages[-1]["store_id"],
            },
            "source_messages": messages,
        },
    }

def fake_run(argv, check, capture_output, text, timeout, shell):
    args = json.loads(argv[argv.index("--args") + 1])
    calls.append(args)
    if len(calls) == 1:
        assert args["summarizer"] == {"mode": "hermes_auxiliary"}
        assert "max_source_messages" not in args
        return mcp_response(needs_summary(source_messages))
    assert args["summarizer"]["mode"] == "provided"
    assert args["summarizer"]["route"] == "deterministic_fallback"
    assert args["summarizer"]["summary_text"].endswith("[deterministic compression fallback]")
    return mcp_response({
        "status": "ok",
        "reason": "compressed_backlog",
        "summary_nodes_created": 1,
        "summary_nodes": [],
        "replay_messages": [{"role": "system", "content": args["summarizer"]["summary_text"]}],
        "frontier": {"current_frontier_store_id": 1, "maintenance_debt": [
            {"kind": "raw_backlog", "from_store_id": 2, "to_store_id": 2}
        ]},
        "summary_request": None,
        "replay_token_estimate": 3,
        "replay_over_budget": False,
    })

plugin.tools.subprocess.run = fake_run

class ContextLimitedAux:
    def __init__(self):
        self.calls = []

    def call_llm(self, **kwargs):
        self.calls.append(kwargs)
        if "old two" in kwargs["messages"][0]["content"]:
            raise RuntimeError("context length exceeded")
        return "Smaller chunk summary"

agent = type("Agent", (), {"auxiliary_client": ContextLimitedAux()})()
engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project", agent=agent)
result = engine._compress_to_result(
    [{"role": "user", "content": "active"}],
    current_tokens=1200,
    focus_topic="handoff",
)

assert result["status"] == "ok"
assert len(calls) == 2
assert len(agent.auxiliary_client.calls) == 2
assert "old two" in agent.auxiliary_client.calls[0]["messages"][0]["content"]
assert "old two" in agent.auxiliary_client.calls[1]["messages"][0]["content"]
assert result["auxiliary_attempts"] == 1
assert result["auxiliary_retry_status"] == "fallback_summary"
assert result["auxiliary_error_classification"] == "retry_worthy"
"#,
        "retry-worthy auxiliary failures should fall through L1/L2 before deterministic fallback",
    );
}

#[test]
fn generated_auxiliary_retry_classifier_matches_hermes_context_markers() {
    run_generated_plugin_script(
        "check_auxiliary_retry_classifier.py",
        r#"
import importlib.machinery
import importlib.util
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_context_retry_classifier"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

retry_worthy_markers = (
    "context length exceeded",
    "maximum context",
    "max context",
    "too many tokens",
    "token limit",
    "prompt is too long",
    "input too long",
    "request too large",
    "timed out",
    "timeout",
)
for marker in retry_worthy_markers:
    assert plugin._auxiliary_error_classification(RuntimeError(marker)) == "retry_worthy", marker

permanent_markers = (
    "rate limit",
    "temporarily unavailable",
    "service unavailable",
    "overloaded",
    "try again",
    "route unavailable",
)
for marker in permanent_markers:
    assert plugin._auxiliary_error_classification(RuntimeError(marker)) == "permanent", marker
"#,
        "generated auxiliary retry classifier should match Hermes LCM context markers",
    );
}

#[test]
fn permanent_auxiliary_failure_falls_back_deterministically() {
    run_generated_plugin_script(
        "check_auxiliary_permanent_failure.py",
        r#"
import importlib.machinery
import importlib.util
import json
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_context"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

calls = []

class Result:
    def __init__(self, returncode, stdout, stderr):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

def mcp_response(inner):
    return Result(0, json.dumps({"content": [{"type": "text", "text": json.dumps(inner)}]}), "")

def fake_run(argv, check, capture_output, text, timeout, shell):
    args = json.loads(argv[argv.index("--args") + 1])
    calls.append(args)
    if len(calls) == 1:
        assert args["summarizer"] == {"mode": "hermes_auxiliary"}
        return mcp_response({
            "status": "needs_summary",
            "reason": "summary_required",
            "summary_nodes_created": 0,
            "summary_nodes": [],
            "replay_messages": [{"role": "user", "content": "fresh"}],
            "frontier": {"current_frontier_store_id": None, "maintenance_debt": [
                {"kind": "raw_backlog", "from_store_id": 1, "to_store_id": 2}
            ]},
            "summary_request": {
                "provider": "cursor",
                "session_id": "session-1",
                "focus_topic": "handoff",
                "prompt": "Summarize backlog",
                "source_range": {"from_store_id": 1, "to_store_id": 2},
                "source_messages": [
                    {"store_id": 1, "role": "user", "content": "old one"},
                    {"store_id": 2, "role": "assistant", "content": "old two"},
                ],
            },
        })

    assert args["summarizer"]["mode"] == "provided"
    assert args["summarizer"]["route"] == "deterministic_fallback"
    assert args["summarizer"]["summary_text"].endswith("[deterministic compression fallback]")
    return mcp_response({
        "status": "ok",
        "reason": "compressed_backlog",
        "summary_nodes_created": 1,
        "summary_nodes": [],
        "replay_messages": [{"role": "system", "content": args["summarizer"]["summary_text"]}],
        "frontier": {"current_frontier_store_id": 1, "maintenance_debt": []},
        "summary_request": None,
        "replay_token_estimate": 3,
        "replay_over_budget": False,
    })

plugin.tools.subprocess.run = fake_run

class BadTemplateAux:
    def __init__(self):
        self.calls = []

    def call_llm(self, **kwargs):
        self.calls.append(kwargs)
        raise RuntimeError("template exploded")

agent = type("Agent", (), {"auxiliary_client": BadTemplateAux()})()
engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project", agent=agent)
result = engine._compress_to_result(
    [{"role": "user", "content": "active"}],
    current_tokens=1200,
    focus_topic="handoff",
)

assert result["status"] == "ok"
assert result["reason"] == "compressed_backlog"
assert result["auxiliary_attempts"] == 1
assert result["auxiliary_retry_status"] == "fallback_summary"
assert result["auxiliary_error_classification"] == "permanent"
assert result["frontier"]["current_frontier_store_id"] == 1
assert len(calls) == 2
assert len(agent.auxiliary_client.calls) == 2
"#,
        "permanent auxiliary failures should still fall through to deterministic fallback",
    );
}

#[test]
fn auxiliary_summary_falls_back_and_tracks_route_cooldown() {
    run_python_check(
        "check_auxiliary_fallbacks.py",
        r#"
import importlib.machinery
import importlib.util
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])

parent_name = "_hermes_user_context"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

class RoutingAux:
    def __init__(self):
        self.calls = []

    def call_llm(self, **kwargs):
        self.calls.append(kwargs)
        if kwargs.get("model") == "primary":
            raise RuntimeError("primary unavailable")
        return "<reasoning>scratch</reasoning>Fallback route summary"

agent = type("Agent", (), {"auxiliary_client": RoutingAux()})()
engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project", agent=agent)
summary = engine._call_auxiliary_summary(
    "Summarize",
    [{"role": "user", "content": "raw"}],
    routes=[
        {"model": "primary", "temperature": 0.2},
        {"model": "backup", "temperature": 0.3},
    ],
)
assert summary["status"] == "ok"
assert summary["text"] == "Fallback route summary"
assert summary["route"] == "backup"
assert summary["model"] == "backup"
assert engine._route_failures["primary"] == 1
assert "primary" not in engine._cooldown_until
assert [call.get("model") for call in agent.auxiliary_client.calls] == ["primary", "backup"]
summary_second = engine._call_auxiliary_summary(
    "Summarize again",
    [{"role": "user", "content": "raw"}],
    routes=[
        {"model": "primary", "temperature": 0.2},
        {"model": "backup", "temperature": 0.3},
    ],
)
assert summary_second["status"] == "ok"
assert engine._route_failures["primary"] == 2
assert engine._cooldown_until["primary"] > 0

class FailingAux:
    def __init__(self):
        self.calls = []

    def call_llm(self, **kwargs):
        self.calls.append(kwargs)
        raise RuntimeError("route unavailable")

failing_agent = type("Agent", (), {"auxiliary_client": FailingAux()})()
failing_engine = plugin.TraceDecayContextEngine()
failing_engine.initialize(session_id="session-1", project_root="/tmp/project", agent=failing_agent)
fallback = failing_engine._call_auxiliary_summary(
    "Summarize",
    [{"role": "user", "content": "A" * 10000}],
)
assert fallback["status"] == "fallback"
assert len(fallback["text"]) < 10000
assert fallback["text"].endswith("[deterministic compression fallback]")
assert failing_engine._route_failures["default"] == 1
assert "default" not in failing_engine._cooldown_until

import os
os.environ["LCM_SUMMARY_CIRCUIT_BREAKER_FAILURE_THRESHOLD"] = "1"
os.environ["LCM_SUMMARY_CIRCUIT_BREAKER_COOLDOWN_SECONDS"] = "30"
tuned_engine = plugin.TraceDecayContextEngine()
tuned_engine.initialize(session_id="session-1", project_root="/tmp/project", agent=failing_agent)
tuned_engine._call_auxiliary_summary(
    "Summarize",
    [{"role": "user", "content": "B" * 1000}],
)
assert tuned_engine._route_failures["default"] == 1
assert tuned_engine._cooldown_until["default"] > 0
del os.environ["LCM_SUMMARY_CIRCUIT_BREAKER_FAILURE_THRESHOLD"]
del os.environ["LCM_SUMMARY_CIRCUIT_BREAKER_COOLDOWN_SECONDS"]
"#,
        "generated context engine should route, cooldown, and fallback auxiliary summaries",
    );
}

#[test]
fn auxiliary_model_route_splits_resolvable_providers() {
    run_generated_plugin_script(
        "check_auxiliary_model_routing.py",
        r#"
import importlib.machinery
import importlib.util
import pathlib
import sys
import types

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_model_routing"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

# Fake Hermes host provider registries used by the model-route resolver.
hermes_cli = types.ModuleType("hermes_cli")
hermes_cli.__path__ = []
auth = types.ModuleType("hermes_cli.auth")
auth.PROVIDER_REGISTRY = {"cerebras": {}, "anthropic": {}, "openai-codex": {}}
runtime_provider = types.ModuleType("hermes_cli.runtime_provider")

def _get_named_custom_provider(name):
    return {"name": name} if name == "mycustom" else None

runtime_provider._get_named_custom_provider = _get_named_custom_provider
sys.modules["hermes_cli"] = hermes_cli
sys.modules["hermes_cli.auth"] = auth
sys.modules["hermes_cli.runtime_provider"] = runtime_provider

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

def routed(value):
    kwargs = {}
    plugin._apply_lcm_model_route(kwargs, value)
    return kwargs

# Registry allowlist provider splits into provider/model.
assert routed("cerebras/llama-3.3-70b") == {"provider": "cerebras", "model": "llama-3.3-70b"}
# Registry providers off the allowlist stay model-only (OpenRouter-style slugs).
assert routed("anthropic/claude-sonnet") == {"model": "anthropic/claude-sonnet"}
# Named custom providers are resolvable, including the custom: prefix form.
assert routed("mycustom/bar-model") == {"provider": "mycustom", "model": "bar-model"}
assert routed("custom:mycustom/foo-model") == {"provider": "mycustom", "model": "foo-model"}
# Canonical built-ins behind custom: stay model-only.
assert routed("custom:openai-codex/gpt") == {"model": "custom:openai-codex/gpt"}
# Plain model names and empty overrides pass through untouched.
assert routed("plain-model") == {"model": "plain-model"}
assert routed("") == {}
assert routed(None) == {}

# End to end: routed kwargs reach the Hermes auxiliary client.
class Aux:
    def __init__(self):
        self.calls = []

    def call_llm(self, **kwargs):
        self.calls.append(kwargs)
        return "Routed summary"

agent = type("Agent", (), {"auxiliary_client": Aux()})()
engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project", agent=agent)
summary = engine._call_auxiliary_summary(
    "Summarize",
    [{"role": "user", "content": "raw"}],
    routes=[{"model": "cerebras/llama-3.3-70b"}],
)
assert summary["status"] == "ok"
assert summary["model"] == "cerebras/llama-3.3-70b"
assert agent.auxiliary_client.calls[0]["provider"] == "cerebras"
assert agent.auxiliary_client.calls[0]["model"] == "llama-3.3-70b"
"#,
        "generated plugin should split resolvable provider-prefixed LCM model overrides",
    );
}

#[test]
fn oversized_l1_summary_escalates_to_l2_bullets() {
    run_generated_plugin_script(
        "check_l2_escalation_rung.py",
        r#"
import importlib.machinery
import importlib.util
import json
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_l2_escalation"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

calls = []
source_content = "alpha beta " * 300
oversize_l1 = "L1 too big " * 2000

class Result:
    def __init__(self, returncode, stdout, stderr):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

def mcp_response(inner):
    return Result(0, json.dumps({"content": [{"type": "text", "text": json.dumps(inner)}]}), "")

def fake_run(argv, check, capture_output, text, timeout, shell):
    args = json.loads(argv[argv.index("--args") + 1])
    calls.append(args)
    if len(calls) == 1:
        return mcp_response({
            "status": "needs_summary",
            "reason": "summary_required",
            "summary_nodes_created": 0,
            "summary_nodes": [],
            "replay_messages": [{"role": "user", "content": "fresh"}],
            "frontier": {"current_frontier_store_id": None},
            "summary_request": {
                "provider": "cursor",
                "session_id": "session-1",
                "focus_topic": "handoff",
                "prompt": "Summarize backlog",
                "source_range": {"from_store_id": 1, "to_store_id": 1},
                "source_messages": [
                    {"store_id": 1, "role": "user", "content": source_content},
                ],
            },
        })
    summary = args["summarizer"]
    assert summary["mode"] == "provided"
    assert summary["summary_text"] == "Bullet summary of decisions"
    assert summary["route"] == "default"
    return mcp_response({
        "status": "ok",
        "reason": "compressed_backlog",
        "summary_nodes_created": 1,
        "summary_nodes": [],
        "replay_messages": [{"role": "system", "content": summary["summary_text"]}],
        "frontier": {"current_frontier_store_id": 1},
        "summary_request": None,
    })

plugin.tools.subprocess.run = fake_run

class EscalatingAux:
    def __init__(self):
        self.calls = []

    def call_llm(self, **kwargs):
        self.calls.append(kwargs)
        prompt = kwargs["messages"][0]["content"]
        if prompt.startswith("Summarize this conversation segment"):
            return oversize_l1
        return "Bullet summary of decisions"

agent = type("Agent", (), {"auxiliary_client": EscalatingAux()})()
engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project", agent=agent)
result = engine._compress_to_result(
    [{"role": "user", "content": source_content}],
    current_tokens=1200,
    focus_topic="handoff",
)

assert result["status"] == "ok"
assert result.get("fallback_used") is not True
assert len(calls) == 2
assert len(agent.auxiliary_client.calls) == 2
l2_prompt = agent.auxiliary_client.calls[1]["messages"][0]["content"]
assert l2_prompt.startswith("Compress this into bullet points. Maximum 1000 tokens.")
assert "Drop all reasoning, alternatives considered, and process detail." in l2_prompt
assert "Primary focus: handoff" in l2_prompt
assert "CONTENT:" in l2_prompt
"#,
        "oversized L1 summaries should escalate to the L2 bullet rung before fallback",
    );
}

#[test]
fn l1_auxiliary_errors_fall_through_to_l2_success() {
    run_generated_plugin_script(
        "check_l1_error_l2_success.py",
        r#"
import importlib.machinery
import importlib.util
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_l1_error_l2_success"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

source_messages = [
    {"store_id": 1, "role": "user", "content": "alpha " * 120},
    {"store_id": 2, "role": "assistant", "content": "beta " * 120},
]

class Aux:
    def __init__(self):
        self.calls = []

    def call_llm(self, **kwargs):
        self.calls.append(kwargs)
        prompt = kwargs["messages"][0]["content"]
        if prompt.startswith("Summarize this conversation segment"):
            raise RuntimeError("timed out")
        return "L2 concise summary"

agent = type("Agent", (), {"auxiliary_client": Aux()})()
engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project", agent=agent)
summary = engine._summarize_with_escalation(
    source_messages,
    focus_topic="handoff",
    allow_retry_signal=True,
)

assert summary["status"] == "ok"
assert summary["text"] == "L2 concise summary"
assert summary["route"] == "default"
assert len(summary["rung_failures"]) == 1
assert summary["rung_failures"][0]["level"] == 1
assert summary["rung_failures"][0]["status"] in ("retry", "error")
assert summary["rung_failures"][0]["error_classification"] == "retry_worthy"
assert len(agent.auxiliary_client.calls) == 2
"#,
        "L1 auxiliary errors should fall through to L2 success",
    );
}

#[test]
fn summary_acceptance_uses_token_estimates_not_chars() {
    run_generated_plugin_script(
        "check_token_based_acceptance.py",
        r#"
import importlib.machinery
import importlib.util
import json
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_token_acceptance"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

# Per-message role overhead means short messages cost ~5 tokens each even
# though they are only two characters long. A 51-char summary therefore has
# fewer tokens than the source but more characters than the char sum, which
# the old char-based acceptance falsely rejected.
source_messages = [
    {"store_id": idx + 1, "role": "user", "content": "hi"} for idx in range(10)
]
accepted_summary = "This summary is longer than the raw character sum."
assert len(accepted_summary) > sum(len(m["content"]) for m in source_messages)
assert plugin._count_messages_tokens(source_messages) > plugin._count_tokens(accepted_summary)

calls = []

class Result:
    def __init__(self, returncode, stdout, stderr):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

def mcp_response(inner):
    return Result(0, json.dumps({"content": [{"type": "text", "text": json.dumps(inner)}]}), "")

def fake_run(argv, check, capture_output, text, timeout, shell):
    args = json.loads(argv[argv.index("--args") + 1])
    calls.append(args)
    if len(calls) == 1:
        return mcp_response({
            "status": "needs_summary",
            "reason": "summary_required",
            "summary_nodes_created": 0,
            "summary_nodes": [],
            "replay_messages": [{"role": "user", "content": "fresh"}],
            "frontier": {"current_frontier_store_id": None},
            "summary_request": {
                "provider": "cursor",
                "session_id": "session-1",
                "focus_topic": None,
                "prompt": "Summarize backlog",
                "source_range": {"from_store_id": 1, "to_store_id": 10},
                "source_messages": source_messages,
            },
        })
    summary = args["summarizer"]
    assert summary["mode"] == "provided"
    assert summary["summary_text"] == accepted_summary
    assert summary["route"] == "default"
    return mcp_response({
        "status": "ok",
        "reason": "compressed_backlog",
        "summary_nodes_created": 1,
        "summary_nodes": [],
        "replay_messages": [{"role": "system", "content": summary["summary_text"]}],
        "frontier": {"current_frontier_store_id": 10},
        "summary_request": None,
    })

plugin.tools.subprocess.run = fake_run

class Aux:
    def __init__(self):
        self.calls = []

    def call_llm(self, **kwargs):
        self.calls.append(kwargs)
        return accepted_summary

agent = type("Agent", (), {"auxiliary_client": Aux()})()
engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project", agent=agent)
result = engine._compress_to_result(
    [{"role": "user", "content": "active"}],
    current_tokens=1200,
)

assert result["status"] == "ok"
assert result.get("fallback_used") is not True
assert len(calls) == 2
assert len(agent.auxiliary_client.calls) == 1
"#,
        "summary acceptance should compare token estimates, not character lengths",
    );
}

#[test]
fn generated_plugin_reads_lcm_env_config_overrides() {
    run_generated_plugin_script(
        "check_lcm_env_config.py",
        r#"
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import sys

for key in [name for name in os.environ if name.startswith("LCM_")]:
    del os.environ[key]

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_env_config"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

calls = []

class Result:
    def __init__(self, returncode, stdout, stderr):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

def fake_run(argv, check, capture_output, text, timeout, shell):
    calls.append(json.loads(argv[argv.index("--args") + 1]))
    outer = {"content": [{"type": "text", "text": json.dumps({"status": "ok"})}]}
    return Result(0, json.dumps(outer), "")

plugin.tools.subprocess.run = fake_run

# Documented hermes-lcm env vars (LCMConfig.from_env) override both the
# hardcoded defaults and host ctx.config attributes.
os.environ.update({
    "LCM_FRESH_TAIL_COUNT": "7",
    "LCM_LEAF_CHUNK_TOKENS": "111",
    "LCM_CONTEXT_THRESHOLD": "0.5",
    "LCM_CONDENSATION_FANIN": "9",
    "LCM_DYNAMIC_LEAF_CHUNK_ENABLED": "true",
    "LCM_DYNAMIC_LEAF_CHUNK_MAX": "222",
    "LCM_MAX_ASSEMBLY_TOKENS": "333",
    "LCM_RESERVE_TOKENS_FLOOR": "444",
    "LCM_INCREMENTAL_MAX_DEPTH": "3",
    "LCM_IGNORE_SESSION_PATTERNS": "tmp-*, scratch-*",
    "LCM_STATELESS_SESSION_PATTERNS": "ro-*",
    "LCM_IGNORE_MESSAGE_PATTERNS": "^/lcm ",
})

config = {"fresh_tail_count": 64, "context_length": 100000, "context_threshold": 0.9}
engine = plugin.TraceDecayContextEngine(config=config)
engine.initialize(session_id="session-1", project_root="/tmp/project")
engine.should_compress_preflight([{"role": "user", "content": "hello"}], current_tokens=10)

args = calls.pop()
assert args["fresh_tail_count"] == 7
assert args["leaf_chunk_tokens"] == 111
assert args["summary_fan_in"] == 9
assert args["dynamic_leaf_chunk_enabled"] is True
assert args["dynamic_leaf_chunk_max"] == 222
assert args["max_assembly_tokens"] == 333
assert args["reserve_tokens_floor"] == 444
assert args["incremental_max_depth"] == 3
assert args["context_length"] == 100000
# LCM_CONTEXT_THRESHOLD beats the ctx.config context_threshold attribute.
assert args["threshold_tokens"] == 50000
assert args["ignore_session_patterns"] == ["tmp-*", "scratch-*"]
assert args["stateless_session_patterns"] == ["ro-*"]
assert args["ignore_message_patterns"] == ["^/lcm"]

# Unparseable env values fall back to ctx.config / defaults instead of failing.
os.environ["LCM_MAX_ASSEMBLY_TOKENS"] = "not-a-number"
os.environ["LCM_CONTEXT_THRESHOLD"] = "also-bad"
engine.should_compress_preflight([{"role": "user", "content": "hello"}], current_tokens=10)
args = calls.pop()
assert args["max_assembly_tokens"] == 0
assert args["threshold_tokens"] == 90000
"#,
        "generated plugin should honor documented LCM_* env config overrides",
    );
}

#[test]
fn generated_plugin_falls_back_to_hermes_yaml_threshold() {
    run_generated_plugin_script(
        "check_lcm_yaml_threshold.py",
        r#"
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import sys
import tempfile

for key in [name for name in os.environ if name.startswith("LCM_")]:
    del os.environ[key]

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_yaml_threshold"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

calls = []

class Result:
    def __init__(self, returncode, stdout, stderr):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

def fake_run(argv, check, capture_output, text, timeout, shell):
    calls.append(json.loads(argv[argv.index("--args") + 1]))
    outer = {"content": [{"type": "text", "text": json.dumps({"status": "ok"})}]}
    return Result(0, json.dumps(outer), "")

plugin.tools.subprocess.run = fake_run

def preflight_threshold(config, hermes_home=None):
    engine = plugin.TraceDecayContextEngine(config=config)
    engine.initialize(
        session_id="session-1",
        project_root="/tmp/project",
        hermes_home=hermes_home,
    )
    engine.should_compress_preflight([{"role": "user", "content": "hello"}], current_tokens=10)
    return calls.pop().get("threshold_tokens")

with tempfile.TemporaryDirectory() as tmp:
    cfg = pathlib.Path(tmp) / "config.yaml"

    # An explicitly supplied host config can backfill LCM.
    cfg.write_text("compression:\n  enabled: true\n  threshold: 0.6\n")
    assert preflight_threshold({"context_length": 100000}, tmp) == 60000

    # Disabled Hermes compression must not leak its threshold into LCM.
    cfg.write_text("compression:\n  enabled: false\n  threshold: 0.9\n")
    assert preflight_threshold({"context_length": 100000}, tmp) == 75000

    # Explicit env and ctx.config thresholds still win over the YAML fallback.
    cfg.write_text("compression:\n  enabled: true\n  threshold: 0.6\n")
    os.environ["LCM_CONTEXT_THRESHOLD"] = "0.5"
    assert preflight_threshold({"context_length": 100000}, tmp) == 50000
    del os.environ["LCM_CONTEXT_THRESHOLD"]
    assert preflight_threshold({"context_length": 100000, "context_threshold": 0.8}, tmp) == 80000

with tempfile.TemporaryDirectory() as tmp:
    # No config.yaml at all: the documented 0.75 default applies whenever the
    # context window is known instead of silently disabling threshold pressure.
    assert preflight_threshold({"context_length": 100000}, tmp) == 75000
    assert preflight_threshold(None, tmp) is None

with tempfile.TemporaryDirectory() as tmp:
    # The explicitly resolved host home remains usable.
    cfg = pathlib.Path(tmp) / "config.yaml"
    cfg.write_text("compression:\n  enabled: true\n  threshold: 0.55\n")
    engine = plugin.TraceDecayContextEngine(config={"context_length": 100000})
    engine.initialize(session_id="session-1", project_root="/tmp/project", hermes_home=tmp)
    args = plugin._lcm_config_args(engine.config, engine.hermes_home)
    assert args["threshold_tokens"] == 55000

with tempfile.TemporaryDirectory() as tmp:
    # The environment override is ignored when no explicit host home is supplied.
    pathlib.Path(tmp, "config.yaml").write_text(
        "compression:\n  enabled: true\n  threshold: 0.1\n"
    )
    os.environ["HERMES_HOME"] = tmp
    assert preflight_threshold({"context_length": 100000}) == 75000
"#,
        "generated plugin should fall back to the Hermes YAML compression threshold",
    );
}

#[test]
fn generated_plugin_defaults_to_reserve_floor_for_runtime_context_cap() {
    run_generated_plugin_script(
        "check_default_reserve_floor.py",
        r#"
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import sys

for key in [name for name in os.environ if name.startswith("LCM_")]:
    del os.environ[key]

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_default_reserve_floor"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

calls = []

class Result:
    def __init__(self, returncode, stdout, stderr):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

def fake_run(argv, check, capture_output, text, timeout, shell):
    calls.append(json.loads(argv[argv.index("--args") + 1]))
    outer = {"content": [{"type": "text", "text": json.dumps({"status": "ok"})}]}
    return Result(0, json.dumps(outer), "")

plugin.tools.subprocess.run = fake_run

engine = plugin.TraceDecayContextEngine(config={"context_length": 100000})
engine.initialize(session_id="session-1", project_root="/tmp/project")
engine.should_compress_preflight(
    [{"role": "user", "content": "hello"}],
    current_tokens=96000,
)

args = calls.pop()
assert args["context_length"] == 100000
assert args["max_assembly_tokens"] == 0
assert args["reserve_tokens_floor"] == 4096

# Explicit config and env still win over the Hermes bridge default.
engine = plugin.TraceDecayContextEngine(
    config={"context_length": 100000, "reserve_tokens_floor": 8192}
)
engine.initialize(session_id="session-1", project_root="/tmp/project")
engine.should_compress_preflight([{"role": "user", "content": "hello"}], current_tokens=96000)
assert calls.pop()["reserve_tokens_floor"] == 8192

os.environ["LCM_RESERVE_TOKENS_FLOOR"] = "1234"
engine = plugin.TraceDecayContextEngine(
    config={"context_length": 100000, "reserve_tokens_floor": 8192}
)
engine.initialize(session_id="session-1", project_root="/tmp/project")
engine.should_compress_preflight([{"role": "user", "content": "hello"}], current_tokens=96000)
assert calls.pop()["reserve_tokens_floor"] == 1234
"#,
        "generated plugin should derive a default assembly cap from the runtime context window",
    );
}

#[test]
fn generated_plugin_preserves_zero_value_leaf_and_tail_knobs() {
    run_generated_plugin_script(
        "check_zero_value_knobs.py",
        r#"
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import sys

for key in [name for name in os.environ if name.startswith("LCM_")]:
    del os.environ[key]

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_zero_knobs"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

calls = []

class Result:
    def __init__(self, returncode, stdout, stderr):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr

def fake_run(argv, check, capture_output, text, timeout, shell):
    calls.append(json.loads(argv[argv.index("--args") + 1]))
    outer = {"content": [{"type": "text", "text": json.dumps({"status": "ok"})}]}
    return Result(0, json.dumps(outer), "")

plugin.tools.subprocess.run = fake_run

os.environ["LCM_FRESH_TAIL_COUNT"] = "0"
os.environ["LCM_LEAF_CHUNK_TOKENS"] = "0"

engine = plugin.TraceDecayContextEngine(config={"fresh_tail_count": 9, "leaf_chunk_tokens": 9000})
engine.initialize(session_id="session-1", project_root="/tmp/project")
engine.should_compress_preflight([{"role": "user", "content": "hello"}], current_tokens=10)

args = calls.pop()
assert args["fresh_tail_count"] == 0
assert args["leaf_chunk_tokens"] == 0
"#,
        "generated plugin should preserve zero-valued LCM env knobs",
    );
}

#[test]
fn context_engine_propagates_runtime_context_window_from_update_model_and_session_start() {
    run_generated_plugin_script(
        "check_context_window_runtime_precedence.py",
        r#"
import importlib.machinery
import importlib.util
import json
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_context_window_precedence"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

calls = []

def fake_call_tracedecay_tool(tool, args, **kwargs):
    calls.append((tool, dict(args)))
    if tool == "tracedecay_lcm_preflight":
        payload = {"status": "ok", "should_compress": True, "reason": "test", "replay_messages": []}
    else:
        payload = {"status": "ok", "reason": "test", "replay_messages": [], "summary_nodes": [], "frontier": {"provider": "cursor", "conversation_id": "session-1", "current_session_id": "session-1", "current_frontier_store_id": None, "last_finalized_session_id": None, "last_finalized_frontier_store_id": None, "maintenance_debt": []}}
    return json.dumps({"content": [{"type": "text", "text": json.dumps(payload)}]})

plugin.tools.call_tracedecay_tool = fake_call_tracedecay_tool

engine = plugin.TraceDecayContextEngine(
    config={"context_length": 100000, "context_threshold": 0.8}
)
engine.initialize(session_id="session-1", project_root="/tmp/project")

# Fallback to ctx.config before runtime updates.
engine.should_compress_preflight([{"role": "user", "content": "hello"}], current_tokens=1)
_, args = calls.pop()
assert args["context_length"] == 100000
assert args["threshold_tokens"] == 80000

# Session-start kwargs should arm threshold pressure by themselves.
engine.on_session_start(session_id="session-1", context_length=200000)
engine.should_compress_preflight([{"role": "user", "content": "hello"}], current_tokens=1)
_, args = calls.pop()
assert args["context_length"] == 200000
assert args["threshold_tokens"] == 160000

# update_model is authoritative over ctx.config/session-start metadata.
engine.update_model(
    model="deepseek-v4-flash",
    context_length=1000000,
    base_url="https://opencode.ai/zen/go",
    api_key="test-key",
    provider="opencode-go",
    api_mode="anthropic_messages",
)
assert engine.model == "deepseek-v4-flash"

engine.should_compress_preflight([{"role": "user", "content": "hello"}], current_tokens=1)
_, args = calls.pop()
assert args["context_length"] == 1000000
assert args["threshold_tokens"] == 800000

# compress must forward the same runtime window and threshold.
engine.compress([{"role": "user", "content": "compress me"}], current_tokens=1)
tool, args = calls.pop()
assert tool == "tracedecay_lcm_compress"
assert args["context_length"] == 1000000
assert args["threshold_tokens"] == 800000

# should_compress gates locally below the tracked threshold (no spawn)...
calls.clear()
assert engine.should_compress(prompt_tokens=1) is False
assert calls == []
# ...and defers to the preflight probe once tokens reach the threshold.
assert engine.should_compress(prompt_tokens=800000) is True
tool, _args = calls.pop()
assert tool == "tracedecay_lcm_preflight"
"#,
        "generated plugin should propagate update_model/session_start context windows into preflight/compress",
    );
}

#[test]
fn context_engine_force_compress_sets_backend_overflow_cap() {
    run_generated_plugin_script(
        "check_force_compress_sets_backend_cap.py",
        r#"
import importlib.machinery
import importlib.util
import json
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_force_compress"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

calls = []

def fake_call_tracedecay_tool(tool, args, **kwargs):
    calls.append((tool, dict(args), dict(kwargs)))
    payload = {
        "status": "ok",
        "reason": "forced_overflow_recovery",
        "summary_nodes": [],
        "replay_messages": [{"role": "user", "content": "compressed"}],
        "frontier": {
            "provider": "cursor",
            "conversation_id": "session-1",
            "current_session_id": "session-1",
            "current_frontier_store_id": None,
            "last_finalized_session_id": None,
            "last_finalized_frontier_store_id": None,
            "maintenance_debt": [],
        },
    }
    return json.dumps({"content": [{"type": "text", "text": json.dumps(payload)}]})

plugin.tools.call_tracedecay_tool = fake_call_tracedecay_tool

engine = plugin.TraceDecayContextEngine(config={"context_length": 100000, "context_threshold": 0.8})
engine.initialize(session_id="session-1", project_root="/tmp/project")

compressed = engine.compress(
    [{"role": "user", "content": "compress this oversized turn now"}],
    current_tokens=90000,
    force=True,
)

tool, args, kwargs = calls.pop()
assert tool == "tracedecay_lcm_compress"
assert args["max_assembly_tokens"] == 89999
assert "force" not in kwargs
assert compressed == [{"role": "user", "content": "compressed"}]
"#,
        "generated plugin should translate Hermes force=True into a backend overflow cap",
    );
}

#[test]
fn context_engine_expand_query_uses_expansion_model_context_and_timeout_knobs() {
    run_generated_plugin_script(
        "check_expand_query_expansion_knobs.py",
        r#"
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import sys

for key in [name for name in os.environ if name.startswith("LCM_EXPANSION_")]:
    del os.environ[key]

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_expand_query_knobs"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

tool_calls = []

def fake_call_tracedecay_tool(tool, args, **kwargs):
    tool_calls.append((tool, dict(args)))
    payload = {
        "status": "ok",
        "prompt": "What changed?",
        "query": "orchard",
        "needs_synthesis": True,
        "max_tokens": 64,
        "context_max_tokens": args.get("context_max_tokens"),
        "context_blocks": [{"kind": "raw_message", "content": "expanded context"}],
        "synthesis_prompt": {"system": "sys", "user": "usr"},
    }
    return json.dumps({"content": [{"type": "text", "text": json.dumps(payload)}]})

plugin.tools.call_tracedecay_tool = fake_call_tracedecay_tool

class FakeAuxClient:
    def __init__(self):
        self.calls = []
    def call_llm(self, **kwargs):
        self.calls.append(kwargs)
        return {"content": "synthetic answer"}

class FakeAgent:
    def __init__(self):
        self.auxiliary_client = FakeAuxClient()

os.environ["LCM_EXPANSION_MODEL"] = "env-expansion-model"
os.environ["LCM_EXPANSION_CONTEXT_TOKENS"] = "4321"
os.environ["LCM_EXPANSION_TIMEOUT_MS"] = "9000"

agent = FakeAgent()
engine = plugin.TraceDecayContextEngine(
    config={
        "expansion_model": "cfg-expansion-model",
        "expansion_context_tokens": 32000,
        "expansion_timeout_ms": 120000,
    }
)
engine.initialize(session_id="session-1", project_root="/tmp/project", agent=agent)

result = engine.expand_query(prompt="What changed?", query="orchard")
assert result["status"] == "ok"
assert result["needs_synthesis"] is False
assert result["answer"] == "synthetic answer"

tool, args = tool_calls.pop()
assert tool == "tracedecay_lcm_expand_query"
assert args["context_max_tokens"] == 4321

llm_call = agent.auxiliary_client.calls.pop()
assert llm_call["model"] == "env-expansion-model"
assert llm_call["timeout"] == 9.0
"#,
        "generated plugin should source expansion knobs from env/config and apply them to expand_query synthesis",
    );
}

#[test]
fn summary_routes_built_from_config_and_env_models() {
    run_generated_plugin_script(
        "check_summary_route_wiring.py",
        r#"
import importlib.machinery
import importlib.util
import os
import pathlib
import sys
import tempfile

for key in [name for name in os.environ if name.startswith("LCM_")]:
    del os.environ[key]

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_summary_routes"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

# summary_model / summary_fallback_models from ctx.config wire the default
# auxiliary route chain (deduplicated, in order, with the summary timeout).
config = {
    "summary_model": "primary",
    "summary_fallback_models": ["backup", "primary"],
    "summary_timeout_ms": 30000,
}
engine = plugin.TraceDecayContextEngine(config=config)
engine.initialize(session_id="session-1", project_root="/tmp/project")
routes = engine._auxiliary_routes()
assert [route.get("model") for route in routes] == ["primary", "backup"]
assert [route.get("timeout") for route in routes] == [30.0, 30.0]

# Hosts that pass no config keep the single task-default route; an environment
# override cannot redirect the timeout config.
with tempfile.TemporaryDirectory() as tmp:
    os.environ["HERMES_HOME"] = tmp
    pathlib.Path(tmp, "config.yaml").write_text(
        "auxiliary:\n  compression:\n    timeout: 1\n"
    )
    bare_engine = plugin.TraceDecayContextEngine()
    bare_engine.initialize(session_id="session-1", project_root="/tmp/project")
    bare_routes = bare_engine._auxiliary_routes()
    assert len(bare_routes) == 1
    assert "model" not in bare_routes[0]
    assert bare_routes[0]["timeout"] == 60.0

with tempfile.TemporaryDirectory() as tmp:
    cfg = pathlib.Path(tmp) / "config.yaml"
    cfg.write_text("auxiliary:\n  compression:\n    timeout: 12.5\n")
    yaml_engine = plugin.TraceDecayContextEngine()
    yaml_engine.initialize(session_id="session-1", project_root="/tmp/project", hermes_home=tmp)
    yaml_routes = yaml_engine._auxiliary_routes()
    assert yaml_routes[0]["timeout"] == 12.5

with tempfile.TemporaryDirectory() as tmp:
    cfg = pathlib.Path(tmp) / "config.yaml"
    cfg.write_text("auxiliary:\n  compression:\n    timeout: not-a-number\n")
    malformed_engine = plugin.TraceDecayContextEngine()
    malformed_engine.initialize(session_id="session-1", project_root="/tmp/project", hermes_home=tmp)
    malformed_routes = malformed_engine._auxiliary_routes()
    assert malformed_routes[0]["timeout"] == 60.0

with tempfile.TemporaryDirectory() as tmp:
    missing_engine = plugin.TraceDecayContextEngine()
    missing_engine.initialize(session_id="session-1", project_root="/tmp/project", hermes_home=tmp)
    missing_routes = missing_engine._auxiliary_routes()
    assert missing_routes[0]["timeout"] == 60.0

# Documented env vars override ctx.config for the route chain.
os.environ["LCM_SUMMARY_MODEL"] = "env-primary"
os.environ["LCM_SUMMARY_FALLBACK_MODELS"] = "env-backup1, env-backup2"
os.environ["LCM_SUMMARY_TIMEOUT_MS"] = "45000"
env_routes = engine._auxiliary_routes()
assert [route.get("model") for route in env_routes] == [
    "env-primary",
    "env-backup1",
    "env-backup2",
]
assert env_routes[0]["timeout"] == 45.0
for key in [name for name in os.environ if name.startswith("LCM_")]:
    del os.environ[key]

# Per-call model overrides still take precedence over the configured chain.
kwarg_routes = engine._auxiliary_routes(model="kwarg-model")
assert [route.get("model") for route in kwarg_routes] == ["kwarg-model"]

# End to end: the configured chain falls over from primary to backup.
class RoutingAux:
    def __init__(self):
        self.calls = []

    def call_llm(self, **kwargs):
        self.calls.append(kwargs)
        if kwargs.get("model") == "primary":
            raise RuntimeError("primary unavailable")
        return "Configured backup summary"

agent = type("Agent", (), {"auxiliary_client": RoutingAux()})()
engine.agent = agent
summary = engine._call_auxiliary_summary(
    "Summarize",
    [{"role": "user", "content": "raw"}],
)
assert summary["status"] == "ok"
assert summary["text"] == "Configured backup summary"
assert summary["route"] == "backup"
assert summary["model"] == "backup"
assert [call.get("model") for call in agent.auxiliary_client.calls] == ["primary", "backup"]
assert agent.auxiliary_client.calls[0]["timeout"] == 30.0
"#,
        "summary model chain should wire config and env models into auxiliary routes",
    );
}

#[test]
fn call_tracedecay_json_normalizes_and_decodes_mcp_envelopes() {
    run_generated_plugin_script(
        "check_bridge_envelope_decoding.py",
        r#"
import json

responses = []

def fake_call_tracedecay_tool(name, args, **kwargs):
    return responses.pop(0)

plugin.tools.call_tracedecay_tool = fake_call_tracedecay_tool

def call_with_outer(outer):
    responses.append(json.dumps(outer))
    return plugin.call_tracedecay_json("tracedecay_lcm_preflight", {})

missing_content = call_with_outer({})
assert missing_content["error"] == "tracedecay tool response missing text content"

empty_content = call_with_outer({"content": []})
assert empty_content["error"] == "tracedecay tool response missing text content"

non_text_content = call_with_outer({"content": [{"type": "text", "text": 123}]})
assert non_text_content["error"] == "tracedecay tool response missing text content"

responses.append(json.dumps({"content": [{"type": "text", "text": "{not json"}]}))
invalid_nested_json = plugin.call_tracedecay_json("tracedecay_lcm_preflight", {})
assert invalid_nested_json["error"] == "tracedecay tool returned invalid nested JSON"

outer_error = {"error": "tool failed", "code": "boom", "content": []}
assert call_with_outer(outer_error) == outer_error

calls = []

def envelope(payload):
    return json.dumps({"content": [{"type": "text", "text": json.dumps(payload)}]})

def fake_retrieve_call(name, args, **kwargs):
    calls.append((name, args, kwargs))
    if name == "tracedecay_lcm_preflight":
        return envelope({"truncated": True, "handle": "payload-1"})
    if name == "tracedecay_retrieve":
        if args == {"handle": "payload-1"}:
            assert kwargs == {"project_root": "/tmp/project"}
            return envelope({"content": json.dumps({"should_compress": True, "source": "retrieved"})})
        assert args == {"handle": "payload-ignored"}
        assert kwargs == {}
        return envelope({"count": 1, "facts": [{"fact": {"content": "retrieved fact"}}]})
    if name == "tracedecay_fact_store":
        return envelope({"truncated": True, "handle": "payload-ignored"})
    raise AssertionError(f"unexpected tool call: {name}")

plugin.tools.call_tracedecay_tool = fake_retrieve_call
retrieved = plugin.call_tracedecay_json("tracedecay_lcm_preflight", {}, project_root="/tmp/project")
assert retrieved == {"should_compress": True, "source": "retrieved"}
assert [call[0] for call in calls] == ["tracedecay_lcm_preflight", "tracedecay_retrieve"]

retrieved_fact = plugin.call_tracedecay_json("tracedecay_fact_store", {})
assert retrieved_fact == {"count": 1, "facts": [{"fact": {"content": "retrieved fact"}}]}
assert [call[0] for call in calls] == [
    "tracedecay_lcm_preflight",
    "tracedecay_retrieve",
    "tracedecay_fact_store",
    "tracedecay_retrieve",
]

plugin.tools.call_tracedecay_tool = fake_call_tracedecay_tool
split_payload = json.dumps({"status": "ok", "source": "split-content"})
responses.append(json.dumps({
    "content": [
        {"type": "text", "text": split_payload[:12]},
        {"type": "text", "text": split_payload[12:]},
    ]
}))
split_content = plugin.call_tracedecay_json("tracedecay_lcm_preflight", {})
assert split_content == {"status": "ok", "source": "split-content"}

nested_payload = {"content": json.dumps({"status": "ok", "source": "nested-content"})}
responses.append(envelope(nested_payload))
nested_content = plugin.call_tracedecay_json("tracedecay_lcm_preflight", {})
assert nested_content == {"status": "ok", "source": "nested-content"}

response_handle_calls = []

def fake_response_handle_call(name, args, **kwargs):
    response_handle_calls.append((name, args, kwargs))
    if name == "tracedecay_lcm_compress":
        return envelope({"truncated": True, "response_handle": "payload-2"})
    if name == "tracedecay_retrieve":
        assert args == {"handle": "payload-2"}
        return envelope({
            "content": [
                {
                    "type": "text",
                    "text": json.dumps({"status": "ok", "source": "response-handle"}),
                }
            ]
        })
    raise AssertionError(f"unexpected tool call: {name}")

plugin.tools.call_tracedecay_tool = fake_response_handle_call
response_handle_payload = plugin.call_tracedecay_json("tracedecay_lcm_compress", {})
assert response_handle_payload == {"status": "ok", "source": "response-handle"}
assert [call[0] for call in response_handle_calls] == [
    "tracedecay_lcm_compress",
    "tracedecay_retrieve",
]
"#,
        "generated JSON bridge should normalize malformed envelopes and decode LCM payloads",
    );
}

// The generated tool bridge wraps every subprocess failure mode in a JSON
// error payload instead of raising: missing binary, nonzero exit with partial
// output, malformed stdout JSON, and empty stdout all stay machine-readable
// for the host.
#[test]
fn call_tracedecay_tool_reports_subprocess_failures_as_json_errors() {
    run_generated_plugin_script(
        "check_subprocess_failure_modes.py",
        r#"
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
fake_tools = pathlib.Path(os.environ["TRACEDECAY_TEST_FAKE_TOOLS"])
parent_name = "_hermes_user_subprocess_failures"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

tools = plugin.tools

# The fake binaries are part of the shared immutable fixture, generated once
# per machine by the Rust harness rather than rewritten (and re-chmodded, and
# on Windows rescanned and re-imaged) on every run of this check.
def fake_binary(name):
    path = fake_tools / (f"{name}.cmd" if os.name == "nt" else name)
    assert path.is_file(), f"missing fake tracedecay fixture: {path}"
    return str(path)

# Missing binary: the OSError is wrapped, never raised.
tools.TRACEDECAY_BIN = str(fake_tools / "definitely-missing-tracedecay")
missing = json.loads(tools.call_tracedecay_tool("tracedecay_lcm_status", {}))
assert missing["error"].startswith("tracedecay tool failed:"), missing
# The JSON bridge surfaces the same error dict without raising.
assert "error" in plugin.call_tracedecay_json("tracedecay_lcm_status", {})

# Subprocess dies mid-handshake: nonzero exit with partial stdout and stderr
# is reported with the exit status and bounded captures.
# cmd.exe `echo` always appends a newline that POSIX printf does not, so trim
# trailing newlines only on Windows and keep Unix byte-exact.
def trim_capture(text):
    return text.rstrip("\r\n") if os.name == "nt" else text

tools.TRACEDECAY_BIN = fake_binary("fake-tracedecay-crash")
crashed = json.loads(tools.call_tracedecay_tool("tracedecay_lcm_status", {}))
assert crashed["error"] == "tracedecay tool exited with status 3", crashed
assert trim_capture(crashed["stdout"]) == '{"content', crashed
assert trim_capture(crashed["stderr"]) == "handshake aborted", crashed

# Exit 0 with malformed JSON on stdout.
tools.TRACEDECAY_BIN = fake_binary("fake-tracedecay-badjson")
malformed = json.loads(tools.call_tracedecay_tool("tracedecay_lcm_status", {}))
assert malformed["error"] == "tracedecay tool returned invalid JSON", malformed
assert trim_capture(malformed["stdout"]) == "not-json-at-all", malformed

# Exit 0 with empty stdout normalizes to an empty JSON object.
tools.TRACEDECAY_BIN = fake_binary("fake-tracedecay-empty")
assert tools.call_tracedecay_tool("tracedecay_lcm_status", {}) == "{}"
"#,
        "generated tool bridge should normalize subprocess failures into JSON errors",
    );
}

// With the tracedecay binary unavailable, the context engine must degrade
// gracefully: preflight/status/compress return error dicts and session-start
// boundary reporting never raises into the host.
#[test]
fn context_engine_degrades_when_tracedecay_binary_missing() {
    run_generated_plugin_script(
        "check_engine_missing_binary_degradation.py",
        r#"
import importlib.machinery
import importlib.util
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_missing_binary"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

plugin.tools.TRACEDECAY_BIN = str(plugin_dir / "definitely-missing-tracedecay")

engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-degraded", project_root=str(plugin_dir))

# Boundary reporting on session start swallows the failure.
engine.on_session_start(
    session_id="session-degraded-next",
    old_session_id="session-degraded",
    boundary_reason="compression",
)

messages = [{"role": "user", "content": "hello"}]
preflight = engine._preflight_probe(messages, current_tokens=128)
assert isinstance(preflight, dict), preflight
assert preflight["error"].startswith("tracedecay tool failed:"), preflight
# ABC contract: the bool probe must not treat the error dict as truthy.
assert engine.should_compress_preflight(messages, current_tokens=128) is False

status = engine.status()
assert isinstance(status, dict)
assert status["error"].startswith("tracedecay tool failed:"), status

# compress() honors the host ABC contract even while degraded: the input
# list comes back unchanged, the abort flag is set, and the raw error dict
# stays on the engine for diagnostics.
compressed = engine.compress(messages, current_tokens=128)
assert compressed == messages, compressed
assert engine._last_compress_aborted is True
assert engine.last_compress_result["error"].startswith("tracedecay tool failed:")

# Tool dispatch through the engine surface stays JSON-stringly typed.
raw = engine.handle_tool_call("lcm_status", {})
assert isinstance(raw, str)
assert "error" in raw
"#,
        "context engine should degrade to error dicts when the tracedecay binary is missing",
    );
}

#[test]
fn lcm_compression_request_contract_serializes_fake_summarizer() {
    let request = LcmCompressionRequest {
        provider: "cursor".to_string(),
        session_id: "session-1".to_string(),
        messages: vec![serde_json::json!({"role": "user", "content": "fresh"})],
        current_tokens: Some(100),
        focus_topic: Some("billing".to_string()),
        ignore_session_patterns: Vec::new(),
        stateless_session_patterns: Vec::new(),
        ignore_message_patterns: Vec::new(),
        expected_current_frontier_store_id: None,
        threshold_tokens: None,
        max_assembly_tokens: None,
        leaf_chunk_tokens: None,
        max_source_messages: None,
        summary_fan_in: None,
        incremental_max_depth: None,
        fresh_tail_count: None,
        dynamic_leaf_chunk_enabled: None,
        dynamic_leaf_chunk_max: None,
        context_length: None,
        reserve_tokens_floor: None,
        summarizer: LcmSummarizerMode::Fake {
            summary_text: "deterministic summary".to_string(),
        },
    };

    let encoded = serde_json::to_value(&request).unwrap();
    assert_eq!(encoded["provider"], "cursor");
    assert_eq!(encoded["session_id"], "session-1");
    assert_eq!(encoded["messages"][0]["content"], "fresh");
    assert_eq!(encoded["current_tokens"], 100);
    assert_eq!(encoded["focus_topic"], "billing");
    assert!(encoded.get("threshold_tokens").is_none());
    assert!(encoded.get("fresh_tail_count").is_none());
    assert!(encoded.get("dynamic_leaf_chunk_enabled").is_none());
    assert_eq!(encoded["summarizer"]["mode"], "fake");
    assert_eq!(
        encoded["summarizer"]["summary_text"],
        "deterministic summary"
    );

    let decoded: LcmCompressionRequest = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.provider, "cursor");
    assert_eq!(decoded.session_id, "session-1");
    assert_eq!(decoded.messages[0]["role"], "user");
    assert_eq!(decoded.current_tokens, Some(100));
    assert_eq!(decoded.focus_topic.as_deref(), Some("billing"));
    assert_eq!(
        decoded.summarizer,
        LcmSummarizerMode::Fake {
            summary_text: "deterministic summary".to_string()
        }
    );
}

#[test]
fn lcm_summarizer_modes_are_stable_bridge_placeholders() {
    let noop = serde_json::to_value(LcmSummarizerMode::Noop).unwrap();
    assert_eq!(noop, serde_json::json!({"mode": "noop"}));

    let hermes_auxiliary = serde_json::to_value(LcmSummarizerMode::HermesAuxiliary).unwrap();
    assert_eq!(
        hermes_auxiliary,
        serde_json::json!({"mode": "hermes_auxiliary"})
    );

    let decoded: LcmSummarizerMode =
        serde_json::from_value(serde_json::json!({"mode": "fake", "summary_text": "fixed"}))
            .unwrap();
    assert_eq!(
        decoded,
        LcmSummarizerMode::Fake {
            summary_text: "fixed".to_string()
        }
    );
}

/// Newer Hermes declares `ContextEngine.update_from_response(usage)` as an
/// abstract method; the generated engine must implement it or the plugin
/// fails to load with "Can't instantiate abstract class".
#[test]
fn generated_context_engine_satisfies_abstract_update_from_response() {
    run_python_check(
        "check_update_from_response.py",
        r#"
import abc
import importlib.machinery
import importlib.util
import pathlib
import sys
import types

plugin_dir = pathlib.Path(sys.argv[1])

# Mimic the newer Hermes ABC *before* the plugin module is executed.
class ContextEngine(abc.ABC):
    @abc.abstractmethod
    def update_from_response(self, usage):
        raise NotImplementedError

agent_module = types.ModuleType("agent")
agent_module.__path__ = []
context_engine_module = types.ModuleType("agent.context_engine")
context_engine_module.ContextEngine = ContextEngine
agent_module.context_engine = context_engine_module
sys.modules["agent"] = agent_module
sys.modules["agent.context_engine"] = context_engine_module

parent_name = "_hermes_user_abc"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

class Ctx:
    def __init__(self):
        self.context_engines = []
    def register_hook(self, name, handler):
        pass
    def register_context_engine(self, engine):
        self.context_engines.append(engine)

ctx = Ctx()
# This instantiates TraceDecayContextEngine; an unimplemented abstract method
# raises TypeError here.
plugin.register(ctx)
assert len(ctx.context_engines) == 1
engine = ctx.context_engines[0]

engine.update_from_response({"prompt_tokens": 11, "completion_tokens": 7})
assert engine.last_prompt_tokens == 11
assert engine.last_completion_tokens == 7
assert engine.last_total_tokens == 18

engine.update_from_response(
    {"input_tokens": "3", "output_tokens": "4", "total_tokens": 9}
)
assert engine.last_prompt_tokens == 3
assert engine.last_completion_tokens == 4
assert engine.last_total_tokens == 9

engine.update_from_response(None)
assert engine.last_prompt_tokens == 0
assert engine.last_completion_tokens == 0
assert engine.last_total_tokens == 0
"#,
        "generated engine must satisfy the abstract update_from_response contract",
    );
}

/// Newer Hermes derives the skill namespace from the plugin name and rejects
/// ':' inside skill names, so registration must use the bare "tracedecay".
#[test]
fn generated_register_uses_colon_free_skill_name() {
    run_generated_plugin_script(
        "check_skill_name.py",
        r#"
class Ctx:
    def __init__(self):
        self.skills = []
    def register_hook(self, name, handler):
        pass
    def register_skill(self, name, path):
        if ":" in name:
            raise ValueError(f"invalid skill name: {name}")
        self.skills.append((name, path))

ctx = Ctx()
plugin.register(ctx)
assert ctx.skills, "expected the tracedecay skill to be registered"
assert ctx.skills[0][0] == "tracedecay", ctx.skills
assert ctx.skills[0][1].name == "SKILL.md"
"#,
        "generated registration must register the skill under the bare 'tracedecay' name",
    );
}

#[test]
fn generated_skill_mirrors_session_context_retrieval_contract() {
    let template = host_sources::HERMES_SKILL_MD;
    let installed =
        std::fs::read_to_string(SHARED_INSTALL.plugin_dir.join("skills/tracedecay/SKILL.md"))
            .unwrap();
    let required_markers = [
        "tracedecay_message_search",
        "`provider=all`",
        "`catch_up=false`",
        "`limit=10`",
        "lcm_grep",
        "lcm_load_session",
        "lcm_describe",
        "lcm_expand",
        "lcm_expand_query",
        "`temporal_mode=current`",
        "`temporal_mode=forensic`",
        "`next_cursor`",
        "same target, source limit, and content slice",
        "`coverage`",
        "`anchors`",
        "needs_synthesis=true",
        "host must synthesize",
        "tracedecay_sessions_for",
        "tracedecay_workflows",
        "`limit=20`",
        "tracedecay_session_refresh",
        "`begin`",
        "`status`",
        "`cancel`",
    ];

    for (label, skill) in [
        (
            "repository managing-session-context skill",
            include_str!("../../plugin/skills/managing-session-context/SKILL.md"),
        ),
        ("Hermes template", template),
        ("installed Hermes skill snapshot", installed.as_str()),
    ] {
        for marker in required_markers {
            assert!(
                skill.contains(marker),
                "{label} should document session retrieval marker {marker:?}",
            );
        }
        assert!(
            !skill.contains("after_store_id"),
            "{label} must not teach deprecated numeric-cursor pagination",
        );
    }

    assert_eq!(
        installed, template,
        "the installed Hermes skill snapshot should exactly match its template",
    );
}

/// Stock Hermes keeps plugin skills out of the flat skills index, so the
/// first-turn code nudge must expose the qualified name that skill_view accepts.
#[test]
fn generated_nudge_makes_plugin_skill_discoverable() {
    run_generated_plugin_script(
        "check_skill_discovery_nudge.py",
        r#"
plugin._REGISTERED_TOOL_NAMES.add("tracedecay_search")
text = plugin._pre_llm_call(
    is_first_turn=True,
    user_message="Find the callers of this Rust function",
)
assert "skill_view" in text, text
assert "tracedecay:tracedecay" in text, text
"#,
        "generated nudge must reveal the qualified Hermes plugin skill name",
    );
}

/// Host runtime config may identify a project, but a real session cwd or
/// explicit call wins. The plugin-owned config block is filtered separately.
#[test]
fn generated_context_engine_prefers_runtime_project_context() {
    run_generated_plugin_script(
        "check_context_engine_project_root.py",
        r#"
class Ctx:
    def __init__(self):
        self.config = {"project_root": "/tmp/pinned-project"}
        self.context_engines = []
    def register_hook(self, name, handler):
        pass
    def register_context_engine(self, engine):
        self.context_engines.append(engine)

ctx = Ctx()
plugin._resolved_project_scope = lambda path, *_args: path
plugin.register(ctx)
engine = ctx.context_engines[0]

# Host runtime config applies at registration time.
assert engine.project_root == "/tmp/pinned-project", engine.project_root

# A real session cwd overrides stale host config.
engine.on_session_start(session_id="s1", cwd="/somewhere/else")
assert engine.project_root == "/somewhere/else", engine.project_root

# An explicit project_root remains highest priority.
engine.on_session_start(session_id="s2", project_root="/explicit/root")
assert engine.project_root == "/explicit/root", engine.project_root

# Returning to a prior session restores its route instead of inheriting the
# most recently active session or the host default.
engine.on_session_start(session_id="s1")
assert engine.project_root == "/somewhere/else", engine.project_root

# Without explicit runtime context, cwd is the fallback.
unpinned = plugin.TraceDecayContextEngine(config={})
assert unpinned.project_root is None
unpinned.on_session_start(session_id="s3", cwd="/cwd/fallback")
assert unpinned.project_root == "/cwd/fallback", unpinned.project_root
"#,
        "generated engine must prefer explicit project and real cwd over host config",
    );
}

/// Legacy profile pins may still exist long enough for migration, but the
/// generated runtime must ignore them for all TraceDecay routing.
#[test]
fn generated_plugin_ignores_legacy_profile_project_root_pin() {
    // Pinned install: the shared unpinned fixture cannot cover this, so it
    // keeps a private TempDir. The pin only affects config.yaml/tools
    // dispatch, so the dashboard deploy is skipped for speed (pinned
    // dashboard deploys are covered in `dashboard.rs`).
    let home = TempDir::new().unwrap();
    let ctx = InstallContext {
        home: home.path().to_path_buf(),
        tracedecay_bin: "/usr/local/bin/tracedecay".to_string(),
        tool_permissions: Vec::new(),
        project_root: Some(std::path::PathBuf::from("/pinned/project")),
        dashboard: false,
    };
    HermesIntegration.install(&ctx).unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    let script = plugin_dir.join("check_project_root_pin.py");
    std::fs::write(
        &script,
        format!(
            "{PYYAML_FALLBACK_PRELUDE}\n{}",
            r#"
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import sys
import types

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_pin"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

tools = plugin.tools
# A legacy pin may exist in config.yaml, but generated code exposes no reader.
assert not hasattr(tools, "PINNED_PROJECT_ROOT")
assert not hasattr(tools, "config_pinned_project_root")

# Default CLI dispatch uses the real process cwd, never the profile pin.
captured = []

class FakeResult:
    returncode = 0
    stdout = "{}"
    stderr = ""

def fake_run(argv, **kwargs):
    captured.append(argv)
    return FakeResult()

tools.subprocess.run = fake_run
tools.call_tracedecay_tool("tracedecay_status", {})
assert captured, "expected a subprocess invocation"
argv = captured[-1]
idx = argv.index("--project")
assert argv[idx + 1] == os.getcwd(), argv
assert argv[idx + 1] != "/pinned/project", argv

# An explicit project still wins over the pin.
tools.call_tracedecay_tool("tracedecay_status", {}, project_root="/explicit/root")
argv = captured[-1]
idx = argv.index("--project")
assert argv[idx + 1] == "/explicit/root", argv

# Memory tools follow the same real cwd route.
tools.call_tracedecay_tool("tracedecay_fact_store", {})
argv = captured[-1]
idx = argv.index("--project")
assert argv[idx + 1] == os.getcwd(), argv

# A stale MCP `project_root` argument is not translated. The normal cwd route
# still selects the user-profile project store, and the strict CLI rejects the
# unknown argument instead of silently preserving a compatibility protocol.
tools.call_tracedecay_tool(
    "tracedecay_lcm_status",
    {"project_root": "/tmp/hermes-profile"},
)
argv = captured[-1]
idx = argv.index("--project")
assert argv[idx + 1] == os.getcwd(), argv
payload = json.loads(argv[argv.index("--args") + 1])
assert payload["project_root"] == "/tmp/hermes-profile", payload

# Engine resolution never consults the legacy profile pin.
engine = plugin.TraceDecayContextEngine(config={})
assert engine.project_root is None, engine.project_root
plugin._resolved_project_scope = lambda path, *_args: path
engine.on_session_start(session_id="s1", cwd="/somewhere/else")
assert engine.project_root == "/somewhere/else", engine.project_root

configured = plugin.TraceDecayContextEngine(config={"project_root": "/from/config"})
assert configured.project_root == "/from/config", configured.project_root

explicit = plugin.TraceDecayContextEngine(config={})
explicit.on_session_start(session_id="s2", project_root="/explicit/root")
assert explicit.project_root == "/explicit/root", explicit.project_root
"#
        ),
    )
    .unwrap();

    let mut check = python_command();
    check
        .arg(&script)
        .arg(&plugin_dir)
        // Reading the config-block pin requires a yaml module; the script
        // falls back to this shim only when the real PyYAML is missing.
        .arg(write_pyyaml_shim(home.path()))
        // expanduser reads HOME on POSIX and USERPROFILE on Windows.
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env_remove("HERMES_HOME");
    let output = check
        .output()
        .expect("python3 should run generated Hermes plugin check");
    assert!(
        output.status.success(),
        "generated plugin must ignore legacy profile project-root pins\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
