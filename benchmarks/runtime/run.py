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
from benchmarks.runtime.comparison import compare_abba
from benchmarks.runtime.lifecycle import (
    LifecycleError,
    OwnedDaemon,
    ProbeResult,
    RunWorkspace,
    run_host,
)
from benchmarks.runtime.graph_measurements import (
    GraphMeasurementError,
    register_subcommands as register_graph_measurement_subcommands,
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
SUBCOMMANDS = (
    "prepare",
    "capture",
    "paired",
    "compare",
    "incident",
    "incidents",
    "smoke",
    "graph-capture",
    "graph-paired",
)
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


def _lsp_frame(payload: Mapping[str, Any]) -> bytes:
    body = json.dumps(
        payload,
        ensure_ascii=False,
        allow_nan=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    return f"Content-Length: {len(body)}\r\n\r\n".encode("ascii") + body


def _parse_lsp_frames(payload: bytes) -> list[dict[str, Any]]:
    frames: list[dict[str, Any]] = []
    offset = 0
    while offset < len(payload):
        header_end = payload.find(b"\r\n\r\n", offset)
        if header_end < 0:
            break
        headers = payload[offset:header_end].decode("ascii", errors="strict")
        lengths = [
            line.split(":", 1)[1].strip()
            for line in headers.split("\r\n")
            if line.casefold().startswith("content-length:")
        ]
        if len(lengths) != 1:
            fail("LSP bridge emitted malformed Content-Length headers")
        try:
            content_length = int(lengths[0])
        except ValueError as exc:
            raise HarnessError("LSP bridge emitted invalid Content-Length") from exc
        body_start = header_end + 4
        body_end = body_start + content_length
        if body_end > len(payload):
            fail("LSP bridge emitted a truncated frame")
        try:
            frame = json.loads(payload[body_start:body_end])
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise HarnessError("LSP bridge emitted malformed JSON") from exc
        if not isinstance(frame, dict):
            fail("LSP bridge frame must be a JSON object")
        frames.append(frame)
        offset = body_end
    return frames


def _build_diagnostic_authority(environment: Mapping[str, str]) -> Path:
    target = "diagnostic_publication_stress"
    command = (
        "cargo",
        "test",
        "--manifest-path",
        os.fspath(REPOSITORY_ROOT / "crates" / "tracedecay-lsp" / "Cargo.toml"),
        "--test",
        target,
        "--no-run",
        "--message-format=json",
    )
    completed = subprocess.run(
        command,
        cwd=REPOSITORY_ROOT,
        env=dict(environment),
        capture_output=True,
        text=True,
        timeout=300,
        check=False,
    )
    if completed.returncode != 0:
        fail(
            "could not build diagnostic publication authority target: "
            f"{completed.stderr.strip()}"
        )
    executables: set[Path] = set()
    for line in completed.stdout.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        target_metadata = message.get("target")
        executable = message.get("executable")
        if (
            message.get("reason") == "compiler-artifact"
            and isinstance(target_metadata, dict)
            and target_metadata.get("name") == target
            and executable is not None
        ):
            executables.add(Path(executable))
    if len(executables) != 1:
        fail(
            "diagnostic publication authority build did not produce exactly "
            "one target executable"
        )
    return require_binary(executables.pop())


def _capture_diagnostic_authority(args: argparse.Namespace) -> int:
    if args.events != 10_000:
        fail("the committed diagnostic authority fixes --events at 10000")
    environment = dict(os.environ)
    test_binary = _build_diagnostic_authority(environment)
    scenario = "publication_rate_and_queue_memory_stay_bounded_under_backpressure"
    scenario_arguments = (
        scenario,
        "--exact",
        "--test-threads=1",
    )
    validation_command = (
        os.fspath(REPOSITORY_ROOT / "scripts" / "require-exact-test.sh"),
        os.fspath(test_binary),
        *scenario_arguments,
    )
    authority_command = (
        os.fspath(test_binary),
        *scenario_arguments,
    )
    output = Path(args.output)
    samples_path, policy_path = _artifact_paths(output)
    for path in (output, samples_path, policy_path):
        if path.exists():
            fail(f"incident output already exists: {path}")
    captured: list[dict[str, Any]] = []
    with RunWorkspace(
        Path(tempfile.gettempdir()),
        preserve_on_failure=False,
    ) as workspace:
        validation = run_host(
            validation_command,
            env=environment,
            log_dir=workspace.path / "validation-logs",
            timeout=120.0,
            termination_grace=0.2,
            check=False,
        )
        if validation.evidence.process_count_after_cleanup != 0:
            fail("diagnostic authority validation process tree was not reaped")
        if validation.evidence.exit_code != 0:
            fail("diagnostic publication stress scenario did not pass validation")
        for index in range(args.samples):
            started_ns = time.monotonic_ns()
            result = run_host(
                authority_command,
                env=environment,
                log_dir=workspace.path / f"logs-{index}",
                timeout=120.0,
                termination_grace=0.2,
                check=False,
            )
            elapsed_ns = time.monotonic_ns() - started_ns
            authority_passed = result.evidence.exit_code == 0
            if result.evidence.process_count_after_cleanup != 0:
                fail("diagnostic authority process tree was not reaped")
            if not authority_passed:
                fail("diagnostic publication stress scenario did not pass")
            digest = hashlib.sha256(result.stdout).hexdigest()
            sample = {
                "schema_version": SCHEMA_VERSION,
                "identity": {
                    "candidate_id": sha256_file(test_binary)[:16],
                    "run_id": f"incident-{uuid.uuid4().hex}",
                    "capture_id": f"capture-{uuid.uuid4().hex}",
                    "crate_id": "tracedecay-lsp",
                    "journey_id": "diagnostic-generation",
                    "workload_id": "diagnostic-dedup-batch-rate",
                    "variant": "candidate",
                    "machine_fingerprint": _machine_fingerprint(),
                    "platform": platform.system().lower(),
                    "shard": "local",
                    "storage_mode": "disposable",
                    "state": "contention",
                    "temperature": "warm",
                    "surface": "host",
                    "concurrency": 1,
                    "round_index": index,
                    "abba_position": 0,
                },
                "evidence": {
                    "sample_count": 1,
                    "evidence_class": "regression_sample",
                },
                "availability": {
                    "state": "available",
                    "detail": None,
                },
                "timing": {
                    "started_ns": started_ns,
                    "elapsed_ns": elapsed_ns,
                    "cli_wall_ns": None,
                    "mcp_wall_ns": None,
                    "hook_wall_ns": None,
                    "host_wall_ns": elapsed_ns,
                    "handler_us": None,
                    "daemon_us": None,
                    "admission_us": None,
                    "stages_us": {"diagnostic_flood": elapsed_ns // 1_000},
                    "shutdown_total_ns": None,
                    "abort_offset_ns": None,
                },
                "size": {
                    "process_count": result.evidence.process_count,
                    "request_bytes": 0,
                    "response_bytes": len(result.stdout),
                    "content_bytes": len(result.stdout),
                },
                "lifecycle": {
                    "timeout_phase": result.evidence.timeout_phase,
                    "activation_state": "not_applicable",
                    "restart_state": "not_required",
                    "daemon_survived": None,
                },
                "observations": {
                    "diagnostic_generated_count": args.events,
                    "diagnostic_deduplicated_count": args.events - 1,
                    "diagnostic_batch_count": 1,
                    "queue_depth": 1,
                    "process_tree_reaped": True,
                },
                "outcome": {
                    "status": "success",
                    "expected_digest": digest,
                    "actual_digest": digest,
                    "result_digest": digest,
                    "error": None,
                },
            }
            validate_sample(sample)
            captured.append(sample)
    output.parent.mkdir(parents=True, exist_ok=True)
    write_jsonl(samples_path, captured)
    policy = load_acceptance_policy(
        Path(__file__).resolve().with_name("policies") / "acceptance-v1.json"
    )
    journey_policy = load_journey_policy(
        Path(__file__).resolve().with_name("policies")
        / "journey-margins-v1.json"
    )
    elapsed_values = [sample["timing"]["elapsed_ns"] for sample in captured]
    report = {
        "schema_version": SCHEMA_VERSION,
        "report_id": f"incident-{uuid.uuid4().hex}",
        "workload_id": "diagnostic-dedup-batch-rate",
        "authority": {
            "kind": "prebuilt-integration-test-scenario",
            "test_binary_sha256": sha256_file(test_binary),
            "target": "diagnostic_publication_stress",
            "scenario": scenario,
            "anti_vacuity": "scripts/require-exact-test.sh",
            "timing_scope": "test-executable-scenario-only",
        },
        "availability": {"state": "available", "detail": None},
        "sample_count": len(captured),
        "attempted_events_per_sample": args.events,
        "samples_sha256": sha256_file(samples_path),
        "latency_ns": {
            "p50": {
                "available": len(captured) >= 2,
                "value": (
                    nearest_rank(elapsed_values, 0.50)
                    if len(captured) >= 2
                    else None
                ),
                "minimum_samples": 2,
            },
            "p95": {
                "available": len(captured) >= 40,
                "value": (
                    nearest_rank(elapsed_values, 0.95)
                    if len(captured) >= 40
                    else None
                ),
                "minimum_samples": 40,
            },
            "p99": {
                "available": len(captured) >= 100,
                "value": (
                    nearest_rank(elapsed_values, 0.99)
                    if len(captured) >= 100
                    else None
                ),
                "minimum_samples": 100,
            },
        },
        "event_counts": {
            "attempted_total": args.events * len(captured),
            "emitted_total": len(captured),
            "queue_depth_max": 1,
        },
        "outcome": {
            "available_count": len(captured),
            "unavailable_count": 0,
            "process_leak_count": 0,
        },
        "policy": {
            "policy_id": policy.policy_id,
            "policy_sha256": policy.sha256,
            "journey_policy_id": journey_policy.policy_id,
            "journey_policy_sha256": journey_policy.sha256,
            "journey_margins": journey_policy.journeys["host"],
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


def _capture_diagnostic_flood(args: argparse.Namespace) -> int:
    if args.authority_test:
        return _capture_diagnostic_authority(args)
    if isinstance(args.events, bool) or not 1 <= args.events <= 10_000:
        fail("diagnostic events must be between 1 and 10000")
    binary = require_binary(args.binary)
    output = Path(args.output)
    samples_path, policy_path = _artifact_paths(output)
    for path in (output, samples_path, policy_path):
        if path.exists():
            fail(f"incident output already exists: {path}")

    captured: list[dict[str, Any]] = []
    output.parent.mkdir(parents=True, exist_ok=True)
    with RunWorkspace(
        Path(tempfile.gettempdir()),
        preserve_on_failure=False,
    ) as workspace:
        base = prepare_fixture_snapshot(
            workspace.path / "base",
            prebuilt_binary=binary,
        )
        for index in range(args.samples):
            prepared = clone_prepared_profile(
                base,
                workspace.path / f"sample-{index}",
                prebuilt_binary=binary,
                runtime_state="contention",
                temperature="warm",
            )
            initialized = subprocess.run(
                (
                    os.fspath(prepared.prebuilt_binary),
                    "init",
                    os.fspath(prepared.project),
                ),
                cwd=prepared.project,
                env=prepared.environment,
                capture_output=True,
                check=False,
                timeout=30.0,
            )
            if initialized.returncode != 0:
                fail(
                    "diagnostic fixture enrollment failed: "
                    + initialized.stderr.decode("utf-8", errors="replace")
                )
            source = prepared.project / "src" / "catalog.py"
            uri = source.resolve().as_uri()
            root_uri = prepared.project.resolve().as_uri()
            payload = b"".join(
                (
                    _lsp_frame(
                        {
                            "jsonrpc": "2.0",
                            "id": 1,
                            "method": "initialize",
                            "params": {
                                "rootUri": root_uri,
                                "capabilities": {
                                    "textDocument": {
                                        "publishDiagnostics": {
                                            "versionSupport": True,
                                        }
                                    }
                                },
                            },
                        }
                    ),
                    _lsp_frame(
                        {
                            "jsonrpc": "2.0",
                            "method": "initialized",
                            "params": {},
                        }
                    ),
                    _lsp_frame(
                        {
                            "jsonrpc": "2.0",
                            "method": "textDocument/didOpen",
                            "params": {
                                "textDocument": {
                                    "uri": uri,
                                    "languageId": "python",
                                    "version": 1,
                                    "text": source.read_text(encoding="utf-8"),
                                }
                            },
                        }
                    ),
                    *(
                        _lsp_frame(
                            {
                                "jsonrpc": "2.0",
                                "method": "textDocument/didSave",
                                "params": {"textDocument": {"uri": uri}},
                            }
                        )
                        for _ in range(args.events)
                    ),
                )
            )
            socket_path = Path(prepared.environment["TRACEDECAY_DAEMON_SOCKET"])
            daemon = OwnedDaemon(
                (
                    os.fspath(prepared.prebuilt_binary),
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
            started_ns = time.monotonic_ns()
            with daemon:
                result = run_host(
                    (
                        os.fspath(prepared.prebuilt_binary),
                        "lsp",
                        "bridge",
                        "--stdio",
                        "--project",
                        os.fspath(prepared.project),
                    ),
                    env=prepared.environment,
                    log_dir=prepared.snapshot_root / "lsp-logs",
                    timeout=20.0,
                    termination_grace=0.2,
                    input_payload=payload,
                    daemon=daemon,
                    check=False,
                )
                daemon_survived = daemon.is_alive
            elapsed_ns = time.monotonic_ns() - started_ns
            if result.evidence.exit_code not in (0, None):
                fail(
                    "diagnostic LSP bridge failed with exit "
                    f"{result.evidence.exit_code}"
                )
            if (
                result.evidence.process_count_after_cleanup != 0
                or daemon.evidence.process_count_after_cleanup != 0
            ):
                fail("diagnostic flood process tree was not reaped")
            frames = _parse_lsp_frames(result.stdout)
            publications = [
                frame
                for frame in frames
                if frame.get("method") == "textDocument/publishDiagnostics"
            ]
            emitted_count = len(publications)
            available = emitted_count > 0
            digest = hashlib.sha256(
                json.dumps(
                    publications,
                    ensure_ascii=False,
                    separators=(",", ":"),
                    sort_keys=True,
                ).encode("utf-8")
            ).hexdigest()
            sample = {
                "schema_version": SCHEMA_VERSION,
                "identity": {
                    "candidate_id": sha256_file(binary)[:16],
                    "run_id": f"incident-{uuid.uuid4().hex}",
                    "capture_id": f"capture-{uuid.uuid4().hex}",
                    "crate_id": "tracedecay-lsp",
                    "journey_id": "diagnostic-generation",
                    "workload_id": "diagnostic-dedup-batch-rate",
                    "variant": "candidate",
                    "machine_fingerprint": _machine_fingerprint(),
                    "platform": str(prepared.runtime_identity["platform"]),
                    "shard": str(prepared.runtime_identity["shard"]),
                    "storage_mode": str(prepared.runtime_identity["storage_mode"]),
                    "state": "contention",
                    "temperature": "warm",
                    "surface": "host",
                    "concurrency": 1,
                    "round_index": index,
                    "abba_position": 0,
                },
                "evidence": {
                    "sample_count": 1,
                    "evidence_class": "regression_sample",
                },
                "availability": {
                    "state": "available" if available else "unavailable",
                    "detail": None if available else "no diagnostic publication observed",
                },
                "timing": {
                    "started_ns": started_ns,
                    "elapsed_ns": elapsed_ns,
                    "cli_wall_ns": None,
                    "mcp_wall_ns": None,
                    "hook_wall_ns": None,
                    "host_wall_ns": elapsed_ns,
                    "handler_us": None,
                    "daemon_us": None,
                    "admission_us": None,
                    "stages_us": {"diagnostic_flood": elapsed_ns // 1_000},
                    "shutdown_total_ns": None,
                    "abort_offset_ns": None,
                },
                "size": {
                    "process_count": (
                        result.evidence.process_count
                        + daemon.evidence.process_count
                    ),
                    "request_bytes": len(payload),
                    "response_bytes": len(result.stdout),
                    "content_bytes": len(result.stdout),
                },
                "lifecycle": {
                    "timeout_phase": result.evidence.timeout_phase,
                    "activation_state": "active",
                    "restart_state": "not_required",
                    "daemon_survived": daemon_survived,
                },
                "observations": {
                    "diagnostic_generated_count": args.events,
                    "diagnostic_deduplicated_count": max(
                        0, args.events - emitted_count
                    ),
                    "diagnostic_batch_count": emitted_count,
                    "queue_depth": emitted_count,
                    "process_tree_reaped": True,
                },
                "outcome": {
                    "status": "success" if available else "error",
                    "expected_digest": digest,
                    "actual_digest": digest,
                    "result_digest": digest,
                    "error": None if available else "diagnostic route unavailable",
                },
            }
            validate_sample(sample)
            captured.append(sample)

    write_jsonl(samples_path, captured)
    policy = load_acceptance_policy(
        Path(__file__).resolve().with_name("policies") / "acceptance-v1.json"
    )
    journey_policy = load_journey_policy(
        Path(__file__).resolve().with_name("policies")
        / "journey-margins-v1.json"
    )
    emitted_counts = [
        sample["observations"]["diagnostic_batch_count"] for sample in captured
    ]
    report = {
        "schema_version": SCHEMA_VERSION,
        "report_id": f"incident-{uuid.uuid4().hex}",
        "workload_id": "diagnostic-dedup-batch-rate",
        "availability": {
            "state": (
                "available"
                if all(count > 0 for count in emitted_counts)
                else "unavailable"
            ),
            "detail": (
                None
                if all(count > 0 for count in emitted_counts)
                else "one or more samples emitted no diagnostic publication"
            ),
        },
        "sample_count": len(captured),
        "attempted_events_per_sample": args.events,
        "samples_sha256": sha256_file(samples_path),
        "event_counts": {
            "emitted_max": max(emitted_counts),
            "emitted_total": sum(emitted_counts),
            "queue_depth_max": max(emitted_counts),
        },
        "outcome": {
            "available_count": sum(count > 0 for count in emitted_counts),
            "unavailable_count": sum(count == 0 for count in emitted_counts),
            "process_leak_count": 0,
        },
        "policy": {
            "policy_id": policy.policy_id,
            "policy_sha256": policy.sha256,
            "journey_policy_id": journey_policy.policy_id,
            "journey_policy_sha256": journey_policy.sha256,
            "journey_margins": journey_policy.journeys["host"],
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


def capture_incident(args: argparse.Namespace) -> int:
    if args.authority_test and args.binary is not None:
        fail("--binary is not used with --authority-test")
    if not args.authority_test and args.binary is None:
        fail("--binary is required for production incident capture")
    if args.workload == "diagnostic-dedup-batch-rate":
        return _capture_diagnostic_flood(args)
    if args.workload != "missing-daemon-after-shell":
        fail(f"incident workload is not wired: {args.workload}")
    if isinstance(args.samples, bool) or args.samples < 1:
        fail("incident samples must be a positive integer")
    binary = require_binary(args.binary)
    output = Path(args.output)
    samples_path, policy_path = _artifact_paths(output)
    for path in (output, samples_path, policy_path):
        if path.exists():
            fail(f"incident output already exists: {path}")

    captured: list[dict[str, Any]] = []
    output.parent.mkdir(parents=True, exist_ok=True)
    with RunWorkspace(
        Path(tempfile.gettempdir()),
        preserve_on_failure=False,
    ) as workspace:
        base = prepare_fixture_snapshot(
            workspace.path / "base",
            prebuilt_binary=binary,
        )
        for index in range(args.samples):
            prepared = clone_prepared_profile(
                base,
                workspace.path / f"sample-{index}",
                prebuilt_binary=binary,
                runtime_state="recovery",
                temperature="warm",
            )
            initialized = subprocess.run(
                (
                    os.fspath(prepared.prebuilt_binary),
                    "init",
                    os.fspath(prepared.project),
                ),
                cwd=prepared.project,
                env=prepared.environment,
                capture_output=True,
                check=False,
                timeout=30.0,
            )
            if initialized.returncode != 0:
                fail(
                    "incident fixture enrollment failed: "
                    + initialized.stderr.decode("utf-8", errors="replace")
                )
            missing_socket = prepared.snapshot_root / "run" / "missing.sock"
            environment = {
                **prepared.environment,
                "TRACEDECAY_DAEMON_SOCKET": os.fspath(missing_socket),
            }
            event = json.dumps(
                {
                    "hook_event_name": "afterShellExecution",
                    "command": "git status",
                    "cwd": os.fspath(prepared.project),
                    "workspace_roots": [os.fspath(prepared.project)],
                },
                ensure_ascii=False,
                separators=(",", ":"),
                sort_keys=True,
            ).encode("utf-8")
            startup_started_ns = time.monotonic_ns()
            startup_control = subprocess.run(
                (os.fspath(prepared.prebuilt_binary), "--version"),
                cwd=prepared.project,
                env=environment,
                capture_output=True,
                check=False,
                timeout=5.0,
            )
            startup_control_ns = time.monotonic_ns() - startup_started_ns
            if startup_control.returncode != 0:
                fail(
                    "process startup control failed with exit "
                    f"{startup_control.returncode}"
                )
            direct_started_ns = time.monotonic_ns()
            direct_hook = subprocess.run(
                (
                    os.fspath(prepared.prebuilt_binary),
                    "hook-cursor-after-shell",
                ),
                cwd=prepared.project,
                env=environment,
                input=event,
                capture_output=True,
                check=False,
                timeout=5.0,
            )
            direct_hook_wall_ns = time.monotonic_ns() - direct_started_ns
            if direct_hook.returncode != 0:
                fail(
                    "direct missing-daemon hook failed with exit "
                    f"{direct_hook.returncode}"
                )
            started_ns = time.monotonic_ns()
            result = run_host(
                (
                    os.fspath(prepared.prebuilt_binary),
                    "hook-cursor-after-shell",
                ),
                env=environment,
                log_dir=prepared.snapshot_root / "host-logs",
                timeout=5.0,
                termination_grace=0.2,
                input_payload=event,
                check=False,
            )
            elapsed_ns = time.monotonic_ns() - started_ns
            hook_residual_ns = max(
                0,
                direct_hook_wall_ns - startup_control_ns,
            )
            lifecycle_wrapper_overhead_ns = max(
                0,
                elapsed_ns - direct_hook_wall_ns,
            )
            if result.evidence.exit_code != 0:
                fail(
                    "missing-daemon after-shell command failed with exit "
                    f"{result.evidence.exit_code}"
                )
            if result.evidence.process_count_after_cleanup != 0:
                fail("missing-daemon after-shell process tree was not reaped")
            digest = hashlib.sha256(result.stdout).hexdigest()
            sample = {
                "schema_version": SCHEMA_VERSION,
                "identity": {
                    "candidate_id": sha256_file(binary)[:16],
                    "run_id": f"incident-{uuid.uuid4().hex}",
                    "capture_id": f"capture-{uuid.uuid4().hex}",
                    "crate_id": "tracedecay-hooks",
                    "journey_id": "daemon-failure-recovery",
                    "workload_id": "missing-daemon-after-shell",
                    "variant": "candidate",
                    "machine_fingerprint": _machine_fingerprint(),
                    "platform": str(prepared.runtime_identity["platform"]),
                    "shard": str(prepared.runtime_identity["shard"]),
                    "storage_mode": str(prepared.runtime_identity["storage_mode"]),
                    "state": "recovery",
                    "temperature": "warm",
                    "surface": "host",
                    "concurrency": 1,
                    "round_index": index,
                    "abba_position": 0,
                },
                "evidence": {
                    "sample_count": 1,
                    "evidence_class": "regression_sample",
                },
                "availability": {
                    "state": "unavailable",
                    "detail": "daemon socket is intentionally absent",
                },
                "timing": {
                    "started_ns": started_ns,
                    "elapsed_ns": elapsed_ns,
                    "cli_wall_ns": None,
                    "mcp_wall_ns": None,
                    "hook_wall_ns": None,
                    "host_wall_ns": elapsed_ns,
                    "handler_us": None,
                    "daemon_us": None,
                    "admission_us": None,
                    "stages_us": {
                        "process_startup_control": startup_control_ns // 1_000,
                        "direct_hook_wall": direct_hook_wall_ns // 1_000,
                        "missing_daemon_hook_residual": hook_residual_ns // 1_000,
                        "lifecycle_wrapper_overhead": (
                            lifecycle_wrapper_overhead_ns // 1_000
                        ),
                        "missing_daemon_fail_fast": elapsed_ns // 1_000,
                    },
                    "shutdown_total_ns": None,
                    "abort_offset_ns": None,
                },
                "size": {
                    "process_count": result.evidence.process_count,
                    "request_bytes": len(event),
                    "response_bytes": len(result.stdout),
                    "content_bytes": len(result.stdout),
                },
                "lifecycle": {
                    "timeout_phase": None,
                    "activation_state": "inactive",
                    "restart_state": "not_required",
                    "daemon_survived": None,
                },
                "observations": {
                    "missing_daemon_fail_fast_ns": elapsed_ns,
                    "process_startup_control_ns": startup_control_ns,
                    "direct_hook_wall_ns": direct_hook_wall_ns,
                    "hook_residual_ns": hook_residual_ns,
                    "lifecycle_wrapper_overhead_ns": (
                        lifecycle_wrapper_overhead_ns
                    ),
                    "process_tree_reaped": True,
                },
                "outcome": {
                    "status": "error",
                    "expected_digest": digest,
                    "actual_digest": digest,
                    "result_digest": digest,
                    "error": "expected daemon unavailable",
                },
            }
            validate_sample(sample)
            captured.append(sample)

    write_jsonl(samples_path, captured)
    elapsed_values = [sample["timing"]["elapsed_ns"] for sample in captured]

    def incident_distribution(field: str) -> dict[str, Any]:
        values = [sample["observations"][field] for sample in captured]
        return {
            "p50": {
                "available": len(values) >= 2,
                "value": nearest_rank(values, 0.50) if len(values) >= 2 else None,
                "minimum_samples": 2,
            },
            "p95": {
                "available": len(values) >= 40,
                "value": nearest_rank(values, 0.95) if len(values) >= 40 else None,
                "minimum_samples": 40,
            },
            "p99": {
                "available": len(values) >= 100,
                "value": nearest_rank(values, 0.99) if len(values) >= 100 else None,
                "minimum_samples": 100,
            },
        }

    policy = load_acceptance_policy(
        Path(__file__).resolve().with_name("policies") / "acceptance-v1.json"
    )
    journey_policy = load_journey_policy(
        Path(__file__).resolve().with_name("policies")
        / "journey-margins-v1.json"
    )
    report = {
        "schema_version": SCHEMA_VERSION,
        "report_id": f"incident-{uuid.uuid4().hex}",
        "workload_id": args.workload,
        "availability": {
            "state": "unavailable",
            "detail": "daemon socket intentionally absent",
        },
        "sample_count": len(captured),
        "samples_sha256": sha256_file(samples_path),
        "latency_ns": {
            "p50": {
                "available": len(elapsed_values) >= 2,
                "value": (
                    nearest_rank(elapsed_values, 0.50)
                    if len(elapsed_values) >= 2
                    else None
                ),
                "minimum_samples": 2,
            },
            "p95": {
                "available": len(elapsed_values) >= 40,
                "value": (
                    nearest_rank(elapsed_values, 0.95)
                    if len(elapsed_values) >= 40
                    else None
                ),
                "minimum_samples": 40,
            },
            "p99": {
                "available": len(elapsed_values) >= 100,
                "value": (
                    nearest_rank(elapsed_values, 0.99)
                    if len(elapsed_values) >= 100
                    else None
                ),
                "minimum_samples": 100,
            },
        },
        "stages_ns": {
            "process_startup_control": incident_distribution(
                "process_startup_control_ns"
            ),
            "direct_hook_wall": incident_distribution("direct_hook_wall_ns"),
            "hook_residual": incident_distribution("hook_residual_ns"),
            "lifecycle_wrapper_overhead": incident_distribution(
                "lifecycle_wrapper_overhead_ns"
            ),
        },
        "outcome": {
            "expected_unavailable_count": len(captured),
            "command_failure_count": 0,
            "process_leak_count": 0,
        },
        "policy": {
            "policy_id": policy.policy_id,
            "policy_sha256": policy.sha256,
            "journey_policy_id": journey_policy.policy_id,
            "journey_policy_sha256": journey_policy.sha256,
            "journey_margins": journey_policy.journeys["host"],
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
    report_id = f"paired-{uuid.uuid4().hex}"
    first_identity = samples[0]["identity"]
    rounds = [
        [
            (
                "A" if sample["identity"]["variant"] == "baseline" else "B",
                sample["timing"]["elapsed_ns"],
            )
            for sample in sorted(
                (
                    item
                    for item in samples
                    if item["identity"]["round_index"] == cycle
                ),
                key=lambda item: item["identity"]["abba_position"],
            )
        ]
        for cycle in range(samples_per_variant // 2)
    ]
    comparison = compare_abba(
        rounds,
        identity={
            "baseline_candidate_id": sha256_file(baseline)[:16],
            "treatment_candidate_id": sha256_file(treatment)[:16],
            "run_id": report_id,
            "capture_id": report_id,
            "crate_id": first_identity["crate_id"],
            "journey_id": first_identity["journey_id"],
            "workload_id": first_identity["workload_id"],
            "platform": first_identity["platform"],
            "shard": first_identity["shard"],
            "storage_mode": first_identity["storage_mode"],
            "state": first_identity["state"],
            "temperature": first_identity["temperature"],
            "surface": first_identity["surface"],
            "concurrency": first_identity["concurrency"],
        },
        baseline_machine_fingerprint=first_identity["machine_fingerprint"],
        treatment_machine_fingerprint=first_identity["machine_fingerprint"],
        baseline_samples=[
            sample
            for sample in samples
            if sample["identity"]["variant"] == "baseline"
        ],
        treatment_samples=[
            sample
            for sample in samples
            if sample["identity"]["variant"] == "treatment"
        ],
        relative_threshold=(
            journey_policy.journeys["cli"]["p50_regression_margin_pct"] / 100
        ),
        seed=0,
    )
    report = {
        "schema_version": SCHEMA_VERSION,
        "report_id": report_id,
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
        "comparison": comparison,
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

    incident_parser = subparsers.add_parser(
        "incident",
        help="capture one wired final incident workload",
    )
    incident_parser.add_argument(
        "--binary",
        help="explicit product binary for production-route incident capture",
    )
    incident_parser.add_argument("--workload", required=True)
    incident_parser.add_argument("--samples", type=int, default=10)
    incident_parser.add_argument("--events", type=int, default=1_000)
    incident_parser.add_argument("--authority-test", action="store_true")
    incident_parser.add_argument("--output", required=True)
    incident_parser.set_defaults(handler=capture_incident)

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
    register_graph_measurement_subcommands(
        subparsers,
        capture_name="graph-capture",
        paired_name="graph-paired",
    )
    return argument_parser


def main(argv: Sequence[str] | None = None) -> int:
    arguments = parser().parse_args(argv)
    try:
        return int(arguments.handler(arguments))
    except (HarnessError, GraphMeasurementError, OSError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
