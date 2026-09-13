#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HELPER="$SCRIPT_DIR/friction-scan.sh"
MIRROR="$SCRIPT_DIR/../../../../.claude/skills/self-improving-from-usage-logs/scripts/friction-scan.sh"
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

FAKE_BIN="$TEST_ROOT/bin"
PROFILE_ROOT="$TEST_ROOT/profile"
SERVING_DB="$PROFILE_ROOT/projects/proj_current/tracedecay.db"
GLOBAL_DB="$PROFILE_ROOT/global.db"
mkdir -p "$FAKE_BIN" "$(dirname "$SERVING_DB")"
: >"$SERVING_DB"
: >"$GLOBAL_DB"

cat >"$FAKE_BIN/tracedecay" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '{"outcome":{"outcome":"evidence","value":{"payload":{"project_id":"proj_current","store_path":"%s"}}}}\n' "$SERVING_DB"
EOF

cat >"$FAKE_BIN/sqlite3" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
database="$4"
query="${*: -1}"
[[ -f "$database" ]] || exit 92
case "$query" in
  *"event_kind IN ('hook_invoked','hook_route')"*)
    case "${TD_EXPECT_SCOPE:?}" in
      project) [[ "$query" == *"project_id='proj_current'"* ]] || exit 91 ;;
      all) [[ "$query" != *"project_id='proj_current'"* ]] || exit 91 ;;
    esac
    printf '8\n'
    ;;
  *"COUNT(*) FROM analytics_events"*) printf '4\n' ;;
  *"GROUP BY tool_name HAVING"*) printf 'tracedecay_status|10|2\n' ;;
  *"'  '||tool_name"*) printf '  tracedecay_status — 10 call(s)\n' ;;
  *"FROM memory_v2_current_facts"*) printf '2 5 2 1 1\n' ;;
  *"GROUP BY session_id, provider"*) printf '  session-1 — 2 errors, provider=codex\n' ;;
  *)
    printf 'unexpected query: %s\n' "$query" >&2
    exit 90
    ;;
esac
EOF

chmod +x "$FAKE_BIN/tracedecay" "$FAKE_BIN/sqlite3"
cmp -s "$HELPER" "$MIRROR" || fail "Codex and Claude helper mirrors differ"

for scope in project all; do
  if [ "$scope" = all ]; then args=(--all); else args=(); fi
  for helper in "$HELPER" "$MIRROR"; do
    output="$({
      PATH="$FAKE_BIN:$PATH" SERVING_DB="$SERVING_DB" TD_EXPECT_SCOPE="$scope" "$helper" "${args[@]}"
    } 2>&1)"
    assert_contains "$output" "TraceDecay friction scan"
    assert_contains "$output" "hook events: 8    tracedecay tool calls: 4"
    assert_contains "$output" "memory_v2_current_facts"
    assert_contains "$output" "session-1 — 2 errors, provider=codex"
  done
done

printf 'ok - typed storage-status payload drives active and all-project friction scans\n'

rm "$GLOBAL_DB"
output="$({
  PATH="$FAKE_BIN:$PATH" SERVING_DB="$SERVING_DB" TD_EXPECT_SCOPE=project "$HELPER"
} 2>&1)"
assert_contains "$output" "global analytics db not found"
[ ! -e "$GLOBAL_DB" ] || fail "missing global analytics database must not be created"

printf 'ok - missing global analytics database is read without creating a file\n'
