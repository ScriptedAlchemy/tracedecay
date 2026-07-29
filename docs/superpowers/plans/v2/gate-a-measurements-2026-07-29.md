# Gate A leaf timing receipts — 2026-07-29

Scope: Gate A of `docs/superpowers/plans/2026-07-28-v2-delivery-root-crate-breakup.md`
(query and code-index extraction). Measurement validity rules from
`docs/plans/tracedecay-v2/33-end-to-end-performance-optimization.md`.

## Environment

- Host: `ubuntu-main` (Linux 6.8.0-136-generic, x86_64)
- Toolchain: rustc 1.97.1 (8bab26f4f 2026-07-14), cargo 1.97.1, cargo-nextest
  (home-dir install), sccache wrapper active, `-C linker=clang -C link-arg=-fuse-ld=mold`
- Worktree: `/fast/projects/tracedecay/.worktrees/v2-root-breakup`,
  branch `codex/v2-root-breakup`, HEAD `0a9a8c552` (plus peer agent's
  uncommitted working-tree edits; ordinary repo-local `target/`)
- Feature set: `--all-features` on every command
- Baseline anchor: **no recorded Phase 0 baseline receipts exist** (no pinned
  `BASE_SHA` value in docs, Phase 0 checkboxes unchecked, no baseline commit on
  `origin/codex/tracedecay-total-redesign-plan -- docs/`). The only anchor is
  the plan-stated pre-extraction root all-feature leaf reference of **121.35s**.
  All numbers below are therefore "interim, no recorded baseline" absolutes.

## Contention protocol

A peer agent builds nearly continuously in this same worktree/target dir
(observed 7–14 concurrent cargo processes, load average 13–27). Before every
measured run we waited for zero `cargo`/`cargo-nextest`/`rustc` processes
(10–20s poll, double-checked after 15s). After every measured run we checked
for any peer process whose elapsed time was less than the measured wall time
(overlap = contaminated). Contaminated runs were discarded and retaken:

- Discarded: query leaf 127.18s (peer build overlapped), root leaf 25.46s
  (peer cargo started ~9s into the 26s run), one root probe aborted before the
  touch, one query measured run aborted before the touch.
- One earlier root attempt (28.42s) passed process-level checks but ran while
  the peer was alternating default-feature vs all-feature checks in the same
  target dir; feature-set flip-flop reruns the root build script and polluted
  warm2 (24.86s instead of a no-op). Discarded; retaken below.

## Receipts

Warmth legend: warm1 = first run after quiet window (may rebuild
peer-invalidated deps), warm2 = no-op confirmation, measured = after a
one-line trailing-comment touch edit (reverted by exact inverse edit — marker
line appended with `>>`, removed by exact-match `sed`; never `git checkout`).

| # | Command | Edit target | warm1 | warm2 | Measured wall | Rebuilt units in measured run | Contention |
|---|---------|-------------|-------|-------|---------------|-------------------------------|------------|
| 1 | `cargo check -p tracedecay-query --lib --all-features` | `crates/tracedecay-query/src/lib.rs` | 0.81s (1 unit) | 0.21s (0) | **0.80s** | 1 (`tracedecay-query`) | clean |
| 2 | `cargo check -p tracedecay-code-index --lib --all-features` | `crates/tracedecay-code-index/src/capabilities.rs` | 30.11s (4: domain, policy, application, code-index) | 0.23s (0) | **1.38s** | 1 (`tracedecay-code-index`; no build-script rerun) | clean |
| 3 | `cargo check -p tracedecay-domain --lib --all-features` | `crates/tracedecay-domain/src/diagnostics.rs` | 5.61s (1 unit) | 0.19s (0) | **5.29s** | 1 (`tracedecay-domain`) | clean |
| 4 | `cargo check -p tracedecay --lib --all-features` | `src/os_str_bytes.rs` (std::ffi-only helper; no dependency on moved query/code-index/domain modules) | 0.46s (0) | 0.45s (0) | **25.76s** | 1 (`tracedecay` lib; full log shows lib check emitting its 289 warnings; zero leaf-crate rebuilds) | clean |
| 5 | `cargo nextest run --lib --all-features -E 'test(mcp::scope)'` | `crates/tracedecay-query/src/lib.rs` | 63.06s (2 units) | 1.11s (0) | **61.33s** | 2 (`tracedecay-query` rlib, then `tracedecay` lib-test binary recompile + relink); 9 tests run, 9 passed, 4486 skipped | clean |
| 6 | `cargo check -p tracedecay --lib --all-features -v` | `src/yaml_scalar.rs` (unrelated root edit) | 0.52s (0) | — | **27.16s** | 1 (`tracedecay` lib); **0 build-script executions** (no `Running .../build-script` lines), 0 WGSL/grammar/tree-sitter recompiles (the 47 `wgsl|tree-sitter|grammar` grep hits are `-L native=` paths in the rustc command line, not activity) | clean |

Receipt 6 corroborates receipt 4 (root warm leaf recheck 25.6–27.2s across two
different unrelated root files).

## Verdicts vs Gate A criteria

1. **Query private leaf edit improves ≥20% or ≥8s** — PASS. 0.80s vs the
   121.35s pre-extraction root-inline reference (>99% / 120.5s better), on an
   identical warm same-host `cargo check --lib --all-features` shape. Caveat:
   comparison is against the plan-stated reference, not a recorded receipt
   (no Phase 0 baseline was published); absolute numbers supplied.
2. **Code-index private leaf edit improves ≥20% or ≥8s** — PASS. 1.38s vs
   121.35s reference, same caveat.
3. **Root no longer compiles moved sources inline** — PASS (structural
   evidence). Root `src/` has no `extraction/`, `retrieval/`, `temporal/`,
   `query/`, or `code_index/` modules; root `Cargo.toml` depends on the path
   crates `tracedecay-query`, `tracedecay-code-index`, `tracedecay-domain`
   (root features forward `lang-*`/`lite` to `tracedecay-code-index`). Root
   leaf-edit runs (receipts 4, 6) rebuild exactly one unit — the `tracedecay`
   lib — with no leaf crates rechecked. The only same-named files
   (`src/path_scope.rs`, `src/types.rs`) are a 23-line helper copy and the
   sanctioned compatibility façade re-exporting from the extracted crates.
4. **Focused tests do not relink the full root** — FAIL for a query-leaf edit.
   Touching only `crates/tracedecay-query/src/lib.rs` forces
   `tracedecay-query` recompile and a full root lib-test binary recompile +
   relink (61.33s, 2 units). This is direct dependency propagation (root lib
   depends on `tracedecay-query`), not an extraction defect — but as literally
   stated the criterion is not met for in-closure leaf edits. No extracted
   leaf exists outside the root test binary's dependency closure to probe the
   out-of-closure case.
5. **WGSL/grammar build ownership does not rerun for unrelated root edits** —
   PASS. WGSL C compilation now lives in `crates/tracedecay-code-index/build.rs`
   (gated on `CARGO_FEATURE_LANG_WGSL`, `rerun-if-changed` only on
   `vendor/tree-sitter-wgsl/src/*`); root `build.rs` watches only plugin/,
   dashboard app-dist, logo, and version stamps. Verbose probe (receipt 6):
   zero build-script executions, zero grammar recompiles on an unrelated root
   edit. Caveat: mixed-feature concurrent use of one target dir (peer running
   default-feature checks alongside all-feature checks) flip-flops the root
   build-script fingerprint and causes reruns unrelated to any edit — a
   shared-target artifact, not a file-ownership regression.

## Peer interactions

- One peer agent was actively editing and building in this worktree the entire
  session (root, application, api, store files; `cargo check`/`cargo test`
  runs in alternating feature sets). No files were killed, staged, or
  reverted on their side; all measurement touches were reverted by exact
  inverse edits (`git status` clean for every touched path at handoff).
- Peer edits between sequences changed warm1 rebuilt-unit counts (noted per
  receipt); measured runs were only accepted with zero process overlap and a
  stable worktree.

## SCOPE DEVIATION

None. Read-only plus the reverted one-line touch edits and this receipt file.
