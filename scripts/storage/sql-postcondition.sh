#!/usr/bin/env bash
# Fail, listing each site, when owned code still names rusqlite. Only the
# driver's SQLite arm, the foreign session-host readers and (until U17) the
# lexical projection artifact may. Pass paths to check one unit's scope;
# the default is every crate.
#
# Usage: scripts/storage/sql-postcondition.sh [PATH...]
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
if [ "$#" -eq 0 ]; then
  set -- crates
fi

allowed=(
  -g '!crates/tracedecay-sql/src/sqlite.rs'
  -g '!crates/tracedecay-sql/Cargo.toml'
  -g '!crates/tracedecay-sessions/src/runtime/shared.rs'
  -g '!crates/tracedecay-sessions/src/runtime/hosts/**'
  -g '!crates/tracedecay-sessions/Cargo.toml'
  -g '!crates/tracedecay-query/src/retrieval/lexical/**'
  -g '!crates/tracedecay-query/Cargo.toml'
)

# Code paths (`rusqlite::`, `use rusqlite;`, `extern crate rusqlite`) and
# Cargo dependency declarations; prose mentions and the
# tracedecay-rusqlite-runtime crate name do not count.
offenders=$(rg --no-heading --line-number --color never \
  -g '*.rs' -g 'Cargo.toml' "${allowed[@]}" \
  -e '(^|[^A-Za-z0-9_])rusqlite(::|;)' \
  -e 'extern crate rusqlite' \
  -e '^\s*rusqlite\s*=' \
  -e '"dep:rusqlite"' \
  "$@" || true)

if [ -z "$offenders" ]; then
  echo "sql-postcondition: no rusqlite references outside the allowlist"
  exit 0
fi
printf '%s\n' "$offenders"
count=$(printf '%s\n' "$offenders" | wc -l)
files=$(printf '%s\n' "$offenders" | cut -d: -f1 | sort -u | wc -l)
echo "sql-postcondition: $count rusqlite reference(s) in $files file(s) outside the allowlist" >&2
exit 1
