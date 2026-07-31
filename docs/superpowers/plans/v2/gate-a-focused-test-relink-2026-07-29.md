# Gate A focused-test relink receipt — 2026-07-29 (addendum)

> **Archived provenance — not current requirements.** This document preserves
> historical measurements and results verbatim where useful. Current scope and
> acceptance come only from
> [`00-plan-set-index.md`](../../../plans/tracedecay-v2/00-plan-set-index.md),
> [`NEXT.md`](../../../plans/tracedecay-v2/NEXT.md), and the applicable numbered
> V2 plan. Do not recreate its branch/worktree/SHA protocol, Gate A/B,
> timing/JUnit receipts, exact test names/counts, generated-byte/source-shape
> checks, PR closure gates, or platform gate lattice.

Addendum to `docs/superpowers/plans/v2/gate-a-measurements-2026-07-29.md`,
closing verdict 4 ("Focused tests do not relink the full root" — FAIL at
61.33s) for the query crate after relocating the query-focused suites into
`crates/tracedecay-query`.

## Relocation performed

- `tests/search_quality_suite/{candidate_producers,single_root}.rs` moved to
  `crates/tracedecay-query/tests/search_quality_suite/` (12 tests: exact,
  lexical, graph, fusion, hydration, pagination). Imports rewritten from
  `tracedecay::query::*` / `tracedecay::code_index::*` to `tracedecay_query::*`
  / `tracedecay_code_index::*`.
- The PR9/PR10 workload-fixture test stayed in root
  (`tests/search_quality_suite/workload_fixture.rs`) because it exercises the
  root-owned `search_eval` module and root-checked-in fixtures.
- `crates/tracedecay-query/Cargo.toml` dev-dependencies gained
  `tracedecay-code-index = { path = "../tracedecay-code-index", features = ["lite"] }`
  so the moved suites keep the bundled Rust grammar in test builds only;
  production consumers keep `default-features = false`.
- `tests/session_suite/temporal_application.rs` intentionally NOT moved: it
  tests the root-inline `tracedecay::application::session` retrieval service
  (application layer depends on query, not vice versa). Its focused ownership
  is the application crate — outside this slice's ownership.

## Environment

- Host: `ubuntu-main` (Linux 6.8.0-136-generic, x86_64)
- Toolchain: rustc 1.97.1, cargo 1.97.1, cargo-nextest, sccache wrapper,
  `-C linker=clang -C link-arg=-fuse-ld=mold`
- Worktree: `/fast/projects/tracedecay/.worktrees/v2-root-breakup`,
  branch `codex/v2-root-breakup`
- Feature set: `--all-features` on every command

## Contention protocol

Same as the parent receipt: wait for zero `cargo`/`cargo-nextest` processes
before every measured run; discard runs overlapped by a peer process whose
elapsed time is less than the measured wall time.

## Receipts

Correctness (run under peer cargo contention per the 40-minute rule;
contention noted, results are contention-insensitive):

- `cargo check -p tracedecay-query --lib --all-features` — PASS (1m 02s wall,
  finished 18:24:41 UTC).
- `cargo test -p tracedecay-query --all-features` — PASS: 308 lib tests,
  12 relocated `search_quality_suite` tests (7 candidate_producers +
  5 single_root), 1 `semantic_search_suite` test; 0 failed
  (finished 18:26:45 UTC).

Gate A criterion 4 re-measurement (query crate, after relocation):

1. Warm builds: `cargo test -p tracedecay-query --all-features --no-run`
   twice (18:31:22 UTC); second run a pure no-op.
2. Touch: added a one-line comment to `crates/tracedecay-query/src/lib.rs`.
3. Measured rebuild: `/usr/bin/time -p cargo test -p tracedecay-query
   --all-features --no-run -v` (18:32:17–18:32:20 UTC):
   - **real 2.14s** (user 2.76s, sys 1.32s).
   - `Dirty`/`Compiling` units: `tracedecay-query` only.
   - `Fresh`: tracedecay-tool-catalog, tracedecay-domain, tracedecay-policy,
     tracedecay-application, tracedecay-code-index.
   - Root `tracedecay` lib and root lib-test binary: **not compiled, not
     fresh-checked** — absent from the entire 141-unit build plan log.
   - Contention note: `peer_cargo=2` at both start and end of the 2.14s
     window (peer jobs predated and outlived the run). Verdict rests on the
     structural absence of the root unit, not on the wall time.
4. Touch reverted by inverse edit; `git status` on
   `crates/tracedecay-query/src/` clean.

## Historical verdict vs Gate A criterion 4

PASS for the query crate. A one-line edit inside `crates/tracedecay-query`
rebuilds only `tracedecay-query` (2.14s wall vs 61.33s FAIL before
relocation); the root lib and root lib-test binary do not participate in the
build at all. Focused query tests (`cargo test -p tracedecay-query`) no
longer link the full root. The remaining root-side suite
(`tests/search_quality_suite/workload_fixture.rs`) is root-owned by design
(search_eval fixture contract) and does not affect query-crate iteration.
