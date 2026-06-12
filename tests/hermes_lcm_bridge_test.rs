mod common;

use std::path::Path;
use std::process::Command;

use common::pyyaml_shim_pythonpath;
use tempfile::TempDir;
use tracedecay::agents::{AgentIntegration, HermesIntegration, InstallContext};
use tracedecay::sessions::lcm::{LcmCompressionRequest, LcmSummarizerMode};

const PLUGIN_LOAD_PRELUDE: &str = r#"
import importlib.machinery
import importlib.util
import os
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
# Hermetic profile home: the generated code reads plugins.tracedecay from
# {HERMES_HOME}/config.yaml, so point it at the temp install instead of the
# developer's real ~/.hermes.
os.environ["HERMES_HOME"] = str(plugin_dir.parent.parent)
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

fn make_install_ctx(home: &Path) -> InstallContext {
    InstallContext {
        home: home.to_path_buf(),
        tracedecay_bin: "/usr/local/bin/tracedecay".to_string(),
        tool_permissions: Vec::new(),
        profile: None,
        project_root: None,
        dashboard: true,
    }
}

fn assert_python_compiles(paths: &[&Path]) {
    let output = Command::new("python3")
        .arg("-m")
        .arg("py_compile")
        .args(paths)
        .output()
        .expect("python3 should be available for Hermes generated Python syntax checks");
    assert!(
        output.status.success(),
        "generated Python should compile\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_generated_plugin_script(script_name: &str, script: &str, failure_message: &str) {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let script_path = plugin_dir.join(script_name);
    let script = format!("{PLUGIN_LOAD_PRELUDE}\n{script}");
    std::fs::write(&script_path, script).unwrap();

    let output = Command::new("python3")
        .arg(&script_path)
        .arg(plugin_dir)
        // Isolate from the developer's real ~/.hermes: the generated plugin
        // resolves HERMES_HOME → ~/.hermes at runtime, and a real host
        // config.yaml pin would override the behavior under test.
        .env("HOME", home.path())
        .env_remove("HERMES_HOME")
        .output()
        .expect("python3 should run generated Hermes plugin check");
    assert!(
        output.status.success(),
        "{failure_message}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
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

assert [name for name, _ in ctx.hooks] == ["pre_llm_call"]
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
assert [name for name, _ in ctx.hooks] == ["pre_llm_call"]
assert len(ctx.memory_providers) == 1
assert len(ctx.context_engines) == 1
"#,
        "generated plugin registration should continue when register_tool raises",
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
        self.tools.append(kwargs)
        raise AssertionError("register_tool should be skipped on unsafe hosts")

    def register_hook(self, name, handler):
        pass

    def register_memory_provider(self, provider):
        pass

    def register_context_engine(self, engine):
        self.context_engines.append(engine)

ctx = UnsafeRegisteredToolCtx()
plugin.register(ctx)

assert ctx.tools == []
assert len(ctx.context_engines) == 1
engine = ctx.context_engines[0]
assert engine.name == "tracedecay"
assert "lcm_grep" in {schema["name"] for schema in engine.get_tool_schemas()}
"#,
        "generated plugin should skip direct tools when host does not forward messages",
    );
}

#[test]
fn generated_context_engine_exposes_native_lcm_surface_and_dispatch() {
    run_generated_plugin_script(
        "check_context_engine_native_surface.py",
        r#"
import json

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
assert status["storage_scope"] == "project_local"
assert status["context_engine_tool_names"] == sorted(schema_names)

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
assert calls[0][1]["storage_scope"] == "project_local"
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
assert calls[1][1]["storage_scope"] == "project_local"
assert calls[1][1]["project_root"] == "/tmp/project"
assert calls[1][1]["session_id"] == "session-1"
assert calls[1][2] == {}
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
assert calls[6][1]["source_offset"] == 3
assert calls[6][1]["source_limit"] == 2
assert "store_id" not in calls[6][1]
assert "max_tokens" not in calls[6][1]
assert calls[7][0] == "tracedecay_lcm_grep"
assert calls[7][1]["query"] == "direct"
assert calls[7][1]["scope"] == "all"
assert "session_scope" not in calls[7][1]
assert calls[7][1]["storage_scope"] == "project_local"
assert calls[7][1]["project_root"] == "/tmp/project"
assert calls[7][1]["session_id"] == "session-1"
assert calls[8][0] == "tracedecay_lcm_grep"
assert calls[8][1]["query"] == "implicit"
assert calls[8][1]["scope"] == "current"
assert "session_scope" not in calls[8][1]
assert calls[8][1]["storage_scope"] == "project_local"
assert calls[8][1]["project_root"] == "/tmp/project"
assert calls[8][1]["session_id"] == "session-1"
"#,
        "generated context engine should expose Hermes-style native LCM surface",
    );
}

#[test]
fn generated_context_engine_uses_env_hermes_home_for_profile_storage() {
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

engine = plugin.TraceDecayContextEngine()
engine.initialize(session_id="session-1")
assert engine.hermes_home == "/tmp/hermes-from-env"
status = engine.get_status()
assert status["storage_scope"] == "hermes_profile"
assert status["hermes_home"] == "/tmp/hermes-from-env"

engine.handle_tool_call(
    "lcm_grep",
    {"query": "orchard"},
    messages=[{"role": "user", "content": "profile current turn"}],
)

assert calls[0][0] == "tracedecay_lcm_preflight"
assert calls[0][1]["storage_scope"] == "hermes_profile"
assert calls[0][1]["hermes_home"] == "/tmp/hermes-from-env"
assert calls[0][1]["messages"] == [{"role": "user", "content": "profile current turn"}]
assert calls[1][0] == "tracedecay_lcm_grep"
assert calls[1][1]["storage_scope"] == "hermes_profile"
assert calls[1][1]["hermes_home"] == "/tmp/hermes-from-env"
"#,
        "generated context engine should resolve HERMES_HOME for profile storage",
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

result = engine.compress([{"role": "user", "content": "current"}], current_tokens=700)

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

result = engine.compress([{"role": "user", "content": "current"}], current_tokens=700)
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
fn generated_context_engine_defaults_to_hermes_home_even_when_missing() {
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
    expected = str(home / ".hermes")
    assert not pathlib.Path(expected).exists()

    engine = plugin.TraceDecayContextEngine()
    engine.initialize(session_id="session-1")

    def normalized(path):
        # Windows expanduser("~/.hermes") emits mixed separators for the same
        # location; normalize separators only there so Unix stays byte-exact.
        return os.path.normpath(path) if os.name == "nt" else path

    assert normalized(engine.hermes_home) == normalized(expected), engine.hermes_home
    status = engine.get_status()
    assert status["storage_scope"] == "hermes_profile"
    assert normalized(status["hermes_home"]) == normalized(expected), status
"#,
        "generated context engine should default to ~/.hermes even if missing",
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
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let script = plugin_dir.join("check_context_engine.py");
    std::fs::write(
        &script,
        r#"
import importlib.machinery
import importlib.util
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

engine.initialize(
    session_id="session-123",
    hermes_home="/tmp/hermes-profile",
    project_root="/tmp/project",
)
assert engine.active_session_id == "session-123"
assert engine.hermes_home == "/tmp/hermes-profile"
assert engine.project_root == "/tmp/project"

local_args = plugin._storage_args(project_root="/tmp/project", hermes_home="/tmp/hermes-profile")
assert local_args == {
    "storage_scope": "project_local",
    "project_root": "/tmp/project",
}

profile_args = plugin._storage_args(hermes_home="/tmp/hermes-profile")
assert profile_args == {
    "storage_scope": "hermes_profile",
    "hermes_home": "/tmp/hermes-profile",
}

fallback_args = plugin._storage_args()
assert fallback_args == {"storage_scope": "hermes_profile"}

calls = []

def fake_call_tracedecay_tool(name, args, **kwargs):
    calls.append((name, args, kwargs))
    return "{}"

plugin.tools.call_tracedecay_tool = fake_call_tracedecay_tool

profile_engine = plugin.TraceDecayContextEngine()
profile_engine.on_session_start(session_id="session-1", hermes_home="/tmp/hermes")
profile_engine.should_compress_preflight(messages=[], current_tokens=123)
name, args, kwargs = calls.pop()
assert name == "tracedecay_lcm_preflight"
assert args["session_id"] == "session-1"
assert args["storage_scope"] == "hermes_profile"
assert args["hermes_home"] == "/tmp/hermes"

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
assert args["storage_scope"] == "project_local"
assert args["project_root"] == "/tmp/project"

project_engine = plugin.TraceDecayContextEngine()
project_engine.initialize(session_id="initial", project_root="/tmp/project")
project_engine.on_session_start(session_id="next")
project_engine.should_compress_preflight(messages=[], current_tokens=789)
name, args, kwargs = calls.pop()
assert name == "tracedecay_lcm_preflight"
assert args["session_id"] == "next"
assert args["storage_scope"] == "project_local"
assert args["project_root"] == "/tmp/project"

profile_engine = plugin.TraceDecayContextEngine()
profile_engine.initialize(session_id="initial", hermes_home="/tmp/hermes")
profile_engine.on_session_start(session_id="next")
profile_engine.should_compress_preflight(messages=[], current_tokens=321)
name, args, kwargs = calls.pop()
assert name == "tracedecay_lcm_preflight"
assert args["session_id"] == "next"
assert args["storage_scope"] == "hermes_profile"
assert args["hermes_home"] == "/tmp/hermes"

class LegacyCtx:
    def register_tool(self, *args, **kwargs):
        pass

    def register_hook(self, *args, **kwargs):
        pass

legacy = LegacyCtx()
plugin.register(legacy)
"#,
    )
    .unwrap();

    let output = Command::new("python3")
        .arg(&script)
        .arg(plugin_dir)
        .env("HOME", home.path())
        .env_remove("HERMES_HOME")
        .output()
        .expect("python3 should run generated Hermes context engine check");
    assert!(
        output.status.success(),
        "generated plugin should register a Hermes context engine when supported\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn context_engine_preflight_uses_tracedecay_tool_json_args() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let script = plugin_dir.join("check_preflight_bridge.py");
    std::fs::write(
        &script,
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
result = engine.should_compress_preflight(
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
    "storage_scope": "project_local",
    "project_root": "/tmp/project",
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
    )
    .unwrap();

    let output = Command::new("python3")
        .arg(&script)
        .arg(plugin_dir)
        .env("HOME", home.path())
        .env_remove("HERMES_HOME")
        .output()
        .expect("python3 should run generated Hermes preflight bridge check");
    assert!(
        output.status.success(),
        "generated context engine should call tracedecay_lcm_preflight through the JSON bridge\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn context_engine_session_start_reports_compression_boundary() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let script = plugin_dir.join("check_session_boundary_bridge.py");
    std::fs::write(
        &script,
        r#"
import importlib.machinery
import importlib.util
import json
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
    "storage_scope": "project_local",
    "project_root": "/tmp/project",
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
    )
    .unwrap();

    let output = Command::new("python3")
        .arg(&script)
        .arg(plugin_dir)
        .env("HOME", home.path())
        .env_remove("HERMES_HOME")
        .output()
        .expect("python3 should run generated Hermes session boundary bridge check");
    assert!(
        output.status.success(),
        "generated context engine should report compression boundaries through tracedecay_lcm_session_boundary\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn context_engine_compress_uses_tracedecay_tool_json_args() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let script = plugin_dir.join("check_compress_bridge.py");
    std::fs::write(
        &script,
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
result = engine.compress(
    [{"role": "assistant", "content": "hello"}],
    current_tokens=1200,
    focus_topic="handoff",
)

assert result == {"status": "not_implemented", "message": "placeholder parsed"}

assert len(calls) == 1
argv = calls[0]
assert argv[0] == plugin.tools.TRACEDECAY_BIN
assert argv[1:6] == ["tool", "--project", "/tmp/project", "tracedecay_lcm_compress", "--json"]
args = json.loads(argv[argv.index("--args") + 1])
assert args == {
    "storage_scope": "project_local",
    "project_root": "/tmp/project",
    "fresh_tail_count": 64,
    "leaf_chunk_tokens": 20000,
    "dynamic_leaf_chunk_enabled": False,
    "dynamic_leaf_chunk_max": 40000,
    "max_assembly_tokens": 0,
    "reserve_tokens_floor": 0,
    "summary_fan_in": 4,
    "incremental_max_depth": 1,
    "session_id": "session-2",
    "messages": [{"role": "assistant", "content": "hello"}],
    "current_tokens": 1200,
    "focus_topic": "handoff",
    "summarizer": {"mode": "hermes_auxiliary"},
}
"#,
    )
    .unwrap();

    let output = Command::new("python3")
        .arg(&script)
        .arg(plugin_dir)
        .output()
        .expect("python3 should run generated Hermes compress bridge check");
    assert!(
        output.status.success(),
        "generated context engine should call tracedecay_lcm_compress through the JSON bridge\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn context_engine_projects_config_defaults_into_preflight_and_compress_args() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let script = plugin_dir.join("check_context_engine_config_defaults.py");
    std::fs::write(
        &script,
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
    )
    .unwrap();

    let output = Command::new("python3")
        .arg(&script)
        .arg(plugin_dir)
        .output()
        .expect("python3 should run generated Hermes config defaults check");
    assert!(
        output.status.success(),
        "generated context engine should project configured Hermes defaults into preflight/compress args\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn context_engine_expand_query_and_profile_storage_project_flags() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let script = plugin_dir.join("check_project_flag_bridge.py");
    std::fs::write(
        &script,
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
    return json.dumps({"content": [{"type": "text", "text": json.dumps(inner)}]})

def fake_run(argv, check, capture_output, text, timeout, shell):
    calls.append(argv)
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
assert project_argv[1:6] == ["tool", "--project", "/tmp/project", "tracedecay_lcm_expand_query", "--json"]
project_args = json.loads(project_argv[project_argv.index("--args") + 1])
assert project_args["storage_scope"] == "project_local"
assert project_args["project_root"] == "/tmp/project"

profile_engine = plugin.TraceDecayContextEngine()
profile_engine.initialize(session_id="session-2", hermes_home="/tmp/hermes-profile")
profile_result = profile_engine.should_compress_preflight(messages=[], current_tokens=100)
assert profile_result["status"] == "ok"
profile_argv = calls.pop()
assert profile_argv[0] == plugin.tools.TRACEDECAY_BIN
assert profile_argv[1:4] == ["tool", "tracedecay_lcm_preflight", "--json"]
assert "--project" not in profile_argv
profile_args = json.loads(profile_argv[profile_argv.index("--args") + 1])
assert profile_args["storage_scope"] == "hermes_profile"
assert profile_args["hermes_home"] == "/tmp/hermes-profile"

explicit = plugin.tools.call_tracedecay_tool(
    "tracedecay_lcm_status",
    {"storage_scope": "hermes_profile", "hermes_home": "/tmp/hermes-profile"},
    project_root="/tmp/project",
)
assert json.loads(explicit)["content"]
explicit_argv = calls.pop()
assert explicit_argv[1:6] == ["tool", "--project", "/tmp/project", "tracedecay_lcm_status", "--json"]
"#,
    )
    .unwrap();

    let output = Command::new("python3")
        .arg(&script)
        .arg(plugin_dir)
        .env("HOME", home.path())
        .env_remove("HERMES_HOME")
        .output()
        .expect("python3 should run generated Hermes project flag bridge check");
    assert!(
        output.status.success(),
        "generated bridge should pass project-local roots through tracedecay tool --project without affecting profile calls\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn auxiliary_summary_strips_reasoning_tags() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let script = plugin_dir.join("check_auxiliary_summary.py");
    std::fs::write(
        &script,
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
    )
    .unwrap();

    let output = Command::new("python3")
        .arg(&script)
        .arg(plugin_dir)
        .output()
        .expect("python3 should run generated Hermes auxiliary summary check");
    assert!(
        output.status.success(),
        "generated context engine should strip reasoning from auxiliary summaries\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn context_engine_expand_query_synthesizes_and_degrades() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let script = plugin_dir.join("check_expand_query_synthesis.py");
    std::fs::write(
        &script,
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
    assert args["storage_scope"] == "project_local"
    assert args["project_root"] == "/tmp/project"
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

agent.auxiliary_client.mode = "unexpected"
responses.append(needs_synthesis())
try:
    engine.expand_query(prompt="What changed?", query="orchard")
except RuntimeError as exc:
    assert "schema bug" in str(exc)
else:
    raise AssertionError("unexpected synthesis exceptions must propagate")
"#,
    )
    .unwrap();

    let output = Command::new("python3")
        .arg(&script)
        .arg(plugin_dir)
        .output()
        .expect("python3 should run generated Hermes expand-query synthesis check");
    assert!(
        output.status.success(),
        "generated context engine should synthesize and degrade expand-query answers\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn context_engine_compress_provides_auxiliary_summary_after_needs_summary() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let script = plugin_dir.join("check_auxiliary_compress_flow.py");
    std::fs::write(
        &script,
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
result = engine.compress(
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
    )
    .unwrap();

    let output = Command::new("python3")
        .arg(&script)
        .arg(plugin_dir)
        .output()
        .expect("python3 should run generated Hermes auxiliary compress flow check");
    assert!(
        output.status.success(),
        "generated context engine should provide auxiliary summaries back to Rust\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn context_engine_compress_rejects_oversized_auxiliary_summary() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let script = plugin_dir.join("check_auxiliary_oversized_fallback.py");
    std::fs::write(
        &script,
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
result = engine.compress(
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
    )
    .unwrap();

    let output = Command::new("python3")
        .arg(&script)
        .arg(plugin_dir)
        .output()
        .expect("python3 should run generated Hermes oversized auxiliary fallback check");
    assert!(
        output.status.success(),
        "generated context engine should reject oversized auxiliary summaries\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
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
result = engine.compress(
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
result = engine.compress(
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
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let script = plugin_dir.join("check_auxiliary_fallbacks.py");
    std::fs::write(
        &script,
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
    )
    .unwrap();

    let output = Command::new("python3")
        .arg(&script)
        .arg(plugin_dir)
        .output()
        .expect("python3 should run generated Hermes auxiliary fallback check");
    assert!(
        output.status.success(),
        "generated context engine should route, cooldown, and fallback auxiliary summaries\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
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
result = engine.compress(
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
result = engine.compress(
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
    os.environ["HERMES_HOME"] = tmp
    cfg = pathlib.Path(tmp) / "config.yaml"

    # Hermes compression.threshold backfills LCM when no override exists.
    cfg.write_text("compression:\n  enabled: true\n  threshold: 0.6\n")
    assert preflight_threshold({"context_length": 100000}) == 60000

    # Disabled Hermes compression must not leak its threshold into LCM.
    cfg.write_text("compression:\n  enabled: false\n  threshold: 0.9\n")
    assert preflight_threshold({"context_length": 100000}) == 75000

    # Explicit env and ctx.config thresholds still win over the YAML fallback.
    cfg.write_text("compression:\n  enabled: true\n  threshold: 0.6\n")
    os.environ["LCM_CONTEXT_THRESHOLD"] = "0.5"
    assert preflight_threshold({"context_length": 100000}) == 50000
    del os.environ["LCM_CONTEXT_THRESHOLD"]
    assert preflight_threshold({"context_length": 100000, "context_threshold": 0.8}) == 80000

with tempfile.TemporaryDirectory() as tmp:
    # No config.yaml at all: the documented 0.75 default applies whenever the
    # context window is known instead of silently disabling threshold pressure.
    os.environ["HERMES_HOME"] = tmp
    assert preflight_threshold({"context_length": 100000}) == 75000
    assert preflight_threshold(None) is None

with tempfile.TemporaryDirectory() as tmp:
    # The engine's resolved hermes_home should be used when HERMES_HOME is unset.
    os.environ.pop("HERMES_HOME", None)
    cfg = pathlib.Path(tmp) / "config.yaml"
    cfg.write_text("compression:\n  enabled: true\n  threshold: 0.55\n")
    engine = plugin.TraceDecayContextEngine(config={"context_length": 100000})
    engine.initialize(session_id="session-1", project_root="/tmp/project", hermes_home=tmp)
    args = plugin._lcm_config_args(engine.config, engine.hermes_home)
    assert args["threshold_tokens"] == 55000
"#,
        "generated plugin should fall back to the Hermes YAML compression threshold",
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

# should_compress mirrors post-response gating and returns a bool.
assert engine.should_compress(prompt_tokens=1) is True
"#,
        "generated plugin should propagate update_model/session_start context windows into preflight/compress",
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

# Hosts that pass no config keep the single task-default route when Hermes
# home has no config.yaml timeout override.
with tempfile.TemporaryDirectory() as tmp:
    os.environ["HERMES_HOME"] = tmp
    bare_engine = plugin.TraceDecayContextEngine()
    bare_engine.initialize(session_id="session-1", project_root="/tmp/project")
    bare_routes = bare_engine._auxiliary_routes()
    assert len(bare_routes) == 1
    assert "model" not in bare_routes[0]
    assert bare_routes[0]["timeout"] == 60.0
    os.environ.pop("HERMES_HOME", None)

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
fn call_tracedecay_json_normalizes_malformed_mcp_envelopes() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let script = plugin_dir.join("check_bridge_error_normalization.py");
    std::fs::write(
        &script,
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
"#,
    )
    .unwrap();

    let output = Command::new("python3")
        .arg(&script)
        .arg(plugin_dir)
        .output()
        .expect("python3 should run generated Hermes bridge error normalization check");
    assert!(
        output.status.success(),
        "generated JSON bridge should normalize malformed MCP envelopes\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
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
import stat
import sys

plugin_dir = pathlib.Path(sys.argv[1])
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

def write_fake_binary(name, body_posix, body_windows):
    if os.name == "nt":
        path = plugin_dir / f"{name}.cmd"
        path.write_text("@echo off\n" + body_windows)
        return str(path)
    path = plugin_dir / name
    path.write_text('#!/bin/sh\n' + body_posix)
    path.chmod(path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return str(path)

# Missing binary: the OSError is wrapped, never raised.
tools.TRACEDECAY_BIN = str(plugin_dir / "definitely-missing-tracedecay")
missing = json.loads(tools.call_tracedecay_tool("tracedecay_lcm_status", {}))
assert missing["error"].startswith("tracedecay tool failed:"), missing
# The JSON bridge surfaces the same error dict without raising.
assert "error" in plugin.call_tracedecay_json("tracedecay_lcm_status", {})

# Subprocess dies mid-handshake: nonzero exit with partial stdout and stderr
# is reported with the exit status and bounded captures.
# cmd.exe `echo` always appends a newline that POSIX printf does not, and a
# plain `echo text 1>&2` would emit a trailing space before the redirect —
# hence the redirect-first form (`>&2 echo`). Trim trailing newlines only on
# Windows so Unix keeps byte-exact capture assertions.
def trim_capture(text):
    return text.rstrip("\r\n") if os.name == "nt" else text

tools.TRACEDECAY_BIN = write_fake_binary(
    "fake-tracedecay-crash",
    'printf \'{"content\'\nprintf \'handshake aborted\' >&2\nexit 3\n',
    'echo {"content\n>&2 echo handshake aborted\nexit /b 3\n',
)
crashed = json.loads(tools.call_tracedecay_tool("tracedecay_lcm_status", {}))
assert crashed["error"] == "tracedecay tool exited with status 3", crashed
assert trim_capture(crashed["stdout"]) == '{"content', crashed
assert trim_capture(crashed["stderr"]) == "handshake aborted", crashed

# Exit 0 with malformed JSON on stdout.
tools.TRACEDECAY_BIN = write_fake_binary(
    "fake-tracedecay-badjson",
    "printf 'not-json-at-all'\nexit 0\n",
    "echo not-json-at-all\nexit /b 0\n",
)
malformed = json.loads(tools.call_tracedecay_tool("tracedecay_lcm_status", {}))
assert malformed["error"] == "tracedecay tool returned invalid JSON", malformed
assert trim_capture(malformed["stdout"]) == "not-json-at-all", malformed

# Exit 0 with empty stdout normalizes to an empty JSON object.
tools.TRACEDECAY_BIN = write_fake_binary("fake-tracedecay-empty", "exit 0\n", "exit /b 0\n")
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
preflight = engine.should_compress_preflight(messages, current_tokens=128)
assert isinstance(preflight, dict), preflight
assert preflight["error"].startswith("tracedecay tool failed:"), preflight

status = engine.status()
assert isinstance(status, dict)
assert status["error"].startswith("tracedecay tool failed:"), status

compressed = engine.compress(messages, current_tokens=128)
assert isinstance(compressed, dict), compressed
assert compressed["error"].startswith("tracedecay tool failed:"), compressed

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
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    let script = plugin_dir.join("check_update_from_response.py");
    std::fs::write(
        &script,
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
    )
    .unwrap();

    let output = Command::new("python3")
        .arg(&script)
        .arg(&plugin_dir)
        .output()
        .expect("python3 should run generated Hermes plugin check");
    assert!(
        output.status.success(),
        "generated engine must satisfy the abstract update_from_response contract\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
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

/// Profiles can pin the indexed project via a `project_root` config key:
/// explicit kwargs win, then config, with the session cwd as last fallback.
#[test]
fn generated_context_engine_resolves_project_root_from_config() {
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
plugin.register(ctx)
engine = ctx.context_engines[0]

# Config pin applies at registration time.
assert engine.project_root == "/tmp/pinned-project", engine.project_root

# A session cwd does NOT override the config pin...
engine.on_session_start(session_id="s1", cwd="/somewhere/else")
assert engine.project_root == "/tmp/pinned-project", engine.project_root

# ...but an explicit kwargs project_root does.
engine.on_session_start(session_id="s2", project_root="/explicit/root")
assert engine.project_root == "/explicit/root", engine.project_root

# Without a pin or explicit root, cwd remains the fallback.
unpinned = plugin.TraceDecayContextEngine(config={})
assert unpinned.project_root is None
unpinned.on_session_start(session_id="s3", cwd="/cwd/fallback")
assert unpinned.project_root == "/cwd/fallback", unpinned.project_root
"#,
        "generated engine must honor the project_root config pin (kwargs > config > cwd)",
    );
}

/// The install-time `--project-root` pin must flow through both dispatch
/// layers: `tools.call_tracedecay_tool` adds `--project <pin>` to every CLI
/// invocation that has no explicit project, and the context engine resolves
/// `project_root` as kwargs > config > pin > cwd.
#[test]
fn generated_plugin_honors_install_time_project_root_pin() {
    let home = TempDir::new().unwrap();
    let ctx = InstallContext {
        home: home.path().to_path_buf(),
        tracedecay_bin: "/usr/local/bin/tracedecay".to_string(),
        tool_permissions: Vec::new(),
        profile: None,
        project_root: Some(std::path::PathBuf::from("/pinned/project")),
        dashboard: true,
    };
    HermesIntegration.install(&ctx).unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let script = plugin_dir.join("check_project_root_pin.py");
    std::fs::write(
        &script,
        r#"
import importlib.machinery
import importlib.util
import json
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
# The pin lives in the profile config.yaml (plugins.tracedecay.project_root);
# the generated tools.py carries no pin constant.
assert not hasattr(tools, "PINNED_PROJECT_ROOT")
assert tools.config_pinned_project_root() == "/pinned/project", tools.config_pinned_project_root()

# Every CLI dispatch picks up the pin when no explicit project is given.
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
assert argv[idx + 1] == "/pinned/project", argv

# An explicit project still wins over the pin.
tools.call_tracedecay_tool("tracedecay_status", {}, project_root="/explicit/root")
argv = captured[-1]
idx = argv.index("--project")
assert argv[idx + 1] == "/explicit/root", argv

# Engine resolution: pin applies by default, config beats pin, kwargs beat
# config, and cwd no longer overrides any pin.
engine = plugin.TraceDecayContextEngine(config={})
assert engine.project_root == "/pinned/project", engine.project_root
engine.on_session_start(session_id="s1", cwd="/somewhere/else")
assert engine.project_root == "/pinned/project", engine.project_root

configured = plugin.TraceDecayContextEngine(config={"project_root": "/from/config"})
assert configured.project_root == "/from/config", configured.project_root

explicit = plugin.TraceDecayContextEngine(config={})
explicit.on_session_start(session_id="s2", project_root="/explicit/root")
assert explicit.project_root == "/explicit/root", explicit.project_root
"#,
    )
    .unwrap();

    let mut check = Command::new("python3");
    check
        .arg(&script)
        .arg(&plugin_dir)
        // expanduser reads HOME on POSIX and USERPROFILE on Windows.
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env_remove("HERMES_HOME");
    // Reading the config-block pin requires a yaml module.
    if let Some(shim_dir) = pyyaml_shim_pythonpath(home.path()) {
        check.env("PYTHONPATH", shim_dir);
    }
    let output = check
        .output()
        .expect("python3 should run generated Hermes plugin check");
    assert!(
        output.status.success(),
        "generated plugin must honor the install-time project-root pin\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
