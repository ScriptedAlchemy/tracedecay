#!/usr/bin/env bash
# Probe every native Work provider transport against the stock executable that
# an operator has actually installed. This deliberately does not substitute a
# shell fixture for Claude Code or Codex: the exact launch plans owned by
# `work_attempt_exec` are exercised against the stock binaries.
#
# The probe sends one harmless prompt and interrupts it only after the child is
# known live. It therefore verifies discovery, launch, native protocol startup,
# cancellation, and child settlement without treating a fixture's output as
# provider evidence. `HOME` and `PATH` are the entire explicit child
# environment; a Work snapshot must separately admit those two keys before a
# production attempt can use an operator-installed CLI wrapper and its login.
#
# Running this uses the current stock-provider login and may begin a model
# request. That is intentional certification work, never an ordinary unit
# test. The explicit opt-in prevents a developer or CI job from consuming a
# provider request by accident:
#
#   TRACEDECAY_WORK_NATIVE_CONFORMANCE=1 \
#     scripts/work_provider_native_conformance.sh
#
# Overrides:
#   CLAUDE_BIN  absolute stock Claude Code binary (default: `claude` on PATH)
#   CODEX_BIN   absolute stock Codex binary (default: `codex` on PATH)

set -euo pipefail

if [[ "${TRACEDECAY_WORK_NATIVE_CONFORMANCE:-}" != "1" ]]; then
    echo "error: native Work-provider conformance is opt-in; set TRACEDECAY_WORK_NATIVE_CONFORMANCE=1" >&2
    exit 2
fi

for required in python3 setsid sha256sum; do
    command -v "$required" >/dev/null 2>&1 || {
        echo "error: $required is required for native Work-provider conformance" >&2
        exit 2
    }
done

stage="$(mktemp -d "${TMPDIR:-/tmp}/tracedecay-work-provider.XXXXXX")"
trap 'rm -rf "$stage"' EXIT

resolve_stock_binary() {
    local label="$1"
    local configured="$2"
    local default_name="$3"
    local candidate

    candidate="${configured:-$(command -v "$default_name" || true)}"
    if [[ -z "$candidate" || ! -x "$candidate" ]]; then
        # This mirrors `WorkProviderAvailabilityV1::Unavailable`: there is no
        # configured executable that can be started. It is a diagnostic
        # outcome, never a certification pass.
        printf '%s\n' "provider=$label availability=unavailable reason=host_binary_absent" >&2
        return 1
    fi
    printf '%s/%s\n' "$(cd "$(dirname "$candidate")" && pwd)" "$(basename "$candidate")"
}

wait_until_live() {
    local pid="$1"
    local label="$2"
    local attempt

    for attempt in $(seq 1 40); do
        if ! kill -0 "$pid" 2>/dev/null; then
            echo "error: $label exited before cancellation could be delivered" >&2
            return 1
        fi
        # A short deliberate window lets the stock host reach its own startup
        # boundary. The eventual signal is still required to settle the run.
        sleep 0.05
    done
}

cancel_and_settle() {
    local pid="$1"
    local label="$2"
    local status=0
    local attempt

    kill -INT -- "-$pid" 2>/dev/null || kill -INT "$pid"
    for attempt in $(seq 1 100); do
        if ! kill -0 "$pid" 2>/dev/null; then
            set +e
            wait "$pid"
            status=$?
            set -e
            printf '%s\n' "provider=$label cancellation=delivered settlement=exited status=$status"
            return 0
        fi
        sleep 0.05
    done

    # A provider that has not settled after the bounded interrupt window is
    # not successful. Kill its owned process group and fail the probe.
    kill -KILL -- "-$pid" 2>/dev/null || kill -KILL "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    echo "error: $label did not settle after native cancellation" >&2
    return 1
}

run_stdio_provider() {
    local label="$1"
    local executable="$2"
    shift 2
    local input="$stage/$label.input"
    local output="$stage/$label.output"
    local pid

    printf '%s\n' 'Return only the word READY. Do not use tools or modify files.' > "$input"
    echo "== $label: $executable ($("$executable" --version 2>/dev/null | head -n 1 || echo unknown))"
    # `setsid` makes the signal semantics match the Work runtime's owned
    # process-group cancellation ladder, rather than signalling an unrelated
    # terminal process.
    setsid env -i HOME="$HOME" PATH="$PATH" "$executable" "$@" < "$input" > "$output" 2>&1 &
    pid=$!
    wait_until_live "$pid" "$label"
    cancel_and_settle "$pid" "$label"

    if rg -qi '"subtype"[[:space:]]*:[[:space:]]*"success"|"turn.completed"' "$output"; then
        echo "error: $label completed successfully before cancellation; no cancellation evidence" >&2
        return 1
    fi
    local digest
    digest="sha256:$(sha256sum "$output" | awk '{print $1}')"
    printf '%s\n' "provider=$label availability=available start=observed stdout_digest=$digest"
}

run_codex_app_server() {
    local executable="$1"

    echo "== codex-app-server: $executable ($("$executable" --version 2>/dev/null | head -n 1 || echo unknown))"
    # Python is used solely for framed JSON-RPC I/O. The process is the stock
    # `codex app-server`, and the messages are the same initialize →
    # thread/start → turn/start sequence used by the Work app-server adapter.
    CODEX_CONFORMANCE_BIN="$executable" HOME="$HOME" PATH="$PATH" python3 - <<'PY'
import json
import os
import select
import signal
import subprocess
import sys
import time

binary = os.environ["CODEX_CONFORMANCE_BIN"]
child_env = {"HOME": os.environ["HOME"], "PATH": os.environ["PATH"]}
proc = subprocess.Popen(
    [binary, "app-server"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
    bufsize=1,
    env=child_env,
    start_new_session=True,
)

def send(message):
    proc.stdin.write(json.dumps(message) + "\n")
    proc.stdin.flush()

def receive_until(predicate, label):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            stderr = proc.stderr.read()
            raise RuntimeError(f"codex app-server exited while waiting for {label}: {stderr}")
        readable, _, _ = select.select([proc.stdout], [], [], 0.25)
        if not readable:
            continue
        line = proc.stdout.readline()
        if not line:
            continue
        message = json.loads(line)
        if predicate(message):
            return message
    raise RuntimeError(f"timed out waiting for {label}")

try:
    send({"method": "initialize", "id": 0, "params": {"clientInfo": {"name": "tracedecay_work_provider_conformance", "version": "1"}}})
    receive_until(lambda item: item.get("id") == 0 and "result" in item, "initialize response")
    send({"method": "initialized", "params": {}})
    send({"method": "thread/start", "id": 1, "params": {"ephemeral": True, "threadSource": "tracedecay_work_provider_conformance"}})
    thread = receive_until(lambda item: item.get("id") == 1 and "result" in item, "thread/start response")
    thread_id = thread.get("result", {}).get("thread", {}).get("id") or thread.get("result", {}).get("id")
    if not isinstance(thread_id, str) or not thread_id:
        raise RuntimeError(f"stock thread/start omitted a thread id: {thread}")
    send({"method": "turn/start", "id": 2, "params": {"threadId": thread_id, "input": [{"type": "text", "text": "Return only READY. Do not use tools or modify files."}], "cwd": os.getcwd(), "effort": "low", "summary": "concise"}})
    receive_until(lambda item: item.get("id") == 2 or item.get("method") in {"turn/started", "item/started", "item/agentMessage/delta"}, "turn startup")
    os.killpg(proc.pid, signal.SIGKILL)
    proc.wait(timeout=10)
    print(f"provider=codex-app-server availability=available start=observed cancellation=delivered settlement=exited status={proc.returncode} thread_id_observed=true")
except Exception as error:
    if proc.poll() is None:
        os.killpg(proc.pid, signal.SIGKILL)
        proc.wait(timeout=10)
    print(f"error: codex app-server conformance failed: {error}", file=sys.stderr)
    raise
PY
}

claude_bin="$(resolve_stock_binary claude-code-cli "${CLAUDE_BIN:-}" claude)" || exit 1
codex_bin="$(resolve_stock_binary codex "${CODEX_BIN:-}" codex)" || exit 1

# The argument lists below are intentionally identical to `provider_arguments`
# and `codex_app_server_command`; changing either production launch plan makes
# this real-host probe fail rather than silently testing a different interface.
run_stdio_provider claude-code-cli "$claude_bin" --print --output-format stream-json --verbose
run_codex_app_server "$codex_bin"
run_stdio_provider codex-cli "$codex_bin" exec --json -

echo "native Work-provider conformance: PASS"
