#!/usr/bin/env bash
#
# Hermetic eval harness for tracedecay.
#
# Guarantees that triggering evals run against the tracedecay build under
# development in THIS worktree -- never the system-installed binary -- and that
# they never mutate the user's real ~/.claude, ~/.tracedecay, or ~/.cargo/bin.
#
# See ./README.md for the design, isolation guarantees, and limitations.
#
# Subcommands:
#   build      Build (or reuse) the dev binary and stage it at a stable path.
#   setup      Build + create an isolated env dir + install the dev plugin into it.
#   index      Index a target project with the dev binary (into the isolated home).
#   run        Execute a corpus JSONL against the isolated env and score it.
#   smoke      One trivial built-in scenario end-to-end (implies setup+index+run).
#   teardown   Remove an env dir.
#
# Every subcommand that needs an env accepts --env-dir to reuse a prior one;
# otherwise a fresh eval-env-<timestamp> is created under $TMPDIR.

set -euo pipefail

# --------------------------------------------------------------------------
# Paths and constants
# --------------------------------------------------------------------------

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd -P)"
# eval/hermetic -> eval -> worktree root
WORKTREE_ROOT="$(cd -- "${SCRIPT_DIR}/../.." >/dev/null 2>&1 && pwd -P)"

TMP_ROOT="${TMPDIR:-/tmp}"
TMP_ROOT="${TMP_ROOT%/}"

# Default project to index/eval against: the main tracedecay checkout.
DEFAULT_PROJECT="/home/zack/projects/tracedecay"

log()  { printf '[hermetic] %s\n' "$*" >&2; }
die()  { printf '[hermetic] ERROR: %s\n' "$*" >&2; exit 1; }

# --------------------------------------------------------------------------
# Dev binary staging
# --------------------------------------------------------------------------
#
# The installer's which_tracedecay() DELIBERATELY refuses to bake a path that
# lives under a cargo target dir (target/{debug,release}) -- it treats those as
# ephemeral and falls back to a PATH lookup, which would resolve the SYSTEM
# tracedecay. To make the dev build authoritative we copy the freshly built
# binary to a stable, non-cargo-target location inside the env dir and run the
# installer from THAT copy with that dir first on PATH. Then:
#   * current_exe (the staged copy) is baked into hook commands, and
#   * the MCP server command "tracedecay" resolves to the staged copy via PATH.

# Build the dev binary from THIS worktree. Echoes the built artifact path.
build_binary() {
  local profile_dir="release"
  local -a cargo_args=(build --release --bin tracedecay)
  if [[ "${BUILD_DEBUG:-0}" == "1" ]]; then
    profile_dir="debug"
    cargo_args=(build --bin tracedecay)
  fi

  # Isolate build artifacts from the user's normal target dir so a concurrent
  # `cargo` in this worktree is not disturbed and so is_cargo_target_binary can
  # recognise the location deterministically.
  local target_dir="${WORKTREE_ROOT}/target"
  log "building dev binary (${profile_dir}) from ${WORKTREE_ROOT}"
  # CRITICAL: a cargo failure must abort the whole run. Otherwise a stale
  # artifact from a previous build would be silently staged and the eval would
  # run against the WRONG binary -- exactly the failure this harness prevents.
  # We check cargo's exit status explicitly (a bare subshell in a command-sub
  # is not always fatal under set -e), and unless HERMETIC_ALLOW_STALE=1 we
  # refuse to proceed on a build failure even if an old artifact exists.
  local rc=0
  (
    cd "${WORKTREE_ROOT}"
    CARGO_TARGET_DIR="${target_dir}" cargo "${cargo_args[@]}" >&2
  ) || rc=$?
  if [[ "${rc}" -ne 0 ]]; then
    if [[ "${HERMETIC_ALLOW_STALE:-0}" == "1" ]]; then
      log "WARNING: cargo build failed (rc=${rc}); HERMETIC_ALLOW_STALE=1 set, reusing prior artifact"
    else
      die "cargo build failed (rc=${rc}); refusing to run against a possibly stale binary. Set HERMETIC_ALLOW_STALE=1 only if you understand the artifact may be old."
    fi
  fi

  local built="${target_dir}/${profile_dir}/tracedecay"
  [[ -x "${built}" ]] || die "expected built binary not found at ${built}"
  printf '%s\n' "${built}"
}

# Copy the built binary to <env>/bin/tracedecay (a stable non-cargo path).
stage_binary() {
  local built="$1" env_dir="$2"
  local bindir="${env_dir}/bin"
  mkdir -p "${bindir}"
  cp -f "${built}" "${bindir}/tracedecay"
  chmod +x "${bindir}/tracedecay"
  printf '%s\n' "${bindir}/tracedecay"
}

# --------------------------------------------------------------------------
# Env dir lifecycle
# --------------------------------------------------------------------------
#
# Layout of an env dir:
#   <env>/bin/tracedecay          staged dev binary (baked + on PATH)
#   <env>/home/                    fake HOME; installer writes home/.claude/...
#   <env>/home/.claude/           == CLAUDE_CONFIG_DIR (transcripts, plugins)
#   <env>/home/.codex/            == CODEX_HOME (auth copy, plugin cache, sessions)
#   <env>/tracedecay-data/        == TRACEDECAY_DATA_DIR (graph, daemon.sock)
#   <env>/results/                results JSONL + markdown summary
#   <env>/env.sh                   sourceable export block for reuse/debugging

make_env_dir() {
  local env_dir
  env_dir="${TMP_ROOT}/eval-env-$(date +%Y%m%d-%H%M%S)-$$"
  mkdir -p "${env_dir}"/{bin,home/.claude,home/.codex,tracedecay-data,results}
  printf '%s\n' "${env_dir}"
}

# Write env.sh into an env dir so it can be sourced by `run`/`smoke` and by a
# human debugging with --keep.
write_env_file() {
  local env_dir="$1" staged_bin="$2"
  local home_dir="${env_dir}/home"
  cat >"${env_dir}/env.sh" <<EOF
# Sourceable isolation env for a hermetic tracedecay eval.
# Generated by eval/hermetic/run.sh -- safe to source in a throwaway shell.
export HERMETIC_ENV_DIR="${env_dir}"
export HOME="${home_dir}"
export CLAUDE_CONFIG_DIR="${home_dir}/.claude"
export CODEX_HOME="${home_dir}/.codex"
export TRACEDECAY_DATA_DIR="${env_dir}/tracedecay-data"
# Daemon socket derives from TRACEDECAY_DATA_DIR (data_dir/daemon.sock); pin it
# explicitly too so we never touch the real daemon socket.
export TRACEDECAY_DAEMON_SOCKET="${env_dir}/tracedecay-data/daemon.sock"
# Staged dev binary first on PATH so the MCP "tracedecay" command resolves here.
export PATH="${env_dir}/bin:\${PATH}"
export HERMETIC_TRACEDECAY_BIN="${staged_bin}"
EOF
}

# Copy just enough of the real ~/.claude for `claude -p` to authenticate.
# We do NOT symlink the whole dir (that would let the isolated session mutate
# the real config). We copy credentials read-only-ish and let claude create the
# rest fresh under the isolated CLAUDE_CONFIG_DIR.
seed_auth() {
  local env_dir="$1"
  local real_claude="${REAL_CLAUDE_DIR:-${ORIG_HOME}/.claude}"
  local real_codex="${REAL_CODEX_HOME:-${ORIG_HOME}/.codex}"
  local dst="${env_dir}/home/.claude"
  local seeded=0
  if [[ -f "${real_claude}/.credentials.json" ]]; then
    cp -f "${real_claude}/.credentials.json" "${dst}/.credentials.json"
    chmod 600 "${dst}/.credentials.json"
    seeded=1
  fi
  if [[ -n "${ANTHROPIC_API_KEY:-}" || -n "${CLAUDE_CODE_OAUTH_TOKEN:-}" ]]; then
    seeded=1
  fi
  # A long-lived setup-token grant (claude setup-token) also authenticates
  # claude -p; surface it into env.sh so run/smoke inherit it after sourcing.
  if [[ -z "${CLAUDE_CODE_OAUTH_TOKEN:-}" && -f "${real_claude}/.claude_code_oauth_token" ]]; then
    seeded=1
    printf 'export CLAUDE_CODE_OAUTH_TOKEN=%q\n' "$(<"${real_claude}/.claude_code_oauth_token")" \
      >>"${env_dir}/env.sh"
  fi
  if [[ "${seeded}" == "0" ]]; then
    log "WARNING: no ~/.claude/.credentials.json and no ANTHROPIC_API_KEY;"
    log "         'claude -p' will report 'Not logged in' and scenarios will fail."
  fi
  if [[ -f "${real_codex}/auth.json" ]]; then
    cp -f "${real_codex}/auth.json" "${env_dir}/home/.codex/auth.json"
    chmod 600 "${env_dir}/home/.codex/auth.json"
  else
    log "WARNING: no ~/.codex/auth.json; 'codex exec' may require login."
  fi
}

# --------------------------------------------------------------------------
# Install the dev plugin into the isolated home
# --------------------------------------------------------------------------
#
# Runs the STAGED dev binary's installer with HOME pointed at the isolated home.
# This bakes the staged binary path into hooks, deploys the plugin bundle from
# THIS worktree's plugin/ dir, and writes the permission allowlist -- validating
# the dev installer (incl. plugin-namespace permissions) end-to-end.
install_plugin() {
  local env_dir="$1" staged_bin="$2" agent="$3"
  local home_dir="${env_dir}/home"
  log "installing dev plugin for ${agent} into isolated home ${home_dir}"
  case "${agent}" in
    claude)
      HOME="${home_dir}" \
      CLAUDE_CONFIG_DIR="${home_dir}/.claude" \
      TRACEDECAY_DATA_DIR="${env_dir}/tracedecay-data" \
      TRACEDECAY_DAEMON_SOCKET="${env_dir}/tracedecay-data/daemon.sock" \
      PATH="${env_dir}/bin:${PATH}" \
        "${staged_bin}" install --agent claude >&2 \
        || die "dev installer failed"

      # Sanity: confirm the baked hook path is the staged dev binary, not system.
      local mkt_dir="${home_dir}/.claude/plugins/marketplaces/tracedecay"
      local -a hooks=()
      if [[ -d "${mkt_dir}" ]]; then
        while IFS= read -r f; do
          [[ -n "${f}" ]] && hooks+=("${f}")
        done < <(grep -Rl '"command"' "${mkt_dir}" 2>/dev/null || true)
      fi
      if [[ ${#hooks[@]} -gt 0 ]]; then
        if grep -q "${staged_bin}" "${hooks[@]}" 2>/dev/null; then
          log "verified: hooks baked with staged dev binary ${staged_bin}"
        else
          log "WARNING: could not confirm staged binary in baked hooks; inspect ${mkt_dir}"
        fi
      fi
      ;;
    codex)
      HOME="${home_dir}" \
      CODEX_HOME="${home_dir}/.codex" \
      TRACEDECAY_DATA_DIR="${env_dir}/tracedecay-data" \
      TRACEDECAY_DAEMON_SOCKET="${env_dir}/tracedecay-data/daemon.sock" \
      PATH="${env_dir}/bin:${PATH}" \
        "${staged_bin}" install --agent codex >&2 \
        || die "dev installer failed"
      HOME="${home_dir}" \
      CODEX_HOME="${home_dir}/.codex" \
      TRACEDECAY_DATA_DIR="${env_dir}/tracedecay-data" \
      TRACEDECAY_DAEMON_SOCKET="${env_dir}/tracedecay-data/daemon.sock" \
      PATH="${env_dir}/bin:${PATH}" \
        codex plugin add tracedecay@personal --json >/dev/null \
        || die "codex plugin add tracedecay@personal failed"
      if [[ -d "${home_dir}/.codex/plugins/cache/personal/tracedecay" ]]; then
        log "verified: Codex plugin cache installed under isolated CODEX_HOME"
      else
        log "WARNING: could not confirm Codex plugin cache under ${home_dir}/.codex"
      fi
      ;;
    *)
      die "unsupported agent: ${agent}"
      ;;
  esac
}

run_agent_turn() {
  local agent="$1" model="$2" prompt="$3" cwd="$4" env_dir="$5" id="$6"
  local out
  case "${agent}" in
    claude)
      # `</dev/null`: the agent must never inherit the caller's stdin — the
      # corpus while-read loop feeds from it, and an agent that slurps stdin
      # (codex exec does) would silently eat every remaining scenario line.
      out="$(cd "${cwd}" && claude -p "${prompt}" \
          --model "${model:-sonnet}" \
          --output-format json \
          --dangerously-skip-permissions </dev/null 2>"${env_dir}/results/${id}.stderr")" || \
        log "scenario ${id}: claude -p exited non-zero (see ${id}.stderr)"
      printf '%s' "${out}" >"${env_dir}/results/${id}.claude.json"
      ;;
    codex)
      local -a cmd=(codex exec --json --cd "${cwd}" --dangerously-bypass-approvals-and-sandbox --dangerously-bypass-hook-trust)
      if [[ -n "${model}" ]]; then
        cmd+=(--model "${model}")
      fi
      cmd+=("${prompt}")
      if ! (cd "${cwd}" && "${cmd[@]}" </dev/null >"${env_dir}/results/${id}.codex.jsonl" 2>"${env_dir}/results/${id}.stderr"); then
        log "scenario ${id}: codex exec exited non-zero (see ${id}.stderr)"
      fi
      ;;
    *)
      die "unsupported agent: ${agent}"
      ;;
  esac
}

score_agent_turn() {
  local agent="$1" line="$2" cwd="$3" env_dir="$4" id="$5" verify_status="${6:-}" rep="${7:-1}"
  local -a extra=(--rep "${rep}")
  if [[ -n "${verify_status}" ]]; then
    extra+=(--verify-status "${verify_status}")
  fi
  case "${agent}" in
    claude)
      python3 "${SCRIPT_DIR}/score.py" \
        --agent claude \
        --scenario "${line}" \
        --claude-json "${env_dir}/results/${id}.claude.json" \
        --config-dir "${CLAUDE_CONFIG_DIR}" \
        --cwd "${cwd}" \
        "${extra[@]}"
      ;;
    codex)
      python3 "${SCRIPT_DIR}/score.py" \
        --agent codex \
        --scenario "${line}" \
        --codex-jsonl "${env_dir}/results/${id}.codex.jsonl" \
        --cwd "${cwd}" \
        "${extra[@]}"
      ;;
    *)
      die "unsupported agent: ${agent}"
      ;;
  esac
}

default_model_for_agent() {
  case "$1" in
    claude) printf 'sonnet\n' ;;
    codex) printf '\n' ;;
    *) die "unsupported agent: $1" ;;
  esac
}

agent_label() {
  local agent="$1" model="$2"
  if [[ -n "${model}" ]]; then
    printf '%s/%s\n' "${agent}" "${model}"
  else
    printf '%s/default\n' "${agent}"
  fi
}

# --------------------------------------------------------------------------
# Index a target project with the dev binary (into the isolated data dir)
# --------------------------------------------------------------------------
index_project() {
  local env_dir="$1" staged_bin="$2" project="$3"
  [[ -d "${project}" ]] || die "project dir does not exist: ${project}"
  log "indexing ${project} with dev binary (isolated data dir)"
  HOME="${env_dir}/home" \
  TRACEDECAY_DATA_DIR="${env_dir}/tracedecay-data" \
  TRACEDECAY_DAEMON_SOCKET="${env_dir}/tracedecay-data/daemon.sock" \
  PATH="${env_dir}/bin:${PATH}" \
    "${staged_bin}" init "${project}" >&2 \
    || die "indexing failed for ${project}"
}

# --------------------------------------------------------------------------
# Stage fixture projects into the isolated env
# --------------------------------------------------------------------------
#
# Copies eval/hermetic/fixtures/* into <env>/fixtures/ and indexes each with
# the dev binary. Corpus scenarios reference them as project_dir
# "fixture:<name>", resolved at run time to <env>/fixtures/<name>. Re-running
# re-copies, so it doubles as the between-reps reset for edit scenarios.
stage_fixtures() {
  local env_dir="$1" staged_bin="$2"
  local src_root="${SCRIPT_DIR}/fixtures"
  [[ -d "${src_root}" ]] || { log "no fixtures dir at ${src_root}, skipping"; return 0; }
  mkdir -p "${env_dir}/fixtures"
  local fixture
  for fixture in "${src_root}"/*/; do
    local name
    name="$(basename "${fixture}")"
    rm -rf "${env_dir:?}/fixtures/${name}"
    cp -R "${fixture%/}" "${env_dir}/fixtures/${name}"
    # The argv-cap scenario needs a >128 KiB payload file; generate it rather
    # than committing a blob.
    if [[ "${name}" == "tool-args" ]]; then
      python3 - "${env_dir}/fixtures/${name}/cargo-output.txt" <<'PY'
import sys
line = "error[E0308]: mismatched types in fixture module alpha::beta — expected `i32`, found `String`\n"
with open(sys.argv[1], "w") as fh:
    fh.write(line * 2000)  # ~190 KiB, comfortably over MAX_ARG_STRLEN
PY
    fi
    reindex_project "${env_dir}" "${staged_bin}" "${env_dir}/fixtures/${name}"
  done
}

# Index a project, tolerating re-staging: `init` refuses when the project is
# already registered in the isolated data dir (it advises `sync`), so fall
# back to a forced sync to rebuild the index for the fresh copy.
reindex_project() {
  local env_dir="$1" staged_bin="$2" project="$3"
  if HOME="${env_dir}/home" \
     TRACEDECAY_DATA_DIR="${env_dir}/tracedecay-data" \
     TRACEDECAY_DAEMON_SOCKET="${env_dir}/tracedecay-data/daemon.sock" \
     PATH="${env_dir}/bin:${PATH}" \
       "${staged_bin}" init "${project}" >&2; then
    return 0
  fi
  log "init refused for ${project} (already registered); running sync --force"
  HOME="${env_dir}/home" \
  TRACEDECAY_DATA_DIR="${env_dir}/tracedecay-data" \
  TRACEDECAY_DAEMON_SOCKET="${env_dir}/tracedecay-data/daemon.sock" \
  PATH="${env_dir}/bin:${PATH}" \
    "${staged_bin}" sync "${project}" --force >&2 \
    || die "re-indexing failed for ${project}"
}

# Resolve a corpus project_dir: "fixture:<name>" targets the staged fixture
# copy inside the env dir; anything else is a literal path.
resolve_project_dir() {
  local env_dir="$1" project="$2"
  if [[ "${project}" == fixture:* ]]; then
    printf '%s/fixtures/%s\n' "${env_dir}" "${project#fixture:}"
  else
    printf '%s\n' "${project}"
  fi
}

# --------------------------------------------------------------------------
# Run a corpus against the isolated env
# --------------------------------------------------------------------------
#
# Corpus schema (one JSON object per line):
#   id, category, project_dir, prompt, expected_tools[], expected_cli[],
#   anti_tools[], providers[], success
# Optional per-scenario fields:
#   setup_cmd    shell command run in project_dir before the agent session
#                (restore fixture state between reps)
#   verify_cmd   shell command run in project_dir after the session with the
#                staged binary first on PATH; its exit status is folded into
#                the scenario's pass as verify_pass
#   attempt_tool tool-name fragment counted across captured CLI commands to
#                produce tool_cmd_attempts / self_corrected
#
# For each scenario we run Claude/Sonnet or Codex with the isolated env vars,
# then count tracedecay MCP tool calls and tracedecay CLI fallback commands.
run_corpus() {
  local env_dir="$1" corpus="$2" model="$3" agent="$4" default_project="${5:-$DEFAULT_PROJECT}" reps="${6:-1}"
  [[ -f "${corpus}" ]] || die "corpus not found: ${corpus}"

  # shellcheck source=/dev/null
  source "${env_dir}/env.sh"

  local results="${env_dir}/results/results.jsonl"
  local summary="${env_dir}/results/summary.md"
  : >"${results}"

  local total=0 passed=0 label rep
  label="$(agent_label "${agent}" "${model}")"

  for (( rep=1; rep<=reps; rep++ )); do
    if [[ "${reps}" -gt 1 && -d "${env_dir}/fixtures" ]]; then
      log "rep ${rep}/${reps}: resetting staged fixtures"
      stage_fixtures "${env_dir}" "${HERMETIC_TRACEDECAY_BIN}"
    fi
    while IFS= read -r line; do
    [[ -z "${line}" ]] && continue
    total=$((total + 1))

    local id project prompt setup_cmd verify_cmd
    id="$(printf '%s' "${line}" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("id",""))')"
    project="$(printf '%s' "${line}" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("project_dir",""))')"
    prompt="$(printf '%s' "${line}" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("prompt",""))')"
    setup_cmd="$(printf '%s' "${line}" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("setup_cmd",""))')"
    verify_cmd="$(printf '%s' "${line}" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("verify_cmd",""))')"

    [[ -n "${prompt}" ]] || { log "scenario ${id}: empty prompt, skipping"; continue; }
    local run_cwd
    run_cwd="$(resolve_project_dir "${env_dir}" "${project}")"
    [[ -d "${run_cwd}" ]] || run_cwd="${default_project}"

    local run_id="${id}"
    [[ "${reps}" -gt 1 ]] && run_id="${id}.r${rep}"

    if [[ -n "${setup_cmd}" ]]; then
      log "scenario ${id}: setup (${setup_cmd})"
      (cd "${run_cwd}" && bash -c "${setup_cmd}") \
        || log "scenario ${id}: WARNING setup_cmd failed"
    fi

    log "scenario ${id} rep ${rep}: running (agent=${label}, cwd=${run_cwd})"
    run_agent_turn "${agent}" "${model}" "${prompt}" "${run_cwd}" "${env_dir}" "${run_id}"

    # Post-session effect check, with the staged dev binary first on PATH.
    local verify_status=""
    if [[ -n "${verify_cmd}" ]]; then
      if (cd "${run_cwd}" && PATH="${env_dir}/bin:${PATH}" bash -c "${verify_cmd}" \
            >"${env_dir}/results/${run_id}.verify.log" 2>&1); then
        verify_status=0
      else
        verify_status=1
      fi
      log "scenario ${id}: verify_cmd exit ${verify_status}"
    fi

    # Score: inspect the isolated transcript/output and count tools/commands.
    local scored
    scored="$(score_agent_turn "${agent}" "${line}" "${run_cwd}" "${env_dir}" "${run_id}" "${verify_status}" "${rep}")"
    printf '%s\n' "${scored}" >>"${results}"

    if printf '%s' "${scored}" | python3 -c 'import sys,json;sys.exit(0 if json.load(sys.stdin).get("pass") else 1)'; then
      passed=$((passed + 1))
      log "scenario ${id} rep ${rep}: PASS"
    else
      log "scenario ${id} rep ${rep}: FAIL"
    fi
    done <"${corpus}"
  done

  # Markdown summary.
  {
    printf '# Hermetic eval results\n\n'
    printf -- '- Env dir: `%s`\n' "${env_dir}"
    printf -- '- Corpus: `%s`\n' "${corpus}"
    printf -- '- Agent: `%s`\n' "${label}"
    printf -- '- Dev binary: `%s`\n' "${HERMETIC_TRACEDECAY_BIN}"
    printf -- '- Passed: **%s / %s**\n\n' "${passed}" "${total}"
    printf '| id | rep | pass | tracedecay tools | CLI commands | attempts | verify | native tools | session |\n'
    printf '| --- | --- | --- | --- | --- | --- | --- | --- | --- |\n'
    python3 - "${results}" <<'PY'
import json, sys
with open(sys.argv[1]) as fh:
    for ln in fh:
        ln = ln.strip()
        if not ln:
            continue
        r = json.loads(ln)
        vp = r.get("verify_pass")
        verify = "-" if vp is None else ("yes" if vp else "no")
        print("| {id} | {rep} | {ok} | {td} | {cli} | {attempts} | {verify} | {nat} | {sid} |".format(
            id=r.get("id",""),
            rep=r.get("rep", 1),
            ok="yes" if r.get("pass") else "no",
            td=r.get("tracedecay_tool_uses",0),
            cli=r.get("cli_command_uses",0),
            attempts=r.get("tool_cmd_attempts", ""),
            verify=verify,
            nat=r.get("native_tool_uses",0),
            sid=(r.get("session_id") or "")[:8],
        ))
PY
  } >"${summary}"

  log "results: ${results}"
  log "summary: ${summary}"
  log "SCORE: ${passed}/${total} passed"
  # Echo summary to stdout for the caller.
  cat "${summary}"
}

# --------------------------------------------------------------------------
# High-level orchestration
# --------------------------------------------------------------------------

ORIG_HOME="${HOME}"

do_setup() {
  local env_dir="$1" agent="$2"
  local built staged
  built="$(build_binary)"
  staged="$(stage_binary "${built}" "${env_dir}")"
  write_env_file "${env_dir}" "${staged}"
  seed_auth "${env_dir}"
  install_plugin "${env_dir}" "${staged}" "${agent}"
  printf '%s\n' "${staged}"
}

usage() {
  cat >&2 <<'EOF'
Usage: run.sh <subcommand> [options]

Subcommands:
  setup                 Build dev binary + create isolated env + install plugin.
  index                 Index a project with the dev binary into the isolated env.
  fixtures              Copy fixture projects into the env and index them.
  run                   Run a corpus JSONL against the isolated env and score it.
  smoke                 Full pipeline with one built-in trivial scenario.
  teardown              Remove an env dir.

Common options:
  --env-dir <path>      Reuse an existing env dir (else a fresh one is created).
  --agent <name>        Agent driver: claude or codex (default: claude).
  --project <path>      Project to index / default cwd (default: main tracedecay checkout).
  --corpus <path.jsonl> Corpus file for `run`.
  --model <name>        Model override (default: sonnet for claude; Codex default for codex).
  --reps <N>            Re-run the corpus N times (default: 1; `run` only).
  --debug               Reuse/produce a debug build instead of release (faster).
  --keep                Do not tear down the env dir on exit.

Examples:
  run.sh smoke --agent claude --debug --keep
  run.sh setup --agent codex --debug
  run.sh run --agent claude --env-dir /tmp/eval-env-... --corpus my-corpus.jsonl --model sonnet
EOF
}

main() {
  [[ $# -ge 1 ]] || { usage; exit 2; }
  local sub="$1"; shift

  local env_dir="" project="${DEFAULT_PROJECT}" corpus="" model="" agent="claude"
  local keep=0 reps=1
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --env-dir) env_dir="$2"; shift 2 ;;
      --agent)   agent="$2"; shift 2 ;;
      --project) project="$2"; shift 2 ;;
      --corpus)  corpus="$2"; shift 2 ;;
      --model)   model="$2"; shift 2 ;;
      --reps)    reps="$2"; shift 2 ;;
      --debug)   export BUILD_DEBUG=1; shift ;;
      --keep)    keep=1; shift ;;
      -h|--help) usage; exit 0 ;;
      *) die "unknown option: $1" ;;
    esac
  done
  case "${agent}" in
    claude|codex) ;;
    *) die "unsupported agent: ${agent}" ;;
  esac
  if [[ -z "${model}" ]]; then
    model="$(default_model_for_agent "${agent}")"
  fi

  case "${sub}" in
    teardown)
      [[ -n "${env_dir}" ]] || die "teardown requires --env-dir"
      [[ "${env_dir}" == "${TMP_ROOT}"/eval-env-* ]] || die "refusing to remove non-eval env dir: ${env_dir}"
      rm -rf "${env_dir}"
      log "removed ${env_dir}"
      ;;

    setup)
      [[ -n "${env_dir}" ]] || env_dir="$(make_env_dir)"
      do_setup "${env_dir}" "${agent}" >/dev/null
      log "env ready: ${env_dir}"
      printf '%s\n' "${env_dir}"
      ;;

    index)
      [[ -n "${env_dir}" ]] || die "index requires --env-dir (run setup first)"
      # shellcheck source=/dev/null
      source "${env_dir}/env.sh"
      index_project "${env_dir}" "${HERMETIC_TRACEDECAY_BIN}" "${project}"
      ;;

    fixtures)
      [[ -n "${env_dir}" ]] || die "fixtures requires --env-dir (run setup first)"
      # shellcheck source=/dev/null
      source "${env_dir}/env.sh"
      stage_fixtures "${env_dir}" "${HERMETIC_TRACEDECAY_BIN}"
      ;;

    run)
      [[ -n "${env_dir}" ]] || die "run requires --env-dir (run setup first)"
      [[ -n "${corpus}" ]] || die "run requires --corpus"
      run_corpus "${env_dir}" "${corpus}" "${model}" "${agent}" "${project}" "${reps}"
      ;;

    smoke)
      local created=0
      if [[ -z "${env_dir}" ]]; then env_dir="$(make_env_dir)"; created=1; fi
      do_setup "${env_dir}" "${agent}" >/dev/null
      # shellcheck source=/dev/null
      source "${env_dir}/env.sh"
      index_project "${env_dir}" "${HERMETIC_TRACEDECAY_BIN}" "${project}"

      local smoke_corpus="${env_dir}/smoke-corpus.jsonl"
      python3 - "${project}" >"${smoke_corpus}" <<'PY'
import json, sys
print(json.dumps({
    "id": "smoke-001",
    "category": "exploring_code",
    "project_dir": sys.argv[1],
    "prompt": "where is decide_hint defined? brief",
    "expected_tools": ["tracedecay"],
    "anti_tools": ["Grep", "Glob", "Read"],
    "providers": ["sonnet", "codex"],
    "success": "Locates decide_hint via a tracedecay tool, not a raw grep/read.",
}))
PY
      run_corpus "${env_dir}" "${smoke_corpus}" "${model}" "${agent}" "${project}"

      if [[ "${keep}" == "1" ]]; then
        log "kept env dir: ${env_dir}"
      elif [[ "${created}" == "1" ]]; then
        rm -rf "${env_dir}"
        log "removed env dir ${env_dir} (pass --keep to preserve)"
      fi
      ;;

    *) usage; exit 2 ;;
  esac

  if [[ "${keep}" == "1" && -n "${env_dir}" && -d "${env_dir}" ]]; then
    log "env preserved: ${env_dir}"
  fi
}

main "$@"
