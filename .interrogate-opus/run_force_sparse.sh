#!/bin/bash
# Applies the local-only patch, runs both repro tests, restores the tracked files.
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1
git apply .interrogate-opus/force_sparse.patch || exit 1
for f in repro_force_sparse_all_languages repro_force_sparse_real_commit_shapes; do
  bazel test //crates/tracedecay-code-index:unit_test --experimental_disk_cache_gc_max_size=50G --jobs=6 \
    --test_filter=$f --test_arg=--nocapture --test_output=all --test_timeout=900 2>&1 \
    | grep -E 'REPRO|first (missing|extra)|test result'
done
git checkout -- crates/tracedecay-code-index/src/production/sparse_differential_tests.rs \
  crates/tracedecay-code-index/src/production/sparse_increment.rs \
  crates/tracedecay-code-index/src/production/sparse_increment_tests.rs
git status --short
