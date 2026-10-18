#!/usr/bin/env bash
# Create a linked worktree, seed dashboard/app-dist from the primary checkout,
# and run `pnpm install` so a fresh tree can compile and commit.
#
# usage: scripts/agent-worktree.sh <path> [-b <branch>] [<start-point>]
set -euo pipefail

usage() {
    cat <<'EOF'
usage: scripts/agent-worktree.sh <path> [-b <branch>] [<start-point>]

Create a linked Git worktree, lock it as an active agent lane, and seed
dashboard/app-dist from the primary checkout, and run a frozen `pnpm install`
for the worktree's own npm packages and Cargo sources. Prints the unlock+remove
one-liner for the owning lane and the recommended env
(TRACEDECAY_SKIP_DASHBOARD_BUILD=1 plus the seeded bundle's
TRACEDECAY_DASHBOARD_BUNDLE_SHA256 digest).
EOF
}

die() {
    echo "agent-worktree: $*" >&2
    exit 1
}

primary_root_from_git() {
    local git_common_dir primary_root
    git_common_dir="$(git rev-parse --path-format=absolute --git-common-dir)"
    primary_root="${git_common_dir%/}"
    primary_root="${primary_root%/.git}"
    if [[ -z "$primary_root" || "$primary_root" == "$git_common_dir" ]]; then
        die "unable to derive primary checkout from git-common-dir ($git_common_dir)"
    fi
    printf '%s\n' "$primary_root"
}

caller_cwd="$PWD"
script_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

path=""
branch=""
start_point=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        -h | --help)
            usage
            exit 0
            ;;
        -b)
            [[ $# -ge 2 ]] || {
                usage >&2
                exit 2
            }
            branch="$2"
            shift 2
            ;;
        -*)
            echo "agent-worktree: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
        *)
            if [[ -z "$path" ]]; then
                path="$1"
            elif [[ -z "$start_point" ]]; then
                start_point="$1"
            else
                echo "agent-worktree: unexpected argument: $1" >&2
                usage >&2
                exit 2
            fi
            shift
            ;;
    esac
done

if [[ -z "$path" ]]; then
    usage >&2
    exit 2
fi

if [[ "$path" != /* ]]; then
    path="$caller_cwd/$path"
fi
if [[ -d "$(dirname -- "$path")" ]]; then
    path="$(cd -- "$(dirname -- "$path")" && pwd)/$(basename -- "$path")"
fi

cd -- "$script_root"
primary_root="$(primary_root_from_git)"
src_app_dist="$primary_root/dashboard/app-dist"

if [[ ! -d "$src_app_dist" ]]; then
    die "primary checkout has no dashboard/app-dist at $src_app_dist; build the dashboard there first"
fi
if [[ -z "$(ls -A "$src_app_dist")" ]]; then
    die "primary checkout dashboard/app-dist is empty at $src_app_dist; build the dashboard there first"
fi

add_args=(worktree add)
if [[ -n "$branch" ]]; then
    add_args+=(-b "$branch")
fi
add_args+=("$path")
if [[ -n "$start_point" ]]; then
    add_args+=("$start_point")
fi
git "${add_args[@]}"

worktree="$(cd -- "$path" && pwd)"
mkdir -p "$worktree/dashboard/app-dist"
cp -R "$src_app_dist/." "$worktree/dashboard/app-dist/"

# Cargo resolves only from the `.pnpm/crates` sources matching this tree's
# Cargo.lock; pnpm links them (and node_modules) from its shared store.
(cd -- "$worktree" && pnpm install --frozen-lockfile)

git worktree lock "$worktree" --reason "active agent lane"

echo "Worktree created and locked: $worktree"
echo "Seeded dashboard/app-dist from $src_app_dist"
echo "Installed npm packages and Cargo sources with pnpm"
echo
echo "When this lane is finished, unlock and remove the exact path:"
echo "  git worktree unlock $worktree && git worktree remove $worktree"
# Skip-mode builds of tracedecay-cli require the expected digest of the
# prebuilt bundle alongside the skip flag; compute it for the seeded copy so
# the recommendation stays honest under the digest contract.
bundle_digest="$(python3 "$worktree/scripts/check-dashboard-bundle.py" "$worktree/dashboard/app-dist" --print-digest)"

echo
echo "Recommended env:"
echo "  export TRACEDECAY_SKIP_DASHBOARD_BUILD=1"
echo "  export TRACEDECAY_DASHBOARD_BUNDLE_SHA256=$bundle_digest"
