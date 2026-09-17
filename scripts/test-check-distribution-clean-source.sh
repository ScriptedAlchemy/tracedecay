#!/usr/bin/env bash
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
gate="$root/scripts/check-distribution-acceptance.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/tracedecay-clean-source-test.XXXXXX")
trap 'rm -rf -- "$work"' EXIT

repo="$work/repo"
bin="$work/bin"
mkdir -p -- "$repo/.release-automation" "$bin"
for command in cargo rustc; do
  cat >"$bin/$command" <<'SH'
#!/usr/bin/env bash
exit 2
SH
  chmod +x "$bin/$command"
done
cat >"$repo/Cargo.toml" <<'TOML'
[workspace]
members = []
TOML

git -C "$repo" init -q
git -C "$repo" config user.name "TraceDecay test"
git -C "$repo" config user.email "test@tracedecay.local"
git -C "$repo" add Cargo.toml
git -C "$repo" commit -qm "test fixture"
git -C "$repo/.release-automation" init -q

output="$work/output"
if PATH="$bin:$PATH" "$gate" --repo "$repo" >"$output" 2>&1; then
  echo "distribution gate unexpectedly accepted the incomplete fixture repository" >&2
  exit 1
fi

if grep -Fq "source repository has tracked or untracked drift" "$output"; then
  echo "workflow-owned .release-automation checkout was treated as source drift" >&2
  exit 1
fi
grep -Fq \
  "packaged host-event fixture copy differs from its authority: claude.json" \
  "$output"

printf 'untracked source\n' >"$repo/untracked.txt"
if PATH="$bin:$PATH" "$gate" --repo "$repo" >"$output" 2>&1; then
  echo "distribution gate unexpectedly accepted real untracked source drift" >&2
  exit 1
fi
grep -Fq "source repository has tracked or untracked drift (?? untracked.txt)" "$output"

printf 'distribution clean-source regression passed\n'
