#!/usr/bin/env python3
"""Hermetic CLI and persistent-MCP request-overhead benchmark.

The benchmark reuses the efficiency scorecard's disposable daemon/profile/index
fixture. It never resolves the operator daemon, HOME, or TraceDecay stores.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
import select
import subprocess
import sys
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SCORECARD_PATH = REPO_ROOT / "scripts" / "efficiency-scorecard.py"
SCHEMA = "tracedecay.request-overhead-benchmark/v1"

TOOL_ARGUMENTS = {
    "search": (
        "search",
        (
            {"query": "BeaconState010", "limit": 10, "format": "json"},
            {"query": "WatermarkState021", "limit": 10, "format": "json"},
        ),
    ),
    "grep": (
        "grep",
        (
            {
                "pattern": "saturating_add",
                "fixed_strings": True,
                "max_results": 20,
                "format": "json",
            },
            {
                "pattern": "Bounded accumulator over an ordered keyspace",
                "fixed_strings": True,
                "max_results": 20,
                "format": "json",
            },
        ),
    ),
    "memory_recall": (
        "fact_store_search",
        (
            {"query": "fixture corpus decision", "limit": 5, "format": "json"},
            {"query": "daemon socket policy", "limit": 5, "format": "json"},
        ),
    ),
    "status": (
        "status",
        ({"format": "json", "include_branch_diagnostics": False},),
    ),
    "git_status": (
        "git_status",
        ({"format": "json"},),
    ),
}


def load_scorecard_module():
    spec = importlib.util.spec_from_file_location("efficiency_scorecard", SCORECARD_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {SCORECARD_PATH}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def payload_digest(payload: object) -> str:
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()


def summarize(samples: list[dict], percentile) -> dict:
    by_surface: dict[str, dict[str, list[float]]] = {}
    for sample in samples:
        by_surface.setdefault(sample["surface"], {}).setdefault(sample["tool"], []).append(
            sample["elapsed_ms"]
        )
    return {
        surface: {
            tool: {
                "samples": len(values),
                "p50_ms": round(percentile(values, 0.50), 3),
                "p95_ms": round(percentile(values, 0.95), 3),
                "max_ms": round(max(values), 3),
            }
            for tool, values in tools.items()
        }
        for surface, tools in by_surface.items()
    }


def extract_tool_payload(result: dict) -> object:
    texts = [
        block["text"]
        for block in result.get("content", [])
        if isinstance(block, dict) and isinstance(block.get("text"), str)
    ]
    payloads = []
    for text in texts:
        try:
            payloads.append(json.loads(text))
        except json.JSONDecodeError:
            continue
    if len(payloads) != 1:
        raise RuntimeError(f"expected one JSON tool payload, observed {len(payloads)}")
    return payloads[0]


class PersistentMcpClient:
    def __init__(self, binary: Path, project: Path, env: dict[str, str], log: Path) -> None:
        self._stderr = log.open("wb")
        self._process = subprocess.Popen(
            (str(binary), "serve", "--path", str(project)),
            cwd=project,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self._stderr,
            text=True,
            bufsize=1,
            start_new_session=True,
        )
        self._next_id = 1
        self.request(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "request-overhead-benchmark",
                    "version": "1",
                },
            },
        )
        self.notify("notifications/initialized", {})

    def _write(self, payload: dict) -> None:
        if self._process.stdin is None:
            raise RuntimeError("persistent MCP stdin is unavailable")
        self._process.stdin.write(json.dumps(payload, separators=(",", ":")) + "\n")
        self._process.stdin.flush()

    def _read_response(self, request_id: int, timeout: float = 60.0) -> dict:
        if self._process.stdout is None:
            raise RuntimeError("persistent MCP stdout is unavailable")
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f"persistent MCP request {request_id} timed out")
            ready, _, _ = select.select([self._process.stdout], [], [], remaining)
            if not ready:
                raise TimeoutError(f"persistent MCP request {request_id} timed out")
            line = self._process.stdout.readline()
            if not line:
                raise RuntimeError(
                    f"persistent MCP exited before response {request_id}: "
                    f"{self._process.poll()}"
                )
            message = json.loads(line)
            if message.get("id") != request_id:
                continue
            if "error" in message:
                raise RuntimeError(f"persistent MCP error: {message['error']}")
            return message["result"]

    def request(self, method: str, params: dict) -> dict:
        request_id = self._next_id
        self._next_id += 1
        self._write(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": method,
                "params": params,
            }
        )
        return self._read_response(request_id)

    def notify(self, method: str, params: dict) -> None:
        self._write({"jsonrpc": "2.0", "method": method, "params": params})

    def tool_json(self, name: str, arguments: dict) -> object:
        result = self.request(
            "tools/call",
            {"name": f"tracedecay_{name}", "arguments": arguments},
        )
        return extract_tool_payload(result)

    def close(self) -> None:
        if self._process.stdin is not None:
            self._process.stdin.close()
        try:
            self._process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self._process.terminate()
            self._process.wait(timeout=5)
        self._stderr.close()


def run_battery(surface: str, invoke, samples: int) -> list[dict]:
    output = []
    for warmup in range(2):
        for _, (name, variants) in TOOL_ARGUMENTS.items():
            invoke(name, variants[warmup % len(variants)])
    for index in range(samples):
        for tool, (name, variants) in TOOL_ARGUMENTS.items():
            started = time.perf_counter_ns()
            payload = invoke(name, variants[index % len(variants)])
            elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
            output.append(
                {
                    "surface": surface,
                    "tool": tool,
                    "sample": index,
                    "elapsed_ms": round(elapsed_ms, 6),
                    "payload_sha256": payload_digest(payload),
                }
            )
    return output


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=40)
    parser.add_argument("--label", default="")
    parser.add_argument("--keep-sandbox", action="store_true")
    args = parser.parse_args()
    if args.samples < 40:
        parser.error("--samples must be at least 40 for a p95 distribution")
    if args.output.exists():
        parser.error(f"--output already exists: {args.output}")

    scorecard = load_scorecard_module()
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error(f"--binary is not a file: {binary}")
    args.output.mkdir(parents=True)
    hotpath_report = args.output / "daemon-hotpath.json"

    sandbox = scorecard.Sandbox(
        binary=binary,
        fixture=scorecard.DEFAULT_FIXTURE,
        keep=args.keep_sandbox,
    )
    try:
        sandbox.env.update(
            HOTPATH_OUTPUT_FORMAT="json-pretty",
            HOTPATH_OUTPUT_PATH=str(hotpath_report),
            HOTPATH_REPORT=(
                "functions-timing,futures,rw_locks,mutexes,io,threads,debug"
            ),
            HOTPATH_TIME_SAMPLING_RATE="1",
            HOTPATH_ENTRIES_LIMIT="512",
            HOTPATH_LOGS_LIMIT="200",
            HOTPATH_METRICS_SERVER_OFF="true",
        )
        sandbox.spawn_daemon()
        # Child CLI/proxy processes must not overwrite the daemon's report.
        sandbox.env.pop("HOTPATH_OUTPUT_PATH", None)
        init = sandbox.run_cli("init", timeout=scorecard.COLD_INDEX_DEADLINE)
        if init.returncode != 0:
            raise RuntimeError(f"sandbox init failed: {init.stderr[-1000:]}")
        # Initial enrollment may race the background code-index scheduler.
        # Request an explicit synchronization so benchmark setup has a sealed,
        # current generation before latency sampling begins.
        sync = sandbox.run_cli("sync", timeout=scorecard.COLD_INDEX_DEADLINE)
        if sync.returncode != 0:
            raise RuntimeError(f"sandbox sync failed: {sync.stderr[-1000:]}")
        scorecard.wait_for(
            sandbox,
            "cold_index",
            scorecard.COLD_INDEX_DEADLINE,
            scorecard.freshness_current,
        )
        for fact in scorecard.SEED_FACTS:
            sandbox.tool_json(
                "fact_store_add",
                {"content": fact, "category": "decision", "format": "json"},
            )

        samples = run_battery("cli", sandbox.tool_json, args.samples)
        client = PersistentMcpClient(
            binary,
            sandbox.project,
            sandbox.env,
            args.output / "persistent-mcp.stderr.log",
        )
        try:
            samples.extend(run_battery("persistent_mcp", client.tool_json, args.samples))
        finally:
            client.close()
        sandbox.stop_daemon()

        report = {
            "schema": SCHEMA,
            "label": args.label,
            "binary": str(binary),
            "binary_sha256": scorecard.sha256_file(binary),
            "fixture_sha256": scorecard.sha256_tree(scorecard.DEFAULT_FIXTURE),
            "samples": samples,
            "summary": summarize(samples, scorecard.percentile),
            "hotpath_report": str(hotpath_report),
            "sandboxed": True,
        }
        (args.output / "request-overhead.json").write_text(
            json.dumps(report, indent=2, sort_keys=True) + "\n"
        )
        print(json.dumps(report["summary"], indent=2, sort_keys=True))
        return 0
    finally:
        sandbox.cleanup()


if __name__ == "__main__":
    raise SystemExit(main())
