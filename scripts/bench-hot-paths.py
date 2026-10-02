#!/usr/bin/env python3
"""Observation-only hot-path benchmark for a prebuilt tracedecay binary.

Drives the shipped binary through its production boundaries against a private
daemon and a disposable clone of a target repository, then writes every raw
sample as JSONL next to a summary. It never builds, never asserts a wall-clock
budget, and never touches the operator's real profile: HOME, XDG, and every
TRACEDECAY_* storage variable point into one run directory that is removed on
exit.

Lanes (each sample row names its lane and operation):

  cli_startup     `--version` and `--help`: wall time, client CPU, client max RSS
  daemon_start    daemon spawn to socket bound, and to the first answered status
  index           `tracedecay init` to a current generation serving a ready graph
  request         sequential warm reads: status, search, grep, callers, context,
                  plan context; wall time per call and the daemon's CPU delta
  memory          daemon RSS split (anon/file), threads, profile size on disk
  edit_reconcile  one appended line, `tracedecay sync`, until a new generation
                  is current and ready; elapsed time and peak daemon RSS

Session capture/read has its own fixture-backed harness,
`scripts/run-session-temporal-benchmark.sh --run`; this script records that
lane as delegated rather than measuring a lookalike.

Daemon CPU and memory come from /proc, so they are typed `unsupported` off
Linux instead of reported as zero.

Usage:
  scripts/bench-hot-paths.py --bin target/release/tracedecay
  scripts/bench-hot-paths.py --bin ./tracedecay --samples 40 --out target/bench-hot-paths/run1

Exit status: 0 when the run completed (whatever the numbers), 2 on a harness
or preflight error.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import json
import os
import platform
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "scripts" / "lib"))

from portable_process import dies_with_this_process  # noqa: E402

LINUX = sys.platform.startswith("linux")
CLOCK_TICKS = os.sysconf("SC_CLK_TCK") if LINUX else None
PAGE_KB = os.sysconf("SC_PAGE_SIZE") // 1024 if LINUX else None

SEED_SYMBOLS = ["DaemonHandshake", "default_socket_path", "call_default_tool", "TraceDecay"]
CONTEXT_TASK = "how does the daemon serve tool calls"


class HarnessError(Exception):
    pass


@dataclass(frozen=True)
class Call:
    ok: bool
    stdout: bytes
    stderr: bytes
    wall_ms: float
    cpu_ms: float
    max_rss_kb: int


def log(message: str) -> None:
    print(f"bench-hot-paths: {message}", file=sys.stderr, flush=True)


# Below this many samples a p90 is one or two observations, so it stays null.
P90_MIN_SAMPLES = 20


def p90(sorted_values: list[float]) -> float | None:
    if len(sorted_values) < P90_MIN_SAMPLES:
        return None
    rank = max(1, round(0.9 * len(sorted_values)))
    return sorted_values[rank - 1]


def proc_cpu_ms(pid: int) -> float | None:
    if not LINUX:
        return None
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    except OSError:
        return None
    # utime and stime are fields 14 and 15; the split drops pid and comm.
    return (int(fields[11]) + int(fields[12])) * 1000 / CLOCK_TICKS


def proc_status(pid: int) -> dict[str, int] | None:
    if not LINUX:
        return None
    wanted = {"VmRSS", "VmHWM", "RssAnon", "RssFile", "RssShmem", "Threads"}
    try:
        lines = Path(f"/proc/{pid}/status").read_text().splitlines()
    except OSError:
        return None
    values = {}
    for line in lines:
        key, _, rest = line.partition(":")
        if key in wanted:
            values[key] = int(rest.split()[0])
    return values


class DaemonRssSampler:
    def __init__(self, pid: int) -> None:
        self.pid = pid
        self._stop = threading.Event()
        self._lock = threading.Lock()
        self._peak_rss_kb: int | None = None
        self._thread: threading.Thread | None = None

    def _sample(self) -> None:
        status = proc_status(self.pid)
        if status is None or "VmRSS" not in status:
            return
        with self._lock:
            self._peak_rss_kb = max(self._peak_rss_kb or 0, status["VmRSS"])

    def _poll(self) -> None:
        while not self._stop.is_set():
            self._sample()
            self._stop.wait(0.1)

    def start(self) -> None:
        if not LINUX:
            return
        self._sample()
        self._thread = threading.Thread(target=self._poll, name="daemon-rss-sampler", daemon=True)
        self._thread.start()

    def stop(self) -> int | None:
        if self._thread is None:
            return None
        self._stop.set()
        self._thread.join()
        self._sample()
        with self._lock:
            return self._peak_rss_kb


def problem_code(stdout: bytes) -> str | None:
    try:
        return json.loads(stdout)["structuredContent"]["problem"]["code"]
    except (ValueError, KeyError, TypeError):
        return None


def tree_bytes(root: Path) -> int:
    total = 0
    for path in root.rglob("*"):
        try:
            if path.is_file() and not path.is_symlink():
                total += path.stat().st_size
        except OSError:
            continue
    return total


class Run:
    def __init__(self, args: argparse.Namespace, run_dir: Path, target_revision: str) -> None:
        self.bin = str(Path(args.bin).resolve())
        self.samples = args.samples
        self.run_dir = run_dir
        self.repo = run_dir / "repo"
        self.profile = run_dir / "profile"
        self.socket = run_dir / "sock" / "d.sock"
        self.out = Path(args.out).resolve()
        self.index_timeout = args.index_timeout
        self.edit_reconcile = not args.skip_edit_reconcile
        self.source_repo = Path(args.target_repo).resolve()
        self.target_revision = target_revision
        self.seed_symbols = args.seed_symbol or SEED_SYMBOLS
        self.daemon: subprocess.Popen[bytes] | None = None
        self.rows = (self.out / "samples.jsonl").open("w", encoding="utf-8")
        self.env = self._isolated_env()
        self.generation: object = None

    def _isolated_env(self) -> dict[str, str]:
        env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith("TRACEDECAY_") and not key.startswith("XDG_")
        }
        home = self.run_dir / "home"
        env.update(
            HOME=str(home),
            XDG_DATA_HOME=str(home / ".local" / "share"),
            XDG_CONFIG_HOME=str(home / ".config"),
            TRACEDECAY_DATA_DIR=str(self.profile),
            TRACEDECAY_DAEMON_SOCKET=str(self.socket),
            TRACEDECAY_GLOBAL_DB=str(self.profile / "global.db"),
            TRACEDECAY_DISABLE_GLOBAL_DB="1",
        )
        return env

    def record(self, lane: str, op: str, **fields: object) -> None:
        self.rows.write(json.dumps({"lane": lane, "op": op, **fields}, sort_keys=True) + "\n")
        self.rows.flush()

    def client(self, argv: list[str], cwd: Path | None = None) -> Call:
        """Run one CLI process and reap it with wait4 so its own rusage is exact."""
        with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
            start = time.perf_counter()
            process = subprocess.Popen(
                [self.bin, *argv],
                cwd=cwd or self.repo,
                env=self.env,
                stdin=subprocess.DEVNULL,
                stdout=stdout,
                stderr=stderr,
            )
            _, wait_status, usage = os.wait4(process.pid, 0)
            wall_ms = (time.perf_counter() - start) * 1000
            process.returncode = os.waitstatus_to_exitcode(wait_status)
            stdout.seek(0)
            stderr.seek(0)
            return Call(
                ok=process.returncode == 0,
                stdout=stdout.read(),
                stderr=stderr.read(),
                wall_ms=wall_ms,
                cpu_ms=(usage.ru_utime + usage.ru_stime) * 1000,
                max_rss_kb=usage.ru_maxrss if LINUX else usage.ru_maxrss // 1024,
            )

    def tool(self, name: str, args: dict[str, object]) -> Call:
        return self.client(
            ["tool", name, "--args", json.dumps(args), "--project", str(self.repo), "--json"]
        )

    def status_payload(self) -> dict | None:
        call = self.tool("tracedecay_status", {"format": "json"})
        if not call.ok:
            return None
        try:
            return json.loads(json.loads(call.stdout)["content"][0]["text"])
        except (ValueError, KeyError, IndexError, TypeError):
            return None

    def daemon_pid(self) -> int:
        if self.daemon is None or self.daemon.poll() is not None:
            raise HarnessError("the daemon is not running")
        return self.daemon.pid

    # ── lanes ────────────────────────────────────────────────────────────

    def lane_cli_startup(self) -> None:
        log("cli_startup")
        for op, argv in (("version", ["--version"]), ("help", ["--help"])):
            for sample in range(self.samples):
                call = self.client(argv, cwd=self.run_dir)
                self.record("cli_startup", op, sample=sample, ok=call.ok, wall_ms=call.wall_ms,
                            client_cpu_ms=call.cpu_ms, client_max_rss_kb=call.max_rss_kb)

    def lane_daemon_start(self) -> None:
        log("daemon_start")
        self.socket.parent.mkdir(mode=0o700, parents=True)
        start = time.perf_counter()
        self.daemon = subprocess.Popen(
            [self.bin, "daemon", "run", "--socket", str(self.socket)],
            cwd=self.run_dir,
            env=self.env,
            stdin=subprocess.DEVNULL,
            stdout=(self.run_dir / "daemon.log").open("wb"),
            stderr=subprocess.STDOUT,
            start_new_session=True,
            preexec_fn=dies_with_this_process(),
        )
        deadline = time.monotonic() + 300
        while not self.socket.exists():
            self.daemon_pid()
            if time.monotonic() > deadline:
                raise HarnessError("the daemon did not bind its socket within 300 s")
            time.sleep(0.05)
        self.record("daemon_start", "socket_bound", wall_ms=(time.perf_counter() - start) * 1000)
        # Nothing is enrolled yet, so a serving daemon answers with a typed
        # refusal such as project_not_enrolled. Only daemon_unavailable means
        # the request never reached it.
        while True:
            self.daemon_pid()
            call = self.tool("tracedecay_status", {"format": "json"})
            code = problem_code(call.stdout)
            if call.ok or (code is not None and code != "daemon_unavailable"):
                break
            if time.monotonic() > deadline:
                raise HarnessError("the daemon did not answer tracedecay_status within 300 s")
            time.sleep(0.2)
        self.record("daemon_start", "first_status_answered", answer=code or "ok",
                    wall_ms=(time.perf_counter() - start) * 1000)

    def wait_for_ready(self, lane: str, previous_generation: str | None) -> dict[str, object]:
        pid = self.daemon_pid()
        start = time.perf_counter()
        deadline = time.monotonic() + self.index_timeout
        peak_rss_kb = 0
        polls = 0
        while True:
            self.daemon_pid()
            status = proc_status(pid)
            if status:
                peak_rss_kb = max(peak_rss_kb, status.get("VmRSS", 0))
            payload = self.status_payload()
            polls += 1
            if payload is not None:
                freshness = payload.get("code_index_freshness") or {}
                worktree = freshness.get("worktree") or {}
                serving = worktree.get("code_graph_serving") or {}
                generation = worktree.get("latest_generation_id")
                if serving.get("state") == "refused":
                    raise HarnessError(f"the daemon refused to serve a graph: {serving}")
                if (
                    freshness.get("status") == "current"
                    and serving.get("state") == "ready"
                    and generation != previous_generation
                ):
                    return {
                        "wall_ms": (time.perf_counter() - start) * 1000,
                        "peak_daemon_rss_kb": peak_rss_kb if LINUX else None,
                        "generation_id": generation,
                        "status_polls": polls,
                    }
            if time.monotonic() > deadline:
                raise HarnessError(f"{lane}: no current, ready generation within {self.index_timeout} s")
            time.sleep(1)

    def lane_index(self) -> None:
        log("index (init until a current generation serves a ready graph)")
        start = time.perf_counter()
        init = self.client(["init"])
        if not init.ok:
            raise HarnessError(f"`tracedecay init` failed: {init.stderr[-2000:].decode(errors='replace')}")
        ready = self.wait_for_ready("index", None)
        self.record("index", "init_request", wall_ms=init.wall_ms)
        self.record("index", "init_to_ready", **{**ready, "wall_ms": (time.perf_counter() - start) * 1000})
        self.generation = ready["generation_id"]

    def resolve_node(self) -> tuple[str, str] | None:
        unavailable = f"no seed symbol resolved to a node; tried: {', '.join(self.seed_symbols)}"
        for attempt in range(3):
            for symbol in self.seed_symbols:
                call = self.tool("tracedecay_search", {"query": symbol, "limit": 10, "format": "json"})
                if not call.ok:
                    continue
                # `tool --json` always carries the typed answer in
                # structuredContent; content[].text may be a truncated preview.
                try:
                    hits = json.loads(call.stdout)["structuredContent"]["results"]
                except (ValueError, KeyError, TypeError):
                    continue
                if not isinstance(hits, list):
                    continue
                for hit in hits:
                    if not isinstance(hit, dict):
                        continue
                    node_id = hit.get("node_id")
                    if isinstance(node_id, str) and node_id:
                        self.record("request", "callers_node", node_id=node_id, seed_symbol=symbol)
                        return node_id, symbol
            if attempt < 2:
                time.sleep(1)
        self.record("request", "callers_node", unavailable=unavailable)
        return None

    def request_ops(
        self, caller_node: tuple[str, str] | None
    ) -> list[tuple[str, str, dict[str, object]]]:
        first_seed = self.seed_symbols[0]
        second_seed = self.seed_symbols[1] if len(self.seed_symbols) > 1 else first_seed
        ops = [
            ("status", "tracedecay_status", {"format": "json"}),
            ("search_symbol", "tracedecay_search", {"query": first_seed, "limit": 10, "format": "json"}),
            ("search_identifier", "tracedecay_search", {"query": second_seed, "limit": 10, "format": "json"}),
            ("grep", "tracedecay_grep", {"pattern": second_seed, "fixed_strings": True, "max_results": 20, "format": "json"}),
            ("context", "tracedecay_context", {"task": CONTEXT_TASK, "max_nodes": 20, "format": "json"}),
            ("plan_context", "tracedecay_context", {"task": CONTEXT_TASK, "max_nodes": 20, "format": "json", "mode": "plan"}),
        ]
        if caller_node:
            node_id, _ = caller_node
            ops.insert(4, ("callers", "tracedecay_callers", {
                "node_id": node_id, "maximum_depth": 2, "format": "json",
            }))
        else:
            unavailable = f"no seed symbol resolved to a node; tried: {', '.join(self.seed_symbols)}"
            self.record("request", "callers", unavailable=unavailable)
        return ops

    def lane_request(self) -> None:
        log("request (sequential warm reads)")
        caller_node = self.resolve_node()
        pid = self.daemon_pid()
        ops = self.request_ops(caller_node)
        for op, tool, args in ops:
            self.tool(tool, args)
        for op, tool, args in ops:
            for sample in range(self.samples):
                before = proc_cpu_ms(pid)
                call = self.tool(tool, args)
                after = proc_cpu_ms(pid)
                daemon_cpu = after - before if before is not None and after is not None else None
                self.record("request", op, sample=sample, ok=call.ok, wall_ms=call.wall_ms,
                            client_cpu_ms=call.cpu_ms, daemon_cpu_ms=daemon_cpu,
                            problem=None if call.ok else problem_code(call.stdout))

    def lane_memory(self, phase: str) -> None:
        pid = self.daemon_pid()
        status = proc_status(pid)
        retained = None
        payload = self.status_payload()
        if payload is not None:
            memory = payload.get("memory") or {}
            retained = {
                "retained_bytes": memory.get("retained_bytes"),
                "owners": {row.get("kind"): row.get("bytes") for row in memory.get("owners") or []},
            }
        if status is None:
            self.record("memory", phase, unsupported="daemon /proc counters are Linux-only",
                        profile_bytes=tree_bytes(self.profile), status_memory=retained)
            return
        self.record("memory", phase, rss_kb=status.get("VmRSS"), hwm_kb=status.get("VmHWM"),
                    anon_kb=status.get("RssAnon"), file_kb=status.get("RssFile"),
                    threads=status.get("Threads"), profile_bytes=tree_bytes(self.profile),
                    status_memory=retained)

    def lane_edit_reconcile(self) -> None:
        log("edit_reconcile (one appended line until a new generation is ready)")
        tracked = subprocess.run(["git", "-C", str(self.repo), "ls-files", "*.rs", "*.py"],
                                 capture_output=True, text=True, check=True).stdout.split()
        if not tracked:
            self.record("edit_reconcile", "edit_to_ready", unavailable="no tracked .rs or .py file")
            return
        target = self.repo / tracked[0]
        marker = "//" if target.suffix == ".rs" else "#"
        with target.open("a", encoding="utf-8") as handle:
            handle.write(f"\n{marker} bench-hot-paths edit {time.time_ns()}\n")
        start = time.perf_counter()
        rss_sampler = DaemonRssSampler(self.daemon_pid())
        rss_sampler.start()
        try:
            sync = self.client(["sync"])
            if not sync.ok:
                raise HarnessError(f"`tracedecay sync` failed: {sync.stderr[-2000:].decode(errors='replace')}")
            ready = self.wait_for_ready("edit_reconcile", self.generation)
        finally:
            peak_rss_kb = rss_sampler.stop()
        if peak_rss_kb is None:
            ready.pop("peak_daemon_rss_kb", None)
            ready["unsupported"] = (
                "daemon /proc counters are Linux-only" if not LINUX else "daemon VmRSS is unavailable"
            )
        else:
            ready["peak_daemon_rss_kb"] = peak_rss_kb
        self.record("edit_reconcile", "sync_request", wall_ms=sync.wall_ms)
        self.record("edit_reconcile", "edited_file", path=tracked[0])
        self.record("edit_reconcile", "edit_to_ready",
                    **{**ready, "wall_ms": (time.perf_counter() - start) * 1000})

    def stop_daemon(self) -> None:
        if self.daemon is None or self.daemon.poll() is not None:
            return
        os.killpg(self.daemon.pid, signal.SIGTERM)
        try:
            self.daemon.wait(timeout=30)
        except subprocess.TimeoutExpired:
            os.killpg(self.daemon.pid, signal.SIGKILL)
            self.daemon.wait(timeout=10)

    def execute(self) -> None:
        log(f"cloning {self.source_repo} into the run directory")
        # No --local: it makes a failed hardlink fatal, and the run dir sits on
        # TMPDIR, which is often a different filesystem from the target.
        subprocess.run(["git", "clone", "--quiet", str(self.source_repo), str(self.repo)],
                       check=True, env=self.env)
        cloned_revision = subprocess.run(
            ["git", "-C", str(self.repo), "rev-parse", "HEAD"],
            capture_output=True, text=True, check=False,
        )
        if cloned_revision.returncode != 0:
            raise HarnessError("could not determine the cloned target revision")
        actual_revision = cloned_revision.stdout.strip()
        if actual_revision != self.target_revision:
            raise HarnessError(
                f"cloned target revision mismatch: expected {self.target_revision}, got {actual_revision}"
            )
        for path in (self.profile, self.run_dir / "home" / ".config", self.run_dir / "home" / ".local" / "share"):
            path.mkdir(parents=True, exist_ok=True)
        self.lane_cli_startup()
        self.lane_daemon_start()
        self.lane_index()
        self.lane_memory("after_index")
        self.lane_request()
        self.lane_memory("after_requests")
        if self.edit_reconcile:
            self.lane_edit_reconcile()
            self.lane_memory("after_edit_reconcile")
        self.record("session_capture_read", "delegated",
                    harness="scripts/run-session-temporal-benchmark.sh --run")
        alive = self.daemon is not None and self.daemon.poll() is None
        self.record("daemon", "alive_after_run", ok=alive)
        if not alive:
            raise HarnessError("the daemon died during the run")


def summarize(samples_path: Path, meta: dict[str, object]) -> tuple[dict, str]:
    groups: dict[tuple[str, str], list[dict]] = {}
    other: list[dict] = []
    for line in samples_path.read_text(encoding="utf-8").splitlines():
        row = json.loads(line)
        if "sample" in row:
            groups.setdefault((row["lane"], row["op"]), []).append(row)
        else:
            other.append(row)
    summary = {"meta": meta, "distributions": [], "single": other}
    lines = [
        f"# bench-hot-paths: {meta['binary_version']}",
        "",
        f"host `{meta['host']}`, {meta['samples']} samples per op, target `{meta['target_revision']}`",
        "",
        "| lane | op | n | err | wall p50 ms | wall p90 ms | wall max ms | daemon CPU p50 ms |",
        "| --- | --- | --- | --- | --- | --- | --- | --- |",
    ]
    for (lane, op), rows in groups.items():
        wall = sorted(row["wall_ms"] for row in rows)
        cpu = sorted(row["daemon_cpu_ms"] for row in rows if row.get("daemon_cpu_ms") is not None)
        entry = {
            "lane": lane, "op": op, "n": len(rows),
            "errors": sum(1 for row in rows if not row.get("ok")),
            "wall_ms": {"min": wall[0], "p50": statistics.median(wall),
                        "p90": p90(wall), "max": wall[-1]},
            "daemon_cpu_ms_p50": statistics.median(cpu) if cpu else None,
        }
        summary["distributions"].append(entry)
        cpu_cell = f"{entry['daemon_cpu_ms_p50']:.0f}" if cpu else "n/a"
        p90_cell = "n/a" if entry["wall_ms"]["p90"] is None else f"{entry['wall_ms']['p90']:.1f}"
        lines.append(
            f"| {lane} | {op} | {entry['n']} | {entry['errors']} | {entry['wall_ms']['p50']:.1f} "
            f"| {p90_cell} | {entry['wall_ms']['max']:.1f} | {cpu_cell} |"
        )
    lines += ["", "| lane | op | observation |", "| --- | --- | --- |"]
    for row in other:
        fields = {key: value for key, value in row.items() if key not in ("lane", "op")}
        lines.append(f"| {row['lane']} | {row['op']} | `{json.dumps(fields, sort_keys=True)}` |")
    return summary, "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bin", required=True, help="prebuilt tracedecay binary; never built here")
    parser.add_argument("--target-repo", default=str(REPO_ROOT),
                        help="repository to clone and index (default: this checkout)")
    parser.add_argument("--samples", type=int, default=20, help="sequential samples per operation")
    parser.add_argument("--out", default=str(REPO_ROOT / "target" / "bench-hot-paths"),
                        help="directory for samples.jsonl, summary.json, summary.md")
    parser.add_argument("--index-timeout", type=int, default=1800, help="seconds to wait for a ready graph")
    parser.add_argument("--skip-edit-reconcile", action="store_true", help="skip the edit_reconcile lane")
    parser.add_argument("--seed-symbol", action="append", metavar="NAME",
                        help="symbol to search for and resolve as a callers node; repeatable")
    args = parser.parse_args()

    binary = Path(args.bin)
    if not (binary.is_file() and os.access(binary, os.X_OK)):
        log(f"--bin '{args.bin}' is not an executable file")
        return 2
    if args.samples < 1:
        log("--samples must be at least 1")
        return 2
    target = Path(args.target_repo)
    git_dir = subprocess.run(["git", "-C", str(target), "rev-parse", "--absolute-git-dir"],
                             capture_output=True, text=True, check=False)
    if git_dir.returncode != 0:
        log(f"--target-repo '{target}' is not a git checkout")
        return 2
    target_status = subprocess.run(["git", "-C", str(target), "status", "--porcelain"],
                                   capture_output=True, text=True, check=False)
    if target_status.returncode != 0:
        log(f"could not inspect --target-repo '{target}' status")
        return 2
    if target_status.stdout.strip():
        log(f"--target-repo '{target}' is dirty; commit or stash first")
        return 2
    target_revision_result = subprocess.run(["git", "-C", str(target), "rev-parse", "HEAD"],
                                            capture_output=True, text=True, check=False)
    if target_revision_result.returncode != 0:
        log(f"could not determine --target-repo '{target}' revision")
        return 2
    target_revision = target_revision_result.stdout.strip()
    Path(args.out).mkdir(parents=True, exist_ok=True)

    # A Unix socket path is capped near 108 bytes, so keep the run dir short.
    run_dir = Path(tempfile.mkdtemp(prefix="tdhot.", dir=os.environ.get("TMPDIR", "/tmp")))
    run_dir.chmod(0o700)
    run = Run(args, run_dir, target_revision)
    meta = {
        "binary": run.bin,
        "binary_version": subprocess.run([run.bin, "--version"], capture_output=True, text=True,
                                         check=False).stdout.strip(),
        "host": f"{platform.system()} {platform.machine()} cpus={os.cpu_count()}",
        "samples": args.samples,
        "target_repo": str(run.source_repo),
        "target_revision": target_revision,
        "started_at_unix": int(time.time()),
    }
    status = 0
    try:
        run.execute()
    except (HarnessError, subprocess.CalledProcessError) as error:
        log(f"harness error: {error}")
        log_path = run_dir / "daemon.log"
        if log_path.exists():
            sys.stderr.write("".join(log_path.read_text(errors="replace").splitlines(True)[-40:]))
        status = 2
    finally:
        run.stop_daemon()
        run.rows.close()
        shutil.rmtree(run_dir, ignore_errors=True)
    summary, markdown = summarize(run.out / "samples.jsonl", meta)
    (run.out / "summary.json").write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
    (run.out / "summary.md").write_text(markdown)
    sys.stdout.write(markdown)
    return status


if __name__ == "__main__":
    raise SystemExit(main())
