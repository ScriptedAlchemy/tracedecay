#!/bin/bash
# Probes the design's liveness rules against real git and a real unmounted volume.
# sketch.rs:799 "Permission errors, unavailable volumes and inconsistent metadata
# are Unknown. A prunable entry with no directory is Gone."
set -u
ROOT=/tmp/tracedecay-storage-redesign/interrogate/opus/liveness
rm -rf "$ROOT"; mkdir -p "$ROOT"; cd "$ROOT" || exit 1
export GIT_CONFIG_GLOBAL=/dev/null GIT_AUTHOR_NAME=x GIT_AUTHOR_EMAIL=x@x GIT_COMMITTER_NAME=x GIT_COMMITTER_EMAIL=x@x

classify() { # locator: what the design's stat plus porcelain rule returns
  local loc="$1"
  if [ -e "$loc" ]; then echo "Exists"; else
    local err; err=$(stat "$loc" 2>&1 | sed 's/.*: //')
    echo "Gone (stat says: $err)"
  fi
}

echo "=== A. Linked worktree on a removable volume, volume unmounted, worktree LOCKED"
git init -q -b main main && (cd main && echo a > a.txt && git add a.txt && git commit -qm a)
hdiutil create -quiet -size 8m -fs APFS -volname TDProbe "$ROOT/vol.dmg"
MP="$ROOT/mnt"; mkdir -p "$MP"
hdiutil attach -quiet -nobrowse -mountpoint "$MP" "$ROOT/vol.dmg"
(cd main && git worktree add -q "$MP/wt" -b onvolume)
(cd main && git worktree lock --reason "on removable volume" "$MP/wt")
echo "before unmount: $(classify "$MP/wt")"
hdiutil detach -quiet "$MP"
echo "after unmount:  $(classify "$MP/wt")"
echo "--- git worktree list --porcelain"
(cd main && git worktree list --porcelain | sed -n '/onvolume/,$p;/mnt\/wt/p' | sort -u)
echo "--- required by requirement 28 and sketch.rs:800: Unknown, keep the slot"

echo
echo "=== B. Same, not locked"
hdiutil attach -quiet -nobrowse -mountpoint "$MP" "$ROOT/vol.dmg"
(cd main && git worktree unlock "$MP/wt")
hdiutil detach -quiet "$MP"
echo "classification: $(classify "$MP/wt")"
(cd main && git worktree list --porcelain | grep -E "^worktree .*wt|prunable|locked")
(cd main && git worktree prune -v --dry-run)

echo
echo "=== C. Whole repository on an unmounted volume: the design's 'Repository deleted' row"
hdiutil attach -quiet -nobrowse -mountpoint "$MP" "$ROOT/vol.dmg"
git init -q -b main "$MP/repo" && (cd "$MP/repo" && echo a > a.txt && git add a.txt && git commit -qm a)
hdiutil detach -quiet "$MP"
echo "classification of the only enrolled locator: $(classify "$MP/repo")"
echo "design row: 'Every enrolled locator proven absent. Slots revoked, store directory removed after holds.'"
hdiutil attach -quiet -nobrowse -mountpoint "$MP" "$ROOT/vol.dmg"
echo "after remount, the repository was never gone: $(git -C "$MP/repo" log --oneline | wc -l | tr -d ' ') commit, $(classify "$MP/repo")"
hdiutil detach -quiet "$MP"

echo
echo "=== D. Worktree path reused by a different repository"
git init -q -b main other && (cd other && echo b > b.txt && git add b.txt && git commit -qm b)
(cd main && git worktree add -q "$ROOT/P" -b feature)
echo "enrolled in project A: $ROOT/P"
(cd main && git worktree remove "$ROOT/P")
git clone -q other "$ROOT/P"
echo "A's git worktree list:"; (cd main && git worktree list --porcelain | grep ^worktree)
echo "stat rule for A's slot: $(classify "$ROOT/P")"
echo "P now belongs to: $(git -C "$ROOT/P" rev-parse --git-common-dir) , origin $(git -C "$ROOT/P" remote get-url origin)"
echo "CheckoutObservation::Exists carries only a locator (sketch.rs:272). A's slot is never Gone."

echo
echo "=== E. Main checkout deleted, linked worktree directory left behind"
git init -q -b main m2 && (cd m2 && echo a > a.txt && git add a.txt && git commit -qm a && git worktree add -q "$ROOT/leftover" -b l)
rm -rf "$ROOT/m2"
echo "stat rule for the leftover locator: $(classify "$ROOT/leftover")"
echo "git in it: $(git -C "$ROOT/leftover" status 2>&1 | head -1)"
echo "design row 'Repository deleted' needs every locator proven absent, so the store stays."
rm -rf "$ROOT"
