#!/usr/bin/env bash
# Reproducible OS/process profiling around a Hotpath workload.
#
# Samples Linux counters as phase-to-phase DELTAS (not lifetime totals):
#   /proc/<pid>/io, stat, status, smaps_rollup
#   cgroup v2 memory.current / memory.events / memory.pressure / io.stat
#   open FDs classified by store/transcript/artifact family (no full paths)
#   thread state + uninterruptible (D) blocked-I/O
# Optional sidecars when installed: pidstat, iostat, perf stat.
# Opt-in (privileges / large artifacts): --perf-record, --samply, --ebpf.
#
# Memory gauges (heap/anonymous RSS, swap) are recorded before catch-up,
# after catch-up, and after idle (default 600s).
#
# One workload sample (no idle, wrap a command; the child is the sample PID):
#   scripts/profile-hotpath-os-counters.sh \
#       --scenario one-workload-sample \
#       --features hotpath,hotpath-mcp \
#       --idle-seconds 0 \
#       --out /tmp/hotpath-os-sample \
#       -- python3 -c 'import hashlib,os,time; t=time.monotonic()+0.4
#           n=0
#           while time.monotonic()<t:
#               n+=1; hashlib.sha256(os.urandom(4096)).digest()'
#
# Attach to a daemon while a catch-up command runs, then idle 10 minutes:
#   scripts/profile-hotpath-os-counters.sh \
#       --pid "$DAEMON_PID" \
#       --scenario session-catch-up \
#       --features hotpath,hotpath-mcp \
#       --profile-identity "$PROFILE_NAME" \
#       --hotpath-report /tmp/hotpath.json \
#       --idle-seconds 600 \
#       --out /tmp/hotpath-os-catchup \
#       -- tracedecay tool status --args '{}'
#
# Hermetic collector tests (no cargo):
#   scripts/profile-hotpath-os-counters.sh --self-test
#
# Environment:
#   HOTPATH_OS_PROFILE_IDLE_SECONDS  default idle when --idle-seconds omitted
#   HOTPATH_OS_PROFILE_REPO          checkout used for commit metadata
set -euo pipefail

usage() {
  sed -n '2,46p' "$0" >&2
  exit 2
}

SCRIPT_DIR=$(CDPATH= cd -P -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -P -- "${HOTPATH_OS_PROFILE_REPO:-$SCRIPT_DIR/..}" && pwd)
COLLECTOR="$SCRIPT_DIR/lib/hotpath_os_profile.py"

if ! command -v python3 >/dev/null 2>&1; then
  echo "profile-hotpath-os-counters: python3 is required" >&2
  exit 127
fi

scenario=""
out_dir=""
sample_pid=""
features="${CARGO_FEATURE_FLAGS:-}"
profile_identity=""
hotpath_report=""
bin=""
idle_seconds="${HOTPATH_OS_PROFILE_IDLE_SECONDS:-600}"
self_test=0
perf_record=0
use_samply=0
use_ebpf=0
workload=()
sidecar_pids=()

while (($# > 0)); do
  case "$1" in
    --help|-h) usage ;;
    --self-test) self_test=1; shift ;;
    --scenario) (($# >= 2)) || usage; scenario="$2"; shift 2 ;;
    --out) (($# >= 2)) || usage; out_dir="$2"; shift 2 ;;
    --pid) (($# >= 2)) || usage; sample_pid="$2"; shift 2 ;;
    --features) (($# >= 2)) || usage; features="$2"; shift 2 ;;
    --profile-identity) (($# >= 2)) || usage; profile_identity="$2"; shift 2 ;;
    --hotpath-report) (($# >= 2)) || usage; hotpath_report="$2"; shift 2 ;;
    --bin) (($# >= 2)) || usage; bin="$2"; shift 2 ;;
    --idle-seconds) (($# >= 2)) || usage; idle_seconds="$2"; shift 2 ;;
    --perf-record) perf_record=1; shift ;;
    --samply) use_samply=1; shift ;;
    --ebpf) use_ebpf=1; shift ;;
    --) shift; workload=("$@"); break ;;
    --catch-up-cmd)
      echo "profile-hotpath-os-counters: use -- COMMAND for the catch-up/workload" >&2
      exit 2
      ;;
    *)
      echo "profile-hotpath-os-counters: unknown argument: $1" >&2
      usage
      ;;
  esac
done

if ((self_test)); then
  exec python3 "$SCRIPT_DIR/test-profile-hotpath-os-counters.py"
fi

[[ -n "$scenario" && -n "$out_dir" ]] || usage
[[ "$idle_seconds" =~ ^[0-9]+$ ]] || {
  echo "profile-hotpath-os-counters: --idle-seconds must be a non-negative integer" >&2
  exit 2
}
if [[ -n "$sample_pid" && ! "$sample_pid" =~ ^[1-9][0-9]*$ ]]; then
  echo "profile-hotpath-os-counters: --pid must be a positive integer" >&2
  exit 2
fi

mkdir -p "$out_dir/phases" "$out_dir/sidecars"
out_dir=$(CDPATH= cd -P -- "$out_dir" && pwd)

stop_sidecars() {
  local pid
  for pid in "${sidecar_pids[@]:-}"; do
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
      kill -INT "$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
  sidecar_pids=()
}

cleanup() {
  stop_sidecars
  if [[ -n "${workload_pid:-}" ]] && kill -0 "$workload_pid" 2>/dev/null; then
    kill -TERM "$workload_pid" 2>/dev/null || true
    wait "$workload_pid" 2>/dev/null || true
  fi
}
trap cleanup EXIT

tool_path() {
  command -v "$1" 2>/dev/null || true
}

start_sidecar() {
  local name="$1"
  shift
  local log="$out_dir/sidecars/$name.log"
  "$@" >"$log" 2>"$out_dir/sidecars/$name.err" &
  sidecar_pids+=("$!")
  echo "$name pid=$! log=$log" >>"$out_dir/sidecars/index.txt"
}

snapshot() {
  local label="$1"
  python3 "$COLLECTOR" snapshot --pid "$sample_pid" --label "$label" \
    --out "$out_dir/phases/${label}.json"
}

commit=$(git -C "$REPO_ROOT" rev-parse HEAD)
commit_dirty=false
if [[ -n "$(git -C "$REPO_ROOT" status --porcelain)" ]]; then
  commit_dirty=true
fi

binary_version="unspecified"
if [[ -n "$bin" && -x "$bin" ]]; then
  binary_version=$("$bin" --version 2>/dev/null | head -n 1 || echo unspecified)
elif command -v tracedecay >/dev/null 2>&1; then
  binary_version=$(tracedecay --version 2>/dev/null | head -n 1 || echo unspecified)
fi

if [[ -z "$profile_identity" ]]; then
  profile_identity="${TRACEDECAY_PROFILE:-}"
fi

hotpath_report_record=""
if [[ -n "$hotpath_report" ]]; then
  if [[ "$hotpath_report" == "$REPO_ROOT/"* ]]; then
    hotpath_report_record=${hotpath_report#"$REPO_ROOT/"}
  else
    hotpath_report_record=$(basename -- "$hotpath_report")
  fi
fi

# Identity is assembled in-shell so we do not pass home paths into Python argv.
python3 -c '
import json, sys
from pathlib import Path
payload = {
  "commit": sys.argv[1],
  "commit_dirty": sys.argv[2] == "true",
  "feature_set": sys.argv[3] or "unspecified",
  "binary_version": sys.argv[4],
  "profile_identity": sys.argv[5] or "unspecified",
  "scenario": sys.argv[6],
  "idle_seconds": int(sys.argv[7]),
  "hotpath_report_path": sys.argv[8] or None,
  "sampled_pid": None,
}
Path(sys.argv[9]).write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
' "$commit" "$commit_dirty" "$features" "$binary_version" "$profile_identity" \
  "$scenario" "$idle_seconds" "$hotpath_report_record" "$out_dir/meta.json"

workload_pid=""
workload_exit=0
if ((${#workload[@]} > 0)); then
  "${workload[@]}" &
  workload_pid=$!
  if [[ -z "$sample_pid" ]]; then
    sample_pid=$workload_pid
  fi
elif [[ -z "$sample_pid" ]]; then
  echo "profile-hotpath-os-counters: provide --pid or a -- COMMAND to sample" >&2
  exit 2
fi

if ! [[ -d "/proc/$sample_pid" ]]; then
  echo "profile-hotpath-os-counters: pid $sample_pid is not running" >&2
  exit 2
fi

python3 -c '
import json, sys
from pathlib import Path
path = Path(sys.argv[1])
payload = json.loads(path.read_text(encoding="utf-8"))
payload["sampled_pid"] = int(sys.argv[2])
path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
' "$out_dir/meta.json" "$sample_pid"

python3 -c '
import json, shutil, sys
from pathlib import Path
names = ["pidstat", "iostat", "perf", "samply", "bpftrace"]
payload = {name: {"available": shutil.which(name) is not None, "used": False} for name in names}
Path(sys.argv[1]).write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
' "$out_dir/tools.json"

mark_tool_used() {
  python3 -c '
import json, sys
from pathlib import Path
path = Path(sys.argv[1])
payload = json.loads(path.read_text(encoding="utf-8"))
payload.setdefault(sys.argv[2], {})["used"] = True
payload[sys.argv[2]]["available"] = True
path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
' "$out_dir/tools.json" "$1"
}

started_ns=$(date +%s%N)
snapshot before

if [[ -n "$(tool_path pidstat)" ]]; then
  start_sidecar pidstat pidstat -h -u -d -r -p "$sample_pid" 1
  mark_tool_used pidstat
fi
if [[ -n "$(tool_path iostat)" ]]; then
  start_sidecar iostat iostat -xz 1
  mark_tool_used iostat
fi
if [[ -n "$(tool_path perf)" ]]; then
  start_sidecar perf-stat perf stat -p "$sample_pid" \
    -e task-clock,cycles,instructions,cache-misses,page-faults,context-switches \
    --interval-print 1000 sleep 86400
  mark_tool_used perf
  if ((perf_record)); then
    start_sidecar perf-record perf record -p "$sample_pid" -o "$out_dir/sidecars/perf.data" -- sleep 86400
  fi
fi
if ((use_samply)) && [[ -n "$(tool_path samply)" ]]; then
  start_sidecar samply samply record -o "$out_dir/sidecars/samply.json.gz" --pid "$sample_pid"
  mark_tool_used samply
fi
if ((use_ebpf)) && [[ -n "$(tool_path bpftrace)" ]]; then
  # Kernel stacks and a counter only — no userspace paths or filenames.
  start_sidecar bpftrace timeout 30 bpftrace -e \
    "profile:hz:49 /pid == ${sample_pid}/ { @oncpu = count(); } interval:s:20 { exit(); }"
  mark_tool_used bpftrace
fi

# /proc lifetime counters vanish when the sampled PID exits. In wrap mode
# (the workload *is* the sample) poll a live after_catch_up snapshot until
# wait succeeds. In attach mode the daemon stays up, so one snapshot after
# the catch-up command is enough.
if [[ -n "$workload_pid" && "$workload_pid" == "$sample_pid" ]]; then
  while kill -0 "$workload_pid" 2>/dev/null; do
    snapshot after_catch_up || true
    if ! kill -0 "$workload_pid" 2>/dev/null; then
      break
    fi
    sleep 0.15
  done
  workload_exit=0
  wait "$workload_pid" || workload_exit=$?
  workload_pid=""
elif [[ -n "$workload_pid" ]]; then
  workload_exit=0
  wait "$workload_pid" || workload_exit=$?
  workload_pid=""
  if [[ -d "/proc/$sample_pid" ]]; then
    snapshot after_catch_up
  else
    echo "profile-hotpath-os-counters: sampled pid $sample_pid exited during workload" >&2
    exit 2
  fi
else
  snapshot after_catch_up
fi

if [[ ! -f "$out_dir/phases/after_catch_up.json" ]]; then
  echo "profile-hotpath-os-counters: missing after_catch_up snapshot" >&2
  exit 2
fi

if ((idle_seconds > 0)) && [[ -d "/proc/$sample_pid" ]]; then
  sleep "$idle_seconds"
  snapshot after_idle
elif ((idle_seconds > 0)); then
  echo "profile-hotpath-os-counters: skipping idle; sampled pid exited (idle needs --pid)" >&2
fi

stop_sidecars
trap - EXIT

ended_ns=$(date +%s%N)
python3 -c '
import json, sys
from pathlib import Path
path = Path(sys.argv[1])
payload = json.loads(path.read_text(encoding="utf-8"))
started = int(sys.argv[2])
ended = int(sys.argv[3])
payload["started_ns"] = started
payload["ended_ns"] = ended
payload["duration_ms"] = (ended - started) // 1_000_000
path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
' "$out_dir/meta.json" "$started_ns" "$ended_ns"

phases=(--phase "before=$out_dir/phases/before.json")
if [[ -f "$out_dir/phases/after_catch_up.json" ]]; then
  phases+=(--phase "after_catch_up=$out_dir/phases/after_catch_up.json")
fi
if [[ -f "$out_dir/phases/after_idle.json" ]]; then
  phases+=(--phase "after_idle=$out_dir/phases/after_idle.json")
fi

python3 "$COLLECTOR" assemble \
  --meta "$out_dir/meta.json" \
  --tools "$out_dir/tools.json" \
  "${phases[@]}" \
  --workload-exit "$workload_exit" \
  --out "$out_dir/report.json"

python3 -c '
import json, sys
from pathlib import Path
report = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
identity = report["identity"]
deltas = report.get("deltas", {})
print("report", sys.argv[1])
print("commit", identity.get("commit"), "scenario", identity.get("scenario"))
outcome = report["test_outcome"]
print("outcome", outcome["status"], "workload_exit", outcome["workload_exit"])
for name, delta in deltas.items():
    stat = delta.get("stat", {})
    ticks = stat.get("value", {}).get("cpu_ticks") if stat.get("state") == "observed" else "unavailable"
    cpu = delta.get("cpu_percent_from_proc", {})
    cpu_v = cpu.get("value") if cpu.get("state") == "observed" else cpu.get("reason")
    io = delta.get("io", {})
    rchar = io.get("rchar", {})
    rchar_v = rchar.get("value") if rchar.get("state") == "delta" else rchar.get("reason")
    print("delta", name, "cpu_ticks=%s cpu_percent=%s rchar=%s" % (ticks, cpu_v, rchar_v))
' "$out_dir/report.json"

exit "$workload_exit"
