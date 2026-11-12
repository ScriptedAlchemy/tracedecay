#!/bin/bash
# sketch.rs:810 GitFingerprint: HEAD, index signature, packed-refs signature and
# the selected ref only. sketch.rs:157 CaptureWitness: "Digest over the HEAD tree,
# index, admitted untracked and ignored files, sanitizer revision and policy.
# Equal witnesses mean equal content."
set -u
ROOT=/tmp/tracedecay-storage-redesign/interrogate/opus/witness
rm -rf "$ROOT"; mkdir -p "$ROOT"; cd "$ROOT" || exit 1
export GIT_CONFIG_GLOBAL=/dev/null GIT_AUTHOR_NAME=x GIT_AUTHOR_EMAIL=x@x GIT_COMMITTER_NAME=x GIT_COMMITTER_EMAIL=x@x
git init -q -b main r; cd r || exit 1
printf 'pub fn foo() {}\n' > lib.rs; git add lib.rs; git commit -qm one
git worktree add -q ../wt -b feature; cd ../wt || exit 1
G=$(git rev-parse --git-dir); C=$(git rev-parse --git-common-dir)

fingerprint() { # the four paths, by content
  { cat "$G/HEAD"; [ -f "$G/index" ] && shasum "$G/index" | cut -d' ' -f1
    [ -f "$C/packed-refs" ] && shasum "$C/packed-refs" | cut -d' ' -f1
    r=$(sed -n 's/^ref: //p' "$G/HEAD"); [ -f "$C/$r" ] && cat "$C/$r"; } | shasum | cut -d' ' -f1
}
witness_tracked() { # HEAD tree plus the index's own entries, no working tree read
  { git rev-parse 'HEAD^{tree}'; git ls-files -s; } | shasum | cut -d' ' -f1
}

F0=$(fingerprint); W0=$(witness_tracked)
echo "start     fingerprint=$F0 witness=$W0"
printf 'pub fn foo_renamed_and_saved() {}\n' > lib.rs
sleep 1
echo "saved edit, not staged:"
echo "  fingerprint=$(fingerprint) changed=$([ "$(fingerprint)" != "$F0" ] && echo yes || echo NO)"
echo "  witness=$(witness_tracked) changed=$([ "$(witness_tracked)" != "$W0" ] && echo yes || echo NO)"
echo "  git status --short: $(git status --short | tr '\n' ' ')"
F1=$(fingerprint)
echo "after git status refreshes the index: fingerprint changed=$([ "$F1" != "$F0" ] && echo yes || echo NO)"
git checkout -q -- lib.rs
F2=$(fingerprint)
printf 'pub fn brand_new() {}\n' > new.rs
echo "new untracked file: fingerprint changed=$([ "$(fingerprint)" != "$F2" ] && echo yes || echo NO)"
rm new.rs

echo
echo "symbolic branch: HEAD -> alias -> main, commit made from another worktree"
cd ../r || exit 1
git symbolic-ref refs/heads/alias refs/heads/feature
cd ../wt || exit 1
git symbolic-ref HEAD refs/heads/alias
F3=$(fingerprint); H3=$(git rev-parse HEAD)
git update-ref refs/heads/feature "$(git commit-tree 'HEAD^{tree}' -p HEAD -m two)"
echo "  HEAD commit moved=$([ "$(git rev-parse HEAD)" != "$H3" ] && echo yes || echo NO), fingerprint changed=$([ "$(fingerprint)" != "$F3" ] && echo yes || echo NO)"
rm -rf "$ROOT"
