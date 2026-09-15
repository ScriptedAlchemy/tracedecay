#!/usr/bin/env bash
# Report (default) or delete stale linked Git worktrees.
#
# Linked worktrees are classified and only STALE-DETACHED / STALE-MERGED
# are GC candidates. DIRTY, ACTIVE, LOCKED, and FRESH trees are never
# removed, even with --force. Orphaned /fast/tmp scratch cargo dirs are
# reported only. Every printed worktree and every `git worktree remove`
# argument is an exact absolute path (never a short name or prefix).
#
# usage: scripts/worktree-gc.sh [--delete] [--force] [--integration REF]
#                               [--max-behind N] [--stale-age-hours N]
#                               [--repo PATH]
#                               [--scratch-dir DIR] [--scratch-age-hours N]
set -euo pipefail

usage() {
    cat <<'EOF'
usage: scripts/worktree-gc.sh [options]

Report-by-default garbage collector for linked Git worktrees. Default mode
prints a classification table and never mutates the repo. --delete removes
only GC candidates, then runs `git worktree prune`.

Options:
  --delete                 Remove STALE-DETACHED and STALE-MERGED worktrees
  --force                  With --delete, pass --force to `git worktree remove`
                           (never `git branch -D`; never overrides
                           DIRTY/ACTIVE/LOCKED/FRESH)
  --integration REF        Integration branch used for merged/ancestor checks
                           (default: current PR base if this checkout has an
                           open PR, else the remote default branch, else master)
  --max-behind N           Max commits behind upstream still labeled CURRENT
                           (default: 32)
  --stale-age-hours N      Age after which a fully-merged lane with no unique
                           commits may become STALE-MERGED (default: 24). Age
                           is `.git/worktrees/<name>/gitdir` mtime.
  --repo PATH              Git worktree or repo to inspect (default: this
                           script's repo, or $PWD if it is a worktree)
  --scratch-dir DIR        Scratch root for orphan cargo dirs (default: /fast/tmp)
  --scratch-age-hours N    Minimum age to report an orphan scratch (default: 48)
  -h, --help               Show this help

Classification (linked worktrees only; the primary checkout is PRIMARY):
  DIRTY           uncommitted changes — never touchable
  ACTIVE          a process cwd is inside the tree (/proc) — never touchable
  LOCKED          `git worktree lock` (porcelain `locked`) — never touchable
  FRESH           clean, HEAD equals the integration tip, no unique commits,
                  and younger than --stale-age-hours — never a GC candidate
  CURRENT         clean, HEAD equals the branch upstream tip or is an ancestor
                  at most N commits behind — keep
  STALE-DETACHED  clean, detached, HEAD is an ancestor of the integration tip
                  — GC candidate
  STALE-MERGED    clean, on a local branch fully merged into the integration
                  branch with no upstream, AND either unique commits already
                  merged into integration (HEAD is a strict ancestor of the
                  tip) OR the worktree is older than --stale-age-hours
                  — GC candidate + `branch -d`
  FOREIGN         anything else — keep, owner's call
EOF
}

die() {
    echo "worktree-gc: $*" >&2
    exit 1
}

usage_error() {
    echo "worktree-gc: $*" >&2
    usage >&2
    exit 2
}

DELETE=0
FORCE=0
INTEGRATION_ARG=""
MAX_BEHIND=32
STALE_AGE_HOURS=24
REPO_ARG=""
SCRATCH_DIR="/fast/tmp"
SCRATCH_AGE_HOURS=48

while [[ $# -gt 0 ]]; do
    case "$1" in
        --delete)
            DELETE=1
            shift
            ;;
        --force)
            FORCE=1
            shift
            ;;
        --integration)
            [[ $# -ge 2 ]] || usage_error "missing value for $1"
            INTEGRATION_ARG="$2"
            shift 2
            ;;
        --max-behind)
            [[ $# -ge 2 ]] || usage_error "missing value for $1"
            MAX_BEHIND="$2"
            shift 2
            ;;
        --stale-age-hours)
            [[ $# -ge 2 ]] || usage_error "missing value for $1"
            STALE_AGE_HOURS="$2"
            shift 2
            ;;
        --repo)
            [[ $# -ge 2 ]] || usage_error "missing value for $1"
            REPO_ARG="$2"
            shift 2
            ;;
        --scratch-dir)
            [[ $# -ge 2 ]] || usage_error "missing value for $1"
            SCRATCH_DIR="$2"
            shift 2
            ;;
        --scratch-age-hours)
            [[ $# -ge 2 ]] || usage_error "missing value for $1"
            SCRATCH_AGE_HOURS="$2"
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        -*)
            usage_error "unknown argument: $1"
            ;;
        *)
            usage_error "unexpected argument: $1"
            ;;
    esac
done

[[ "$MAX_BEHIND" =~ ^[0-9]+$ ]] || usage_error "--max-behind must be a non-negative integer"
[[ "$STALE_AGE_HOURS" =~ ^[0-9]+$ ]] || usage_error "--stale-age-hours must be a non-negative integer"
[[ "$SCRATCH_AGE_HOURS" =~ ^[1-9][0-9]*$ ]] || usage_error "--scratch-age-hours must be a positive integer"
if ((FORCE == 1)) && ((DELETE == 0)); then
    usage_error "--force requires --delete"
fi

SCRIPT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

resolve_repo() {
    local candidate
    if [[ -n "$REPO_ARG" ]]; then
        candidate="$REPO_ARG"
    elif git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        candidate="$(git rev-parse --show-toplevel)"
    else
        candidate="$SCRIPT_ROOT"
    fi
    git -C "$candidate" rev-parse --is-inside-work-tree >/dev/null 2>&1 ||
        die "not a git worktree: $candidate"
    git -C "$candidate" rev-parse --show-toplevel
}

REPO="$(resolve_repo)"

primary_root() {
    local git_common_dir primary
    git_common_dir="$(git -C "$REPO" rev-parse --path-format=absolute --git-common-dir)"
    primary="${git_common_dir%/}"
    primary="${primary%/.git}"
    if [[ -z "$primary" || "$primary" == "$git_common_dir" ]]; then
        die "unable to derive primary checkout from git-common-dir ($git_common_dir)"
    fi
    printf '%s\n' "$primary"
}

PRIMARY="$(primary_root)"

detect_default_integration() {
    local base=""
    if command -v gh >/dev/null 2>&1; then
        base="$(
            cd -- "$REPO" || exit 0
            if command -v timeout >/dev/null 2>&1; then
                timeout 8 gh pr view --json baseRefName --jq .baseRefName 2>/dev/null || true
            else
                gh pr view --json baseRefName --jq .baseRefName 2>/dev/null || true
            fi
        )"
    fi
    if [[ -n "$base" ]]; then
        printf '%s\n' "$base"
        return
    fi
    if base="$(git -C "$REPO" symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null)"; then
        printf '%s\n' "${base#origin/}"
        return
    fi
    printf '%s\n' master
}

if [[ -z "$INTEGRATION_ARG" ]]; then
    INTEGRATION_ARG="$(detect_default_integration)"
fi

# Prefer the remote-tracking tip when the caller names a local branch so
# "merged into integration" tracks published history, not a stale local ref.
resolve_integration() {
    local ref="$1"
    local tip=""

    if [[ "$ref" == origin/* || "$ref" == refs/* ]]; then
        tip="$(git -C "$REPO" rev-parse --verify --quiet "${ref}^{commit}")" ||
            die "unknown integration ref: $ref"
        INTEGRATION_NAME="$ref"
        INTEGRATION_TIP="$tip"
        return
    fi
    if tip="$(git -C "$REPO" rev-parse --verify --quiet "refs/remotes/origin/${ref}^{commit}")"; then
        INTEGRATION_NAME="origin/$ref"
        INTEGRATION_TIP="$tip"
        return
    fi
    if tip="$(git -C "$REPO" rev-parse --verify --quiet "refs/heads/${ref}^{commit}")"; then
        INTEGRATION_NAME="$ref"
        INTEGRATION_TIP="$tip"
        return
    fi
    tip="$(git -C "$REPO" rev-parse --verify --quiet "${ref}^{commit}")" ||
        die "unknown integration ref: $ref"
    INTEGRATION_NAME="$ref"
    INTEGRATION_TIP="$tip"
}

resolve_integration "$INTEGRATION_ARG"
INTEGRATION_SHORT="${INTEGRATION_NAME#origin/}"
INTEGRATION_SHORT="${INTEGRATION_SHORT#refs/heads/}"
INTEGRATION_SHORT="${INTEGRATION_SHORT#refs/remotes/origin/}"

repo_git() {
    git -C "$REPO" "$@"
}

short_sha() {
    repo_git rev-parse --short "$1"
}

is_ancestor() {
    repo_git merge-base --is-ancestor "$1" "$2"
}

branch_upstream() {
    local branch="$1"
    repo_git rev-parse --abbrev-ref "${branch}@{upstream}" 2>/dev/null || true
}

require_absolute_path() {
    local path="$1"
    if [[ -z "$path" || "$path" != /* ]]; then
        die "refusing non-absolute worktree path: ${path:-<empty>}"
    fi
}

# Unique commits already absorbed into integration: HEAD is a strict ancestor
# of the integration tip. HEAD equal to the tip has no unique commits.
branch_has_unique_commits_merged() {
    local head="$1"
    [[ -n "$head" && "$head" != "$INTEGRATION_TIP" ]] && is_ancestor "$head" "$INTEGRATION_TIP"
}

worktree_gitdir_path() {
    local path="$1"
    local git_dir gitdir_file
    git_dir="$(git -C "$path" rev-parse --path-format=absolute --git-dir 2>/dev/null)" || return 1
    gitdir_file="${git_dir}/gitdir"
    [[ -f "$gitdir_file" ]] || return 1
    printf '%s' "$gitdir_file"
}

worktree_age_seconds() {
    local path="$1"
    local gitdir_file mtime now
    gitdir_file="$(worktree_gitdir_path "$path")" || return 1
    mtime="$(stat -c '%Y' "$gitdir_file" 2>/dev/null || true)"
    [[ -n "$mtime" ]] || return 1
    now="$(date +%s)"
    printf '%s' "$((now - mtime))"
}

worktree_is_aged() {
    local path="$1"
    local age
    age="$(worktree_age_seconds "$path")" || return 1
    ((age >= STALE_AGE_HOURS * 3600))
}

porcelain_locked_now() {
    local target="$1"
    local line cur="" locked=0
    require_absolute_path "$target"
    while IFS= read -r line || [[ -n "$line" ]]; do
        if [[ -z "$line" ]]; then
            if [[ "$cur" == "$target" ]] && ((locked == 1)); then
                return 0
            fi
            cur=""
            locked=0
            continue
        fi
        case "$line" in
            worktree\ *)
                if [[ -n "$cur" && "$cur" == "$target" ]] && ((locked == 1)); then
                    return 0
                fi
                cur="${line#worktree }"
                locked=0
                ;;
            locked*)
                locked=1
                ;;
        esac
    done < <(repo_git worktree list --porcelain)
    [[ -n "$cur" && "$cur" == "$target" ]] && ((locked == 1))
}

live_cwd_pids_now() {
    local wt="$1"
    local wt_real cwd_link dest pid
    local found=()
    wt_real="$(readlink -f "$wt" 2>/dev/null || printf '%s' "$wt")"
    shopt -s nullglob
    for cwd_link in /proc/[0-9]*/cwd; do
        pid="${cwd_link#/proc/}"
        pid="${pid%/cwd}"
        dest="$(readlink -f "$cwd_link" 2>/dev/null || true)"
        [[ -n "$dest" ]] || continue
        if path_is_inside "$wt_real" "$dest"; then
            found+=("$pid")
        fi
    done
    shopt -u nullglob
    if ((${#found[@]} == 0)); then
        return 0
    fi
    if ((${#found[@]} > 8)); then
        printf '%s,... (%d pids)' "$(IFS=','; printf '%s' "${found[*]:0:8}")" "${#found[@]}"
        return 0
    fi
    local IFS=','
    printf '%s' "${found[*]}"
}

# --- /proc cwd inventory (one pass) -----------------------------------------

PROC_PID=()
PROC_CWD=()

scan_proc_cwds() {
    local cwd_link pid dest
    shopt -s nullglob
    for cwd_link in /proc/[0-9]*/cwd; do
        pid="${cwd_link#/proc/}"
        pid="${pid%/cwd}"
        dest="$(readlink -f "$cwd_link" 2>/dev/null || true)"
        [[ -n "$dest" ]] || continue
        PROC_PID+=("$pid")
        PROC_CWD+=("$dest")
    done
    shopt -u nullglob
}

path_is_inside() {
    local parent="$1" child="$2"
    [[ "$child" == "$parent" || "$child" == "$parent"/* ]]
}

active_pids_for() {
    local wt="$1"
    local wt_real i c
    local found=()
    wt_real="$(readlink -f "$wt" 2>/dev/null || printf '%s' "$wt")"
    for i in "${!PROC_CWD[@]}"; do
        c="${PROC_CWD[$i]}"
        if path_is_inside "$wt_real" "$c"; then
            found+=("${PROC_PID[$i]}")
        fi
    done
    if ((${#found[@]} == 0)); then
        return 0
    fi
    if ((${#found[@]} > 8)); then
        printf '%s,... (%d pids)' "$(IFS=','; printf '%s' "${found[*]:0:8}")" "${#found[@]}"
        return 0
    fi
    local IFS=','
    printf '%s' "${found[*]}"
}

dirty_path_count() {
    local path="$1"
    local output
    [[ -d "$path" ]] || {
        printf '%s' 0
        return 0
    }
    if ! output="$(git -C "$path" status --porcelain --untracked-files=normal 2>/dev/null)"; then
        printf '%s' -1
        return 0
    fi
    if [[ -z "$output" ]]; then
        printf '%s' 0
        return 0
    fi
    local -a lines
    mapfile -t lines <<<"$output"
    printf '%s' "${#lines[@]}"
}

# --- worktree inventory ------------------------------------------------------

WT_PATH=()
WT_HEAD=()
WT_BRANCH=()
WT_DETACHED=()
WT_LOCKED=()
WT_PRUNABLE=()
WT_STATUS=()
WT_REASON=()

CUR_PATH=""
CUR_HEAD=""
CUR_BRANCH=""
CUR_DETACHED=0
CUR_LOCKED=0
CUR_PRUNABLE=0

reset_current_worktree() {
    CUR_PATH=""
    CUR_HEAD=""
    CUR_BRANCH=""
    CUR_DETACHED=0
    CUR_LOCKED=0
    CUR_PRUNABLE=0
}

flush_worktree() {
    [[ -n "$CUR_PATH" ]] || return 0
    WT_PATH+=("$CUR_PATH")
    WT_HEAD+=("$CUR_HEAD")
    WT_BRANCH+=("$CUR_BRANCH")
    WT_DETACHED+=("$CUR_DETACHED")
    WT_LOCKED+=("$CUR_LOCKED")
    WT_PRUNABLE+=("$CUR_PRUNABLE")
    reset_current_worktree
}

parse_worktrees() {
    local line
    reset_current_worktree
    while IFS= read -r line || [[ -n "$line" ]]; do
        if [[ -z "$line" ]]; then
            flush_worktree
            continue
        fi
        case "$line" in
            worktree\ *)
                flush_worktree
                CUR_PATH="${line#worktree }"
                require_absolute_path "$CUR_PATH"
                ;;
            HEAD\ *)
                CUR_HEAD="${line#HEAD }"
                ;;
            branch\ *)
                CUR_BRANCH="${line#branch }"
                CUR_DETACHED=0
                ;;
            detached)
                CUR_DETACHED=1
                CUR_BRANCH=""
                ;;
            locked*)
                CUR_LOCKED=1
                ;;
            prunable*)
                CUR_PRUNABLE=1
                ;;
            bare)
                reset_current_worktree
                ;;
        esac
    done < <(repo_git worktree list --porcelain)
    flush_worktree
}

branch_short_name() {
    local ref="$1"
    ref="${ref#refs/heads/}"
    printf '%s\n' "$ref"
}

is_integration_branch() {
    local short="$1"
    [[ "$short" == "$INTEGRATION_SHORT" ]]
}

classify_vs_upstream() {
    local head="$1" branch="$2"
    local upstream up_tip behind
    upstream="$(branch_upstream "$branch")"
    if [[ -z "$upstream" ]]; then
        return 1
    fi
    up_tip="$(repo_git rev-parse --verify --quiet "${upstream}^{commit}")" || return 1
    if [[ "$head" == "$up_tip" ]]; then
        printf '%s\n' "CURRENT|HEAD equals upstream ${upstream}"
        return 0
    fi
    if is_ancestor "$head" "$up_tip"; then
        behind="$(repo_git rev-list --count "${head}..${up_tip}")"
        if ((behind <= MAX_BEHIND)); then
            printf '%s\n' "CURRENT|HEAD is ${behind} commit(s) behind upstream ${upstream} (≤${MAX_BEHIND})"
            return 0
        fi
        printf '%s\n' "FOREIGN|HEAD is ${behind} commit(s) behind upstream ${upstream} (>${MAX_BEHIND})"
        return 0
    fi
    printf '%s\n' "FOREIGN|HEAD has diverged from or is ahead of upstream ${upstream}"
    return 0
}

classify_worktree() {
    local path="$1" head="$2" branch_ref="$3" detached="$4" locked="$5" prunable="$6"
    local branch dirty pids verdict extra="" unique_merged=0 aged=0 age_secs="" why=""

    if [[ "$path" == "$PRIMARY" ]]; then
        printf '%s\n' "PRIMARY|primary checkout — never a GC candidate"
        return
    fi

    if ((prunable == 1)) || [[ ! -e "$path" ]]; then
        extra="directory missing (prunable)"
    fi

    dirty="$(dirty_path_count "$path")"
    if ((dirty < 0)); then
        printf '%s\n' "FOREIGN|git status failed; owner's call"
        return
    fi
    if ((dirty > 0)); then
        printf '%s\n' "DIRTY|${path}: uncommitted changes (${dirty} paths) — never touchable"
        return
    fi

    pids="$(active_pids_for "$path")"
    if [[ -n "$pids" ]]; then
        printf '%s\n' "ACTIVE|${path}: process cwd inside worktree (pid ${pids}) — never touchable"
        return
    fi

    if ((locked == 1)); then
        printf '%s\n' "LOCKED|git worktree lock on ${path} — never touchable"
        return
    fi

    if ((detached == 1)); then
        if [[ -n "$head" ]] && is_ancestor "$head" "$INTEGRATION_TIP"; then
            if [[ -n "$extra" ]]; then
                printf '%s\n' "STALE-DETACHED|detached HEAD is an ancestor of ${INTEGRATION_NAME}; ${extra}"
            else
                printf '%s\n' "STALE-DETACHED|detached HEAD is an ancestor of ${INTEGRATION_NAME}"
            fi
            return
        fi
        if [[ -n "$extra" ]]; then
            printf '%s\n' "FOREIGN|detached HEAD is not an ancestor of ${INTEGRATION_NAME}; ${extra}"
        else
            printf '%s\n' "FOREIGN|detached HEAD is not an ancestor of ${INTEGRATION_NAME}"
        fi
        return
    fi

    branch="$(branch_short_name "$branch_ref")"
    if [[ -z "$branch" ]]; then
        printf '%s\n' "FOREIGN|porcelain listed neither branch nor detached"
        return
    fi

    if is_integration_branch "$branch"; then
        if verdict="$(classify_vs_upstream "$head" "$branch")"; then
            printf '%s\n' "$verdict"
            return
        fi
        printf '%s\n' "FOREIGN|linked checkout of integration branch ${branch}; owner's call"
        return
    fi

    if verdict="$(classify_vs_upstream "$head" "$branch")"; then
        printf '%s\n' "$verdict"
        return
    fi

    if [[ -n "$head" ]] && is_ancestor "$head" "$INTEGRATION_TIP"; then
        if branch_has_unique_commits_merged "$head"; then
            unique_merged=1
        fi
        if worktree_is_aged "$path"; then
            aged=1
            age_secs="$(worktree_age_seconds "$path" || true)"
        fi
        if ((unique_merged == 1 || aged == 1)); then
            why="local branch ${branch} at ${path} is fully merged into ${INTEGRATION_NAME} and has no upstream"
            if ((unique_merged == 1)); then
                why="${why}; unique commits merged into ${INTEGRATION_NAME}"
            else
                why="${why}; no unique commits but gitdir mtime is older than ${STALE_AGE_HOURS}h"
            fi
            if ((aged == 1)) && [[ -n "$age_secs" ]]; then
                why="${why} (age $(format_age "$age_secs"))"
            fi
            if [[ -n "$extra" ]]; then
                why="${why}; ${extra}"
            fi
            printf '%s\n' "STALE-MERGED|${why}"
            return
        fi
        printf '%s\n' "FRESH|${path} HEAD equals ${INTEGRATION_NAME} with no unique commits and younger than ${STALE_AGE_HOURS}h — never a GC candidate"
        return
    fi

    if [[ -n "$extra" ]]; then
        printf '%s\n' "FOREIGN|local branch ${branch} has no upstream and is not merged into ${INTEGRATION_NAME}; ${extra}"
    else
        printf '%s\n' "FOREIGN|local branch ${branch} has no upstream and is not merged into ${INTEGRATION_NAME}"
    fi
}

classify_all() {
    local i verdict status reason
    for i in "${!WT_PATH[@]}"; do
        verdict="$(classify_worktree \
            "${WT_PATH[$i]}" \
            "${WT_HEAD[$i]}" \
            "${WT_BRANCH[$i]}" \
            "${WT_DETACHED[$i]}" \
            "${WT_LOCKED[$i]}" \
            "${WT_PRUNABLE[$i]}")"
        status="${verdict%%|*}"
        reason="${verdict#*|}"
        WT_STATUS+=("$status")
        WT_REASON+=("$reason")
    done
}

# --- scratch orphans (report only) ------------------------------------------

scratch_kind() {
    local base="$1"
    if [[ "$base" == *-target ]]; then
        printf '%s\n' target
    else
        printf '%s\n' td-rt
    fi
}

scratch_stems() {
    local base="$1"
    local stem
    printf '%s\n' "$base"
    if [[ "$base" == *-target ]]; then
        stem="${base%-target}"
        printf '%s\n' "$stem"
        if [[ "$stem" == td-rt-* ]]; then
            printf '%s\n' "td-${stem#td-rt-}"
            printf '%s\n' "${stem#td-rt-}"
        fi
    elif [[ "$base" == td-rt-* ]]; then
        stem="${base#td-rt-}"
        printf '%s\n' "$stem"
        printf '%s\n' "td-${stem}"
        printf '%s\n' "td-rt-${stem}"
    fi
}

scratch_matches_worktree() {
    local scratch="$1"
    local base stem wt wtbase
    base="$(basename -- "$scratch")"
    while IFS= read -r stem; do
        [[ -n "$stem" ]] || continue
        for wt in "${WT_PATH[@]}"; do
            wtbase="$(basename -- "$wt")"
            if [[ "$wtbase" == "$stem" || "$wt" == "$scratch" ]]; then
                return 0
            fi
        done
    done < <(scratch_stems "$base")
    return 1
}

format_age() {
    local secs="$1"
    local days=$((secs / 86400))
    local hours=$(((secs % 86400) / 3600))
    if ((days > 0)); then
        printf '%dd%dh' "$days" "$hours"
    else
        printf '%dh' "$hours"
    fi
}

list_scratch_dirs() {
    local dir="$1"
    local p
    [[ -d "$dir" ]] || return 0
    shopt -s nullglob
    for p in "$dir"/td-rt-* "$dir"/*-target; do
        [[ -d "$p" ]] || continue
        printf '%s\n' "$p"
    done
    shopt -u nullglob
}

# --- output ------------------------------------------------------------------

print_report() {
    local i path head_short branch status reason
    local n_primary=0 n_dirty=0 n_active=0 n_locked=0 n_fresh=0 n_current=0
    local n_stale_det=0 n_stale_mer=0 n_foreign=0
    local integration_short

    integration_short="$(short_sha "$INTEGRATION_TIP")"
    printf 'worktree-gc  mode=%s  integration=%s (%s)  max-behind=%s\n' \
        "$( ((DELETE == 1)) && printf delete || printf report)" \
        "$INTEGRATION_NAME" \
        "$integration_short" \
        "$MAX_BEHIND"
    printf 'primary      %s\n' "$PRIMARY"
    printf '\n'
    printf '%-15s %-12s %-36s %s\n' STATUS HEAD BRANCH TREE
    printf '%-15s %-12s %-36s %s\n' "---------------" "------------" "------------------------------------" "----"

    for i in "${!WT_PATH[@]}"; do
        path="${WT_PATH[$i]}"
        if [[ -n "${WT_HEAD[$i]}" ]]; then
            head_short="$(short_sha "${WT_HEAD[$i]}")"
        else
            head_short="-"
        fi
        if ((WT_DETACHED[i] == 1)); then
            branch="(detached)"
        else
            branch="$(branch_short_name "${WT_BRANCH[$i]}")"
            [[ -n "$branch" ]] || branch="-"
        fi
        status="${WT_STATUS[$i]}"
        reason="${WT_REASON[$i]}"
        printf '%-15s %-12s %-36s %s\n' "$status" "$head_short" "$branch" "$path"
        printf '                %s\n' "$reason"
        case "$status" in
            PRIMARY) n_primary=$((n_primary + 1)) ;;
            DIRTY) n_dirty=$((n_dirty + 1)) ;;
            ACTIVE) n_active=$((n_active + 1)) ;;
            LOCKED) n_locked=$((n_locked + 1)) ;;
            FRESH) n_fresh=$((n_fresh + 1)) ;;
            CURRENT) n_current=$((n_current + 1)) ;;
            STALE-DETACHED) n_stale_det=$((n_stale_det + 1)) ;;
            STALE-MERGED) n_stale_mer=$((n_stale_mer + 1)) ;;
            FOREIGN) n_foreign=$((n_foreign + 1)) ;;
        esac
    done

    printf '\n'
    printf 'summary  %d worktrees  primary=%d  dirty=%d  active=%d  locked=%d  fresh=%d  current=%d  foreign=%d  stale-detached=%d  stale-merged=%d\n' \
        "${#WT_PATH[@]}" \
        "$n_primary" \
        "$n_dirty" \
        "$n_active" \
        "$n_locked" \
        "$n_fresh" \
        "$n_current" \
        "$n_foreign" \
        "$n_stale_det" \
        "$n_stale_mer"
    printf 'gc candidates are STALE-DETACHED and STALE-MERGED only; DIRTY/ACTIVE/LOCKED/FRESH are never touchable\n'
}

print_scratches() {
    local now mtime age min_age path base kind
    local -a orphans=()
    local -a orphan_ages=()
    local -a orphan_kinds=()
    local n_seen=0

    now="$(date +%s)"
    min_age=$((SCRATCH_AGE_HOURS * 3600))

    while IFS= read -r path; do
        [[ -n "$path" ]] || continue
        n_seen=$((n_seen + 1))
        scratch_matches_worktree "$path" && continue
        mtime="$(stat -c '%Y' "$path" 2>/dev/null || true)"
        [[ -n "$mtime" ]] || continue
        age=$((now - mtime))
        ((age >= min_age)) || continue
        base="$(basename -- "$path")"
        kind="$(scratch_kind "$base")"
        orphans+=("$path")
        orphan_ages+=("$(format_age "$age")")
        orphan_kinds+=("$kind")
    done < <(list_scratch_dirs "$SCRATCH_DIR" | sort -u)

    printf '\n'
    printf 'orphaned scratch under %s  (age>=%sh, no matching worktree; report only, never deleted)\n' \
        "$SCRATCH_DIR" "$SCRATCH_AGE_HOURS"
    if ((${#orphans[@]} == 0)); then
        printf '  (none; scanned %d td-rt-* / *-target dirs)\n' "$n_seen"
        return 0
    fi
    printf '%-10s %-8s %s\n' KIND AGE PATH
    local i
    for i in "${!orphans[@]}"; do
        printf '%-10s %-8s %s\n' "${orphan_kinds[$i]}" "${orphan_ages[$i]}" "${orphans[$i]}"
    done
    printf 'orphans    %d  (not deleted)\n' "${#orphans[@]}"
}

delete_candidates() {
    local i path status branch rc=0 remove_ok=0 live_pids
    local removed=0 skipped=0 failed=0

    printf '\n'
    printf 'delete mode: removing GC candidates only (exact absolute paths)\n'
    for i in "${!WT_PATH[@]}"; do
        path="${WT_PATH[$i]}"
        status="${WT_STATUS[$i]}"
        case "$status" in
            STALE-DETACHED | STALE-MERGED) ;;
            *)
                skipped=$((skipped + 1))
                continue
                ;;
        esac

        require_absolute_path "$path"

        if porcelain_locked_now "$path"; then
            printf '  skip %s  (LOCKED at delete-time — never touchable)\n' "$path"
            skipped=$((skipped + 1))
            continue
        fi

        live_pids="$(live_cwd_pids_now "$path")"
        if [[ -n "$live_pids" ]]; then
            printf '  skip %s  (ACTIVE at delete-time, pid %s — never touchable)\n' "$path" "$live_pids"
            skipped=$((skipped + 1))
            continue
        fi

        printf '  worktree remove %s  (%s)\n' "$path" "$status"
        remove_ok=0
        if ((FORCE == 1)); then
            repo_git worktree remove --force -- "$path" && remove_ok=1
        else
            repo_git worktree remove -- "$path" && remove_ok=1
        fi
        if ((remove_ok == 0)); then
            echo "worktree-gc: failed to remove $path" >&2
            failed=$((failed + 1))
            rc=1
            continue
        fi
        removed=$((removed + 1))

        if [[ "$status" == STALE-MERGED ]]; then
            branch="$(branch_short_name "${WT_BRANCH[$i]}")"
            if [[ -n "$branch" ]] && is_ancestor "${WT_HEAD[$i]}" "$INTEGRATION_TIP"; then
                printf '  branch -d %s  (fully merged into %s; worktree was %s)\n' \
                    "$branch" "$INTEGRATION_NAME" "$path"
                if ! repo_git branch -d "$branch"; then
                    echo "worktree-gc: left branch $branch (git branch -d refused; never -D)" >&2
                    failed=$((failed + 1))
                    rc=1
                fi
            else
                echo "worktree-gc: skipped branch -d for $path (not fully merged into $INTEGRATION_NAME)" >&2
            fi
        fi
    done

    printf '  worktree prune\n'
    if ! repo_git worktree prune; then
        echo "worktree-gc: git worktree prune failed" >&2
        rc=1
        failed=$((failed + 1))
    fi

    printf 'delete     removed=%d  skipped=%d  failed=%d\n' "$removed" "$skipped" "$failed"
    return "$rc"
}

scan_proc_cwds
parse_worktrees
((${#WT_PATH[@]} > 0)) || die "git worktree list returned no worktrees"
classify_all
print_report
print_scratches

if ((DELETE == 1)); then
    delete_candidates
fi
