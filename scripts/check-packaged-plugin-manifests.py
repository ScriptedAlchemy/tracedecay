#!/usr/bin/env python3
import json
import sys
from pathlib import Path
from typing import Any


def fail(message: str) -> None:
    raise SystemExit(f"distribution acceptance: {message}")


def load(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"invalid packaged JSON {path}: {error}")
    if not isinstance(value, dict):
        fail(f"packaged plugin manifest must be an object: {path}")
    return value


def require_string(manifest: dict[str, Any], field: str, path: Path) -> str:
    value = manifest.get(field)
    if not isinstance(value, str) or not value:
        fail(f"{path} requires non-empty string field {field!r}")
    return value


def require_equal(
    manifest: dict[str, Any], field: str, expected: Any, path: Path
) -> None:
    if manifest.get(field) != expected:
        fail(f"{path} requires {field!r} to equal {expected!r}")


def validate_common(manifest: dict[str, Any], path: Path) -> None:
    require_equal(manifest, "name", "tracedecay", path)
    require_string(manifest, "version", path)
    require_string(manifest, "description", path)


def validate_claude(path: Path) -> None:
    manifest = load(path)
    validate_common(manifest, path)
    require_equal(manifest, "lspServers", "./.lsp.json", path)


def validate_codex(path: Path) -> None:
    manifest = load(path)
    validate_common(manifest, path)
    require_equal(manifest, "mcpServers", "./.mcp.json", path)
    require_equal(manifest, "skills", "./skills/", path)
    require_equal(manifest, "hooks", "./hooks/hooks.json", path)
    interface = manifest.get("interface")
    if not isinstance(interface, dict):
        fail(f"{path} requires object field 'interface'")
    capabilities = interface.get("capabilities")
    required = {"mcp", "skills", "code-search", "project-memory"}
    if not isinstance(capabilities, list) or not required.issubset(capabilities):
        fail(f"{path} interface capabilities omit {sorted(required)}")


def validate_cursor(path: Path) -> None:
    manifest = load(path)
    validate_common(manifest, path)
    require_equal(manifest, "displayName", "TraceDecay", path)
    require_equal(manifest, "mcpServers", "mcp.json", path)
    require_equal(manifest, "hooks", "hooks/hooks.json", path)
    require_equal(manifest, "commands", "commands/", path)
    require_equal(manifest, "skills", "skills/", path)
    require_equal(manifest, "agents", "agents/", path)
    rules = manifest.get("rules")
    if not isinstance(rules, list) or "rules/tracedecay.mdc" not in rules:
        fail(f"{path} must reference rules/tracedecay.mdc")


def validate_kimi(path: Path) -> None:
    manifest = load(path)
    validate_common(manifest, path)
    require_equal(manifest, "skills", ["./skills/"], path)
    require_equal(manifest, "commands", "./commands/", path)
    hooks = manifest.get("hooks")
    if not isinstance(hooks, list):
        fail(f"{path} requires array field 'hooks'")
    by_event = {
        hook.get("event"): hook for hook in hooks if isinstance(hook, dict)
    }
    for event in ("PostToolUse", "Stop"):
        hook = by_event.get(event)
        if (
            not isinstance(hook, dict)
            or not isinstance(hook.get("command"), str)
            or not hook["command"]
            or not isinstance(hook.get("timeout"), int)
        ):
            fail(f"{path} requires a callable {event} hook with an integer timeout")
    server = manifest.get("mcpServers", {}).get("tracedecay")
    if not isinstance(server, dict):
        fail(f"{path} requires mcpServers.tracedecay")
    require_equal(server, "command", "tracedecay", path)
    require_equal(server, "args", ["serve"], path)


def main() -> None:
    if len(sys.argv) != 2:
        fail("usage: check-packaged-plugin-manifests.py <packaged-plugin-root>")
    root = Path(sys.argv[1])
    validate_claude(root / ".claude-plugin/plugin.json")
    validate_codex(root / ".codex-plugin/plugin.json")
    validate_cursor(root / ".cursor-plugin/plugin.json")
    validate_kimi(root / ".kimi-plugin/plugin.json")


if __name__ == "__main__":
    main()
