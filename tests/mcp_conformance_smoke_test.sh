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
  tools/call:tracedecay_diagnostics | tools/call:tracedecay_affected | tools/call:tracedecay_test_map)
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
      echo "transient graph admission failure" >&2
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
chmod +x "$fake_bin/tracedecay" "$fake_bin/npx"

gate_run env \
  PATH="$fake_bin:$PATH" \
  TRACEDECAY_BIN="$fake_bin/tracedecay" \
  INSPECTOR_VERSION=0.22.0 \
  CALL_TIMEOUT_SECS=5 \
  FAKE_IMPACT_ATTEMPTS="$impact_attempts" \
  "$SCRIPT" --run "$work_dir" "$fixture"
gate_expect_success "transient impact admission"
gate_output_contains "transient impact admission" \
  "ok   tools/call tracedecay_impact returns typed evidence"

if [[ $(<"$impact_attempts") != 2 ]]; then
  echo "$GATE_OUTPUT" >&2
  gate_fail "transient impact admission: expected exactly two impact attempts"
fi
