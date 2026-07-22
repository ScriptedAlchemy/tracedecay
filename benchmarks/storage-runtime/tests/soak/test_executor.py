from __future__ import annotations

import asyncio
import json
import os
import sys
import time
from pathlib import Path

import psutil
import pytest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

from runner_contract import ConfigError, SafetyError  # noqa: E402
from process_execution import process_tree_capability  # noqa: E402
from soak.executor import execute_fixed_argv  # noqa: E402
from soak.schemas import validate_plan, validate_result  # noqa: E402
from soak.scheduler import CampaignConfig, build_campaign  # noqa: E402
from soak.trends import RESOURCE_NAMES, TrendError, TrendPolicy, evaluate_resource_trends  # noqa: E402
import run_storage_baseline as rsb  # noqa: E402


def campaign(duration_seconds: int = 3) -> dict:
    return build_campaign(
        CampaignConfig(
            seed=7,
            duration_seconds=duration_seconds,
            rates_per_second={"current": 1, "ten_x": 2, "overload": 3},
            crash_count=0,
            restore_rehearsals=0,
        )
    )


def limits(value: float) -> dict[str, float]:
    return {name: value for name in RESOURCE_NAMES}


def test_plan_schema_rejects_arbitrary_argv_and_unknown_workload() -> None:
    malicious = campaign()
    malicious["argv"] = ["cargo", "install", "malicious"]
    with pytest.raises(ConfigError, match="schema"):
        validate_plan(malicious)

    unknown = campaign()
    unknown["workload_id"] = "shell-command"
    with pytest.raises(ConfigError, match="allowlist"):
        validate_plan(unknown)

    s11 = build_campaign(
        CampaignConfig(
            seed=7,
            duration_seconds=3,
            rates_per_second={"current": 1, "ten_x": 2, "overload": 3},
            crash_count=0,
            restore_rehearsals=0,
            workload_id="storage-runtime-s11-product-gates-v1",
        )
    )
    assert s11["workload_id"] == "storage-runtime-s11-product-gates-v1"


def test_result_schema_rejects_fabricated_legacy_receipt() -> None:
    fabricated = {
        "schema": "storage-runtime-soak-result-v2",
        "resource_samples": [],
        "post_eviction": {},
        "trend_policy": {},
        "execution_receipt": {"status": "completed"},
    }
    with pytest.raises(ConfigError, match="schema"):
        validate_result(fabricated)


def _process_tree_argv(pid_directory: Path) -> list[str]:
    helper = ROOT / "tests" / "fixtures" / "process_tree_helper.py"
    return [sys.executable, str(helper), "root", str(pid_directory)]


def _helper_pids(pid_directory: Path) -> set[int]:
    return {
        int((pid_directory / f"{role}.pid").read_text(encoding="ascii"))
        for role in ("root", "child", "grandchild")
    }


def _is_live(pid: int) -> bool:
    if not psutil.pid_exists(pid):
        return False
    try:
        return psutil.Process(pid).status() != psutil.STATUS_ZOMBIE
    except psutil.NoSuchProcess:
        return False


def _assert_processes_exit(pids: set[int]) -> None:
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline and any(_is_live(pid) for pid in pids):
        time.sleep(0.02)
    assert not {pid for pid in pids if _is_live(pid)}


def test_fixed_executor_times_out_and_kills_child_and_grandchild(tmp_path: Path) -> None:
    sandbox = tmp_path / "sandbox"
    sandbox.mkdir()
    result = asyncio.run(
        execute_fixed_argv(
            _process_tree_argv(sandbox),
            cwd=sandbox,
            env={},
            timeout_seconds=1.5,
        )
    )
    pids = _helper_pids(sandbox)
    assert result["timed_out"] is True
    assert result["process_tree_clean"] is True
    assert result["process_tree"]["peak_process_count"] >= 3
    assert set(result["process_tree"]["observed_pids"]).issuperset(pids)
    assert result["process_tree"]["child_process_coverage_complete"] is False
    _assert_processes_exit(pids)


def test_fixed_executor_cancellation_kills_observed_tree(tmp_path: Path) -> None:
    sandbox = tmp_path / "sandbox"
    sandbox.mkdir()

    async def cancel_running_tree() -> set[int]:
        task = asyncio.create_task(
            execute_fixed_argv(
                _process_tree_argv(sandbox),
                cwd=sandbox,
                env={},
                timeout_seconds=30,
            )
        )
        deadline = time.monotonic() + 5
        expected = [sandbox / f"{role}.pid" for role in ("root", "child", "grandchild")]
        while not all(path.exists() for path in expected):
            if time.monotonic() >= deadline:
                task.cancel()
                raise AssertionError("process tree did not start")
            await asyncio.sleep(0.02)
        pids = _helper_pids(sandbox)
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await task
        return pids

    _assert_processes_exit(asyncio.run(cancel_running_tree()))


@pytest.mark.skipif(sys.platform != "win32", reason="Windows-only containment contract")
def test_windows_reports_no_job_object_boundary() -> None:
    capability = process_tree_capability("windows")
    assert capability["state"] == "supported_best_effort"
    assert "Windows Job Object" in capability["limitation"]


def test_trends_must_cover_campaign_duration_and_cadence() -> None:
    policy = TrendPolicy(
        maximum_slope_per_second=limits(1),
        maximum_end_to_baseline_ratio=limits(2),
        maximum_post_eviction_ratio=limits(2),
        maximum_cadence_gap_seconds=2,
    )
    samples = [
        {"elapsed_seconds": second, **limits(100)}
        for second in (0, 1, 2)
    ]
    with pytest.raises(TrendError, match="campaign duration"):
        evaluate_resource_trends(
            samples,
            limits(100),
            policy,
            campaign_duration_seconds=10,
        )


def test_symlink_output_is_rejected_before_publication(tmp_path: Path) -> None:
    live = tmp_path / "home" / ".tracedecay"
    live.mkdir(parents=True)
    alias = tmp_path / "alias"
    try:
        alias.symlink_to(live)
    except OSError:
        pytest.skip("symlinks unsupported")
    from runner_commands import publish_json

    with pytest.raises(SafetyError):
        publish_json(alias / "plan.json", campaign(), "soak plan", home=tmp_path / "home")


@pytest.mark.skipif(os.name == "nt", reason="executable fixture script is POSIX-only")
def test_soak_run_executes_allowlisted_workload_and_owns_receipt(tmp_path: Path) -> None:
    binary = tmp_path / "tracedecay-fixture"
    binary.write_text(
        "#!/usr/bin/env python3\n"
        "import json\n"
        "print(json.dumps({'matches': [{'id': 1}]}))\n",
        encoding="utf-8",
    )
    binary.chmod(0o700)
    evidence_binary = tmp_path / "storage-runtime-evidence"
    evidence_binary.write_text("#!/usr/bin/env python3\nraise SystemExit(2)\n")
    evidence_binary.chmod(0o700)
    fixture = tmp_path / "fixture"
    (fixture / "project").mkdir(parents=True)
    (fixture / "profile").mkdir()
    (fixture / "storage-runtime-fixture-v1.json").write_text(
        json.dumps(
            {
                "schema_version": 1,
                "project_root": "project",
                "profile_root": "profile",
                "fts_queries": {"graph": "needle", "session": "needle"},
            }
        ),
        encoding="utf-8",
    )
    frozen_identity = tmp_path / "frozen.json"
    frozen_identity.write_text(
        json.dumps(
            {
                "artifact_id": "storage-runtime-frozen-identity-v3",
                "schema_version": 3,
                "product_commit_sha": "d" * 40,
                "product_binary": {
                    "kind": "file",
                    **rsb.binary_identity(binary),
                },
                "evidence_binary": {
                    "kind": "file",
                    **rsb.binary_identity(evidence_binary),
                },
            }
        ),
        encoding="utf-8",
    )
    plan = build_campaign(
        CampaignConfig(
            seed=1,
            duration_seconds=1,
            rates_per_second={"current": 0.1, "ten_x": 0.1, "overload": 0.1},
            crash_count=0,
            restore_rehearsals=0,
            sample_interval_seconds=0.1,
            operation_timeout_seconds=2,
        )
    )
    plan_path = tmp_path / "plan.json"
    plan_path.write_text(json.dumps(plan), encoding="utf-8")
    output = tmp_path / "output"

    assert (
        rsb.main(
            [
                "soak-run",
                "--plan",
                str(plan_path),
                "--product-binary",
                str(binary),
                "--evidence-binary",
                str(evidence_binary),
                "--fixture",
                str(fixture),
                "--frozen-identity",
                str(frozen_identity),
                "--family",
                "graph",
                "--output",
                str(output),
                "--mode",
                "lint",
            ]
        )
        == 0
    )
    result = json.loads(
        (output / "storage-runtime-soak-result.json").read_text(encoding="utf-8")
    )
    receipt = result["execution_receipt"]
    assert receipt["schema"] == "storage-runtime-soak-execution-receipt-v2"
    assert receipt["plan_sha256"] == plan["plan_sha256"]
    assert receipt["product_adapter_validated"] is True
    assert receipt["logical_evidence"] == []
