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
#   <env>/tracedecay-data/        == TRACEDECAY_DATA_DIR (graph, daemon.sock)
#   <env>/results/                results JSONL + markdown summary
#   <env>/env.sh                   sourceable export block for reuse/debugging

make_env_dir() {
  local env_dir
  env_dir="${TMP_ROOT}/eval-env-$(date +%Y%m%d-%H%M%S)-$$"
  mkdir -p "${env_dir}"/{bin,home/.claude,tracedecay-data,results}
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
  local dst="${env_dir}/home/.claude"
  local seeded=0
  if [[ -f "${real_claude}/.credentials.json" ]]; then
    cp -f "${real_claude}/.credentials.json" "${dst}/.credentials.json"
    chmod 600 "${dst}/.credentials.json"
    seeded=1
  fi
  if [[ -n "${ANTHROPIC_API_KEY:-}" ]]; then
    seeded=1
  fi
  if [[ "${seeded}" == "0" ]]; then
    log "WARNING: no ~/.claude/.credentials.json and no ANTHROPIC_API_KEY;"
    log "         'claude -p' will report 'Not logged in' and scenarios will fail."
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
  local env_dir="$1" staged_bin="$2"
  local home_dir="${env_dir}/home"
  log "installing dev plugin into isolated home ${home_dir}"
  # --agent claude: only touch the Claude integration in the isolated home.
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
# Run a corpus against the isolated env
# --------------------------------------------------------------------------
#
# Corpus schema (one JSON object per line):
#   id, category, project_dir, prompt, expected_tools[], anti_tools[],
#   providers[], success
#
# For each scenario we run `claude -p <prompt>` with the isolated env vars, in
# --output-format json so we can recover the session id, then read that
# session's transcript from the ISOLATED CLAUDE_CONFIG_DIR and count how many
# tool_use entries were tracedecay MCP tools vs native tools.
run_corpus() {
  local env_dir="$1" corpus="$2" model="$3"
  [[ -f "${corpus}" ]] || die "corpus not found: ${corpus}"

  # shellcheck source=/dev/null
  source "${env_dir}/env.sh"

  local results="${env_dir}/results/results.jsonl"
  local summary="${env_dir}/results/summary.md"
  : >"${results}"

  local total=0 passed=0
  local scorer="${SCRIPT_DIR}/score.py"

  while IFS= read -r line; do
    [[ -z "${line}" ]] && continue
    total=$((total + 1))

    local id project prompt
    id="$(printf '%s' "${line}" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("id",""))')"
    project="$(printf '%s' "${line}" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("project_dir",""))')"
    prompt="$(printf '%s' "${line}" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("prompt",""))')"

    [[ -n "${prompt}" ]] || { log "scenario ${id}: empty prompt, skipping"; continue; }
    local run_cwd="${project}"
    [[ -d "${run_cwd}" ]] || run_cwd="${DEFAULT_PROJECT}"

    log "scenario ${id}: running (model=${model}, cwd=${run_cwd})"

    # Run claude -p with JSON output to recover the session id. All isolation
    # env vars are already exported via env.sh above.
    local out
    if ! out="$(cd "${run_cwd}" && claude -p "${prompt}" \
        --model "${model}" \
        --output-format json \
        --dangerously-skip-permissions 2>"${env_dir}/results/${id}.stderr")"; then
      log "scenario ${id}: claude -p exited non-zero (see ${id}.stderr)"
    fi
    printf '%s' "${out}" >"${env_dir}/results/${id}.claude.json"

    # Score: find the session transcript in the isolated config and count tools.
    local scored
    scored="$(python3 "${scorer}" \
      --scenario "${line}" \
      --claude-json "${env_dir}/results/${id}.claude.json" \
      --config-dir "${CLAUDE_CONFIG_DIR}" \
      --cwd "${run_cwd}")"
    printf '%s\n' "${scored}" >>"${results}"

    if printf '%s' "${scored}" | python3 -c 'import sys,json;sys.exit(0 if json.load(sys.stdin).get("pass") else 1)'; then
      passed=$((passed + 1))
      log "scenario ${id}: PASS"
    else
      log "scenario ${id}: FAIL"
    fi
  done <"${corpus}"

  # Markdown summary.
  {
    printf '# Hermetic eval results\n\n'
    printf -- '- Env dir: `%s`\n' "${env_dir}"
    printf -- '- Corpus: `%s`\n' "${corpus}"
    printf -- '- Model: `%s`\n' "${model}"
    printf -- '- Dev binary: `%s`\n' "${HERMETIC_TRACEDECAY_BIN}"
    printf -- '- Passed: **%s / %s**\n\n' "${passed}" "${total}"
    printf '| id | pass | tracedecay tools | native tools | session |\n'
    printf '| --- | --- | --- | --- | --- |\n'
    python3 - "${results}" <<'PY'
import json, sys
with open(sys.argv[1]) as fh:
    for ln in fh:
        ln = ln.strip()
        if not ln:
            continue
        r = json.loads(ln)
        print("| {id} | {ok} | {td} | {nat} | {sid} |".format(
            id=r.get("id",""),
            ok="yes" if r.get("pass") else "no",
            td=r.get("tracedecay_tool_uses",0),
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
  local env_dir="$1"
  local built staged
  built="$(build_binary)"
  staged="$(stage_binary "${built}" "${env_dir}")"
  write_env_file "${env_dir}" "${staged}"
  seed_auth "${env_dir}"
  install_plugin "${env_dir}" "${staged}"
  printf '%s\n' "${staged}"
}

usage() {
  cat >&2 <<'EOF'
Usage: run.sh <subcommand> [options]

Subcommands:
  setup                 Build dev binary + create isolated env + install plugin.
  index                 Index a project with the dev binary into the isolated env.
  run                   Run a corpus JSONL against the isolated env and score it.
  smoke                 Full pipeline with one built-in trivial scenario.
  teardown              Remove an env dir.

Common options:
  --env-dir <path>      Reuse an existing env dir (else a fresh one is created).
  --project <path>      Project to index / default cwd (default: main tracedecay checkout).
  --corpus <path.jsonl> Corpus file for `run`.
  --model <name>        Model for claude -p (default: sonnet).
  --debug               Reuse/produce a debug build instead of release (faster).
  --keep                Do not tear down the env dir on exit.

Examples:
  run.sh smoke --debug --keep
  run.sh setup --debug
  run.sh run --env-dir /tmp/eval-env-... --corpus my-corpus.jsonl --model sonnet
EOF
}

main() {
  [[ $# -ge 1 ]] || { usage; exit 2; }
  local sub="$1"; shift

  local env_dir="" project="${DEFAULT_PROJECT}" corpus="" model="sonnet"
  local keep=0
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --env-dir) env_dir="$2"; shift 2 ;;
      --project) project="$2"; shift 2 ;;
      --corpus)  corpus="$2"; shift 2 ;;
      --model)   model="$2"; shift 2 ;;
      --debug)   export BUILD_DEBUG=1; shift ;;
      --keep)    keep=1; shift ;;
      -h|--help) usage; exit 0 ;;
      *) die "unknown option: $1" ;;
    esac
  done

  case "${sub}" in
    teardown)
      [[ -n "${env_dir}" ]] || die "teardown requires --env-dir"
      [[ "${env_dir}" == "${TMP_ROOT}"/eval-env-* ]] || die "refusing to remove non-eval env dir: ${env_dir}"
      rm -rf "${env_dir}"
      log "removed ${env_dir}"
      ;;

    setup)
      [[ -n "${env_dir}" ]] || env_dir="$(make_env_dir)"
      do_setup "${env_dir}" >/dev/null
      log "env ready: ${env_dir}"
      printf '%s\n' "${env_dir}"
      ;;

    index)
      [[ -n "${env_dir}" ]] || die "index requires --env-dir (run setup first)"
      # shellcheck source=/dev/null
      source "${env_dir}/env.sh"
      index_project "${env_dir}" "${HERMETIC_TRACEDECAY_BIN}" "${project}"
      ;;

    run)
      [[ -n "${env_dir}" ]] || die "run requires --env-dir (run setup first)"
      [[ -n "${corpus}" ]] || die "run requires --corpus"
      run_corpus "${env_dir}" "${corpus}" "${model}"
      ;;

    smoke)
      local created=0
      if [[ -z "${env_dir}" ]]; then env_dir="$(make_env_dir)"; created=1; fi
      do_setup "${env_dir}" >/dev/null
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
    "expected_tools": ["tracedecay_context", "tracedecay_search"],
    "anti_tools": ["Grep", "Glob", "Read"],
    "providers": ["sonnet"],
    "success": "Locates decide_hint via a tracedecay tool, not a raw grep/read.",
}))
PY
      run_corpus "${env_dir}" "${smoke_corpus}" "${model}"

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
