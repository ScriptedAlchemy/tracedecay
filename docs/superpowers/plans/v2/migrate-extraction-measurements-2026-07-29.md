# `tracedecay-migrate` extraction receipts — 2026-07-29

Scope: the migration near-leaf slice of
[Plan 12](../../../plans/tracedecay-v2/12-root-compatibility-migration.md)
("The final root package owns composition, daemon lifecycle, discovery, upgrade
handoff, and stable compatibility entry points. It is not a catch-all product
implementation or a permanent migration runtime."). Measurement validity rules
from [Plan 33](../../../plans/tracedecay-v2/33-end-to-end-performance-optimization.md);
receipt shape follows [`gate-a-measurements-2026-07-29.md`](gate-a-measurements-2026-07-29.md).

No dedicated child plan exists for this slice. It applies Plan 12's leaves-first
dependency authority; it does not create a migration orchestration framework,
which Plan 12's PR19 defaults explicitly forbid.

## What moved, and what deliberately did not

`tracedecay-migrate` (1,354 lines / 4 files) owns the migration decisions and
records that need no store authority:

- `durability` — store durability classification (`Derived`/`Durable`/
  `Recoverable`, whole-store drop eligibility, session-table classification),
  moved verbatim.
- `inventory` — the preflight scan's record vocabulary, moved verbatim.
- `manifest` — the manifest schema, the derived checkpoint protocol, the
  forward-only artifact state ladder, checkpoint save/load, and store-artifact
  path safety.

Root keeps every authority named in the slice brief: the lifecycle lease,
maintenance database scopes, `DatabaseAuthority`, the `sqlite_read_snapshot`
and `tracedecay_rusqlite_runtime` snapshot paths, registry reconstruction,
enrollment markers, the inventory scanners, `consolidate/**`, `hermes/**`,
`registry.rs`, `memory_cutover.rs`, and apply/export/cleanup/verify/finalize.
`src/migrate/**` remains 31,365 lines.

### Narrow port instead of widened visibility

Writing the checkpoint needs owner-private file semantics the package must not
choose for itself, so `manifest` declares a `CheckpointWriter` port with
`write_file` and `write_file_atomically`; root implements it over
`PrivateStoreIo` and keeps `save_manifest(&MigrationManifest)` as the façade.

Exactly one previously private helper became public (`validate_migration_id`,
needed by root's planner). `validate_protocol_paths`,
`ArtifactState::can_transition_to`, `validate_artifact_relpath`, and
`reject_symlink_components` stayed private inside the package. Every fs
durability primitive entangled with `DatabaseAuthority::replace_file_atomically`
stayed in root rather than becoming a public API — Plan 12 requires narrow
ports, not wholesale visibility widening.

## Environment

- Host: Linux 6.8.0-136-generic x86_64
- Toolchain: rustc 1.97.1 (8bab26f4f 2026-07-14), cargo 1.97.1, cargo-nextest,
  sccache wrapper active
- Feature set: `--all-features` on every command
- Commit measured: `4a2834bb1`
- Worktree: `/fast/projects/tracedecay/.worktrees/v2-migrate-verify` (detached
  at `4a2834bb1`) with a private `CARGO_TARGET_DIR`, created because the shared
  `v2-root-breakup` checkout carried concurrent peer edits (see Contention).
- Baseline anchor: still **no recorded Phase 0 baseline receipts** — same
  limitation the Gate A receipt records. The plan-stated pre-extraction root
  all-feature leaf reference is 121.35 s.

## Contention protocol

Peer agents built continuously in the shared checkout throughout (load average
23–28, 12–15 concurrent cargo/rustc processes). They were never killed. The
measurement worktree used its own target directory, so no peer cache was shared
or invalidated; the host CPU was still shared, so **every absolute number below
is an upper bound**. Both sides of the comparison were inflated by the same
load, so the relative improvement is conservative.

Warmth legend matches the Gate A receipt: warm1 = first run in the fresh target
dir, warm2 = no-op confirmation, measured = after appending one trailing comment
line, reverted by exact inverse edit (never `git checkout`).

## Receipts

| # | Command | Edit target | warm1 | warm2 | Measured wall |
|---|---------|-------------|-------|-------|---------------|
| 1 | `cargo check -p tracedecay-migrate --lib --all-features` | `crates/tracedecay-migrate/src/durability.rs` | 34.15s | 0.28s (no-op) | **0.61s** |
| 2 | `cargo check -p tracedecay --lib --all-features` | `src/os_str_bytes.rs` (unrelated root helper) | 196.58s | 0.59s (no-op) | **28.13s** |

Receipt 2 is the same-host, same-target-dir, same-session root leaf control. It
agrees with the Gate A root leaf figures (25.76s / 27.16s) taken in the other
worktree, which supports treating the pair as comparable.

### Verdict against the Gate A leaf criterion (≥20% or ≥8s)

**PASS.** A migration planning/checkpoint edit rechecks in 0.61s instead of
participating in a 28.13s root leaf recheck — 97.8% / 27.5s better than the
measured control, and 99.5% / 120.7s better than the plan-stated 121.35s
reference. Caveat: compared against a control and a plan-stated reference, not
a recorded Phase 0 baseline, because none exists.

Both touch edits reverted exactly; `git status` was clean for both paths
afterwards.

## Direct tests

Run in the isolated worktree at `4a2834bb1`, `--all-features`:

- `cargo test -p tracedecay-migrate --all-targets` — **23 run, 23 passed.**
  Package tests cover the forward-only ladder, its rejected backward and
  skipping transitions, failure from any unpublished state,
  lock-before-atomic-publish checkpoint ordering, a surfaced publish failure,
  and the unconfirmed-token / unsafe-`migration_id` / tampered-protocol-path
  refusals, plus durability classification and artifact path safety.
- `cargo nextest run -p tracedecay --all-features --test storage_suite --test architecture_boundaries -j1 -E 'test(migration_manifest_test) + test(migrate_inventory_test) + test(profile_storage_migration_test) + test(migrate)'`
  — **95 run, 95 passed, 315 skipped.** This exercises the moved code through
  the production root façade, including
  `manifest_atomic_save_roundtrips_and_cleans_protocol_files`,
  `manifest_save_requires_confirmation_token`,
  `manifest_save_generates_token_and_records_protocol_context`,
  `store_artifact_path_rejects_path_traversal`,
  `store_artifact_path_rejects_symlinks`, and
  `migrate_apply_copies_single_store_and_cuts_over_profile_shard`.

### Falsifiability probes

Neither new gate passes vacuously:

- Allowing `Applied -> Verified` in the checkpoint ladder makes
  `checkpoint_ladder_refuses_backward_and_skipping_transitions` fail with
  `Applied -> Verified must be rejected`. Reverted.
- Adding code that names `rusqlite` inside the package makes
  `migrate_package_owns_no_store_or_lifecycle_authority` fail with the exact
  file and token. Reverted. That guard scans code with line comments stripped,
  and asserts in-test that stripping still leaves real code visible while prose
  naming the boundary does not trip it.

### Parallel-execution caveat (pre-existing, not from this slice)

Under nextest's default parallelism, 11 `migrate_inventory_test` cases fail with
`cannot start migration inventory: another lifecycle operation is already
active`. All 24 pass with `-j1`. The cause is those tests contending on the
exclusive profile lifecycle lease, which this slice does not touch — lease
acquisition stayed in `src/migrate/inventory/mod.rs`. Recorded as an existing
test-isolation gap, not a result of the extraction, and not fixed here.

## Blocker observed in the shared checkout (peer-owned, not fixed here)

Commit `62f4afda1` ("refactor(host-integration): extract embedded host
contracts") removed `Component` from the `use std::path::{...}` list in
`src/agents/host_bundle_v2.rs` while leaving two `Component::Normal` uses at
lines 3180 and 3184, so `cargo check -p tracedecay --lib` fails at that commit
with two `E0433` errors. The base commit `5da69d284` imported `Component`
correctly.

That file is peer-owned and had uncommitted peer edits, so it was left untouched
in the shared checkout. The verification worktree applied the one-line import
locally as throwaway scaffolding, which is why receipts above could be taken;
the scaffold was never committed and the worktree was removed afterwards. The
owner of `62f4afda1` still needs to restore that import on the branch.

Concurrent peer churn in the shared checkout also reached 46 root errors at one
point (`src/dashboard/settings_api.rs`, `ApplicationProblemKind`), which is why
no aggregate root gate is claimed here.

## SCOPE DEVIATION

None. Changes are limited to `crates/tracedecay-migrate/**`, `src/migrate/**`,
`tests/architecture_boundaries/compile_isolation.rs`, this receipt, and the
`Cargo.toml`/`Cargo.lock` lines that register the new package. No push, no
end-to-end run, and no aggregate workspace gate was attempted or claimed.
