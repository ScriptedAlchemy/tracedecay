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

# Ablation matrix. Default is "full" only (all discovery channels on) to keep
# cost bounded — each extra condition multiplies the number of live agent runs.
# Opt in with e.g. CHANNELS="full no-hints no-skills bare". See the README.
CHANNELS="${CHANNELS:-full}"
KNOWN_CONDITIONS="full no-hints no-skills bare"
for c in $CHANNELS; do
  case " $KNOWN_CONDITIONS " in
    *" $c "*) : ;;
    *) echo "error: unknown CHANNELS condition '$c' (allowed: $KNOWN_CONDITIONS)" >&2; exit 2 ;;
  esac
done

# Fixed steering string used for the hermetic ablation conditions. It replaces
# the user's ambient ~/.claude/CLAUDE.md (deliberately excluded in ablations via
# --setting-sources) so steering is held constant across no-hints/no-skills
# instead of varying with whatever global memory the operator happens to run.
STEER_TEXT="This repository is indexed for semantic code intelligence; prefer the available code-graph tools over raw file search when answering code questions."

TD="${TRACEDECAY_BIN:-$(command -v tracedecay || true)}"
if [[ -z "$TD" || ! -x "$TD" ]]; then
  echo "error: tracedecay binary not found; set TRACEDECAY_BIN" >&2
  exit 2
fi

# Neutrality lint (USER DOCTRINE): fail fast — before building fixtures or
# spending a single token — if any scenario prompt names tracedecay/MCP/a
# tool/a skill. Keeps future scenarios honest at the point of use.
echo "linting scenario prompts for neutrality..."
if ! python3 "$here/grade.py" --lint-only --scenarios "$here/scenarios"; then
  echo "abort: scenario prompts failed the neutrality lint (see above)." >&2
  exit 3
fi

# Hint-signature drift guard: channel attribution mirrors distinctive fragments
# of src/hooks/tool_hints.rs. If that wording drifted and the mirror did not, a
# live run would silently misclassify hint-driven adoptions as steering, so fail
# fast here — before building fixtures or spending a token. Skips cleanly when
# run from a published package without the Rust source tree.
echo "checking hint signatures against tool_hints.rs..."
if ! python3 "$here/grade.py" --check-hints; then
  echo "abort: hint signatures drifted from src/hooks/tool_hints.rs (see above)." >&2
  exit 3
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
echo "channels:     $CHANNELS"
"$TD" disable-upload-counter >/dev/null 2>&1 || true

# ---- build fixtures -------------------------------------------------------- #
gitc() { git -C "$1" -c user.email=eval@tracedecay -c user.name=eval "${@:2}"; }

build_fixture() {
  # $1 = dest dir, $2 = "broken" to plant the type error, $3 = "history" to seed
  # a small multi-branch git history with a merge (for git-tier scenarios).
  local dest="$1" variant="${2:-clean}" history="${3:-}"
  cp -R "$here/fixture" "$dest"
  if [[ "$variant" == "broken" ]]; then
    cp "$here/fixture_broken/orders.rs" "$dest/src/orders.rs"
  fi
  git -C "$dest" init -q
  gitc "$dest" add -A >/dev/null 2>&1 || true
  gitc "$dest" commit -qm "init: orders fixture" >/dev/null 2>&1 || true

  if [[ "$history" == "history" ]]; then
    # Seed 3-4 commits across 2 branches with a real (--no-ff) merge so
    # branch_list / branch_diff / commit_context / diff_context have something
    # to grade. Branch and main touch DIFFERENT files so the merge is clean.
    local base
    base="$(git -C "$dest" symbolic-ref --short HEAD 2>/dev/null || echo master)"
    gitc "$dest" checkout -q -b feature/pricing-notes
    printf '\n// Reviewed 2026-06: discount cap origin is the pricing review.\n' >> "$dest/src/discount.rs"
    gitc "$dest" commit -qam "docs: annotate discount cap origin" >/dev/null 2>&1 || true
    printf '# Pricing notes\n\nDiscounts are capped at 25%% per the 2026-06 review.\n' > "$dest/NOTES.md"
    gitc "$dest" add -A >/dev/null 2>&1 || true
    gitc "$dest" commit -qm "docs: add pricing NOTES" >/dev/null 2>&1 || true
    gitc "$dest" checkout -q "$base"
    printf '# Changelog\n\n- init: orders fixture\n' > "$dest/CHANGELOG.md"
    gitc "$dest" add -A >/dev/null 2>&1 || true
    gitc "$dest" commit -qm "chore: start changelog" >/dev/null 2>&1 || true
    gitc "$dest" merge --no-ff -q -m "merge: pricing notes into $base" feature/pricing-notes >/dev/null 2>&1 || true
  fi

  ( cd "$dest" && "$TD" init >/dev/null 2>&1 )
}

fixture_main="$work/fixture-main"
fixture_broken="$work/fixture-broken"
echo "indexing fixture-main (with seeded git history)..."
build_fixture "$fixture_main" clean history
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

# ---- ablation provisioning ------------------------------------------------- #
# Channel isolation is hard because a globally-installed plugin bundles hooks
# (hints) + skills + MCP together. To ablate ONE channel we build a hermetic,
# componentized copy of the plugin per condition and load ONLY it, dropping the
# ambient user config via --setting-sources so global hooks/skills/CLAUDE.md do
# not leak in. Descriptions (MCP) are held constant across every condition via a
# fixed --mcp-config + --strict-mcp-config.
#
# Condition -> channels:
#   full      hooks ON  | skills ON  | steering=ambient   (production parity; global install)
#   no-hints  hooks OFF | skills ON  | steering=fixed     (isolates skill/description efficacy)
#   no-skills hooks ON  | skills OFF | steering=fixed     (isolates hint/description efficacy)
#   bare      hooks OFF | skills OFF | steering=none       (pure MCP-description/unprompted)
plugin_src="$repo_root/plugin"
mcp_cfg="$work/mcp-tracedecay.json"
have_plugin=0
if [[ -d "$plugin_src/.claude-plugin" ]]; then
  have_plugin=1
  cat > "$mcp_cfg" <<JSON
{"mcpServers":{"tracedecay":{"type":"stdio","command":"$TD","args":["serve"],"env":{"TRACEDECAY_DATA_DIR":"$TRACEDECAY_DATA_DIR","TRACEDECAY_ENABLE_GLOBAL_DB":"0"}}}}
JSON
fi

provision_variant() {
  # $1 = condition; provisions $work/plugins/<cond> (hermetic plugin copy).
  local cond="$1" d="$work/plugins/$1"
  [[ -d "$d" ]] && return 0
  mkdir -p "$work/plugins"
  cp -R "$plugin_src" "$d"
  # Point the hook command at the resolved binary (install-time substitution).
  if [[ -f "$d/hooks/hooks-claude.json" ]]; then
    sed -i "s#__TRACEDECAY_BIN__#$TD#g" "$d/hooks/hooks-claude.json"
  fi
  case "$cond" in
    no-hints) rm -f "$d"/hooks/*.json ;;   # skills + mcp, no hooks
    no-skills) rm -rf "$d/skills" ;;       # hooks + mcp, no skills
    bare) rm -f "$d"/hooks/*.json; rm -rf "$d/skills" ;;
  esac
}

# Extra `claude` CLI flags for a given ablation condition. Prints a flag string.
CLAUDE_EXTRA=()
claude_extra_for() {
  CLAUDE_EXTRA=()
  local cond="$1" fdir="$2"
  [[ "$cond" == "full" ]] && return 0     # production parity: unchanged invocation
  if [[ "$have_plugin" != "1" ]]; then
    echo "warn: no repo plugin dir; cannot ablate '$cond' hermetically — running as full" >&2
    return 0
  fi
  provision_variant "$cond"
  # Drop ambient user config (global plugin + user CLAUDE.md); pin MCP explicitly.
  CLAUDE_EXTRA=(--setting-sources project,local
                --strict-mcp-config --mcp-config "$mcp_cfg"
                --add-dir "$fdir"
                --plugin-dir "$work/plugins/$cond")
  # Hold steering constant for the single-channel ablations; bare gets none.
  if [[ "$cond" == "no-hints" || "$cond" == "no-skills" ]]; then
    CLAUDE_EXTRA+=(--append-system-prompt "$STEER_TEXT")
  fi
}

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

# ---- run each scenario x host x condition ---------------------------------- #
run_claude() {
  # Uses the global CLAUDE_EXTRA array set by claude_extra_for.
  local prompt="$1" fixture="$2" out="$3" err="$4"
  ( cd "$fixture" && timeout "$SCENARIO_TIMEOUT" claude -p "$prompt" \
      --output-format stream-json --verbose \
      --model "$CLAUDE_MODEL" \
      --dangerously-skip-permissions \
      "${CLAUDE_EXTRA[@]}" ) >"$out" 2>"$err"
}
run_codex() {
  local prompt="$1" fixture="$2" out="$3" err="$4"
  timeout "$SCENARIO_TIMEOUT" codex exec "$prompt" --json \
    -C "$fixture" --skip-git-repo-check \
    --dangerously-bypass-approvals-and-sandbox \
    ${CODEX_MODEL:+-m "$CODEX_MODEL"} >"$out" 2>"$err"
}

# Transcript/meta basename for a run: full keeps the legacy <id>__<host> shape;
# ablations append __<condition>.
out_base_for() {
  if [[ "$1" == "full" ]]; then echo "$2__$3"; else echo "$2__$3__$1"; fi
}

for cond in $CHANNELS; do
while IFS=$'\t' read -r sid host fixture prompt; do
  [[ -z "$sid" ]] && continue
  case " $HOSTS " in *" $host "*) : ;; *) continue ;; esac
  # Ablation of hooks/skills is Claude-only (Codex hooks/skills live host-global
  # under ~/.codex and are not hermetically componentized here).
  if [[ "$host" == "codex" && "$cond" != "full" ]]; then
    [[ "$live" == "1" ]] && echo "skip $sid [codex/$cond]: codex ablation not supported"
    continue
  fi
  fdir="$(fixture_dir_for "$fixture")"
  base="$(out_base_for "$cond" "$sid" "$host")"
  out="$run_dir/${base}.stdout.jsonl"
  err="$run_dir/${base}.stderr.log"
  if [[ "$live" != "1" ]]; then
    echo "DRY  $sid [$host/$cond] fixture=$fixture"
    if [[ "$host" == "claude" ]]; then claude_extra_for "$cond" "$fdir"; fi
    echo "     cwd=$fdir extra=[${CLAUDE_EXTRA[*]:-}] prompt=\"$prompt\"" >"$err"
    : >"$out"
    continue
  fi
  echo "RUN  $sid [$host/$cond] ..."
  start=$(date +%s)
  rc=0
  if [[ "$host" == "claude" ]]; then
    claude_extra_for "$cond" "$fdir"
    run_claude "$prompt" "$fdir" "$out" "$err" || rc=$?
  else
    run_codex "$prompt" "$fdir" "$out" "$err" || rc=$?
  fi
  end=$(date +%s)
  timed_out=false; [[ $rc -eq 124 ]] && timed_out=true
  cat > "$run_dir/${base}.meta.json" <<JSON
{"scenario_id":"$sid","host":"$host","fixture":"$fixture","channel_condition":"$cond","exit_code":$rc,"duration_s":$((end-start)),"timed_out":$timed_out}
JSON
  echo "     rc=$rc dur=$((end-start))s bytes=$(wc -c <"$out")"
done < "$work/selected.tsv"
done

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
