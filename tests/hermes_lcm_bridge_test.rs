use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use tokensave::agents::{AgentIntegration, HermesIntegration, InstallContext};
use tokensave::sessions::lcm::{LcmCompressionRequest, LcmSummarizerMode};

fn make_install_ctx(home: &Path) -> InstallContext {
    InstallContext {
        home: home.to_path_buf(),
        tokensave_bin: "/usr/local/bin/tokensave".to_string(),
        tool_permissions: Vec::new(),
        profile: None,
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

    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let script_path = plugin_dir.join(script_name);
    std::fs::write(&script_path, script).unwrap();

    let output = Command::new("python3")
        .arg(&script_path)
        .arg(plugin_dir)
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
import importlib.machinery
import importlib.util
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_no_register_tool"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tokensave"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

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
assert isinstance(ctx.context_engines[0], plugin.TokenSaveContextEngine)
"#,
        "generated plugin registration should continue when host lacks register_tool",
    );
}

#[test]
fn generated_registration_continues_when_register_tool_raises() {
    run_generated_plugin_script(
        "check_registration_register_tool_raises.py",
        r#"
import importlib.machinery
import importlib.util
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_register_tool_raises"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tokensave"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

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
import importlib.machinery
import importlib.util
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_registration_gate"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tokensave"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

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
assert engine.name == "lcm"
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
import importlib.machinery
import importlib.util
import json
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_context_surface"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tokensave"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

engine = plugin.TokenSaveContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project")

assert engine.name == "lcm"

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
assert "tokensave_lcm_preflight" not in schema_names
assert "tokensave_lcm_compress" not in schema_names
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
assert "target" not in expand_params["properties"]
assert expand_params.get("required") == []

status_params = schemas_by_name["lcm_status"]["parameters"]
doctor_params = schemas_by_name["lcm_doctor"]["parameters"]
assert status_params["properties"] == {}
assert doctor_params["properties"] == {}

status = engine.get_status()
assert status["engine"] == "lcm"
assert status["session_id"] == "session-1"
assert status["storage_scope"] == "project_local"
assert status["context_engine_tool_names"] == sorted(schema_names)

calls = []

def fake_call_tokensave_tool(name, args, **kwargs):
    calls.append((name, args, kwargs))
    return json.dumps({"ok": True, "tool": name})

plugin.tools.call_tokensave_tool = fake_call_tokensave_tool

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
expand_result = engine.handle_tool_call("lcm_expand", {"store_id": 42, "max_tokens": 77})
direct_result = engine.handle_tool_call("tokensave_lcm_grep", {"query": "direct", "session_scope": "all"})
implicit_current_result = engine.handle_tool_call("lcm_grep", {"query": "implicit"})

assert json.loads(native_result) == {"ok": True, "tool": "tokensave_lcm_grep"}
assert json.loads(load_result) == {"ok": True, "tool": "tokensave_lcm_load_session"}
assert json.loads(describe_node_result) == {"ok": True, "tool": "tokensave_lcm_describe"}
assert json.loads(describe_payload_result) == {"ok": True, "tool": "tokensave_lcm_describe"}
assert json.loads(expand_result) == {"ok": True, "tool": "tokensave_lcm_expand"}
assert json.loads(direct_result) == {"ok": True, "tool": "tokensave_lcm_grep"}
assert json.loads(implicit_current_result) == {"ok": True, "tool": "tokensave_lcm_grep"}
assert calls[0][0] == "tokensave_lcm_preflight"
assert calls[0][1]["messages"] == [{"role": "user", "content": "current turn"}]
assert calls[0][1]["session_id"] == "session-1"
assert calls[0][1]["storage_scope"] == "project_local"
assert calls[1][0] == "tokensave_lcm_grep"
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
assert calls[2][0] == "tokensave_lcm_preflight"
assert calls[2][1]["messages"] == [{"role": "assistant", "content": "load turn"}]
assert calls[3][0] == "tokensave_lcm_load_session"
assert calls[3][1]["content_limit"] == 123
assert calls[3][1]["roles"] == ["user", "tool"]
assert calls[3][1]["start_time"] == 1
assert calls[3][1]["end_time"] == 2
assert "max_content_chars" not in calls[3][1]
assert "role" not in calls[3][1]
assert "time_from" not in calls[3][1]
assert "time_to" not in calls[3][1]
assert calls[4][0] == "tokensave_lcm_describe"
assert calls[4][1]["target"] == {"kind": "summary_node", "node_id": "7"}
assert "node_id" not in calls[4][1]
assert calls[5][0] == "tokensave_lcm_describe"
assert calls[5][1]["target"] == {"kind": "external_payload", "payload_ref": "payload_123.payload"}
assert "externalized_ref" not in calls[5][1]
assert calls[6][0] == "tokensave_lcm_expand"
assert calls[6][1]["target"] == {"kind": "raw_message", "store_id": 42}
assert calls[6][1]["content_limit"] == 308
assert "store_id" not in calls[6][1]
assert "max_tokens" not in calls[6][1]
assert calls[7][0] == "tokensave_lcm_grep"
assert calls[7][1]["query"] == "direct"
assert calls[7][1]["scope"] == "all"
assert "session_scope" not in calls[7][1]
assert calls[7][1]["storage_scope"] == "project_local"
assert calls[7][1]["project_root"] == "/tmp/project"
assert calls[7][1]["session_id"] == "session-1"
assert calls[8][0] == "tokensave_lcm_grep"
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

module_name = f"{parent_name}.tokensave"
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

def fake_call_tokensave_tool(name, args, **kwargs):
    calls.append((name, args, kwargs))
    return json.dumps({"content": [{"type": "text", "text": json.dumps({"status": "ok"})}]})

plugin.tools.call_tokensave_tool = fake_call_tokensave_tool

engine = plugin.TokenSaveContextEngine()
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

assert calls[0][0] == "tokensave_lcm_preflight"
assert calls[0][1]["storage_scope"] == "hermes_profile"
assert calls[0][1]["hermes_home"] == "/tmp/hermes-from-env"
assert calls[0][1]["messages"] == [{"role": "user", "content": "profile current turn"}]
assert calls[1][0] == "tokensave_lcm_grep"
assert calls[1][1]["storage_scope"] == "hermes_profile"
assert calls[1][1]["hermes_home"] == "/tmp/hermes-from-env"
"#,
        "generated context engine should resolve HERMES_HOME for profile storage",
    );
}

#[test]
fn generated_context_engine_defaults_to_hermes_home_even_when_missing() {
    run_generated_plugin_script(
        "check_context_engine_default_home.py",
        r#"
import importlib.machinery
import importlib.util
import os
import pathlib
import sys
import tempfile

plugin_dir = pathlib.Path(sys.argv[1])
parent_name = "_hermes_user_default_home"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tokensave"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

os.environ.pop("HERMES_HOME", None)
with tempfile.TemporaryDirectory() as tmp:
    home = pathlib.Path(tmp) / "isolated-home"
    home.mkdir()
    os.environ["HOME"] = str(home)
    expected = str(home / ".hermes")
    assert not pathlib.Path(expected).exists()

    engine = plugin.TokenSaveContextEngine()
    engine.initialize(session_id="session-1")

    assert engine.hermes_home == expected
    status = engine.get_status()
    assert status["storage_scope"] == "hermes_profile"
    assert status["hermes_home"] == expected
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
spec = importlib.util.spec_from_file_location("tokensave_hermes_tools_kwargs", tools_path)
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
tools.call_tokensave_tool(
    "tokensave_lcm_grep",
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

module_name = f"{parent_name}.tokensave"
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
assert engine.name == "lcm"
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

    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
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

module_name = f"{parent_name}.tokensave"
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
assert isinstance(engine, plugin.TokenSaveContextEngine)
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

def fake_call_tokensave_tool(name, args, **kwargs):
    calls.append((name, args, kwargs))
    return "{}"

plugin.tools.call_tokensave_tool = fake_call_tokensave_tool

profile_engine = plugin.TokenSaveContextEngine()
profile_engine.on_session_start(session_id="session-1", hermes_home="/tmp/hermes")
profile_engine.should_compress_preflight(messages=[], current_tokens=123)
name, args, kwargs = calls.pop()
assert name == "tokensave_lcm_preflight"
assert args["session_id"] == "session-1"
assert args["storage_scope"] == "hermes_profile"
assert args["hermes_home"] == "/tmp/hermes"

project_engine = plugin.TokenSaveContextEngine()
project_engine.on_session_start(
    session_id="session-2",
    hermes_home="/tmp/hermes",
    project_root="/tmp/project",
)
project_engine.should_compress_preflight(messages=[], current_tokens=456)
name, args, kwargs = calls.pop()
assert name == "tokensave_lcm_preflight"
assert args["session_id"] == "session-2"
assert args["storage_scope"] == "project_local"
assert args["project_root"] == "/tmp/project"

project_engine = plugin.TokenSaveContextEngine()
project_engine.initialize(session_id="initial", project_root="/tmp/project")
project_engine.on_session_start(session_id="next")
project_engine.should_compress_preflight(messages=[], current_tokens=789)
name, args, kwargs = calls.pop()
assert name == "tokensave_lcm_preflight"
assert args["session_id"] == "next"
assert args["storage_scope"] == "project_local"
assert args["project_root"] == "/tmp/project"

profile_engine = plugin.TokenSaveContextEngine()
profile_engine.initialize(session_id="initial", hermes_home="/tmp/hermes")
profile_engine.on_session_start(session_id="next")
profile_engine.should_compress_preflight(messages=[], current_tokens=321)
name, args, kwargs = calls.pop()
assert name == "tokensave_lcm_preflight"
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
fn context_engine_preflight_uses_tokensave_tool_json_args() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
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
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])

parent_name = "_hermes_user_context"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tokensave"
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

engine = plugin.TokenSaveContextEngine()
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
assert argv[0] == plugin.tools.TOKENSAVE_BIN
assert argv[1:6] == ["tool", "--project", "/tmp/project", "tokensave_lcm_preflight", "--json"]
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
        .output()
        .expect("python3 should run generated Hermes preflight bridge check");
    assert!(
        output.status.success(),
        "generated context engine should call tokensave_lcm_preflight through the JSON bridge\nstdout:\n{}\nstderr:\n{}",
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

    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
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

module_name = f"{parent_name}.tokensave"
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

engine = plugin.TokenSaveContextEngine()
engine.initialize(session_id="session-a", project_root="/tmp/project")

# Plain session starts must not call the boundary tool.
engine.on_session_start(session_id="session-a")
assert calls == []

# Compression boundary session starts hand bound/old session ids to tokensave
# so the Rust side can decide whether the boundary skipped carry-over.
engine.on_session_start(
    session_id="session-b",
    old_session_id="session-c",
    boundary_reason="compression",
)
assert len(calls) == 1
argv = calls[0]
assert argv[0] == plugin.tools.TOKENSAVE_BIN
assert argv[1:6] == ["tool", "--project", "/tmp/project", "tokensave_lcm_session_boundary", "--json"]
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
        .output()
        .expect("python3 should run generated Hermes session boundary bridge check");
    assert!(
        output.status.success(),
        "generated context engine should report compression boundaries through tokensave_lcm_session_boundary\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn context_engine_compress_uses_tokensave_tool_json_args() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
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
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])

parent_name = "_hermes_user_context"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tokensave"
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

engine = plugin.TokenSaveContextEngine()
engine.initialize(session_id="session-2", project_root="/tmp/project")
result = engine.compress(
    [{"role": "assistant", "content": "hello"}],
    current_tokens=1200,
    focus_topic="handoff",
)

assert result == {"status": "not_implemented", "message": "placeholder parsed"}

assert len(calls) == 1
argv = calls[0]
assert argv[0] == plugin.tools.TOKENSAVE_BIN
assert argv[1:6] == ["tool", "--project", "/tmp/project", "tokensave_lcm_compress", "--json"]
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
        "generated context engine should call tokensave_lcm_compress through the JSON bridge\nstdout:\n{}\nstderr:\n{}",
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

    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
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
import pathlib
import sys

plugin_dir = pathlib.Path(sys.argv[1])

parent_name = "_hermes_user_config_defaults"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
parent_module = importlib.util.module_from_spec(parent_spec)
sys.modules[parent_name] = parent_module

module_name = f"{parent_name}.tokensave"
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
}
engine = plugin.TokenSaveContextEngine(config=config)
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

    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
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

module_name = f"{parent_name}.tokensave"
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
    if tool_name == "tokensave_lcm_expand_query":
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

project_engine = plugin.TokenSaveContextEngine()
project_engine.initialize(session_id="session-1", project_root="/tmp/project")
answer = project_engine.expand_query(prompt="What changed?", query="orchard")
assert answer["status"] == "ok"
project_argv = calls.pop()
assert project_argv[0] == plugin.tools.TOKENSAVE_BIN
assert project_argv[1:6] == ["tool", "--project", "/tmp/project", "tokensave_lcm_expand_query", "--json"]
project_args = json.loads(project_argv[project_argv.index("--args") + 1])
assert project_args["storage_scope"] == "project_local"
assert project_args["project_root"] == "/tmp/project"

profile_engine = plugin.TokenSaveContextEngine()
profile_engine.initialize(session_id="session-2", hermes_home="/tmp/hermes-profile")
profile_result = profile_engine.should_compress_preflight(messages=[], current_tokens=100)
assert profile_result["status"] == "ok"
profile_argv = calls.pop()
assert profile_argv[0] == plugin.tools.TOKENSAVE_BIN
assert profile_argv[1:4] == ["tool", "tokensave_lcm_preflight", "--json"]
assert "--project" not in profile_argv
profile_args = json.loads(profile_argv[profile_argv.index("--args") + 1])
assert profile_args["storage_scope"] == "hermes_profile"
assert profile_args["hermes_home"] == "/tmp/hermes-profile"

explicit = plugin.tools.call_tokensave_tool(
    "tokensave_lcm_status",
    {"storage_scope": "hermes_profile", "hermes_home": "/tmp/hermes-profile"},
    project_root="/tmp/project",
)
assert json.loads(explicit)["content"]
explicit_argv = calls.pop()
assert explicit_argv[1:6] == ["tool", "--project", "/tmp/project", "tokensave_lcm_status", "--json"]
"#,
    )
    .unwrap();

    let output = Command::new("python3")
        .arg(&script)
        .arg(plugin_dir)
        .output()
        .expect("python3 should run generated Hermes project flag bridge check");
    assert!(
        output.status.success(),
        "generated bridge should pass project-local roots through tokensave tool --project without affecting profile calls\nstdout:\n{}\nstderr:\n{}",
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

    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
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

module_name = f"{parent_name}.tokensave"
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
engine = plugin.TokenSaveContextEngine()
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

    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
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

module_name = f"{parent_name}.tokensave"
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

def fake_call_tokensave_tool(name, args, **kwargs):
    assert name == "tokensave_lcm_expand_query"
    assert args["session_id"] == "session-1"
    assert args["storage_scope"] == "project_local"
    assert args["project_root"] == "/tmp/project"
    assert args["prompt"] == "What changed?"
    assert args["query"] == "orchard"
    return mcp_response(responses.pop(0))

plugin.tools.call_tokensave_tool = fake_call_tokensave_tool

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
engine = plugin.TokenSaveContextEngine()
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

    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
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

module_name = f"{parent_name}.tokensave"
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
engine = plugin.TokenSaveContextEngine()
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

    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
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

module_name = f"{parent_name}.tokensave"
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
engine = plugin.TokenSaveContextEngine()
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
fn retry_worthy_auxiliary_failure_retries_smaller_summary_request() {
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

module_name = f"{parent_name}.tokensave"
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
    if len(calls) == 2:
        assert args["summarizer"] == {"mode": "hermes_auxiliary"}
        assert args["max_source_messages"] == 1
        return mcp_response(needs_summary(source_messages[:1]))
    assert args["summarizer"] == {
        "mode": "provided",
        "summary_text": "Smaller chunk summary",
        "route": "default",
    }
    return mcp_response({
        "status": "ok",
        "reason": "compressed_backlog",
        "summary_nodes_created": 1,
        "summary_nodes": [],
        "replay_messages": [{"role": "system", "content": "Smaller chunk summary"}],
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
engine = plugin.TokenSaveContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project", agent=agent)
result = engine.compress(
    [{"role": "user", "content": "active"}],
    current_tokens=1200,
    focus_topic="handoff",
)

assert result["status"] == "ok"
assert len(calls) == 3
assert len(agent.auxiliary_client.calls) == 2
assert "old two" in agent.auxiliary_client.calls[0]["messages"][0]["content"]
assert "old two" not in agent.auxiliary_client.calls[1]["messages"][0]["content"]
assert "old one" in agent.auxiliary_client.calls[1]["messages"][0]["content"]
assert result["auxiliary_attempts"] == 2
assert result["auxiliary_retry_status"] == "retried"
assert result["auxiliary_error_classification"] == "retry_worthy"
"#,
        "retry-worthy auxiliary failures should retry with a smaller summary request",
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

module_name = f"{parent_name}.tokensave"
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
fn permanent_auxiliary_failure_returns_error_without_advancing_frontier() {
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

module_name = f"{parent_name}.tokensave"
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

plugin.tools.subprocess.run = fake_run

class BadTemplateAux:
    def __init__(self):
        self.calls = []

    def call_llm(self, **kwargs):
        self.calls.append(kwargs)
        raise RuntimeError("template exploded")

agent = type("Agent", (), {"auxiliary_client": BadTemplateAux()})()
engine = plugin.TokenSaveContextEngine()
engine.initialize(session_id="session-1", project_root="/tmp/project", agent=agent)
result = engine.compress(
    [{"role": "user", "content": "active"}],
    current_tokens=1200,
    focus_topic="handoff",
)

assert result["status"] == "error"
assert result["reason"] == "auxiliary_summary_permanent_failure"
assert result["auxiliary_attempts"] == 1
assert result["auxiliary_retry_status"] == "not_retryable"
assert result["auxiliary_error_classification"] == "permanent"
assert result["frontier"]["current_frontier_store_id"] is None
assert len(calls) == 1
assert len(agent.auxiliary_client.calls) == 1
"#,
        "permanent auxiliary failures should not write a provided summary",
    );
}

#[test]
fn auxiliary_summary_falls_back_and_tracks_route_cooldown() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
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

module_name = f"{parent_name}.tokensave"
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
engine = plugin.TokenSaveContextEngine()
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
assert engine._cooldown_until["primary"] > 0
assert [call.get("model") for call in agent.auxiliary_client.calls] == ["primary", "backup"]

class FailingAux:
    def __init__(self):
        self.calls = []

    def call_llm(self, **kwargs):
        self.calls.append(kwargs)
        raise RuntimeError("route unavailable")

failing_agent = type("Agent", (), {"auxiliary_client": FailingAux()})()
failing_engine = plugin.TokenSaveContextEngine()
failing_engine.initialize(session_id="session-1", project_root="/tmp/project", agent=failing_agent)
fallback = failing_engine._call_auxiliary_summary(
    "Summarize",
    [{"role": "user", "content": "A" * 10000}],
)
assert fallback["status"] == "fallback"
assert len(fallback["text"]) < 10000
assert fallback["text"].endswith("[deterministic compression fallback]")
assert failing_engine._route_failures["default"] == 1
assert failing_engine._cooldown_until["default"] > 0
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

module_name = f"{parent_name}.tokensave"
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
engine = plugin.TokenSaveContextEngine()
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

module_name = f"{parent_name}.tokensave"
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
engine = plugin.TokenSaveContextEngine()
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

module_name = f"{parent_name}.tokensave"
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
engine = plugin.TokenSaveContextEngine()
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
fn call_tokensave_json_normalizes_malformed_mcp_envelopes() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tokensave");
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

module_name = f"{parent_name}.tokensave"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

responses = []

def fake_call_tokensave_tool(name, args, **kwargs):
    return responses.pop(0)

plugin.tools.call_tokensave_tool = fake_call_tokensave_tool

def call_with_outer(outer):
    responses.append(json.dumps(outer))
    return plugin.call_tokensave_json("tokensave_lcm_preflight", {})

missing_content = call_with_outer({})
assert missing_content["error"] == "tokensave tool response missing text content"

empty_content = call_with_outer({"content": []})
assert empty_content["error"] == "tokensave tool response missing text content"

non_text_content = call_with_outer({"content": [{"type": "text", "text": 123}]})
assert non_text_content["error"] == "tokensave tool response missing text content"

responses.append(json.dumps({"content": [{"type": "text", "text": "{not json"}]}))
invalid_nested_json = plugin.call_tokensave_json("tokensave_lcm_preflight", {})
assert invalid_nested_json["error"] == "tokensave tool returned invalid nested JSON"

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
