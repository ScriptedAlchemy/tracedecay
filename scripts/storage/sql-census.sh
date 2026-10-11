#!/usr/bin/env bash
# Regenerate the guard-parity artifacts in crates/tracedecay-sql/tests/testdata
# (#3402) while the owned stores still run on SQLite:
#   policy.tsv             the SQLite arm's verdict per capability for each
#                          statement already listed (add rows to extend it)
#   census.tsv             every distinct statement the owned-store suites ran,
#                          with its capability
#
# Usage: scripts/storage/sql-census.sh [bazel test flags...]
# BAZEL names the bazel wrapper (default: bazel).
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
bazel=${BAZEL:-bazel}
testdata=crates/tracedecay-sql/tests/testdata
testlogs=$("$bazel" info bazel-testlogs 2>/dev/null)

# Prints the undeclared-output directories of the named test targets.
outputs() {
  for target in "$@"; do
    local package=${target#//}
    find "$testlogs/${package%%:*}/${package##*:}" -type d -name test.outputs 2>/dev/null || true
  done
}

copy_output() { # copy_output TARGET FILE
  local found
  found=$(outputs "$1" | while read -r dir; do [ -f "$dir/$2" ] && echo "$dir/$2"; done | head -n1)
  [ -n "$found" ] || { echo "sql-census: $1 wrote no $2" >&2; exit 1; }
  cp "$found" "$testdata/$2"
}

parity=//crates/tracedecay-sql:policy_parity
census_targets=(
  //crates/tracedecay-rusqlite-runtime/...
  //crates/tracedecay-global-db/...
  //crates/tracedecay-runtime-core/...
  //crates/tracedecay-lcm/...
  //crates/tracedecay-session-memory/...
  //crates/tracedecay-session-temporal-store/...
  //crates/tracedecay-sessions/...
  //crates/tracedecay-query/...
  //crates/tracedecay:storage_suite
)
status=0
"$bazel" test --test_env=TD_SQL_CENSUS=1 \
  --nozip_undeclared_test_outputs --keep_going "$@" "${census_targets[@]}" || status=$?
[ "$status" -eq 0 ] || echo "sql-census: some census suites failed (exit $status); their statements up to the failure are kept" >&2

# Every tests/outputs dir under the census packages, then the sqlite_replay
# column and the policy verdicts, which the default-engine parity test
# computes.
dirs=$("$bazel" query "tests(set(${census_targets[*]}))" 2>/dev/null | while read -r target; do outputs "$target"; done)
{
  printf 'capability\tsql\tsqlite_replay\n'
  for dir in $dirs; do cat "$dir"/sql-census-*.tsv 2>/dev/null || true; done |
    LC_ALL=C sort -u | sed 's/$/\tmissing/'
} >"$testdata/census.tsv"
"$bazel" test --nozip_undeclared_test_outputs "$@" "$parity" >/dev/null 2>&1 || true
copy_output "$parity" census.tsv
copy_output "$parity" policy.tsv
"$bazel" test "$@" "$parity"
wc -l "$testdata"/*.tsv
