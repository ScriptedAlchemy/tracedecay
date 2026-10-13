#!/usr/bin/env bash
# Generates learned_skills/<workflow>/generated/ through the production skill
# writer: the workflow's synthetic Claude sessions are imported into an isolated
# daemon, the automation scheduler runs the skill writer on the codex app-server
# backend, and the activated skill is copied back with its provenance.
#
#   evals/agent_adoption/generate_learned_skill.sh <workflow>            # dry run
#   TRACEDECAY_AGENT_EVALS=1 evals/agent_adoption/generate_learned_skill.sh <workflow>
#
# The dry run builds the fixture, configures automation (left disabled),
# imports the sessions, checks the writer's query finds them, and prints the
# remaining plan without a codex turn. A live run costs one writer turn plus at
# most one repair turn; transient backend failures retry up to three times.
#
# Executed directly, this re-enters run.sh, which owns host/daemon isolation and
# fixture setup, and run.sh sources this file once fixture-main is indexed.
#
# Env knobs:
#   LEARNED_SKILL_GENERATE_TIMEOUT  seconds to wait for a writer run (default: 900)
set -euo pipefail

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  [[ $# -eq 1 ]] || { echo "usage: $0 <workflow>" >&2; exit 2; }
  exec env LEARNED_SKILL_GENERATE="$1" "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/run.sh"
fi

wf="$LEARNED_SKILL_GENERATE"
pack="$here/learned_skills/$wf"
gen_out="$pack/generated"
if [[ ! "$wf" =~ ^[a-z0-9_]+$ ]] || ! compgen -G "$pack/sessions/*.jsonl" >/dev/null; then
  echo "error: no generation sessions for workflow '$wf'" >&2; exit 2
fi
if [[ -e "$gen_out" ]]; then
  echo "error: $gen_out already exists; remove it to regenerate" >&2; exit 2
fi

td() { ( cd "$fixture_main" && "$TD" "$@" ); }

codex_bin="$(command -v codex || true)"
[[ -n "$codex_bin" ]] && codex_bin="$(readlink -f "$codex_bin")"

# Binds the absolute codex path the automation backend spawns. Read the
# revision right before writing: any earlier configuration write advances it.
summarizer_mutation() {
  local key="lcm.summarizer_executables.v1" config_json="" project_id revision
  for _ in $(seq 30); do
    config_json="$(td tool --project "$fixture_main" configuration_get \
      --args "{\"key\":\"$key\"}" --json 2>/dev/null || true)"
    [[ "$config_json" == *'"revision_id"'* ]] && break
    sleep 2
  done
  if [[ "$config_json" != *'"revision_id"'* ]]; then
    echo "error: could not read $key: $config_json" >&2; return 1
  fi
  read -r project_id revision < <(python3 -c 'import json,sys
d = json.loads(sys.argv[1])["structuredContent"]
print(d["scope"]["project_id"], d["outcome"]["value"]["payload"]["revision_id"])' "$config_json")
  python3 -c 'import json,sys
project_id, revision, codex, wf = sys.argv[1:]
print(json.dumps({
    "layer": {"kind": "project", "project_id": project_id},
    "key": "lcm.summarizer_executables.v1",
    "value": {"kind": "lcm_summarizer_executables", "value": {
        "cursor_agent": {"state": "unconfigured"},
        "codex": {"state": "configured", "canonical_path": codex, "timeout_secs": 300}}},
    "expected_revision": revision,
    "idempotency_key": f"agent-eval.learned-skill.{wf}.codex-executable",
}))' "$project_id" "$revision" "${codex_bin:-<codex not on PATH>}" "$wf"
}

# The reflector and curator stay off so the writer is not folded into a
# combined review, and the cleared idle gate lets it run on fresh evidence.
automation_args=(automation config set -p "$fixture_main" --backend codex-app-server
  --timeout-secs 300 --memory-curator false --session-reflector false
  --skill-writer true --skill-writer-interval-secs 60 --skill-writer-min-idle-secs '')

if [[ "$live" == "1" ]]; then
  [[ -n "$codex_bin" ]] || { echo "error: codex is not on PATH" >&2; exit 2; }
  [[ -f "$REAL_CODEX_HOME/auth.json" ]] || { echo "error: no codex auth.json in $REAL_CODEX_HOME" >&2; exit 2; }
  mkdir -p "$CODEX_HOME"
  copy_auth_readonly "$REAL_CODEX_HOME/auth.json" "$CODEX_HOME/auth.json"
fi
# Automation stays disabled until the sessions are searchable: a writer tick
# that runs early records no_session_evidence and consumes the activity, and
# the default-enabled curator and reflector must never reach the backend.
td automation config disable -p "$fixture_main" >/dev/null
if [[ -n "$codex_bin" ]]; then
  mutation="$(summarizer_mutation)"
  td tool --project "$fixture_main" configuration_set --args "$mutation" --json >/dev/null
fi
td "${automation_args[@]}" >/dev/null

# The daemon sweeps $HOME/.claude/projects and binds each session by its cwd.
# Scheduler activity and evidence recency use message time, so the sessions
# are re-dated to end now whenever this runs.
sessions_dir="$HOME/.claude/projects/${fixture_main//[\/.]/-}"
mkdir -p "$sessions_dir"
python3 - "$pack/sessions" "$sessions_dir" "$fixture_main" <<'PY'
import datetime as dt, json, os, sys
src, dest, fixture = sys.argv[1:]
rows = {
    fn: [json.loads(ln) for ln in open(os.path.join(src, fn)) if ln.strip()]
    for fn in sorted(os.listdir(src)) if fn.endswith(".jsonl")
}
parse = lambda ts: dt.datetime.fromisoformat(ts.replace("Z", "+00:00"))
latest = max(parse(r["timestamp"]) for rs in rows.values() for r in rs)
shift = dt.datetime.now(dt.timezone.utc) - dt.timedelta(seconds=1) - latest
for fn, rs in rows.items():
    with open(os.path.join(dest, fn), "w") as out:
        for r in rs:
            r["cwd"] = fixture
            r["timestamp"] = (parse(r["timestamp"]) + shift).isoformat(timespec="milliseconds").replace("+00:00", "Z")
            out.write(json.dumps(r) + "\n")
PY
echo "importing $(ls "$sessions_dir" | wc -l) $wf session(s) into $sessions_dir..."
td sessions import --project-path "$fixture_main" >/dev/null
# The writer only sees sessions that match its fixed evidence query, so wait
# until that query finds them in this project's store.
writer_query="workflow correction repeated skill tool pattern"
ingested=0
for _ in $(seq 60); do
  if td tool --project "$fixture_main" message_search --query "$writer_query" \
      --format json --json 2>/dev/null | grep -q 'pcr-'; then
    ingested=1; break
  fi
  sleep 2
done
if [[ "$ingested" != "1" ]]; then
  echo "error: the skill writer's query ($writer_query) finds no imported $wf session" >&2; exit 1
fi
echo "sessions match the skill writer's evidence query."

if [[ "$live" != "1" ]]; then
  echo
  echo "DRY generation plan for $wf (automation is configured and disabled):"
  echo "  copy $REAL_CODEX_HOME/auth.json -> $CODEX_HOME/auth.json (0400, scrubbed on exit)"
  echo "  codex executable: ${codex_bin:-<not on PATH; live run refuses>}"
  echo "  tracedecay automation config enable -p $fixture_main"
  echo "  poll 'tracedecay automation runs list --json' every 15s up to ${LEARNED_SKILL_GENERATE_TIMEOUT:-900}s for a finished skill_writer run"
  echo "  tracedecay automation config disable, then copy the run's active skill:"
  echo "    automation skills view <id> --json -> $gen_out/skill.json"
  echo "    \$HOME/.claude/skills/<slug>/SKILL.md -> $gen_out/<slug>/SKILL.md"
  echo "    run record + validation_gate artifact -> $gen_out/PROVENANCE.json"
  echo "Set TRACEDECAY_AGENT_EVALS=1 to run the writer for real."
  return 0
fi

td automation config enable -p "$fixture_main" >/dev/null

deadline=$(( $(date +%s) + ${LEARNED_SKILL_GENERATE_TIMEOUT:-900} ))
record=""
while [[ -z "$record" ]]; do
  if (( $(date +%s) >= deadline )); then
    echo "error: no finished skill_writer run within ${LEARNED_SKILL_GENERATE_TIMEOUT:-900}s;" \
      "inspect with 'tracedecay automation runs list' in a rerun" >&2
    exit 1
  fi
  sleep 15
  record="$(td automation runs list -p "$fixture_main" --json 2>/dev/null | python3 -c 'import json,sys
try:
    records = json.load(sys.stdin)["records"]
except Exception:
    raise SystemExit(0)
for r in records:
    if r.get("task") == "skill_writer" and r.get("status") in ("succeeded", "failed"):
        print(json.dumps(r))
        break' || true)"
done
# Stop the scheduler before it spends another writer turn.
td automation config disable -p "$fixture_main" >/dev/null

run_id="$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["run_id"])' "$record")"
status="$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["status"])' "$record")"
if [[ "$status" != "succeeded" ]]; then
  echo "error: skill_writer run $run_id $status: $(python3 -c 'import json,sys; print(json.loads(sys.argv[1]).get("error"))' "$record")" >&2
  exit 1
fi
skill_ids="$(td automation skills list --json | python3 -c 'import json,sys
run_id = sys.argv[1]
for s in json.load(sys.stdin)["skills"]:
    m = s["metadata"]
    if m["state"] == "active" and m["provenance"].get("run_id") == run_id:
        print(m["id"])' "$run_id")"
if [[ "$(wc -w <<<"$skill_ids")" -ne 1 ]]; then
  echo "error: run $run_id activated [${skill_ids//$'\n'/ }], expected exactly one skill;" \
    "a no_skill_needed decision means rerun" >&2
  exit 1
fi

mkdir -p "$gen_out"
td automation skills view "$skill_ids" --json >"$gen_out/skill.json"
skill_md="$(grep -l "^skill-id: \"\{0,1\}$skill_ids\"\{0,1\}$" "$HOME"/.claude/skills/*/SKILL.md 2>/dev/null | head -1 || true)"
if [[ -n "$skill_md" ]]; then
  slug="$(basename "$(dirname "$skill_md")")"
  mkdir -p "$gen_out/$slug"
  cp "$skill_md" "$gen_out/$slug/SKILL.md"
else
  echo "notice: $skill_ids was not materialized for Claude; run.sh will skip the generated variant"
fi
gate="$(td automation runs artifact "$run_id" validation_gate -p "$fixture_main" --json 2>/dev/null || echo '{}')"
python3 - "$record" "$gate" "$wf" "$skill_ids" "$("$TD" --version | head -1)" \
  "$(git -C "$repo_root" rev-parse --short HEAD)" \
  >"$gen_out/PROVENANCE.json" <<'PY'
import json, sys
record, gate, wf, skill_id, version, git_sha = sys.argv[1:]
record = json.loads(record)
validation = (json.loads(gate).get("payload") or {})
print(json.dumps({
    "workflow": wf,
    "skill_id": skill_id,
    "run_id": record["run_id"],
    "task": record["task"],
    "status": record["status"],
    "prompt_version": record.get("prompt_version"),
    "backend": record.get("backend"),
    "model": record.get("model"),
    "date": record["completed_at"][:10],
    "accepted_count": record.get("accepted_count"),
    "validation_decision": (validation.get("task_validation") or {}).get("decision"),
    "improvement_gate_decision": (validation.get("improvement_gate") or {}).get("decision"),
    "artifact_kinds": sorted(a["kind"] for a in record.get("artifacts", [])),
    "tracedecay_version": version,
    "git_sha": git_sha,
}, indent=2))
PY
echo "generated $wf skill $skill_ids from run $run_id into $gen_out"
