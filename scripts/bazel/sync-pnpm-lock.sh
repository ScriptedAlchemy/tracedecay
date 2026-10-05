#!/usr/bin/env bash
# pnpm 12 writes a leading document recording its own packageManager
# resolution before the real lockfile document. rules_js'
# npm_translate_lock parses single-document YAML only, so the Bazel lane
# consumes this derived copy. The copy keeps every line after the second
# `---` marker, which is the actual dependency lockfile.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

# The output stays at the repo root. npm_translate_lock resolves the root
# package.json relative to the lockfile's directory.
out=pnpm-lock.bazel.yaml
awk 'BEGIN{n=0} /^---[[:space:]]*$/{n++; if(n==2){emit=1; next}} emit' \
    pnpm-lock.yaml > "$out"
if ! grep -q "^lockfileVersion:" "$out"; then
    echo "sync-pnpm-lock: $out lost its lockfileVersion header" >&2
    exit 1
fi
