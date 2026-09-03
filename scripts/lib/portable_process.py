#!/usr/bin/env python3
"""Portable Unix process controls for TraceDecay shell harnesses.

Only Python's standard library is used so the same process-tree and timeout
semantics are available on Linux and macOS without GNU coreutils or util-linux.
"""

from __future__ import annotations

import argparse
import errno
import math
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import time
from collections.abc import Sequence


def _positive_seconds(value: str) -> float:
    seconds = float(value)
    if not math.isfinite(seconds) or seconds <= 0:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return seconds


def _positive_pid(value: str) -> int:
    pid = int(value)
    if pid <= 0:
        raise argparse.ArgumentTypeError("must be a positive process ID")
    return pid


def _command_after_separator(command: Sequence[str]) -> list[str]:
    result = list(command)
    if result[:1] == ["--"]:
        result.pop(0)
    if not result:
        raise ValueError("a command is required after --")
    return result


def _group_alive(pid: int) -> bool:
    try:
        os.killpg(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        # Some BSD kernels return EPERM for a group whose only remaining
        # member is a zombie.  Permission confirms that the group exists,
        # not that it still contains work capable of running.
        pass
    return _group_has_runnable_member(pid)


def _group_has_runnable_member(pgid: int) -> bool:
    """Whether the group still holds a member that can execute.

    killpg(pgid, 0) keeps succeeding while an exited leader is an unreaped
    zombie of the calling shell — a process this helper can never reap.
    Counting that zombie as live would burn the entire stop grace period and
    misreport a graceful shutdown as forced cleanup.
    """
    proc_root = Path("/proc")
    if proc_root.is_dir():
        for entry in proc_root.iterdir():
            if not entry.name.isdigit():
                continue
            try:
                stat_fields = (
                    (entry / "stat").read_text().rsplit(")", 1)[1].split()
                )
            except (OSError, IndexError):
                continue
            # Fields after the command name: state, ppid, pgrp, ...
            if (
                len(stat_fields) >= 3
                and stat_fields[2] == str(pgid)
                and stat_fields[0] != "Z"
            ):
                return True
        return False
    try:
        listing = subprocess.run(
            ["ps", "-A", "-o", "pgid=", "-o", "state="],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError):
        # Without process states we cannot tell zombies apart; keep the
        # signal-based answer rather than fabricating a shutdown.
        return True
    for line in listing.splitlines():
        columns = line.split()
        if len(columns) >= 2 and columns[0] == str(pgid) and not columns[1].startswith("Z"):
            return True
    return False


def _signal_group(pid: int, sig: signal.Signals) -> None:
    try:
        os.killpg(pid, sig)
    except ProcessLookupError:
        pass


def _stop_group(pid: int, grace_seconds: float) -> bool:
    """Stop a process group, returning True when SIGKILL was required."""

    if not _group_alive(pid):
        return False

    _signal_group(pid, signal.SIGTERM)
    deadline = time.monotonic() + grace_seconds
    while _group_alive(pid) and time.monotonic() < deadline:
        time.sleep(min(0.05, max(0.0, deadline - time.monotonic())))

    forced = _group_alive(pid)
    if forced:
        _signal_group(pid, signal.SIGKILL)
    return forced


def _return_code(status: int) -> int:
    return status if status >= 0 else 128 + abs(status)


def command_exec_session(args: argparse.Namespace) -> int:
    command = _command_after_separator(args.command)
    try:
        os.setsid()
    except PermissionError:
        # A process that is already its own process-group leader cannot call
        # setsid(), but its negative PID still addresses the owned group.
        if os.getpgrp() != os.getpid():
            raise
    os.execvp(command[0], command)
    return 127


def command_run(args: argparse.Namespace) -> int:
    command = _command_after_separator(args.command)
    process = subprocess.Popen(command, start_new_session=True)
    interrupted_by: int | None = None

    def handle_signal(signum: int, _frame: object) -> None:
        nonlocal interrupted_by
        interrupted_by = signum

    previous_handlers = {
        sig: signal.signal(sig, handle_signal)
        for sig in (signal.SIGINT, signal.SIGTERM)
    }
    deadline = time.monotonic() + args.timeout
    timed_out = False
    try:
        while process.poll() is None:
            if interrupted_by is not None:
                break
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                timed_out = True
                break
            try:
                process.wait(timeout=min(0.1, remaining))
            except subprocess.TimeoutExpired:
                pass

        if timed_out or interrupted_by is not None:
            _stop_group(process.pid, args.kill_after)
        elif _group_alive(process.pid):
            # The leader may have exited after launching background work.
            # A bounded smoke phase owns and cleans its entire process group.
            _stop_group(process.pid, args.kill_after)

        try:
            status = process.wait(timeout=args.kill_after)
        except subprocess.TimeoutExpired:
            _signal_group(process.pid, signal.SIGKILL)
            status = process.wait()

        if interrupted_by is not None:
            return 128 + interrupted_by
        if timed_out:
            return 124
        return _return_code(status)
    finally:
        for sig, handler in previous_handlers.items():
            signal.signal(sig, handler)


def command_group_alive(args: argparse.Namespace) -> int:
    return 0 if _group_alive(args.pid) else 1


def command_stop_group(args: argparse.Namespace) -> int:
    return 2 if _stop_group(args.pid, args.grace) else 0


def command_wait_unix_socket(args: argparse.Namespace) -> int:
    deadline = time.monotonic() + args.timeout
    while time.monotonic() < deadline:
        try:
            os.kill(args.pid, 0)
        except ProcessLookupError:
            print(
                "error: tracedecay daemon exited before becoming ready", file=sys.stderr
            )
            return 1
        except PermissionError:
            pass

        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.settimeout(min(0.25, max(0.01, deadline - time.monotonic())))
        try:
            client.connect(args.path)
        except OSError:
            time.sleep(min(0.1, max(0.0, deadline - time.monotonic())))
        else:
            return 0
        finally:
            client.close()

    print(
        f"error: tracedecay daemon did not become ready within {args.timeout:g} seconds",
        file=sys.stderr,
    )
    return 1


def command_monotonic_ms(_args: argparse.Namespace) -> int:
    print(time.monotonic_ns() // 1_000_000)
    return 0


def command_realpath(args: argparse.Namespace) -> int:
    try:
        print(Path(args.path).resolve(strict=True))
    except (FileNotFoundError, OSError) as error:
        detail = error.strerror if getattr(error, "strerror", None) else str(error)
        print(f"error: cannot resolve path '{args.path}': {detail}", file=sys.stderr)
        return errno.ENOENT
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="action", required=True)

    exec_session = subparsers.add_parser("exec-session")
    exec_session.add_argument("command", nargs=argparse.REMAINDER)
    exec_session.set_defaults(func=command_exec_session)

    run = subparsers.add_parser("run")
    run.add_argument("--timeout", required=True, type=_positive_seconds)
    run.add_argument("--kill-after", required=True, type=_positive_seconds)
    run.add_argument("command", nargs=argparse.REMAINDER)
    run.set_defaults(func=command_run)

    group_alive = subparsers.add_parser("group-alive")
    group_alive.add_argument("--pid", required=True, type=_positive_pid)
    group_alive.set_defaults(func=command_group_alive)

    stop_group = subparsers.add_parser("stop-group")
    stop_group.add_argument("--pid", required=True, type=_positive_pid)
    stop_group.add_argument("--grace", required=True, type=_positive_seconds)
    stop_group.set_defaults(func=command_stop_group)

    wait_socket = subparsers.add_parser("wait-unix-socket")
    wait_socket.add_argument("--path", required=True)
    wait_socket.add_argument("--pid", required=True, type=_positive_pid)
    wait_socket.add_argument("--timeout", required=True, type=_positive_seconds)
    wait_socket.set_defaults(func=command_wait_unix_socket)

    monotonic_ms = subparsers.add_parser("monotonic-ms")
    monotonic_ms.set_defaults(func=command_monotonic_ms)

    realpath = subparsers.add_parser("realpath")
    realpath.add_argument("path")
    realpath.set_defaults(func=command_realpath)
    return parser


def main() -> int:
    args = build_parser().parse_args()
    try:
        return args.func(args)
    except (ValueError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 127 if getattr(error, "errno", None) == errno.ENOENT else 126


if __name__ == "__main__":
    raise SystemExit(main())
