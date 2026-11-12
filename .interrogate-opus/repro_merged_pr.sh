#!/bin/bash
# Intent: leftovers disappear when "a worktree goes away or its branch's PR merges".
set -u
ROOT=/tmp/tracedecay-storage-redesign/interrogate/opus/merged
rm -rf "$ROOT"; mkdir -p "$ROOT"; cd "$ROOT" || exit 1
export GIT_CONFIG_GLOBAL=/dev/null GIT_AUTHOR_NAME=x GIT_AUTHOR_EMAIL=x@x GIT_COMMITTER_NAME=x GIT_COMMITTER_EMAIL=x@x
git init -q -b main r; cd r || exit 1
echo a > a.txt; git add a.txt; git commit -qm one
git worktree add -q ../pr -b feature
(cd ../pr && echo b > b.txt && git add b.txt && git commit -qm feature)
git merge -q --squash feature && git commit -qm "squash merge of the PR"
git branch -q -D feature 2>/dev/null || echo "branch delete refused: checked out in the worktree"
echo "main has the change: $(git ls-files | tr '\n' ' ')"
echo "git worktree list:"; git worktree list --porcelain | grep -E '^(worktree|branch)'
[ -d "$ROOT/pr" ] && echo "stat rule: Exists. Design row 'PR merged': 'Nothing changes until the checkout moves or goes.'"
cd "$(dirname "$0")" 2>/dev/null
rm -rf "$ROOT"
