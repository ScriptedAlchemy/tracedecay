#!/usr/bin/env python3
"""Opt-in Codex app-server launch/start/signal smoke, without Work receipts."""

import json
import os
import select
import signal
import subprocess
import sys
import time


def response_result(message, request_id):
    if not isinstance(message, dict):
        raise ValueError("app-server message is not an object")
    if type(message.get("id")) is not int or message["id"] != request_id:
        return None
    if "error" in message or "method" in message:
        raise ValueError("app-server request failed or returned a request instead of a response")
    result = message.get("result")
    if not isinstance(result, dict):
        raise ValueError("app-server response omitted its result object")
    return result


def thread_start_response(message):
    result = response_result(message, 1)
    if result is None:
        return None
    thread = result.get("thread")
    thread_id = thread.get("id") if isinstance(thread, dict) else None
    if not isinstance(thread_id, str) or not thread_id.strip():
        raise ValueError("thread/start omitted its thread identity")
    return thread_id


def turn_start_response(message, thread_id):
    result = response_result(message, 2)
    if result is None:
        return None
    turn = result.get("turn")
    if not isinstance(turn, dict):
        raise ValueError("turn/start omitted its turn")
    turn_id = turn.get("id")
    if not isinstance(turn_id, str) or not turn_id.strip():
        raise ValueError("turn/start omitted its turn identity")
    if turn.get("status") != "inProgress" or turn.get("error") is not None:
        raise ValueError("turn/start did not accept an active turn")
    for owner in (result, turn):
        if "threadId" in owner and owner["threadId"] != thread_id:
            raise ValueError("turn/start named a different thread")
    # Request 2 was sent for this thread. Unsolicited turn/item notifications
    # cannot acknowledge that request, regardless of their thread/turn IDs.
    return turn_id


def _receive_until(stdout, parse, label):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        readable, _, _ = select.select([stdout], [], [], 0.25)
        if not readable:
            continue
        line = stdout.readline()
        if not line:
            raise RuntimeError(f"app-server closed stdout while waiting for {label}")
        value = parse(json.loads(line))
        if value is not None:
            return value
    raise RuntimeError(f"timed out waiting for {label}")


def run_app_server(binary, cwd):
    proc = subprocess.Popen(
        [binary, "app-server"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        # Unbuffered stdout reads one frame per readline(). A buffered reader
        # would keep later frames from the same pipe write where select()
        # cannot see them.
        bufsize=0,
        env={"HOME": os.environ["HOME"], "PATH": os.environ["PATH"]},
        cwd=cwd,
        start_new_session=True,
    )

    def send(message):
        proc.stdin.write((json.dumps(message) + "\n").encode())

    try:
        send({"method": "initialize", "id": 0, "params": {
            "clientInfo": {"name": "tracedecay_native_launch_smoke", "version": "1"},
        }})
        _receive_until(proc.stdout, lambda item: response_result(item, 0), "initialize response")
        send({"method": "initialized", "params": {}})
        send({"method": "thread/start", "id": 1, "params": {
            "ephemeral": True, "threadSource": "tracedecay_native_launch_smoke",
            "cwd": cwd, "approvalPolicy": "never", "sandbox": "read-only",
        }})
        thread_id = _receive_until(proc.stdout, thread_start_response, "thread/start response")
        send({"method": "turn/start", "id": 2, "params": {
            "threadId": thread_id,
            "input": [{"type": "text", "text": "Return only READY."}],
            "cwd": cwd, "effort": "low", "summary": "concise",
            "approvalPolicy": "never",
            "sandboxPolicy": {"type": "readOnly", "networkAccess": False},
        }})
        _receive_until(proc.stdout, lambda item: turn_start_response(item, thread_id), "turn/start response")
        if proc.poll() is not None:
            raise RuntimeError("app-server exited before the signal could be delivered")
        os.killpg(proc.pid, signal.SIGKILL)
        proc.wait(timeout=10)
        print("provider=codex-app-server scope=launch-and-signal availability=available "
              f"turn_start=accepted cancellation_signal=delivered process_exit=observed status={proc.returncode}")
    finally:
        if proc.poll() is None:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait(timeout=10)
        for stream in (proc.stdin, proc.stdout, proc.stderr):
            stream.close()


def main():
    if os.environ.get("TRACEDECAY_WORK_NATIVE_SMOKE") != "1":
        print("error: native provider launch smoke requires TRACEDECAY_WORK_NATIVE_SMOKE=1", file=sys.stderr)
        return 2
    if len(sys.argv) != 3:
        print("usage: work_provider_native_smoke.py <codex-binary> <disposable-directory>", file=sys.stderr)
        return 2
    try:
        run_app_server(sys.argv[1], os.path.abspath(sys.argv[2]))
    except (OSError, RuntimeError, ValueError, subprocess.TimeoutExpired) as error:
        print(f"error: codex app-server launch smoke failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
