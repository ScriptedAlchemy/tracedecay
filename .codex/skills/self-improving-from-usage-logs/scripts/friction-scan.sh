#!/usr/bin/env bash
# friction-scan.sh — mine TraceDecay usage logs for friction the self-improving
# loop should act on: tool error rates, low-adoption tools, dead feedback loops,
# and the evidence sessions behind them. Maps directly onto this skill's
# "Opportunity Ranking" table.
#
# Prefers CLI/tools; drops to SQL on the durable analytics_events + memory store
# only for what `tracedecay analytics diagnostics` does not expose. Store paths
# come from `tracedecay tool storage_status`, never hardcoded ~/.tracedecay.
#
# Usage:
#   scripts/friction-scan.sh            # active project
#   scripts/friction-scan.sh --all      # across all projects
set -euo pipefail

ALL=0
[ "${1:-}" = "--all" ] && ALL=1
have() { command -v "$1" >/dev/null 2>&1; }
have sqlite3 || { echo "error: sqlite3 not found on PATH" >&2; exit 3; }
PY=python3; have "$PY" || PY=python

SS="$(tracedecay tool storage_status --args '{"format":"json"}' 2>/dev/null || true)"
[ -n "$SS" ] || { echo "error: could not read 'tracedecay tool storage_status'" >&2; exit 4; }
if ! STORAGE_FIELDS="$(printf '%s' "$SS" | "$PY" -c 'import sys,json
d=json.load(sys.stdin)
payload=d.get("outcome", {}).get("value", {}).get("payload")
if not isinstance(payload, dict):
    payload=d
store_path=payload.get("store_path")
project_id=payload.get("project_id")
if not isinstance(store_path, str) or not store_path or not isinstance(project_id, str) or not project_id:
    raise SystemExit(1)
print(f"{store_path}\t{project_id}")')"; then
  echo "error: storage_status is missing required store_path or project_id" >&2
  exit 5
fi
IFS=$'\t' read -r SERVING_DB PROJECT_ID <<EOF
$STORAGE_FIELDS
EOF
[ -f "$SERVING_DB" ] || { echo "error: serving store does not exist: $SERVING_DB" >&2; exit 6; }
DATA_ROOT="$(dirname "$SERVING_DB")"
TD_HOME="$(dirname "$(dirname "$DATA_ROOT")")"
GLOBAL_DB="$TD_HOME/global.db"
q() {
  [ -f "$1" ] || return 0
  sqlite3 -noheader -separator '|' "$1" "$2" 2>/dev/null
}

WHERE="event_kind='mcp_tool_call'"
[ "$ALL" -eq 0 ] && WHERE="$WHERE AND project_id='$PROJECT_ID'"
SCOPE=$([ "$ALL" -eq 1 ] && echo "ALL PROJECTS" || echo "$PROJECT_ID")

echo "================================================================"
echo " TraceDecay friction scan — $SCOPE"
echo "================================================================"
[ -f "$GLOBAL_DB" ] || { echo "(global analytics db not found at $GLOBAL_DB)"; }

# --- 1. Adoption ratio: hook fan-out vs. actual tracedecay tool calls. --------
echo
echo "## Adoption: hook volume vs. tracedecay tool calls"
HOOKS=$(q "$GLOBAL_DB" "SELECT COUNT(*) FROM analytics_events WHERE event_kind IN ('hook_invoked','hook_route')$([ "$ALL" -eq 0 ] && echo " AND project_id='$PROJECT_ID'");")
TOOLS=$(q "$GLOBAL_DB" "SELECT COUNT(*) FROM analytics_events WHERE $WHERE;")
echo "  hook events: ${HOOKS:-0}    tracedecay tool calls: ${TOOLS:-0}"
[ "${TOOLS:-0}" -gt 0 ] && echo "  ratio: $("$PY" -c "print(f'{${HOOKS:-0}/${TOOLS:-0}:.1f}')") hook events per tool call"

# --- 2. Error-rate ranking: tools most likely to fail the agent. -------------
echo
echo "## Highest tool error rates (min 10 calls)"
printf '  %-40s %7s %7s %7s\n' "tool" "calls" "errors" "rate"
q "$GLOBAL_DB" "SELECT tool_name, COUNT(*) c, SUM(outcome='error') e
     FROM analytics_events WHERE $WHERE GROUP BY tool_name HAVING c>=10
     ORDER BY (1.0*SUM(outcome='error')/COUNT(*)) DESC, e DESC LIMIT 12;" \
  | while IFS='|' read -r name c e; do
      rate=$("$PY" -c "print(f'{100*${e:-0}/${c:-1}:.1f}%')")
      printf '  %-40s %7s %7s %7s\n' "$name" "$c" "${e:-0}" "$rate"
    done

# --- 3. Low-adoption tools: called, but rarely (discovery/trigger gaps). ------
echo
echo "## Least-invoked tools (bottom 12 of those ever called) — candidate discovery gaps"
q "$GLOBAL_DB" "SELECT '  '||tool_name||' — '||COUNT(*)||' call(s)'
     FROM analytics_events WHERE $WHERE GROUP BY tool_name ORDER BY COUNT(*) ASC LIMIT 12;"
echo "  (a tool the agents know exists but almost never call is a trigger-text or discoverability gap)"

# --- 4. Dead feedback loop: facts seen vs. rated (self-improving eval surface).
echo
echo "## Feedback loop health (memory_v2_current_facts)"
read -r FACTS RETR HELP UNH RATED <<EOF
$(sqlite3 -noheader -separator ' ' "$SERVING_DB" "SELECT COUNT(*), COALESCE(SUM(retrieval_count),0),
   COALESCE(SUM(helpful_count),0), COALESCE(SUM(unhelpful_count),0), SUM(helpful_count+unhelpful_count>0)
   FROM memory_v2_current_facts;" 2>/dev/null)
EOF
FB=$(( ${HELP:-0} + ${UNH:-0} ))
echo "  facts: ${FACTS:-0}   retrievals: ${RETR:-0}   rated: ${RATED:-0}   feedback events: $FB"
if [ "$FB" -eq 0 ] && [ "${RETR:-0}" -gt 0 ]; then
  echo "  >> OPPORTUNITY (dead loop): ${RETR} retrievals, 0 feedback. Trust never earned."
  echo "     Fix per table: surface fact_id on recall + add a 'rate the fact you used' trigger."
elif [ "$FB" -gt 0 ] && [ "$RATED" -lt "$FACTS" ]; then
  echo "  >> sparse: only ${RATED}/${FACTS} facts ever rated. Feedback is possible but under-used."
fi

# --- 5. Evidence: sessions carrying the most tool errors. --------------------
echo
echo "## Evidence — sessions with the most tool errors (cite these)"
q "$GLOBAL_DB" "SELECT '  '||COALESCE(NULLIF(session_id,''),'(no session)')||' — '||COUNT(*)||' errors, provider='||provider
     FROM analytics_events WHERE $WHERE AND outcome='error'
     GROUP BY session_id, provider ORDER BY COUNT(*) DESC LIMIT 8;"
echo
