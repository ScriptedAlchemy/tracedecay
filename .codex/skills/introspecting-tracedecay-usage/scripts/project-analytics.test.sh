#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HELPER="$SCRIPT_DIR/project-analytics.sh"
TEST_ROOT="$(mktemp -d)"
trap 'rm -rf "$TEST_ROOT"' EXIT

fail() {
  printf 'not ok - %s\n' "$1" >&2
  exit 1
}

assert_contains() {
  case "$1" in
    *"$2"*) ;;
    *) fail "expected output to contain: $2" ;;
  esac
}

assert_excludes() {
  case "$1" in
    *"$2"*) fail "expected output to exclude: $2" ;;
    *) ;;
  esac
}

FAKE_BIN="$TEST_ROOT/bin"
PROFILE_ROOT="$TEST_ROOT/profile"
SERVING_DB="$PROFILE_ROOT/projects/proj_current/graph.db"
mkdir -p "$FAKE_BIN" "$(dirname "$SERVING_DB")"
: >"$SERVING_DB"
: >"$PROFILE_ROOT/global.db"

cat >"$FAKE_BIN/tracedecay" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '{
  "status":"ok",
  "read_only":false,
  "database_bytes":4096,
  "page_size_bytes":4096,
  "page_count":1,
  "freelist_pages":0,
  "details":[],
  "project_id":"proj_current",
  "store_path":"%s",
  "history":[],
  "history_coverage":"durable_project_store_history"
}\n' "$SERVING_DB"
EOF

cat >"$FAKE_BIN/sqlite3" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
database="$4"
query="${*: -1}"
[[ -f "$database" ]] || exit 92
case "$query" in
  *"GROUP BY tool_name"*)
    case "$query" in
      *"project_id='proj_current'"*) ;;
      *) exit 91 ;;
    esac
    printf 'tracedecay_status  3  1\n'
    ;;
  *"COUNT(*) FROM analytics_events"*) printf '3\n' ;;
  *"FROM memory_facts"*) printf '2  4  1  1  0  1  2\n' ;;
  *"FROM memory_feedback_events"*) ;;
  *"FROM memory_oplog GROUP BY"*) printf '  add: 2\n' ;;
  *"SUM(op IN"*) printf '2\n' ;;
  *)
    printf 'unexpected query: %s\n' "$query" >&2
    exit 90
    ;;
esac
EOF

chmod +x "$FAKE_BIN/tracedecay" "$FAKE_BIN/sqlite3"

output="$({
  PATH="$FAKE_BIN:$PATH" SERVING_DB="$SERVING_DB" "$HELPER"
} 2>&1)"

assert_contains "$output" "TraceDecay usage & fact-store adoption — proj_current"
assert_contains "$output" "serving store: graph.db"
assert_contains "$output" "total mcp_tool_call events: 3"
assert_contains "$output" "facts stored:              2"
assert_excludes "$output" "KeyError"

printf 'ok - beta.11 storage-status shape drives the analytics snapshot\n'

rm "$SERVING_DB"
set +e
unavailable_output="$({
  PATH="$FAKE_BIN:$PATH" SERVING_DB="$SERVING_DB" "$HELPER"
} 2>&1)"
unavailable_status=$?
set -e

if [ "$unavailable_status" -eq 0 ]; then
  fail "an unavailable serving store must fail"
fi
assert_contains "$unavailable_output" "serving store does not exist"
assert_excludes "$unavailable_output" "facts stored:"

printf 'ok - unavailable serving store fails before printing adoption counts\n'
