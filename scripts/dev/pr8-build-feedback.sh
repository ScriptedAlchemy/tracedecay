#!/usr/bin/env bash
# PR8 developer-feedback evidence:
# compare explicit revisions in disposable clones on the current host.
#
# This script intentionally invokes ordinary `cargo` without changing target
# directories, TraceDecay data directories, analyzer processes, or repository
# lane policy. Caller-owned repositories are read only; all touches and builds
# happen in clones owned and removed by this process.
set -euo pipefail

RUNS=3
MODE="measure"
TIME_BIN=""
TIME_STYLE=""
TEMP_DIR=""
SOURCE_ROOT=""
SOURCE_GIT_DIR=""
BASELINE_ROOT=""
CANDIDATE_ROOT=""
BASELINE_COMMIT=""
CANDIDATE_COMMIT=""
BASELINE_TREE=""
CANDIDATE_TREE=""
BASELINE_RELEVANT_TREE=""
CANDIDATE_RELEVANT_TREE=""
BASELINE_CONFIG_TREE=""
CANDIDATE_CONFIG_TREE=""
BASELINE_IS_ANCESTOR="false"
MERGE_BASE=""
MERGE_BASE_MATCHES_BASELINE="false"
DISTINCT_COMMITS="false"
DISTINCT_TREES="false"
DISTINCT_RELEVANT_TREES="false"
MATCHING_CONFIG="false"
SAME_TOOLCHAIN="not-checked"
MATCHING_WORKLOAD="not-checked"
BASELINE_SESSION_RUNNER=""
CANDIDATE_SESSION_RUNNER=""
SESSION_RUNNER=""
SESSION_COMMAND=()
INELIGIBILITY_REASONS=()

RELEVANT_PATHS=(
    Cargo.toml
    Cargo.lock
    build.rs
    src
    crates
    tests
)

CONFIG_PATHS=(
    .cargo
    Cargo.toml
    Cargo.lock
    build.rs
    rust-toolchain
    rust-toolchain.toml
)

usage() {
    cat <<'USAGE'
Usage:
  scripts/dev/pr8-build-feedback.sh [--dry-run | --validate] \
    <source-repository> <baseline-revision> <candidate-revision>

Resolve two explicit commits from one caller-owned repository, then perform all
validation and measurement work in disposable `git clone --shared` clones.
The source repository is never checked out, touched, cleaned, or status-refreshed.

Recorded workloads per revision:
  - one excluded warmup and three warm no-op
    `cargo check --all-features` runs;
  - three mtime-only touches of `src/global_db.rs`, each followed by
    `cargo check --all-features`;
  - three mtime-only touches of that file, each followed by a `session_suite`
    compile/no-run command;
  - wall time, maximum RSS where supported, CPU time/utilization, and rebuilt
    unit evidence.

Clean attestation requires distinct commits and relevant source trees, the
baseline to be the candidate's merge base and ancestor, matching committed
Cargo/toolchain configuration, matching toolchains and workload selection,
clean owned clones, and completed non-provisional validation or measurement.

Modes:
  --dry-run   Resolve and validate Git revisions and render commands/metadata.
              Executes no cargo, rustc, rustup, nextest, or time command and
              is always provisional and measurement-ineligible.
  --validate  Create disposable clones and validate the complete measurement
              environment without running build workloads.
  --self-test-ineligible-metadata
              Run embedded fake tests for ineligible comparison metadata only.
  --self-test-timing
              Run embedded fake tests for GNU/BSD/POSIX timing parsing only.
  -h, --help  Show this help.

Redirect stdout to retain emitted key/value evidence:
  scripts/dev/pr8-build-feedback.sh . HEAD~1 HEAD > pr8-build-feedback.txt
USAGE
}

die() {
    printf 'error: %s\n' "$*" >&2
    exit 2
}

emit() {
    printf '%s=%q\n' "$1" "$2"
}

render_command() {
    local argument=""
    local rendered=""
    for argument in "$@"; do
        printf -v argument '%q' "$argument"
        rendered+="${rendered:+ }${argument}"
    done
    printf '%s' "$rendered"
}

require_command() {
    command -v "$1" >/dev/null 2>&1 \
        || die "required command not found: $1"
}

find_external_time() {
    local candidate=""
    local resolved=""
    local seen=":"

    for candidate in gtime time; do
        resolved="$(type -P "$candidate" 2>/dev/null || true)"
        [[ -n "$resolved" && -x "$resolved" ]] || continue
        if [[ "$seen" == *":${resolved}:"* ]]; then
            continue
        fi
        seen+="${resolved}:"
        printf '%s' "$resolved"
        return 0
    done
    return 1
}

cleanup() {
    local status="$1"
    trap - EXIT INT TERM HUP
    if [[ -n "$TEMP_DIR" && -d "$TEMP_DIR" ]]; then
        rm -rf -- "$TEMP_DIR"
    fi
    exit "$status"
}

signal_exit() {
    exit "$1"
}

create_temp_root() {
    TEMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/tracedecay-pr8-build-feedback.XXXXXX")"
    trap 'cleanup "$?"' EXIT
    trap 'signal_exit 130' INT
    trap 'signal_exit 143' TERM
    trap 'signal_exit 129' HUP
}

resolve_source() {
    local input="$1"
    local root=""

    [[ -d "$input" ]] || die "source repository does not exist: $input"
    root="$(git -C "$input" rev-parse --path-format=absolute --show-toplevel 2>/dev/null)" \
        || die "not a Git working repository: $input"
    (
        cd "$root"
        pwd -P
    )
}

resolve_common_git_dir() {
    local common_dir=""
    common_dir="$(
        git -C "$SOURCE_ROOT" rev-parse \
            --path-format=absolute --git-common-dir 2>/dev/null
    )" \
        || die "cannot resolve source repository common Git directory"
    (
        cd "$common_dir"
        pwd -P
    )
}

resolve_commit() {
    local revision="$1"
    [[ "$revision" != -* ]] || die "revision must not begin with '-': $revision"
    git -C "$SOURCE_ROOT" rev-parse --verify "${revision}^{commit}" 2>/dev/null \
        || die "revision does not resolve to a commit: $revision"
}

require_revision_path() {
    local commit="$1"
    local path="$2"
    git -C "$SOURCE_ROOT" cat-file -e "${commit}:${path}" 2>/dev/null \
        || die "revision $commit does not contain $path"
}

tree_fingerprint() {
    local commit="$1"
    shift
    git -C "$SOURCE_ROOT" ls-tree -r --full-tree "$commit" -- "$@" \
        | git hash-object --stdin
}

revisions_differ_at_paths() {
    local status=0
    set +e
    git -C "$SOURCE_ROOT" diff --quiet \
        "$BASELINE_COMMIT" "$CANDIDATE_COMMIT" -- "$@"
    status=$?
    set -e
    case "$status" in
        0)
            return 1
            ;;
        1)
            return 0
            ;;
        *)
            die "failed to compare revisions at configured paths"
            ;;
    esac
}

resolve_revision_relationship() {
    local ancestor_status=0

    BASELINE_TREE="$(git -C "$SOURCE_ROOT" rev-parse "${BASELINE_COMMIT}^{tree}")"
    CANDIDATE_TREE="$(git -C "$SOURCE_ROOT" rev-parse "${CANDIDATE_COMMIT}^{tree}")"
    BASELINE_RELEVANT_TREE="$(
        tree_fingerprint "$BASELINE_COMMIT" "${RELEVANT_PATHS[@]}"
    )"
    CANDIDATE_RELEVANT_TREE="$(
        tree_fingerprint "$CANDIDATE_COMMIT" "${RELEVANT_PATHS[@]}"
    )"
    BASELINE_CONFIG_TREE="$(
        tree_fingerprint "$BASELINE_COMMIT" "${CONFIG_PATHS[@]}"
    )"
    CANDIDATE_CONFIG_TREE="$(
        tree_fingerprint "$CANDIDATE_COMMIT" "${CONFIG_PATHS[@]}"
    )"

    [[ "$BASELINE_COMMIT" != "$CANDIDATE_COMMIT" ]] \
        && DISTINCT_COMMITS="true"
    [[ "$BASELINE_TREE" != "$CANDIDATE_TREE" ]] \
        && DISTINCT_TREES="true"
    if revisions_differ_at_paths "${RELEVANT_PATHS[@]}"; then
        DISTINCT_RELEVANT_TREES="true"
    fi
    if ! revisions_differ_at_paths "${CONFIG_PATHS[@]}"; then
        MATCHING_CONFIG="true"
    fi

    set +e
    git -C "$SOURCE_ROOT" merge-base --is-ancestor \
        "$BASELINE_COMMIT" "$CANDIDATE_COMMIT"
    ancestor_status=$?
    set -e
    case "$ancestor_status" in
        0)
            BASELINE_IS_ANCESTOR="true"
            ;;
        1)
            BASELINE_IS_ANCESTOR="false"
            ;;
        *)
            die "failed to evaluate candidate ancestry"
            ;;
    esac

    MERGE_BASE="$(
        git -C "$SOURCE_ROOT" merge-base \
            "$BASELINE_COMMIT" "$CANDIDATE_COMMIT" 2>/dev/null \
            || true
    )"
    if [[ -n "$MERGE_BASE" && "$MERGE_BASE" == "$BASELINE_COMMIT" ]]; then
        MERGE_BASE_MATCHES_BASELINE="true"
    fi
}

create_owned_clone() {
    local destination="$1"
    local commit="$2"

    git clone --quiet --shared --no-checkout -- "$SOURCE_GIT_DIR" "$destination"
    git -C "$destination" checkout --quiet --detach "$commit"
}

status_entry_count() {
    git -C "$1" status --porcelain=v1 --untracked-files=normal \
        | awk 'END { print NR + 0 }'
}

cargo_version_for() {
    (
        cd "$1"
        cargo --version
    )
}

rustc_version_for() {
    (
        cd "$1"
        rustc -Vv
    ) | awk '{ printf "%s%s", separator, $0; separator = ";" } END { print "" }'
}

rustup_toolchain_for() {
    if command -v rustup >/dev/null 2>&1; then
        (
            cd "$1"
            rustup show active-toolchain 2>/dev/null || true
        ) | awk '{ printf "%s%s", separator, $0; separator = ";" } END { print "" }'
    else
        printf 'rustup-unavailable'
    fi
}

session_runner_for() {
    local root="$1"
    local help_output=""

    if help_output="$(
        cd "$root"
        cargo nextest run --help 2>&1
    )" && [[ "$help_output" == *"--no-run"* ]]; then
        printf 'nextest'
    else
        printf 'cargo-test'
    fi
}

set_session_command() {
    local runner="$1"
    if [[ "$runner" == "nextest" ]]; then
        SESSION_COMMAND=(
            cargo nextest run --all-features --no-run
            -E 'binary(session_suite)'
        )
    else
        SESSION_COMMAND=(
            cargo test --all-features --test session_suite --no-run
        )
    fi
}

detect_time_command() {
    local candidate=""
    local probe_file=""
    local version_output=""
    local version_is_gnu="false"
    local candidates=()

    candidate="$(type -P gtime 2>/dev/null || true)"
    [[ -n "$candidate" && -x "$candidate" ]] && candidates+=("$candidate")
    candidate="$(type -P time 2>/dev/null || true)"
    if [[ -n "$candidate" && -x "$candidate" ]]; then
        if [[ "${#candidates[@]}" -eq 0 || "${candidates[0]}" != "$candidate" ]]; then
            candidates+=("$candidate")
        fi
    fi
    [[ "${#candidates[@]}" -gt 0 ]] \
        || die "required external time command not found"

    for candidate in "${candidates[@]}"; do
        version_output="$("$candidate" --version 2>&1 || true)"
        case "$version_output" in
            *GNU*"Time"*|*GNU*"time"*|*gnu*"Time"*|*gnu*"time"*)
                version_is_gnu="true"
                ;;
            *)
                version_is_gnu="false"
                ;;
        esac

        probe_file="$TEMP_DIR/time-probe.gnu"
        rm -f -- "$probe_file"
        if [[ "$version_is_gnu" == "true" ]] \
            && "$candidate" \
                -f 'wall_seconds=%e' -o "$probe_file" true \
                >/dev/null 2>&1 \
            && [[ -s "$probe_file" ]]; then
            TIME_BIN="$candidate"
            TIME_STYLE="gnu"
            return 0
        fi

        probe_file="$TEMP_DIR/time-probe.bsd"
        rm -f -- "$probe_file"
        if "$candidate" -l true >/dev/null 2>"$probe_file"; then
            TIME_BIN="$candidate"
            TIME_STYLE="bsd"
            return 0
        fi

        probe_file="$TEMP_DIR/time-probe.posix"
        rm -f -- "$probe_file"
        if "$candidate" -p true >/dev/null 2>"$probe_file"; then
            TIME_BIN="$candidate"
            TIME_STYLE="posix"
            return 0
        fi
    done

    die "no external time command supports GNU -f/-o, BSD -l, or POSIX -p"
}

inherited_environment_value() {
    case "$1" in
        CARGO_TARGET_DIR)
            if [[ -n ${CARGO_TARGET_DIR+x} ]]; then
                printf 'inherited:%s' "$CARGO_TARGET_DIR"
            else
                printf 'unset'
            fi
            ;;
        TRACEDECAY_DATA_DIR)
            if [[ -n ${TRACEDECAY_DATA_DIR+x} ]]; then
                printf 'inherited:%s' "$TRACEDECAY_DATA_DIR"
            else
                printf 'unset'
            fi
            ;;
        *)
            die "unsupported inherited environment key: $1"
            ;;
    esac
}

time_value() {
    local key="$1"
    local file="$2"
    awk -F= -v wanted="$key" '
        $1 == wanted {
            sub(/^[^=]*=/, "")
            print
            exit
        }
    ' "$file"
}

rebuilt_unit_count() {
    awk '
        /^[[:space:]]*(Checking|Compiling|Building)[[:space:]]/ { count += 1 }
        END { print count + 0 }
    ' "$1"
}

parse_non_gnu_time_field() {
    local log_file="$1"
    local label="$2"
    # Prefer BSD value-before-label (`0.03 real ...`) and otherwise accept
    # POSIX label-before-value (`real 0.03`). Normalize decimal, 1m2.5s, and
    # 1:02.5 forms so Git Bash and Unix time implementations agree.
    awk -v wanted="$label" '
        function seconds(s, parts) {
            if (s ~ /^[0-9]+(\.[0-9]+)?$/) {
                return s
            }
            if (s ~ /^[0-9]+m[0-9]+(\.[0-9]+)?s$/) {
                split(s, parts, "m")
                sub(/s$/, "", parts[2])
                return sprintf("%.6f", parts[1] * 60 + parts[2])
            }
            if (s ~ /^[0-9]+:[0-9]+(\.[0-9]+)?$/) {
                split(s, parts, ":")
                return sprintf("%.6f", parts[1] * 60 + parts[2])
            }
            return ""
        }
        {
            for (i = 1; i <= NF; i++) {
                if ($i != wanted) {
                    continue
                }
                if (i > 1 && seconds($(i - 1)) != "") {
                    print seconds($(i - 1))
                    exit
                }
                if (i < NF && seconds($(i + 1)) != "") {
                    print seconds($(i + 1))
                    exit
                }
            }
        }
    ' "$log_file"
}

parse_bsd_max_rss_raw() {
    local log_file="$1"
    awk '
        /maximum resident set size/ {
            if ($1 ~ /^[0-9]+$/) {
                print $1
            }
            exit
        }
    ' "$log_file"
}

emit_non_gnu_time_values() {
    local log_file="$1"
    local system_name=""
    local wall_seconds=""
    local user_seconds=""
    local system_seconds=""
    local max_rss_raw=""
    local max_rss_unit="unavailable"
    local max_rss_kib="unavailable"

    system_name="$(uname -s)"
    wall_seconds="$(parse_non_gnu_time_field "$log_file" real)"
    user_seconds="$(parse_non_gnu_time_field "$log_file" user)"
    system_seconds="$(parse_non_gnu_time_field "$log_file" sys)"

    if [[ "$TIME_STYLE" == "bsd" ]]; then
        max_rss_raw="$(parse_bsd_max_rss_raw "$log_file")"
        if [[ "$system_name" == "Darwin" && -n "$max_rss_raw" ]]; then
            max_rss_unit="bytes"
            max_rss_kib="$(
                awk -v bytes="$max_rss_raw" \
                    'BEGIN { printf "%.0f", bytes / 1024 }'
            )"
        elif [[ -n "$max_rss_raw" ]]; then
            max_rss_unit="platform-reported"
        fi
    fi

    emit "measurement.wall_seconds" "${wall_seconds:-unavailable}"
    emit "measurement.max_rss_raw" "${max_rss_raw:-unavailable}"
    emit "measurement.max_rss_raw_unit" "$max_rss_unit"
    emit "measurement.max_rss_kib" "$max_rss_kib"
    emit "measurement.cpu_user_seconds" "${user_seconds:-unavailable}"
    emit "measurement.cpu_system_seconds" "${system_seconds:-unavailable}"
    emit "measurement.cpu_percent" "unavailable"
}

run_timed() {
    local label="$1"
    local phase="$2"
    local run="$3"
    local root="$4"
    shift 4

    local cache_outcome=""
    local command_text=""
    local exit_status=0
    local log_file=""
    local rebuilt_units=""
    local time_file=""

    command_text="$(render_command "$@")"
    log_file="$TEMP_DIR/${label}.${phase}.${run}.log"
    time_file="$TEMP_DIR/${label}.${phase}.${run}.time"

    set +e
    if [[ "$TIME_STYLE" == "gnu" ]]; then
        (
            cd "$root"
            LC_ALL=C "$TIME_BIN" \
                -f 'wall_seconds=%e\nmax_rss_kib=%M\ncpu_user_seconds=%U\ncpu_system_seconds=%S\ncpu_percent=%P' \
                -o "$time_file" \
                "$@"
        ) >"$log_file" 2>&1
        exit_status=$?
    elif [[ "$TIME_STYLE" == "bsd" ]]; then
        (
            cd "$root"
            LC_ALL=C "$TIME_BIN" -l bash -c \
                'log_file="$1"; shift; exec "$@" >"$log_file" 2>&1' \
                bash "$log_file" "$@"
        ) 2>"$time_file"
        exit_status=$?
    else
        (
            cd "$root"
            LC_ALL=C "$TIME_BIN" -p bash -c \
                'log_file="$1"; shift; exec "$@" >"$log_file" 2>&1' \
                bash "$log_file" "$@"
        ) 2>"$time_file"
        exit_status=$?
    fi
    set -e

    if [[ "$exit_status" -ne 0 ]]; then
        printf 'measurement failed: label=%s phase=%s run=%s command=%s\n' \
            "$label" "$phase" "$run" "$command_text" >&2
        awk '{ print "measurement-log: " $0 }' "$log_file" >&2
        awk '{ print "timing-log: " $0 }' "$time_file" >&2
        return "$exit_status"
    fi

    rebuilt_units="$(rebuilt_unit_count "$log_file")"
    if [[ "$rebuilt_units" -eq 0 ]]; then
        cache_outcome="no-rebuild-lines-observed"
    else
        cache_outcome="rebuild-lines-observed"
    fi

    emit "measurement.label" "$label"
    emit "measurement.phase" "$phase"
    emit "measurement.run" "$run"
    emit "measurement.command" "$command_text"
    emit "measurement.rebuilt_units" "$rebuilt_units"
    emit "measurement.cache_evidence" "$cache_outcome"
    emit "measurement.build_script_evidence" \
        "not-exposed-by-default-cargo-output"

    if [[ "$TIME_STYLE" == "gnu" ]]; then
        emit "measurement.wall_seconds" \
            "$(time_value wall_seconds "$time_file")"
        emit "measurement.max_rss_kib" \
            "$(time_value max_rss_kib "$time_file")"
        emit "measurement.cpu_user_seconds" \
            "$(time_value cpu_user_seconds "$time_file")"
        emit "measurement.cpu_system_seconds" \
            "$(time_value cpu_system_seconds "$time_file")"
        emit "measurement.cpu_percent" \
            "$(time_value cpu_percent "$time_file")"
    else
        emit_non_gnu_time_values "$time_file"
    fi
}

set_workload_command_for_runner() {
    set_session_command "$SESSION_RUNNER"
}

emit_workload_plan() {
    local label="$1"
    local run=0

    emit "workload.${label}.warmup" "cargo check --all-features"
    for ((run = 1; run <= RUNS; run += 1)); do
        emit "workload.${label}.warm_noop.${run}" \
            "cargo check --all-features"
        emit "workload.${label}.root_touch_incremental.${run}" \
            "touch src/global_db.rs && cargo check --all-features"
        emit "workload.${label}.session_suite_compile.${run}" \
            "touch src/global_db.rs && $(render_command "${SESSION_COMMAND[@]}")"
    done
}

measure_checkout() {
    local label="$1"
    local root="$2"
    local run=0

    run_timed "$label" "warmup_excluded" 0 "$root" \
        cargo check --all-features
    for ((run = 1; run <= RUNS; run += 1)); do
        run_timed "$label" "warm_noop" "$run" "$root" \
            cargo check --all-features
    done
    for ((run = 1; run <= RUNS; run += 1)); do
        touch "$root/src/global_db.rs"
        run_timed "$label" "root_touch_incremental" "$run" "$root" \
            cargo check --all-features
    done
    for ((run = 1; run <= RUNS; run += 1)); do
        touch "$root/src/global_db.rs"
        run_timed "$label" "session_suite_compile" "$run" "$root" \
            "${SESSION_COMMAND[@]}"
    done
}

emit_revision_metadata() {
    emit "source.root" "$SOURCE_ROOT"
    emit "source.common_git_dir" "$SOURCE_GIT_DIR"
    emit "source.working_tree" \
        "not-inspected-to-avoid-caller-index-or-mtime-mutation"
    emit "checkout.baseline.requested_revision" "$BASELINE_REVISION"
    emit "checkout.baseline.commit" "$BASELINE_COMMIT"
    emit "checkout.baseline.tree" "$BASELINE_TREE"
    emit "checkout.baseline.relevant_tree" "$BASELINE_RELEVANT_TREE"
    emit "checkout.baseline.config_tree" "$BASELINE_CONFIG_TREE"
    emit "checkout.candidate.requested_revision" "$CANDIDATE_REVISION"
    emit "checkout.candidate.commit" "$CANDIDATE_COMMIT"
    emit "checkout.candidate.tree" "$CANDIDATE_TREE"
    emit "checkout.candidate.relevant_tree" "$CANDIDATE_RELEVANT_TREE"
    emit "checkout.candidate.config_tree" "$CANDIDATE_CONFIG_TREE"
    emit "comparison.distinct_commits" "$DISTINCT_COMMITS"
    emit "comparison.distinct_trees" "$DISTINCT_TREES"
    emit "comparison.distinct_relevant_trees" "$DISTINCT_RELEVANT_TREES"
    emit "comparison.baseline_is_ancestor" "$BASELINE_IS_ANCESTOR"
    emit "comparison.merge_base" "${MERGE_BASE:-none}"
    emit "comparison.merge_base_matches_baseline" \
        "$MERGE_BASE_MATCHES_BASELINE"
    emit "comparison.matching_config" "$MATCHING_CONFIG"
}

emit_common_metadata() {
    emit "pr8_build_feedback.version" "2"
    emit "comparison.mode" "$MODE"
    emit "comparison.host" "$(hostname 2>/dev/null || uname -n)"
    emit "comparison.uname" "$(uname -srm)"
    emit "comparison.runs" "$RUNS"
    emit "cargo.command" "cargo"
    if [[ "$MODE" == "dry-run" ]]; then
        emit "cargo.resolved" "not-resolved-dry-run"
        emit "rustc.resolved" "not-resolved-dry-run"
    else
        emit "cargo.resolved" "$(command -v cargo)"
        emit "rustc.resolved" "$(command -v rustc)"
    fi
    emit "environment.CARGO_TARGET_DIR" \
        "$(inherited_environment_value CARGO_TARGET_DIR)"
    emit "environment.TRACEDECAY_DATA_DIR" \
        "$(inherited_environment_value TRACEDECAY_DATA_DIR)"
    emit "measurement.policy" \
        "ordinary cargo; owned shared clones; no target/data override, analyzer action, lane coordination, or caller-root touch"
}

emit_clone_metadata() {
    local baseline_status="$1"
    local candidate_status="$2"

    emit "checkout.baseline.root" "$BASELINE_ROOT"
    emit "checkout.baseline.ownership" "process-owned-disposable-clone"
    emit "checkout.baseline.status_entries" "$baseline_status"
    emit "checkout.candidate.root" "$CANDIDATE_ROOT"
    emit "checkout.candidate.ownership" "process-owned-disposable-clone"
    emit "checkout.candidate.status_entries" "$candidate_status"
}

record_reason() {
    INELIGIBILITY_REASONS+=("$1")
}

emit_reasons() {
    local index=0
    emit "comparison.ineligibility_reason_count" \
        "${#INELIGIBILITY_REASONS[@]}"
    for ((index = 0; index < "${#INELIGIBILITY_REASONS[@]}"; index += 1)); do
        emit "comparison.ineligibility_reason.${index}" \
            "${INELIGIBILITY_REASONS[index]}"
    done
}

emit_ineligible_comparison_metadata() {
    emit "comparison.measurement_eligible" "false"
    emit "comparison.provisional_state" "true"
    emit "comparison.attestation" "not-attested"
    emit "comparison.attestation_scope" "none"
}

self_test_has() {
    local file="$1"
    local needle="$2"
    awk -v s="$needle" 'index($0, s) { found = 1 } END { exit found ? 0 : 1 }' "$file"
}

self_test_pass() {
    printf 'ok - %s\n' "$1"
}

self_test_fail() {
    printf 'not ok - %s\n' "$1" >&2
}

run_self_test_ineligible_metadata() {
    local script_path root repo bin
    local commit_a commit_b commit_c commit_d commit_u
    local out log last failures=0
    local fake_case=""

    script_path="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
    root="$(mktemp -d "${TMPDIR:-/tmp}/tracedecay-pr8-ineligible-metadata.XXXXXX")"
    repo="$root/source repo with spaces"
    bin="$root/fake bin"
    mkdir -p "$repo/src" "$repo/tests/session_suite" "$bin" "$repo/.cargo"

    printf '%s\n' '[package]' 'name = "fake"' 'version = "0.1.0"' > "$repo/Cargo.toml"
    printf '# lock\n' > "$repo/Cargo.lock"
    printf 'pub fn baseline() {}\n' > "$repo/src/global_db.rs"
    printf 'fn main() {}\n' > "$repo/tests/session_suite/main.rs"

    git init -q "$repo"
    git -C "$repo" add Cargo.toml Cargo.lock src/global_db.rs tests/session_suite/main.rs
    env GIT_AUTHOR_NAME=Fake GIT_AUTHOR_EMAIL=fake@example.invalid \
        GIT_COMMITTER_NAME=Fake GIT_COMMITTER_EMAIL=fake@example.invalid \
        GIT_AUTHOR_DATE=2026-01-01T00:00:00Z GIT_COMMITTER_DATE=2026-01-01T00:00:00Z \
        git -C "$repo" -c commit.gpgsign=false commit -q -m baseline
    commit_a="$(git -C "$repo" rev-parse HEAD)"

    printf 'pub fn candidate() {}\n' > "$repo/src/global_db.rs"
    git -C "$repo" add src/global_db.rs
    env GIT_AUTHOR_NAME=Fake GIT_AUTHOR_EMAIL=fake@example.invalid \
        GIT_COMMITTER_NAME=Fake GIT_COMMITTER_EMAIL=fake@example.invalid \
        GIT_AUTHOR_DATE=2026-01-02T00:00:00Z GIT_COMMITTER_DATE=2026-01-02T00:00:00Z \
        git -C "$repo" -c commit.gpgsign=false commit -q -m candidate
    commit_b="$(git -C "$repo" rev-parse HEAD)"

    env GIT_AUTHOR_NAME=Fake GIT_AUTHOR_EMAIL=fake@example.invalid \
        GIT_COMMITTER_NAME=Fake GIT_COMMITTER_EMAIL=fake@example.invalid \
        GIT_AUTHOR_DATE=2026-01-03T00:00:00Z GIT_COMMITTER_DATE=2026-01-03T00:00:00Z \
        git -C "$repo" -c commit.gpgsign=false commit -q --allow-empty -m same-tree
    commit_c="$(git -C "$repo" rev-parse HEAD)"

    printf '[build]\nincremental = true\n' > "$repo/.cargo/config.toml"
    printf 'pub fn config_candidate() {}\n' > "$repo/src/global_db.rs"
    git -C "$repo" add .cargo/config.toml src/global_db.rs
    env GIT_AUTHOR_NAME=Fake GIT_AUTHOR_EMAIL=fake@example.invalid \
        GIT_COMMITTER_NAME=Fake GIT_COMMITTER_EMAIL=fake@example.invalid \
        GIT_AUTHOR_DATE=2026-01-04T00:00:00Z GIT_COMMITTER_DATE=2026-01-04T00:00:00Z \
        git -C "$repo" -c commit.gpgsign=false commit -q -m config-change
    commit_d="$(git -C "$repo" rev-parse HEAD)"

    commit_u="$(
        printf 'unrelated\n' \
            | env GIT_AUTHOR_NAME=Fake GIT_AUTHOR_EMAIL=fake@example.invalid \
                GIT_COMMITTER_NAME=Fake GIT_COMMITTER_EMAIL=fake@example.invalid \
                GIT_AUTHOR_DATE=2026-01-05T00:00:00Z GIT_COMMITTER_DATE=2026-01-05T00:00:00Z \
                git -C "$repo" commit-tree "$(git -C "$repo" rev-parse "${commit_b}^{tree}")"
    )"

    printf '%s\n' \
        '#!/usr/bin/env bash' \
        'printf "cargo|%s|%s\n" "$PWD" "$*" >> "$FAKE_TOOL_LOG"' \
        'case "$*" in' \
        '  "--version")' \
        '    if [[ "${FAKE_CASE:-}" == "toolchain-mismatch" && "$PWD" == *candidate ]]; then' \
        '      printf "%s\n" "cargo 2.0.0 fake"' \
        '    else' \
        '      printf "%s\n" "cargo 1.0.0 fake"' \
        '    fi ;;' \
        '  "nextest run --help")' \
        '    if [[ "${FAKE_CASE:-}" == "workload-mismatch" && "$PWD" == *candidate ]]; then' \
        '      exit 1' \
        '    fi' \
        '    printf "%s\n" "--no-run" ;;' \
        '  *) printf "unexpected cargo: %s\n" "$*" >&2; exit 97 ;;' \
        'esac' > "$bin/cargo"
    printf '%s\n' \
        '#!/usr/bin/env bash' \
        'printf "rustc|%s|%s\n" "$PWD" "$*" >> "$FAKE_TOOL_LOG"' \
        'printf "%s\n" "rustc 1.0.0 fake" "binary: rustc" "commit-hash: fake" "host: fake-host"' \
        > "$bin/rustc"
    printf '%s\n' \
        '#!/usr/bin/env bash' \
        'printf "rustup|%s|%s\n" "$PWD" "$*" >> "$FAKE_TOOL_LOG"' \
        'printf "%s\n" "fake-toolchain (overridden by fake)"' \
        > "$bin/rustup"
    printf '%s\n' \
        '#!/usr/bin/env bash' \
        'printf "time|%s|%s\n" "$PWD" "$*" >> "$FAKE_TOOL_LOG"' \
        'if [[ "${1:-}" == "--version" ]]; then' \
        '  printf "%s\n" "time (GNU Time) fake"' \
        '  exit 0' \
        'fi' \
        'if [[ "${1:-}" == "-f" ]]; then' \
        '  shift 2' \
        '  [[ "${1:-}" == "-o" ]] || exit 92' \
        '  out="$2"' \
        '  shift 2' \
        '  "$@"' \
        '  status=$?' \
        '  printf "%s\n" "wall_seconds=0.01" > "$out"' \
        '  exit "$status"' \
        'fi' \
        'exit 95' \
        > "$bin/time"
    chmod +x "$bin/cargo" "$bin/rustc" "$bin/rustup" "$bin/time"

    assert_ineligible() {
        local name="$1"
        local reason_needle="$2"
        shift 2
        out="$root/${name}.out"
        log="$root/${name}.log"
        : > "$log"
        mkdir -p "$root/tmp-$name"
        set +e
        PATH="$bin:$PATH" TMPDIR="$root/tmp-$name" \
            FAKE_TOOL_LOG="$log" FAKE_CASE="$fake_case" \
            bash "$script_path" "$@" > "$out" 2>&1
        last=$?
        set -e
        if [[ "$last" -eq 2 ]] \
            && self_test_has "$out" 'comparison.measurement_eligible=false' \
            && self_test_has "$out" 'comparison.provisional_state=true' \
            && self_test_has "$out" 'comparison.attestation=not-attested' \
            && self_test_has "$out" "$reason_needle"; then
            self_test_pass "$name"
        else
            self_test_fail "$name"
            awk '/comparison\.(measurement_eligible|provisional_state|attestation)=/ { print }' "$out" >&2 || true
            failures=$((failures + 1))
        fi
    }

    fake_case="normal"
    assert_ineligible "identical-commits" \
        "baseline-and-candidate-commits-must-differ" \
        --validate "$repo" "$commit_a" "$commit_a"
    assert_ineligible "identical-trees" \
        "baseline-and-candidate-full-trees-must-differ" \
        --validate "$repo" "$commit_b" "$commit_c"
    assert_ineligible "unrelated-ancestry" \
        "baseline-must-be-an-ancestor-of-candidate" \
        --validate "$repo" "$commit_a" "$commit_u"
    assert_ineligible "config-mismatch" \
        "committed-cargo-and-toolchain-config-must-match" \
        --validate "$repo" "$commit_a" "$commit_d"
    fake_case="toolchain-mismatch"
    assert_ineligible "toolchain-mismatch" \
        "baseline-and-candidate-toolchains-must-match" \
        --validate "$repo" "$commit_a" "$commit_b"
    fake_case="workload-mismatch"
    assert_ineligible "workload-mismatch" \
        "baseline-and-candidate-session-runners-must-match" \
        --validate "$repo" "$commit_a" "$commit_b"

    # Direct helper contract: dirty/other validation failures share one emitter.
    out="$root/helper.out"
    (
        emit_ineligible_comparison_metadata
    ) > "$out"
    if self_test_has "$out" 'comparison.measurement_eligible=false' \
        && self_test_has "$out" 'comparison.provisional_state=true' \
        && self_test_has "$out" 'comparison.attestation=not-attested' \
        && ! self_test_has "$out" 'comparison.provisional_state=false'; then
        self_test_pass "helper-ineligible-metadata"
    else
        self_test_fail "helper-ineligible-metadata"
        failures=$((failures + 1))
    fi

    # Clean eligible validation attests only environment eligibility.
    fake_case="normal"
    out="$root/clean.out"
    log="$root/clean.log"
    : > "$log"
    mkdir -p "$root/tmp-clean"
    set +e
    PATH="$bin:$PATH" TMPDIR="$root/tmp-clean" \
        FAKE_TOOL_LOG="$log" FAKE_CASE="$fake_case" \
        bash "$script_path" --validate "$repo" "$commit_a" "$commit_b" > "$out" 2>&1
    last=$?
    set -e
    if [[ "$last" -eq 0 ]] \
        && self_test_has "$out" 'comparison.measurement_eligible=true' \
        && self_test_has "$out" 'comparison.provisional_state=true' \
        && self_test_has "$out" 'comparison.attestation=clean-environment-validated' \
        && self_test_has "$out" 'comparison.attestation_scope=environment-only'; then
        self_test_pass "clean-validation-remains-provisional"
    else
        self_test_fail "clean-eligible-remains-final"
        failures=$((failures + 1))
    fi

    rm -rf -- "$root"
    [[ "$failures" -eq 0 ]]
}

run_self_test_timing() {
    local root out log_file time_file failures=0
    local saved_style="${TIME_STYLE:-}"

    root="$(mktemp -d "${TMPDIR:-/tmp}/tracedecay-pr8-timing.XXXXXX")"
    out="$root/emit.out"
    log_file="$root/sample.log"
    time_file="$root/sample.time"

    assert_has() {
        local name="$1"
        local needle="$2"
        if self_test_has "$out" "$needle"; then
            self_test_pass "$name"
        else
            self_test_fail "$name"
            awk '{ print }' "$out" >&2 || true
            failures=$((failures + 1))
        fi
    }

    assert_lacks() {
        local name="$1"
        local needle="$2"
        if ! self_test_has "$out" "$needle"; then
            self_test_pass "$name"
        else
            self_test_fail "$name"
            awk '{ print }' "$out" >&2 || true
            failures=$((failures + 1))
        fi
    }

    # GNU key=value file path (preserved; not non-GNU log parsing).
    printf '%s\n' \
        'wall_seconds=0.01' \
        'max_rss_kib=1234' \
        'cpu_user_seconds=0.02' \
        'cpu_system_seconds=0.03' \
        'cpu_percent=99%' > "$time_file"
    : > "$out"
    {
        emit "measurement.wall_seconds" "$(time_value wall_seconds "$time_file")"
        emit "measurement.max_rss_kib" "$(time_value max_rss_kib "$time_file")"
        emit "measurement.cpu_user_seconds" "$(time_value cpu_user_seconds "$time_file")"
        emit "measurement.cpu_system_seconds" "$(time_value cpu_system_seconds "$time_file")"
        emit "measurement.cpu_percent" "$(time_value cpu_percent "$time_file")"
    } > "$out"
    assert_has "gnu-wall" 'measurement.wall_seconds=0.01'
    assert_has "gnu-rss" 'measurement.max_rss_kib=1234'
    assert_has "gnu-user" 'measurement.cpu_user_seconds=0.02'
    assert_has "gnu-sys" 'measurement.cpu_system_seconds=0.03'
    assert_has "gnu-cpu-percent" 'measurement.cpu_percent=99%'

    # BSD value-before-label one-liner plus RSS.
    TIME_STYLE="bsd"
    printf '%s\n' \
        '0.03 real 0.01 user 0.00 sys' \
        '4096 maximum resident set size' > "$log_file"
    : > "$out"
    emit_non_gnu_time_values "$log_file" > "$out"
    assert_has "bsd-wall" 'measurement.wall_seconds=0.03'
    assert_has "bsd-user" 'measurement.cpu_user_seconds=0.01'
    assert_has "bsd-sys" 'measurement.cpu_system_seconds=0.00'
    assert_has "bsd-rss-raw" 'measurement.max_rss_raw=4096'
    assert_lacks "bsd-rss-not-unavailable" 'measurement.max_rss_raw=unavailable'

    # POSIX label-before-value lines; RSS truly absent.
    TIME_STYLE="posix"
    printf '%s\n' \
        'real 0.03' \
        'user 0.01' \
        'sys 0.00' > "$log_file"
    : > "$out"
    emit_non_gnu_time_values "$log_file" > "$out"
    assert_has "posix-wall" 'measurement.wall_seconds=0.03'
    assert_has "posix-user" 'measurement.cpu_user_seconds=0.01'
    assert_has "posix-sys" 'measurement.cpu_system_seconds=0.00'
    assert_has "posix-rss-unavailable" 'measurement.max_rss_raw=unavailable'
    assert_has "posix-rss-unit-unavailable" 'measurement.max_rss_raw_unit=unavailable'

    # Shell-style portable durations are normalized to decimal seconds.
    TIME_STYLE="posix"
    printf '%s\n' \
        'real 1m2.50s' \
        'user 0m1.25s' \
        'sys 0:00.75' > "$log_file"
    : > "$out"
    emit_non_gnu_time_values "$log_file" > "$out"
    assert_has "portable-wall" 'measurement.wall_seconds=62.500000'
    assert_has "portable-user" 'measurement.cpu_user_seconds=1.250000'
    assert_has "portable-sys" 'measurement.cpu_system_seconds=0.750000'

    # Malformed: labels must not be emitted as numeric values.
    TIME_STYLE="posix"
    printf '%s\n' \
        'real user' \
        'user sys' \
        'sys real' > "$log_file"
    : > "$out"
    emit_non_gnu_time_values "$log_file" > "$out"
    assert_has "malformed-wall-unavailable" 'measurement.wall_seconds=unavailable'
    assert_has "malformed-user-unavailable" 'measurement.cpu_user_seconds=unavailable'
    assert_has "malformed-sys-unavailable" 'measurement.cpu_system_seconds=unavailable'
    assert_lacks "malformed-wall-not-label" 'measurement.wall_seconds=user'
    assert_lacks "malformed-wall-not-sys" 'measurement.wall_seconds=sys'
    assert_lacks "malformed-user-not-label" 'measurement.cpu_user_seconds=sys'
    assert_lacks "malformed-user-not-real" 'measurement.cpu_user_seconds=real'
    assert_lacks "malformed-sys-not-label" 'measurement.cpu_system_seconds=real'
    assert_lacks "malformed-sys-not-user" 'measurement.cpu_system_seconds=user'

    # Malformed BSD RSS first field must not be accepted as a size.
    TIME_STYLE="bsd"
    printf '%s\n' \
        '0.03 real 0.01 user 0.00 sys' \
        'bogus maximum resident set size' > "$log_file"
    : > "$out"
    emit_non_gnu_time_values "$log_file" > "$out"
    assert_has "malformed-rss-unavailable" 'measurement.max_rss_raw=unavailable'
    assert_lacks "malformed-rss-not-bogus" 'measurement.max_rss_raw=bogus'

    # BSD separate-line value-before-label form.
    TIME_STYLE="bsd"
    printf '%s\n' \
        '0.12 real' \
        '0.08 user' \
        '0.04 sys' \
        '8192 maximum resident set size' > "$log_file"
    : > "$out"
    emit_non_gnu_time_values "$log_file" > "$out"
    assert_has "bsd-lines-wall" 'measurement.wall_seconds=0.12'
    assert_has "bsd-lines-user" 'measurement.cpu_user_seconds=0.08'
    assert_has "bsd-lines-sys" 'measurement.cpu_system_seconds=0.04'
    assert_has "bsd-lines-rss" 'measurement.max_rss_raw=8192'

    TIME_STYLE="$saved_style"
    rm -rf -- "$root"
    [[ "$failures" -eq 0 ]]
}

evaluate_static_eligibility() {
    [[ "$DISTINCT_COMMITS" == "true" ]] \
        || record_reason "baseline-and-candidate-commits-must-differ"
    [[ "$DISTINCT_TREES" == "true" ]] \
        || record_reason "baseline-and-candidate-full-trees-must-differ"
    [[ "$DISTINCT_RELEVANT_TREES" == "true" ]] \
        || record_reason "baseline-and-candidate-relevant-source-trees-must-differ"
    [[ "$BASELINE_IS_ANCESTOR" == "true" ]] \
        || record_reason "baseline-must-be-an-ancestor-of-candidate"
    [[ "$MERGE_BASE_MATCHES_BASELINE" == "true" ]] \
        || record_reason "candidate-merge-base-must-equal-baseline"
    [[ "$MATCHING_CONFIG" == "true" ]] \
        || record_reason "committed-cargo-and-toolchain-config-must-match"
}

while [[ "$#" -gt 0 ]]; do
    case "$1" in
        -h|--help)
            usage
            exit 0
            ;;
        --self-test-ineligible-metadata)
            run_self_test_ineligible_metadata
            exit $?
            ;;
        --self-test-timing)
            run_self_test_timing
            exit $?
            ;;
        --dry-run)
            [[ "$MODE" == "measure" ]] \
                || die "--dry-run cannot be combined with --validate"
            MODE="dry-run"
            shift
            ;;
        --validate)
            [[ "$MODE" == "measure" ]] \
                || die "--validate cannot be combined with --dry-run"
            MODE="validate"
            shift
            ;;
        --)
            shift
            break
            ;;
        -*)
            die "unknown option: $1"
            ;;
        *)
            break
            ;;
    esac
done

[[ "$#" -eq 3 ]] || {
    usage >&2
    die "expected <source-repository> <baseline-revision> <candidate-revision>"
}

SOURCE_INPUT="$1"
BASELINE_REVISION="$2"
CANDIDATE_REVISION="$3"

require_command git

SOURCE_ROOT="$(resolve_source "$SOURCE_INPUT")"
SOURCE_GIT_DIR="$(resolve_common_git_dir)"
BASELINE_COMMIT="$(resolve_commit "$BASELINE_REVISION")"
CANDIDATE_COMMIT="$(resolve_commit "$CANDIDATE_REVISION")"
require_revision_path "$BASELINE_COMMIT" src/global_db.rs
require_revision_path "$BASELINE_COMMIT" tests/session_suite/main.rs
require_revision_path "$CANDIDATE_COMMIT" src/global_db.rs
require_revision_path "$CANDIDATE_COMMIT" tests/session_suite/main.rs
resolve_revision_relationship
evaluate_static_eligibility

emit_common_metadata
emit_revision_metadata

if [[ "$MODE" == "dry-run" ]]; then
    SESSION_RUNNER="cargo-test"
    set_workload_command_for_runner
    emit "time.command" "not-resolved-dry-run"
    emit "time.style" "not-checked-dry-run"
    emit "toolchain.baseline" "not-executed-dry-run"
    emit "toolchain.candidate" "not-executed-dry-run"
    emit "comparison.same_toolchain" "not-checked-dry-run"
    emit "comparison.matching_workload" "not-checked-dry-run"
    emit "workload.session_runner" "planned-cargo-test"
    emit "dry_run.clone.baseline" \
        "$(render_command git clone --shared --no-checkout -- "$SOURCE_GIT_DIR" "<temporary-root>/baseline")"
    emit "dry_run.clone.candidate" \
        "$(render_command git clone --shared --no-checkout -- "$SOURCE_GIT_DIR" "<temporary-root>/candidate")"
    emit_workload_plan "baseline"
    emit_workload_plan "candidate"
    record_reason "dry-run-is-provisional"
    emit "comparison.measurement_eligible" "false"
    emit "comparison.provisional_state" "true"
    emit "comparison.attestation" "provisional-dry-run"
    emit "comparison.attestation_scope" "plan-only"
    emit_reasons
    exit 0
fi

require_command awk
require_command mktemp
require_command rm
require_command cargo
require_command rustc
require_command touch
require_command bash
TIME_BIN="$(find_external_time)" \
    || die "required external time command not found"

create_temp_root
BASELINE_ROOT="$TEMP_DIR/baseline"
CANDIDATE_ROOT="$TEMP_DIR/candidate"
create_owned_clone "$BASELINE_ROOT" "$BASELINE_COMMIT"
create_owned_clone "$CANDIDATE_ROOT" "$CANDIDATE_COMMIT"

BASELINE_STATUS_ENTRIES="$(status_entry_count "$BASELINE_ROOT")"
CANDIDATE_STATUS_ENTRIES="$(status_entry_count "$CANDIDATE_ROOT")"
[[ "$BASELINE_STATUS_ENTRIES" -eq 0 ]] \
    || record_reason "baseline-owned-clone-must-be-clean"
[[ "$CANDIDATE_STATUS_ENTRIES" -eq 0 ]] \
    || record_reason "candidate-owned-clone-must-be-clean"

detect_time_command

BASELINE_CARGO_VERSION="$(cargo_version_for "$BASELINE_ROOT")"
CANDIDATE_CARGO_VERSION="$(cargo_version_for "$CANDIDATE_ROOT")"
BASELINE_RUSTC_VERSION="$(rustc_version_for "$BASELINE_ROOT")"
CANDIDATE_RUSTC_VERSION="$(rustc_version_for "$CANDIDATE_ROOT")"
BASELINE_RUSTUP_TOOLCHAIN="$(rustup_toolchain_for "$BASELINE_ROOT")"
CANDIDATE_RUSTUP_TOOLCHAIN="$(rustup_toolchain_for "$CANDIDATE_ROOT")"

SAME_TOOLCHAIN="true"
if [[ "$BASELINE_CARGO_VERSION" != "$CANDIDATE_CARGO_VERSION" \
    || "$BASELINE_RUSTC_VERSION" != "$CANDIDATE_RUSTC_VERSION" \
    || "$BASELINE_RUSTUP_TOOLCHAIN" != "$CANDIDATE_RUSTUP_TOOLCHAIN" ]]; then
    SAME_TOOLCHAIN="false"
    record_reason "baseline-and-candidate-toolchains-must-match"
fi

BASELINE_SESSION_RUNNER="$(session_runner_for "$BASELINE_ROOT")"
CANDIDATE_SESSION_RUNNER="$(session_runner_for "$CANDIDATE_ROOT")"
MATCHING_WORKLOAD="true"
if [[ "$BASELINE_SESSION_RUNNER" != "$CANDIDATE_SESSION_RUNNER" ]]; then
    MATCHING_WORKLOAD="false"
    record_reason "baseline-and-candidate-session-runners-must-match"
fi
SESSION_RUNNER="$BASELINE_SESSION_RUNNER"
set_workload_command_for_runner

emit_clone_metadata "$BASELINE_STATUS_ENTRIES" "$CANDIDATE_STATUS_ENTRIES"
emit "time.command" "$TIME_BIN"
emit "time.style" "$TIME_STYLE"
emit "toolchain.baseline.cargo_version" "$BASELINE_CARGO_VERSION"
emit "toolchain.candidate.cargo_version" "$CANDIDATE_CARGO_VERSION"
emit "toolchain.baseline.rustc_vv" "$BASELINE_RUSTC_VERSION"
emit "toolchain.candidate.rustc_vv" "$CANDIDATE_RUSTC_VERSION"
emit "toolchain.baseline.rustup_active" "$BASELINE_RUSTUP_TOOLCHAIN"
emit "toolchain.candidate.rustup_active" "$CANDIDATE_RUSTUP_TOOLCHAIN"
emit "comparison.same_toolchain" "$SAME_TOOLCHAIN"
emit "comparison.matching_workload" "$MATCHING_WORKLOAD"
emit "workload.session_runner" "$SESSION_RUNNER"
emit_workload_plan "baseline"
emit_workload_plan "candidate"

if [[ "${#INELIGIBILITY_REASONS[@]}" -gt 0 ]]; then
    emit_ineligible_comparison_metadata
    emit_reasons
    die "comparison is not measurement-eligible"
fi

if [[ "$MODE" == "validate" ]]; then
    emit "comparison.measurement_eligible" "true"
    emit "comparison.provisional_state" "true"
    emit "comparison.attestation" "clean-environment-validated"
    emit "comparison.attestation_scope" "environment-only"
    emit_reasons
    emit "validation.result" "passed"
    exit 0
fi

measure_checkout "baseline" "$BASELINE_ROOT"
measure_checkout "candidate" "$CANDIDATE_ROOT"

POSTFLIGHT_BASELINE_STATUS="$(status_entry_count "$BASELINE_ROOT")"
POSTFLIGHT_CANDIDATE_STATUS="$(status_entry_count "$CANDIDATE_ROOT")"
emit "postflight.baseline.status_entries" "$POSTFLIGHT_BASELINE_STATUS"
emit "postflight.candidate.status_entries" "$POSTFLIGHT_CANDIDATE_STATUS"

if [[ "$POSTFLIGHT_BASELINE_STATUS" -ne 0 \
    || "$POSTFLIGHT_CANDIDATE_STATUS" -ne 0 ]]; then
    record_reason "measurement-must-not-change-owned-clone-contents"
    emit_ineligible_comparison_metadata
    emit_reasons
    emit "measurement.result" "failed-dirty-owned-clone"
    die "measurement changed owned clone contents"
fi

emit "comparison.measurement_eligible" "true"
emit "comparison.provisional_state" "false"
emit "comparison.attestation" "clean-measurement-attested"
emit "comparison.attestation_scope" "measurement-completed"
emit_reasons
emit "measurement.result" "completed"
