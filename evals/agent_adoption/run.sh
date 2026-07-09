#!/usr/bin/env bash
# TraceDecay agent-adoption eval runner.
#
# Drives real headless Claude Code and/or Codex agents against a small indexed
# fixture project and captures each agent's full tool-call stream for grading by
# grade.py. This measures what agents ACTUALLY do (native grep vs tracedecay
# tools, whether they rate facts, etc.), not what an offline classifier decides.
#
# Live agent invocations cost tokens, so they are gated: fixtures are always
# built, but agents only run when TRACEDECAY_AGENT_EVALS=1. Without it the script
# performs a dry run (sets up fixtures, prints the exact commands, grades nothing
# live) so you can inspect the harness for free.
#
# Store isolation: the tracedecay graph + seeded facts live in a throwaway
# TRACEDECAY_DATA_DIR under the work dir. HOME is left untouched, so the agents'
# own auth (Claude OAuth, Codex ~/.codex) keeps working while the tracedecay MCP
# server the agents spawn inherits TRACEDECAY_DATA_DIR and sees the fixture store.
#
# Usage:
#   evals/agent_adoption/run.sh                       # dry run (safe, free)
#   TRACEDECAY_AGENT_EVALS=1 evals/agent_adoption/run.sh
#   TRACEDECAY_AGENT_EVALS=1 HOSTS=claude \
#     SCENARIOS="explore_reserve_stock recall_discount_decision feedback_currency" \
#     evals/agent_adoption/run.sh          # smoke: 3 scenarios, one host
#
# Env knobs:
#   HOSTS                space-separated: "claude", "codex", or "claude codex" (default: claude)
#   SCENARIOS            space-separated scenario ids to run (default: all active)
#   EVAL_INCLUDE_DEFERRED=1   also run scenarios with status="deferred"
#   CLAUDE_MODEL         model alias for claude --model (default: haiku, cheapest for smoke)
#   CODEX_MODEL          model for codex -m (default: unset -> codex config default)
#   SCENARIO_TIMEOUT     per scenario wall-clock seconds (default: 240)
#   TRACEDECAY_BIN       tracedecay binary (default: resolve from PATH)
#   EVAL_OUT            directory to also copy scoreboard.json + report.md into
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"

HOSTS="${HOSTS:-claude}"
CLAUDE_MODEL="${CLAUDE_MODEL:-haiku}"
SCENARIO_TIMEOUT="${SCENARIO_TIMEOUT:-240}"

TD="${TRACEDECAY_BIN:-$(command -v tracedecay || true)}"
if [[ -z "$TD" || ! -x "$TD" ]]; then
  echo "error: tracedecay binary not found; set TRACEDECAY_BIN" >&2
  exit 2
fi

live=0
if [[ "${TRACEDECAY_AGENT_EVALS:-}" == "1" ]]; then live=1; fi

# ---- work dir + hermetic tracedecay store ---------------------------------- #
work="$(mktemp -d "${TMPDIR:-/tmp}/agent-evals.XXXXXX")"
run_dir="$work/run"
mkdir -p "$run_dir"
export TRACEDECAY_DATA_DIR="$work/.tracedecay"
export TRACEDECAY_ENABLE_GLOBAL_DB=0
echo "work dir:     $work"
echo "run dir:      $run_dir"
echo "tracedecay:   $TD ($("$TD" --version 2>/dev/null | head -1))"
echo "hosts:        $HOSTS   live=$live"
"$TD" disable-upload-counter >/dev/null 2>&1 || true

# ---- build fixtures -------------------------------------------------------- #
build_fixture() {
  # $1 = dest dir, $2 = "broken" to plant the type error
  local dest="$1" variant="${2:-clean}"
  cp -R "$here/fixture" "$dest"
  if [[ "$variant" == "broken" ]]; then
    cp "$here/fixture_broken/orders.rs" "$dest/src/orders.rs"
  fi
  git -C "$dest" init -q
  git -C "$dest" add -A >/dev/null 2>&1 || true
  git -C "$dest" -c user.email=eval@tracedecay -c user.name=eval commit -qm init >/dev/null 2>&1 || true
  ( cd "$dest" && "$TD" init >/dev/null 2>&1 )
}

fixture_main="$work/fixture-main"
fixture_broken="$work/fixture-broken"
echo "indexing fixture-main..."
build_fixture "$fixture_main" clean
echo "indexing fixture-broken..."
build_fixture "$fixture_broken" broken

fixture_dir_for() {
  case "$1" in
    broken) echo "$fixture_broken" ;;
    *) echo "$fixture_main" ;;
  esac
}

# ---- seed facts (scoped to fixture-main project) --------------------------- #
seed_fact() {
  # $1 = content ; echoes numeric id
  ( cd "$fixture_main" && "$TD" tool fact_store --action add \
      --content "$1" --category decision --trust 0.9 2>/dev/null ) \
    | grep -oE '#[0-9]+' | head -1 | tr -d '#'
}
echo "seeding facts..."
discount_id="$(seed_fact "The 2026-06 pricing review decided that order discounts are capped at 25 percent for all orders; apply_discount clamps anything larger than the 25 percent cap.")"
currency_id="$(seed_fact "Order totals are always denominated in USD cents. Multi-currency support was explicitly rejected in the 2026-05 architecture review, so every total is USD.")"
printf '{"discount_fact_id": %s, "currency_fact_id": %s}\n' "${discount_id:-null}" "${currency_id:-null}" > "$run_dir/seeded_facts.json"
echo "  discount_fact_id=$discount_id currency_fact_id=$currency_id"

git_sha="$(git -C "$repo_root" rev-parse --short HEAD 2>/dev/null || echo unknown)"
cat > "$run_dir/meta.json" <<JSON
{
  "git_sha": "$git_sha",
  "hosts": {"claude": {"model": "$CLAUDE_MODEL"}, "codex": {"model": "${CODEX_MODEL:-default}"}},
  "scenario_timeout_s": $SCENARIO_TIMEOUT,
  "work_dir": "$work"
}
JSON

# ---- select scenarios ------------------------------------------------------ #
python3 - "$here/scenarios" "${SCENARIOS:-}" "${EVAL_INCLUDE_DEFERRED:-0}" > "$work/selected.tsv" <<'PY'
import json, os, sys
sdir, filt, incl_def = sys.argv[1], sys.argv[2].split(), sys.argv[3] == "1"
for fn in sorted(os.listdir(sdir)):
    if not fn.endswith(".json"):
        continue
    s = json.load(open(os.path.join(sdir, fn)))
    if s.get("status") == "deferred" and not incl_def:
        continue
    if filt and s["id"] not in filt:
        continue
    for host in s.get("hosts", []):
        # emit: id \t host \t fixture \t prompt
        print("\t".join([s["id"], host, s.get("fixture", "main"), s["prompt"].replace("\t", " ")]))
PY

# ---- run each scenario x host --------------------------------------------- #
run_claude() {
  local prompt="$1" fixture="$2" out="$3" err="$4"
  ( cd "$fixture" && timeout "$SCENARIO_TIMEOUT" claude -p "$prompt" \
      --output-format stream-json --verbose \
      --model "$CLAUDE_MODEL" \
      --dangerously-skip-permissions ) >"$out" 2>"$err"
}
run_codex() {
  local prompt="$1" fixture="$2" out="$3" err="$4"
  timeout "$SCENARIO_TIMEOUT" codex exec "$prompt" --json \
    -C "$fixture" --skip-git-repo-check \
    --dangerously-bypass-approvals-and-sandbox \
    ${CODEX_MODEL:+-m "$CODEX_MODEL"} >"$out" 2>"$err"
}

while IFS=$'\t' read -r sid host fixture prompt; do
  [[ -z "$sid" ]] && continue
  case " $HOSTS " in *" $host "*) : ;; *) continue ;; esac
  fdir="$(fixture_dir_for "$fixture")"
  out="$run_dir/${sid}__${host}.stdout.jsonl"
  err="$run_dir/${sid}__${host}.stderr.log"
  if [[ "$live" != "1" ]]; then
    echo "DRY  $sid [$host] fixture=$fixture"
    echo "     cwd=$fdir prompt=\"$prompt\"" >"$err"
    : >"$out"
    continue
  fi
  echo "RUN  $sid [$host] ..."
  start=$(date +%s)
  rc=0
  if [[ "$host" == "claude" ]]; then
    run_claude "$prompt" "$fdir" "$out" "$err" || rc=$?
  else
    run_codex "$prompt" "$fdir" "$out" "$err" || rc=$?
  fi
  end=$(date +%s)
  timed_out=false; [[ $rc -eq 124 ]] && timed_out=true
  cat > "$run_dir/${sid}__${host}.meta.json" <<JSON
{"scenario_id":"$sid","host":"$host","fixture":"$fixture","exit_code":$rc,"duration_s":$((end-start)),"timed_out":$timed_out}
JSON
  echo "     rc=$rc dur=$((end-start))s bytes=$(wc -c <"$out")"
done < "$work/selected.tsv"

# ---- grade ----------------------------------------------------------------- #
if [[ "$live" == "1" ]]; then
  echo "grading..."
  python3 "$here/grade.py" --run-dir "$run_dir" --scenarios "$here/scenarios" || true
  if [[ -n "${EVAL_OUT:-}" ]]; then
    mkdir -p "$EVAL_OUT"
    cp "$run_dir/scoreboard.json" "$run_dir/report.md" "$EVAL_OUT/" 2>/dev/null || true
    echo "copied scoreboard.json + report.md to $EVAL_OUT"
  fi
  echo
  echo "scoreboard: $run_dir/scoreboard.json"
  echo "report:     $run_dir/report.md"
else
  echo
  echo "dry run complete. Fixtures built and indexed under $work."
  echo "Set TRACEDECAY_AGENT_EVALS=1 to launch agents for real."
  echo "Selected scenario x host pairs:"
  cat "$work/selected.tsv" | sed 's/\t/  /g' | cut -c1-100
fi
