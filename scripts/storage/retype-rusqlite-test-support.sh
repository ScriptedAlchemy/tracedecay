#!/bin/sh
# Retypes the runtime test executors from the rusqlite crate to tracedecay-sql,
# whose types are rusqlite's on the SQLite engine and the native driver's under
# --config=native.
set -eu
cd "$(git rev-parse --show-toplevel)"
for file in \
  crates/tracedecay-rusqlite-runtime/tests/rusqlite_suite/storage_contention.rs \
  crates/tracedecay-rusqlite-runtime/tests/rusqlite_suite/runtime_actor/support.rs \
  tests/storage_runtime_rusqlite_suite/runtime_test_support.rs
do
  sed -i -e 's/\brusqlite::/tracedecay_sql::/g' -e 's/^use rusqlite::{/use tracedecay_sql::{/' "$file"
done
