#!/usr/bin/env bash
# project-analytics.sh — TraceDecay usage & fact-store adoption snapshot.
#
# Fills the gaps `tracedecay analytics diagnostics` leaves open: a per-tool MCP
# call breakdown, and fact-store *adoption* (how often facts are seen vs. rated).
# Prefers built-in CLI/tools; drops to SQL only for what they do not expose,
# using store paths resolved from `tracedecay tool storage_status` (never
# hardcoded ~/.tracedecay paths).
#
# Usage:
#   scripts/project-analytics.sh            # active project
#   scripts/project-analytics.sh --all      # per-tool breakdown across all projects
set -euo pipefail

ALL=0
[ "${1:-}" = "--all" ] && ALL=1

have() { command -v "$1" >/dev/null 2>&1; }
if ! have sqlite3; then echo "error: sqlite3 not found on PATH" >&2; exit 3; fi
PY=python3; have "$PY" || PY=python

# --- Resolve store paths from TraceDecay, not from hardcoded locations. -------
SS="$(tracedecay tool storage_status --args '{"format":"json"}' 2>/dev/null || true)"
if [ -z "$SS" ]; then echo "error: could not read 'tracedecay tool storage_status'" >&2; exit 4; fi
if ! STORAGE_FIELDS="$(printf '%s' "$SS" | "$PY" -c 'import sys,json
d=json.load(sys.stdin)
store_path=d.get("store_path")
project_id=d.get("project_id")
if not isinstance(store_path, str) or not store_path or not isinstance(project_id, str) or not project_id:
    raise SystemExit(1)
print(f"{store_path}\t{project_id}")')"; then
  echo "error: storage_status is missing required store_path or project_id" >&2
  exit 5
fi
IFS=$'\t' read -r SERVING_DB PROJECT_ID <<EOF
$STORAGE_FIELDS
EOF
if [ ! -f "$SERVING_DB" ]; then
  echo "error: serving store does not exist: $SERVING_DB" >&2
  exit 6
fi
DATA_ROOT="$(dirname "$SERVING_DB")"
TD_HOME="$(dirname "$(dirname "$DATA_ROOT")")"     # .../.tracedecay/projects/<proj> -> .../.tracedecay
GLOBAL_DB="$TD_HOME/global.db"

q() { sqlite3 -noheader -separator '  ' "$1" "$2" 2>/dev/null; }

echo "================================================================"
echo " TraceDecay usage & fact-store adoption — $PROJECT_ID"
echo "================================================================"

# --- 1. MCP tool adoption (per-tool breakdown; the CLI only groups by kind). --
echo
echo "## MCP tool calls (analytics_events)"
if [ -f "$GLOBAL_DB" ]; then
  FILTER="event_kind='mcp_tool_call'"
  [ "$ALL" -eq 0 ] && FILTER="$FILTER AND project_id='$PROJECT_ID'"
  SCOPE=$([ "$ALL" -eq 1 ] && echo "ALL PROJECTS" || echo "this project")
  echo "scope: $SCOPE"
  printf '  %-42s %8s %8s\n' "tool" "calls" "errors"
  q "$GLOBAL_DB" "SELECT tool_name, COUNT(*), SUM(outcome='error')
       FROM analytics_events WHERE $FILTER
       GROUP BY tool_name ORDER BY COUNT(*) DESC LIMIT 25;" \
    | while IFS='  ' read -r name calls errs; do printf '  %-42s %8s %8s\n' "$name" "$calls" "${errs:-0}"; done
  echo "  ------------------------------------------------------------"
  TOT=$(q "$GLOBAL_DB" "SELECT COUNT(*) FROM analytics_events WHERE $FILTER;")
  echo "  total mcp_tool_call events: ${TOT:-0}"
else
  echo "  (global analytics db not found at $GLOBAL_DB)"
fi

# --- 2. Fact-store adoption: SEEN vs RATED. ----------------------------------
echo
echo "## Fact-store adoption (serving store: $(basename "$SERVING_DB"))"
read -r FACTS RETR ACC HELP UNH RATED RETRIEVED <<EOF
$(q "$SERVING_DB" "SELECT COUNT(*), COALESCE(SUM(retrieval_count),0), COALESCE(SUM(access_count),0),
     COALESCE(SUM(helpful_count),0), COALESCE(SUM(unhelpful_count),0),
     SUM(helpful_count+unhelpful_count>0), SUM(retrieval_count>0) FROM memory_facts;")
EOF
FB=$(( ${HELP:-0} + ${UNH:-0} ))
SEEN=$(( ${RETR:-0} + ${ACC:-0} ))
printf '  %-26s %s\n' "facts stored:"        "${FACTS:-0}"
printf '  %-26s %s\n' "retrievals (seen):"   "${RETR:-0}"
printf '  %-26s %s\n' "accesses:"            "${ACC:-0}"
printf '  %-26s %s\n' "helpful / unhelpful:" "${HELP:-0} / ${UNH:-0}"
printf '  %-26s %s of %s\n' "facts ever rated:" "${RATED:-0}" "${FACTS:-0}"
printf '  %-26s %s of %s\n' "facts ever retrieved:" "${RETRIEVED:-0}" "${FACTS:-0}"
if [ "$FB" -gt 0 ]; then
  printf '  %-26s %s : 1\n' "seen : feedback ratio:" "$(( SEEN / FB ))"
  RATE=$("$PY" -c "print(f'{100*$FB/max($RETR,1):.2f}%')")
  printf '  %-26s %s of retrievals\n' "feedback rate:" "$RATE"
  echo "  signal: feedback loop is ACTIVE but sparse — confirm trust scores are earned, not just seeded."
else
  echo "  seen : feedback ratio:     ${SEEN} : 0"
  echo "  >> DEAD FEEDBACK LOOP: facts are seen ${SEEN}x but never rated helpful/unhelpful."
  echo "     Trust scores are entirely seed-time values, never earned. Adoption gap."
fi

# --- 3. Feedback ledger (transport-agnostic: CLI + MCP + automation). ---------
echo
echo "## Feedback ledger (memory_feedback_events — all transports)"
LEDGER="$(q "$SERVING_DB" "SELECT action, datetime(created_at,'unixepoch'), source, substr(COALESCE(note,''),1,60)
              FROM memory_feedback_events ORDER BY created_at;")"
if [ -n "$LEDGER" ]; then printf '%s\n' "$LEDGER" | sed 's/^/  /'; else echo "  (none — no fact has ever received feedback)"; fi

# --- 4. Read vs write activity (oplog is write-side; retrievals are read-side).
echo
echo "## Write ops (memory_oplog) vs read activity"
q "$SERVING_DB" "SELECT '  '||op||': '||COUNT(*) FROM memory_oplog GROUP BY op ORDER BY COUNT(*) DESC;"
WRITES=$(q "$SERVING_DB" "SELECT COALESCE(SUM(op IN ('add','update','remove')),0) FROM memory_oplog;")
echo "  ------------------------------------------------------------"
echo "  write ops (add+update+remove): ${WRITES:-0}   |   retrieval yield (facts returned): ${RETR:-0}"
echo
