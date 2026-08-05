//! Hermes agent tests: plugin install/uninstall, generated Python plugin
//! behavior, config rewriting, and Hermes healthchecks.

use crate::agent_test_support::*;
use crate::common::{PYYAML_FALLBACK_PRELUDE, host_sources, write_pyyaml_shim};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay::agents::*;

#[test]
fn test_hermes_user_install_writes_single_plugin() {
    let home = TempDir::new().unwrap();
    let _agent_env = crate::common::AgentEnvLock::pin(&home);

    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    let manifest = std::fs::read_to_string(plugin_dir.join("plugin.yaml")).unwrap();
    assert!(manifest.contains("name: tracedecay"));
    assert!(manifest.contains("kind: standalone"));
    // `hermes plugins list` shows the manifest version; it must track the
    // generating binary so stale plugins are detectable after upgrades.
    assert!(
        manifest.contains(&format!("version: {}\n", env!("CARGO_PKG_VERSION"))),
        "manifest version must match the generating binary:\n{manifest}"
    );
    assert!(manifest.contains("author: "));
    assert!(manifest.contains("provides_tools:"));
    assert!(manifest.contains("tracedecay_context"));
    assert!(manifest.contains("tracedecay_lcm_status"));
    assert!(!manifest.contains("tracedecay_lcm_compress"));
    assert!(manifest.contains("provides_hooks:"));
    assert!(manifest.contains("pre_llm_call"));
    assert!(manifest.contains("post_tool_call"));
    assert!(manifest.contains("provides_commands:"));
    assert!(manifest.contains("/tracedecay_status"));

    let init_py = std::fs::read_to_string(plugin_dir.join("__init__.py")).unwrap();
    assert!(init_py.contains("def register(ctx):"));
    assert!(init_py.contains("class TracedecayMemoryProvider"));
    assert!(init_py.contains("ctx.register_memory_provider("));
    assert!(init_py.contains("register_tool = getattr(ctx, \"register_tool\", None)"));
    assert!(init_py.contains("ctx.register_hook(\"pre_llm_call\""));
    assert!(init_py.contains("ctx.register_hook(\"post_tool_call\""));
    assert!(init_py.contains("getattr(ctx, \"register_command\", None)"));
    assert!(init_py.contains("getattr(ctx, \"register_skill\", None)"));
    assert!(init_py.contains("register_skill(skill_name, skill_path)"));
    assert!(init_py.contains("class TraceDecayContextEngine"));
    assert!(init_py.contains("routed.setdefault(\"storage_scope\", \"user\")"));
    assert!(!init_py.contains("hermes_profile"));
    assert!(!init_py.contains("hermes_home\": self.hermes_home"));
    assert!(!init_py.contains("HERMES_HOME"));
    assert!(!init_py.contains("tracedecay_lcm_compress"));

    let schemas_py = std::fs::read_to_string(plugin_dir.join("schemas.py")).unwrap();
    assert!(schemas_py.contains("TOOL_SCHEMAS"));
    assert!(schemas_py.contains("json.load"));
    let schemas_json = read_json(&plugin_dir.join("schemas.json"));
    assert!(schemas_json.as_array().is_some_and(|schemas| {
        schemas
            .iter()
            .any(|schema| schema["name"] == "tracedecay_context")
    }));

    let tools_py = std::fs::read_to_string(plugin_dir.join("tools.py")).unwrap();
    assert!(tools_py.contains("/usr/local/bin/tracedecay"));
    assert!(tools_py.contains("subprocess.run"));
    assert!(tools_py.contains("tracedecay tool"));
    assert!(tools_py.contains("TRACEDECAY_TIMEOUT_SECONDS = 120"));
    assert!(tools_py.contains("TRACEDECAY_LONG_TIMEOUT_SECONDS = 600"));
    assert!(tools_py.contains("ARGS_FILE_THRESHOLD_BYTES"));
    assert!(tools_py.contains("truncate_output"));
    assert!(tools_py.contains("\"stderr\""));
    assert!(tools_py.contains("\"stdout\""));
    assert!(tools_py.contains("kwargs.get(\"project_root\")"));
    assert!(!tools_py.contains("tool_args.pop(\"project_root\", None)"));
    assert!(tools_py.contains("code_project_root("));
    assert!(!tools_py.contains("config_pinned_project_root"));
    assert!(!tools_py.contains("HERMES_HOME"));
    assert!(!tools_py.contains("PROFILE_STORE_TOOLS"));
    assert!(
        !tools_py.contains("PINNED_PROJECT_ROOT"),
        "the install-time pin lives only in plugins.tracedecay.project_root"
    );
    assert!(tools_py.contains("argv.extend([\"--project\", str(project_root)])"));
    // Large payloads spill to a tempfile passed as `--args @<path>` so argv
    // never exceeds the kernel's per-string cap.
    assert!(tools_py.contains("argv.extend([name, \"--json\", \"--args\"])"));
    assert!(tools_py.contains("argv.append(\"@\" + args_file)"));
    assert!(!tools_py.contains("shell=True"));
    assert_python_compiles(&[
        &plugin_dir.join("tools.py"),
        &plugin_dir.join("schemas.py"),
        &plugin_dir.join("__init__.py"),
    ]);

    let skill = std::fs::read_to_string(plugin_dir.join("skills/tracedecay/SKILL.md")).unwrap();
    assert!(skill.contains("Use tracedecay"));
    // CLI-fallback steering: mirrors CLI_FALLBACK_PROMPT_RULES, worded for the
    // Hermes plugin surface (tool calls already shell out to the tracedecay CLI).
    assert!(
        skill.contains("tracedecay tool ") && skill.contains("--help"),
        "Hermes skill must steer the agent to the `tracedecay tool` CLI when a tool invocation fails"
    );
    assert!(
        skill.contains("instead of querying"),
        "Hermes skill must warn against querying .tracedecay databases directly"
    );
    assert!(
        skill.contains("Do not invent per-key CLI flags")
            && skill.contains("\"mode\":\"explore\"")
            && skill.contains("\"max_nodes\":20")
            && skill.contains("supported modes are `explore` and `plan`")
            && skill.contains("`--max-tokens` or `--paths`"),
        "Hermes skill must give schema-safe context fallback guidance"
    );

    assert_hermes_config_enables_tracedecay_memory(&home.path().join(".hermes/config.yaml"));
    assert!(!home.path().join(".hermes/profiles").exists());
}

#[test]
fn test_hermes_plugin_init_snapshot_matches_embedded_asset() {
    let home = TempDir::new().unwrap();
    install_hermes_default(home.path());

    let init_py =
        std::fs::read_to_string(home.path().join(".hermes/plugins/tracedecay/__init__.py"))
            .unwrap();

    // Line 1 is the provenance header (generating binary version + commit);
    // everything after it must be the templates/plugin_init.py asset copied
    // verbatim — plugin_init() performs no interpolation over the payload.
    let (header, body) = init_py
        .split_once('\n')
        .expect("generated __init__.py must start with a provenance header line");
    assert!(
        header.starts_with("# Generated by tracedecay "),
        "unexpected provenance header: {header}"
    );
    assert!(
        body == host_sources::HERMES_PLUGIN_INIT_PY,
        "generated __init__.py body must be a verbatim copy of templates/plugin_init.py"
    );

    // Snapshot hash of the template payload. If this fails after an
    // intentional template edit, update the hash; an unexpected failure means
    // the generated plugin changed without anyone touching the asset review.
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    assert_eq!(
        hex::encode(hasher.finalize()),
        "5d297ea66e289675992183dfab10e89e280427ecd3659d3fe6db5a71fdd5f5ba",
        "templates/plugin_init.py payload hash changed — verify the edit is intentional and update this snapshot"
    );
}

#[test]
fn test_hermes_generated_python_registers_lcm_context_engine() {
    let home = TempDir::new().unwrap();
    install_hermes_default(home.path());

    let init_py =
        std::fs::read_to_string(home.path().join(".hermes/plugins/tracedecay/__init__.py"))
            .unwrap();

    assert!(init_py.contains("class TraceDecayContextEngine"));
    assert!(init_py.contains("ctx.register_context_engine"));
    assert!(init_py.contains("routed.setdefault(\"storage_scope\", \"user\")"));
    assert!(init_py.contains("def call_tracedecay_json"));
    assert!(init_py.contains("tracedecay_lcm_status"));
    assert!(!init_py.contains("\"tracedecay_lcm_preflight\","));
    assert!(!init_py.contains("\"tracedecay_lcm_compress\","));
    assert!(!init_py.contains("tracedecay_lcm_session_boundary"));
    // Both registered provider identities are "tracedecay"; "lcm" is reserved
    // for the tool surface (lcm_* / tracedecay_lcm_*), not the engine name.
    assert!(init_py.contains("return \"tracedecay\""));
    assert!(
        !init_py.contains("return \"lcm\""),
        "context engine identity must be \"tracedecay\", not \"lcm\""
    );

    // The context engine exposes the full native LCM tool surface: every
    // native lcm_* tool must be aliased to its tracedecay_lcm_* MCP tool and
    // ship a native schema entry.
    let native_lcm_tools = [
        "lcm_grep",
        "lcm_load_session",
        "lcm_describe",
        "lcm_expand",
        "lcm_expand_query",
        "lcm_status",
        "lcm_doctor",
    ];
    for native in native_lcm_tools {
        assert!(
            init_py.contains(&format!("\"{native}\": \"tracedecay_{native}\"")),
            "__init__.py LCM_TOOL_ALIASES must map {native} -> tracedecay_{native}"
        );
        assert!(
            init_py.contains(&format!("\"name\": \"{native}\"")),
            "__init__.py LCM_NATIVE_SCHEMAS must define a schema for {native}"
        );
    }
    for adapter in [
        "translated.setdefault(\"scope\", \"current\")",
        "translated.setdefault(\"sort\", \"relevance\")",
        "translated.setdefault(\"include_summaries\", False)",
        "translated.setdefault(\"temporal_mode\", \"current\")",
        "translated.setdefault(\"limit\", 100)",
        "translated.setdefault(\"content_limit\", 4000)",
        "translated.setdefault(\"temporal_mode\", \"forensic\")",
    ] {
        assert!(
            init_py.contains(adapter),
            "Hermes LCM alias must keep its surface default explicit: {adapter}"
        );
    }

    // Every tracedecay_lcm_* MCP tool must be declared in the generated plugin
    // manifest and tool schemas so the embedded constants cannot drift from
    // the current LCM tool surface.
    let manifest =
        std::fs::read_to_string(home.path().join(".hermes/plugins/tracedecay/plugin.yaml"))
            .unwrap();
    let schemas_json =
        std::fs::read_to_string(home.path().join(".hermes/plugins/tracedecay/schemas.json"))
            .unwrap();
    let lcm_tool_names: Vec<String> = tool_names()
        .into_iter()
        .filter(|name| name.starts_with("tracedecay_lcm_"))
        .collect();
    assert_eq!(
        lcm_tool_names.len(),
        7,
        "expected the 7 read-only LCM MCP tools, got {lcm_tool_names:?}"
    );
    for name in &lcm_tool_names {
        assert!(
            manifest.contains(&format!("  - {name}")),
            "plugin.yaml provides_tools must list {name}"
        );
        assert!(
            schemas_json.contains(&format!("\"name\": \"{name}\"")),
            "schemas.json must contain a schema for {name}"
        );
    }
}

#[test]
fn test_hermes_generated_python_handles_quoted_unicode_tracedecay_path() {
    let home = TempDir::new().unwrap();
    let _agent_env = crate::common::AgentEnvLock::pin(&home);
    let tracedecay_bin = home.path().join("bin with spaces").join("token\"save-π");
    let ctx = InstallContext {
        home: home.path().to_path_buf(),
        tracedecay_bin: tracedecay_bin.to_string_lossy().to_string(),
        tool_permissions: expected_tool_perms(),
        project_root: None,
        dashboard: false,
    };

    HermesIntegration.install(&ctx).unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    let script = plugin_dir.join("check_tools.py");
    std::fs::write(
        &script,
        format!(
            "{}\n{}",
            python_compile_check("_compile_pathlib.Path(_compile_sys.argv[1]).parent"),
            r#"
import importlib.util
import json
import os
import pathlib
import sys

tools_path = pathlib.Path(sys.argv[1])
expected_bin = sys.argv[2]
# Hermetic user home; the Hermes override must not redirect generated tools.
os.environ["HOME"] = str(tools_path.parent.parent.parent.parent)
os.environ["HERMES_HOME"] = "/ignored/hermes-home"
spec = importlib.util.spec_from_file_location("tracedecay_hermes_tools", tools_path)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

assert module.TRACEDECAY_BIN == expected_bin

class Result:
    returncode = 7
    stdout = "stdout-" * 1000
    stderr = "stderr-" * 1000

def fake_run(argv, **kwargs):
    assert argv[0] == expected_bin
    assert argv[1:3] == ["tool", "--project"]
    assert argv[3] == os.getcwd()
    assert argv[4:7] == ["tracedecay_context", "--json", "--args"]
    assert json.loads(argv[7]) == {"format": "json", "query": "x"}
    assert "cwd" not in kwargs
    assert kwargs["timeout"] == 120
    assert kwargs["shell"] is False
    return Result()

module.subprocess.run = fake_run
payload = json.loads(module.call_tracedecay_tool("tracedecay_context", {"query": "x"}))
assert payload["error"] == "tracedecay tool exited with status 7"
assert payload["stdout"].startswith("stdout-")
assert payload["stderr"].startswith("stderr-")
assert payload["stdout"].endswith("...<truncated>")
assert payload["stderr"].endswith("...<truncated>")
"#
        ),
    )
    .unwrap();

    let output = python_command()
        .arg(&script)
        .arg(plugin_dir.join("tools.py"))
        .arg(tracedecay_bin)
        .output()
        .expect("python3 should run generated Hermes tools import check");
    assert!(
        output.status.success(),
        "generated tools.py should import and expose diagnosable errors\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_hermes_generated_python_registers_memory_provider() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx_with_real_bin(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    let script = plugin_dir.join("check_memory_provider.py");
    std::fs::write(
        &script,
        format!(
            "{}\n{}",
            python_compile_check("_compile_pathlib.Path(_compile_sys.argv[1])"),
            r#"
import importlib
import importlib.machinery
import importlib.util
import abc
import json
import os
import pathlib
import sys
import types

plugin_dir = pathlib.Path(sys.argv[1])
os.environ["HOME"] = str(plugin_dir.parent.parent.parent)
os.environ["HERMES_HOME"] = "/ignored/hermes-home"

class MemoryProvider(abc.ABC):
    @property
    @abc.abstractmethod
    def name(self):
        pass

    @abc.abstractmethod
    def is_available(self):
        pass

    @abc.abstractmethod
    def initialize(self, session_id, **kwargs):
        pass

    @abc.abstractmethod
    def get_tool_schemas(self):
        pass

agent_module = types.ModuleType("agent")
memory_provider_module = types.ModuleType("agent.memory_provider")
memory_provider_module.MemoryProvider = MemoryProvider
sys.modules["agent"] = agent_module
sys.modules["agent.memory_provider"] = memory_provider_module

parent_name = "_hermes_user_memory"
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

class FullCtx:
    context_engine_tool_handlers_receive_messages = True

    def __init__(self):
        self.tools = []
        self.hooks = []
        self.commands = []
        self.skills = []
        self.memory_providers = []
        self.config_defaults = []

    def register_tool(self, **kwargs):
        self.tools.append(kwargs)

    def register_hook(self, name, handler):
        self.hooks.append((name, handler))

    def register_command(self, name, handler, **kwargs):
        self.commands.append((name, handler, kwargs))

    def register_skill(self, name, path):
        self.skills.append((name, path))

    def register_memory_provider(self, provider):
        self.memory_providers.append(provider)

    def register_config_defaults(self, defaults):
        self.config_defaults.append(defaults)

ctx = FullCtx()
plugin.register(ctx)
assert any(tool["name"] == "tracedecay_context" for tool in ctx.tools)
assert ctx.hooks and ctx.hooks[0][0] == "pre_llm_call"
assert ctx.commands and ctx.commands[0][0] == "/tracedecay_status"
assert ctx.skills and ctx.skills[0][0] == "tracedecay"
assert len(ctx.memory_providers) == 1

# Conventional config defaults registered under the plugins.tracedecay block.
assert len(ctx.config_defaults) == 1
defaults = ctx.config_defaults[0]
assert set(defaults) == {"plugins"}
assert "project_root" not in defaults["plugins"]["tracedecay"]

provider = ctx.memory_providers[0]
assert isinstance(provider, MemoryProvider)
assert provider.name == "tracedecay"
assert provider.provider_id == "tracedecay"
assert provider.is_available() is True
original_bin = plugin.tools.TRACEDECAY_BIN
plugin.tools.TRACEDECAY_BIN = "/definitely/missing/tracedecay"
assert provider.is_available() is False
plugin.tools.TRACEDECAY_BIN = original_bin
provider.initialize("session-123", hermes_home="/tmp/hermes-profile")
assert provider.hermes_home == "/tmp/hermes-profile"
assert provider.project_root is None
assert provider.project_root != provider.hermes_home
assert provider.session_id == "session-123"
# Hermes home remains host config only; TraceDecay routing stays on cwd.
provider.initialize("session-only")
assert provider.hermes_home == str(plugin_dir.parent.parent)
assert provider.project_root is None
assert provider.session_id == "session-only"
provider.project_root = os.getcwd()

# Collapsed schema surface: fact_store(action=...) covers the nine legacy
# fixed-action aliases, which stay dispatchable but cost no schema footprint.
schemas = provider.get_tool_schemas()
schema_names = [schema.get("name") for schema in schemas]
assert schema_names == ["fact_store", "fact_feedback", "memory_status"]
assert all("function" not in schema for schema in schemas)
schema_by_name = {schema["name"]: schema for schema in schemas}
fact_store_schema = schema_by_name["fact_store"]
fact_feedback_schema = schema_by_name["fact_feedback"]
assert fact_store_schema["parameters"]["required"] == ["action"]
assert fact_feedback_schema["parameters"]["required"] == ["fact_id"]

calls = []

def fake_call(name, args, **kwargs):
    calls.append((name, args, kwargs))
    return json.dumps({"name": name, "args": args})

plugin.tools.call_tracedecay_tool = fake_call
store_result = provider.handle_tool_call("fact_store", {"action": "list"}, request_id="r1")
feedback_result = provider.handle_tool_call("fact_feedback", {"fact_id": 7, "helpful": True})
search_result = provider.handle_tool_call("fact_search", {"query": "Project Phoenix"})
status_result = provider.handle_tool_call("memory_status", None)
assert isinstance(store_result, str)
assert isinstance(feedback_result, str)
assert isinstance(search_result, str)
assert isinstance(status_result, str)
assert json.loads(store_result)["name"] == "tracedecay_fact_store"
assert json.loads(feedback_result)["name"] == "tracedecay_fact_feedback"
assert json.loads(search_result)["name"] == "tracedecay_fact_store"
assert json.loads(status_result)["name"] == "tracedecay_memory_status"
assert calls[0][0] == "tracedecay_fact_store"
assert calls[0][1] == {"action": "list", "memory_scope": "project"}
assert calls[0][2]["request_id"] == "r1"
assert calls[1][0] == "tracedecay_fact_feedback"
assert calls[2][0] == "tracedecay_fact_store"
assert calls[2][1] == {"query": "Project Phoenix", "action": "search", "memory_scope": "project"}
assert calls[3][0] == "tracedecay_memory_status"
assert calls[3][1] == {"memory_scope": "project"}

class LegacyCtx:
    context_engine_tool_handlers_receive_messages = True

    def __init__(self):
        self.tools = []
        self.hooks = []

    def register_tool(self, **kwargs):
        self.tools.append(kwargs)

    def register_hook(self, name, handler):
        self.hooks.append((name, handler))

    def register_skill(self, name, path):
        pass

legacy = LegacyCtx()
plugin.register(legacy)
assert any(tool["name"] == "tracedecay_context" for tool in legacy.tools)
assert legacy.hooks and legacy.hooks[0][0] == "pre_llm_call"

class ProviderCollector:
    def __init__(self):
        self.provider = None

    def register_memory_provider(self, provider):
        self.provider = provider

    def register_tool(self, *args, **kwargs):
        pass

    def register_hook(self, *args, **kwargs):
        pass

    def register_cli_command(self, *args, **kwargs):
        pass

collector = ProviderCollector()
plugin.register(collector)
assert collector.provider is not None
assert collector.provider.name == "tracedecay"
"#
        ),
    )
    .unwrap();

    let output = python_command()
        .arg(&script)
        .arg(plugin_dir)
        .output()
        .expect("python3 should run generated Hermes memory provider check");
    assert!(
        output.status.success(),
        "generated plugin should register a Hermes memory provider\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_hermes_generated_memory_provider_is_discovered_from_active_home() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx_with_real_bin(home.path()))
        .unwrap();

    let hermes_home = home.path().join(".hermes");
    let plugin_dir = hermes_home.join("plugins/tracedecay");
    let script = plugin_dir.join("check_hermes_discovery.py");
    std::fs::write(
        &script,
        format!(
            "{PYYAML_FALLBACK_PRELUDE}\n{}",
            r#"
import abc
import importlib.machinery
import importlib.util
import os
import pathlib
import sys
import types

hermes_home = pathlib.Path(sys.argv[1])
os.environ["HOME"] = str(hermes_home.parent)
os.environ["HERMES_HOME"] = "/ignored/hermes-home"

class MemoryProvider(abc.ABC):
    @property
    @abc.abstractmethod
    def name(self):
        pass

    @abc.abstractmethod
    def is_available(self):
        pass

    @abc.abstractmethod
    def initialize(self, session_id, **kwargs):
        pass

    @abc.abstractmethod
    def get_tool_schemas(self):
        pass

    def get_config_schema(self):
        return []

    def save_config(self, values, hermes_home):
        pass

agent_module = types.ModuleType("agent")
memory_provider_module = types.ModuleType("agent.memory_provider")
memory_provider_module.MemoryProvider = MemoryProvider
sys.modules["agent"] = agent_module
sys.modules["agent.memory_provider"] = memory_provider_module

def is_memory_provider_dir(path):
    init_file = path / "__init__.py"
    if not init_file.exists():
        return False
    source = init_file.read_text(errors="replace")[:8192]
    return "register_memory_provider" in source or "MemoryProvider" in source

def iter_user_provider_dirs():
    plugins_dir = hermes_home / "plugins"
    for child in sorted(plugins_dir.iterdir()):
        if child.is_dir() and not child.name.startswith(("_", ".")) and is_memory_provider_dir(child):
            yield child.name, child

def load_provider(provider_dir):
    parent_name = "_hermes_user_memory"
    if parent_name not in sys.modules:
        parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
        parent_spec.submodule_search_locations = []
        sys.modules[parent_name] = importlib.util.module_from_spec(parent_spec)

    module_name = f"{parent_name}.{provider_dir.name}"
    spec = importlib.util.spec_from_file_location(
        module_name,
        provider_dir / "__init__.py",
        submodule_search_locations=[str(provider_dir)],
    )
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    spec.loader.exec_module(module)

    class ProviderCollector:
        def __init__(self):
            self.provider = None
        def register_memory_provider(self, provider):
            self.provider = provider
        def register_tool(self, *args, **kwargs):
            pass
        def register_hook(self, *args, **kwargs):
            pass
        def register_cli_command(self, *args, **kwargs):
            pass

    collector = ProviderCollector()
    module.register(collector)
    return collector.provider

config = (hermes_home / "config.yaml").read_text()
assert "memory:" in config
assert "provider: tracedecay" in config
assert "context:" in config
assert "engine: tracedecay" in config

providers = dict(iter_user_provider_dirs())
assert "tracedecay" in providers
provider = load_provider(providers["tracedecay"])
assert provider is not None
assert isinstance(provider, MemoryProvider)
assert provider.name == "tracedecay"
assert provider.is_available() is True

# `hermes memory setup` has no TraceDecay storage selector fields.
schema = provider.get_config_schema()
assert schema == []

# Hermes layers get_config_defaults() under DEFAULT_CONFIG.
defaults = provider.get_config_defaults()
assert "project_root" not in defaults["plugins"]["tracedecay"]
assert "nudge" in defaults["plugins"]["tracedecay"]

# Dashboard hints use full config dot-paths.
field_meta = provider.get_config_field_meta()
assert "plugins.tracedecay.project_root" not in field_meta
assert "plugins.tracedecay.nudge" in field_meta

provider.initialize("doctor-session", hermes_home=str(hermes_home), platform="cli")
assert provider.hermes_home == str(hermes_home)
assert "fact_store" in [schema["name"] for schema in provider.get_tool_schemas()]
assert "memory_status" in [schema["name"] for schema in provider.get_tool_schemas()]
"#
        ),
    )
    .unwrap();

    let mut check = python_command();
    check
        .arg(&script)
        .arg(hermes_home)
        .arg(write_pyyaml_shim(home.path()));
    let output = check
        .output()
        .expect("python3 should run Hermes memory provider discovery check");
    assert!(
        output.status.success(),
        "Hermes-style memory provider discovery should find the generated tracedecay provider\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Stock-ABC variant harness: upstream (NousResearch/hermes-agent) Hermes
/// differs from forks in two load-bearing ways — the ABCs enforce a fixed
/// abstract-method set (instantiation raises `TypeError` on any miss), and
/// the general `PluginContext` has **no** `register_memory_provider` or
/// `register_config_defaults` (memory providers load through the
/// `plugins/memory` `_ProviderCollector` instead). The generated plugin must
/// keep memory + context + tools functional on that surface and skip
/// fork-only registrations without raising.
#[test]
fn test_hermes_generated_python_degrades_gracefully_on_stock_hermes_api() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx_with_real_bin(home.path()))
        .unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    let script = plugin_dir.join("check_stock_abi.py");
    std::fs::write(
        &script,
        r#"
import abc
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import sys
import types

plugin_dir = pathlib.Path(sys.argv[1])
os.environ["HOME"] = str(plugin_dir.parent.parent.parent)
os.environ["HERMES_HOME"] = "/ignored/hermes-home"

# Stock agent/memory_provider.py abstract surface (upstream pins these four).
class MemoryProvider(abc.ABC):
    @property
    @abc.abstractmethod
    def name(self):
        pass

    @abc.abstractmethod
    def is_available(self):
        pass

    @abc.abstractmethod
    def initialize(self, session_id, **kwargs):
        pass

    @abc.abstractmethod
    def get_tool_schemas(self):
        pass

# Stock agent/context_engine.py abstract surface. Instantiating the generated
# engine under this ABC proves every stock-abstract method is implemented
# (update_from_response is the one newer stock releases added).
class ContextEngine(abc.ABC):
    @property
    @abc.abstractmethod
    def name(self):
        pass

    @abc.abstractmethod
    def update_from_response(self, usage):
        pass

    @abc.abstractmethod
    def should_compress(self, prompt_tokens=None):
        pass

    @abc.abstractmethod
    def compress(self, messages, current_tokens=None, focus_topic=None, **kwargs):
        pass

agent_module = types.ModuleType("agent")
memory_provider_module = types.ModuleType("agent.memory_provider")
memory_provider_module.MemoryProvider = MemoryProvider
context_engine_module = types.ModuleType("agent.context_engine")
context_engine_module.ContextEngine = ContextEngine
sys.modules["agent"] = agent_module
sys.modules["agent.memory_provider"] = memory_provider_module
sys.modules["agent.context_engine"] = context_engine_module

parent_name = "_hermes_stock_plugins"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
sys.modules[parent_name] = importlib.util.module_from_spec(parent_spec)

module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

# Stock general PluginContext: hooks/commands/skills/context engine/tools
# exist; register_memory_provider, register_config_defaults, and the
# context_engine_tool_handlers_receive_messages capability do not.
class StockPluginContext:
    def __init__(self):
        self.tools = []
        self.hooks = []
        self.commands = []
        self.cli_commands = []
        self.middleware = []
        self.skills = []
        self.context_engine = None

    def register_tool(self, **kwargs):
        self.tools.append(kwargs)

    def register_cli_command(self, *args, **kwargs):
        self.cli_commands.append((args, kwargs))

    def register_command(self, name, handler, description="", args_hint=""):
        self.commands.append((name, handler, description))

    def register_context_engine(self, engine):
        assert self.context_engine is None, "only one context engine is allowed"
        self.context_engine = engine

    def register_hook(self, hook_name, callback):
        self.hooks.append((hook_name, callback))

    def register_middleware(self, kind, callback):
        self.middleware.append((kind, callback))

    def register_skill(self, name, path, description=""):
        self.skills.append((name, path))

ctx = StockPluginContext()
plugin.register(ctx)

# Core registrations stay functional on the stock surface.
assert ctx.hooks and ctx.hooks[0][0] == "pre_llm_call"
assert ctx.commands and ctx.commands[0][0] == "/tracedecay_status"
assert ctx.skills and ctx.skills[0][0] == "tracedecay"
assert ctx.context_engine is not None
assert isinstance(ctx.context_engine, ContextEngine)
assert ctx.context_engine.name == "tracedecay"
# Code-graph / memory / transcript tools register unconditionally; only the
# messages-dependent LCM live-ingest verbs (and the context-engine tool
# mirrors) stay gated on the message-forwarding capability, which stock
# never advertises — the LCM surface stays reachable through the context
# engine schemas instead.
registered = [tool["name"] for tool in ctx.tools]
assert "tracedecay_search" in registered
assert "tracedecay_context" in registered
assert "tracedecay_lcm_compress" not in registered
assert "tracedecay_lcm_preflight" not in registered
assert "lcm_grep" not in registered
lcm_names = [schema["name"] for schema in ctx.context_engine.get_tool_schemas()]
assert "lcm_status" in lcm_names and "lcm_grep" in lcm_names

# The stock-abstract ContextEngine methods round-trip.
engine = ctx.context_engine
engine.update_from_response({"prompt_tokens": 11, "completion_tokens": 4})
assert engine.last_total_tokens == 15

calls = []

def fake_call(name, args, **kwargs):
    calls.append((name, args))
    return json.dumps({"content": [{"type": "text", "text": json.dumps({"should_compress": False})}]})

original_call = plugin.tools.call_tracedecay_tool
plugin.tools.call_tracedecay_tool = fake_call
assert engine.should_compress(123) is False
assert calls == []
plugin.tools.call_tracedecay_tool = original_call

# Stock memory activation: plugins/memory drives register() through its
# _ProviderCollector (exactly these four methods — notably no
# register_command, register_context_engine, or register_skill).
class StockProviderCollector:
    def __init__(self):
        self.provider = None

    def register_memory_provider(self, provider):
        self.provider = provider

    def register_tool(self, *args, **kwargs):
        pass

    def register_hook(self, *args, **kwargs):
        pass

    def register_cli_command(self, *args, **kwargs):
        pass

collector = StockProviderCollector()
plugin.register(collector)
assert collector.provider is not None
assert isinstance(collector.provider, MemoryProvider)
assert collector.provider.name == "tracedecay"
assert collector.provider.is_available() is True
assert collector.provider.get_tool_schemas()

# Stock fallback branch: when register() yields no provider, the loader
# instantiates any module-level MemoryProvider subclass directly.
fallback = plugin.TracedecayMemoryProvider()
assert isinstance(fallback, MemoryProvider)
assert fallback.name == "tracedecay"
"#,
    )
    .unwrap();

    let output = python_command()
        .arg(&script)
        .arg(plugin_dir)
        .output()
        .expect("python3 should run stock Hermes API degradation check");
    assert!(
        output.status.success(),
        "generated plugin should degrade gracefully on the stock Hermes plugin API\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_hermes_global_install_and_uninstall_plugin() {
    let home = TempDir::new().unwrap();
    let _agent_env = crate::common::AgentEnvLock::pin(&home);
    let ctx = make_install_ctx(home.path());

    HermesIntegration.install(&ctx).unwrap();

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    assert!(plugin_dir.join("plugin.yaml").exists());
    assert!(plugin_dir.join("__init__.py").exists());
    let config = std::fs::read_to_string(home.path().join(".hermes/config.yaml")).unwrap();
    assert!(config.contains("- tracedecay"));

    HermesIntegration.uninstall(&ctx).unwrap();
    assert!(
        !plugin_dir.exists(),
        "uninstall should remove only the tracedecay Hermes plugin directory"
    );
    let config = std::fs::read_to_string(home.path().join(".hermes/config.yaml")).unwrap();
    assert!(
        !config.contains("- tracedecay"),
        "uninstall should remove tracedecay from plugins.enabled"
    );
    assert!(
        !config.contains("memory:\n"),
        "uninstall should remove the empty tracedecay-created memory block"
    );
    assert!(
        !config.contains("engine: tracedecay") && !config.contains("context:\n"),
        "uninstall should remove the tracedecay context engine activation:\n{config}"
    );
}

#[test]
fn test_removed_hermes_profile_flags_are_unknown() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    for (command, flag, value) in [
        ("install", "--profile", Some("work")),
        ("install", "--all-profiles", None),
        ("install", "--project-root", Some("/tmp/project")),
        ("uninstall", "--profile", Some("work")),
        ("uninstall", "--all-profiles", None),
    ] {
        let mut process = tracedecay_command(project.path(), home.path());
        process.arg(command).arg("--agent").arg("hermes").arg(flag);
        if let Some(value) = value {
            process.arg(value);
        }
        let output = process.output().expect("run removed Hermes selector");
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("unexpected argument"),
            "stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn test_hermes_install_removes_tracedecay_from_disabled_list() {
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    std::fs::write(
        hermes_dir.join("config.yaml"),
        "theme: dark\nplugins:\n  disabled:\n    - tracedecay\n    - other\n",
    )
    .unwrap();

    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(config.contains("theme: dark"));
    assert!(config.contains("enabled:"));
    assert!(config.contains("    - tracedecay"));
    assert!(
        !config.contains("  disabled:\n    - tracedecay"),
        "plugins.disabled must not keep tracedecay because disabled wins"
    );
    assert!(config.contains("    - other"));
}

#[test]
fn test_hermes_install_matches_two_space_list_item_indent() {
    // Hermes itself writes sequence items at the same indent as the key
    // (`enabled:` + `  - item`); inserting a 4-space item into such a list
    // produces unparseable YAML, so the installer must match the style.
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    std::fs::write(
        hermes_dir.join("config.yaml"),
        "theme: dark\nplugins:\n  enabled:\n  - other\n  - second\n",
    )
    .unwrap();

    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(
        config.contains("  enabled:\n  - other\n  - second\n  - tracedecay\n"),
        "tracedecay must be inserted with the existing 2-space item indent:\n{config}"
    );
    assert!(
        !config.contains("    - tracedecay"),
        "no 4-space item may be mixed into a 2-space list:\n{config}"
    );

    // Idempotency: a second install must detect the 2-space item.
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();
    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert_eq!(
        config.matches("- tracedecay").count(),
        1,
        "re-install must not duplicate the 2-space list item:\n{config}"
    );
}

#[test]
fn test_hermes_install_removes_two_space_indent_disabled_entry() {
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    std::fs::write(
        hermes_dir.join("config.yaml"),
        "plugins:\n  disabled:\n  - tracedecay\n  - other\n  enabled:\n  - kept\n",
    )
    .unwrap();

    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(
        config.contains("  disabled:\n  - other\n"),
        "tracedecay must be removed from the 2-space disabled list:\n{config}"
    );
    assert!(
        config.contains("  enabled:\n  - kept\n  - tracedecay\n"),
        "tracedecay must be enabled with matching indent:\n{config}"
    );
}

#[test]
fn test_hermes_install_accepts_flow_style_empty_disabled_list() {
    // Hermes writes `disabled: []` for the empty list; the installer used to
    // reject it as "unsupported Hermes plugins config".
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    std::fs::write(
        hermes_dir.join("config.yaml"),
        "theme: dark\nplugins:\n  disabled: []\n  enabled:\n    - other\n",
    )
    .unwrap();

    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(
        config.contains("  disabled: []"),
        "the empty flow-style disabled list must be preserved:\n{config}"
    );
    assert!(
        config.contains("  enabled:\n    - other\n    - tracedecay\n"),
        "tracedecay must be added to the enabled list:\n{config}"
    );
}

#[test]
fn test_hermes_install_fills_flow_style_empty_enabled_list() {
    // The lossless editor keeps the author's flow style: `enabled: []`
    // becomes `enabled: [tracedecay]` in place instead of being rewritten
    // into a block list.
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    std::fs::write(
        hermes_dir.join("config.yaml"),
        "plugins:\n  enabled: []\n  disabled: []\n",
    )
    .unwrap();

    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(
        config.contains("  enabled: [tracedecay]"),
        "`enabled: []` must gain tracedecay in place:\n{config}"
    );
    assert!(
        config.contains("  disabled: []"),
        "the untouched disabled flow list must survive:\n{config}"
    );
}

#[test]
fn test_hermes_install_appends_to_non_empty_flow_lists() {
    // The lossless editor supports non-empty flow lists in place; the
    // pre-lossless installer rejected them as unsupported.
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    let original = "plugins:\n  enabled: [other]\n";
    std::fs::write(hermes_dir.join("config.yaml"), original).unwrap();

    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(
        config.contains("  enabled: [other, tracedecay]"),
        "tracedecay must be appended to the flow list in place:\n{config}"
    );

    // Idempotency: a second install must detect the flow entry.
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();
    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert_eq!(
        config.matches("tracedecay]").count(),
        1,
        "re-install must not duplicate the flow list entry:\n{config}"
    );
}

#[test]
fn test_hermes_install_backs_up_existing_config() {
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    let original = "theme: dark\nplugins:\n  enabled:\n    - other\n";
    std::fs::write(hermes_dir.join("config.yaml"), original).unwrap();

    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let backup = hermes_dir.join("config.yaml.bak");
    assert!(
        backup.exists(),
        "install should back up existing Hermes config"
    );
    assert_eq!(
        std::fs::read_to_string(backup).unwrap(),
        original,
        "backup should preserve the exact original config"
    );
}

#[test]
fn test_hermes_install_rejects_existing_memory_provider_without_rewrite() {
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    let original =
        "theme: dark\nmemory:\n  provider: other-memory\nplugins:\n  enabled:\n    - other\n";
    std::fs::write(hermes_dir.join("config.yaml"), original).unwrap();

    let err = HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap_err()
        .to_string();

    assert!(err.contains("Hermes memory provider already configured"));
    assert_eq!(
        std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap(),
        original,
        "install must not overwrite an existing Hermes memory provider"
    );
}

#[test]
fn test_hermes_install_rejects_existing_context_engine_without_rewrite() {
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    let original = "theme: dark\ncontext:\n  engine: other-engine\n";
    std::fs::write(hermes_dir.join("config.yaml"), original).unwrap();

    let err = HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap_err()
        .to_string();

    assert!(
        err.contains("Hermes context engine already configured"),
        "unexpected error: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap(),
        original,
        "install must not overwrite a foreign Hermes context engine"
    );
}

#[test]
fn test_hermes_install_replaces_default_compressor_context_engine() {
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    // `compressor` is the built-in default the host falls back to anyway, so
    // replacing it is the activation step, not an overwrite.
    std::fs::write(
        hermes_dir.join("config.yaml"),
        "theme: dark\ncontext:\n  engine: compressor\n  other_key: 1\n",
    )
    .unwrap();

    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(
        config.contains("  engine: tracedecay"),
        "install must replace the default compressor engine:\n{config}"
    );
    assert!(
        config.contains("  other_key: 1"),
        "install must keep unrelated context keys:\n{config}"
    );

    // Uninstall removes only the engine selection, not the user's block.
    HermesIntegration
        .uninstall(&make_install_ctx(home.path()))
        .unwrap();
    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(
        !config.contains("engine: tracedecay"),
        "uninstall must deactivate the tracedecay context engine:\n{config}"
    );
    assert!(
        config.contains("context:") && config.contains("  other_key: 1"),
        "uninstall must keep the user's remaining context block:\n{config}"
    );
}

#[test]
fn test_hermes_install_preserves_user_keys_in_tracedecay_config_block() {
    let home = TempDir::new().unwrap();
    let _agent_env = crate::common::AgentEnvLock::pin(&home);
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    std::fs::write(
        hermes_dir.join("config.yaml"),
        "plugins:\n  tracedecay:\n    summary_model: glm-4.7\n  enabled:\n    - other\n",
    )
    .unwrap();

    let ctx = InstallContext {
        home: home.path().to_path_buf(),
        tracedecay_bin: "/usr/local/bin/tracedecay".to_string(),
        tool_permissions: expected_tool_perms(),
        project_root: Some(std::path::PathBuf::from("/pinned/project")),
        dashboard: false,
    };
    HermesIntegration.install(&ctx).unwrap();

    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(!config.contains("project_root:"));
    assert!(
        config.contains("    summary_model: glm-4.7"),
        "install must keep user keys in the plugins.tracedecay block:\n{config}"
    );

    // Uninstall keeps user-owned plugin settings.
    HermesIntegration
        .uninstall(&make_install_ctx(home.path()))
        .unwrap();
    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(
        !config.contains("project_root:"),
        "uninstall must remove the generated pin:\n{config}"
    );
    assert!(
        config.contains("  tracedecay:") && config.contains("    summary_model: glm-4.7"),
        "uninstall must keep user keys in the plugins.tracedecay block:\n{config}"
    );
}

#[test]
fn test_hermes_healthcheck_warns_on_stale_plugin() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    install_hermes_default(home.path());

    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    let hctx = HealthcheckContext {
        home: home.path().to_path_buf(),
        project_path: project.path().to_path_buf(),
    };

    // Fresh install: version matches the binary — no warnings.
    let mut dc = DoctorCounters::new();
    HermesIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(dc.warnings, 0, "fresh install should be healthy");

    // Stale manifest version (an old generator wrote it) must warn.
    let manifest_path = plugin_dir.join("plugin.yaml");
    let manifest = std::fs::read_to_string(&manifest_path).unwrap();
    std::fs::write(
        &manifest_path,
        manifest.replace(
            &format!("version: {}", env!("CARGO_PKG_VERSION")),
            "version: 1.0.0",
        ),
    )
    .unwrap();
    let mut dc = DoctorCounters::new();
    HermesIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(
        dc.warnings, 1,
        "stale generated plugin version should warn once"
    );
}

#[test]
fn test_hermes_install_edits_inline_plugins_config_in_place() {
    // The lossless editor supports the inline flow-mapping form; the
    // pre-lossless installer rejected it as unsupported.
    let home = TempDir::new().unwrap();
    let hermes_dir = home.path().join(".hermes");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    let original = "theme: dark\nplugins: { enabled: [other] }\n";
    std::fs::write(hermes_dir.join("config.yaml"), original).unwrap();

    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();

    let config = std::fs::read_to_string(hermes_dir.join("config.yaml")).unwrap();
    assert!(
        config.contains("theme: dark"),
        "unrelated config must be preserved:\n{config}"
    );
    assert!(
        config.contains("plugins: { enabled: [other, tracedecay] }"),
        "the inline flow mapping must be edited in place, preserving its style:\n{config}"
    );
}

#[test]
fn test_hermes_uninstall_retires_legacy_named_profile_plugin() {
    let home = TempDir::new().unwrap();
    let _agent_env = crate::common::AgentEnvLock::pin(&home);
    let profile = home.path().join(".hermes/profiles/work");
    let plugin_dir = profile.join("plugins/tracedecay");
    let other_plugin = profile.join("plugins/other");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::create_dir_all(&other_plugin).unwrap();
    std::fs::write(plugin_dir.join("plugin.yaml"), "name: tracedecay\n").unwrap();
    std::fs::write(other_plugin.join("plugin.yaml"), "name: other\n").unwrap();
    std::fs::write(
        profile.join("config.yaml"),
        "theme: dark\nplugins:\n  enabled:\n    - other\n    - tracedecay\n",
    )
    .unwrap();

    let ctx = InstallContext {
        home: home.path().to_path_buf(),
        tracedecay_bin: String::new(),
        tool_permissions: expected_tool_perms(),
        project_root: None,
        dashboard: false,
    };

    HermesIntegration.uninstall(&ctx).unwrap();

    assert!(!plugin_dir.exists());
    assert!(other_plugin.join("plugin.yaml").exists());
    let config = std::fs::read_to_string(profile.join("config.yaml")).unwrap();
    assert!(config.contains("theme: dark"));
    assert!(config.contains("    - other"));
    assert!(!config.contains("tracedecay"));
}

#[test]
fn test_hermes_uninstall_preserves_unknown_files_in_tracedecay_plugin_dir() {
    let home = TempDir::new().unwrap();
    let plugin_dir = home.path().join(".hermes/plugins/tracedecay");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("plugin.yaml"), "name: tracedecay\n").unwrap();
    std::fs::write(plugin_dir.join("user-notes.txt"), "keep me\n").unwrap();

    HermesIntegration
        .uninstall(&make_install_ctx(home.path()))
        .unwrap();

    assert!(
        plugin_dir.join("user-notes.txt").exists(),
        "uninstall should not delete unknown files in the tracedecay plugin dir"
    );
    assert!(
        !plugin_dir.join("plugin.yaml").exists(),
        "uninstall should remove tracedecay-generated files"
    );
}

#[test]
fn test_healthcheck_hermes_install_ignores_profile_context() {
    let home = TempDir::new().unwrap();
    let _agent_env = crate::common::AgentEnvLock::pin(&home);
    let project = TempDir::new().unwrap();
    let ctx = InstallContext {
        home: home.path().to_path_buf(),
        tracedecay_bin: "/usr/local/bin/tracedecay".to_string(),
        tool_permissions: expected_tool_perms(),
        project_root: None,
        dashboard: false,
    };
    HermesIntegration.install(&ctx).unwrap();
    assert!(
        home.path()
            .join(".hermes/plugins/tracedecay/plugin.yaml")
            .exists()
    );
    assert!(!home.path().join(".hermes/profiles/work").exists());

    let mut dc = DoctorCounters::new();
    let hctx = HealthcheckContext {
        home: home.path().to_path_buf(),
        project_path: project.path().to_path_buf(),
    };
    HermesIntegration.healthcheck(&mut dc, &hctx);
    assert_eq!(dc.issues, 0, "Hermes user install should have no issues");
    assert_eq!(
        dc.warnings, 0,
        "Hermes healthcheck should check the one user install"
    );
}

#[test]
fn test_hermes_install_ignores_project_root_context() {
    let home = TempDir::new().unwrap();
    let _agent_env = crate::common::AgentEnvLock::pin(&home);
    let pinned = InstallContext {
        home: home.path().to_path_buf(),
        tracedecay_bin: "/usr/local/bin/tracedecay".to_string(),
        tool_permissions: expected_tool_perms(),
        project_root: Some(std::path::PathBuf::from("/pinned/project")),
        dashboard: false,
    };
    HermesIntegration.install(&pinned).unwrap();

    let tools_path = home.path().join(".hermes/plugins/tracedecay/tools.py");
    let config_path = home.path().join(".hermes/config.yaml");
    let tools_py = std::fs::read_to_string(&tools_path).unwrap();
    assert!(!tools_py.contains("PINNED_PROJECT_ROOT"));
    assert!(!tools_py.contains("config_pinned_project_root"));
    let config = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !config.contains("project_root:"),
        "Hermes install context must not create a profile-local project pin:\n{config}"
    );

    // Reinstalls regenerate artifacts without introducing a pin.
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();
    let config = std::fs::read_to_string(&config_path).unwrap();
    assert!(!config.contains("project_root:"));

    // A reinstall after tools.py was deleted remains pin-free.
    std::fs::remove_file(&tools_path).unwrap();
    HermesIntegration
        .install(&make_install_ctx(home.path()))
        .unwrap();
    assert!(tools_path.is_file(), "reinstall must regenerate tools.py");
    let config = std::fs::read_to_string(&config_path).unwrap();
    assert!(!config.contains("project_root:"));

    HermesIntegration
        .uninstall(&make_install_ctx(home.path()))
        .unwrap();
    let config = std::fs::read_to_string(&config_path).unwrap();
    assert!(!config.contains("project_root:"));
}

#[test]
fn test_hermes_generated_python_reads_plugins_tracedecay_config_block() {
    let home = TempDir::new().unwrap();
    HermesIntegration
        .install(&make_install_ctx_with_real_bin(home.path()))
        .unwrap();

    let hermes_home = home.path().join(".hermes");
    let plugin_dir = hermes_home.join("plugins/tracedecay");
    let script = plugin_dir.join("check_config_block.py");
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

plugin_dir = pathlib.Path(sys.argv[1])
hermes_home = plugin_dir.parent.parent
os.environ["HOME"] = str(hermes_home.parent)
os.environ["HERMES_HOME"] = "/ignored/hermes-home"

# Simulate a user (or the installer) putting settings in the conventional
# plugins.tracedecay config block.
import yaml
config_path = hermes_home / "config.yaml"
config = yaml.safe_load(config_path.read_text()) or {}
plugins_cfg = config.setdefault("plugins", {})
plugins_cfg["tracedecay"] = {
    "project_root": "/config/block/project",
    "summary_model": "glm-4.7",
}
config_path.write_text(yaml.dump(config, default_flow_style=False))
healthy_cwd = hermes_home.parent / "healthy-cwd"
healthy_cwd.mkdir()
(healthy_cwd / ".tracedecay").mkdir()

parent_name = "_hermes_user_config_block"
parent_spec = importlib.machinery.ModuleSpec(parent_name, None, is_package=True)
parent_spec.submodule_search_locations = []
sys.modules[parent_name] = importlib.util.module_from_spec(parent_spec)
module_name = f"{parent_name}.tracedecay"
spec = importlib.util.spec_from_file_location(
    module_name,
    plugin_dir / "__init__.py",
    submodule_search_locations=[str(plugin_dir)],
)
plugin = importlib.util.module_from_spec(spec)
sys.modules[module_name] = plugin
spec.loader.exec_module(plugin)

# Legacy project_root is ignored; host-behavior settings remain readable.
assert not hasattr(plugin.tools, "PINNED_PROJECT_ROOT")
assert not hasattr(plugin.tools, "config_pinned_project_root")

captured = {}

class Result:
    returncode = 0
    stdout = "{}"
    stderr = ""

def fake_run(argv, **kwargs):
    captured["argv"] = argv
    return Result()

plugin.tools.subprocess.run = fake_run
plugin.tools.call_tracedecay_tool("tracedecay_status", {})
argv = captured["argv"]
assert "--project" in argv, argv
assert argv[argv.index("--project") + 1] == os.getcwd(), argv
plugin.tools.call_tracedecay_tool("tracedecay_status", {}, cwd=str(healthy_cwd))
argv = captured["argv"]
assert "--project" in argv, argv
assert argv[argv.index("--project") + 1] == str(healthy_cwd), argv

# The context engine filters the legacy pin but layers host-behavior settings.
plugin._resolved_project_scope = lambda path, *_args: (
    str(path)
    if path and (
        os.path.realpath(str(path)) == os.path.realpath(str(healthy_cwd))
        or str(path) == "/host/wins"
    )
    else None
)
engine = plugin.TraceDecayContextEngine()
assert engine.project_root is None, engine.project_root
engine.on_session_start(session_id="s1", cwd=str(healthy_cwd))
assert engine.project_root == str(healthy_cwd), engine.project_root
assert plugin._lcm_str_setting(engine.config, "LCM_SUMMARY_MODEL", "summary_model", default="") == "glm-4.7"

host_engine = plugin.TraceDecayContextEngine(config={"project_root": "/host/wins", "summary_model": "host-model"})
assert host_engine.project_root == "/host/wins"
assert plugin._lcm_str_setting(host_engine.config, "LCM_SUMMARY_MODEL", "summary_model", default="") == "host-model"

# Attribute-style host configs chain through to the block too.
class HostConfig:
    summary_model = None
    fresh_tail_count = 16

attr_engine = plugin.TraceDecayContextEngine(config=HostConfig())
assert plugin._lcm_str_setting(attr_engine.config, "LCM_SUMMARY_MODEL", "summary_model", default="") == "glm-4.7"
assert plugin._configured_int(attr_engine.config, "fresh_tail_count") == 16

# Engines bound to a different profile home do not inherit this block.
other_engine = plugin.TraceDecayContextEngine(hermes_home="/tmp/definitely-missing-hermes-home")
assert other_engine.project_root is None
"#
        ),
    )
    .unwrap();

    let mut check = python_command();
    check
        .arg(&script)
        .arg(&plugin_dir)
        .arg(write_pyyaml_shim(home.path()));
    let output = check
        .output()
        .expect("python3 should run generated Hermes config block check");
    assert!(
        output.status.success(),
        "generated plugin should read the plugins.tracedecay config block\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
