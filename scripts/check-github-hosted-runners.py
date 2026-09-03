#!/usr/bin/env python3
"""Require every Actions job to resolve to a standard GitHub-hosted runner."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

import yaml


STANDARD_GITHUB_HOSTED_RUNNERS = frozenset(
    {
        "ubuntu-latest",
        "ubuntu-24.04",
        "ubuntu-22.04",
        "ubuntu-24.04-arm",
        "ubuntu-22.04-arm",
        "windows-latest",
        "windows-2025",
        "windows-2022",
        "macos-latest",
        "macos-15",
        "macos-15-intel",
        "macos-14",
    }
)
RELEASE_MATRIX_EXPRESSION = "${{ matrix.runner }}"
INLINE_MATRIX_EXPRESSION = "${{ fromJSON(matrix.runner) }}"
RELEASE_WORKFLOWS = frozenset({"release.yml", "release-beta.yml"})


class PolicyViolation(ValueError):
    """A workflow can route work outside the standard GitHub-hosted fleet."""


def require_hosted_label(label: object, authority: str) -> None:
    if not isinstance(label, str) or label not in STANDARD_GITHUB_HOSTED_RUNNERS:
        raise PolicyViolation(
            f"{authority} resolves to {label!r}; use a standard GitHub-hosted runner"
        )


def inline_matrix_runners(job: dict[str, Any], authority: str) -> list[str]:
    strategy = job.get("strategy")
    matrix = strategy.get("matrix") if isinstance(strategy, dict) else None
    includes = matrix.get("include") if isinstance(matrix, dict) else None
    if not isinstance(includes, list) or not includes:
        raise PolicyViolation(f"{authority} has no auditable inline matrix entries")

    runners: list[str] = []
    for index, entry in enumerate(includes):
        encoded = entry.get("runner") if isinstance(entry, dict) else None
        if not isinstance(encoded, str):
            raise PolicyViolation(f"{authority} matrix entry {index} has no runner")
        try:
            runner = json.loads(encoded)
        except json.JSONDecodeError as error:
            raise PolicyViolation(
                f"{authority} matrix entry {index} is not a JSON runner label"
            ) from error
        require_hosted_label(runner, f"{authority} matrix entry {index}")
        runners.append(runner)
    return runners


def validate_release_manifest(root: Path) -> None:
    manifest_path = root / ".github/release-targets.json"
    document = json.loads(manifest_path.read_text(encoding="utf-8"))
    includes = document.get("include") if isinstance(document, dict) else None
    if not isinstance(includes, list) or not includes:
        raise PolicyViolation(".github/release-targets.json has no release targets")
    for index, entry in enumerate(includes):
        runner = entry.get("runner") if isinstance(entry, dict) else None
        require_hosted_label(runner, f"release target {index}")


def validate_workflow(path: Path) -> None:
    document = yaml.safe_load(path.read_text(encoding="utf-8"))
    jobs = document.get("jobs") if isinstance(document, dict) else None
    if not isinstance(jobs, dict):
        raise PolicyViolation(f"{path.name} has no jobs mapping")

    for name, job in jobs.items():
        if not isinstance(job, dict) or "runs-on" not in job:
            continue
        runner = job["runs-on"]
        authority = f"{path.name} job {name!r}"
        if not isinstance(runner, str):
            raise PolicyViolation(
                f"{authority} uses runner labels/groups instead of one hosted label"
            )
        if "${{" not in runner:
            require_hosted_label(runner, authority)
        elif runner == RELEASE_MATRIX_EXPRESSION and path.name in RELEASE_WORKFLOWS:
            # The shared release manifest is validated independently.
            continue
        elif runner == INLINE_MATRIX_EXPRESSION:
            inline_matrix_runners(job, authority)
        else:
            raise PolicyViolation(
                f"{authority} uses unaudited dynamic runner expression {runner!r}"
            )


def validate_repository(root: Path) -> None:
    validate_release_manifest(root)
    workflow_paths = sorted((root / ".github/workflows").glob("*.y*ml"))
    if not workflow_paths:
        raise PolicyViolation("repository has no GitHub Actions workflows")
    for path in workflow_paths:
        validate_workflow(path)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[1],
    )
    args = parser.parse_args()
    try:
        validate_repository(args.root.resolve())
    except (OSError, json.JSONDecodeError, yaml.YAMLError, PolicyViolation) as error:
        print(f"GitHub-hosted runner policy violation: {error}", file=sys.stderr)
        raise SystemExit(1) from error
    print("all Actions jobs use standard GitHub-hosted runners")


if __name__ == "__main__":
    main()
