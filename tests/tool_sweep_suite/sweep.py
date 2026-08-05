"""Live catalog-complete MCP execution sweep.

This module deliberately has no production tool inventory. The only inventory
is the negotiated ``tools/list`` response from the release binary under test.
"""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import asdict, dataclass, field
from datetime import UTC, datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import threading
import time
from typing import Any

from fixture import fixture_state, prime_fixture_ledger
from journeys import JourneyCall
from reports import PASSING_VERDICTS, write_reports


INITIALIZE_DEADLINE_MS = 15_000
HEALTH_DEADLINE_MS = 5_000
WARM_SAMPLES = 5
READ_ONLY_EFFECTS = frozenset({"read", "preview"})


@dataclass(frozen=True)
class SweepRuntime:
    client_type: type[Any]
    policy_decoder: Any
    argument_materializer: Any
    completion_check: Any
    timing_summary: Any
    typed_unavailable: Any
    typed_denial: Any
    typed_deadline: Any
    tool_error: Any
    problem_code: Any
    sweep_error: type[Exception]
    fixture_workspace: Any
    catalog_writer: Any
    catalog_loader: Any
    effect_journey: Any


@dataclass
class ToolRow:
    name: str
    effect: str | None = None
    availability: str | None = None
    deadline_ms: int | None = None
    samples_ms: list[int] = field(default_factory=list)
    p95_ms: int | None = None
    max_ms: int | None = None
    verdict: str = "FAIL"
    note: str = "not run"
    request_ids: list[int] = field(default_factory=list)
    cancellation: list[dict[str, Any]] = field(default_factory=list)
    same_client_healthy: bool | None = None
    fresh_client_healthy: bool | None = None
    rollback: str | None = None
    argument_keys: list[str] = field(default_factory=list)
    load_mode: str = "normal"
    timing_stages: list[dict[str, Any]] = field(default_factory=list)
    journey_calls: list[dict[str, Any]] = field(default_factory=list)

    def value(self) -> dict[str, Any]:
        return asdict(self)


class StageLog:
    def __init__(self) -> None:
        self._started = time.monotonic()
        self.events: list[dict[str, Any]] = []

    def run(self, name: str, callback: Any) -> Any:
        started = time.monotonic()
        event: dict[str, Any] = {"name": name, "started_ms": int((started - self._started) * 1000)}
        try:
            result = callback()
        except Exception as error:
            event.update({"elapsed_ms": int((time.monotonic() - started) * 1000), "outcome": "error", "detail": str(error)[:800]})
            self.events.append(event)
            raise
        event.update({"elapsed_ms": int((time.monotonic() - started) * 1000), "outcome": "ok"})
        self.events.append(event)
        return result


class ClientFactory:
    def __init__(self, runtime: SweepRuntime, binary: Path, project: Path, out: Path) -> None:
        self._runtime = runtime
        self._binary = binary
        self._project = project
        self._logs = out / "clients"
        self._logs.mkdir(parents=True, exist_ok=True)
        self._lock = threading.Lock()
        self._next = 0

    def open(self, label: str) -> Any:
        safe_label = "".join(character if character.isalnum() or character in "-_" else "-" for character in label)
        with self._lock:
            self._next += 1
            log_path = self._logs / f"{self._next:03d}-{safe_label}.log"
        client = self._runtime.client_type(self._binary, self._project, log_path)
        try:
            client.initialize(INITIALIZE_DEADLINE_MS)
        except Exception:
            client.close()
            raise
        return client


def _utc_now() -> str:
    return datetime.now(UTC).isoformat().replace("+00:00", "Z")


def _partition(definitions: list[dict[str, Any]], shards: int) -> list[list[dict[str, Any]]]:
    """Stable tool-name sharding; each negotiated read has exactly one owner."""
    result = [[] for _ in range(shards)]
    for definition in definitions:
        name = definition["name"]
        shard = int.from_bytes(hashlib.sha256(name.encode()).digest()[:8], "big") % shards
        result[shard].append(definition)
    return result


def _exception_row(definition: dict[str, Any], policy: Any | None, detail: str) -> ToolRow:
    return ToolRow(
        name=str(definition.get("name", "unnamed-tool")),
        effect=None if policy is None else policy.effect,
        availability=None if policy is None else policy.availability_state,
        deadline_ms=None if policy is None else policy.deadline_ms,
        verdict="FAIL",
        note=detail[:800],
    )


def _record_attempt(row: ToolRow, attempt: Any) -> None:
    row.request_ids.append(attempt.request_id)
    row.samples_ms.append(attempt.elapsed_ms)
    row.timing_stages.append(
        {
            "request_id": attempt.request_id,
            "client_queue_ms": getattr(attempt, "client_queue_ms", 0),
            "round_trip_ms": attempt.elapsed_ms,
        }
    )
    if attempt.cancellation_sent or attempt.timed_out or attempt.transport_error is not None:
        row.cancellation.append(
            {
                "request_id": attempt.request_id,
                "timed_out": attempt.timed_out,
                "sent": attempt.cancellation_sent,
                "settled": attempt.cancellation_settled,
                "transport_error": attempt.transport_error,
            }
        )


def _record_journey_calls(row: ToolRow, calls: list[JourneyCall]) -> None:
    for call in calls:
        attempt = call.attempt
        row.journey_calls.append(
            {
                "role": call.role,
                "tool": call.tool,
                "argument_keys": sorted(call.arguments),
                "request_id": getattr(attempt, "request_id", None),
                "elapsed_ms": getattr(attempt, "elapsed_ms", None),
                "timed_out": getattr(attempt, "timed_out", None),
                "transport_error": getattr(attempt, "transport_error", None),
            }
        )


def _response_has_effect_receipt(response: dict[str, Any]) -> bool:
    receipt_keys = {
        "receipt",
        "receipt_id",
        "effect_id",
        "operation_id",
        "preview_id",
        "revision",
        "revision_id",
        "session_id",
        "transaction_id",
    }
    stack: list[Any] = [response]
    while stack:
        value = stack.pop()
        if isinstance(value, dict):
            if any(isinstance(value.get(key), str) and value[key] for key in receipt_keys):
                return True
            stack.extend(value.values())
        elif isinstance(value, list):
            stack.extend(value)
        elif isinstance(value, str):
            try:
                decoded = json.loads(value)
            except json.JSONDecodeError:
                continue
            stack.append(decoded)
    return False


def _post_timeout_health(client: Any, factory: ClientFactory) -> tuple[bool, bool]:
    same_healthy = client.ping(HEALTH_DEADLINE_MS)
    fresh = None
    try:
        fresh = factory.open("post-timeout-health")
        fresh.list_tools(INITIALIZE_DEADLINE_MS)
        fresh_healthy = fresh.ping(HEALTH_DEADLINE_MS)
    except Exception:
        fresh_healthy = False
    finally:
        if fresh is not None:
            fresh.close()
    return same_healthy, fresh_healthy


def _set_timing(row: ToolRow, runtime: SweepRuntime) -> None:
    if row.samples_ms:
        row.p95_ms, row.max_ms = runtime.timing_summary(row.samples_ms)


def _timeout_verdict(row: ToolRow, attempt: Any, runtime: SweepRuntime, client: Any, factory: ClientFactory, detail: str) -> None:
    row.same_client_healthy, row.fresh_client_healthy = _post_timeout_health(client, factory)
    if not attempt.cancellation_settled or not row.same_client_healthy or not row.fresh_client_healthy:
        row.verdict = "WORKER_LEAK"
        row.note = "deadline cancellation or post-timeout health failed"
        return
    if attempt.response is None or not runtime.typed_deadline(attempt.response):
        row.verdict = "FAIL"
        row.note = "deadline cancellation settled without typed deadline state"
        return
    row.verdict = "TIMEOUT"
    row.note = detail


def _finish_available_read(row: ToolRow, policy: Any, runtime: SweepRuntime, response: dict[str, Any]) -> bool:
    problem_code = runtime.problem_code(response)
    suffix = f": {problem_code}" if isinstance(problem_code, str) and problem_code else ""
    if runtime.tool_error(response):
        if runtime.typed_unavailable(response):
            row.note = f"available tool returned typed unavailable{suffix}"
        else:
            row.note = f"tool returned error{suffix}"
        return False
    if runtime.typed_unavailable(response):
        row.note = f"available tool returned typed unavailable{suffix}"
        return False
    if runtime.typed_denial(response):
        row.note = "read tool returned typed denial"
        return False
    return True


def _metadata_matches_annotations(definition: dict[str, Any], policy: Any) -> str | None:
    annotations = definition.get("annotations")
    if not isinstance(annotations, dict):
        return "tool annotations missing"
    read_only = annotations.get("readOnlyHint")
    if not isinstance(read_only, bool):
        return "tool readOnlyHint missing"
    expected_read_only = policy.effect in READ_ONLY_EFFECTS
    if read_only != expected_read_only:
        return (
            f"readOnlyHint={str(read_only).lower()} conflicts with dispatch "
            f"effect={policy.effect}; expected readOnlyHint={str(expected_read_only).lower()}"
        )
    return None


def _select_phase_effect(
    runnable: list[tuple[dict[str, Any], Any]], target: str
) -> tuple[dict[str, Any], Any]:
    """Choose one advertised available mutation from the negotiated catalog."""
    matches = [(definition, policy) for definition, policy in runnable if definition["name"] == target]
    if len(matches) != 1:
        raise RuntimeError(f"requested effect is absent or duplicated in catalog: {target}")
    definition, policy = matches[0]
    if policy.availability_state != "available" or policy.effect in READ_ONLY_EFFECTS:
        raise RuntimeError(f"requested effect is not an available mutating tool: {target}")
    return definition, policy


def _deadline_for(policies: dict[str, Any], tool: str) -> int:
    policy = policies.get(tool)
    deadline_ms = None if policy is None else policy.deadline_ms
    if policy is None or policy.availability_state != "available" or not isinstance(deadline_ms, int):
        raise RuntimeError(f"fixture producer {tool} lacks an available negotiated dispatch deadline")
    return deadline_ms


def _invoke_unavailable(
    definition: dict[str, Any], policy: Any, client: Any, runtime: SweepRuntime, factory: ClientFactory
) -> ToolRow:
    """Execute the advertised unavailable path before reporting its typed state."""
    row = _exception_row(definition, policy, "unavailable tool not invoked")
    row.rollback = "not_required"
    try:
        attempt = client.call_tool(row.name, {}, policy.deadline_ms)
    except Exception as error:
        row.verdict = "DAEMON_DOWN"
        row.note = f"transport failure: {error}"
        return row
    _record_attempt(row, attempt)
    _set_timing(row, runtime)
    if attempt.transport_error is not None:
        row.verdict = "DAEMON_DOWN"
        row.note = attempt.transport_error
        return row
    if attempt.timed_out:
        _timeout_verdict(
            row,
            attempt,
            runtime,
            client,
            factory,
            "unavailable dispatch exceeded its canonical deadline",
        )
        return row
    if attempt.response is None:
        row.note = "unavailable dispatch produced no response"
        return row
    if not runtime.tool_error(attempt.response) or not runtime.typed_unavailable(attempt.response):
        row.note = "advertised unavailable tool did not return a typed unavailable state"
        return row
    if row.max_ms is not None and row.max_ms > policy.deadline_ms:
        row.verdict = "SLOW"
        row.note = f"typed unavailable response exceeded canonical deadline {policy.deadline_ms}ms"
        return row
    row.verdict = "PASS"
    row.note = f"typed unavailable state confirmed: {policy.availability_reason}"
    return row


def _invoke_read(
    definition: dict[str, Any],
    policy: Any,
    client: Any,
    runtime: SweepRuntime,
    fixture: Any,
    factory: ClientFactory,
    load_mode: str = "normal",
) -> ToolRow:
    row = _exception_row(definition, policy, "read not invoked")
    row.load_mode = load_mode
    try:
        arguments = runtime.argument_materializer(definition, fixture, effect=policy.effect)
        row.argument_keys = sorted(arguments)
    except Exception as error:
        row.note = f"argument materialization failed: {error}"
        return row
    for _ in range(WARM_SAMPLES):
        try:
            attempt = client.call_tool(row.name, arguments, policy.deadline_ms)
        except Exception as error:
            row.verdict = "DAEMON_DOWN"
            row.note = f"transport failure: {error}"
            return row
        _record_attempt(row, attempt)
        if attempt.transport_error is not None:
            row.verdict = "DAEMON_DOWN"
            row.note = attempt.transport_error
            return row
        if attempt.timed_out:
            _timeout_verdict(row, attempt, runtime, client, factory, "deadline exceeded; cancellation and health recorded")
            return row
        if attempt.response is None or not _finish_available_read(row, policy, runtime, attempt.response):
            row.verdict = "FAIL"
            return row
    _set_timing(row, runtime)
    if row.max_ms is not None and row.max_ms > policy.deadline_ms:
        row.verdict = "SLOW"
        row.note = f"max {row.max_ms}ms exceeds canonical deadline {policy.deadline_ms}ms"
    elif row.p95_ms is not None and row.p95_ms > policy.deadline_ms:
        row.verdict = "SLOW"
        row.note = f"p95 {row.p95_ms}ms exceeds canonical deadline {policy.deadline_ms}ms"
    else:
        row.verdict = "PASS"
        row.note = "five warm samples within canonical deadline"
    return row


def _invoke_effect(
    definition: dict[str, Any], policy: Any, client: Any, runtime: SweepRuntime, fixture: Any, factory: ClientFactory,
    deadline_for: Any,
) -> ToolRow:
    row = _exception_row(definition, policy, "effect not invoked")
    prepared = None
    attempt = None
    try:
        prepared = runtime.effect_journey(definition, policy, client, runtime, fixture, deadline_for)
        arguments = prepared.arguments
        row.argument_keys = sorted(arguments)
    except Exception as error:
        calls = getattr(error, "calls", None)
        if isinstance(calls, list):
            _record_journey_calls(row, calls)
        row.note = f"real journey preparation failed: {error}"
        row.rollback = "not_started"
        return row
    before = fixture_state(fixture.root)
    try:
        try:
            attempt = client.call_tool(row.name, arguments, policy.deadline_ms)
        except Exception as error:
            row.verdict = "DAEMON_DOWN"
            row.note = f"transport failure: {error}"
            return row
        _record_attempt(row, attempt)
        _set_timing(row, runtime)
        if attempt.transport_error is not None:
            row.verdict = "DAEMON_DOWN"
            row.note = attempt.transport_error
            return row
        if attempt.timed_out:
            _timeout_verdict(row, attempt, runtime, client, factory, "effect deadline exceeded; cancellation and health recorded")
            return row
        if attempt.response is None:
            row.note = "effect produced no response"
            return row
        after = fixture_state(fixture.root)
        if runtime.typed_denial(attempt.response):
            row.note = "advertised available effect returned typed denial"
            return row
        if runtime.tool_error(attempt.response) or runtime.typed_unavailable(attempt.response):
            problem_code = runtime.problem_code(attempt.response)
            suffix = f": {problem_code}" if isinstance(problem_code, str) and problem_code else ""
            row.note = f"available effect returned error or unavailable state{suffix}"
            return row
        journey_problem = prepared.verify_success(attempt.response)
        if journey_problem is not None:
            row.note = journey_problem
            return row
        if after == before and not _response_has_effect_receipt(attempt.response) and not prepared.allow_no_repository_change:
            row.note = "effect returned no repository change or typed receipt"
            return row
        if row.max_ms is not None and row.max_ms > policy.deadline_ms:
            row.verdict = "SLOW"
            row.note = f"max {row.max_ms}ms exceeds canonical deadline {policy.deadline_ms}ms"
            return row
        row.verdict = "PASS"
        row.note = "effect observed through a real isolated producer/consumer journey"
        return row
    finally:
        if prepared is not None:
            try:
                cleanup = prepared.cleanup(None if attempt is None else attempt.response)
                if not isinstance(cleanup, str) or not cleanup:
                    raise RuntimeError("journey cleanup returned no verification")
            except Exception as error:
                cleanup_note = f"real journey rollback failed: {error}"
                if row.verdict != "PASS":
                    cleanup_note = f"{row.note}; {cleanup_note}"
                row.verdict = "FAIL"
                row.note = cleanup_note
                row.rollback = "failed"
            else:
                row.rollback = cleanup
            _record_journey_calls(row, prepared.calls)


def _run_read_shard(
    shard: list[tuple[dict[str, Any], Any]], runtime: SweepRuntime, factory: ClientFactory, fixture: Any, label: str
) -> list[ToolRow]:
    client = None
    try:
        client = factory.open(label)
        return [_invoke_read(definition, policy, client, runtime, fixture, factory) for definition, policy in shard]
    except Exception as error:
        return [_exception_row(definition, policy, f"read shard unavailable: {error}") for definition, policy in shard]
    finally:
        if client is not None:
            client.close()


def _run(
    args: argparse.Namespace,
    runtime: SweepRuntime,
    stage_log: StageLog,
    report: dict[str, Any],
    rows: list[ToolRow],
) -> None:
    fixture = stage_log.run("fixture", lambda: runtime.fixture_workspace.create(args.bin, args.out))
    factory = ClientFactory(runtime, args.bin, fixture.root, args.out)
    primary = stage_log.run("connect", lambda: factory.open("primary"))
    try:
        definitions = stage_log.run("tools/list", lambda: primary.list_tools(INITIALIZE_DEADLINE_MS))
        names = [definition.get("name") for definition in definitions]
        if any(not isinstance(name, str) or not name for name in names):
            raise runtime.sweep_error("tools/list exposed an invalid tool name")
        if len(set(names)) != len(names):
            raise runtime.sweep_error("tools/list exposed duplicate tool names")
        manifest = stage_log.run(
            "catalog-manifest", lambda: runtime.catalog_writer(args.out / "catalog.json", definitions)
        )
        report["discovered_tools"] = manifest["tool_names"]
        report["catalog"] = {
            "fingerprint": manifest["fingerprint"],
            "tool_names": manifest["tool_names"],
        }
        if args.phase == "effect":
            expected_manifest = runtime.catalog_loader(args.catalog)
            if manifest != expected_manifest:
                raise runtime.sweep_error("effect phase catalog drifted from the shared read catalog")
        policies: dict[str, Any] = {}
        runnable: list[tuple[dict[str, Any], Any]] = []
        invalid_names: set[str] = set()
        for definition in definitions:
            try:
                policy = runtime.policy_decoder(definition)
            except Exception as error:
                invalid_names.add(definition["name"])
                if args.phase == "reads":
                    rows.append(_exception_row(definition, None, f"dispatch metadata invalid: {error}"))
            else:
                mismatch = _metadata_matches_annotations(definition, policy)
                if mismatch is not None:
                    invalid_names.add(definition["name"])
                    if args.phase == "reads":
                        rows.append(_exception_row(definition, policy, mismatch))
                    continue
                policies[definition["name"]] = policy
                runnable.append((definition, policy))

        deadline_for = lambda tool: _deadline_for(policies, tool)
        ledger = stage_log.run(
            "fixture-producers", lambda: prime_fixture_ledger(primary, fixture, deadline_for)
        )
        if args.phase == "effect":
            definition, policy = _select_phase_effect(runnable, args.effect)
            rows.append(_invoke_effect(definition, policy, primary, runtime, ledger, factory, deadline_for))
            expected_names = {definition["name"]}
        else:
            unavailable = [
                (definition, policy)
                for definition, policy in runnable
                if policy.availability_state == "unavailable"
            ]
            reads = [
                (definition, policy)
                for definition, policy in runnable
                if policy.availability_state == "available" and policy.effect in READ_ONLY_EFFECTS
            ]
            for definition, policy in unavailable:
                rows.append(_invoke_unavailable(definition, policy, primary, runtime, factory))
            partitions = _partition([definition for definition, _ in reads], args.shards)
            policy_by_name = {definition["name"]: policy for definition, policy in reads}
            with ThreadPoolExecutor(max_workers=args.shards, thread_name_prefix="mcp-tool-sweep") as executor:
                futures = [
                    executor.submit(
                        _run_read_shard,
                        [(definition, policy_by_name[definition["name"]]) for definition in partition],
                        runtime,
                        factory,
                        ledger,
                        f"read-shard-{index}",
                    )
                    for index, partition in enumerate(partitions)
                    if partition
                ]
                for future in as_completed(futures):
                    rows.extend(future.result())
            expected_names = invalid_names | {definition["name"] for definition, _ in unavailable} | {
                definition["name"] for definition, _ in reads
            }
        completed_names = [row.name for row in rows]
        if len(completed_names) != len(set(completed_names)):
            raise runtime.sweep_error("catalog completion produced duplicate tool rows")
        runtime.completion_check(expected_names, set(completed_names))
    finally:
        primary.close()


def _summary(rows: list[ToolRow], discovered: list[str]) -> dict[str, Any]:
    counts: dict[str, int] = {}
    for row in rows:
        counts[row.verdict] = counts.get(row.verdict, 0) + 1
    return {
        "discovered": len(discovered),
        "completed": len(rows),
        "verdicts": dict(sorted(counts.items())),
        "failed": sum(1 for row in rows if row.verdict not in PASSING_VERDICTS),
    }


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Run every negotiated production MCP tool against a disposable fixture.")
    parser.add_argument("--bin", type=Path, required=True, help="release tracedecay binary")
    parser.add_argument("--out", type=Path, required=True, help="artifact directory")
    parser.add_argument("--shards", type=int, default=4, help="parallel read shards (default: 4)")
    parser.add_argument(
        "--phase",
        choices=("reads", "effect"),
        default="reads",
        help="run shared read coverage or one isolated mutating effect",
    )
    parser.add_argument("--effect", help="negotiated mutating tool name for --phase effect")
    parser.add_argument("--catalog", type=Path, help="exact catalog manifest produced by the reads phase")
    args = parser.parse_args(argv)
    args.bin = args.bin.resolve()
    args.out = args.out.resolve()
    if args.shards < 1:
        parser.error("--shards must be positive")
    if not args.bin.is_file() or not args.bin.stat().st_mode & 0o111:
        parser.error(f"--bin is not executable: {args.bin}")
    if args.phase == "reads" and (args.effect is not None or args.catalog is not None):
        parser.error("--effect and --catalog are only valid with --phase effect")
    if args.phase == "effect":
        if not isinstance(args.effect, str) or not args.effect:
            parser.error("--phase effect requires --effect")
        if args.catalog is None:
            parser.error("--phase effect requires --catalog")
        args.catalog = args.catalog.resolve()
        if not args.catalog.is_file():
            parser.error(f"catalog manifest does not exist: {args.catalog}")
    return args


def require_isolated_runtime(args: argparse.Namespace) -> None:
    """Refuse a live sweep unless every TraceDecay and host path is disposable."""
    if os.environ.get("TRACEDECAY_DAEMON_HARNESS_ACTIVE") != "1":
        raise RuntimeError("tool sweep requires the isolated daemon harness")
    artifact_root = args.out.resolve()
    raw_harness_root = os.environ.get("TRACEDECAY_DAEMON_HARNESS_ROOT")
    if not raw_harness_root:
        raise RuntimeError("tool sweep requires isolated TRACEDECAY_DAEMON_HARNESS_ROOT")
    harness_root = Path(raw_harness_root).resolve()

    def require_within(variable: str, root: Path, scope: str) -> None:
        raw_path = os.environ.get(variable)
        if not raw_path:
            raise RuntimeError(f"tool sweep requires isolated {variable}")
        path = Path(raw_path).resolve()
        try:
            path.relative_to(root)
        except ValueError as error:
            raise RuntimeError(f"{variable} is outside isolated {scope}: {path}") from error

    for variable in (
        "HOME",
        "CODEX_HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "TMPDIR",
        "TRACEDECAY_PROFILE_DIR",
    ):
        require_within(variable, artifact_root, "artifact root")
    for variable in ("TRACEDECAY_DATA_DIR", "TRACEDECAY_GLOBAL_DB", "TRACEDECAY_DAEMON_SOCKET"):
        require_within(variable, harness_root, "daemon harness root")


def retain_daemon_log(out: Path) -> None:
    """Keep the harness-owned log after it tears down its disposable profile."""
    data_dir = os.environ.get("TRACEDECAY_DATA_DIR")
    if not data_dir:
        return
    source = Path(data_dir).parent / "daemon.log"
    if source.is_file():
        shutil.copyfile(source, out / "daemon.log")


def main(argv: list[str], runtime: SweepRuntime) -> int:
    args = parse_args(argv)
    args.out.mkdir(parents=True, exist_ok=True)
    require_isolated_runtime(args)
    started = _utc_now()
    stage_log = StageLog()
    report: dict[str, Any] = {
        "schema_version": 1,
        "started_at": started,
        "binary": str(args.bin),
        "shards": args.shards,
        "phase": args.phase,
        "effect": args.effect,
        "tools": [],
        "discovered_tools": [],
        "stages": [],
    }
    rows: list[ToolRow] = []
    fatal: str | None = None
    try:
        _run(args, runtime, stage_log, report, rows)
    except Exception as error:
        fatal = str(error)
    finally:
        report["finished_at"] = _utc_now()
        report["stages"] = stage_log.events
        report["tools"] = [row.value() for row in sorted(rows, key=lambda item: item.name)]
        report["summary"] = _summary(rows, report["discovered_tools"])
        if fatal is not None:
            report["fatal"] = fatal
        try:
            retain_daemon_log(args.out)
        except OSError as error:
            report["fatal"] = f"could not retain daemon log: {error}"
            fatal = report["fatal"]
        (args.out / "stages.json").write_text(json.dumps(stage_log.events, indent=2, sort_keys=True) + "\n")
        write_reports(args.out, report)
    if fatal is not None:
        print(f"MCP tool sweep failed before completion: {fatal}")
        return 1
    print(json.dumps(report["summary"], sort_keys=True))
    return 0 if report["summary"]["failed"] == 0 else 1
