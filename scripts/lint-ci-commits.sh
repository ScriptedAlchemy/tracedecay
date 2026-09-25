#!/usr/bin/env bash
# Lint the commits a CI event is admitting.
#
# A master push lints github.event.before..HEAD, which is how an integration
# merge is judged after it lands. workflow_dispatch is the admission path for
# that integration branch, so it lints the same not-yet-on-the-default-branch
# range. Already published history is not rejudged: a dispatch of the default
# branch has an empty range.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
project_root=$(cd "${script_dir}/.." && pwd)
repository=${REPOSITORY:-$project_root}
event=${EVENT_NAME:-}
head=${HEAD_SHA:-}
zero_sha=0000000000000000000000000000000000000000

if [[ -z $event || -z $head ]]; then
  echo "usage: EVENT_NAME=<push|workflow_dispatch> HEAD_SHA=<sha> [BEFORE_SHA=<sha>] [DEFAULT_BRANCH=<name>] [REPOSITORY=<path>] $0" >&2
  exit 2
fi

lint_range() {
  local base=$1
  node "${script_dir}/lint-commit-range.mjs" --repository "$repository" "$base" "$head"
}

lint_root_commit() {
  git -C "$repository" show --no-patch --format=%B "$head" | (
    cd "$project_root"
    pnpm run --silent lint:commit --
  )
}

resolve_default_branch() {
  local branch=$1
  local remote_ref="refs/remotes/origin/${branch}"
  if git -C "$repository" remote get-url origin >/dev/null 2>&1; then
    git -C "$repository" fetch --no-tags --quiet origin \
      "+refs/heads/${branch}:${remote_ref}" >/dev/null
  fi
  if git -C "$repository" rev-parse --verify --quiet "$remote_ref" >/dev/null; then
    echo "$remote_ref"
    return 0
  fi
  if git -C "$repository" rev-parse --verify --quiet "refs/heads/${branch}" >/dev/null; then
    echo "refs/heads/${branch}"
    return 0
  fi
  echo "commit lint: default branch ${branch} is not available" >&2
  return 1
}

case "$event" in
  push)
    before=${BEFORE_SHA:-}
    if [[ -z $before ]]; then
      echo "commit lint: push requires BEFORE_SHA" >&2
      exit 2
    fi
    if [[ $before == "$zero_sha" ]]; then
      if git -C "$repository" rev-parse --verify --quiet "${head}^" >/dev/null; then
        lint_range "${head}^"
      else
        lint_root_commit
      fi
    else
      lint_range "$before"
    fi
    ;;
  workflow_dispatch)
    default_branch=${DEFAULT_BRANCH:-}
    if [[ -z $default_branch ]]; then
      echo "commit lint: workflow_dispatch requires DEFAULT_BRANCH" >&2
      exit 2
    fi
    upstream=$(resolve_default_branch "$default_branch")
    base=$(git -C "$repository" merge-base "$head" "$upstream")
    lint_range "$base"
    ;;
  *)
    echo "commit lint: unsupported event ${event}" >&2
    exit 2
    ;;
esac
