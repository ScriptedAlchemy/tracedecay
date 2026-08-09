#!/usr/bin/env python3
"""Verify the generated tracedecay plugin against STOCK (upstream) Hermes.

Run from the upstream hermes-agent repo root with its own interpreter
(`.venv/bin/python` after `uv sync`), after `tracedecay install --agent hermes`
wrote the plugin into a throwaway profile:

    HOME=<throwaway> \
    .venv/bin/python scripts/hermes_stock_check.py

Asserts the surfaces stock Hermes actually exposes:
  1. the general PluginManager loads + enables the plugin (hook, command),
  2. the context engine registers and is selected via `context.engine`,
  3. the memory provider is discovered via `memory.provider` config
     (stock routes providers through plugins/memory, not PluginContext),
  4. real tool dispatch round-trips through the tracedecay binary
     (memory facts, LCM status/preflight/compress, graph status).

Everything runs offline: no model calls (compress stays below threshold).
"""

import copy
import json
import os
import subprocess
import sys
import time
from pathlib import Path

PASS = 0


def ok(label, detail=""):
    global PASS
    PASS += 1
    suffix = f" ({detail})" if detail else ""
    print(f"ok {PASS} - {label}{suffix}")


def assert_tool_dispatch_success(raw):
    """Validate the stock provider's raw MCP envelope without decoding its text."""
    outer = json.loads(raw)
    assert isinstance(outer, dict), outer
    assert "error" not in outer, f"tool dispatch returned an error: {outer}"
    assert outer.get("isError") is not True, f"tool dispatch failed: {outer}"
    content = outer["content"]
    assert content and content[0]["type"] == "text", outer
    return outer


def assert_lcm_status_argv(argv, project_root):
    """Validate the canonical CLI route used by the stock context engine."""
    assert len(argv) >= 6, argv
    assert argv[1] == "tool", argv
    name_index = 2
    if argv[name_index] == "--project":
        assert len(argv) >= 8, argv
        assert os.path.realpath(argv[name_index + 1]) == os.path.realpath(project_root), argv
        name_index += 2
    else:
        raise AssertionError(f"lcm status must carry the project route: {argv!r}")
    assert argv[name_index] == "tracedecay_lcm_status", argv
    assert argv[name_index + 1 : name_index + 3] == ["--json", "--args"], argv
    assert len(argv) == name_index + 4, argv
    return argv[-1]


def assert_decoded_result(payload, *required_fields):
    """Require a host-facing result, rather than an application transport envelope."""
    assert isinstance(payload, dict), payload
    assert "error" not in payload, payload
    assert "contract" not in payload and "outcome" not in payload, payload
    for field in required_fields:
        assert field in payload, (field, payload)
    return payload


def main():
    hermes_home = os.path.join(os.environ["HOME"], ".hermes")
    project_root = os.getcwd()

    # The installer projects approved managed skills beside the bundled
    # plugin skill. Add one fixture before discovery to verify that stock
    # Hermes registers the complete plugin-native skill overlay.
    managed_skill = (
        Path(hermes_home)
        / "plugins"
        / "tracedecay"
        / "skills"
        / "agent-managed"
        / "managed-check"
        / "SKILL.md"
    )
    managed_skill.parent.mkdir(parents=True, exist_ok=True)
    managed_skill.write_text(
        "---\nname: managed-check\ndescription: Managed skill registration check.\n---\n\n"
        "# Managed check\n\nHermes can load this exported managed skill.\n",
        encoding="utf-8",
    )

    # 1. Stock general plugin manager: discovery, enablement, registrations.
    from hermes_cli.plugins import get_plugin_manager, get_plugin_context_engine

    manager = get_plugin_manager()
    manager.discover_and_load()
    loaded = manager._plugins.get("tracedecay")
    assert loaded is not None, f"tracedecay missing from {sorted(manager._plugins)}"
    assert loaded.enabled, f"tracedecay plugin not enabled: {loaded.error}"
    assert loaded.error is None, f"tracedecay plugin load error: {loaded.error}"
    plugin = loaded.module
    ok("plugin loads via stock PluginManager")
    assert "pre_llm_call" in loaded.hooks_registered, loaded.hooks_registered
    ok("pre_llm_call hook registered")
    assert "tracedecay_status" in loaded.commands_registered, loaded.commands_registered
    ok("/tracedecay_status command registered")
    assert manager.list_plugin_skills("tracedecay") == [
        "managed-check",
        "tracedecay",
    ], manager.list_plugin_skills("tracedecay")
    from tools.skills_tool import skill_view

    managed_view = json.loads(skill_view("tracedecay:managed-check"))
    assert managed_view.get("success") is True, managed_view
    assert "Hermes can load this exported managed skill" in managed_view.get("content", "")
    ok("plugin-native managed skills resolve through qualified skill_view")
    # Code-graph / memory / transcript tools register unconditionally; only
    # the live-ingest LCM verbs (whose schemas take the in-memory messages
    # list) depend on the context_engine_tool_handlers_receive_messages
    # capability, which stock does not advertise.
    registered = set(loaded.tools_registered)
    assert "tracedecay_search" in registered, sorted(registered)
    assert "tracedecay_context" in registered, sorted(registered)
    assert "tracedecay_message_search" in registered, sorted(registered)
    assert "tracedecay_lcm_compress" not in registered, sorted(registered)
    assert "tracedecay_lcm_preflight" not in registered, sorted(registered)
    # memory.provider is tracedecay here, so the provider-owned fact trio
    # must not register as direct duplicates.
    assert "tracedecay_fact_store" not in registered, sorted(registered)
    assert "tracedecay_fact_feedback" not in registered, sorted(registered)
    assert "tracedecay_memory_status" not in registered, sorted(registered)
    ok(
        "code-graph tools register on stock; LCM + provider-owned tools stay gated",
        f"{len(registered)} tools",
    )
    from model_tools import get_tool_definitions
    from tools.tool_search import (
        ToolSearchConfig,
        assemble_tool_defs,
        dispatch_tool_describe,
        dispatch_tool_search,
        resolve_underlying_call,
    )

    raw_tool_defs = get_tool_definitions(
        enabled_toolsets=["tracedecay"],
        quiet_mode=True,
        skip_tool_search_assembly=True,
    )
    raw_names = {(item.get("function") or {}).get("name") for item in raw_tool_defs}
    assert "tracedecay_search" in raw_names, sorted(raw_names)
    forced_search = ToolSearchConfig(
        enabled="on",
        threshold_pct=10.0,
        search_default_limit=5,
        max_search_limit=20,
    )
    assembled = assemble_tool_defs(raw_tool_defs, config=forced_search)
    visible_names = {
        (item.get("function") or {}).get("name") for item in assembled.tool_defs
    }
    assert assembled.activated is True, assembled
    assert "tracedecay_search" not in visible_names, sorted(visible_names)
    assert {"tool_search", "tool_describe", "tool_call"}.issubset(visible_names)
    search_result = json.loads(
        dispatch_tool_search(
            {"query": "semantic code search"},
            current_tool_defs=raw_tool_defs,
            config=forced_search,
        )
    )
    match_names = {item["name"] for item in search_result.get("matches", [])}
    assert "tracedecay_search" in match_names, search_result
    described = json.loads(
        dispatch_tool_describe(
            {"name": "tracedecay_search"}, current_tool_defs=raw_tool_defs
        )
    )
    assert described.get("name") == "tracedecay_search", described
    underlying, arguments, error = resolve_underlying_call(
        {
            "name": "tracedecay_search",
            "arguments": {"query": "register skill"},
        }
    )
    assert (underlying, arguments, error) == (
        "tracedecay_search",
        {"query": "register skill"},
        None,
    )
    ok("TraceDecay tools participate in stock progressive tool discovery")

    # 2. Context engine: registered through the plugin and selected the way
    #    stock agent/agent_init.py selects it (config-driven, plugin fallback).
    from hermes_cli.config import load_config

    config = load_config()
    engine_name = (config.get("context") or {}).get("engine")
    assert engine_name == "tracedecay", f"context.engine = {engine_name!r}"
    ok("config.yaml selects context.engine: tracedecay")

    from plugins.context_engine import load_context_engine

    assert load_context_engine(engine_name) is None
    engine = get_plugin_context_engine()
    assert engine is not None and engine.name == engine_name
    from agent.context_engine import ContextEngine

    assert isinstance(engine, ContextEngine)
    ok("context engine activates via stock plugin fallback")

    engine.initialize(session_id="stock-check-session", project_root=project_root)
    assert engine.project_root is not None
    assert os.path.realpath(engine.project_root) == os.path.realpath(project_root)
    engine.update_model("stock-check-model", 128000)
    engine.update_from_response({"prompt_tokens": 120, "completion_tokens": 30})
    assert engine.last_total_tokens == 150
    ok("stock ContextEngine ABC surface works", "update_from_response")

    # Stock Hermes deep-copies the registered plugin singleton for every
    # AIAgent. The copy must retain routing/budget state without sharing locks
    # or live agent references, otherwise Hermes falls back to its compressor.
    engine.agent = object()
    cloned_engine = copy.deepcopy(engine)
    assert cloned_engine is not engine
    assert cloned_engine._state_lock is not engine._state_lock
    assert cloned_engine.project_root == engine.project_root
    assert cloned_engine.context_length == 128000
    assert cloned_engine.last_total_tokens == 150
    assert cloned_engine.agent is None
    ok("context engine deep-copies through the stock Hermes agent contract")

    assert engine.should_compress(1000) is False
    ok("should_compress gates locally below the tracked threshold")
    assert engine.should_compress_preflight([], current_tokens=1000) is False
    ok("should_compress_preflight honors the bool ABC contract")

    lcm_status_argv = []
    real_subprocess_run = plugin.tools.subprocess.run
    try:
        def capture_lcm_status_argv(argv, *args, **kwargs):
            lcm_status_argv.append(argv)
            return real_subprocess_run(argv, *args, **kwargs)

        plugin.tools.subprocess.run = capture_lcm_status_argv
        status = engine.status()
    finally:
        plugin.tools.subprocess.run = real_subprocess_run
    assert lcm_status_argv, "context engine did not invoke tracedecay_lcm_status"
    status_payload = json.loads(assert_lcm_status_argv(lcm_status_argv[-1], project_root))
    assert status_payload == {
        "provider": "hermes",
        "session_id": "stock-check-session",
        "format": "json",
    }, status_payload
    assert_decoded_result(status, "status")
    if status.get("status") == "not_ingested":
        assert status.get("store_exists") is False, status
        ok("lcm_status dispatch round-trips", "not_ingested before compress")
    else:
        assert status.get("session_id") == "stock-check-session", status
        ok("lcm_status dispatch round-trips")

    messages = [
        {"role": "user", "content": "hello"},
        {"role": "assistant", "content": "hi there"},
    ]
    compressed = engine.compress(list(messages), current_tokens=50)
    # Host ABC contract: compress() returns a MESSAGE LIST the host adopts as
    # the live transcript. Hermes exposes no authentic raw-compression
    # protocol, so the daemon-owned LCM authority reports host raw compaction
    # as a typed unavailable capability and the transcript passes through
    # unchanged (the transcript itself still ingests through the daemon).
    assert isinstance(compressed, list), type(compressed)
    assert compressed == messages, compressed
    result = engine.last_compress_result
    assert isinstance(result, dict) and result == {
        "status": "unavailable",
        "reason": "host_raw_compression_unavailable",
        "semantic_error": True,
    }, result
    ok(
        "compress returns the unchanged transcript with typed unavailability",
        f"reason={result.get('reason')}",
    )

    # 3. Memory provider: stock discovers providers via plugins/memory and the
    #    memory.provider config key (the general PluginContext has no
    #    register_memory_provider, so this is the only stock activation path).
    from plugins.memory import _get_active_memory_provider, load_memory_provider

    assert _get_active_memory_provider() == "tracedecay"
    ok("config.yaml selects memory.provider: tracedecay")

    provider = load_memory_provider("tracedecay")
    assert provider is not None, "stock plugins/memory failed to load tracedecay"
    from agent.memory_provider import MemoryProvider

    assert isinstance(provider, MemoryProvider)
    assert provider.name == "tracedecay"
    assert provider.is_available() is True
    ok("memory provider discovered and available on stock")

    provider.initialize("stock-check-session", project_root=project_root)
    schema_names = [schema["name"] for schema in provider.get_tool_schemas()]
    assert schema_names == ["fact_store", "fact_feedback", "memory_status"], schema_names
    ok("memory tool schemas collapsed to fact_store/fact_feedback/memory_status")

    # The shipped fixed-action compatibility wire still dispatches through the
    # canonical fact-store contract without expanding the public schema.
    assert_tool_dispatch_success(
        provider.handle_tool_call(
            "fact_add",
            {
                "content": "stock hermes integration verified",
                "fact_type": "decision",
                "format": "json",
            },
        )
    )
    found = plugin.call_tracedecay_json(
        "tracedecay_fact_store",
        {
            "action": "search",
            "query": "stock hermes integration",
            "limit": 1,
            "format": "json",
        },
        project_root=project_root,
    )
    assert_decoded_result(found, "count", "facts", "results")
    assert found.get("count", 0) >= 1, found
    ok("memory fact add/search round-trips through the binary")

    # A second Hermes session rooted in another registered project must get a
    # distinct provider instance and fact shard. This is the gateway/Desktop
    # routing invariant that prevents one project's memories leaking into
    # another project.
    other_project = os.path.join(os.path.dirname(project_root), "project-two")
    os.makedirs(other_project, exist_ok=True)
    with open(os.path.join(other_project, "README.md"), "w", encoding="utf-8") as handle:
        handle.write("# project two\n")
    init_result = subprocess.run(
        [loaded.module.tools.TRACEDECAY_BIN, "init"],
        cwd=other_project,
        check=False,
        capture_output=True,
        text=True,
    )
    assert init_result.returncode == 0 or "already initialized" in (
        init_result.stdout + init_result.stderr
    ).lower(), init_result.stderr
    other_provider = load_memory_provider("tracedecay")
    assert other_provider is not None and other_provider is not provider
    other_provider.initialize("stock-check-session-two", project_root=other_project)
    assert provider.project_root != other_provider.project_root, (
        provider.project_root,
        other_provider.project_root,
    )
    isolation_marker = "stock hermes project two isolated"
    assert_tool_dispatch_success(
        other_provider.handle_tool_call(
            "fact_add",
            {"content": isolation_marker, "fact_type": "decision", "format": "json"},
        )
    )
    first_project_result = plugin.call_tracedecay_json(
        "tracedecay_fact_store",
        {"action": "list", "limit": 200, "format": "json"},
        project_root=project_root,
    )
    second_project_result = plugin.call_tracedecay_json(
        "tracedecay_fact_store",
        {"action": "list", "limit": 200, "format": "json"},
        project_root=other_project,
    )
    assert_decoded_result(first_project_result, "count", "facts", "results")
    assert_decoded_result(second_project_result, "count", "facts", "results")
    first_contents = {
        item.get("fact", item).get("content") for item in first_project_result.get("facts", [])
    }
    second_contents = {
        item.get("fact", item).get("content") for item in second_project_result.get("facts", [])
    }
    assert isolation_marker not in first_contents, first_project_result
    assert isolation_marker in second_contents, second_project_result
    ok("memory facts remain isolated between Hermes session projects")

    # Passive-ingest / recall hooks (sync_turn, queue_prefetch, on_memory_write).
    # prefetch() is the fast inline half: recall happens in queue_prefetch's
    # background thread and is consumed on the next turn.
    assert provider.prefetch("stock hermes integration") == ""
    provider.queue_prefetch("stock hermes integration")
    deadline = time.time() + 15
    prefetched = ""
    while time.time() < deadline and not prefetched:
        prefetched = provider.prefetch("stock hermes integration")
        time.sleep(0.1)
    assert "stock hermes integration" in prefetched, prefetched
    ok("queue_prefetch recalls stored facts for the next prefetch")
    provider.sync_turn(
        "hello", "hi there", session_id="stock-check-session", messages=messages
    )
    grep = plugin.call_tracedecay_json(
        "tracedecay_lcm_grep",
        {
            "provider": "hermes",
            "session_id": "stock-check-session",
            "query": "hello",
            "scope": "all",
        },
        project_root=project_root,
    )
    assert_decoded_result(grep, "status", "hits", "provider", "query")
    assert grep["provider"] == "hermes", grep
    assert grep["query"] == "hello", grep
    assert isinstance(grep["hits"], list), grep
    ok("sync_turn ingests the turn into the LCM raw store")
    provider.on_memory_write(
        "add", "memory", "stock on-memory-write mirror fact", {"session_id": "s"}
    )
    mirrored = plugin.call_tracedecay_json(
        "tracedecay_fact_store",
        {
            "action": "search",
            "query": "on-memory-write mirror",
            "limit": 1,
            "format": "json",
        },
        project_root=project_root,
    )
    assert_decoded_result(mirrored, "count", "facts", "results")
    assert mirrored.get("count", 0) >= 1, mirrored
    ok("on_memory_write mirrors built-in memory writes")

    # 4. Graph tool dispatch through generated tools.py against the real cwd,
    #    never the Hermes plugin/config directory.
    graph_status = plugin.call_tracedecay_json("tracedecay_status", {})
    assert_decoded_result(graph_status, "file_count", "node_count")
    assert graph_status.get("file_count", 0) >= 1, graph_status
    assert graph_status.get("node_count", 0) >= 1, graph_status
    ok(
        "graph tool dispatch round-trips against the working project",
        f"files={graph_status.get('file_count')} nodes={graph_status.get('node_count')}",
    )
    command_status = plugin._tracedecay_status(hermes_home=hermes_home)
    assert isinstance(command_status, str) and command_status.startswith(
        "tracedecay status:\n"
    ), command_status
    assert "files:" in command_status and "nodes:" in command_status, command_status
    ok("registered /tracedecay_status command renders the mounted graph result")
    assert project_root != hermes_home
    ok("Hermes home does not select the TraceDecay project", project_root)

    print(f"1..{PASS}")
    print(f"stock hermes integration: all {PASS} checks passed")


if __name__ == "__main__":
    main()
