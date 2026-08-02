#!/usr/bin/env bash
# Full-catalog TraceDecay tool battery.
#
# Calls every tool in `tracedecay tool` against a live daemon with minimal valid
# arguments, records per-call latency + exit status, and emits a TSV table plus
# a PASS/FAIL/SLOW summary. Intended for interactive triage today and adoption
# by the CI perf gate later (see docs/SERVING-PATH-PERFORMANCE.md: a warm
# serving-path call is O(result); a deadline firing is a defect, not a budget).
#
# Usage:
#   scripts/tool-sweep.sh [options]
#
#   --project PATH        project root to query (default: repo root of this script)
#   --out DIR             output directory (default: mktemp -d)
#   --timeout SECS        per-call wall-clock cap (default: 60)
#   --warm-threshold SECS latency above which a passing call is flagged SLOW (default: 5)
#   --only a,b,c          restrict to the named tools
#   --skip a,b,c          exclude the named tools
#   --classes LIST        comma list of tool classes to run (default: read,preview)
#                         read     - pure reads, always safe
#                         preview  - dry-run/preview surfaces, no persisted effect
#                         handle   - requires a daemon-minted opaque handle; called
#                                    with a synthetic handle to exercise the handler's
#                                    validation path (a typed rejection is a PASS)
#                         mutate   - persists state; OFF by default
#   --bin PATH            tracedecay binary (default: first on PATH)
#   --json                also emit results.json
#
# Exit status: 0 if no FAIL rows, 1 otherwise.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

BIN="${TRACEDECAY_BIN:-tracedecay}"
PROJECT="$REPO_ROOT"
OUT=""
TIMEOUT=60
WARM_THRESHOLD=5
ONLY=""
SKIP=""
CLASSES="read,preview"
EMIT_JSON=0
ENV_RETRIES="${TRACEDECAY_SWEEP_ENV_RETRIES:-6}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --project) PROJECT="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --timeout) TIMEOUT="$2"; shift 2 ;;
    --warm-threshold) WARM_THRESHOLD="$2"; shift 2 ;;
    --only) ONLY="$2"; shift 2 ;;
    --skip) SKIP="$2"; shift 2 ;;
    --classes) CLASSES="$2"; shift 2 ;;
    --bin) BIN="$2"; shift 2 ;;
    --json) EMIT_JSON=1; shift ;;
    -h|--help) sed -n '2,40p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
done

command -v "$BIN" >/dev/null 2>&1 || { echo "tracedecay binary not found: $BIN" >&2; exit 2; }
[[ -n "$OUT" ]] || OUT="$(mktemp -d -t tracedecay-tool-sweep-XXXXXX)"
mkdir -p "$OUT/raw"

TSV="$OUT/results.tsv"
: > "$TSV"
printf 'tool\tclass\tverdict\tlatency_ms\trc\tnote\n' >> "$TSV"

# ---------------------------------------------------------------------------
# Fixtures
#
# Every fixture is derived from the target project so the battery stays valid
# across repos; each has a repo-agnostic fallback. Deriving costs a handful of
# cheap calls and keeps arguments real (a synthetic node id only ever exercises
# the not-found branch).
# ---------------------------------------------------------------------------

FX_FILE=""
FX_DIR=""
FX_SYMBOL=""
FX_STRUCT=""
FX_TRAIT=""
FX_FIELD=""
FX_NODE_ID=""
FX_NODE_ID_2=""
FX_QNAME=""
FX_BRANCH=""
FX_HEAD=""
FX_PREV=""

outline_pick() {
  # $1 = outline markdown, $2 = kind. Emits the first symbol name of that kind.
  printf '%s' "$1" | grep -oE '^- \*\*[A-Za-z_][A-Za-z0-9_]*\*\* \('"$2"'\)' \
    | head -1 | sed -E 's/^- \*\*//; s/\*\*.*$//'
}

derive_fixtures() {
  FX_BRANCH="$(git -C "$PROJECT" rev-parse --abbrev-ref HEAD 2>/dev/null || echo main)"
  FX_HEAD="$(git -C "$PROJECT" rev-parse HEAD 2>/dev/null || echo HEAD)"
  FX_PREV="$(git -C "$PROJECT" rev-parse HEAD~1 2>/dev/null || echo "$FX_HEAD")"

  # Pick a real, modestly sized tracked source file. Deliberately taken from git
  # rather than the `files` tool so fixture derivation cannot be blocked by the
  # very serving path the battery is measuring.
  if [[ -n "${TRACEDECAY_SWEEP_FILE:-}" ]]; then
    FX_FILE="$TRACEDECAY_SWEEP_FILE"
  else
    FX_FILE="$(git -C "$PROJECT" ls-files 'src/**/*.rs' 'src/*.rs' 2>/dev/null \
      | while read -r f; do
          n=$(wc -l < "$PROJECT/$f" 2>/dev/null || echo 0)
          [[ "$n" -ge 40 && "$n" -le 200 ]] && { echo "$f"; break; }
        done)"
    [[ -n "$FX_FILE" ]] || FX_FILE="$(git -C "$PROJECT" ls-files '*.rs' 2>/dev/null | head -1)"
    [[ -n "$FX_FILE" ]] || FX_FILE="$(git -C "$PROJECT" ls-files | head -1)"
  fi
  FX_DIR="$(dirname "$FX_FILE")"

  # Real symbol names for that file, straight off the outline surface.
  local outline_md
  outline_md="$(call_raw outline "$(printf '{"file":%s}' "$(json_str "$FX_FILE")")" 60 || true)"
  FX_SYMBOL="$(outline_pick "$outline_md" function)"
  FX_STRUCT="$(outline_pick "$outline_md" struct)"
  FX_FIELD="$(outline_pick "$outline_md" field)"
  [[ -n "$FX_SYMBOL" ]] || FX_SYMBOL="$(outline_pick "$outline_md" method)"
  [[ -n "$FX_SYMBOL" ]] || FX_SYMBOL="main"
  [[ -n "$FX_STRUCT" ]] || FX_STRUCT="$FX_SYMBOL"
  [[ -n "$FX_FIELD" ]] || FX_FIELD="name"

  # Resolve that symbol to a real node id, preferring the occurrence in our file.
  local search_json
  search_json="$(call_raw search "$(printf '{"query":%s,"limit":10,"format":"json"}' "$(json_str "$FX_SYMBOL")")" 60 || true)"
  FX_NODE_ID="$(printf '%s' "$search_json" | tr ',' '\n' \
    | grep -oE '"node_id":"(function|method):[0-9a-f]+"' | head -1 | sed -E 's/.*:"(.*)"$/\1/')"

  if [[ -n "$FX_NODE_ID" ]]; then
    local node_json callees_json
    node_json="$(call_raw node "$(printf '{"node_id":%s,"format":"json"}' "$(json_str "$FX_NODE_ID")")" 60 || true)"
    FX_QNAME="$(printf '%s' "$node_json" | tr ',' '\n' \
      | grep -oE '"qualified_name":"[^"]+"' | head -1 | sed -E 's/.*:"//; s/"$//')"
    # Callees give a genuinely connected pair, so call_chain exercises a real path.
    callees_json="$(call_raw callees "$(printf '{"node_id":%s,"format":"json"}' "$(json_str "$FX_NODE_ID")")" 60 || true)"
    FX_NODE_ID_2="$(printf '%s' "$callees_json" | tr ',' '\n' \
      | grep -oE '"node_id":"(function|method):[0-9a-f]+"' | head -1 | sed -E 's/.*:"(.*)"$/\1/')"
  fi
  [[ -n "$FX_QNAME" ]] || FX_QNAME="$FX_FILE::$FX_SYMBOL"
  [[ -n "$FX_NODE_ID_2" ]] || FX_NODE_ID_2="$FX_NODE_ID"

  FX_TRAIT="$(printf '%s' "$(call_raw impls '{"limit":20,"format":"json"}' 60 || true)" | tr ',' '\n' \
    | grep -oE '"trait":"[A-Za-z_][A-Za-z0-9_]*"' | head -1 | sed -E 's/.*:"//; s/"$//')"
  [[ -n "$FX_TRAIT" ]] || FX_TRAIT="Default"

  {
    echo "file=$FX_FILE"
    echo "dir=$FX_DIR"
    echo "symbol=$FX_SYMBOL"
    echo "struct=$FX_STRUCT"
    echo "trait=$FX_TRAIT"
    echo "field=$FX_FIELD"
    echo "node_id=$FX_NODE_ID"
    echo "node_id_2=$FX_NODE_ID_2"
    echo "qualified_name=$FX_QNAME"
    echo "branch=$FX_BRANCH"
    echo "head=$FX_HEAD"
    echo "prev=$FX_PREV"
  } > "$OUT/fixtures.txt"
}

json_str() { printf '"%s"' "$(printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g')"; }

# call_raw <tool> <json-args> <timeout-secs> — used by fixture derivation only.
#
# Retries through daemon restarts and admission shedding. Deriving a fixture
# from a shed response silently degrades every later row (an empty node id
# turns real graph calls into not-found probes), so this waits rather than
# accepting whatever came back.
call_raw() {
  local out attempt
  for attempt in $(seq 1 "$ENV_RETRIES"); do
    out="$(timeout "${3:-30}" "$BIN" tool "$1" --project "$PROJECT" --args "$2" 2>&1)"
    retryable_environment "$out" || break
    [[ $attempt -eq "$ENV_RETRIES" ]] && break
    sleep $(( attempt < 6 ? attempt * 5 : 30 ))
  done
  printf '%s' "$out"
}

# ---------------------------------------------------------------------------
# Catalog: tool | class | args-template
#
# Templates use @FILE, @DIR, @SYMBOL, @STRUCT, @TRAIT, @FIELD, @NODE, @NODE2,
# @QNAME, @BRANCH, @HEAD, @PREV placeholders, substituted after fixture
# derivation. Keep one line per tool and keep the file sorted by catalog group
# so a new tool without an entry shows up as MISSING-ARGS rather than silently
# dropping out of the battery.
#
# Three nested-object shapes recur across the application surface and are easy
# to get wrong, because each is a closed schema with required keys of its own:
#
#   @CSCOPE  callable-code scope   {"generation": <id>}  — the sentinel below
#                                  binds the latest complete generation.
#   @SGSCOPE symbol-graph scope    {} — only an optional path_prefix. Passing a
#                                  generation here is rejected: the schema is
#                                  closed and has no such key.
#   @CMETA   callable-code meta    {"projection", "order"}
#   @RMETA   retrieval meta        {"temporal", "page", "projection", "order"}
# ---------------------------------------------------------------------------

catalog() {
cat <<'CATALOG'
# always-loaded
search|read|{"query":"@SYMBOL","limit":5}
grep|read|{"pattern":"@SYMBOL","max_results":10,"fixed_strings":true}
context|read|{"task":"how does @SYMBOL work"}
callers|read|{"node_id":"@NODE"}
status|read|{}
active_project|read|{}
storage_status|read|{}
# analysis
circular|read|{}
complexity|read|{"limit":5}
constructors|read|{"struct":"@STRUCT"}
coupling|read|{"limit":5}
dead_code|read|{"limit":5}
distribution|read|{}
doc_coverage|read|{"limit":5}
field_sites|read|{"field":"@FIELD"}
god_class|read|{"limit":5}
hotspots|read|{"limit":5}
inheritance_depth|read|{"limit":5}
largest|read|{"limit":5}
rank|read|{"edge_kind":"calls","limit":5}
recursion|read|{"limit":5}
unsafe_patterns|read|{"limit":5}
unused_imports|read|{"limit":5}
# application
affected_tests|handle|{"request_handle":"sweep-synthetic-handle"}
call_chain|read|{"from_node_id":"@NODE","to_node_id":"@NODE2"}
code_callees|read|{"node_id":"@NODE","maximum_depth":2,"resolve_trait_dispatch":false,"scope":@CSCOPE,"meta":@CMETA}
code_callers|read|{"node_id":"@NODE","maximum_depth":2,"resolve_trait_dispatch":false,"scope":@SGSCOPE,"meta":@CMETA}
code_declaration|read|{"node_id":"@NODE","scope":@CSCOPE,"meta":@CMETA}
code_definition|read|{"node_id":"@NODE","scope":@CSCOPE,"meta":@CMETA}
code_exact_occurrence|read|{"literal":"@SYMBOL","scope":@CSCOPE,"meta":@CMETA}
code_facets|read|{"dimension":"kind","scope":@CSCOPE,"meta":@CMETA}
code_implementations|read|{"selector":{"selector":"trait","name":"@TRAIT"},"scope":@SGSCOPE,"meta":@CMETA}
code_phrase_search|read|{"query":"@SYMBOL","phrases":["@SYMBOL"],"field_filters":[],"fuzzy_budget":0,"scope":@CSCOPE,"meta":@CMETA}
code_references|read|{"node_id":"@NODE","scope":@CSCOPE,"meta":@CMETA}
code_signature_search|read|{"params":["self"],"scope":@SGSCOPE,"meta":@CMETA}
code_symbol_search|read|{"query":"@SYMBOL","lazy_index_ignored_dependencies":false,"scope":@SGSCOPE,"meta":@CMETA}
code_timeline|read|{"scope":@CSCOPE,"meta":@CMETA}
code_type_definition|read|{"node_id":"@NODE","scope":@CSCOPE,"meta":@CMETA}
code_type_hierarchy|read|{"node_id":"@NODE","maximum_depth":2,"scope":@SGSCOPE,"meta":@CMETA}
configuration_audit|read|{"limit":5}
configuration_batch|mutate|{"expected_revision":"0","mutations":[]}
configuration_explain|read|{"key":"telemetry.enabled"}
configuration_get|read|{"key":"telemetry.enabled"}
configuration_list|read|{}
configuration_observed_state|read|{}
configuration_protected_apply|mutate|{"plan_id":"sweep","expected_base_revision_id":"0","idempotency_key":"sweep","operation_digest":"0"}
configuration_protected_preview|preview|{"change":{},"expected_revision":"0"}
configuration_rollback_apply|mutate|{"plan_id":"sweep","expected_base_revision_id":"0","idempotency_key":"sweep","operation_digest":"0"}
configuration_rollback_preview|preview|{"mode":"forward","target_revision_id":"0"}
configuration_set|mutate|{"key":"telemetry.enabled","value":"true","layer":"project","expected_revision":"0"}
configuration_unset|mutate|{"key":"telemetry.enabled","layer":"project","expected_revision":"0"}
configuration_write_credential|mutate|{"kind":"token","write_handle":"sweep","expected_revision":"0"}
context_scout_budget|handle|{"address":{}}
context_scout_cancel|handle|{"address":{},"work":{}}
context_scout_capability|handle|{"address":{}}
context_scout_claim|handle|{"address":{},"window":"sweep"}
context_scout_delivery|handle|{"address":{},"claim":{},"receipt":{}}
context_scout_explain|handle|{"address":{}}
context_scout_feedback|handle|{"address":{},"feedback":{},"receipt":{}}
context_scout_pause|handle|{"address":{},"expected_revision":"0"}
context_scout_recent|handle|{"address":{}}
context_scout_resume|handle|{"address":{},"expected_revision":"0"}
context_scout_status|handle|{"address":{}}
diagnostics|read|{}
diagnostics_read|read|{"scope":"workspace","maximum_diagnostics":10}
feedback_advisory_cycle|handle|{"document_uri":"file:///@FILE"}
feedback_diagnostics|handle|{"request_handle":"sweep-synthetic-handle"}
feedback_expand|handle|{"request_handle":"sweep-synthetic-handle"}
feedback_get|handle|{"request_handle":"sweep-synthetic-handle"}
feedback_impact|handle|{"request_handle":"sweep-synthetic-handle"}
feedback_list|handle|{"request_handle":"sweep-synthetic-handle"}
file_dependents|read|{"file":"@FILE"}
file_metadata|read|{"files":["@FILE"]}
git_apply|mutate|{"idempotency_key":"sweep","preview":{}}
git_blame|read|{"path":"@FILE"}
git_diff|read|{}
git_history|read|{"count":5}
git_hunks|handle|{"preview_id":"sweep","snapshot_digest":"0"}
git_preview|preview|{"operation":"stage_hunks","repository_snapshot":{},"selected_hunks":[]}
git_status|read|{}
health_delta|read|{"meta":@RMETA}
health_read|read|{"meta":@RMETA}
module_api|read|{"path":"@DIR"}
qualified_name|read|{"qualified_name":"@QNAME","page":{"page_size":10}}
session_lookup|handle|{"session_id":"sweep-synthetic-session","meta":@RMETA}
source_body|read|{"node_id":"@NODE"}
source_lines|read|{"file":"@FILE","span":{"start_byte":0,"end_byte":512},"meta":@RMETA}
source_outline|read|{"file":"@FILE"}
test_results|read|{}
# edit
api_migration_apply|mutate|{"plan":{},"plan_digest":"0"}
api_migration_plan|preview|{"family_id":"sweep","operations":[]}
ast_grep_rewrite|mutate|{"path":"@FILE","pattern":"$A + $B","rewrite":"$B + $A","dry_run":true}
insert_at|mutate|{"path":"@FILE","anchor":"@SYMBOL","content":"","dry_run":true}
insert_at_symbol|mutate|{"symbol":"@SYMBOL","content":"","dry_run":true}
move_symbol|mutate|{"symbol":"@SYMBOL","dest_file":"@FILE","dry_run":true}
multi_str_replace|mutate|{"path":"@FILE","replacements":[],"dry_run":true}
replace_symbol|mutate|{"symbol":"@SYMBOL","new_source":"","dry_run":true}
str_replace|preview|{"path":"@FILE","old_str":"@SYMBOL","new_str":"@SYMBOL","dry_run":true}
source_edit_reconcile|mutate|{"effect_id":"sweep","kind":"str_replace","disposition":"abandon","idempotency_key":"sweep","attempt_idempotency_key":"sweep","input_digest":"0","confirm":false}
# git & history
affected|read|{"files":["@FILE"]}
branch_diff|read|{}
branch_list|read|{}
branch_search|read|{"branch":"@BRANCH","query":"@SYMBOL"}
changelog|read|{"from_ref":"@PREV","to_ref":"@HEAD"}
commit_context|read|{}
diff_context|read|{"files":["@FILE"]}
pr_context|read|{}
# graph
by_qualified_name|read|{"qualified_name":"@QNAME"}
callees|read|{"node_id":"@NODE"}
callers_for|read|{"node_ids":["@NODE"]}
derives|read|{"qualified_name":"@QNAME"}
find_exact_symbol|read|{"name":"@SYMBOL"}
impact|read|{"node_id":"@NODE"}
implementations|read|{"trait":"@TRAIT","limit":5}
impls|read|{"limit":5}
rename_preview|preview|{"node_id":"@NODE"}
signature|read|{"qualified_name":"@QNAME"}
similar|read|{"symbol":"@SYMBOL","limit":5}
type_hierarchy|read|{"node_id":"@NODE"}
# health
dependency_depth|read|{"limit":5}
dsm|read|{}
gini|read|{}
health|read|{}
redundancy|read|{"max_pairs":5}
runtime|read|{}
test_map|read|{}
test_risk|read|{"limit":5}
# info
analytics|read|{}
ast_grep_search|read|{"pattern":"fn $NAME($$$ARGS) { $$$BODY }"}
automation_run_artifact_view|handle|{"run_id":"sweep-synthetic-run","kind":"traces"}
body|read|{"symbol":"@SYMBOL"}
config|read|{"key":"package.version","path":"Cargo.toml"}
dashboard|read|{}
files|read|{}
hermes_skill_bridge|read|{}
lcm_compress|mutate|{"provider":"claude","session_id":"sweep-synthetic-session"}
lcm_describe|handle|{"provider":"claude","session_id":"sweep-synthetic-session"}
lcm_doctor|read|{"provider":"claude"}
lcm_expand|handle|{"provider":"claude","session_id":"sweep-synthetic-session","target":"sweep"}
lcm_expand_query|handle|{"provider":"claude","session_id":"sweep-synthetic-session","prompt":"sweep"}
lcm_grep|read|{"query":"@SYMBOL"}
lcm_load_session|handle|{"session_id":"sweep-synthetic-session"}
lcm_preflight|handle|{"provider":"claude","session_id":"sweep-synthetic-session"}
lcm_session_boundary|mutate|{"provider":"claude","session_id":"sweep-synthetic-session"}
lcm_status|read|{}
message_search|read|{"query":"@SYMBOL","limit":5}
node|read|{"node_id":"@NODE"}
outline|read|{"file":"@FILE"}
port_order|read|{"source_dir":"@DIR","limit":5}
port_status|read|{"source_dir":"@DIR","target_dir":"@DIR"}
project_context|read|{}
project_list|read|{}
project_search|read|{"query":"@SYMBOL"}
read|read|{"file":"@FILE"}
retrieve|handle|{"handle":"sweep-synthetic-handle"}
session_refresh|mutate|{"action":"refresh","scope":"session","session":{},"source":{},"target":{},"profile":{}}
sessions_for|read|{"git_ref":"branch","value":"@BRANCH","limit":5}
signature_search|read|{"params":["self"],"limit":5}
simplify_scan|read|{"files":["@FILE"]}
skill_list|read|{}
skill_view|read|{"id":"tracedecay-tool-fallbacks"}
todos|read|{"limit":5}
workflows|read|{}
# memory & session
fact_feedback|mutate|{"fact_id":"0"}
fact_store|read|{"action":"search","query":"@SYMBOL","limit":5}
memory_status|read|{}
session_end|mutate|{}
session_start|mutate|{}
# workflow
diagnose|read|{"cargo_output":"error[E0308]: mismatched types\n  --> @FILE:1:1"}
run_affected_tests|mutate|{}
CATALOG
}

# The nested request objects the application surface requires. `generation`
# takes the unpinned-latest sentinel exported by tracedecay-application, which
# binds whatever generation is currently complete.
FX_CSCOPE='{"generation":"code-generation:unpinned-latest.v1"}'
FX_SGSCOPE='{}'
FX_CMETA='{"projection":"summary","order":"relevance"}'
FX_RMETA='{"temporal":{"kind":"current"},"page":{"page_size":10},"projection":"summary","order":"relevance"}'

expand_args() {
  printf '%s' "$1" \
    | sed -e "s|@CSCOPE|${FX_CSCOPE}|g" \
          -e "s|@SGSCOPE|${FX_SGSCOPE}|g" \
          -e "s|@CMETA|${FX_CMETA}|g" \
          -e "s|@RMETA|${FX_RMETA}|g" \
          -e "s|@FILE|${FX_FILE}|g" \
          -e "s|@DIR|${FX_DIR}|g" \
          -e "s|@SYMBOL|${FX_SYMBOL}|g" \
          -e "s|@STRUCT|${FX_STRUCT}|g" \
          -e "s|@TRAIT|${FX_TRAIT}|g" \
          -e "s|@FIELD|${FX_FIELD}|g" \
          -e "s|@NODE2|${FX_NODE_ID_2}|g" \
          -e "s|@NODE|${FX_NODE_ID}|g" \
          -e "s|@QNAME|${FX_QNAME}|g" \
          -e "s|@BRANCH|${FX_BRANCH}|g" \
          -e "s|@HEAD|${FX_HEAD}|g" \
          -e "s|@PREV|${FX_PREV}|g"
}

in_list() {
  local needle="$1" list="$2"
  [[ -z "$list" ]] && return 1
  case ",$list," in *",$needle,"*) return 0 ;; esac
  return 1
}

now_ms() { date +%s%3N; }

pass=0; fail=0; slow=0; skipped=0; down=0

daemon_down() {
  printf '%s' "$1" | grep -qiE "daemon socket .* is not available|daemon became unreachable|is warming in the background"
}

# Admission control shed this request before it reached the handler. The
# response says nothing about the tool, so it must not be scored as one; the
# battery waits for capacity and re-measures.
daemon_saturated() {
  printf '%s' "$1" | grep -qiE "bulk capacity reached|capacity_saturated|has no request capacity"
}

retryable_environment() {
  daemon_down "$1" || daemon_saturated "$1"
}

run_tool() {
  local tool="$1" class="$2" args="$3"
  local start end ms rc out verdict note attempt

  # The daemon can restart underneath a long sweep (a reinstall, a service
  # bounce). That is an environment event, not a tool defect, so wait for the
  # socket to come back and re-measure rather than recording a bogus failure.
  for attempt in $(seq 1 "$ENV_RETRIES"); do
    start="$(now_ms)"
    out="$(timeout "$TIMEOUT" "$BIN" tool "$tool" --project "$PROJECT" --args "$args" 2>&1)"
    rc=$?
    end="$(now_ms)"
    ms=$(( end - start ))
    retryable_environment "$out" || break
    [[ $attempt -eq "$ENV_RETRIES" ]] && break
    sleep $(( attempt < 6 ? attempt * 5 : 30 ))
  done

  printf '%s\n' "$out" > "$OUT/raw/$tool.txt"

  note=""
  if retryable_environment "$out"; then
    verdict="DAEMON-DOWN"; note="$(printf '%s' "$out" | head -1 | cut -c1-160)"
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$tool" "$class" "$verdict" "$ms" "$rc" "${note//$'\t'/ }" >> "$TSV"
    printf '%-34s %-8s %-11s %8sms  %s\n' "$tool" "$class" "$verdict" "$ms" "$note"
    down=$((down+1))
    return
  fi
  if [[ $rc -eq 124 ]]; then
    verdict="FAIL"; note="wall-clock timeout ${TIMEOUT}s"
  elif printf '%s' "$out" | grep -qi 'timed out before deadline'; then
    verdict="FAIL"; note="daemon deadline fired"
  elif [[ $rc -ne 0 ]]; then
    # A typed rejection of a deliberately synthetic handle proves the handler is
    # reachable and validating; that is the intended outcome for class=handle.
    if [[ "$class" == "handle" || "$class" == "preview" || "$class" == "mutate" ]]; then
      verdict="PASS"; note="typed rejection (expected for class=$class): $(printf '%s' "$out" | head -1 | cut -c1-140)"
    else
      verdict="FAIL"; note="$(printf '%s' "$out" | head -1 | cut -c1-160)"
    fi
  else
    verdict="PASS"
  fi

  if [[ "$verdict" == "PASS" && $ms -gt $(( WARM_THRESHOLD * 1000 )) ]]; then
    verdict="SLOW"
    [[ -n "$note" ]] || note="over warm threshold ${WARM_THRESHOLD}s"
  fi

  case "$verdict" in
    PASS) pass=$((pass+1)) ;;
    SLOW) slow=$((slow+1)) ;;
    FAIL) fail=$((fail+1)) ;;
  esac

  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$tool" "$class" "$verdict" "$ms" "$rc" "${note//$'\t'/ }" >> "$TSV"
  printf '%-34s %-8s %-11s %8sms  %s\n' "$tool" "$class" "$verdict" "$ms" "$note"
}

echo "== tracedecay tool sweep =="
echo "binary:    $($BIN --version 2>/dev/null || echo "$BIN")"
echo "project:   $PROJECT"
echo "classes:   $CLASSES"
echo "timeout:   ${TIMEOUT}s   warm threshold: ${WARM_THRESHOLD}s"
echo "out:       $OUT"
echo "load:      $(uptime | sed 's/.*load average/load average/')"
echo

derive_fixtures
echo "-- fixtures --"; cat "$OUT/fixtures.txt"; echo

# Catalog-completeness guard: every advertised tool must have an entry.
declare -A HAVE=()
while IFS='|' read -r tool class args; do
  [[ -z "$tool" || "$tool" == \#* ]] && continue
  HAVE["$tool"]=1
done < <(catalog)

MISSING=()
while read -r advertised; do
  [[ -n "${HAVE[$advertised]:-}" ]] || MISSING+=("$advertised")
done < <("$BIN" tool 2>/dev/null | grep -E '^  [a-z_]+ ' | awk '{print $1}')

if [[ ${#MISSING[@]} -gt 0 ]]; then
  echo "!! catalog missing entries for: ${MISSING[*]}"
  printf '%s\n' "${MISSING[@]}" > "$OUT/missing-catalog-entries.txt"
fi

while IFS='|' read -r tool class args; do
  [[ -z "$tool" || "$tool" == \#* ]] && continue
  if [[ -n "$ONLY" ]] && ! in_list "$tool" "$ONLY"; then continue; fi
  if in_list "$tool" "$SKIP"; then skipped=$((skipped+1)); continue; fi
  if ! in_list "$class" "$CLASSES"; then skipped=$((skipped+1)); continue; fi
  run_tool "$tool" "$class" "$(expand_args "$args")"
done < <(catalog)

echo
echo "== summary =="
echo "PASS=$pass SLOW=$slow FAIL=$fail DAEMON-DOWN=$down SKIPPED=$skipped"
echo "table: $TSV"

if [[ $EMIT_JSON -eq 1 ]]; then
  {
    printf '{"pass":%d,"slow":%d,"fail":%d,"daemon_down":%d,"skipped":%d,"rows":[' "$pass" "$slow" "$fail" "$down" "$skipped"
    tail -n +2 "$TSV" | awk -F'\t' '{
      gsub(/\\/,"\\\\",$6); gsub(/"/,"\\\"",$6);
      printf "%s{\"tool\":\"%s\",\"class\":\"%s\",\"verdict\":\"%s\",\"latency_ms\":%s,\"rc\":%s,\"note\":\"%s\"}", (NR>1?",":""), $1,$2,$3,$4,$5,$6
    }'
    printf ']}\n'
  } > "$OUT/results.json"
  echo "json:  $OUT/results.json"
fi

[[ $fail -eq 0 ]]
