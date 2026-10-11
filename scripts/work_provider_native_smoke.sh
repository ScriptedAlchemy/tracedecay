#!/usr/bin/env bash
# Opt-in launch/signal smoke against operator-installed Claude and Codex.
# App-server startup requires a successful turn/start reply. CLI checks only
# establish that the owned process remained live until it was signalled.
# These independent launches produce no Work admission or terminal receipts.
#
# Uses the operator's provider login and may start a model request. The prompt
# alone cannot disable CLI tools. Run only deliberately in this disposable cwd:
#   TRACEDECAY_WORK_NATIVE_SMOKE=1 scripts/work_provider_native_smoke.sh
# Overrides: CLAUDE_BIN / CODEX_BIN name installed stock executables.

set -euo pipefail

if [[ "${TRACEDECAY_WORK_NATIVE_SMOKE:-}" != "1" ]]; then
    echo "error: native provider launch smoke is opt-in; set TRACEDECAY_WORK_NATIVE_SMOKE=1" >&2
    exit 2
fi

for required in python3 setsid sha256sum rg; do
    command -v "$required" >/dev/null 2>&1 || {
        echo "error: $required is required for native provider launch smoke" >&2
        exit 2
    }
done

script_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

stage="$(mktemp -d "${TMPDIR:-/tmp}/tracedecay-work-provider.XXXXXX")"
active_pid=""
cleanup() {
    if [[ -n "$active_pid" ]]; then
        kill -KILL -- "-$active_pid" 2>/dev/null || kill -KILL "$active_pid" 2>/dev/null || true
        wait "$active_pid" 2>/dev/null || true
    fi
    rm -rf "$stage"
}
trap cleanup EXIT

resolve_stock_binary() {
    local label="$1"
    local configured="$2"
    local default_name="$3"
    local candidate

    candidate="${configured:-$(command -v "$default_name" || true)}"
    if [[ -z "$candidate" || ! -x "$candidate" ]]; then
        printf '%s\n' "provider=$label availability=unavailable reason=host_binary_absent" >&2
        return 1
    fi
    printf '%s/%s\n' "$(cd "$(dirname "$candidate")" && pwd)" "$(basename "$candidate")"
}

wait_until_live() {
    local pid="$1"
    local label="$2"
    local attempt

    for ((attempt = 0; attempt < 40; attempt++)); do
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
    for ((attempt = 0; attempt < 100; attempt++)); do
        if ! kill -0 "$pid" 2>/dev/null; then
            set +e
            wait "$pid"
            status=$?
            set -e
            printf '%s\n' "provider=$label cancellation_signal=delivered process_exit=observed status=$status"
            return 0
        fi
        sleep 0.05
    done

    # A provider that has not settled after the bounded interrupt window is
    # not successful. Kill its owned process group and fail the probe.
    kill -KILL -- "-$pid" 2>/dev/null || kill -KILL "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    active_pid=""
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
    (cd "$stage" && exec setsid env -i HOME="$HOME" PATH="$PATH" "$executable" "$@") < "$input" > "$output" 2>&1 &
    pid=$!
    active_pid="$pid"
    wait_until_live "$pid" "$label"
    cancel_and_settle "$pid" "$label"
    active_pid=""

    if rg -qi '"subtype"[[:space:]]*:[[:space:]]*"success"|"turn.completed"' "$output"; then
        echo "error: $label completed successfully before cancellation; no cancellation evidence" >&2
        return 1
    fi
    local digest
    digest="sha256:$(sha256sum "$output" | awk '{print $1}')"
    printf '%s\n' "provider=$label scope=launch-and-signal availability=available process_start=observed stdout_digest=$digest"
}

run_codex_app_server() {
    local executable="$1"

    echo "== codex-app-server: $executable ($("$executable" --version 2>/dev/null | head -n 1 || echo unknown))"
    python3 "$script_root/work_provider_native_smoke.py" "$executable" "$stage"
}

claude_bin="$(resolve_stock_binary claude-code-cli "${CLAUDE_BIN:-}" claude)" || exit 1
codex_bin="$(resolve_stock_binary codex "${CODEX_BIN:-}" codex)" || exit 1

# These stock CLI arguments are a manual smoke scope, independent of Work admission.
run_stdio_provider claude-code-cli "$claude_bin" --print --output-format stream-json --verbose
run_codex_app_server "$codex_bin"
run_stdio_provider codex-cli "$codex_bin" exec --json -

echo "native provider launch smoke: PASS"
