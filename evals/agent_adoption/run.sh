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
# Store and host isolation: the graph, host HOME/config, and candidate plugin
# install all live under the throwaway work dir. Authentication is copied from
# the real profile with read-only permissions; the real profile is never loaded
# or mutated by an eval agent.
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
#   CLAUDE_MODELS        space-separated Claude matrix (default: opus sonnet)
#   CODEX_MODELS         space-separated Codex matrix (default: gpt-5.5 gpt-5.6-terra)
#   SCENARIO_TIMEOUT     per scenario wall-clock seconds (default: 240)
#   REPS                 repetitions per scenario x host x model x condition cell
#                        (default: 1; >1 suffixes transcripts with __r<N>)
#   PARALLEL             concurrent live agent runs (default: 1); runs are
#                        read-only against shared fixtures, so this is safe
#   TRACEDECAY_BIN       tracedecay binary (default: resolve from PATH)
#   EVAL_OUT            directory to also copy scoreboard.json + report.md into
set -euo pipefail
umask 077

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/../.." && pwd)"
EVAL_SCENARIOS_DIR="$(cd "${EVAL_SCENARIOS_DIR:-$here/scenarios}" && pwd)"
export EVAL_SCENARIOS_DIR

HOSTS="${HOSTS:-claude}"
CLAUDE_MODELS="${CLAUDE_MODELS:-opus sonnet}"
CODEX_MODELS="${CODEX_MODELS:-gpt-5.5 gpt-5.6-terra}"
SCENARIO_TIMEOUT="${SCENARIO_TIMEOUT:-240}"
REPS="${REPS:-1}"
PARALLEL="${PARALLEL:-1}"
for knob in REPS PARALLEL; do
  [[ "${!knob}" =~ ^[1-9][0-9]*$ ]] || { echo "error: $knob must be a positive integer" >&2; exit 2; }
done

# Ablation matrix. Default is "full" only (all discovery channels on) to keep
# cost bounded — each extra condition multiplies the number of live agent runs.
# Opt in with e.g. CHANNELS="full no-hints no-skills bare cli-only". See the README.
CHANNELS="${CHANNELS:-full}"
KNOWN_CONDITIONS="full no-hints no-skills bare cli-only"
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
STEER_TEXT="This repository has indexed code-relationship evidence available. Choose tools according to the evidence the task requires."

live=0
if [[ "${TRACEDECAY_AGENT_EVALS:-}" == "1" ]]; then live=1; fi

# Live runs default to the candidate branch binary. TRACEDECAY_BIN remains an
# explicit override for release-binary comparisons; dry runs reuse a built
# candidate when present and otherwise fall back to PATH.
if [[ -n "${TRACEDECAY_BIN:-}" ]]; then
  TD="$TRACEDECAY_BIN"
elif [[ "$live" == "1" ]]; then
  echo "building candidate tracedecay binary..."
  if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    eval_target="$CARGO_TARGET_DIR"
  elif [[ -d /fast/cargo-target && -w /fast/cargo-target ]]; then
    eval_target="/fast/cargo-target/tracedecay-agent-adoption-evals"
  else
    eval_target="$repo_root/target"
  fi
  (
    cd "$repo_root"
    CARGO_TARGET_DIR="$eval_target" \
      cargo build --quiet --package tracedecay-cli --bin tracedecay
  )
  TD="$eval_target/debug/tracedecay"
elif [[ -x "$repo_root/target/debug/tracedecay" ]]; then
  TD="$repo_root/target/debug/tracedecay"
else
  TD="$(command -v tracedecay || true)"
fi
if [[ -z "$TD" || ! -x "$TD" ]]; then
  echo "error: tracedecay binary not found; set TRACEDECAY_BIN" >&2
  exit 2
fi
TD="$(cd "$(dirname "$TD")" && pwd)/$(basename "$TD")"
EVAL_PATH="$(dirname "$TD"):$PATH"

# Neutrality lint (USER DOCTRINE): fail fast — before building fixtures or
# spending a single token — if any scenario prompt names tracedecay/MCP/a
# tool/a skill. Keeps future scenarios honest at the point of use.
echo "linting scenario prompts for neutrality..."
if ! python3 "$here/grade.py" --lint-only --scenarios "$EVAL_SCENARIOS_DIR"; then
  echo "abort: scenario prompts failed the neutrality lint (see above)." >&2
  exit 3
fi

# Hint-signature drift guard: channel attribution mirrors distinctive fragments
# of crates/tracedecay-agent-hosts/src/hooks/tool_hints.rs. If that wording
# drifted and the mirror did not, a live run would silently misclassify
# hint-driven adoptions as steering, so fail fast here — before building
# fixtures or spending a token. Skips cleanly when run from a published package
# without the Rust source tree.
echo "checking hint signatures against tool_hints.rs..."
if ! python3 "$here/grade.py" --check-hints; then
  echo "abort: hint signatures drifted from crates/tracedecay-agent-hosts/src/hooks/tool_hints.rs (see above)." >&2
  exit 3
fi

# Fixture initialization is daemon-brokered: the daemon owns the code-index
# scheduler. Re-enter the runner under the repository's bounded isolated-daemon
# harness so fixture init and every agent tool call share one private profile
# and socket. TRACEDECAY_BIN pins the child invocation to the same candidate
# binary that owns the daemon.
export TRACEDECAY_ENABLE_GLOBAL_DB=0
if [[ "${TRACEDECAY_AGENT_EVAL_ISOLATED:-}" != "1" ]]; then
  # Preserve only explicit read-only auth sources before changing HOME. The
  # daemon must not discover the operator's host transcripts or configuration.
  export AGENT_EVAL_CODEX_AUTH_SOURCE="${CODEX_HOME:-$HOME/.codex}"
  export AGENT_EVAL_CLAUDE_AUTH_SOURCE="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
  eval_home="$(mktemp -d "${TMPDIR:-/tmp}/agent-eval-home.XXXXXX")"
  trap 'rm -rf "$eval_home"' EXIT
  mkdir -p "$eval_home/home" "$eval_home/workspace" "$eval_home/tmp"
  export HOME="$eval_home/home"
  export XDG_CONFIG_HOME="$HOME/.config"
  export XDG_DATA_HOME="$HOME/.local/share"
  export XDG_CACHE_HOME="$HOME/.cache"
  export XDG_STATE_HOME="$HOME/.local/state"
  export CODEX_HOME="$HOME/.codex"
  export CLAUDE_CONFIG_DIR="$HOME/.claude"
  export TMPDIR="$eval_home/tmp"
  # Keep report/fixture artifacts outside the disposable authority home.
  export AGENT_EVAL_ARTIFACT_TMP="${AGENT_EVAL_ARTIFACT_TMP:-/tmp}"
  (
    cd "$eval_home/workspace"
    "$repo_root/scripts/with-isolated-tracedecay-daemon.sh" \
      --bin "$TD" \
      --ready-timeout 60 \
      --stop-timeout 10 \
      --lifecycle-label "agent-adoption eval daemon" \
      -- env TRACEDECAY_AGENT_EVAL_ISOLATED=1 AGENT_EVAL_ISOLATION_ROOT="$eval_home" \
        TRACEDECAY_BIN="$TD" "$here/run.sh" "$@"
  )
  exit $?
fi
# Re-entry is valid only with the exact paths established by this runner. A
# preconfigured flag alone must not suppress host/profile isolation.
if [[ -z "${AGENT_EVAL_ISOLATION_ROOT:-}" ||
      "$HOME" != "$AGENT_EVAL_ISOLATION_ROOT/home" ||
      "$PWD" != "$AGENT_EVAL_ISOLATION_ROOT/workspace" ||
      "$TMPDIR" != "$AGENT_EVAL_ISOLATION_ROOT/tmp" ||
      "$XDG_CONFIG_HOME" != "$HOME/.config" ||
      "$XDG_DATA_HOME" != "$HOME/.local/share" ||
      "$XDG_CACHE_HOME" != "$HOME/.cache" ||
      "$XDG_STATE_HOME" != "$HOME/.local/state" ||
      "$CODEX_HOME" != "$HOME/.codex" ||
      "$CLAUDE_CONFIG_DIR" != "$HOME/.claude" ||
      "${TRACEDECAY_DATA_DIR:-}" != "$TMPDIR/"*/profile ||
      "${TRACEDECAY_DAEMON_SOCKET:-}" != "$TMPDIR/"*/daemon.sock ]]; then
  echo "error: evaluator isolation re-entry paths do not match" >&2
  exit 2
fi

# ---- work dir + hermetic host state ---------------------------------------- #
work="$(mktemp -d "${AGENT_EVAL_ARTIFACT_TMP:-${TMPDIR:-/tmp}}/agent-evals.XXXXXX")"
run_dir="$work/run"
mkdir -p "$run_dir"

REAL_HOME="${HOME:?HOME must be set}"
REAL_CODEX_HOME="${AGENT_EVAL_CODEX_AUTH_SOURCE:-${CODEX_HOME:-$REAL_HOME/.codex}}"
REAL_CLAUDE_CONFIG="${AGENT_EVAL_CLAUDE_AUTH_SOURCE:-${CLAUDE_CONFIG_DIR:-$REAL_HOME/.claude}}"
CODEX_EVAL_HOME="$work/host-homes/codex"
CODEX_EVAL_CONFIG="$CODEX_EVAL_HOME/.codex"
CLAUDE_EVAL_HOME="$work/host-homes/claude"
CLAUDE_EVAL_CONFIG="$CLAUDE_EVAL_HOME/.claude"
mkdir -p "$CODEX_EVAL_CONFIG" "$CLAUDE_EVAL_CONFIG"

copy_auth_readonly() {
  local src="$1" dest="$2"
  [[ -f "$src" ]] || return 0
  cp "$src" "$dest"
  chmod 400 "$dest"
}

scrub_auth_copies() {
  rm -f "$CODEX_EVAL_CONFIG/auth.json" \
    "$CLAUDE_EVAL_CONFIG/.credentials.json" \
    "$CLAUDE_EVAL_CONFIG/credentials.json"
}
trap scrub_auth_copies EXIT
trap 'exit 130' INT TERM HUP

prepare_host_profiles() {
  # Candidate assets install only into the throwaway Codex profile. Auth is
  # copied afterward so install/update code cannot touch the real credential.
  if [[ " $HOSTS " == *" codex "* ]]; then
    HOME="$CODEX_EVAL_HOME" CODEX_HOME="$CODEX_EVAL_CONFIG" PATH="$EVAL_PATH" \
      "$TD" install --agent codex >"$work/codex-install.log" 2>&1
    HOME="$CODEX_EVAL_HOME" CODEX_HOME="$CODEX_EVAL_CONFIG" PATH="$EVAL_PATH" \
      codex plugin add tracedecay@personal --json \
      >>"$work/codex-install.log" 2>&1
    copy_auth_readonly "$REAL_CODEX_HOME/auth.json" "$CODEX_EVAL_CONFIG/auth.json"
  fi
  if [[ " $HOSTS " == *" claude "* ]]; then
    copy_auth_readonly "$REAL_CLAUDE_CONFIG/.credentials.json" \
      "$CLAUDE_EVAL_CONFIG/.credentials.json"
    copy_auth_readonly "$REAL_CLAUDE_CONFIG/credentials.json" \
      "$CLAUDE_EVAL_CONFIG/credentials.json"
  fi
}

if [[ "$live" == "1" ]]; then
  prepare_host_profiles
fi
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
  # $1 = content ; echoes canonical fact_id (fact.v1....)
  ( cd "$fixture_main" && "$TD" tool fact_store_add \
      --content "$1" --category decision --trust 0.9 --format json 2>/dev/null ) \
    | python3 -c 'import json,sys
try:
    d = json.load(sys.stdin)
except Exception:
    raise SystemExit(0)
fact = d.get("fact") if isinstance(d.get("fact"), dict) else {}
print(fact.get("fact_id") or d.get("fact_id") or "")'
}
echo "seeding facts..."
discount_id="$(seed_fact "The 2026-06 pricing review decided that order discounts are capped at 25 percent for all orders; apply_discount clamps anything larger than the 25 percent cap.")"
currency_id="$(seed_fact "Order totals are always denominated in USD cents. Multi-currency support was explicitly rejected in the 2026-05 architecture review, so every total is USD.")"
python3 -c 'import json,sys; print(json.dumps({"discount_fact_id": sys.argv[1] or None, "currency_fact_id": sys.argv[2] or None}))' \
  "${discount_id:-}" "${currency_id:-}" > "$run_dir/seeded_facts.json"
echo "  discount_fact_id=$discount_id currency_fact_id=$currency_id"

git_sha="$(git -C "$repo_root" rev-parse --short HEAD 2>/dev/null || echo unknown)"
cat > "$run_dir/meta.json" <<JSON
{
  "git_sha": "$git_sha",
  "hosts": {"claude": {"models": "$CLAUDE_MODELS"}, "codex": {"models": "$CODEX_MODELS"}},
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
#   full      hooks ON  | skills ON  | steering=plugin    (candidate plugin, hermetic profile)
#   no-hints  hooks OFF | skills ON  | steering=fixed     (isolates skill/description efficacy)
#   no-skills hooks ON  | skills OFF | steering=fixed     (isolates hint/description efficacy)
#   bare      hooks OFF | skills OFF | steering=none       (pure MCP-description/unprompted)
#   cli-only  hooks OFF | skills ON  | MCP OFF              (supported shell fallback)
plugin_src="$repo_root/plugin"
mcp_cfg="$work/mcp-tracedecay.json"
empty_mcp_cfg="$work/empty-mcp.json"
printf '%s\n' '{"mcpServers":{}}' > "$empty_mcp_cfg"
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
  # Plugin `commands/` are exposed to Claude as `tracedecay:*` skills too (the
  # Skill tool launches them and their bodies name tracedecay tools), so a
  # skill-free condition must strip them alongside `skills/`.
  case "$cond" in
    no-hints) rm -f "$d"/hooks/*.json ;;   # skills + mcp, no hooks
    no-skills) rm -rf "$d/skills" "$d/commands" ;;       # hooks + mcp, no skills
    bare) rm -f "$d"/hooks/*.json; rm -rf "$d/skills" "$d/commands" ;;
    cli-only) rm -f "$d"/.mcp.json "$d"/mcp.json "$d"/hooks/*.json ;;
  esac
}

# Extra `claude` CLI flags for a given ablation condition. Prints a flag string.
CLAUDE_EXTRA=()
claude_extra_for() {
  CLAUDE_EXTRA=()
  local cond="$1" fdir="$2"
  if [[ "$have_plugin" != "1" ]]; then
    echo "error: candidate plugin dir missing; cannot run '$cond' hermetically" >&2
    return 2
  fi
  provision_variant "$cond"
  local selected_mcp="$mcp_cfg"
  [[ "$cond" == "cli-only" ]] && selected_mcp="$empty_mcp_cfg"
  # Drop ambient user config (global plugin + user CLAUDE.md); pin MCP explicitly.
  CLAUDE_EXTRA=(--setting-sources project,local
                --strict-mcp-config --mcp-config "$selected_mcp"
                --add-dir "$fdir"
                --plugin-dir "$work/plugins/$cond")
  # Hold steering constant for the single-channel ablations; bare gets none.
  if [[ "$cond" == "no-hints" || "$cond" == "no-skills" ]]; then
    CLAUDE_EXTRA+=(--append-system-prompt "$STEER_TEXT")
  fi
}

# ---- select scenarios ------------------------------------------------------ #
python3 - "$EVAL_SCENARIOS_DIR" "${SCENARIOS:-}" "${EVAL_INCLUDE_DEFERRED:-0}" > "$work/selected.tsv" <<'PY'
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
        print("\t".join([s["id"], host, s.get("fixture", "main"), s["prompt"].replace("\t", " ").replace("\n", " ").replace("\r", " ")]))
PY

# ---- run each scenario x host x condition ---------------------------------- #
run_claude() {
  # Uses the global CLAUDE_EXTRA array set by claude_extra_for.
  local prompt="$1" fixture="$2" out="$3" err="$4" model="$5"
  local allowed="Read,Glob,Grep,Task,Agent,Skill,mcp__tracedecay__*,mcp__plugin_tracedecay_graph__*,Bash(tracedecay tool *),Bash($TD tool *)"
  ( cd "$fixture" && HOME="$CLAUDE_EVAL_HOME" CLAUDE_CONFIG_DIR="$CLAUDE_EVAL_CONFIG" PATH="$EVAL_PATH" \
      timeout "$SCENARIO_TIMEOUT" claude -p "$prompt" \
      --output-format stream-json --verbose \
      --model "$model" \
      --permission-mode dontAsk --allowedTools "$allowed" \
      --no-session-persistence \
      "${CLAUDE_EXTRA[@]}" ) </dev/null >"$out" 2>"$err"
}
run_codex() {
  local prompt="$1" fixture="$2" out="$3" err="$4" model="$5"
  HOME="$CODEX_EVAL_HOME" CODEX_HOME="$CODEX_EVAL_CONFIG" PATH="$EVAL_PATH" \
    timeout "$SCENARIO_TIMEOUT" codex -a never -s workspace-write exec "$prompt" --json \
      -C "$fixture" --add-dir "$work" --skip-git-repo-check --ephemeral --ignore-rules \
      -m "$model" </dev/null >"$out" 2>"$err"
}

# Transcript/meta basename includes model identity; ablations append condition.
out_base_for() {
  if [[ "$1" == "full" ]]; then
    echo "$2__$3__$4"
  else
    echo "$2__$3__$4__$1"
  fi
}

# One live scenario x host x model x condition x repetition. Runs in a
# background subshell when PARALLEL > 1, so it must not mutate shared state.
run_one() {
  local cond="$1" sid="$2" host="$3" fixture="$4" prompt="$5" model="$6" base="$7"
  local fdir out err start end rc=0 timed_out=false
  fdir="$(fixture_dir_for "$fixture")"
  out="$run_dir/${base}.stdout.jsonl"
  err="$run_dir/${base}.stderr.log"
  echo "RUN  $sid [$host/$model/$cond] ..."
  start=$(date +%s)
  if [[ "$host" == "claude" ]]; then
    claude_extra_for "$cond" "$fdir"
    run_claude "$prompt" "$fdir" "$out" "$err" "$model" || rc=$?
  else
    run_codex "$prompt" "$fdir" "$out" "$err" "$model" || rc=$?
  fi
  end=$(date +%s)
  [[ $rc -eq 124 ]] && timed_out=true
  cat > "$run_dir/${base}.meta.json" <<JSON
{"scenario_id":"$sid","host":"$host","model":"$model","fixture":"$fixture","channel_condition":"$cond","exit_code":$rc,"duration_s":$((end-start)),"timed_out":$timed_out}
JSON
  echo "     $sid [$host/$model/$cond] rc=$rc dur=$((end-start))s bytes=$(wc -c <"$out")"
}

for cond in $CHANNELS; do
# Provision each condition once, before any concurrent run could race on it.
[[ "$have_plugin" == "1" ]] && provision_variant "$cond"
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
  if [[ "$host" == "claude" ]]; then
    models="$CLAUDE_MODELS"
  else
    models="$CODEX_MODELS"
  fi
  for model in $models; do
  for ((rep = 1; rep <= REPS; rep++)); do
  base="$(out_base_for "$cond" "$sid" "$host" "$model")"
  [[ "$REPS" -gt 1 ]] && base="${base}__r${rep}"
  if [[ "$live" != "1" ]]; then
    echo "DRY  $sid [$host/$model/$cond] fixture=$fixture"
    if [[ "$host" == "claude" ]]; then claude_extra_for "$cond" "$fdir"; fi
    echo "     cwd=$fdir model=$model extra=[${CLAUDE_EXTRA[*]:-}] prompt=\"$prompt\"" >"$run_dir/${base}.stderr.log"
    : >"$run_dir/${base}.stdout.jsonl"
    continue
  fi
  if [[ "$PARALLEL" -gt 1 ]]; then
    while (( $(jobs -rp | wc -l) >= PARALLEL )); do wait -n || true; done
    run_one "$cond" "$sid" "$host" "$fixture" "$prompt" "$model" "$base" &
  else
    run_one "$cond" "$sid" "$host" "$fixture" "$prompt" "$model" "$base"
  fi
  done
  done
done < "$work/selected.tsv"
done
wait

# ---- grade ----------------------------------------------------------------- #
if [[ "$live" == "1" ]]; then
  echo "grading..."
  python3 "$here/grade.py" --run-dir "$run_dir" --scenarios "$EVAL_SCENARIOS_DIR" || true
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
