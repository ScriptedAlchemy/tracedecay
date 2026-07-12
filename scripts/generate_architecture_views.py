#!/usr/bin/env python3
"""Generate checked V2 architecture views from architecture-boundaries.toml."""

from __future__ import annotations

import argparse
import json
import pathlib
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "architecture-boundaries.toml"
HEADER = "<!-- Generated from architecture-boundaries.toml; do not edit. -->\n"


def title(value: str) -> str:
    return value.replace("-", " ").title()


def owners_view(data: dict) -> str:
    lines = [HEADER.rstrip(), "# V2 Architecture Owners", "", "| Owner | Kind | Target | Tier | Public facade | Normative plan |", "|---|---|---|---:|---|---|"]
    for name, owner in data["owners"].items():
        facade = owner.get("public_facade", "private")
        lines.append(f"| {name} | {title(owner['kind'])} | `{owner['path']}` | {owner['release_tier']} | `{facade}` | `{owner['plan']}` |")
    lines += ["", f"Rust packages are capped at {data['package_ceiling']}. Root-private adapters remain module-lint boundaries, not package-admission precedents."]
    return "\n".join(lines) + "\n"


def dag_view(data: dict) -> str:
    lines = [HEADER.rstrip(), "# V2 Dependency DAG", "", "```mermaid", "flowchart TD"]
    for edge in data["edges"]:
        lines.append(f"  {edge['from'].replace('-', '_')}[\"{edge['from']}\"] --> {edge['to'].replace('-', '_')}[\"{edge['to']}\"]")
    lines += ["```", "", "An arrow means a compile-time import or generation dependency. This list is byte-generated from the complete allowed dependency sets in the authority manifest."]
    return "\n".join(lines) + "\n"


def release_view(data: dict) -> str:
    release = data["release"]
    lines = [HEADER.rstrip(), "# V2 Release and Deletion Policy", "", "## Release waves", ""]
    for index, wave in enumerate(release["waves"], 1):
        lines.append(f"{index}. `{wave}`")
    lines += ["", "## Mandatory gates", "", f"- Compatibility: {release['compatibility_gate']}", f"- Rollback: {release['rollback_gate']}", f"- V1 removal: {release['v1_removal_gate']}", "", "## Deletion waves", ""]
    for wave in data["deletion_waves"]:
        lines.append(f"- **{wave['id']} ({wave['delete_by_pr']}):** {wave['replaced_cluster']}")
    return "\n".join(lines) + "\n"


def scorecard_view(data: dict) -> str:
    lines = [HEADER.rstrip(), "# V2 Convergence Scorecard Skeleton", "", "| Metric | Detector | Target |", "|---|---|---|"]
    for metric in data["scorecard"]:
        lines.append(f"| {metric['metric']} | `{metric['detector']}` | {metric['target']} |")
    return "\n".join(lines) + "\n"


def dependency_policy(data: dict) -> str:
    lines = ["# Generated from architecture-boundaries.toml; do not edit.", f"version = {data['version']}", ""]
    for name, owner in data["owners"].items():
        lines += [
            f"[owners.{name}]",
            f"path = {json.dumps(owner['path'], ensure_ascii=False)}",
            f"allowed = {json.dumps(owner.get('allowed_dependencies', []), ensure_ascii=False)}",
            f"forbidden = {json.dumps(owner.get('forbidden_dependencies', []), ensure_ascii=False)}",
            "forbidden_source_patterns = "
            f"{json.dumps(owner.get('forbidden_source_patterns', []), ensure_ascii=False)}",
            "",
        ]
    rendered = "\n".join(lines)
    tomllib.loads(rendered)
    return rendered


def render(data: dict) -> dict[pathlib.Path, str]:
    return {
        ROOT / "docs/architecture/v2/generated/owners.md": owners_view(data),
        ROOT / "docs/architecture/v2/generated/dependency-dag.md": dag_view(data),
        ROOT / "docs/architecture/v2/generated/release-policy.md": release_view(data),
        ROOT / "docs/architecture/v2/generated/convergence-scorecard.md": scorecard_view(data),
        ROOT / "architecture-dependency-policy.toml": dependency_policy(data),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true", help="fail if checked views differ")
    args = parser.parse_args()
    data = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
    failures = []
    for path, expected in render(data).items():
        actual = path.read_text(encoding="utf-8") if path.exists() else None
        if args.check:
            if actual != expected:
                failures.append(str(path.relative_to(ROOT)))
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(expected, encoding="utf-8", newline="\n")
    if failures:
        print("architecture generated-view drift: " + ", ".join(failures), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
