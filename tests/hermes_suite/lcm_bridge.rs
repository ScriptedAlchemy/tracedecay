use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::{Duration, Instant, SystemTime};

use tempfile::TempDir;
use tracedecay::agents::host_bundle_registry::verified_embedded_host_component_set_with_tracedecay_bin;
use tracedecay::agents::host_bundle_v2::{HostBundleComponentV1, HostKindV1};
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

/// One unpinned Hermes install shared by every check.
///
/// The embedded host catalog regenerates the full plugin from embedded
/// templates, so the output is identical for every unpinned install — there
/// is no reason to redo it per test. Each check writes its own uniquely
/// named script into the shared plugin dir and only mutates state inside its
/// own python interpreter, so tests stay independent.
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
    // The rendered plugin's schemas.json and JSON_FORMAT_TOOLS come from the
    // root MCP catalog ports, which `main` wires at process startup; a test
    // process must wire them itself or the bundle renders empty tool sets.
    static PORTS: std::sync::Once = std::sync::Once::new();
    PORTS.call_once(tracedecay::register_runtime_ports);
    let component_set = verified_embedded_host_component_set_with_tracedecay_bin(
        HostKindV1::Hermes,
        &[HostBundleComponentV1::Core],
        0,
        FIXTURE_TRACEDECAY_BIN,
    )
    .map_err(std::io::Error::other)?;
    for component in component_set.component_set.components {
        for artifact in component.contents {
            let path = home.join(artifact.relative_path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, artifact.bytes)?;
        }
    }
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

assert [name for name, _ in ctx.hooks] == ["pre_llm_call", "post_tool_call"]
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
assert [name for name, _ in ctx.hooks] == ["pre_llm_call", "post_tool_call"]
assert len(ctx.memory_providers) == 1
assert len(ctx.context_engines) == 1
"#,
        "generated plugin registration should continue when register_tool raises",
    );
}

#[test]
fn generated_native_callbacks_forward_content_free_terminal_receipts() {
    run_generated_plugin_script(
        "check_native_hook_forwarding.py",
        r#"
import copy

captured = []
plugin._notify_host_receipt = (
    lambda event, thread_name: captured.append((copy.deepcopy(event), thread_name)) or True
)
plugin._code_project_root = lambda **_kwargs: "/workspace/project"

# Non-terminal tools never produce receipts: transcript content flows through
# the daemon's authenticated ingest route, not host callbacks.
plugin._post_tool_call(
    tool_name="write_file",
    session_id="session-hermes-native",
    tool_input={"path": "/workspace/project/secret.txt", "content": "secret"},
    cwd="/workspace/project",
)
assert captured == []

plugin._post_tool_call(
    tool_name="bash",
    session_id="session-hermes-native",
    thread_id="thread-hermes-native",
    turn_id="turn-hermes-native",
    tool_call_id="call-hermes-native",
    args={"command": "cat /workspace/project/secret.txt"},
    cwd="/workspace/project",
    status="success",
    duration_ms=42,
)

assert len(captured) == 1, captured
receipt_event, receipt_thread = captured[0]
assert receipt_thread == "tracedecay-terminal-receipt"
assert receipt_event == {
    "agent": "hermes",
    "event": "terminalReceipt",
    "_project_candidate": "/workspace/project",
    "_hermes_home": None,
    "_trusted_project": False,
    "route": {
        "session_id": "session-hermes-native",
        "thread_id": "thread-hermes-native",
    },
    "receipt": {
        "tool_call_id": "call-hermes-native",
        "turn_id": "turn-hermes-native",
        "status": "success",
        "duration_ms": 42,
        "transcript_watermark": "turn-hermes-native",
    },
}
assert "secret.txt" not in repr(captured)
assert "cat " not in repr(captured)

# The engine's session-end boundary drops per-session state without emitting
# a host receipt; the daemon owns any stop-side transcript work.
engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-hermes-native", project_root="/workspace/project")
assert engine.active_session_id == "session-hermes-native"
engine.on_session_end(session_id="session-hermes-native")
assert engine.active_session_id is None
assert len(captured) == 1

class Ctx:
    def __init__(self):
        self.hooks = []
    def register_hook(self, name, handler):
        self.hooks.append((name, handler))

ctx = Ctx()
plugin.register(ctx)
assert [name for name, _ in ctx.hooks] == ["pre_llm_call", "post_tool_call"]
"#,
        "generated Hermes callbacks must forward terminal receipt identity without content",
    );
}

#[test]
fn generated_native_callback_queue_applies_explicit_backpressure() {
    run_generated_plugin_script(
        "check_native_hook_queue_bound.py",
        r#"
import threading as real_threading

class DormantThread:
    def __init__(self, *args, **kwargs):
        pass
    def start(self):
        pass

# plugin.threading is the shared real module; keep a handle to the real
# Thread class before patching it out for the plugin's worker spawn.
RealThread = real_threading.Thread
plugin.threading.Thread = DormantThread
plugin._HOST_RECEIPT_QUEUE.clear()
plugin._HOST_RECEIPT_WORKER = None

for sequence in range(plugin._HOST_RECEIPT_QUEUE_LIMIT):
    plugin._notify_host_receipt({"sequence": sequence}, "test-native-hook")

assert len(plugin._HOST_RECEIPT_QUEUE) == plugin._HOST_RECEIPT_QUEUE_LIMIT

admitted = real_threading.Event()

def overflow_producer():
    plugin._notify_host_receipt(
        {"sequence": plugin._HOST_RECEIPT_QUEUE_LIMIT}, "test-native-hook"
    )
    admitted.set()

producer = RealThread(target=overflow_producer, daemon=True)
producer.start()

# A producer at capacity must block -- native events are never dropped and
# admitted events are never evicted while the queue is full.
assert not admitted.wait(0.5)
assert len(plugin._HOST_RECEIPT_QUEUE) == plugin._HOST_RECEIPT_QUEUE_LIMIT
assert [event["sequence"] for event in plugin._HOST_RECEIPT_QUEUE] == list(
    range(plugin._HOST_RECEIPT_QUEUE_LIMIT)
)

with plugin._HOST_RECEIPT_QUEUE_CONDITION:
    drained = plugin._HOST_RECEIPT_QUEUE.popleft()
    plugin._HOST_RECEIPT_QUEUE_CONDITION.notify_all()
assert drained["sequence"] == 0

assert admitted.wait(5), "blocked producer must resume once capacity frees"
producer.join(5)
assert [event["sequence"] for event in plugin._HOST_RECEIPT_QUEUE] == list(
    range(1, plugin._HOST_RECEIPT_QUEUE_LIMIT + 1)
)

# Restore the real Thread and drop the dormant worker so the plugin's
# atexit join sees a clean queue instead of the stub.
plugin.threading.Thread = RealThread
with plugin._HOST_RECEIPT_QUEUE_CONDITION:
    plugin._HOST_RECEIPT_QUEUE.clear()
    plugin._HOST_RECEIPT_WORKER = None
    plugin._HOST_RECEIPT_QUEUE_CONDITION.notify_all()
"#,
        "generated Hermes callback queue must block producers at capacity without dropping or evicting admitted events",
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
assert "source_offset" not in expand_params["properties"]
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
# Read tools dispatch straight to the daemon-owned CLI route: no local
# preflight interception, and current-turn messages never leave the host.
assert len(calls) == 7
assert calls[0][0] == "tracedecay_lcm_grep"
assert calls[0][1]["query"] == "orchard"
assert calls[0][1]["scope"] == "current"
assert calls[0][1]["sort"] == "relevance"
assert calls[0][1]["source"] == "cli"
assert calls[0][1]["role"] == "assistant"
assert calls[0][1]["start_time"] == 1
assert calls[0][1]["end_time"] == 2
assert "session_scope" not in calls[0][1]
assert "time_from" not in calls[0][1]
assert "time_to" not in calls[0][1]
assert "messages" not in calls[0][1]
assert "project_root" not in calls[0][1]
assert calls[0][1]["session_id"] == "session-1"
assert calls[0][2] == {"project_root": "/tmp/project"}
assert calls[1][0] == "tracedecay_lcm_load_session"
assert calls[1][1]["content_limit"] == 123
assert calls[1][1]["roles"] == ["user", "tool"]
assert calls[1][1]["start_time"] == 1
assert calls[1][1]["end_time"] == 2
assert "max_content_chars" not in calls[1][1]
assert "role" not in calls[1][1]
assert "time_from" not in calls[1][1]
assert "time_to" not in calls[1][1]
assert "messages" not in calls[1][1]
assert calls[2][0] == "tracedecay_lcm_describe"
assert calls[2][1]["target"] == {"kind": "summary_node", "node_id": "7"}
assert "node_id" not in calls[2][1]
assert calls[3][0] == "tracedecay_lcm_describe"
assert calls[3][1]["target"] == {"kind": "external_payload", "payload_ref": "payload_123.payload"}
assert "externalized_ref" not in calls[3][1]
assert calls[4][0] == "tracedecay_lcm_expand"
assert calls[4][1]["target"] == {"kind": "raw_message", "store_id": 42}
assert calls[4][1]["session_id"] == "session-foreign"
assert calls[4][1]["content_limit"] == 308
assert "source_offset" not in calls[4][1]
assert "source_limit" not in calls[4][1]
assert "store_id" not in calls[4][1]
assert "max_tokens" not in calls[4][1]
assert calls[5][0] == "tracedecay_lcm_grep"
assert calls[5][1]["query"] == "direct"
assert calls[5][1]["scope"] == "all"
assert "session_scope" not in calls[5][1]
assert "project_root" not in calls[5][1]
assert calls[5][2] == {"project_root": "/tmp/project"}
assert calls[5][1]["session_id"] == "session-1"
assert calls[6][0] == "tracedecay_lcm_grep"
assert calls[6][1]["query"] == "implicit"
assert calls[6][1]["scope"] == "current"
assert "session_scope" not in calls[6][1]
assert "project_root" not in calls[6][1]
assert calls[6][2] == {"project_root": "/tmp/project"}
assert calls[6][1]["session_id"] == "session-1"
"#,
        "generated context engine should expose Hermes-style native LCM surface",
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

assert len(calls) == 1
assert calls[0][0] == "tracedecay_lcm_grep"
assert "project_root" not in calls[0][1]
assert calls[0][1]["storage_scope"] == "user"
assert calls[0][2] == {}
assert "messages" not in calls[0][1]

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
assert profile_engine.should_compress_preflight(messages=[], current_tokens=123) is False

project_engine = plugin.TraceDecayContextEngine()
project_engine.on_session_start(
    session_id="session-2",
    hermes_home="/tmp/hermes",
    project_root="/tmp/project",
)
assert project_engine.should_compress_preflight(messages=[], current_tokens=456) is False

project_engine = plugin.TraceDecayContextEngine()
project_engine.initialize(session_id="initial", project_root="/tmp/project")
project_engine.on_session_start(session_id="next", project_root="/tmp/project")
assert project_engine.should_compress_preflight(messages=[], current_tokens=789) is False

profile_engine = plugin.TraceDecayContextEngine()
profile_engine.initialize(session_id="initial", hermes_home="/tmp/hermes")
profile_engine.on_session_start(session_id="next")
assert profile_engine.should_compress_preflight(messages=[], current_tokens=321) is False
assert calls == []

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
fn memory_provider_sync_turn_assigns_unique_fallback_message_ids() {
    run_generated_plugin_script(
        "check_sync_turn_unique_ids.py",
        r#"
calls = []

def fake_call_tracedecay_json(name, args, **kwargs):
    calls.append((name, args, kwargs))
    return {"status": "committed"}

plugin.call_tracedecay_json = fake_call_tracedecay_json
plugin._project_scope_resolution = lambda root, *_args: ("registered", str(root))

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
    assert user_call[0] == "tracedecay_hook_runtime"
    assert user_call[1]["action"] == "ingest_transcript"
    assert user_call[1]["storage_scope"] == "user"
    assert user_call[2] == {}
    assert project_call[0] == "tracedecay_hook_runtime"
    assert project_call[1]["action"] == "ingest_transcript"
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

# Without an explicit or registered project identity the provider must stay
# user-scoped even though root resolution consults the working directory.
plugin._project_scope_resolution = lambda root, *_args: ("unregistered", None)
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

def fake_call_tracedecay_json(name, args, **kwargs):
    calls.append((name, dict(args), dict(kwargs)))
    return {"status": "committed"}

plugin.call_tracedecay_json = fake_call_tracedecay_json
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
assert "source_offset" not in summary
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
    return plugin.call_tracedecay_json("tracedecay_lcm_status", {})

missing_content = call_with_outer({})
assert missing_content["error"] == "tracedecay tool response missing text content"

empty_content = call_with_outer({"content": []})
assert empty_content["error"] == "tracedecay tool response missing text content"

non_text_content = call_with_outer({"content": [{"type": "text", "text": 123}]})
assert non_text_content["error"] == "tracedecay tool response missing text content"

responses.append(json.dumps({"content": [{"type": "text", "text": "{not json"}]}))
invalid_nested_json = plugin.call_tracedecay_json("tracedecay_lcm_status", {})
assert invalid_nested_json["error"] == "tracedecay tool returned invalid nested JSON"

outer_error = {"error": "tool failed", "code": "boom", "content": []}
assert call_with_outer(outer_error) == outer_error

calls = []

def envelope(payload):
    return json.dumps({"content": [{"type": "text", "text": json.dumps(payload)}]})

def fake_retrieve_call(name, args, **kwargs):
    calls.append((name, args, kwargs))
    if name == "tracedecay_lcm_status":
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
retrieved = plugin.call_tracedecay_json("tracedecay_lcm_status", {}, project_root="/tmp/project")
assert retrieved == {"should_compress": True, "source": "retrieved"}
assert [call[0] for call in calls] == ["tracedecay_lcm_status", "tracedecay_retrieve"]

retrieved_fact = plugin.call_tracedecay_json("tracedecay_fact_store", {})
assert retrieved_fact == {"count": 1, "facts": [{"fact": {"content": "retrieved fact"}}]}
assert [call[0] for call in calls] == [
    "tracedecay_lcm_status",
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
split_content = plugin.call_tracedecay_json("tracedecay_lcm_status", {})
assert split_content == {"status": "ok", "source": "split-content"}

nested_payload = {"content": json.dumps({"status": "ok", "source": "nested-content"})}
responses.append(envelope(nested_payload))
nested_content = plugin.call_tracedecay_json("tracedecay_lcm_status", {})
assert nested_content == {"status": "ok", "source": "nested-content"}

response_handle_calls = []

def fake_response_handle_call(name, args, **kwargs):
    response_handle_calls.append((name, args, kwargs))
    if name == "tracedecay_lcm_status":
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
response_handle_payload = plugin.call_tracedecay_json("tracedecay_lcm_status", {})
assert response_handle_payload == {"status": "ok", "source": "response-handle"}
assert [call[0] for call in response_handle_calls] == [
    "tracedecay_lcm_status",
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
