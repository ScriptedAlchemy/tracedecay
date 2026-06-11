#!/usr/bin/env python3
"""Verify the generated tokensave plugin against STOCK (upstream) Hermes.

Run from the upstream hermes-agent repo root with its own interpreter
(`.venv/bin/python` after `uv sync`), after `tokensave install --agent hermes`
wrote the plugin into a throwaway profile:

    HERMES_HOME=<throwaway>/.hermes \
    TOKENSAVE_PROJECT_ROOT=<throwaway-project> \
    .venv/bin/python scripts/hermes_stock_check.py

Asserts the surfaces stock Hermes actually exposes:
  1. the general PluginManager loads + enables the plugin (hook, command),
  2. the context engine registers and is selected via `context.engine`,
  3. the memory provider is discovered via `memory.provider` config
     (stock routes providers through plugins/memory, not PluginContext),
  4. real tool dispatch round-trips through the tokensave binary
     (memory facts, LCM status/preflight/compress, graph status).

Everything runs offline: no model calls (compress stays below threshold).
"""

import json
import os
import sys

PASS = 0


def ok(label, detail=""):
    global PASS
    PASS += 1
    suffix = f" ({detail})" if detail else ""
    print(f"ok {PASS} - {label}{suffix}")


def unwrap_tool_json(raw):
    """Decode a generated-tools.py response: MCP envelope with JSON text."""
    outer = json.loads(raw)
    assert "error" not in outer, f"tool dispatch returned an error: {outer}"
    content = outer["content"]
    assert content and content[0]["type"] == "text", outer
    inner = json.loads(content[0]["text"])
    assert "error" not in inner, f"tool payload carries an error: {inner}"
    return inner


def main():
    hermes_home = os.environ["HERMES_HOME"]
    project_root = os.environ["TOKENSAVE_PROJECT_ROOT"]
    sys.path.insert(0, os.getcwd())

    # 1. Stock general plugin manager: discovery, enablement, registrations.
    from hermes_cli.plugins import get_plugin_manager, get_plugin_context_engine

    manager = get_plugin_manager()
    manager.discover_and_load()
    loaded = manager._plugins.get("tokensave")
    assert loaded is not None, f"tokensave missing from {sorted(manager._plugins)}"
    assert loaded.enabled, f"tokensave plugin not enabled: {loaded.error}"
    assert loaded.error is None, f"tokensave plugin load error: {loaded.error}"
    ok("plugin loads via stock PluginManager")
    assert "pre_llm_call" in loaded.hooks_registered, loaded.hooks_registered
    ok("pre_llm_call hook registered")
    assert "tokensave_status" in loaded.commands_registered, loaded.commands_registered
    ok("/tokensave_status command registered")
    # Stock has no context_engine_tool_handlers_receive_messages capability;
    # direct tool registration must degrade to a silent skip, not an error.
    assert loaded.tools_registered == [], loaded.tools_registered
    ok("direct tool registration degrades gracefully on stock")

    # 2. Context engine: registered through the plugin and selected the way
    #    stock agent/agent_init.py selects it (config-driven, plugin fallback).
    from hermes_cli.config import load_config

    config = load_config()
    engine_name = (config.get("context") or {}).get("engine")
    assert engine_name == "tokensave", f"context.engine = {engine_name!r}"
    ok("config.yaml selects context.engine: tokensave")

    from plugins.context_engine import load_context_engine

    assert load_context_engine(engine_name) is None
    engine = get_plugin_context_engine()
    assert engine is not None and engine.name == engine_name
    from agent.context_engine import ContextEngine

    assert isinstance(engine, ContextEngine)
    ok("context engine activates via stock plugin fallback")

    engine.initialize(session_id="stock-check-session", hermes_home=hermes_home)
    engine.update_model("stock-check-model", 128000)
    engine.update_from_response({"prompt_tokens": 120, "completion_tokens": 30})
    assert engine.last_total_tokens == 150
    ok("stock ContextEngine ABC surface works", "update_from_response")

    assert engine.should_compress(1000) is False
    ok("should_compress round-trips through tokensave_lcm_preflight")

    status = unwrap_tool_json(engine.handle_tool_call("lcm_status", {}))
    assert status.get("session_id") == "stock-check-session", status
    ok("lcm_status dispatch round-trips")

    messages = [
        {"role": "user", "content": "hello"},
        {"role": "assistant", "content": "hi there"},
    ]
    compressed = engine.compress(messages, current_tokens=50)
    assert isinstance(compressed, dict) and compressed.get("status") == "ok", compressed
    ok("compress round-trips offline", f"status={compressed.get('status')}")

    # 3. Memory provider: stock discovers providers via plugins/memory and the
    #    memory.provider config key (the general PluginContext has no
    #    register_memory_provider, so this is the only stock activation path).
    from plugins.memory import _get_active_memory_provider, load_memory_provider

    assert _get_active_memory_provider() == "tokensave"
    ok("config.yaml selects memory.provider: tokensave")

    provider = load_memory_provider("tokensave")
    assert provider is not None, "stock plugins/memory failed to load tokensave"
    from agent.memory_provider import MemoryProvider

    assert isinstance(provider, MemoryProvider)
    assert provider.name == "tokensave"
    assert provider.is_available() is True
    ok("memory provider discovered and available on stock")

    provider.initialize("stock-check-session", hermes_home=hermes_home)
    schema_names = [schema["name"] for schema in provider.get_tool_schemas()]
    assert "fact_add" in schema_names and "memory_status" in schema_names
    ok("memory tool schemas exposed", f"{len(schema_names)} tools")

    added = unwrap_tool_json(
        provider.handle_tool_call(
            "fact_add",
            {"content": "stock hermes integration verified", "fact_type": "decision"},
        )
    )
    fact = added.get("fact") or {}
    assert fact.get("content") == "stock hermes integration verified", added
    found = unwrap_tool_json(
        provider.handle_tool_call("fact_search", {"query": "stock hermes integration"})
    )
    assert found.get("count", 0) >= 1, found
    ok("memory fact add/search round-trips through the binary")

    # 4. Graph tool dispatch through the generated tools.py against the
    #    pinned throwaway project.
    plugin = loaded.module
    graph_status = unwrap_tool_json(plugin.tools.call_tokensave_tool("tokensave_status", {}))
    assert graph_status.get("file_count", 0) >= 1, graph_status
    assert graph_status.get("node_count", 0) >= 1, graph_status
    ok(
        "graph tool dispatch round-trips against the pinned project",
        f"files={graph_status.get('file_count')} nodes={graph_status.get('node_count')}",
    )
    assert plugin.tools.config_pinned_project_root() == project_root
    ok("plugins.tokensave.project_root pin resolves", project_root)

    print(f"1..{PASS}")
    print(f"stock hermes integration: all {PASS} checks passed")


if __name__ == "__main__":
    main()
