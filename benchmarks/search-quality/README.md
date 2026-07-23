# PR9 search-quality direct verification

Direct Rust contracts cover deterministic extraction and generations,
projection replay after restart, exact non-demotion, lexical ranking,
graph/Git/diagnostic/test joins, coverage and abstention, and V1 import
parity against the real sanitized search-quality corpus. There is no static
acceptance snapshot, packet, gate manifest, owner receipt, or promotion
authority in this directory; the legacy locked-acceptance scaffolding was
removed when the delivery model shifted to direct contract execution.

Developer quality evaluation is Linux-only. Normal Linux/macOS/Windows CI owns
default-feature product support.

Run the direct contracts through Cargo, for example:

```sh
cargo nextest run --all-features --no-fail-fast -E 'binary(search_quality_suite)'
```

## Direct-evaluation record

`direct-evaluation-record-v1.json` is a plain, honest snapshot of the direct
offline evaluation flow over the checked-in fixtures. It records what ran, the
provenance digests of the fixtures and candidate workload (as evidence, never
as promotion gates), and a per-step status (`pass` / `recorded` / `pending`).
It is not a receipt, gate, packet, attestation, or promotion state machine —
those were deliberately removed. When a metric has no declared threshold it is
reported with status `recorded` instead of being judged against an invented
bar.

The record is produced with ordinary commands, no network and no model runtime
(the semantic path returns the byte-identical PR9 fallback while offline):

```sh
cargo run --bin tracedecay-search-eval -- validate
cargo run --bin tracedecay-search-eval -- generate-candidates --profiles pr9-fallback
cargo run --bin tracedecay-search-eval -- compare --require-outcome accepted
```

The `compare` step ends in `blocked`/`pending` because the checked-in fixtures
are `contract_only` authority: authoritative direct holdout labels are
intentionally never committed. To reach an `Accepted` or `Rejected` quality
outcome, the owner runs `compare` against a locked-quality run, supplying the
direct holdout labels and frozen saved candidates from local paths:

```sh
cargo run --bin tracedecay-search-eval -- compare \
  --run-manifest <locked-quality-run.json> \
  --holdout-labels <direct/holdout/labels.json> \
  --saved-candidates <frozen/pr9-saved-candidates.json> \
  --require-outcome accepted
```

The end-to-end flow itself is validated by the `search_quality_suite` and
`search_eval_cli_test` Rust tests; those tests are the quality gate, not any
separate checker over the record file.

### PR10 semantic/vector section

The same `direct-evaluation-record-v1.json` also carries a
`pr10_semantic_vector_evaluation` section for the PR10 semantic/vector path:
vector generation, the FastEmbed local-artifact runtime, exact-flat/lexical/
graph fusion and calibration, and the PR9 semantic-abstention fallback. Because
those semantic contracts, determinism, and fail-closed behavior are asserted by
the Rust test suites rather than the CLI, that section records the executed
suites (by feature/surface and date, never by commit hash) instead of a
CLI transcript. It is produced offline with no network and no downloaded model
runtime (embeddings are the deterministic in-process fake; the real FastEmbed
constructor is asserted present by source inspection, not invoked):

```sh
cargo test --all-features --no-fail-fast \
  --test semantic_search_suite \
  --test pr10_vector_generation_prep_test \
  --test pr10_artifact_runtime_prep_test \
  --test search_quality_suite \
  --test search_eval_cli_test \
  --test search_eval_holdout_authority_test
```

Use `cargo test --test <name>` (not `cargo nextest -E 'binary(...)'`) so only
the non-test lib is linked: these integration binaries are unaffected by the
lib unit-test target, which can be independently uncompilable during unrelated
in-flight work. All 153 tests across the 6 binaries pass. Real-model
latency/resource benchmarks and any accept/reject quality outcome remain
pending on a locked-quality run, exactly as for the PR9 flow.
