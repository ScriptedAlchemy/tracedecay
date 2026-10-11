#!/usr/bin/env bash
# Regenerate the guard-parity artifacts in crates/tracedecay-sql/tests/testdata
# (#3402) while the owned stores still run on SQLite:
#   policy.tsv, policy-fixture.sql  the SQLite authorizers' verdicts per capability
#   census.tsv                      every distinct statement the owned-store
#                                   suites ran, with its capability
#   known_native_gaps.tsv           the native gate's current failures (`gaps`)
#
# Usage: scripts/storage/sql-census.sh [census|gaps] [bazel test flags...]
# BAZEL names the bazel wrapper (default: bazel).
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
bazel=${BAZEL:-bazel}
mode=${1:-census}
[ "$#" -gt 0 ] && shift
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

case $mode in
census)
  census_targets=(
    //crates/tracedecay-rusqlite-runtime/...
    //crates/tracedecay-global-db/...
    //crates/tracedecay-runtime-core/...
    //crates/tracedecay-lcm/...
    //crates/tracedecay-session-memory/...
    //crates/tracedecay-session-temporal-store/...
    //crates/tracedecay-sessions/...
    //crates/tracedecay:storage_suite
  )
  status=0
  "$bazel" test --test_env=TD_SQL_CENSUS=1 \
    --nozip_undeclared_test_outputs --keep_going "$@" "${census_targets[@]}" || status=$?
  [ "$status" -eq 0 ] || echo "sql-census: some census suites failed (exit $status); their statements up to the failure are kept" >&2

  copy_output //crates/tracedecay-rusqlite-runtime:unit_test policy.tsv
  copy_output //crates/tracedecay-rusqlite-runtime:unit_test policy-fixture.sql

  # Every tests/outputs dir under the census packages, then the sqlite_replay
  # column, which the default-engine parity test computes.
  dirs=$("$bazel" query "tests(set(${census_targets[*]}))" 2>/dev/null | while read -r target; do outputs "$target"; done)
  {
    printf 'capability\tsql\tsqlite_replay\n'
    for dir in $dirs; do cat "$dir"/sql-census-*.tsv 2>/dev/null || true; done |
      LC_ALL=C sort -u | sed 's/$/\tmissing/'
  } >"$testdata/census.tsv"
  "$bazel" test --nozip_undeclared_test_outputs "$@" "$parity" --test_filter=census_sqlite_replay_column_is_current >/dev/null 2>&1 || true
  copy_output "$parity" census.tsv
  "$bazel" test --nozip_undeclared_test_outputs "$@" "$parity"
  ;;
gaps)
  "$bazel" test --config=native --nozip_undeclared_test_outputs "$@" "$parity" \
    --test_filter=every_census_statement_runs_under_its_capability >/dev/null 2>&1 || true
  copy_output "$parity" known_native_gaps.tsv
  "$bazel" test --config=native "$@" "$parity"
  ;;
*)
  echo "usage: $0 [census|gaps] [bazel test flags...]" >&2
  exit 2
  ;;
esac
wc -l "$testdata"/*.tsv
