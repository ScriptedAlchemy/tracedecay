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
assert argv[1:4] == ["tool", "tokensave_lcm_preflight", "--json"]
args_index = argv.index("--args")
args = json.loads(argv[args_index + 1])
assert args == {
    "storage_scope": "project_local",
    "project_root": "/tmp/project",
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
assert argv[1:4] == ["tool", "tokensave_lcm_compress", "--json"]
args = json.loads(argv[argv.index("--args") + 1])
assert args == {
    "storage_scope": "project_local",
    "project_root": "/tmp/project",
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
fn lcm_compression_request_contract_serializes_fake_summarizer() {
    let request = LcmCompressionRequest {
        provider: "cursor".to_string(),
        session_id: "session-1".to_string(),
        messages: vec![serde_json::json!({"role": "user", "content": "fresh"})],
        current_tokens: Some(100),
        focus_topic: Some("billing".to_string()),
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
