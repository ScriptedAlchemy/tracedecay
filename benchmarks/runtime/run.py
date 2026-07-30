#!/usr/bin/env python3
"""Cargo-free command line entrypoint for runtime performance captures."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import socket
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path
from typing import Any, Mapping, NoReturn, Sequence

REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
if os.fspath(REPOSITORY_ROOT) not in sys.path:
    sys.path.insert(0, os.fspath(REPOSITORY_ROOT))

from benchmarks.runtime.fixtures import (
    FixtureError,
    PreparedFixture,
    clone_prepared_profile,
    fixture_source_root,
    isolated_environment,
    prepare_fixture_snapshot,
    provider_fixture_files,
    provider_roots,
)
from benchmarks.runtime.lifecycle import (
    LifecycleError,
    OwnedDaemon,
    ProbeResult,
    RunWorkspace,
)
from benchmarks.runtime.incident_workloads import incident_catalog_document
from benchmarks.runtime.policy import (
    PolicyViolation,
    evaluate_artifact,
    load_acceptance_policy,
    load_journey_policy,
    make_policy_receipt,
)
from benchmarks.runtime.scenarios import (
    SCENARIOS,
    WORKLOADS,
    Workload,
    WorkloadInputs,
    stable_digest,
)
from benchmarks.runtime.schema import (
    SchemaValidationError,
    read_jsonl,
    validate_report,
    validate_sample,
    write_jsonl,
)
from benchmarks.runtime.statistics import nearest_rank


SCHEMA_VERSION = 1
SUBCOMMANDS = ("prepare", "capture", "paired", "compare", "incidents", "smoke")
FORBIDDEN_REPORT_FIELDS = frozenset({"pr_stage", "milestone_budget_ns"})


class HarnessError(RuntimeError):
    """A user-actionable harness validation or execution failure."""


def fail(message: str) -> NoReturn:
    raise HarnessError(message)


def require_binary(value: str | os.PathLike[str]) -> Path:
    path = Path(value).expanduser()
    if not path.exists():
        fail(f"binary does not exist: {path}")
    if not path.is_file():
        fail(f"binary is not a regular file: {path}")
    if not os.access(path, os.X_OK):
        fail(f"binary is not executable: {path}")
    return path.resolve()


def load_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        fail(f"cannot read JSON report {path}: {exc}")
    if not isinstance(value, dict):
        fail(f"JSON report must be an object: {path}")
    return value


def write_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp-{os.getpid()}")
    temporary.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    temporary.replace(path)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def sha256_tree(path: Path) -> str:
    digest = hashlib.sha256()
    for entry in sorted(Path(path).rglob("*")):
        if not entry.is_file():
            continue
        digest.update(entry.relative_to(path).as_posix().encode("utf-8"))
        digest.update(b"\0")
        digest.update(bytes.fromhex(sha256_file(entry)))
    return digest.hexdigest()


def load_prepared_fixture(root: Path, *, candidate_binary: Path) -> PreparedFixture:
    root = Path(root).resolve()
    evidence_path = root / "evidence" / "prepared.json"
    if not root.is_dir() or not evidence_path.is_file():
        fail(f"prepared fixture is incomplete: {root}")
    evidence = load_json(evidence_path)
    if evidence.get("schema_version") != SCHEMA_VERSION:
        fail("prepared fixture schema version is unsupported")
    runtime_identity = evidence.get("runtime_identity")
    if not isinstance(runtime_identity, dict):
        fail("prepared fixture runtime identity is missing")
    copied_binary = root / "bin" / "tracedecay"
    prepared_binary = require_binary(copied_binary)
    if sha256_file(candidate_binary) != sha256_file(prepared_binary):
        fail("candidate binary does not match the prepared fixture binary")
    home = root / "home"
    project = home / "workspace" / "runtime-fixture"
    if not project.is_dir():
        fail("prepared fixture project is missing")
    prepared = PreparedFixture(
        snapshot_root=root,
        home=home,
        project=project,
        provider_roots=provider_roots(home),
        provider_files=provider_fixture_files(home),
        prebuilt_binary=prepared_binary,
        evidence_root=root / "evidence",
        prepared_evidence=evidence_path,
        runtime_identity=dict(runtime_identity),
        environment={},
        fixture_digests={},
        git_head="prepared",
    )
    return PreparedFixture(
        **{
            **prepared.__dict__,
            "environment": isolated_environment(prepared),
        }
    )


def runtime_scenario() -> tuple[Any, Workload]:
    scenario = next(
        (
            item
            for item in SCENARIOS
            if item.crate_lane.value == "tracedecay"
            and item.workload_id == "exact-symbol"
            and item.surface.value == "cli"
            and item.state.value == "cold-admission"
        ),
        None,
    )
    if scenario is None:
        fail("declarative cold exact-symbol scenario is missing")
    workload = next(
        (item for item in WORKLOADS if item.id == scenario.workload_id),
        None,
    )
    if workload is None:
        fail(f"scenario workload is missing: {scenario.workload_id}")
    return scenario, workload


def socket_probe(socket_path: Path) -> ProbeResult:
    if not socket_path.exists():
        return ProbeResult(
            ready=False,
            phase="daemon_socket",
            availability_state="unavailable",
            availability_detail="daemon socket not created",
        )
    connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    connection.settimeout(0.1)
    try:
        connection.connect(os.fspath(socket_path))
    except OSError as exc:
        return ProbeResult(
            ready=False,
            phase="daemon_socket",
            availability_state="unavailable",
            availability_detail=str(exc),
        )
    finally:
        connection.close()
    return ProbeResult(
        ready=True,
        phase="daemon_socket",
        availability_state="available",
        availability_detail=None,
        activation_state="active",
        restart_state="not_required",
    )


def _handler_duration_us(document: Any) -> int | None:
    if not isinstance(document, Mapping):
        return None
    metadata = document.get("_meta")
    if isinstance(metadata, Mapping):
        duration = metadata.get("duration_us")
        if isinstance(duration, int) and not isinstance(duration, bool) and duration >= 0:
            return duration
    for value in document.values():
        duration = _handler_duration_us(value)
        if duration is not None:
            return duration
    return None


def _machine_fingerprint() -> str:
    identity = "\0".join(
        (
            platform.system(),
            platform.machine(),
            platform.python_implementation(),
            platform.python_version(),
        )
    )
    return hashlib.sha256(identity.encode("utf-8")).hexdigest()[:24]


def _process_observations(process_id: int) -> dict[str, int]:
    observations: dict[str, int] = {}
    proc = Path("/proc") / str(process_id)
    try:
        stat_fields = (proc / "stat").read_text(encoding="utf-8").split()
        clock_ticks = int(os.sysconf("SC_CLK_TCK"))
        cpu_ticks = int(stat_fields[13]) + int(stat_fields[14])
        observations["daemon_cpu_time_ns"] = (
            cpu_ticks * 1_000_000_000 // clock_ticks
        )
    except (FileNotFoundError, IndexError, OSError, ValueError):
        pass
    try:
        status = (proc / "status").read_text(encoding="utf-8")
        for line in status.splitlines():
            if line.startswith(("VmHWM:", "VmRSS:")):
                label, value, _unit = line.split()
                if (
                    label == "VmHWM:"
                    or "daemon_peak_rss_bytes" not in observations
                ):
                    observations["daemon_peak_rss_bytes"] = int(value) * 1024
    except (FileNotFoundError, OSError, ValueError):
        pass
    try:
        smaps_rollup = (proc / "smaps_rollup").read_text(encoding="utf-8")
        for line in smaps_rollup.splitlines():
            if line.startswith("Pss:"):
                observations["daemon_pss_bytes"] = int(line.split()[1]) * 1024
                break
    except (FileNotFoundError, OSError, PermissionError, ValueError):
        pass
    try:
        io_values = {}
        for line in (proc / "io").read_text(encoding="utf-8").splitlines():
            key, value = line.split(":", 1)
            io_values[key] = int(value.strip())
        observations["disk_read_bytes"] = io_values["read_bytes"]
        observations["disk_write_bytes"] = io_values["write_bytes"]
    except (FileNotFoundError, KeyError, OSError, PermissionError, ValueError):
        pass
    return observations


def _resource_delta(
    before: Mapping[str, int],
    after: Mapping[str, int],
    *,
    logical_write_bytes: int | None,
) -> dict[str, int | None]:
    result: dict[str, int | None] = {}
    for field in ("daemon_cpu_time_ns", "disk_read_bytes", "disk_write_bytes"):
        if field in before and field in after:
            result[field] = max(0, after[field] - before[field])
    for field in ("daemon_peak_rss_bytes", "daemon_pss_bytes"):
        if field in after:
            result[field] = after[field]
    disk_write_bytes = result.get("disk_write_bytes")
    if isinstance(disk_write_bytes, int) and logical_write_bytes is not None:
        result["write_amplification_ppm"] = (
            disk_write_bytes * 1_000_000 // max(1, logical_write_bytes)
        )
    else:
        result["write_amplification_ppm"] = None
    result["memory_peak_bytes"] = None
    result["profiler_overhead_ns"] = None
    return result


def _wal_bytes(root: Path) -> int:
    return sum(
        path.stat().st_size
        for path in Path(root).rglob("*-wal")
        if path.is_file()
    )


def _artifact_paths(output: Path) -> tuple[Path, Path]:
    return output.with_suffix(".samples.jsonl"), output.with_suffix(".policy.json")


def _run_capture(
    *,
    binary: Path,
    prepared: PreparedFixture,
    output: Path,
    variant: str,
    round_index: int = 0,
    abba_position: int = 0,
) -> None:
    scenario, workload = runtime_scenario()
    try:
        initialized = subprocess.run(
            (
                os.fspath(binary),
                "init",
                os.fspath(prepared.project),
            ),
            cwd=prepared.project,
            env=prepared.environment,
            capture_output=True,
            check=False,
            timeout=30.0,
        )
    except subprocess.TimeoutExpired as exc:
        fail(f"fixture enrollment timed out: {exc}")
    if initialized.returncode != 0:
        detail = initialized.stderr.decode("utf-8", errors="replace").strip()
        fail(
            f"fixture enrollment failed with exit {initialized.returncode}: "
            f"{detail or 'no stderr'}"
        )
    run_id = f"run-{uuid.uuid4().hex}"
    capture_id = f"capture-{uuid.uuid4().hex}"
    runtime_identity = prepared.runtime_identity
    platform_id = str(runtime_identity["platform"])
    shard = str(runtime_identity["shard"])
    storage_mode = str(runtime_identity["storage_mode"])
    expected_symbol = "fixture_catalog"
    arguments = workload.arguments(WorkloadInputs(symbol=expected_symbol))
    request_payload = json.dumps(
        arguments,
        ensure_ascii=False,
        allow_nan=False,
        separators=(",", ":"),
        sort_keys=True,
    )
    socket_path = Path(prepared.environment["TRACEDECAY_DAEMON_SOCKET"])
    command = (
        os.fspath(binary),
        "tool",
        workload.tool,
        "--project",
        os.fspath(prepared.project),
        "--args",
        request_payload,
        "--json",
    )
    started_ns = time.monotonic_ns()
    daemon_started_ns = time.monotonic_ns()
    daemon = OwnedDaemon(
        (
            os.fspath(binary),
            "daemon",
            "run",
            "--socket",
            os.fspath(socket_path),
        ),
        env=prepared.environment,
        log_dir=prepared.snapshot_root / "daemon-logs",
        readiness=lambda: socket_probe(socket_path),
        readiness_timeout=10.0,
        poll_interval=0.01,
        termination_grace=1.0,
    )
    with daemon:
        admission_ns = time.monotonic_ns() - daemon_started_ns
        if daemon.process is None:
            fail("owned daemon process is missing after readiness")
        resources_before = _process_observations(daemon.process.pid)
        cli_started_ns = time.monotonic_ns()
        try:
            completed = subprocess.run(
                command,
                cwd=prepared.project,
                env=prepared.environment,
                capture_output=True,
                check=False,
                timeout=20.0,
            )
        except subprocess.TimeoutExpired as exc:
            fail(f"tool command timed out: {exc}")
        cli_wall_ns = time.monotonic_ns() - cli_started_ns
        if completed.returncode != 0:
            detail = completed.stderr.decode("utf-8", errors="replace").strip()
            fail(
                f"tool command failed with exit {completed.returncode}: "
                f"{detail or 'no stderr'}"
            )
        response = completed.stdout.strip()
        try:
            response_document = json.loads(response)
        except json.JSONDecodeError as exc:
            fail(f"tool command returned malformed JSON: {exc}")
        if expected_symbol not in json.dumps(
            response_document, ensure_ascii=False, sort_keys=True
        ):
            fail(f"exact-symbol smoke response did not contain {expected_symbol}")
        daemon_survived = daemon.is_alive
        process_count = daemon.evidence.process_count
        resources_after = _process_observations(daemon.process.pid)
    if daemon.evidence.process_count_after_cleanup != 0:
        fail("owned daemon process tree was not reaped")

    elapsed_ns = time.monotonic_ns() - started_ns
    result_digest = stable_digest(response_document, workload.digest_semantics)
    identity = scenario.sample_identity(
        run_id=run_id,
        variant=variant,
        machine_fingerprint=_machine_fingerprint(),
        round_index=round_index,
        abba_position=abba_position,
        capture_id=capture_id,
        platform=platform_id,
        shard=shard,
        storage_mode=storage_mode,
    )
    sample = {
        "schema_version": SCHEMA_VERSION,
        "identity": identity,
        "evidence": {
            "sample_count": 1,
            "evidence_class": "regression_sample",
        },
        "availability": {"state": "available", "detail": None},
        "timing": {
            "started_ns": started_ns,
            "elapsed_ns": elapsed_ns,
            "cli_wall_ns": cli_wall_ns,
            "mcp_wall_ns": None,
            "hook_wall_ns": None,
            "host_wall_ns": None,
            "handler_us": _handler_duration_us(response_document),
            "daemon_us": None,
            "admission_us": admission_ns // 1_000,
            "stages_us": {
                "daemon_admission": admission_ns // 1_000,
                "cli_call": cli_wall_ns // 1_000,
            },
            "shutdown_total_ns": None,
            "abort_offset_ns": None,
        },
        "size": {
            "process_count": process_count,
            "request_bytes": len(request_payload.encode("utf-8")),
            "response_bytes": len(completed.stdout),
            "content_bytes": len(response),
        },
        "lifecycle": {
            "timeout_phase": None,
            "activation_state": "active",
            "restart_state": "not_required",
            "daemon_survived": daemon_survived,
        },
        "observations": {
            **_resource_delta(
                resources_before,
                resources_after,
                logical_write_bytes=None,
            ),
            "wal_bytes": _wal_bytes(prepared.snapshot_root),
            "process_tree_reaped": True,
        },
        "outcome": {
            "status": "success",
            "expected_digest": result_digest,
            "actual_digest": result_digest,
            "result_digest": result_digest,
            "error": None,
        },
    }
    validate_sample(sample)
    samples_path, policy_path = _artifact_paths(output)
    write_jsonl(samples_path, [sample])
    samples_sha256 = sha256_file(samples_path)
    ended_ns = time.monotonic_ns()
    report = {
        "schema_version": SCHEMA_VERSION,
        "identity": {
            "report_id": f"report-{uuid.uuid4().hex}",
            **{
                key: value
                for key, value in identity.items()
                if key not in {"round_index", "abba_position"}
            },
            "samples_sha256": samples_sha256,
        },
        "evidence": {
            "sample_count": 1,
            "evidence_class": "regression_sample",
        },
        "timing": {"started_ns": started_ns, "ended_ns": ended_ns},
        "size": {
            "sample_count": 1,
            "process_count": process_count,
            "request_bytes": len(request_payload.encode("utf-8")),
            "response_bytes": len(completed.stdout),
            "content_bytes": len(response),
        },
        "availability": {
            "available_count": 1,
            "unavailable_count": 0,
            "unsupported_count": 0,
            "partial_count": 0,
            "failed_count": 0,
        },
        "outcome": {
            "success_count": 1,
            "error_count": 0,
            "timeout_count": 0,
            "digest_mismatch_count": 0,
            "daemon_death_count": 0,
        },
        "statistics": {
            "latency_ns": {
                "sample_count": 1,
                "p50": elapsed_ns,
                "p95": {
                    "available": False,
                    "value": None,
                    "minimum_samples": 40,
                },
                "p99": {
                    "available": False,
                    "value": None,
                    "minimum_samples": 100,
                },
            }
        },
    }
    validate_report(report)
    policy = load_acceptance_policy(
        Path(__file__).resolve().with_name("policies") / "acceptance-v1.json"
    )
    evaluate_artifact(
        {"sample_count": 1, "measurements": [{"latency_ns": elapsed_ns}]},
        policy,
    )
    write_json(output, report)
    receipt = make_policy_receipt(
        policy,
        artifact_sha256=sha256_file(output),
    )
    write_json(policy_path, receipt)


def validate_comparison_report(report: dict[str, Any], label: str) -> None:
    forbidden = sorted(FORBIDDEN_REPORT_FIELDS.intersection(report))
    if forbidden:
        fail(f"{label} report contains forbidden field: {forbidden[0]}")

    identity = report.get("identity")
    if not isinstance(identity, dict):
        fail(f"{label} report identity is required")
    for field in ("crate_id", "journey_id", "workload_id"):
        if not isinstance(identity.get(field), str) or not identity[field]:
            fail(f"{label} report identity.{field} is required")

    route = report.get("production_route")
    outcome = report.get("outcome")
    if (
        isinstance(identity.get("journey_id"), str)
        and identity["journey_id"].startswith("remote-")
        and isinstance(outcome, dict)
        and outcome.get("status") == "success"
    ):
        if not isinstance(route, dict) or not (
            route.get("committed") is True and route.get("mounted") is True
        ):
            fail(
                f"{label} remote route is unwired: committed production route "
                "must be mounted before success"
            )

    if "measurements" not in report:
        fail(f"{label} report measurements are required")


def prepare(args: argparse.Namespace) -> int:
    binary = require_binary(args.binary)
    output = Path(args.output)
    if output.exists():
        fail(f"prepared output already exists: {output}")
    try:
        prepare_fixture_snapshot(output, prebuilt_binary=binary)
    except FixtureError as exc:
        fail(str(exc))
    return 0


def compare(args: argparse.Namespace) -> int:
    baseline = load_json(Path(args.baseline))
    treatment = load_json(Path(args.treatment))
    baseline_schema = baseline.get("schema_version")
    treatment_schema = treatment.get("schema_version")
    if baseline_schema != treatment_schema:
        fail(
            "report schema mismatch: "
            f"baseline={baseline_schema!r}, treatment={treatment_schema!r}"
        )
    if baseline_schema != SCHEMA_VERSION:
        fail(f"unsupported report schema: {baseline_schema!r}")
    validate_comparison_report(baseline, "baseline")
    validate_comparison_report(treatment, "treatment")
    baseline_fixture = baseline.get("fixture_digest")
    treatment_fixture = treatment.get("fixture_digest")
    if (
        baseline_fixture is not None
        and treatment_fixture is not None
        and baseline_fixture != treatment_fixture
    ):
        fail("report fixture digest mismatch")
    write_json(
        Path(args.output),
        {
            "schema_version": SCHEMA_VERSION,
            "decision": "descriptive_only",
            "evidence_class": "n=1_regression_only",
            "latency_policy": "advisory_until_stable_baseline",
            "baseline": os.fspath(Path(args.baseline)),
            "treatment": os.fspath(Path(args.treatment)),
        },
    )
    return 0


def incidents(args: argparse.Namespace) -> int:
    output = Path(args.output)
    if output.exists():
        fail(f"incident catalog output already exists: {output}")
    write_json(output, incident_catalog_document())
    return 0


def capture(args: argparse.Namespace) -> int:
    binary = require_binary(args.binary)
    output = Path(args.output)
    samples_path, policy_path = _artifact_paths(output)
    for path in (output, samples_path, policy_path):
        if path.exists():
            fail(f"capture output already exists: {path}")
    prepared_source = (
        load_prepared_fixture(Path(args.prepared), candidate_binary=binary)
        if args.prepared
        else None
    )
    output.parent.mkdir(parents=True, exist_ok=True)
    with RunWorkspace(
        Path(tempfile.gettempdir()),
        preserve_on_failure=False,
    ) as workspace:
        if prepared_source is None:
            prepared = prepare_fixture_snapshot(
                workspace.path / "fixture",
                prebuilt_binary=binary,
            )
        else:
            prepared = clone_prepared_profile(
                prepared_source,
                workspace.path / "fixture",
                runtime_state="cold",
                temperature="cold",
            )
        try:
            _run_capture(
                binary=prepared.prebuilt_binary,
                prepared=prepared,
                output=output,
                variant="candidate",
            )
        except (FixtureError, LifecycleError, PolicyViolation, SchemaValidationError) as exc:
            fail(str(exc))
    return 0


def paired(args: argparse.Namespace) -> int:
    baseline = require_binary(args.baseline)
    treatment = require_binary(args.treatment)
    if baseline.samefile(treatment):
        fail("baseline and treatment resolve to the same binary")
    if sha256_file(baseline) == sha256_file(treatment):
        fail("baseline and treatment binaries have identical content")
    samples_per_variant = args.samples_per_variant
    if (
        isinstance(samples_per_variant, bool)
        or samples_per_variant < 2
        or samples_per_variant % 2 != 0
    ):
        fail("samples-per-variant must be a positive even integer of at least 2")
    output = Path(args.output)
    samples_path, policy_path = _artifact_paths(output)
    for path in (output, samples_path, policy_path):
        if path.exists():
            fail(f"paired output already exists: {path}")

    schedule = (
        ("baseline", baseline),
        ("treatment", treatment),
        ("treatment", treatment),
        ("baseline", baseline),
    )
    samples: list[dict[str, Any]] = []
    output.parent.mkdir(parents=True, exist_ok=True)
    with RunWorkspace(
        Path(tempfile.gettempdir()),
        preserve_on_failure=False,
    ) as workspace:
        base = prepare_fixture_snapshot(
            workspace.path / "base",
            prebuilt_binary=baseline,
        )
        for cycle in range(samples_per_variant // 2):
            for position, (variant, binary) in enumerate(schedule):
                sample_root = workspace.path / f"sample-{cycle}-{position}"
                prepared = clone_prepared_profile(
                    base,
                    sample_root,
                    prebuilt_binary=binary,
                    runtime_state="cold",
                    temperature="cold",
                )
                sample_output = workspace.path / f"capture-{cycle}-{position}.json"
                _run_capture(
                    binary=prepared.prebuilt_binary,
                    prepared=prepared,
                    output=sample_output,
                    variant=variant,
                    round_index=cycle,
                    abba_position=position,
                )
                captured = read_jsonl(sample_output.with_suffix(".samples.jsonl"))
                if len(captured) != 1:
                    fail("paired child capture did not produce exactly one raw sample")
                samples.append(captured[0])

    result_digests = {
        sample["outcome"]["result_digest"]
        for sample in samples
        if sample["outcome"]["status"] == "success"
    }
    if len(result_digests) != 1:
        fail("paired same-input result digest mismatch")
    write_jsonl(samples_path, samples)

    def summarize_variant(variant: str) -> dict[str, Any]:
        matching = [
            sample
            for sample in samples
            if sample["identity"]["variant"] == variant
        ]
        latencies = [
            sample["timing"]["elapsed_ns"]
            for sample in matching
            if sample["timing"]["elapsed_ns"] is not None
        ]
        return {
            "sample_count": len(matching),
            "latency_ns": {
                "p50": {
                    "available": len(latencies) >= 2,
                    "value": (
                        nearest_rank(latencies, 0.50)
                        if len(latencies) >= 2
                        else None
                    ),
                    "minimum_samples": 2,
                },
                "p95": {
                    "available": len(latencies) >= 40,
                    "value": (
                        nearest_rank(latencies, 0.95)
                        if len(latencies) >= 40
                        else None
                    ),
                    "minimum_samples": 40,
                },
                "p99": {
                    "available": len(latencies) >= 100,
                    "value": (
                        nearest_rank(latencies, 0.99)
                        if len(latencies) >= 100
                        else None
                    ),
                    "minimum_samples": 100,
                },
            },
        }

    policy = load_acceptance_policy(
        Path(__file__).resolve().with_name("policies") / "acceptance-v1.json"
    )
    journey_policy = load_journey_policy(
        Path(__file__).resolve().with_name("policies")
        / "journey-margins-v1.json"
    )
    baseline_summary = summarize_variant("baseline")
    treatment_summary = summarize_variant("treatment")
    evaluate_artifact(
        {
            "sample_count": samples_per_variant,
            "measurements": [
                {"latency_ns": sample["timing"]["elapsed_ns"]}
                for sample in samples
            ],
        },
        policy,
    )
    report = {
        "schema_version": SCHEMA_VERSION,
        "report_id": f"paired-{uuid.uuid4().hex}",
        "evidence_class": "repeated_raw_samples",
        "fixture": {
            "id": "runtime-v2-final",
            "same_input": True,
            "input_sha256": sha256_tree(fixture_source_root()),
        },
        "binaries": {
            "baseline_sha256": sha256_file(baseline),
            "treatment_sha256": sha256_file(treatment),
        },
        "schedule": "ABBA",
        "samples_sha256": sha256_file(samples_path),
        "variants": {
            "baseline": baseline_summary,
            "treatment": treatment_summary,
        },
        "outcome": {
            "digest_match": True,
            "error_count": sum(
                sample["outcome"]["status"] != "success" for sample in samples
            ),
            "process_leak_count": sum(
                sample["observations"].get("process_tree_reaped") is not True
                for sample in samples
            ),
        },
        "policy": {
            "policy_id": policy.policy_id,
            "policy_sha256": policy.sha256,
            "latency_mode": policy.latency_mode,
            "journey_policy_id": journey_policy.policy_id,
            "journey_policy_sha256": journey_policy.sha256,
            "journey_margins": journey_policy.journeys["cli"],
        },
    }
    write_json(output, report)
    artifact_sha256 = sha256_file(output)
    write_json(
        policy_path,
        {
            "acceptance": make_policy_receipt(
                policy,
                artifact_sha256=artifact_sha256,
            ),
            "journey": make_policy_receipt(
                journey_policy,
                artifact_sha256=artifact_sha256,
            ),
        },
    )
    return 0


def parser() -> argparse.ArgumentParser:
    argument_parser = argparse.ArgumentParser(
        description="Capture Cargo-free TraceDecay CLI/MCP runtime performance evidence."
    )
    subparsers = argument_parser.add_subparsers(
        dest="command",
        required=True,
        metavar="{" + ",".join(SUBCOMMANDS) + "}",
    )

    prepare_parser = subparsers.add_parser(
        "prepare",
        help="prepare deterministic fixtures without starting a daemon",
    )
    prepare_parser.add_argument("--binary", required=True)
    prepare_parser.add_argument("--output", required=True)
    prepare_parser.set_defaults(handler=prepare)

    capture_parser = subparsers.add_parser(
        "capture",
        help="capture one candidate into raw JSONL and a report",
    )
    capture_parser.add_argument("--binary", required=True)
    capture_parser.add_argument("--output", required=True)
    capture_parser.add_argument("--prepared")
    capture_parser.set_defaults(handler=capture)

    paired_parser = subparsers.add_parser(
        "paired",
        help="capture an ABBA baseline/treatment comparison",
    )
    paired_parser.add_argument("--baseline", required=True)
    paired_parser.add_argument("--treatment", required=True)
    paired_parser.add_argument("--output", required=True)
    paired_parser.add_argument("--prepared")
    paired_parser.add_argument("--samples-per-variant", type=int, default=4)
    paired_parser.set_defaults(handler=paired)

    compare_parser = subparsers.add_parser(
        "compare",
        help="compare compatible captured reports",
    )
    compare_parser.add_argument("--baseline", required=True)
    compare_parser.add_argument("--treatment", required=True)
    compare_parser.add_argument("--output", required=True)
    compare_parser.set_defaults(handler=compare)

    incidents_parser = subparsers.add_parser(
        "incidents",
        help="write fail-closed final incident workload catalog",
    )
    incidents_parser.add_argument("--output", required=True)
    incidents_parser.set_defaults(handler=incidents)

    smoke_parser = subparsers.add_parser(
        "smoke",
        help="run a bounded capture against an explicit prebuilt binary",
    )
    smoke_parser.add_argument("--binary", required=True)
    smoke_parser.add_argument("--output", required=True)
    smoke_parser.add_argument("--prepared")
    smoke_parser.set_defaults(handler=capture)
    return argument_parser


def main(argv: Sequence[str] | None = None) -> int:
    arguments = parser().parse_args(argv)
    try:
        return int(arguments.handler(arguments))
    except HarnessError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
