# Historical partitioned generation

These bytes were emitted by the unmodified production writer at commit
`e95f373f883e9cb68e4f6d1cb786efc8a17bf5de`. A temporary test in that
commit's isolated archival worktree invoked its existing
`partitioned_codec_fixture`, then exported `encode_sealed()` as the expected
canonical generation. The fixture includes a successor generation, lineage,
edges, and an unchanged file segment reused from its parent.

`provenance.json` records the writer and temporary exporter source hashes,
manifest and canonical generation hashes, identities, and all emitted segment
hashes. Only segments referenced by this successor manifest are included here;
the archival export retains the unreferenced parent segments. No current
manifest was modified to impersonate the historical format.

Reproduce in a fresh owned worktree at the exact writer commit, using the
current checkout's maintained worktree script. Save the absolute path to this
fixture directory before changing directories:

```sh
fixture_dir="$PWD/crates/tracedecay-code-index/tests/fixtures/partitioned_pre_paging"
scripts/agent-worktree.sh /fast/tmp/tracedecay-historical-writer-reproduction \
  -b codex/historical-writer-reproduction e95f373f883e9cb68e4f6d1cb786efc8a17bf5de
cd /fast/tmp/tracedecay-historical-writer-reproduction
git apply "$fixture_dir/exporter.patch"
sha256sum crates/tracedecay-code-index/src/production/partitioned_codec.rs
cargo test -p tracedecay-code-index --test code_index_suite \
  production_orchestration::export_historical_partitioned_writer_fixture \
  -- --exact --nocapture
```

Use the canonical Cargo broker and an absent worktree path/branch. The unchanged
production writer must hash to
`5a344630c7ced0ce67035018fc688ea2766742edaa1977cd0784176d9811e0f3`.
The exporter creates `fixture-export` in the historical worktree and refuses to
overwrite an existing export. Compare its manifest, expected generation,
provenance, and referenced segments with this fixture. The exact exporter patch
also reproduces `export_test_sha256` in the provenance. It is archival tooling,
not part of the current production writer or current test suite.
