#!/usr/bin/env python3
"""Standalone contract checks for the GENERATED Hermes plugin.

Unlike scripts/hermes_stock_check.py (which needs a full stock Hermes
checkout), this harness imports the generated plugin package with a stubbed
plugin context and a fake tracedecay binary, so it runs anywhere Python 3
exists. It asserts the host contracts the 2026-06 review found broken:

  1. compress() returns a MESSAGE LIST on success / no-op / error
     (hermes ContextEngine ABC), never the raw result dict.
  2. Payloads >128 KiB round-trip through `--args @file` and the spill
     tempfile is cleaned up.
  3. Code-graph tools register WITHOUT the message-forwarding capability
     flag; the messages-dependent LCM verbs stay gated.
  4. The memory provider implements sync_turn / prefetch / on_memory_write
     and calls the right subprocess verbs.
  5. pre_llm_call returns None on non-first turns and on first-turn greetings
     (prompt-cache safety without hijacking small talk).
  6. Hermes host-home state cannot redirect the TraceDecay install, store, or
     project, and removed storage-routing fields stay out of tool schemas.

Usage:
    python3 scripts/hermes_plugin_unit_check.py [plugin_dir]

plugin_dir defaults to a fresh install generated into a temp HOME via the
tracedecay binary named by $TRACEDECAY_BIN (default: target/debug/tracedecay).
"""

import copy
import json
import os
import shutil
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
        os.environ.get("TRACEDECAY_BIN", repo_root / "target" / "debug" / "tracedecay")
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
    assert plugin.STANDARD_HERMES_LCM_PROVIDER == "hermes"
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
    skill = (plugin_dir / "skills" / "tracedecay" / "SKILL.md").read_text(
        encoding="utf-8"
    )
    assert "normal user-profile installation" in skill, skill
    assert "current schemas and are rejected" in skill, skill
    ok("provenance stamp + cli passthrough + storage guidance generated")


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
            ("tracedecay_fact_store", {"action": "list", "memory_scope": "user"}),
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
    home_engine = plugin.TraceDecayContextEngine(hermes_home=str(host_home))
    home_engine.on_session_start(session_id="home-scope", cwd=str(host_home))
    assert home_engine.project_root is None
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
    assert ctx.engine.hermes_home == str(host_home), (
        ctx.engine.hermes_home,
        str(host_home),
    )
    assert ctx.skills == {
        "managed-test": managed_skill,
        "tracedecay": plugin_dir / "skills" / "tracedecay" / "SKILL.md",
    }, ctx.skills
    ok("bundled and managed skills register through Hermes discovery")
    assert "tracedecay_search" in ctx.tools, sorted(ctx.tools)
    assert "tracedecay_context" in ctx.tools, sorted(ctx.tools)
    assert "tracedecay_message_search" in ctx.tools, sorted(ctx.tools)
    assert "tracedecay_fact_store" not in ctx.tools, sorted(ctx.tools)
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
    assert custom_ctx.engine.hermes_home == str(custom_home)
    custom_ctx.engine.on_session_start(session_id="custom-home", cwd=str(custom_home))
    assert custom_ctx.engine.project_root is None
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
    # Context-engine native mirrors stay gated without the capability flag.
    assert "lcm_grep" not in ctx.tools, sorted(ctx.tools)
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
    assert "tracedecay_fact_store" in other.tools, sorted(other.tools)
    ok("fact tools register when another memory provider is active")

    class ForwardingCtx(StubCtx):
        context_engine_tool_handlers_receive_messages = True

    fwd = ForwardingCtx()
    plugin.register(fwd)
    assert "tracedecay_lcm_compress" in fwd.tools, sorted(fwd.tools)
    assert "lcm_grep" in fwd.tools, sorted(fwd.tools)
    ok("LCM live-ingest verbs register when the host forwards messages")
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


def _check_response_handle_deref(plugin):
    # Response handles are a generic MCP transport feature, not LCM-only.
    # Large fact-store searches must dereference their handle before the
    # Hermes memory provider tries to read count/facts from the payload.
    real_tool = plugin.tools.call_tracedecay_tool
    bridge_calls = []
    try:
        def _handled_response(name, args, **kwargs):
            bridge_calls.append((name, args, kwargs))
            if name == "tracedecay_retrieve":
                payload = {
                    "count": 1,
                    "facts": [{"fact": {"fact_id": 7, "content": "remember me"}}],
                }
            else:
                payload = {
                    "truncated": True,
                    "handle": "rh_fact_search",
                    "retrieve_tool": "tracedecay_retrieve",
                    "preview": "{\"count\":1",
                }
            return json.dumps({"content": [{"type": "text", "text": json.dumps(payload)}]})

        plugin.tools.call_tracedecay_tool = _handled_response
        resolved = plugin.call_tracedecay_json(
            "tracedecay_fact_store",
            {
                "action": "search",
                "query": "remember",
                "project_selector": {"path": "/tmp/selected-project"},
            },
        )
        assert resolved.get("count") == 1, resolved
        assert [call[0] for call in bridge_calls] == [
            "tracedecay_fact_store",
            "tracedecay_retrieve",
        ], bridge_calls
        assert bridge_calls[1][1]["project_selector"] == {
            "path": "/tmp/selected-project"
        }, bridge_calls
        ok("generic response handles dereference for memory-provider results")
    finally:
        plugin.tools.call_tracedecay_tool = real_tool


def _check_engine_state_and_compress(
    plugin, ctx, host_home: Path, runtime_project: Path
) -> list:
    # ── 1. compress() message-list contract ──────────────────────────────
    engine = ctx.engine
    assert engine is not None
    engine.initialize(session_id="check-session", hermes_home=str(host_home))
    engine.project_root = str(runtime_project)
    engine.context_length = 200_000
    engine.threshold_tokens = 150_000
    engine.agent = object()
    cloned_engine = copy.deepcopy(engine)
    assert cloned_engine is not engine
    assert cloned_engine._state_lock is not engine._state_lock
    assert cloned_engine.project_root == str(runtime_project)
    assert cloned_engine.context_length == 200_000
    assert cloned_engine.threshold_tokens == 150_000
    assert cloned_engine.agent is None
    cloned_engine.context_length = 100_000
    assert engine.context_length == 200_000
    ok("context engine safely deep-copies per-agent budget state")

    real_resolver = plugin._resolved_project_scope
    try:
        plugin._resolved_project_scope = lambda path, *_args: (
            str(runtime_project)
            if path
            and os.path.realpath(str(path)) == os.path.realpath(str(runtime_project))
            else None
        )
        engine.initialize(session_id="project-session", project_root=str(runtime_project))
        assert engine.project_root == str(runtime_project)
        engine.initialize(session_id="untethered-session", cwd=str(host_home))
        assert engine.project_root is None
        engine.initialize(session_id="project-session")
        assert engine.project_root == str(runtime_project)
        engine.initialize(session_id="untethered-session")
        assert engine.project_root is None
    finally:
        plugin._resolved_project_scope = real_resolver
    ok("context engine isolates project routing per Hermes session")

    messages = [
        {"role": "user", "content": "hello"},
        {"role": "assistant", "content": "hi"},
    ]
    replay = [{"role": "user", "content": "[summary] hello/hi"}]

    real_json = plugin.call_tracedecay_json
    try:
        plugin.call_tracedecay_json = lambda name, args, **kw: {
            "status": "ok",
            "reason": "compressed",
            "replay_messages": list(replay),
        }
        out = engine.compress(list(messages), current_tokens=10)
        assert out == replay, out
        assert all(isinstance(m, dict) and m.get("role") for m in out)
        assert engine._last_compress_aborted is False
        assert engine.last_compress_result.get("status") == "ok"
        ok("compress() returns replay message list on success")

        plugin.call_tracedecay_json = lambda name, args, **kw: {
            "status": "ok",
            "reason": "below_threshold",
            "replay_messages": list(messages),
        }
        out = engine.compress(list(messages), current_tokens=10)
        assert out == messages, out
        ok("compress() no-op returns the input list (host skips rotation)")

        plugin.call_tracedecay_json = lambda name, args, **kw: {"error": "boom"}
        out = engine.compress(list(messages), current_tokens=10)
        assert out == messages, out
        assert engine._last_compress_aborted is True
        assert "boom" in str(engine._last_summary_error)
        ok("compress() error returns the input list and flags the abort")

        def _raise(name, args, **kw):
            raise RuntimeError("subprocess exploded")

        plugin.call_tracedecay_json = _raise
        out = engine.compress(list(messages), current_tokens=10)
        assert out == messages and engine._last_compress_aborted is True
        ok("compress() exception degrades to the input list")
    finally:
        plugin.call_tracedecay_json = real_json

    # Host-contract token attrs (minimum-context guard reads these).
    engine.update_model("check-model", 128000)
    assert engine.context_length == 128000
    assert 0 < engine.threshold_tokens <= 128000
    ok("update_model populates context_length/threshold_tokens")
    return messages


def _check_engine_thread_isolation(engine):
    # Per-session isolation: the singleton engine must not leak one
    # conversation's state into another (gateway sessions, delegate_task
    # children all share this instance).
    import threading as _threading

    seen = {}

    def _other_session():
        engine.on_session_start(session_id="check-session-b")
        engine.update_model("other-model", 64000)
        engine.update_from_response({"prompt_tokens": 7, "completion_tokens": 3})
        seen["b_context"] = engine.context_length
        seen["b_total"] = engine.last_total_tokens

    worker = _threading.Thread(target=_other_session)
    worker.start()
    worker.join(timeout=10)
    assert seen == {"b_context": 64000, "b_total": 10}, seen
    # The original thread stays bound to its own session.
    assert engine.context_length == 128000, engine.context_length
    assert engine.model == "check-model", engine.model
    assert engine._last_compress_aborted is True  # from the exception check above
    ok("per-session engine state stays isolated across threads")


def _check_should_compress_gating(plugin, engine):
    # should_compress gates locally below the tracked threshold: no
    # subprocess spawn for the up-to-~90 per-turn host probes.
    probes = []
    real_probe = plugin.TraceDecayContextEngine._preflight_probe
    try:
        def _spy_probe(self, *args, **kwargs):
            probes.append(args)
            return {"status": "ok", "should_compress": True}

        plugin.TraceDecayContextEngine._preflight_probe = _spy_probe
        assert engine.should_compress(prompt_tokens=10) is False
        assert probes == [], probes
        assert engine.should_compress(prompt_tokens=engine.threshold_tokens) is True
        assert len(probes) == 1, probes
    finally:
        plugin.TraceDecayContextEngine._preflight_probe = real_probe
    ok("should_compress short-circuits below threshold, probes at it")

    # ABC contract: should_compress_preflight returns a BOOL; an error dict
    # from the probe must read as False, never truthy.
    real_probe = plugin.TraceDecayContextEngine._preflight_probe
    try:
        plugin.TraceDecayContextEngine._preflight_probe = (
            lambda self, *a, **k: {"error": "boom"}
        )
        assert engine.should_compress_preflight([{"role": "user", "content": "x"}]) is False
        plugin.TraceDecayContextEngine._preflight_probe = (
            lambda self, *a, **k: {"status": "ok", "should_compress": True}
        )
        assert engine.should_compress_preflight([]) is True
    finally:
        plugin.TraceDecayContextEngine._preflight_probe = real_probe
    ok("should_compress_preflight honors the bool ABC contract")


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
    real_tool = plugin.tools.call_tracedecay_tool
    try:
        plugin.tools.call_tracedecay_tool = lambda name, args, **kw: (
            calls.append((name, args, kw)) or "{}"
        )

        provider.handle_tool_call("fact_store", {"action": "search", "query": "rust"})
        name, args, kwargs = calls[-1]
        assert name == "tracedecay_fact_store", calls
        assert "project_root" not in args, args
        assert args["memory_scope"] == "project", args
        assert kwargs["project_root"] == expected_project_root, kwargs
        ok("memory tool calls stay bound to the provider's session project")

        before = len(calls)
        provider.sync_turn("u", "a", session_id="other-session", messages=messages)
        turn_calls = calls[before:]
        assert len(turn_calls) == 2, turn_calls
        user_name, user_args, user_kwargs = turn_calls[0]
        assert user_name == "tracedecay_lcm_preflight"
        assert user_args["storage_scope"] == "user", user_args
        assert "project_root" not in user_kwargs, user_kwargs
        name, args, kwargs = turn_calls[1]
        assert name == "tracedecay_lcm_preflight", calls
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
        assert args["transcript_projection"] is True
        assert "project_root" not in args, args
        assert kwargs["project_root"] == expected_project_root, kwargs

        ok("sync_turn stores a canonical user turn plus its project projection")

        before = len(calls)
        provider.sync_turn("only user", "and assistant", session_id="s2", messages=None)
        turn_calls = calls[before:]
        assert len(turn_calls) == 2, turn_calls
        assert turn_calls[0][1]["storage_scope"] == "user", turn_calls
        assert "project_root" not in turn_calls[0][2], turn_calls
        name, args, kwargs = turn_calls[1]
        assert name == "tracedecay_lcm_preflight", calls
        assert args["messages"][0]["content"] == "only user"
        assert args["messages"][1]["content"] == "and assistant"
        assert "project_root" not in args, args
        assert kwargs["project_root"] == expected_project_root, kwargs
        ok("sync_turn synthesizes a turn when messages are missing")

        provider.on_memory_write("add", "user", "likes rust", {"session_id": "s"})
        name, args, kwargs = calls[-1]
        assert name == "tracedecay_fact_store", calls
        assert args["action"] == "add" and args["category"] == "user_pref"
        assert args["memory_scope"] == "user"
        assert args["metadata"]["hermes_action"] == "add"
        assert "project_root" not in args, args
        assert "project_root" not in kwargs, kwargs
        before = len(calls)
        provider.on_memory_write("remove", "memory", "anything")
        assert len(calls) == before
        ok("on_memory_write mirrors adds and skips removals")

        provider.project_root = None
        before = len(calls)
        provider.sync_turn("u", "a", session_id="untethered", messages=messages)
        assert len(calls) == before + 1
        name, args, kwargs = calls[-1]
        assert name == "tracedecay_lcm_preflight"
        assert args["storage_scope"] == "user", args
        assert args["transcript_projection"] is True
        assert "project_root" not in kwargs, kwargs
        provider.handle_tool_call("fact_store", {"action": "add", "content": "pref"})
        name, args, kwargs = calls[-1]
        assert args["memory_scope"] == "user", args
        assert "project_root" not in kwargs, kwargs
        ok("untethered memory and LCM use profile-level user scope")

        real_resolver = plugin._resolved_project_scope
        plugin._resolved_project_scope = lambda path, *_args: expected_project_root
        before = len(calls)
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
        turn_calls = calls[before:]
        assert len(turn_calls) == 2, turn_calls
        assert turn_calls[0][1]["storage_scope"] == "user", turn_calls
        assert "project_root" not in turn_calls[0][2], turn_calls
        name, args, kwargs = turn_calls[1]
        assert name == "tracedecay_lcm_preflight"
        assert kwargs["project_root"] == expected_project_root
        assert [message["id"] for message in turn_calls[0][1]["messages"]] == [
            message["id"] for message in args["messages"]
        ]
        assert args["transcript_projection"] is True
        plugin._resolved_project_scope = real_resolver
        ok("structured tool activity correlates an untethered turn to its project")
        provider.project_root = expected_project_root

        # Non-primary execution contexts must not write turn state.
        before = len(calls)
        provider.agent_context = "cron"
        provider.sync_turn("u", "a", session_id="cron-session", messages=messages)
        assert len(calls) == before, calls[before:]
        provider.agent_context = ""
        ok("sync_turn skips cron/flush execution contexts")
    finally:
        plugin.tools.call_tracedecay_tool = real_tool


def _check_prefetch_cache(plugin, ctx):
    provider = ctx.provider
    real_json = plugin.call_tracedecay_json
    try:
        # Real search responses nest each row under "fact" (with scores
        # beside it); flat rows must keep working too.
        plugin.call_tracedecay_json = lambda name, args, **kw: {
            "facts": [
                {"fact": {"fact_id": 7, "content": "zack prefers rust"}, "score": 0.9},
                {"fact_id": 8, "content": "flat row"},
            ],
            "count": 2,
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
            "tracedecay_lcm_preflight",
            {"session_id": "s", "messages": [{"role": "user", "content": big}]},
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
    _check_response_handle_deref(plugin)
    messages = _check_engine_state_and_compress(plugin, ctx, host_home, runtime_project)
    _check_engine_thread_isolation(ctx.engine)
    _check_should_compress_gating(plugin, ctx.engine)
    _check_expand_query_degradation(plugin)
    _check_no_hermes_storage_routing(plugin)
    _check_provider_verbs(plugin, ctx, host_home, messages)
    _check_prefetch_cache(plugin, ctx)
    _check_system_prompt_block(ctx)
    _check_args_file_spill(plugin, work)


if __name__ == "__main__":
    main()
