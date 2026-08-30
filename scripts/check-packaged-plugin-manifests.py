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


def require_object(value: Any, label: str, path: Path) -> dict[str, Any]:
    if not isinstance(value, dict):
        fail(f"{path} requires object field {label}")
    return value


def require_string_list(value: Any, label: str, path: Path) -> list[str]:
    if (
        not isinstance(value, list)
        or not value
        or not all(isinstance(item, str) and item for item in value)
    ):
        fail(f"{path} requires {label} as a non-empty list of strings")
    return value


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


def shared_skill_slugs(root: Path) -> list[str]:
    skills = root / "skills"
    if not skills.is_dir():
        fail(f"packaged plugin is missing skills directory: {skills}")
    slugs = sorted(
        entry.name
        for entry in skills.iterdir()
        if entry.is_dir() and (entry / "SKILL.md").is_file()
    )
    if not slugs:
        fail(f"no SKILL.md skill slugs under {skills}")
    return slugs


def resolve_skills_dir(manifest: dict[str, Any], root: Path, path: Path) -> Path:
    value = manifest.get("skills")
    if isinstance(value, str) and value:
        return (root / value).resolve()
    if isinstance(value, list):
        directories = [item for item in value if isinstance(item, str) and item]
        if len(directories) != 1:
            fail(f"{path} skills must name a single directory")
        return (root / directories[0]).resolve()
    return (root / "skills").resolve()


def host_plugin_dirs(root: Path) -> list[Path]:
    return sorted(
        entry
        for entry in root.iterdir()
        if entry.is_dir()
        and entry.name.startswith(".")
        and entry.name.endswith("-plugin")
    )


def validate_host_skill_inventories(root: Path) -> None:
    slugs = shared_skill_slugs(root)
    inventories: list[tuple[str, Path]] = []
    for plugin_dir in host_plugin_dirs(root):
        manifest_path = plugin_dir / "plugin.json"
        if not manifest_path.is_file():
            fail(f"host plugin directory is missing plugin.json: {manifest_path}")
        host = plugin_dir.name.removeprefix(".").removesuffix("-plugin")
        inventories.append(
            (host, resolve_skills_dir(load(manifest_path), root, manifest_path))
        )
    opencode_dir = root / "opencode"
    if not opencode_dir.is_dir():
        fail(f"packaged plugin is missing OpenCode directory: {opencode_dir}")
    inventories.append(("opencode", (root / "skills").resolve()))
    if not inventories:
        fail("packaged plugin has no host deploy inventories")
    for host, skills_dir in inventories:
        if not skills_dir.is_dir():
            fail(f"{host} deploy inventory is missing skills directory: {skills_dir}")
        for slug in slugs:
            skill = skills_dir / slug / "SKILL.md"
            if not skill.is_file():
                fail(
                    f"{host} deploy inventory is missing shared skill {slug!r}: {skill}"
                )


def validate_opencode(root: Path) -> None:
    plugin = root / "opencode" / "tracedecay.ts"
    mcp_plugin = root / "opencode" / "tracedecay-mcp.ts"
    registration_path = root / "opencode" / "opencode.registration.json"
    for path in (plugin, mcp_plugin, registration_path):
        if not path.is_file():
            fail(f"missing packaged OpenCode asset: {path}")
        if path.stat().st_size == 0:
            fail(f"packaged OpenCode asset is empty: {path}")

    registration = load(registration_path)
    mcp_server = require_object(
        require_object(registration.get("mcp"), "mcp", registration_path).get(
            "tracedecay"
        ),
        "mcp.tracedecay",
        registration_path,
    )
    require_equal(mcp_server, "type", "local", registration_path)
    mcp_command = require_string_list(
        mcp_server.get("command"), "mcp.tracedecay.command", registration_path
    )
    if "serve" not in mcp_command:
        fail(f"{registration_path} mcp.tracedecay.command must include 'serve'")

    lsp = require_object(
        require_object(registration.get("lsp"), "lsp", registration_path).get(
            "tracedecay"
        ),
        "lsp.tracedecay",
        registration_path,
    )
    lsp_command = require_string_list(
        lsp.get("command"), "lsp.tracedecay.command", registration_path
    )
    for expected in ("lsp", "bridge", "--stdio"):
        if expected not in lsp_command:
            fail(
                f"{registration_path} lsp.tracedecay.command must include {expected!r}"
            )
    require_string_list(
        lsp.get("extensions"), "lsp.tracedecay.extensions", registration_path
    )
    require_object(lsp.get("env"), "lsp.tracedecay.env", registration_path)
    initialization = require_object(
        lsp.get("initialization"), "lsp.tracedecay.initialization", registration_path
    )
    require_object(
        initialization.get("tracedecay"),
        "lsp.tracedecay.initialization.tracedecay",
        registration_path,
    )


def main() -> None:
    if len(sys.argv) != 2:
        fail("usage: check-packaged-plugin-manifests.py <packaged-plugin-root>")
    root = Path(sys.argv[1])
    validate_claude(root / ".claude-plugin/plugin.json")
    validate_codex(root / ".codex-plugin/plugin.json")
    validate_cursor(root / ".cursor-plugin/plugin.json")
    validate_kimi(root / ".kimi-plugin/plugin.json")
    validate_opencode(root)
    validate_host_skill_inventories(root)


if __name__ == "__main__":
    main()
