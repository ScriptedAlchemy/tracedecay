#!/usr/bin/env python3
"""Standalone contract checks for the GENERATED Hermes plugin.

Unlike scripts/hermes_stock_check.py (which needs a full stock Hermes
checkout), this harness imports the generated plugin package with a stubbed
plugin context and a fake tokensave binary, so it runs anywhere Python 3
exists. It asserts the host contracts the 2026-06 review found broken:

  1. compress() returns a MESSAGE LIST on success / no-op / error
     (hermes ContextEngine ABC), never the raw result dict.
  2. Payloads >128 KiB round-trip through `--args @file` and the spill
     tempfile is cleaned up.
  3. Code-graph tools register WITHOUT the message-forwarding capability
     flag; the messages-dependent LCM verbs stay gated.
  4. The memory provider implements sync_turn / prefetch / on_memory_write
     and calls the right subprocess verbs.
  5. pre_llm_call returns None on non-first turns (prompt-cache safety).

Usage:
    python3 scripts/hermes_plugin_unit_check.py [plugin_dir]

plugin_dir defaults to a fresh install generated into a temp HOME via the
tokensave binary named by $TOKENSAVE_BIN (default: target/debug/tokensave).
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
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
        os.environ.get("TOKENSAVE_BIN", repo_root / "target" / "debug" / "tokensave")
    )
    assert bin_path.is_file(), f"tokensave binary not found at {bin_path}"
    home = work / "home"
    home.mkdir()
    env = dict(os.environ)
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
    env["PATH"] = f"{bin_path.parent}{os.pathsep}{env.get('PATH', '')}"
    env.pop("HERMES_HOME", None)
    subprocess.run(
        [str(bin_path), "install", "--agent", "hermes", "--no-dashboard"],
        check=True,
        env=env,
        capture_output=True,
        text=True,
        timeout=120,
    )
    plugin_dir = home / ".hermes" / "plugins" / "tokensave"
    assert (plugin_dir / "__init__.py").is_file(), plugin_dir
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

    def register_hook(self, name, fn):
        self.hooks[name] = fn

    def register_tool(self, name=None, toolset=None, schema=None, handler=None, **kwargs):
        self.tools[name] = {"toolset": toolset, "schema": schema, "handler": handler}

    def register_memory_provider(self, provider):
        self.provider = provider

    def register_context_engine(self, engine):
        self.engine = engine

    def register_skill(self, name, path):
        pass

    def register_config_defaults(self, defaults):
        pass

    def register_command(self, name, fn, description=""):
        pass


def write_fake_bin(work: Path) -> Path:
    """A fake tokensave binary that echoes how `--args` reached it."""
    fake = work / "fake-tokensave.py"
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


def run_checks(work: Path):
    if len(sys.argv) > 1:
        plugin_dir = Path(sys.argv[1]).resolve()
    else:
        plugin_dir = generate_plugin(work)
    ok("generated plugin present", str(plugin_dir))

    hermes_home = plugin_dir.parent.parent
    os.environ["HERMES_HOME"] = str(hermes_home)
    sys.path.insert(0, str(plugin_dir.parent))
    plugin = __import__("tokensave")
    assert Path(plugin.__file__).resolve() == (plugin_dir / "__init__.py").resolve()
    ok("plugin package imports standalone (no hermes on sys.path)")

    # ── 3. Registration split ────────────────────────────────────────────
    ctx = StubCtx()
    plugin.register(ctx)
    assert "tokensave_search" in ctx.tools, sorted(ctx.tools)
    assert "tokensave_context" in ctx.tools, sorted(ctx.tools)
    assert "tokensave_fact_store" in ctx.tools, sorted(ctx.tools)
    assert "tokensave_message_search" in ctx.tools, sorted(ctx.tools)
    assert "tokensave_lcm_compress" not in ctx.tools, sorted(ctx.tools)
    assert "tokensave_lcm_preflight" not in ctx.tools, sorted(ctx.tools)
    # Context-engine native mirrors stay gated without the capability flag.
    assert "lcm_grep" not in ctx.tools, sorted(ctx.tools)
    ok("code-graph tools register without the capability flag", f"{len(ctx.tools)} tools")

    class ForwardingCtx(StubCtx):
        context_engine_tool_handlers_receive_messages = True

    fwd = ForwardingCtx()
    plugin.register(fwd)
    assert "tokensave_lcm_compress" in fwd.tools, sorted(fwd.tools)
    assert "lcm_grep" in fwd.tools, sorted(fwd.tools)
    ok("LCM live-ingest verbs register when the host forwards messages")

    # ── 5. pre_llm_call cache safety ─────────────────────────────────────
    hook = ctx.hooks["pre_llm_call"]
    assert hook(is_first_turn=False) is None
    assert hook() is None
    first = hook(is_first_turn=True)
    assert isinstance(first, str) and "tokensave" in first
    ok("pre_llm_call injects on first turn only")
    saved_names = set(plugin._REGISTERED_TOOL_NAMES)
    plugin._REGISTERED_TOOL_NAMES.clear()
    assert hook(is_first_turn=True) is None
    plugin._REGISTERED_TOOL_NAMES.update(saved_names)
    ok("pre_llm_call stays silent when no tools registered")

    # ── 1. compress() message-list contract ──────────────────────────────
    engine = ctx.engine
    assert engine is not None
    engine.initialize(session_id="check-session", hermes_home=str(hermes_home))
    messages = [
        {"role": "user", "content": "hello"},
        {"role": "assistant", "content": "hi"},
    ]
    replay = [{"role": "user", "content": "[summary] hello/hi"}]

    real_json = plugin.call_tokensave_json
    try:
        plugin.call_tokensave_json = lambda name, args, **kw: {
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

        plugin.call_tokensave_json = lambda name, args, **kw: {
            "status": "ok",
            "reason": "below_threshold",
            "replay_messages": list(messages),
        }
        out = engine.compress(list(messages), current_tokens=10)
        assert out == messages, out
        ok("compress() no-op returns the input list (host skips rotation)")

        plugin.call_tokensave_json = lambda name, args, **kw: {"error": "boom"}
        out = engine.compress(list(messages), current_tokens=10)
        assert out == messages, out
        assert engine._last_compress_aborted is True
        assert "boom" in str(engine._last_summary_error)
        ok("compress() error returns the input list and flags the abort")

        def _raise(name, args, **kw):
            raise RuntimeError("subprocess exploded")

        plugin.call_tokensave_json = _raise
        out = engine.compress(list(messages), current_tokens=10)
        assert out == messages and engine._last_compress_aborted is True
        ok("compress() exception degrades to the input list")
    finally:
        plugin.call_tokensave_json = real_json

    # Host-contract token attrs (minimum-context guard reads these).
    engine.update_model("check-model", 128000)
    assert engine.context_length == 128000
    assert 0 < engine.threshold_tokens <= 128000
    ok("update_model populates context_length/threshold_tokens")

    # Storage scope: the pin never forces project_local anymore.
    storage = plugin._storage_args("/some/pin", str(hermes_home))
    assert storage["storage_scope"] == "hermes_profile", storage
    assert storage["hermes_home"] == str(hermes_home), storage
    ok("LCM/memory storage stays hermes_profile-scoped")

    # ── 4. provider hooks call the right verbs ───────────────────────────
    provider = ctx.provider
    assert provider is not None
    provider.initialize("check-session", hermes_home=str(hermes_home))
    schema_names = [schema["name"] for schema in provider.get_tool_schemas()]
    assert schema_names == ["fact_store", "fact_feedback", "memory_status"], schema_names
    ok("memory schemas collapsed to 3")

    calls = []
    real_tool = plugin.tools.call_tokensave_tool
    try:
        plugin.tools.call_tokensave_tool = lambda name, args, **kw: (
            calls.append((name, args)) or "{}"
        )
        provider.sync_turn("u", "a", session_id="other-session", messages=messages)
        name, args = calls[-1]
        assert name == "tokensave_lcm_preflight", calls
        assert args["session_id"] == "other-session"
        assert args["messages"] == messages
        assert args["storage_scope"] == "hermes_profile"
        ok("sync_turn ingests via tokensave_lcm_preflight")

        provider.sync_turn("only user", "and assistant", session_id="s2", messages=None)
        name, args = calls[-1]
        assert args["messages"][0]["content"] == "only user"
        assert args["messages"][1]["content"] == "and assistant"
        ok("sync_turn synthesizes a turn when messages are missing")

        provider.on_memory_write("add", "user", "likes rust", {"session_id": "s"})
        name, args = calls[-1]
        assert name == "tokensave_fact_store", calls
        assert args["action"] == "add" and args["category"] == "user_pref"
        assert args["metadata"]["hermes_action"] == "add"
        before = len(calls)
        provider.on_memory_write("remove", "memory", "anything")
        assert len(calls) == before
        ok("on_memory_write mirrors adds and skips removals")
    finally:
        plugin.tools.call_tokensave_tool = real_tool

    real_json = plugin.call_tokensave_json
    try:
        plugin.call_tokensave_json = lambda name, args, **kw: {
            "facts": [{"fact_id": 7, "content": "zack prefers rust"}],
            "count": 1,
        }
        text = provider.prefetch("rust preferences")
        assert "zack prefers rust" in text and "[fact 7]" in text, text
        plugin.call_tokensave_json = lambda name, args, **kw: {"error": "nope"}
        assert provider.prefetch("anything") == ""
        assert provider.prefetch("") == ""
        ok("prefetch formats fact recall and degrades to empty string")
    finally:
        plugin.call_tokensave_json = real_json

    block = provider.system_prompt_block()
    assert isinstance(block, str) and "fact_store" in block
    ok("system_prompt_block is static provider guidance")

    # ── 2. --args @file spill round-trip ─────────────────────────────────
    fake_bin = write_fake_bin(work)
    spill_dir = work / "spill"
    spill_dir.mkdir()
    real_bin = plugin.tools.TOKENSAVE_BIN
    real_tmp = tempfile.tempdir
    try:
        plugin.tools.TOKENSAVE_BIN = str(fake_bin)
        tempfile.tempdir = str(spill_dir)

        big = "x" * (200 * 1024)
        raw = plugin.tools.call_tokensave_tool(
            "tokensave_lcm_preflight",
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

        raw = plugin.tools.call_tokensave_tool("tokensave_status", {"small": True})
        inner = json.loads(json.loads(raw)["content"][0]["text"])
        assert inner["at_file"] is False, inner
        ok("small payloads stay inline on argv")
    finally:
        plugin.tools.TOKENSAVE_BIN = real_bin
        tempfile.tempdir = real_tmp


if __name__ == "__main__":
    main()
