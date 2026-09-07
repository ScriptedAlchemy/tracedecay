#!/usr/bin/env python3
"""OS/process counter snapshots that record deltas, not lifetime totals.

Owned by the hotpath OS-profiling harness. Do not import this from CI
commit-msg or distribution gates.

A lifetime counter (rchar, utime, minflt, …) is stored on each snapshot
and subtracted between phases. Gauges (RSS, swap, cgroup memory.current)
are stored as absolute values at each phase *and* as phase-to-phase
deltas so catch-up vs idle leaks are visible.

File-descriptor classification uses basename + fd type only. Full paths
are hashed; they are never written into a report.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import posixpath
import re
import stat
import sys
import time
from pathlib import Path
from typing import Any

SCHEMA = "tracedecay.hotpath.os_counter_profile.v1"

STORE_BASENAMES = {
    "tracedecay.db": "store_graph",
    "graph.db": "store_graph",
    "sessions.db": "store_sessions",
    "user-sessions.db": "store_sessions",
    "user-memory.db": "store_memory",
    "memory.db": "store_memory",
    "global.db": "store_profile",
    "remote.db": "store_remote",
    "store_manifest.json": "store_manifest",
    "profile-identity.json": "store_identity",
    "enrollment.json": "store_identity",
    "branch-meta.json": "store_identity",
    "tracedecay-project.json": "store_identity",
    "lifecycle.lock": "store_lock",
}

ARTIFACT_PARENTS = frozenset(
    {
        "artifacts",
        "capture",
        "code-index-v1",
        "app-dist",
        "replay",
        "recovery",
    }
)
TRANSCRIPT_PARENTS = frozenset({"transcripts", "sessions", "session-archive"})
SAFE_BASENAME = re.compile(r"^[A-Za-z0-9._+-]{1,128}$")


def observed(value: Any) -> dict[str, Any]:
    return {"state": "observed", "value": value}


def unavailable(reason: str) -> dict[str, Any]:
    return {"state": "unavailable", "reason": reason}


def path_digest(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8", "surrogateescape")).hexdigest()[:16]


def clk_tck() -> int:
    ticks = os.sysconf("SC_CLK_TCK")
    if ticks <= 0:
        raise RuntimeError("SC_CLK_TCK must be a positive integer")
    return ticks


def read_text(path: Path) -> dict[str, Any]:
    try:
        return observed(path.read_text(encoding="utf-8", errors="replace"))
    except FileNotFoundError:
        return unavailable("missing")
    except PermissionError:
        return unavailable("permission_denied")
    except OSError as error:
        return unavailable(error.__class__.__name__)


def parse_key_colon_u64(text: str, key: str) -> dict[str, Any]:
    prefix = f"{key}:"
    for line in text.splitlines():
        if line.startswith(prefix):
            token = line[len(prefix) :].split()
            if not token:
                return unavailable("empty")
            try:
                return observed(int(token[0]))
            except ValueError:
                return unavailable("unparseable")
    return unavailable("missing_key")


def parse_status_kib(text: str, key: str) -> dict[str, Any]:
    raw = parse_key_colon_u64(text, key)
    if raw["state"] != "observed":
        return raw
    return observed(raw["value"] * 1024)


def parse_smaps_kib(text: str, key: str) -> dict[str, Any]:
    return parse_status_kib(text, key)


def split_proc_stat(text: str) -> dict[str, Any]:
    close = text.rfind(")")
    if close < 0:
        return unavailable("missing_comm")
    fields = text[close + 1 :].split()
    # After comm: [0]=state [7]=minflt [9]=majflt [11]=utime [12]=stime
    needed = {"state": 0, "minflt": 7, "majflt": 9, "utime": 11, "stime": 12}
    if len(fields) <= max(needed.values()):
        return unavailable("short_stat")
    parsed: dict[str, Any] = {"state": fields[0]}
    for name, index in needed.items():
        if name == "state":
            continue
        try:
            parsed[name] = int(fields[index])
        except ValueError:
            return unavailable(f"unparseable_{name}")
    return observed(parsed)


def parse_io(text: str) -> dict[str, Any]:
    keys = (
        "rchar",
        "wchar",
        "syscr",
        "syscw",
        "read_bytes",
        "write_bytes",
        "cancelled_write_bytes",
    )
    values: dict[str, Any] = {}
    for key in keys:
        values[key] = parse_key_colon_u64(text, key)
    return values


def parse_memory_events(text: str) -> dict[str, Any]:
    events: dict[str, int] = {}
    for line in text.splitlines():
        parts = line.split()
        if len(parts) != 2:
            continue
        try:
            events[parts[0]] = int(parts[1])
        except ValueError:
            return unavailable("unparseable")
    if not events:
        return unavailable("empty")
    return observed(events)


def parse_io_stat(text: str) -> dict[str, Any]:
    devices: list[dict[str, Any]] = []
    for line in text.splitlines():
        if not line.strip():
            continue
        tokens = line.split()
        device = tokens[0]
        if ":" not in device:
            continue
        fields: dict[str, int] = {}
        for token in tokens[1:]:
            if "=" not in token:
                continue
            key, raw = token.split("=", 1)
            try:
                fields[key] = int(raw)
            except ValueError:
                continue
        devices.append({"device": device, **fields})
    if not devices:
        return unavailable("empty")
    return observed(devices)


def classify_fd_target(target: str, fd_type: str) -> dict[str, Any]:
    if target.startswith("socket:"):
        family = "socket"
    elif target.startswith("pipe:") or target.startswith("fifo:"):
        family = "pipe"
    elif target.startswith("anon_inode:"):
        kind = target.split(":", 1)[1].strip("[]")
        family = f"anon_{kind}" if kind else "anon_inode"
    else:
        name = posixpath.basename(target.rstrip("/"))
        parent = posixpath.basename(posixpath.dirname(target))
        if name.endswith("-wal"):
            family = "store_wal"
        elif name.endswith("-shm"):
            family = "store_shm"
        elif name.endswith(".dirty"):
            family = "store_dirty"
        elif name in STORE_BASENAMES:
            family = STORE_BASENAMES[name]
        elif parent in TRANSCRIPT_PARENTS or name.endswith((".jsonl", ".jsonl.gz")):
            family = "transcript"
        elif parent in ARTIFACT_PARENTS:
            family = "artifact"
        elif fd_type in {"socket", "fifo", "pipe"}:
            family = fd_type
        else:
            family = "other"

    record: dict[str, Any] = {
        "family": family,
        "fd_type": fd_type,
        "path_digest": path_digest(target),
    }
    basename = posixpath.basename(target.rstrip("/"))
    if family.startswith("store_") and basename in STORE_BASENAMES:
        record["basename"] = basename
    elif family in {"store_wal", "store_shm", "store_dirty"}:
        record["suffix"] = basename.rsplit(".", 1)[-1] if "." in basename else basename[-4:]
    return record


def fd_type_name(mode: int) -> str:
    if stat.S_ISSOCK(mode):
        return "socket"
    if stat.S_ISFIFO(mode):
        return "fifo"
    if stat.S_ISREG(mode):
        return "reg"
    if stat.S_ISDIR(mode):
        return "dir"
    if stat.S_ISCHR(mode):
        return "chr"
    if stat.S_ISBLK(mode):
        return "blk"
    if stat.S_ISLNK(mode):
        return "lnk"
    return "other"


def sample_fds(pid: int) -> dict[str, Any]:
    fd_dir = Path(f"/proc/{pid}/fd")
    try:
        entries = list(fd_dir.iterdir())
    except FileNotFoundError:
        return unavailable("missing")
    except PermissionError:
        return unavailable("permission_denied")
    except OSError as error:
        return unavailable(error.__class__.__name__)

    families: dict[str, int] = {}
    samples: list[dict[str, Any]] = []
    for entry in entries:
        try:
            target = os.readlink(entry)
            mode = entry.stat(follow_symlinks=False).st_mode
        except OSError:
            continue
        classified = classify_fd_target(target, fd_type_name(mode))
        family = classified["family"]
        families[family] = families.get(family, 0) + 1
        samples.append(classified)
    return observed(
        {
            "open_count": len(entries),
            "classified_count": len(samples),
            "by_family": dict(sorted(families.items())),
            "descriptors": samples,
        }
    )


def sample_threads(pid: int) -> dict[str, Any]:
    task_dir = Path(f"/proc/{pid}/task")
    try:
        tids = [int(entry.name) for entry in task_dir.iterdir() if entry.name.isdigit()]
    except FileNotFoundError:
        return unavailable("missing")
    except PermissionError:
        return unavailable("permission_denied")
    except OSError as error:
        return unavailable(error.__class__.__name__)

    by_state: dict[str, int] = {}
    blocked: list[dict[str, Any]] = []
    for tid in tids:
        stat_text = read_text(Path(f"/proc/{pid}/task/{tid}/stat"))
        if stat_text["state"] != "observed":
            continue
        parsed = split_proc_stat(stat_text["value"])
        if parsed["state"] != "observed":
            continue
        state = parsed["value"]["state"]
        by_state[state] = by_state.get(state, 0) + 1
        if state != "D":
            continue
        wchan = read_text(Path(f"/proc/{pid}/task/{tid}/wchan"))
        syscall = read_text(Path(f"/proc/{pid}/task/{tid}/syscall"))
        blocked.append(
            {
                "tid": tid,
                "wchan": wchan["value"].strip() if wchan["state"] == "observed" else None,
                "syscall": (
                    syscall["value"].split()[0]
                    if syscall["state"] == "observed" and syscall["value"].split()
                    else None
                ),
            }
        )
    return observed(
        {
            "thread_count": len(tids),
            "by_state": dict(sorted(by_state.items())),
            "blocked_io": blocked,
            "blocked_io_count": len(blocked),
        }
    )


def resolve_cgroup_dir(pid: int) -> dict[str, Any]:
    raw = read_text(Path(f"/proc/{pid}/cgroup"))
    if raw["state"] != "observed":
        return raw
    relative = None
    for line in raw["value"].splitlines():
        if "::" in line:
            relative = line.split("::", 1)[1].strip()
            break
        parts = line.split(":", 2)
        if len(parts) == 3 and "memory" in parts[1]:
            relative = parts[2].strip()
    if relative is None:
        return unavailable("no_cgroup_line")
    relative = relative.lstrip("/")
    candidates = [
        Path("/sys/fs/cgroup") / relative,
        Path("/sys/fs/cgroup/memory") / relative,
    ]
    for candidate in candidates:
        if candidate.is_dir():
            return observed(
                {
                    "dir": str(candidate),
                    "leaf": posixpath.basename(relative.rstrip("/") or "root"),
                    "path_digest": path_digest(relative),
                }
            )
    return unavailable("cgroup_dir_missing")


def sample_cgroup(pid: int) -> dict[str, Any]:
    location = resolve_cgroup_dir(pid)
    if location["state"] != "observed":
        return location
    directory = Path(location["value"]["dir"])
    current = read_text(directory / "memory.current")
    events = read_text(directory / "memory.events")
    pressure = read_text(directory / "memory.pressure")
    io_stat = read_text(directory / "io.stat")
    return observed(
        {
            "leaf": location["value"]["leaf"],
            "path_digest": location["value"]["path_digest"],
            "memory_current_bytes": (
                observed(int(current["value"].strip()))
                if current["state"] == "observed" and current["value"].strip().isdigit()
                else current
                if current["state"] != "observed"
                else unavailable("unparseable")
            ),
            "memory_events": (
                parse_memory_events(events["value"])
                if events["state"] == "observed"
                else events
            ),
            "memory_pressure": (
                observed(pressure["value"].strip())
                if pressure["state"] == "observed"
                else pressure
            ),
            "io_stat": parse_io_stat(io_stat["value"]) if io_stat["state"] == "observed" else io_stat,
        }
    )


def sample_proc(pid: int) -> dict[str, Any]:
    captured_ns = time.time_ns()
    io_text = read_text(Path(f"/proc/{pid}/io"))
    stat_text = read_text(Path(f"/proc/{pid}/stat"))
    status_text = read_text(Path(f"/proc/{pid}/status"))
    smaps_text = read_text(Path(f"/proc/{pid}/smaps_rollup"))
    parsed_stat = (
        split_proc_stat(stat_text["value"]) if stat_text["state"] == "observed" else stat_text
    )
    status_value = status_text["value"] if status_text["state"] == "observed" else ""
    smaps_value = smaps_text["value"] if smaps_text["state"] == "observed" else ""
    return {
        "pid": pid,
        "captured_ns": captured_ns,
        "clk_tck": clk_tck(),
        "io": parse_io(io_text["value"]) if io_text["state"] == "observed" else {"_": io_text},
        "stat": parsed_stat,
        "memory": {
            "status_rss_bytes": (
                parse_status_kib(status_value, "VmRSS")
                if status_text["state"] == "observed"
                else status_text
            ),
            "status_rss_anon_bytes": (
                parse_status_kib(status_value, "RssAnon")
                if status_text["state"] == "observed"
                else status_text
            ),
            "status_swap_bytes": (
                parse_status_kib(status_value, "VmSwap")
                if status_text["state"] == "observed"
                else status_text
            ),
            "smaps_rss_bytes": (
                parse_smaps_kib(smaps_value, "Rss")
                if smaps_text["state"] == "observed"
                else smaps_text
            ),
            "smaps_anonymous_bytes": (
                parse_smaps_kib(smaps_value, "Anonymous")
                if smaps_text["state"] == "observed"
                else smaps_text
            ),
            "smaps_swap_bytes": (
                parse_smaps_kib(smaps_value, "Swap")
                if smaps_text["state"] == "observed"
                else smaps_text
            ),
        },
        "fds": sample_fds(pid),
        "threads": sample_threads(pid),
        "cgroup": sample_cgroup(pid),
    }


def _numeric(cell: Any) -> dict[str, Any] | None:
    if isinstance(cell, dict) and cell.get("state") == "observed":
        value = cell.get("value")
        if isinstance(value, (int, float)):
            return cell
    return None


def delta_cell(before: Any, after: Any) -> dict[str, Any]:
    left = _numeric(before)
    right = _numeric(after)
    if left is None or right is None:
        return unavailable("not_observed")
    return {
        "state": "delta",
        "value": right["value"] - left["value"],
        "before": left["value"],
        "after": right["value"],
    }


def delta_stat(before: dict[str, Any], after: dict[str, Any]) -> dict[str, Any]:
    if before.get("state") != "observed" or after.get("state") != "observed":
        return unavailable("not_observed")
    left = before["value"]
    right = after["value"]
    utime = right["utime"] - left["utime"]
    stime = right["stime"] - left["stime"]
    return observed(
        {
            "utime_ticks": utime,
            "stime_ticks": stime,
            "cpu_ticks": utime + stime,
            "minflt": right["minflt"] - left["minflt"],
            "majflt": right["majflt"] - left["majflt"],
        }
    )


def delta_io(before: dict[str, Any], after: dict[str, Any]) -> dict[str, Any]:
    if "_" in before or "_" in after:
        return unavailable("not_observed")
    return {key: delta_cell(before.get(key), after.get(key)) for key in before}


def delta_memory(before: dict[str, Any], after: dict[str, Any]) -> dict[str, Any]:
    return {key: delta_cell(before.get(key), after.get(key)) for key in before}


def delta_cgroup(before: dict[str, Any], after: dict[str, Any]) -> dict[str, Any]:
    if before.get("state") != "observed" or after.get("state") != "observed":
        return unavailable("not_observed")
    left = before["value"]
    right = after["value"]
    events_delta: dict[str, Any] = unavailable("not_observed")
    if (
        left["memory_events"].get("state") == "observed"
        and right["memory_events"].get("state") == "observed"
    ):
        events_delta = observed(
            {
                key: right["memory_events"]["value"].get(key, 0)
                - left["memory_events"]["value"].get(key, 0)
                for key in sorted(
                    set(left["memory_events"]["value"]) | set(right["memory_events"]["value"])
                )
            }
        )
    return {
        "memory_current_bytes": delta_cell(
            left.get("memory_current_bytes"), right.get("memory_current_bytes")
        ),
        "memory_events": events_delta,
    }


def cpu_percent(tick_delta: int, elapsed_ns: int, ticks: int) -> dict[str, Any]:
    if elapsed_ns <= 0:
        return unavailable("zero_elapsed")
    elapsed_s = elapsed_ns / 1_000_000_000
    return observed((tick_delta / (elapsed_s * ticks)) * 100.0)


def phase_delta(before: dict[str, Any], after: dict[str, Any]) -> dict[str, Any]:
    elapsed_ns = after["captured_ns"] - before["captured_ns"]
    stat_delta = delta_stat(before["stat"], after["stat"])
    ticks = before["clk_tck"]
    cpu = unavailable("not_observed")
    if stat_delta["state"] == "observed":
        cpu = cpu_percent(stat_delta["value"]["cpu_ticks"], elapsed_ns, ticks)
    return {
        "elapsed_ns": elapsed_ns,
        "io": delta_io(before["io"], after["io"]),
        "stat": stat_delta,
        "cpu_percent_from_proc": cpu,
        "memory": delta_memory(before["memory"], after["memory"]),
        "cgroup": delta_cgroup(before["cgroup"], after["cgroup"]),
        "fds": {
            "open_count": delta_cell(
                (
                    observed(before["fds"]["value"]["open_count"])
                    if before["fds"].get("state") == "observed"
                    else before["fds"]
                ),
                (
                    observed(after["fds"]["value"]["open_count"])
                    if after["fds"].get("state") == "observed"
                    else after["fds"]
                ),
            ),
            "after_by_family": (
                after["fds"]["value"]["by_family"]
                if after["fds"].get("state") == "observed"
                else after["fds"]
            ),
        },
        "threads": {
            "after_by_state": (
                after["threads"]["value"]["by_state"]
                if after["threads"].get("state") == "observed"
                else after["threads"]
            ),
            "blocked_io_count": (
                after["threads"]["value"]["blocked_io_count"]
                if after["threads"].get("state") == "observed"
                else after["threads"]
            ),
        },
    }


def redact_identity_path(raw: str | None) -> str | None:
    if not raw:
        return None
    name = posixpath.basename(raw.rstrip("/"))
    if SAFE_BASENAME.match(name) and name not in {".", ".."}:
        return name
    return f"digest:{path_digest(raw)}"


def contains_sensitive_path(value: Any) -> str | None:
    forbidden = (
        "/home/",
        "/Users/",
        "/root/",
        str(Path.home()),
    )
    env_keys = (
        "TRACEDECAY_HOME",
        "TRACEDECAY_PROFILE_DIR",
        "TRACEDECAY_DATA_DIR",
        "XDG_DATA_HOME",
    )
    extra = [os.environ[key] for key in env_keys if os.environ.get(key)]
    needles = [item for item in (*forbidden, *extra) if item]

    def walk(node: Any, trail: str) -> str | None:
        if isinstance(node, dict):
            if node.get("dir"):
                return trail + ".dir"
            for key, child in node.items():
                hit = walk(child, f"{trail}.{key}")
                if hit:
                    return hit
            return None
        if isinstance(node, list):
            for index, child in enumerate(node):
                hit = walk(child, f"{trail}[{index}]")
                if hit:
                    return hit
            return None
        if isinstance(node, str):
            for needle in needles:
                if needle and needle in node and node not in {"/usr/bin/perf", "/usr/bin/pidstat"}:
                    if node.startswith("/usr/") or node.startswith("/proc/"):
                        continue
                    return trail
        return None

    return walk(value, "$")


def load_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def cmd_snapshot(args: argparse.Namespace) -> int:
    snapshot = sample_proc(args.pid)
    snapshot["label"] = args.label
    if snapshot["stat"].get("state") != "observed":
        raise SystemExit(
            f"snapshot {args.label}: /proc/{args.pid}/stat unavailable; "
            "refusing to overwrite a live sample with a dead-PID read"
        )
    leak = contains_sensitive_path(snapshot)
    if leak:
        raise SystemExit(f"snapshot leaked a filesystem path at {leak}")
    write_json(Path(args.out), snapshot)
    return 0


def cmd_delta(args: argparse.Namespace) -> int:
    before = load_json(Path(args.before))
    after = load_json(Path(args.after))
    payload = phase_delta(before, after)
    leak = contains_sensitive_path(payload)
    if leak:
        raise SystemExit(f"delta leaked a filesystem path at {leak}")
    write_json(Path(args.out), payload)
    return 0


def cmd_assemble(args: argparse.Namespace) -> int:
    meta = load_json(Path(args.meta))
    phases: dict[str, Any] = {}
    for item in args.phase:
        if "=" not in item:
            raise SystemExit(f"phase must be name=path, got {item!r}")
        name, raw_path = item.split("=", 1)
        phases[name] = load_json(Path(raw_path))
    order = [name for name in ("before", "after_catch_up", "after_idle") if name in phases]
    deltas: dict[str, Any] = {}
    for left, right in zip(order, order[1:]):
        deltas[f"{left}_to_{right}"] = phase_delta(phases[left], phases[right])
    report = {
        "schema": SCHEMA,
        "identity": meta,
        "phases": phases,
        "deltas": deltas,
        "tools": load_json(Path(args.tools)) if args.tools else {},
        "sidecars": load_json(Path(args.sidecars)) if args.sidecars else {},
        "test_outcome": {
            "workload_exit": args.workload_exit,
            "status": (
                "passed"
                if args.workload_exit == 0
                else "failed"
                if args.workload_exit > 0
                else "harness_error"
            ),
        },
        "runtime_snapshot": (
            load_json(Path(args.runtime_snapshot)) if args.runtime_snapshot else None
        ),
    }
    leak = contains_sensitive_path(report)
    if leak:
        raise SystemExit(f"report leaked a filesystem path at {leak}")
    write_json(Path(args.out), report)
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    snapshot = sub.add_parser("snapshot", help="write one /proc+cgroup snapshot")
    snapshot.add_argument("--pid", type=int, required=True)
    snapshot.add_argument("--label", required=True)
    snapshot.add_argument("--out", required=True)
    snapshot.set_defaults(func=cmd_snapshot)

    delta = sub.add_parser("delta", help="subtract two snapshots")
    delta.add_argument("--before", required=True)
    delta.add_argument("--after", required=True)
    delta.add_argument("--out", required=True)
    delta.set_defaults(func=cmd_delta)

    assemble = sub.add_parser("assemble", help="write the durable run report")
    assemble.add_argument("--meta", required=True)
    assemble.add_argument("--phase", action="append", default=[])
    assemble.add_argument("--tools")
    assemble.add_argument("--sidecars")
    assemble.add_argument("--runtime-snapshot")
    assemble.add_argument("--workload-exit", type=int, required=True)
    assemble.add_argument("--out", required=True)
    assemble.set_defaults(func=cmd_assemble)

    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    raise SystemExit(main())
