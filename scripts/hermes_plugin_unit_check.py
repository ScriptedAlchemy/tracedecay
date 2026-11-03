#!/usr/bin/env python3
"""Standalone contract checks for the GENERATED Hermes plugin.

Unlike scripts/hermes_stock_check.py (which needs a full stock Hermes
checkout), this harness imports the generated plugin package with a stubbed
plugin context and a fake tracedecay binary, so it runs anywhere Python 3
exists. It asserts the host contracts the 2026-06 review found broken:

  1. TraceDecay leaves the native context engine selected and registers
     LCM retrieval tools independently, scoped to each calling session.
  2. Payloads >128 KiB round-trip through `--args @file` and the spill
     tempfile is cleaned up.
  3. Code-graph tools register WITHOUT the message-forwarding capability
     flag; LCM readers use stored transcripts and forwarded session identity.
  4. The memory provider implements sync_turn / prefetch / on_memory_write
     and calls the right subprocess verbs.
  5. pre_llm_call returns None on non-first turns and on first-turn greetings
     (prompt-cache safety without hijacking small talk).
  6. Hermes host-home state cannot redirect the TraceDecay install, store, or
     project, and removed storage-routing fields stay out of tool schemas.
  7. Exact routes and read-only membership derive from the generated
     schemas.json, and call_tracedecay_json decodes only the shapes
     `tracedecay tool --json` prints (MCP result, retained ApplicationEnvelope).

Usage:
    python3 scripts/hermes_plugin_unit_check.py [plugin_dir]

plugin_dir defaults to a fresh install generated into a temp HOME via the
tracedecay binary named by $TRACEDECAY_BIN
(default: bazel-bin/crates/tracedecay-cli/tracedecay).
"""

import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

PASS = 0


def ok(label, detail=""):
    global PASS
    PASS += 1
    suffix = f" ({detail})" if detail else ""
    print(f"ok {PASS} - {label}{suffix}")


def generate_plugin(work: Path) -> Path:
    """Generates the plugin into a throwaway HOME using the real installer."""
    repo_root = Path(__file__).resolve().parent.parent
    bin_path = Path(
        os.environ.get("TRACEDECAY_BIN", repo_root / "bazel-bin" / "crates" / "tracedecay-cli" / "tracedecay")
    )
    assert bin_path.is_file(), f"tracedecay binary not found at {bin_path}"
    home = work / "home"
    home.mkdir()
    env = dict(os.environ)
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
    # HOME alone does not isolate TraceDecay when the invoking Hermes process
    # exports an explicit profile root. Keep the generated-install fixture on
    # its throwaway profile so it cannot wait on a live profile's skill-store
    # lock or inspect its managed skills.
    env["TRACEDECAY_DATA_DIR"] = str(home / ".tracedecay")
    env["PATH"] = f"{bin_path.parent}{os.pathsep}{env.get('PATH', '')}"
    env.pop("CARGO_TARGET_DIR", None)
    env["HERMES_HOME"] = str(work / "ignored-hermes-host-home")
    subprocess.run(
        [str(bin_path), "install", "--agent", "hermes", "--no-dashboard"],
        check=True,
        env=env,
        capture_output=True,
        text=True,
        timeout=120,
    )
    plugin_dir = home / ".hermes" / "plugins" / "tracedecay"
    assert (plugin_dir / "__init__.py").is_file(), plugin_dir
    assert not (work / "ignored-hermes-host-home").exists()
    return plugin_dir


class StubCtx:
    """Minimal Hermes PluginContext stand-in WITHOUT the message-forwarding
    capability flag (i.e. what stock Hermes looks like to the plugin)."""

    def __init__(self):
        self.tools = {}
        self.hooks = {}
        self.provider = None
        self.engine = None
        self.config = None
        self.skills = {}

    def register_hook(self, name, fn):
        self.hooks[name] = fn

    def register_tool(self, name=None, toolset=None, schema=None, handler=None, **kwargs):
        self.tools[name] = {"toolset": toolset, "schema": schema, "handler": handler}

    def register_memory_provider(self, provider):
        self.provider = provider

    def register_context_engine(self, engine):
        self.engine = engine

    def register_skill(self, name, path):
        self.skills[name] = Path(path)

    def register_config_defaults(self, defaults):
        pass

    def register_command(self, name, fn, description=""):
        pass


class ToolResult:
    returncode = 0
    stdout = "{}"
    stderr = ""


def write_fake_bin(work: Path) -> Path:
    """A fake tracedecay binary that echoes how `--args` reached it."""
    fake = work / "fake-tracedecay.py"
    fake.write_text(
        """#!/usr/bin/env python3
import json, sys
argv = sys.argv[1:]
value = argv[argv.index("--args") + 1]
at_file = value.startswith("@")
payload = open(value[1:], encoding="utf-8").read() if at_file else value
args = json.loads(payload)
inner = json.dumps({
    "status": "ok",
    "at_file": at_file,
    "payload_bytes": len(payload.encode("utf-8")),
    "args_keys": sorted(args.keys()),
})
print(json.dumps({"content": [{"type": "text", "text": inner}]}))
""",
        encoding="utf-8",
    )
    fake.chmod(0o755)
    return fake


def main():
    work = Path(tempfile.mkdtemp(prefix="ts-hermes-plugin-check-"))
    try:
        run_checks(work)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    print(f"1..{PASS}")
    print(f"hermes plugin unit checks: all {PASS} passed")


def _resolve_plugin_dir(work: Path) -> Path:
    if len(sys.argv) > 1:
        plugin_dir = Path(sys.argv[1]).resolve()
    else:
        plugin_dir = generate_plugin(work)
    ok("generated plugin present", str(plugin_dir))
    return plugin_dir


def _import_plugin(work: Path, plugin_dir: Path):
    host_home = plugin_dir.parent.parent
    os.environ["HOME"] = str(host_home.parent)
    os.environ["HERMES_HOME"] = str(work / "ignored-runtime-hermes-home")
    sys.path.insert(0, str(plugin_dir.parent))
    plugin = __import__("tracedecay")
    assert Path(plugin.__file__).resolve() == (plugin_dir / "__init__.py").resolve()
    ok("plugin package imports standalone (no hermes on sys.path)")
    return host_home, plugin


def _check_provenance(plugin_dir: Path):
    header = (plugin_dir / "__init__.py").read_text(encoding="utf-8").splitlines()[0]
    assert header.startswith("# Generated by tracedecay ") and "commit " in header, header
    manifest = (plugin_dir / "plugin.yaml").read_text(encoding="utf-8")
    assert "generator_commit: " in manifest, manifest.splitlines()[:5]
    schemas = json.loads((plugin_dir / "schemas.json").read_text(encoding="utf-8"))
    assert any(schema.get("name") == "tracedecay_search" for schema in schemas), schemas
    assert (plugin_dir / "cli.py").is_file()
    ok("provenance stamp + cli passthrough generated")


def _write_managed_skill_fixtures(plugin_dir: Path) -> Path:
    managed_skill = (
        plugin_dir / "skills" / "agent-managed" / "managed-test" / "SKILL.md"
    )
    managed_skill.parent.mkdir(parents=True)
    managed_skill.write_text("# Managed test\n", encoding="utf-8")
    managed_collision = (
        plugin_dir / "skills" / "agent-managed" / "tracedecay" / "SKILL.md"
    )
    managed_collision.parent.mkdir(parents=True)
    managed_collision.write_text("# Must not replace bundled skill\n", encoding="utf-8")
    return managed_skill


def _check_runtime_workspace_scope(work: Path, plugin) -> Path:
    # The stock Hermes runtime exposes the logical session workspace through
    # TERMINAL_CWD (and, when importable, agent.runtime_cwd). Memory/LCM
    # routing must follow that workspace rather than the gateway process cwd.
    runtime_project = work / "runtime-project"
    runtime_project.mkdir()
    previous_terminal_cwd = os.environ.get("TERMINAL_CWD")
    os.environ["TERMINAL_CWD"] = str(runtime_project)
    try:
        assert plugin._code_project_root() == str(runtime_project)
        runtime_provider = plugin.TracedecayMemoryProvider()
        runtime_provider.initialize(session_id="runtime-cwd")
        assert runtime_provider.project_root is None
    finally:
        if previous_terminal_cwd is None:
            os.environ.pop("TERMINAL_CWD", None)
        else:
            os.environ["TERMINAL_CWD"] = previous_terminal_cwd
    ok("unindexed runtime workspace uses profile-level user memory")
    return runtime_project


def _check_hermes_home_scope(work: Path, plugin, host_home: Path) -> Path:
    registered_root = work / "registered-project"
    registered_child = registered_root / "src" / "nested"
    unrelated_root = work / "unrelated-project"
    registered_child.mkdir(parents=True)
    unrelated_root.mkdir()
    hermes_descendant = host_home / "repos" / "registered-project"
    hermes_descendant.mkdir(parents=True)
    (hermes_descendant / "src").mkdir()
    assert plugin.tools.code_project_root(cwd=str(host_home)) is None
    assert plugin._code_project_root(cwd=str(host_home), hermes_home=str(host_home)) is None
    assert plugin.tools.code_project_root(cwd=str(hermes_descendant)) is None
    assert plugin._code_project_root(
        cwd=str(hermes_descendant), hermes_home=str(host_home)
    ) is None
    missing_home_child = host_home / "missing-project"
    assert plugin._project_scope_resolution(
        str(missing_home_child), str(host_home)
    ) == ("rejected", None)
    tool_argv = []
    tool_run_kwargs = []
    real_tools_run = plugin.tools.subprocess.run
    try:
        def capture_tool_run(argv, **run_kwargs):
            tool_argv.append(argv)
            tool_run_kwargs.append(run_kwargs)
            return ToolResult()
        plugin.tools.subprocess.run = capture_tool_run
        plugin.tools.call_tracedecay_tool(
            "tracedecay_project_search", {"query": "scope"}, cwd=str(host_home)
        )
        assert "--project" not in tool_argv[-1], tool_argv[-1]
        assert tool_argv[-1][1:4] == ["projects", "search", "scope"], tool_argv[-1]
        assert tool_run_kwargs[-1]["cwd"] == os.path.abspath(os.sep)
        plugin.tools.call_tracedecay_tool(
            "tracedecay_project_search", {"query": "scope"}, cwd=str(hermes_descendant)
        )
        assert tool_run_kwargs[-1]["cwd"] == os.path.abspath(os.sep)
        registry_raw = plugin.tools.call_tracedecay_tool(
            "tracedecay_project_list", {"limit": 3}, cwd=str(host_home)
        )
        assert tool_argv[-1][1:3] == ["projects", "list"], tool_argv[-1]
        assert json.loads(json.loads(registry_raw)["content"][0]["text"]) == {}
        plugin.tools.call_tracedecay_tool(
            "tracedecay_project_context", {"project_id": "proj_test"}, cwd=str(host_home)
        )
        assert tool_argv[-1][1:4] == ["projects", "context", "proj_test"]
        active_context_raw = plugin.tools.call_tracedecay_tool(
            "tracedecay_project_context", {}, cwd=str(hermes_descendant)
        )
        assert "--project" not in tool_argv[-1], (tool_argv[-1], active_context_raw)
        assert tool_run_kwargs[-1]["cwd"] == os.path.abspath(os.sep)
        assert "tracedecay_project_context" in tool_argv[-1], tool_argv[-1]
        plugin.tools.call_tracedecay_tool(
            "tracedecay_status", {"small": True}, cwd=str(hermes_descendant)
        )
        assert "--project" not in tool_argv[-1], tool_argv[-1]
        assert tool_run_kwargs[-1]["cwd"] == os.path.abspath(os.sep)
        for name, args in (
            ("tracedecay_fact_store_list", {"memory_scope": "user"}),
            ("tracedecay_lcm_status", {"storage_scope": "user"}),
            (
                "tracedecay_message_search",
                {"query": "general chat", "storage_scope": "user"},
            ),
        ):
            plugin.tools.call_tracedecay_tool(name, args, cwd=str(hermes_descendant))
            assert "--project" not in tool_argv[-1], tool_argv[-1]
            assert tool_run_kwargs[-1]["cwd"] == os.path.abspath(os.sep)
    finally:
        plugin.tools.subprocess.run = real_tools_run
    real_json = plugin.call_tracedecay_json
    try:
        plugin.call_tracedecay_json = lambda *_args, **_kwargs: {"error": "offline"}
        assert plugin._project_scope_resolution(
            str(hermes_descendant), str(host_home)
        ) == ("rejected", None)
        def raise_resolution(*_args, **_kwargs):
            raise RuntimeError("offline")
        plugin.call_tracedecay_json = raise_resolution
        assert plugin._project_scope_resolution(
            str(hermes_descendant), str(host_home)
        ) == ("rejected", None)
        plugin.call_tracedecay_json = lambda *_args, **_kwargs: {
            "project_root": str(registered_root)
        }
        assert plugin._resolved_project_scope(str(registered_child)) == str(registered_root)
        assert plugin._resolved_project_scope(str(unrelated_root)) is None
        assert plugin._resolved_project_scope(str(host_home), str(host_home)) is None
        plugin.call_tracedecay_json = lambda *_args, **_kwargs: {
            "project_root": str(host_home)
        }
        assert plugin._resolved_project_scope(
            str(hermes_descendant / "src"), str(host_home)
        ) is None
        plugin.call_tracedecay_json = lambda *_args, **_kwargs: {
            "project_root": str(hermes_descendant)
        }
        assert plugin._resolved_project_scope(
            str(hermes_descendant / "src"), str(host_home)
        ) is None
    finally:
        plugin.call_tracedecay_json = real_json
    home_provider = plugin.TracedecayMemoryProvider()
    home_provider.initialize(
        session_id="home-scope", hermes_home=str(host_home), cwd=str(host_home)
    )
    assert home_provider.project_root is None
    ok("Hermes home and all descendants remain user scope")
    return hermes_descendant


def _check_registration_split(
    plugin, plugin_dir: Path, work: Path, host_home: Path, managed_skill: Path
):
    # ── 3. Registration split + provider dedup ──────────────────────────
    # The installer wrote memory.provider: tracedecay into the temp profile
    # config, so the provider-owned fact trio must NOT register as direct
    # duplicates; transcript search has no provider twin and stays.
    ctx = StubCtx()
    plugin.register(ctx)
    assert ctx.engine is None, "TraceDecay must not replace Hermes' native compressor"
    assert ctx.provider.hermes_home == str(host_home)
    assert ctx.skills == {
        "managed-test": managed_skill,
        "tracedecay": plugin_dir / "skills" / "tracedecay" / "SKILL.md",
    }, ctx.skills
    ok("bundled and managed skills register through Hermes discovery")
    assert "tracedecay_search" in ctx.tools, sorted(ctx.tools)
    assert "tracedecay_context" in ctx.tools, sorted(ctx.tools)
    assert "tracedecay_message_search" in ctx.tools, sorted(ctx.tools)
    assert "tracedecay_fact_store" not in ctx.tools, sorted(ctx.tools)
    assert not any(name.startswith("tracedecay_fact_store_") for name in ctx.tools), sorted(ctx.tools)
    assert "tracedecay_fact_feedback" not in ctx.tools, sorted(ctx.tools)
    assert "tracedecay_memory_status" not in ctx.tools, sorted(ctx.tools)

    custom_home = work / "custom-hermes-home"
    custom_home.mkdir()
    custom_descendant = custom_home / "repos" / "unregistered"
    custom_descendant.mkdir(parents=True)
    custom_registered = work / "custom-registered-project"
    custom_registered.mkdir()
    custom_ctx = StubCtx()
    custom_ctx.hermes_home = str(custom_home)
    plugin.register(custom_ctx)
    assert custom_ctx.engine is None
    custom_ctx.provider.initialize(session_id="custom-home", cwd=str(custom_home))
    assert custom_ctx.provider.hermes_home == str(custom_home)
    assert custom_ctx.provider.project_root is None
    registered_argv = []
    real_tools_run = plugin.tools.subprocess.run
    real_json = plugin.call_tracedecay_json
    try:
        plugin.tools.subprocess.run = lambda argv, **_kwargs: (
            registered_argv.append(argv) or ToolResult()
        )
        for tool_name, tool_args in (
            ("tracedecay_search", {"query": "scope"}),
            ("tracedecay_status", {}),
            ("tracedecay_runtime", {}),
        ):
            result = custom_ctx.tools[tool_name]["handler"](
                tool_args, cwd=str(custom_home)
            )
            assert "requires a registered project" in result, (tool_name, result)
        assert registered_argv == [], registered_argv
        custom_ctx.tools["tracedecay_project_search"]["handler"](
            {"query": "scope"}, cwd=str(custom_home)
        )
        assert "--project" not in registered_argv[-1], registered_argv[-1]
        assert registered_argv[-1][1:4] == ["projects", "search", "scope"]
        before = len(registered_argv)
        plugin.call_tracedecay_json = lambda *_args, **_kwargs: {
            "project_root": str(custom_home)
        }
        result = custom_ctx.tools["tracedecay_search"]["handler"](
            {"query": "scope"}, cwd=str(custom_descendant)
        )
        assert "requires a registered project" in result, result
        assert len(registered_argv) == before, registered_argv[before:]
        plugin.call_tracedecay_json = lambda *_args, **_kwargs: {
            "project": {"project_root": str(custom_registered)}
        }
        custom_ctx.tools["tracedecay_search"]["handler"](
            {
                "query": "scope",
                "project_selector": {"project_path": str(custom_registered)},
            },
            cwd=str(custom_home),
        )
        assert registered_argv[-1][registered_argv[-1].index("--project") + 1] == str(
            custom_registered
        )
    finally:
        plugin.tools.subprocess.run = real_tools_run
        plugin.call_tracedecay_json = real_json
    ok("registered tools and providers honor a custom Hermes home")
    assert "tracedecay_lcm_compress" not in ctx.tools, sorted(ctx.tools)
    assert "tracedecay_lcm_preflight" not in ctx.tools, sorted(ctx.tools)
    # Readers work through the ordinary registry without taking over compaction.
    assert set(plugin.LCM_TOOL_ALIASES).issubset(ctx.tools), sorted(ctx.tools)
    assert ctx.provider.get_config_schema() == [], ctx.provider.get_config_schema()
    schemas = json.dumps([entry["schema"] for entry in ctx.tools.values()])
    assert "storage_scope" not in schemas and "hermes_home" not in schemas, schemas
    ok("code-graph tools register; provider-owned fact tools dedup", f"{len(ctx.tools)} tools")

    class OtherProviderCtx(StubCtx):
        def __init__(self):
            super().__init__()
            self.config = {"memory": {"provider": "honcho"}}

    other = OtherProviderCtx()
    plugin.register(other)
    assert "tracedecay_fact_store" not in other.tools, sorted(other.tools)
    assert "tracedecay_fact_store_add" in other.tools, sorted(other.tools)
    assert "tracedecay_fact_store_search" in other.tools, sorted(other.tools)
    ok("fact tools register when another memory provider is active")

    class ForwardingCtx(StubCtx):
        context_engine_tool_handlers_receive_messages = True

    fwd = ForwardingCtx()
    plugin.register(fwd)
    assert "tracedecay_lcm_compress" not in fwd.tools, sorted(fwd.tools)
    assert "lcm_grep" in fwd.tools, sorted(fwd.tools)
    ok("LCM readers register regardless of live-message forwarding")
    return ctx


def _check_pre_llm_call_hooks(plugin, ctx):
    # ── 5. pre_llm_call cache safety + small-talk guard ───────────────────
    hook = ctx.hooks["pre_llm_call"]
    assert hook(is_first_turn=False, user_message="Can you debug src/main.rs?") is None
    assert hook() is None
    assert hook(is_first_turn=True) is None
    assert hook(is_first_turn=True, user_message="Hi") is None
    assert hook(is_first_turn=True, user_message="hello!") is None
    assert hook(is_first_turn=True, user_message="What time is it?") is None
    first = hook(is_first_turn=True, user_message="Can you debug this bug in src/main.rs?")
    assert isinstance(first, str) and "tracedecay" in first
    path_first = hook(is_first_turn=True, user_message="Please review scripts/check.py")
    assert isinstance(path_first, str) and "tracedecay" in path_first
    ok("pre_llm_call only nudges first-turn code/project requests")
    saved_names = set(plugin._REGISTERED_TOOL_NAMES)
    plugin._REGISTERED_TOOL_NAMES.clear()
    assert hook(is_first_turn=True) is None
    plugin._REGISTERED_TOOL_NAMES.update(saved_names)
    ok("pre_llm_call stays silent when no tools registered")


def _check_terminal_receipts(
    plugin, ctx, host_home: Path, hermes_descendant: Path, runtime_project: Path
):
    receipt_hook = ctx.hooks["post_tool_call"]
    notifications = []
    real_run = plugin.subprocess.run
    real_thread = plugin.threading.Thread
    real_resolution = plugin._project_scope_resolution
    pending_threads = []
    resolver_calls = []
    class DeferredThread:
        def __init__(self, target, **_kwargs):
            self.target = target
        def start(self):
            pending_threads.append(self.target)
    try:
        plugin.threading.Thread = DeferredThread
        plugin.subprocess.run = lambda argv, **kwargs: notifications.append((argv, kwargs))
        def resolve_receipt(path, *_args):
            resolver_calls.append(path)
            if path and os.path.realpath(str(path)) == os.path.realpath(str(runtime_project)):
                return "registered", str(runtime_project)
            if path and os.path.realpath(str(path)) == os.path.realpath(str(hermes_descendant)):
                return "rejected", None
            return "unregistered", str(path) if path else None
        plugin._project_scope_resolution = resolve_receipt
        assert receipt_hook(tool_name="web_search", cwd=str(runtime_project)) is None
        assert notifications == []
        assert receipt_hook(tool_name="terminal", cwd=str(host_home)) is None
        assert notifications == []
        assert resolver_calls == []
        assert pending_threads == []
        assert receipt_hook(tool_name="terminal", cwd=str(hermes_descendant)) is None
        assert resolver_calls == []
        assert pending_threads == []
        assert notifications == []
        assert receipt_hook(
            tool_name="terminal",
            args={"command": "secret output is deliberately absent"},
            cwd=str(runtime_project),
            session_id="session-1",
            turn_id="turn-1",
            tool_call_id="call-1",
            status="success",
            duration_ms=9,
        ) is None
        assert resolver_calls == []
        assert len(pending_threads) == 1
        pending_threads.pop(0)()
        assert resolver_calls == [str(runtime_project)]
        assert len(notifications) == 1
        argv, call = notifications[0]
        assert argv[-1] == "hook-hermes-terminal-receipt"
        event = json.loads(call["input"])
        assert event["route"]["session_id"] == "session-1"
        assert event["receipt"]["tool_call_id"] == "call-1"
        assert "command" not in call["input"] and "output" not in call["input"]
    finally:
        plugin.subprocess.run = real_run
        plugin.threading.Thread = real_thread
        plugin._project_scope_resolution = real_resolution
    ok("post_tool_call emits bounded asynchronous terminal receipts")


def _check_nudge_kill_switch(plugin, ctx):
    hook = ctx.hooks["pre_llm_call"]
    real_block = plugin.tools.plugin_config_block
    try:
        plugin.tools.plugin_config_block = lambda *_args, **_kwargs: {"nudge": False}
        assert hook(is_first_turn=True, user_message="Can you debug src/main.rs?") is None
    finally:
        plugin.tools.plugin_config_block = real_block
    ok("plugins.tracedecay.nudge kill switch silences the nudge")


def _check_canonical_metadata(plugin, plugin_dir: Path, ctx):
    # Exact routes and read-only membership come from the generated catalog
    # (schemas.json carries readOnlyHint as `read_only`), not from lists
    # maintained beside it.
    generated = json.loads((plugin_dir / "schemas.json").read_text(encoding="utf-8"))
    assert all(isinstance(schema.get("read_only"), bool) for schema in generated), generated[0]
    assert plugin.TOOL_SCHEMAS_BY_NAME.keys() == {schema["name"] for schema in generated}
    assert {"tracedecay_search", "tracedecay_grep", "tracedecay_fact_store_search"} <= (
        plugin.READ_ONLY_TOOL_NAMES
    ), sorted(plugin.READ_ONLY_TOOL_NAMES)
    assert plugin.READ_ONLY_TOOL_NAMES.isdisjoint(
        {"tracedecay_str_replace", "tracedecay_fact_store_add", "tracedecay_fact_store_curate"}
    ), sorted(plugin.READ_ONLY_TOOL_NAMES)
    assert set(plugin.FACT_STORE_EXACT_ROUTES) >= set(plugin.MEMORY_FACT_ACTIONS.values())
    assert all(
        route == f"tracedecay_fact_store_{action}" and route in plugin.TOOL_SCHEMAS_BY_NAME
        for action, route in plugin.FACT_STORE_EXACT_ROUTES.items()
    ), plugin.FACT_STORE_EXACT_ROUTES
    collapsed = plugin._collapsed_fact_store_schema()
    assert collapsed["parameters"]["properties"]["action"]["enum"] == sorted(
        plugin.FACT_STORE_EXACT_ROUTES
    )
    assert plugin.MISSING_MEMORY_PROVIDER_TOOLS == (), plugin.MISSING_MEMORY_PROVIDER_TOOLS
    ok("exact routes and read-only membership derive from schemas.json")

    # The mutating/read-only gate for explicit registered-project selectors
    # follows that annotation.
    refused = ctx.tools["tracedecay_str_replace"]["handler"](
        {"path": "src/lib.rs", "old": "a", "new": "b", "project_id": "proj_other"}
    )
    assert "does not permit a cross-project mutating selector" in refused, refused
    ok("catalog readOnlyHint gates cross-project selectors")

    # A canonical schema this catalog does not advertise is an explicit gap:
    # omitted from the provider's schemas, refused on dispatch, never a
    # valid-looking empty schema.
    feedback_schema = plugin.TOOL_SCHEMAS_BY_NAME.pop("tracedecay_fact_feedback")
    try:
        assert plugin._memory_schema("tracedecay_fact_feedback", "fact_feedback") is None
        names = [schema["name"] for schema in ctx.provider.get_tool_schemas()]
        assert names == ["fact_store", "memory_status"], names
        refused = ctx.provider.handle_tool_call("fact_feedback", {"fact_id": 1})
        assert "does not advertise" in refused, refused
    finally:
        plugin.TOOL_SCHEMAS_BY_NAME["tracedecay_fact_feedback"] = feedback_schema
    restored = plugin._memory_schema("tracedecay_fact_feedback", "fact_feedback")
    assert restored["parameters"] == feedback_schema["parameters"]
    assert restored["parameters"] is not feedback_schema["parameters"]
    ok("missing canonical metadata is visible, not papered over")


def _check_response_envelope_table(plugin):
    # `tracedecay tool --json` prints the daemon's MCP result; its one JSON
    # content block is either the tool's own result (compatibility tools) or
    # a retained ApplicationEnvelope whose result is outcome.value.payload.
    # The CLI reassembles truncated payloads itself, so the plugin never
    # dereferences handles: the decoder is one flat pass, not a recursive guess.
    real_tool = plugin.tools.call_tracedecay_tool
    bridge_calls = []
    responses = {}

    def mcp_result(*texts):
        return json.dumps({"content": [{"type": "text", "text": text} for text in texts]})

    def envelope(payload, outcome="evidence"):
        return {
            "contract": {
                "schema_id": "schema.application.retained.lcm-status.result",
                "schema_revision": 1,
            },
            "request_id": "request.check",
            "scope": {"project_id": "project.check"},
            "outcome": {"outcome": outcome, "value": {"page": {}, "payload": payload}},
        }

    def _scripted(name, args, **kwargs):
        bridge_calls.append((name, args, kwargs))
        return responses[name]

    try:
        plugin.tools.call_tracedecay_tool = _scripted
        status_payload = {"status": "not_ingested", "provider": "hermes", "session_id": "s"}
        responses["tracedecay_lcm_status"] = mcp_result(json.dumps(envelope(status_payload)))
        assert plugin.call_tracedecay_json("tracedecay_lcm_status", {}) == status_payload
        responses["tracedecay_fact_store_curate"] = mcp_result(
            json.dumps(envelope({"run_id": "run.1"}, outcome="effect"))
        )
        assert plugin.call_tracedecay_json("tracedecay_fact_store_curate", {}) == {
            "run_id": "run.1"
        }
        ok("retained application envelopes unwrap to outcome.value.payload")

        # Adversarial payload keys are not envelope evidence: a plain result
        # that happens to carry status/answer/content/outcome stays intact and
        # its string fields are never re-parsed.
        plain = {
            "status": "ok",
            "answer": "42",
            "content": json.dumps({"nested": True}),
            "outcome": "evidence",
        }
        responses["tracedecay_status"] = mcp_result(json.dumps(plain))
        assert plugin.call_tracedecay_json("tracedecay_status", {}) == plain
        ok("compatibility payloads are returned as-is without key heuristics")

        # A prose trailer block beside the JSON block (stale-graph freshness
        # note) is skipped, not concatenated into the payload.
        responses["tracedecay_status"] = mcp_result(
            json.dumps(plain), "\ncode_graph_freshness: stale, serving generation g1"
        )
        assert plugin.call_tracedecay_json("tracedecay_status", {}) == plain
        ok("prose content trailers do not disturb the JSON payload")

        # A truncation envelope is a CLI-level failure (non-zero exit) and
        # reaches the plugin as tools.error_payload; the plugin issues no
        # tracedecay_retrieve follow-up of its own.
        responses["tracedecay_fact_store_search"] = plugin.tools.error_payload(
            "tracedecay tool exited with status 1"
        )
        bridge_calls.clear()
        refused = plugin.call_tracedecay_json("tracedecay_fact_store_search", {"query": "q"})
        assert refused == {"error": "tracedecay tool exited with status 1"}, refused
        assert [call[0] for call in bridge_calls] == ["tracedecay_fact_store_search"]
        ok("CLI-refused truncation stays a typed error with no plugin retrieve")

        # Malformed shapes are explicit refusals, not empty successes.
        responses["tracedecay_lcm_status"] = mcp_result(
            json.dumps(envelope(None))
        )
        missing_payload = plugin.call_tracedecay_json("tracedecay_lcm_status", {})
        assert "omitted its result payload" in missing_payload["error"], missing_payload
        responses["tracedecay_lcm_status"] = mcp_result(json.dumps(plain), json.dumps(plain))
        two_payloads = plugin.call_tracedecay_json("tracedecay_lcm_status", {})
        assert "returned 2 JSON payloads" in two_payloads["error"], two_payloads
        responses["tracedecay_lcm_status"] = json.dumps(envelope(status_payload))
        bare = plugin.call_tracedecay_json("tracedecay_lcm_status", {})
        assert bare["error"] == "tracedecay tool response missing text content", bare
        ok("envelopes without a payload, extra payloads, or no content block are refused")
    finally:
        plugin.tools.call_tracedecay_tool = real_tool


def _check_lcm_retrieval_scope(plugin, ctx, host_home: Path, runtime_project: Path):
    assert ctx.engine is None, "TraceDecay must leave native compaction enabled"
    calls = []
    real_tool = plugin.tools.call_tracedecay_tool
    real_resolution = plugin._project_scope_resolution
    try:
        plugin._project_scope_resolution = lambda path, *_args: (
            ("rejected", None) if str(path) == str(host_home) else ("registered", str(path))
        )
        plugin.tools.call_tracedecay_tool = lambda name, args, **kwargs: (
            calls.append((name, args, kwargs)) or "{}"
        )
        handler = ctx.tools["lcm_grep"]["handler"]
        handler({"query": "orchard"}, session_id="project-session", cwd=str(runtime_project))
        handler({"query": "orchard"}, session_id="chat-session", cwd=str(host_home))
        handler({"query": "orchard"}, session_id="project-session", cwd=str(runtime_project))
        assert [args["session_id"] for _, args, _ in calls] == [
            "project-session", "chat-session", "project-session",
        ]
        assert calls[0][2] == calls[2][2] == {"project_root": str(runtime_project)}
        assert calls[1][2] == {}
        assert calls[1][1]["storage_scope"] == "user"
        missing = json.loads(handler({"query": "orchard"}, cwd=str(runtime_project)))
        assert "session_id" in missing["error"]
        assert len(calls) == 3, "missing session identity must never reuse a previous session"
        ctx.tools["lcm_status"]["handler"]({}, session_id="status-session", cwd=str(runtime_project))
        assert calls[-1] == (
            "tracedecay_lcm_status",
            {"provider": "hermes", "session_id": "status-session"},
            {"project_root": str(runtime_project)},
        )
    finally:
        plugin.tools.call_tracedecay_tool = real_tool
        plugin._project_scope_resolution = real_resolution
    ok("retrieval tools retain per-call session/project scope without replacing compaction")
    return [
        {"role": "user", "content": "hello"},
        {"role": "assistant", "content": "hi"},
    ]


def _check_expand_query_degradation(plugin):
    # Expand-query synthesis must degrade (retrieval intact) on ANY
    # auxiliary failure, not just TimeoutError.
    class _BoomClient:
        def call_llm(self, **kwargs):
            raise RuntimeError("provider exploded")

    retrieval = {
        "status": "ok",
        "needs_synthesis": True,
        "prompt": "q",
        "matches": [{"node_id": 1}],
        "context_blocks": [],
        "synthesis_prompt": {},
    }
    degraded = plugin._synthesize_expand_query_payload(
        dict(retrieval), agent=type("A", (), {"auxiliary_client": _BoomClient()})()
    )
    assert degraded["degraded"] is True, degraded
    assert "provider exploded" in degraded["error"], degraded
    assert degraded["matches"] == retrieval["matches"]
    ok("expand-query synthesis degrades on RuntimeError with retrieval intact")


def _check_no_hermes_storage_routing(plugin):
    # Hermes host paths are host configuration only. They never become a
    # TraceDecay storage selector or fallback project identity.
    assert not hasattr(plugin, "_storage_args")
    ok("LCM/memory exposes no Hermes profile storage routing")


def _check_provider_verbs(plugin, ctx, host_home: Path, messages: list):
    # ── 4. provider hooks call the right verbs ───────────────────────────
    provider = ctx.provider
    assert provider is not None
    provider.initialize("check-session", hermes_home=str(host_home))
    expected_project_root = plugin._runtime_working_directory()
    assert provider.project_root is None
    assert provider.project_root != str(host_home)
    provider.project_root = expected_project_root
    schema_names = [schema["name"] for schema in provider.get_tool_schemas()]
    assert schema_names == ["fact_store", "fact_feedback", "memory_status"], schema_names
    ok("memory schemas collapsed to 3")

    calls = []
    transcript_calls = []
    real_tool = plugin.tools.call_tracedecay_tool
    real_json = plugin.call_tracedecay_json
    try:
        plugin.tools.call_tracedecay_tool = lambda name, args, **kw: (
            calls.append((name, args, kw)) or "{}"
        )
        plugin.call_tracedecay_json = lambda name, args, **kw: (
            transcript_calls.append((name, args, kw))
            or {"status": "committed"}
        )

        provider.handle_tool_call("fact_store", {"action": "search", "query": "rust"})
        name, args, kwargs = calls[-1]
        assert name == "tracedecay_fact_store_search", calls
        assert "action" not in args, args
        assert "project_root" not in args, args
        assert args["memory_scope"] == "project", args
        assert kwargs["project_root"] == expected_project_root, kwargs
        ok("memory tool calls stay bound to the provider's session project")

        provider.handle_tool_call(
            "fact_store",
            {"action": "add", "content": "canonical field", "category": "decision"},
        )
        name, args, _kwargs = calls[-1]
        assert name == "tracedecay_fact_store_add", calls
        assert "action" not in args, args
        assert args["category"] == "decision" and "fact_type" not in args, args
        ok("fact_store add routes category to the exact fact-store route")

        before = len(transcript_calls)
        provider.sync_turn("u", "a", session_id="other-session", messages=messages)
        turn_calls = transcript_calls[before:]
        assert len(turn_calls) == 2, turn_calls
        user_name, user_args, user_kwargs = turn_calls[0]
        assert user_name == "tracedecay_hook_runtime"
        assert user_args["action"] == "ingest_transcript"
        assert user_args["user_scope"] is True, user_args
        assert "storage_scope" not in user_args, user_args
        assert "project_root" not in user_kwargs, user_kwargs
        name, args, kwargs = turn_calls[1]
        assert name == "tracedecay_hook_runtime", transcript_calls
        assert args["action"] == "ingest_transcript"
        assert args["session_id"] == "other-session"
        assert [message["content"] for message in args["messages"]] == ["u", "a"]
        assert all(message.get("id") for message in args["messages"])
        assert [message["id"] for message in user_args["messages"]] == [
            message["id"] for message in args["messages"]
        ]
        assert all(
            message["associated_project_roots"] == [expected_project_root]
            for message in args["messages"]
        )
        assert "project_root" not in args, args
        assert kwargs["project_root"] == expected_project_root, kwargs

        ok("sync_turn stores a canonical user turn plus its project projection")

        before = len(transcript_calls)
        provider.sync_turn("only user", "and assistant", session_id="s2", messages=None)
        turn_calls = transcript_calls[before:]
        assert len(turn_calls) == 2, turn_calls
        assert turn_calls[0][1]["user_scope"] is True, turn_calls
        assert "project_root" not in turn_calls[0][2], turn_calls
        name, args, kwargs = turn_calls[1]
        assert name == "tracedecay_hook_runtime", transcript_calls
        assert args["messages"][0]["content"] == "only user"
        assert args["messages"][1]["content"] == "and assistant"
        assert "project_root" not in args, args
        assert kwargs["project_root"] == expected_project_root, kwargs
        ok("sync_turn synthesizes a turn when messages are missing")

        provider.on_memory_write("add", "user", "likes rust", {"session_id": "s"})
        name, args, kwargs = calls[-1]
        assert name == "tracedecay_fact_store_add", calls
        assert "action" not in args, args
        assert args["category"] == "user_pref"
        assert args["memory_scope"] == "user"
        assert args["metadata"]["hermes_action"] == "add"
        assert "project_root" not in args, args
        assert "project_root" not in kwargs, kwargs
        before = len(calls)
        provider.on_memory_write("remove", "memory", "anything")
        assert len(calls) == before
        ok("on_memory_write mirrors adds and skips removals")

        provider.project_root = None
        before = len(transcript_calls)
        provider.sync_turn("u", "a", session_id="untethered", messages=messages)
        assert len(transcript_calls) == before + 1
        name, args, kwargs = transcript_calls[-1]
        assert name == "tracedecay_hook_runtime"
        assert args["user_scope"] is True, args
        assert "project_root" not in kwargs, kwargs
        provider.handle_tool_call("fact_store", {"action": "add", "content": "pref"})
        name, args, kwargs = calls[-1]
        assert args["memory_scope"] == "user", args
        assert "project_root" not in kwargs, kwargs
        ok("untethered memory and LCM use profile-level user scope")

        real_resolver = plugin._resolved_project_scope
        plugin._resolved_project_scope = lambda path, *_args: expected_project_root
        before = len(transcript_calls)
        provider.sync_turn(
            "project task",
            "done",
            session_id="tool-routed",
            messages=[
                {"role": "user", "content": "work on the repo"},
                {
                    "role": "assistant",
                    "tool_calls": [{
                        "function": {
                            "name": "tracedecay_grep",
                            "arguments": json.dumps({"project_path": expected_project_root}),
                        }
                    }],
                },
            ],
        )
        turn_calls = transcript_calls[before:]
        assert len(turn_calls) == 2, turn_calls
        assert turn_calls[0][1]["user_scope"] is True, turn_calls
        assert "project_root" not in turn_calls[0][2], turn_calls
        name, args, kwargs = turn_calls[1]
        assert name == "tracedecay_hook_runtime"
        assert kwargs["project_root"] == expected_project_root
        assert [message["id"] for message in turn_calls[0][1]["messages"]] == [
            message["id"] for message in args["messages"]
        ]
        plugin._resolved_project_scope = real_resolver
        ok("structured tool activity correlates an untethered turn to its project")
        provider.project_root = expected_project_root

        # Non-primary execution contexts must not write turn state.
        before = len(transcript_calls)
        provider.agent_context = "cron"
        provider.sync_turn("u", "a", session_id="cron-session", messages=messages)
        assert len(transcript_calls) == before, transcript_calls[before:]
        provider.agent_context = ""
        ok("sync_turn skips cron/flush execution contexts")
    finally:
        plugin.tools.call_tracedecay_tool = real_tool
        plugin.call_tracedecay_json = real_json


def _check_prefetch_cache(plugin, ctx):
    provider = ctx.provider
    real_json = plugin.call_tracedecay_json
    try:
        # Real search responses nest each row under "fact" (with scores
        # beside it); flat rows must keep working too.
        plugin.call_tracedecay_json = lambda name, args, **kw: {
            "hits": [
                {"fact": {"fact_id": 7, "content": "zack prefers rust"}, "score": 0.9},
                {"fact_id": 8, "content": "flat row"},
            ],
            "owner": {"kind": "user"},
        }
        # prefetch() is the fast inline half: it only serves what
        # queue_prefetch() recalled in the background after the last turn.
        assert provider.prefetch("rust preferences") == ""
        provider.queue_prefetch("rust preferences", session_id="check-session")
        deadline = time.time() + 10
        text = ""
        while time.time() < deadline:
            text = provider.prefetch("rust preferences", session_id="check-session")
            if text:
                break
            time.sleep(0.05)
        assert "zack prefers rust" in text and "[user fact 7]" in text, text
        assert "flat row" in text and "[user fact 8]" in text, text
        # Consumed on read: the next prefetch starts empty again.
        assert provider.prefetch("rust preferences", session_id="check-session") == ""
        plugin.call_tracedecay_json = lambda name, args, **kw: {"error": "nope"}
        assert provider._recall_facts("anything") == ""
        assert provider._recall_facts("") == ""
        ok("queue_prefetch fills the cache; prefetch serves and clears it")
    finally:
        plugin.call_tracedecay_json = real_json


def _check_system_prompt_block(ctx):
    provider = ctx.provider
    block = provider.system_prompt_block()
    assert isinstance(block, str) and "fact_store" in block
    ok("system_prompt_block is static provider guidance")


def _check_args_file_spill(plugin, work: Path):
    # ── 2. --args @file spill round-trip ─────────────────────────────────
    fake_bin = write_fake_bin(work)
    spill_dir = work / "spill"
    spill_dir.mkdir()
    real_bin = plugin.tools.TRACEDECAY_BIN
    real_tmp = tempfile.tempdir
    try:
        plugin.tools.TRACEDECAY_BIN = str(fake_bin)
        tempfile.tempdir = str(spill_dir)

        big = "x" * (200 * 1024)
        raw = plugin.tools.call_tracedecay_tool(
            "tracedecay_hook_runtime",
            {
                "action": "ingest_transcript",
                "provider": "hermes",
                "session_id": "s",
                "messages": [{"role": "user", "content": big}],
            },
        )
        outer = json.loads(raw)
        inner = json.loads(outer["content"][0]["text"])
        assert inner["at_file"] is True, inner
        assert inner["payload_bytes"] > 200 * 1024, inner
        assert "messages" in inner["args_keys"], inner
        leftovers = list(spill_dir.iterdir())
        assert leftovers == [], leftovers
        ok("payload >128KiB round-trips via --args @file and cleans up")

        raw = plugin.tools.call_tracedecay_tool("tracedecay_status", {"small": True})
        inner = json.loads(json.loads(raw)["content"][0]["text"])
        assert inner["at_file"] is False, inner
        assert "format" in inner["args_keys"], inner
        ok("small payloads stay inline on argv")
    finally:
        plugin.tools.TRACEDECAY_BIN = real_bin
        tempfile.tempdir = real_tmp


_ISOLATED_PROFILE_ENV = (
    "HOME",
    "USERPROFILE",
    "TRACEDECAY_DATA_DIR",
    "TRACEDECAY_GLOBAL_DB",
    "TRACEDECAY_DAEMON_SOCKET",
)


def _tool_result_still_starting(result) -> bool:
    if not isinstance(result, dict) or "error" not in result:
        return False
    text = json.dumps(result)
    return "is warming in the background" in text or "daemon.sock" in text


def _call_until_tool_ready(call):
    # `tracedecay tool` answers the typed warming state until the isolated
    # profile's project is open. That retry is the client protocol, same as
    # the CLI bridge test.
    deadline = time.monotonic() + 60
    result = call()
    while _tool_result_still_starting(result):
        if time.monotonic() >= deadline:
            raise AssertionError(result)
        time.sleep(0.25)
        result = call()
    return result


def _session_message_count(status, session_id: str) -> int:
    assert isinstance(status, dict) and "error" not in status, status
    assert status.get("status") == "ok", status
    assert status.get("session_id") == session_id, status
    count = status["lcm"]["raw_message_count"]
    assert isinstance(count, int) and not isinstance(count, bool), status
    return count


def _stop_daemon(proc: subprocess.Popen):
    if proc.poll() is not None:
        return
    proc.send_signal(signal.SIGTERM)
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.send_signal(signal.SIGKILL)
        proc.wait(timeout=5)


def _check_hermes_lcm_session_census(plugin, work: Path):
    """One ingested Hermes session shows its message; a session that was not
    ingested shows none. Status is asked only for the session id."""
    home = work / "home"
    profile = home / ".tracedecay"
    profile.mkdir(parents=True, exist_ok=True)
    saved_env = {key: os.environ.get(key) for key in _ISOLATED_PROFILE_ENV}
    os.environ["HOME"] = str(home)
    os.environ["USERPROFILE"] = str(home)
    os.environ["TRACEDECAY_DATA_DIR"] = str(profile)
    os.environ["TRACEDECAY_GLOBAL_DB"] = str(profile / "global.db")
    os.environ["TRACEDECAY_DAEMON_SOCKET"] = str(profile / "daemon.sock")
    daemon_env = os.environ.copy()
    daemon_env["TRACEDECAY_TEST_ALLOW_INCOMPLETE_HOLDER_SCAN"] = "1"
    daemon = subprocess.Popen(
        [plugin.tools.TRACEDECAY_BIN, "daemon", "run"],
        env=daemon_env,
        cwd=str(home),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        deadline = time.monotonic() + 10
        while not (profile / "daemon.sock").exists():
            if daemon.poll() is not None:
                raise AssertionError(daemon.stderr.read() if daemon.stderr else "daemon exited")
            if time.monotonic() >= deadline:
                raise AssertionError("isolated daemon socket did not appear")
            time.sleep(0.05)
        ingested = _call_until_tool_ready(
            lambda: plugin.call_tracedecay_json(
                "tracedecay_hook_runtime",
                {
                    "action": "ingest_transcript",
                    "provider": "hermes",
                    "session_id": "gate-hermes-session",
                    "user_scope": True,
                    "format": "json",
                    "messages": [
                        {
                            "id": "gate-hermes-message",
                            "role": "user",
                            "content": "orchard note",
                        }
                    ],
                },
            )
        )
        assert ingested.get("status") in ("accepted", "committed"), ingested
        present = _call_until_tool_ready(
            lambda: plugin.call_tracedecay_json("tracedecay_lcm_status", {
                "provider": "hermes", "session_id": "gate-hermes-session", "storage_scope": "user",
            })
        )
        absent = _call_until_tool_ready(
            lambda: plugin.call_tracedecay_json("tracedecay_lcm_status", {
                "provider": "hermes", "session_id": "gate-empty-session", "storage_scope": "user",
            })
        )
        assert _session_message_count(present, "gate-hermes-session") == 1, present
        assert _session_message_count(absent, "gate-empty-session") == 0, absent
        ok("LCM status counts the ingested session only")
    finally:
        _stop_daemon(daemon)
        for key, value in saved_env.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value


def run_checks(work: Path):
    plugin_dir = _resolve_plugin_dir(work)
    host_home, plugin = _import_plugin(work, plugin_dir)
    _check_provenance(plugin_dir)
    managed_skill = _write_managed_skill_fixtures(plugin_dir)
    runtime_project = _check_runtime_workspace_scope(work, plugin)
    hermes_descendant = _check_hermes_home_scope(work, plugin, host_home)
    ctx = _check_registration_split(plugin, plugin_dir, work, host_home, managed_skill)
    _check_pre_llm_call_hooks(plugin, ctx)
    _check_terminal_receipts(plugin, ctx, host_home, hermes_descendant, runtime_project)
    _check_nudge_kill_switch(plugin, ctx)
    _check_canonical_metadata(plugin, plugin_dir, ctx)
    _check_response_envelope_table(plugin)
    messages = _check_lcm_retrieval_scope(plugin, ctx, host_home, runtime_project)
    _check_expand_query_degradation(plugin)
    _check_no_hermes_storage_routing(plugin)
    _check_provider_verbs(plugin, ctx, host_home, messages)
    _check_prefetch_cache(plugin, ctx)
    _check_system_prompt_block(ctx)
    _check_args_file_spill(plugin, work)
    _check_hermes_lcm_session_census(plugin, work)


if __name__ == "__main__":
    main()
