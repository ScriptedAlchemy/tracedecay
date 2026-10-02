# Branch triage: 2026-10-02

Report only. No branch was deleted or modified.

Base: `origin/master` at `06edb2fec5` (2026-10-01).

## Verdicts

| Branch | Ahead / behind master | Verdict |
| --- | --- | --- |
| `codex/ci-green-warm` | 2 / 268 | Superseded (#2451, #2488) |
| `codex/retire-warm-ci-experiments` | 2 / 213 | Superseded by a different design (#2546) |
| `simplify/refresh-and-review-cuts` | 2 / 913 | Superseded (#1927) |
| `wip/refresh-cuts-uncommitted-2026-09-21` | 1 / 1135 | Superseded (#1927, #2748) |
| `merge/master-into-refresh-cuts` | 0 / 1135 | Superseded (tip is a master ancestor) |
| `fleet/red-tests-2317` | 1 / 311 | Superseded (#2404) |
| `agent/1361-reconcile-59f516` | 6 / 2966 | Superseded by a different design (#1424, #1814, #1954) |
| `agent/1103-lifecycle-stall` | 10 / 2982 | Superseded (#1377, #1458), except one WIP idea |
| `agent/dashboard-graph-grant-fe49` | 2 / 2994 | Superseded (#1347) |
| `agent/1280-tip-receipt` | n/a | Branch does not exist on `origin` |

"Superseded" means master contains the branch's change, or master replaced it with
a later design that makes the branch obsolete. No branch has work that master
needs as-is. The two items worth a look are listed under
[Unlanded items](#unlanded-items).

## Method

For each branch `B`, with `MB = git merge-base origin/master B`:

- `git cherry origin/master B`: `-` marks a commit whose patch is already on master.
- Line coverage: for each file in `git diff MB B`, the share of the branch's added
  lines (12 or more characters, stripped) that appear in the master file, and the
  share of removed lines that no longer appear there. Reported below as
  "added A/N, removed R/M".
- Provenance: `git log -S <added line> MB..origin/master -- <file>` for a sample of
  added lines that master has. This gives the master commit that introduced them.
- Same-subject lookup on master, `git range-diff` where a branch commit has a
  rewritten twin on master, and the PR/issue state from the GitHub API.

## Per-branch evidence

### `codex/ci-green-warm`

- `d88002cbec test(ci): align fixtures with current runtime contracts` landed as
  `19af307653` (#2451, head `codex/ci-fixture-contracts`). 12 of the sampled added
  lines trace to it. `extract_alloc.rs` is byte-identical to master.
- `5dd81b21bb chore: preserve stranded ubuntu-main worktree changes` (CI `run-name`
  plus cancel-closed-PR-runs in `pr-run-cleanup.yml`) landed as `fe6caf1bed`
  (#2488, head `codex/pr-ci-lifecycle`). Master refines the rerun filter to
  `.run_attempt == 1 or (... .run_started_at < $closed)`. The branch's older
  filter and its comment are the only missing lines.
- Line coverage: added 55/58, removed 9/10.

### `codex/retire-warm-ci-experiments`

- No PR was opened from this branch.
- The branch deletes `ci-warm.yml`, `ci-folding.yml`, and
  `evals/ci-folding/{action.yml,automatic.py,test_automatic.py,warm.py}`. Master
  deleted the same files in `8176213587` (#2546, `codex/hauler-ci-takeover`). The
  deletions match master exactly.
- The branch adds `5eab5651e2 ci: run Linux partitions in two warm PR workers`.
  That commit regroups `.github/linux-test-partitions.json` into
  `sessions-runtime-contracts` / `transport-journeys-storage` and bumps
  `cargo-hauler` to 0.10.0. Master took a different path: PR tests run through
  the Hauler pool (`.github/hauler-ci.json`), and the master groups are still
  `core-contracts`, `root-lib-sessions`, `root-transport`, and so on. The
  two-worker matrix is obsolete.
- Line coverage: added 5/41, removed 101/144. The coverage is low because master
  chose another design, not because work is missing.
- See [Unlanded items](#unlanded-items) for a stale doc paragraph.

### `simplify/refresh-and-review-cuts`

- No PR was opened from this branch. Both commits landed through #1927
  (`fix/land-refresh-cuts`, merged 2026-09-21) with the same author timestamp
  (2026-09-20 18:16:20):
  - `97d0e38ace` maps to `0c46b24cd2 fix(sessions): keep one stalled source from blocking history`
  - `9457ad6668` maps to `44b479deb1 refactor: delete unused wrappers found in review`
- `git range-diff` marks both as rewritten. The master side is the larger one:
  72 master-only lines against 40 branch-only lines. The extra master lines
  include the `admitted_cursor_covers` ledger-row cursor restore,
  `observation_projection/rebuild.rs`, and `agents/prompt_rules.rs`. The branch
  is the earlier revision.
- Line coverage: added 420/559, removed 171/236. The remainder is code that
  master has rewritten since then.

### `wip/refresh-cuts-uncommitted-2026-09-21`

- `c6aec4b259 chore: park uncommitted refresh-cuts edits from the shared checkout`.
  No PR.
- The capture-wrapper cuts landed in `692e08ba90 refactor(capture): delete wrappers
  the codex and cursor paths forward` (#1927). 21 sampled lines trace to it,
  including the `opencode.rs` `created_at` dedupe.
- Every function the branch deletes is gone from master, or exists only in its
  consolidated form: `search_tree_scoped`, `bubble_epoch`,
  `composer_todos_have_admittable_items`,
  `normalize_cursor_composer_observation_with_projected_message_id`, the domain
  `zero_digest`, and the duplicate code-index `placeholder_digest`. Master keeps
  one shared `generations::placeholder_digest`.
- `sessions/runtime/hosts/codex/records.rs`, which the branch edits, was deleted
  on master by #2748.
- Line coverage: added 26/40, removed 67/96. The residue is import-list spelling,
  for example `crate::generations::placeholder_digest()` on the branch versus
  `use super::generations::placeholder_digest` on master.

### `merge/master-into-refresh-cuts`

- `git merge-base --is-ancestor origin/merge/master-into-refresh-cuts origin/master`
  succeeds. The tip `4ff8c2a569` is 0 ahead of master, so it is fully contained.

### `fleet/red-tests-2317`

- PR #2404 was merged 2026-09-28 as `1fe441558f`. `git cherry` reports the
  commit as patch-equivalent (`-`). Issue #2275 is closed.
- Line coverage: added 30/30. The 5 "removed" lines that are still on master are
  generic lines that also occur elsewhere in the same files, for example
  `return Err(CandidateOutputError::Contract(`.

### `agent/1361-reconcile-59f516`

- This is a reconcile of the #1355 builder-caller work: `1194a08346`,
  `7043f9577a`, and the WIP snapshots `59f5165102` and `1c1936c48d`. None of
  them is on master. `git cherry` reports all four as `+`.
- PRs #1361 (`agent/1355-false-complete-callers`) and #1371
  (`agent/1355-callers-typed-locals`) were closed unmerged on 2026-09-16.
- Issue #1355 was closed on 2026-09-22 by #1954 (`fix(graph): disclose incomplete
  Rust method caller coverage`). Master resolved the problem in these PRs:
  - #1424: `aa0a5c5ff5`, typed-binding `Type::method` naming, tested by
    `test_rust_dotted_calls_on_typed_bindings_also_name_the_method_by_type`
  - #1814: stop bare receivers inventing callers
  - #1954: report partial coverage instead of a false complete
- The branch's `LocalTypeScope` and its three tests do not exist on master
  (`rg` finds 0 hits): `cross_file_builder_method_calls_bind_through_typed_locals`,
  `mut_builder_local_emits_qualified_method_ref`, and
  `callers_of_cross_file_builder_method_include_typed_local_sites`.
- Line coverage: added 157/549, removed 18/56. The branch's lexical-scope design
  was not adopted.

### `agent/1103-lifecycle-stall`

- PR #1339 was closed unmerged. `git cherry` reports 9 of the 10 commits as
  patch-equivalent (`-`). They landed through #1377 (`agent/1103-graph-seat`) and
  #1458 (`8c78bddae6`, read lock), with matching subjects (`ef6fb4422f`,
  `dcc942b89b`, `b6aafa5b06`, `292a5ef7f5`, `d384ce5fd3`, and others).
- Master has since replaced that machinery:
  - `drain_clone_backfill` was removed by `a7b08f2032 perf(code-index)!: shrink
    index artifacts, share across worktrees`.
  - `acquire_code_generation_store_read_lock` was removed by `60fce0d848` (#2823).
- Issue #1103 was closed on 2026-09-25.
- The only commit not on master is `90a6f877f8 chore(campaign): wip snapshot`.
  See [Unlanded items](#unlanded-items).

### `agent/dashboard-graph-grant-fe49`

- Both commits landed as `8884781906 fix(dashboard): authorize embedded graph
  search and neighbors` (#1347, head `agent/dashboard-graph-grant`, merged
  2026-09-16). 14 sampled lines trace to it.
- Line coverage: added 71/73, removed 15/16. The two missing lines take
  `GLOBAL_DB_ENV_LOCK` in a test. That lock no longer exists anywhere on master,
  because tests now pass an explicit `ProfileRoot` (#2267).

### `agent/1280-tip-receipt`

- `GET repos/ScriptedAlchemy/tracedecay/branches/agent/1280-tip-receipt`
  returns 404. `git ls-remote` does not list it, and a PR search for
  `head:agent/1280-tip-receipt` returns 0 results. Its content cannot be
  evaluated.
- Related state: issue #1280 was closed on 2026-09-17. The tip-receipt checker
  doc landed as `d28db3ad17 docs(clones): document the envelope tip receipt
  checker` through #1441. Other `agent/1280-*` branches still exist on `origin`
  (`envelope`, `envelope-land`, `increment-cost`, `p1-cost`,
  `parent-delta{,-s2..s5}`, and `sealed-rev12`). They are outside the scope of
  this report.

## Unlanded items

These are the only branch contents with no counterpart on master. Neither one
blocks deleting its branch.

1. `agent/1103-lifecycle-stall` `90a6f877f8` proposes chunker revision
   `chunker.daemon.v5`. When a one-line symbol's body span equals its signature
   span, it skips the `SymbolSignature` chunk, which avoids two identical rows.
   Master is still at `chunker.daemon.v4` and pushes the signature
   unconditionally (`crates/tracedecay-code-index/src/chunks.rs`, the
   `if let Some(signature) = emission.signature` block). If this is wanted, it
   needs a fresh change on current master. The snapshot predates the
   `a7b08f2032` artifact rewrite and does not apply.
2. `codex/retire-warm-ci-experiments` rewrites the intro paragraph of
   `docs/ci-folding-2026-09-28.md`. Master still says that "the current workflow
   accepts two explicitly trusted PR heads" (line 331). Those workflows were
   deleted in #2546, so the master sentence is stale. `evals/ci-folding/README.md`
   on master is already correct.
