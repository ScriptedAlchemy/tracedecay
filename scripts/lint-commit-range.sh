#!/usr/bin/env bash
# Lint every non-merge commit in an exact Git range with one Node process.
set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: scripts/lint-commit-range.sh <base> <head>" >&2
    exit 2
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec node "$script_dir/lint-commit-range.mjs" --repository "$PWD" "$1" "$2"
