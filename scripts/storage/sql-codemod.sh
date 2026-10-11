#!/usr/bin/env bash
# Point Rust sources at the owned-store driver: rewrites `rusqlite::` paths and
# `use rusqlite;` to `tracedecay_sql` in every .rs file under the given paths.
# Idempotent: the replacement never matches the pattern. It edits sources only;
# the owning crate must depend on `tracedecay-sql` (then run `pnpm install` and
# `bazel run //scripts:bazel/gen_builds.py`).
#
# Usage: scripts/storage/sql-codemod.sh PATH...
set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: $0 PATH..." >&2
  exit 2
fi

changed=0
while IFS= read -r -d '' file; do
  if grep -Eq '(^|[^A-Za-z0-9_])rusqlite(::|;)' "$file"; then
    perl -pi -e 's/(?<![A-Za-z0-9_])rusqlite(?=::|;)/tracedecay_sql/g' "$file"
    echo "$file"
    changed=$((changed + 1))
  fi
done < <(find "$@" -type f -name '*.rs' -print0)
echo "sql-codemod: rewrote $changed file(s)" >&2
