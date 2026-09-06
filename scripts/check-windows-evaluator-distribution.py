#!/usr/bin/env python3
"""Require Windows nextest shards to receive the packaged evaluator binaries."""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Any

import yaml


WORKFLOW_PATH = Path(__file__).resolve().parents[1] / ".github/workflows/ci.yml"
NEXTEST_CONFIG_PATH = Path(__file__).resolve().parents[1] / ".config/nextest.toml"
WORKSPACE_BIN_BUILD = (
    "cargo build --workspace --bins --locked --features tracedecay/test-helpers"
)
PACKAGE_COMMAND = "package-windows-evaluator-bins.py package"
RESTORE_COMMAND = "package-windows-evaluator-bins.py restore"
PREFLIGHT_COMMAND = "package-windows-evaluator-bins.py preflight"
ARTIFACT_NAME = "windows-evaluator-bins"
UPLOAD_STEP = "Upload Windows evaluator binaries"
DOWNLOAD_STEP = "Download Windows evaluator binaries"
RESTORE_STEP = "Restore Windows evaluator binaries"
PREFLIGHT_STEP = "Preflight Windows evaluator binaries"
TEST_STEP = "Run Windows tests"
SEARCH_EVAL_OVERRIDE = "TRACEDECAY_SEARCH_EVAL_TEST_BIN"
SEARCH_EVAL_DIRECT_OVERRIDE = "TRACEDECAY_SEARCH_EVAL_DIRECT_TEST_BIN"
FORBIDDEN_PACKAGE_BUILD = "cargo build -p tracedecay-search-eval"
ARCHIVE_INCLUDES = (
    "debug/tracedecay-search-eval.exe",
    "debug/tracedecay-search-eval-direct.exe",
)


def fail(message: str) -> None:
    print(f"Windows evaluator distribution policy violation: {message}", file=sys.stderr)
    raise SystemExit(1)


def job_steps(job: dict[str, Any]) -> list[dict[str, Any]]:
    steps = job.get("steps", [])
    return [step for step in steps if isinstance(step, dict)]


def find_step(steps: list[dict[str, Any]], fragment: str) -> int | None:
    return next(
        (index for index, step in enumerate(steps) if fragment in str(step)),
        None,
    )


def find_named_step(steps: list[dict[str, Any]], name: str) -> int | None:
    return next(
        (
            index
            for index, step in enumerate(steps)
            if step.get("name") == name
        ),
        None,
    )


def require_step(steps: list[dict[str, Any]], fragment: str, job: str) -> int:
    index = find_step(steps, fragment)
    if index is None:
        fail(f"'{job}' is missing {fragment!r}")
    return index


def require_named_step(steps: list[dict[str, Any]], name: str, job: str) -> int:
    index = find_named_step(steps, name)
    if index is None:
        fail(f"'{job}' is missing a step named {name!r}")
    return index


def assert_windows_build(job: dict[str, Any]) -> None:
    steps = job_steps(job)
    if find_step(steps, FORBIDDEN_PACKAGE_BUILD) is not None:
        fail(
            "'windows-build' must not use a differently featured "
            f"{FORBIDDEN_PACKAGE_BUILD} invocation"
        )
    build_index = require_step(steps, WORKSPACE_BIN_BUILD, "windows-build")
    package_index = require_step(steps, PACKAGE_COMMAND, "windows-build")
    upload_index = require_named_step(steps, UPLOAD_STEP, "windows-build")
    archive_index = require_step(steps, "cargo nextest archive", "windows-build")
    if ARTIFACT_NAME not in str(steps[upload_index]):
        fail("'windows-build' must upload the windows-evaluator-bins artifact")
    if not (build_index < package_index < upload_index and build_index < archive_index):
        fail(
            "'windows-build' must compile workspace bins, package the evaluator "
            "artifact, and archive tests in fail-closed order"
        )
    if "actions/upload-artifact@" not in str(steps[upload_index]):
        fail("'windows-build' must upload the evaluator support artifact")


def assert_windows_shard(job: dict[str, Any]) -> None:
    steps = job_steps(job)
    download_index = require_named_step(steps, DOWNLOAD_STEP, "windows-test-shard")
    restore_index = require_named_step(steps, RESTORE_STEP, "windows-test-shard")
    preflight_index = require_step(steps, PREFLIGHT_COMMAND, "windows-test-shard")
    named_preflight = require_named_step(steps, PREFLIGHT_STEP, "windows-test-shard")
    test_index = require_named_step(steps, TEST_STEP, "windows-test-shard")
    if ARTIFACT_NAME not in str(steps[download_index]):
        fail("'windows-test-shard' must download the windows-evaluator-bins artifact")
    if RESTORE_COMMAND not in str(steps[restore_index]):
        fail("'windows-test-shard' must restore evaluator binaries from the artifact")
    require_step(steps, SEARCH_EVAL_OVERRIDE, "windows-test-shard")
    require_step(steps, SEARCH_EVAL_DIRECT_OVERRIDE, "windows-test-shard")
    if not (
        download_index
        < restore_index
        < preflight_index
        == named_preflight
        < test_index
    ):
        fail(
            "'windows-test-shard' must download, restore, and preflight evaluator "
            "binaries before tests run"
        )
    test_step = steps[test_index]
    test_env = test_step.get("env")
    if not isinstance(test_env, dict):
        fail("'Run Windows tests' must bind evaluator executable overrides")
    if SEARCH_EVAL_OVERRIDE not in test_env or SEARCH_EVAL_DIRECT_OVERRIDE not in test_env:
        fail("'Run Windows tests' must set both search-eval executable overrides")


def assert_nextest_archive_includes(config_text: str) -> None:
    for path in ARCHIVE_INCLUDES:
        if path not in config_text:
            fail(f".config/nextest.toml must archive extra file {path!r}")


def main() -> None:
    text = WORKFLOW_PATH.read_text(encoding="utf-8")
    workflow = yaml.safe_load(text)
    jobs = workflow.get("jobs") if isinstance(workflow, dict) else None
    if not isinstance(jobs, dict):
        fail("ci.yml must define jobs")
    build = jobs.get("windows-build")
    shard = jobs.get("windows-test-shard")
    if not isinstance(build, dict) or not isinstance(shard, dict):
        fail("ci.yml must define windows-build and windows-test-shard")
    assert_windows_build(build)
    assert_windows_shard(shard)
    assert_nextest_archive_includes(NEXTEST_CONFIG_PATH.read_text(encoding="utf-8"))
    print(
        "ci.yml ships workspace-featured evaluator binaries to every Windows "
        "nextest shard and preflights them before tests."
    )


if __name__ == "__main__":
    main()
