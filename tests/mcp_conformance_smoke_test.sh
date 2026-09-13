#!/usr/bin/env bash
set -euo pipefail

# shellcheck source=../scripts/lib/gate-test.sh
# shellcheck disable=SC1091
. "$(dirname "${BASH_SOURCE[0]}")/../scripts/lib/gate-test.sh"

SCRIPT="$GATE_REPO_ROOT/scripts/mcp-conformance-smoke.sh"
fake_bin="$GATE_SCRATCH/bin"
work_dir="$GATE_SCRATCH/work"
fixture="$GATE_SCRATCH/fixture"
impact_attempts="$GATE_SCRATCH/impact-attempts"
test_map_attempts="$GATE_SCRATCH/test-map-attempts"
test_map_timeouts="$GATE_SCRATCH/test-map-timeouts"
terminal_test_map_attempts="$GATE_SCRATCH/terminal-test-map-attempts"
mkdir -p "$fake_bin" "$work_dir" "$fixture"

cat >"$fake_bin/tracedecay" <<'SH'
#!/usr/bin/env bash
exit 0
SH

cat >"$fake_bin/npx" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

method=""
tool=""
while (($# > 0)); do
  case "$1" in
    --method)
      method="$2"
      shift 2
      ;;
    --tool-name)
      tool="$2"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done

case "$method:$tool" in
  tools/list:)
    printf '%s\n' '{"tools":[{"name":"tracedecay_search","inputSchema":{"type":"object"}},{"name":"tracedecay_diagnostics","inputSchema":{"type":"object"}},{"name":"tracedecay_impact","inputSchema":{"type":"object"}},{"name":"tracedecay_affected","inputSchema":{"type":"object"}},{"name":"tracedecay_test_map","inputSchema":{"type":"object"}},{"name":"tracedecay_find_exact_symbol","inputSchema":{"type":"object"}}]}'
    ;;
  tools/call:tracedecay_find_exact_symbol)
    printf '%s\n' '{"content":[{"type":"text","text":"{\"count\":1,\"matches\":[{\"id\":\"symbol.v1.sha256:fixture\"}]}"}]}'
    ;;
  tools/call:tracedecay_search)
    printf '%s\n' '{"content":[{"type":"text","text":"Search Results: main"}]}'
    ;;
  tools/call:tracedecay_diagnostics | tools/call:tracedecay_affected)
    printf '%s\n' '{"content":[{"type":"text","text":"typed evidence"}]}'
    ;;
  tools/call:tracedecay_test_map)
    attempts=0
    if [[ -f "$FAKE_TEST_MAP_ATTEMPTS" ]]; then
      attempts=$(<"$FAKE_TEST_MAP_ATTEMPTS")
    fi
    attempts=$((attempts + 1))
    printf '%s\n' "$attempts" >"$FAKE_TEST_MAP_ATTEMPTS"
    if [[ ${FAKE_TEST_MAP_TERMINAL:-0} == 1 ]]; then
      echo "Failed to call tool tracedecay_test_map: MCP error -32602: tool project route failed: reason_code=code-graph-invalid-request retryable=false: invalid test-map arguments" >&2
      exit 1
    fi
    if ((attempts == 1)); then
      echo "Failed to call tool tracedecay_test_map: MCP error -32603: tool project route failed: reason_code=code-graph-unavailable retryable=true: the verified code graph is not ready for the exact project root" >&2
      exit 1
    fi
    printf '%s\n' '{"content":[{"type":"text","text":"typed evidence"}]}'
    ;;
  tools/call:tracedecay_impact)
    attempts=0
    if [[ -f "$FAKE_IMPACT_ATTEMPTS" ]]; then
      attempts=$(<"$FAKE_IMPACT_ATTEMPTS")
    fi
    attempts=$((attempts + 1))
    printf '%s\n' "$attempts" >"$FAKE_IMPACT_ATTEMPTS"
    if ((attempts == 1)); then
      echo "Failed to call tool tracedecay_impact: MCP error -32603: tool project route failed: reason_code=code-graph-stale retryable=true: the exact graph generation is changing" >&2
      exit 1
    fi
    printf '%s\n' '{"content":[{"type":"text","text":"{\"node_count\":1}"}]}'
    ;;
  resources/list:)
    printf '%s\n' '{"resources":[{"uri":"tracedecay://status"}]}'
    ;;
  tools/call:definitely_not_a_tool)
    exit 1
    ;;
  *)
    echo "unexpected inspector invocation: method=$method tool=$tool" >&2
    exit 1
    ;;
esac
SH

cat >"$fake_bin/timeout" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
for argument in "$@"; do
  if [[ "$argument" == tracedecay_test_map ]]; then
    printf '%s\n' "$1" >>"$FAKE_TEST_MAP_TIMEOUTS"
    break
  fi
done
exec /usr/bin/timeout "$@"
SH
chmod +x "$fake_bin/tracedecay" "$fake_bin/npx" "$fake_bin/timeout"

gate_run env \
  PATH="$fake_bin:$PATH" \
  TRACEDECAY_BIN="$fake_bin/tracedecay" \
  INSPECTOR_VERSION=0.22.0 \
  CALL_TIMEOUT_SECS=5 \
  FAKE_IMPACT_ATTEMPTS="$impact_attempts" \
  FAKE_TEST_MAP_ATTEMPTS="$test_map_attempts" \
  FAKE_TEST_MAP_TIMEOUTS="$test_map_timeouts" \
  "$SCRIPT" --run "$work_dir" "$fixture"
gate_expect_success "transient impact admission"
gate_output_contains "transient impact admission" \
  "ok   tools/call tracedecay_impact returns typed evidence"

if [[ $(<"$impact_attempts") != 2 ]]; then
  echo "$GATE_OUTPUT" >&2
  gate_fail "transient impact admission: expected exactly two impact attempts"
fi

if [[ $(<"$test_map_attempts") != 2 ]]; then
  echo "$GATE_OUTPUT" >&2
  gate_fail "transient test-map admission: expected exactly two attempts"
fi

if [[ $(<"$test_map_timeouts") != $'5\n4' ]]; then
  echo "$GATE_OUTPUT" >&2
  gate_fail "transient test-map admission: attempts must consume one shared timeout budget"
fi

gate_run env \
  PATH="$fake_bin:$PATH" \
  TRACEDECAY_BIN="$fake_bin/tracedecay" \
  INSPECTOR_VERSION=0.22.0 \
  CALL_TIMEOUT_SECS=5 \
  FAKE_IMPACT_ATTEMPTS="$impact_attempts" \
  FAKE_TEST_MAP_ATTEMPTS="$terminal_test_map_attempts" \
  FAKE_TEST_MAP_TIMEOUTS="$test_map_timeouts" \
  FAKE_TEST_MAP_TERMINAL=1 \
  "$SCRIPT" --run "$work_dir" "$fixture"
gate_expect_failure "terminal test-map error"
gate_output_contains "terminal test-map error" \
  "reason_code=code-graph-invalid-request retryable=false"

if [[ $(<"$terminal_test_map_attempts") != 1 ]]; then
  echo "$GATE_OUTPUT" >&2
  gate_fail "terminal test-map error: expected exactly one attempt"
fi
