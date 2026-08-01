# Dogfood Migration-Boundary Recovery Runbook

Operational record of the forward-only migration boundary recovery for
`cargo dogfood` on 2026-08-01 (attempts 7-11). Documents the boundary state
file format, the backup contract, the recovery ladder that was actually
used, and the failure classes fixed along the way. Sourced from
`scripts/dogfood.sh`, `.claude/skills/dogfooding-tracedecay/SKILL.md`, and
the runtime source cited inline. Do not read `~/.tracedecay` directly to
verify this document — every claim below traces to a file in this repo.

## 1. The boundary state file

Path: `$HOME/.tracedecay/dogfood-migration-boundary.state` (overridable via
`TRACEDECAY_DOGFOOD_PROFILE_DIR`). Written and read entirely by
`scripts/dogfood.sh`; nothing else touches it.

Structural rules, enforced by `load_boundary_state()`
(`scripts/dogfood.sh:287-380`):

- Must not be a symlink, must be a regular file, must be mode `0600`.
- Currently written as `format=3` (`record_boundary_outcome()`,
  `scripts/dogfood.sh:430-470`). `format=2` is still accepted for reading
  (no `retained_binary_sha256` line) so a marker left by an older dogfood
  build doesn't hard-fail the next run.
- Eight newline-delimited fields for `format=3`:
  1. `format=3`
  2. `attempt_id=<epoch-pid-random-random>`
  3. `outcome=<state>`
  4. `attempt_boundary=<reached|not-reached>`
  5. `old_binary_policy=<allowed|forbidden>`
  6. `managed_daemon=<state>`
  7. `retained_binary_sha256=<sha256|none>` — checksum of whatever is
     currently at `~/.local/bin/tracedecay` at write time, used to prove a
     later reader that the *retained* binary is the one the marker was
     written for.
  8. `checksum=<sha256 of lines 1-7>` — tamper/corruption check for the
     marker itself.

### outcome values and what each means

`marker_transition_is_valid()` (`scripts/dogfood.sh:255-276`) is the
authoritative table of legal `outcome:boundary:policy:daemon` tuples. In
practice:

| outcome | boundary | old_binary_policy | meaning |
|---|---|---|---|
| `preparing` | `not-reached` | either | a run is staging a new candidate binary; nothing has replaced the installed binary yet. |
| `safe-rollback-complete` | `not-reached` | either | a run failed before crossing the boundary; `scripts/dogfood.sh` restored the previous installed/staged binaries (`cleanup_install()`, `scripts/dogfood.sh:610-681`, the non-`boundary_reached` branch). Safe to just rerun `cargo dogfood`. |
| `post-update-starting` | `reached` | `forbidden` | the new binary is installed and `post-update --strict --mode dogfood-forward-only` is running. If the process dies here, the marker is left in this state and the **next** run must recover forward, never re-run an old binary. |
| `forward-recovery-required` | `reached` | `forbidden` | a run crossed the boundary and then failed; `cleanup_install()` recorded this outcome instead of restoring the old binary, because doing so would let an older binary reopen a store a newer one may have already migrated. `managed_daemon` is `inactivity-pending`, `inactive`, or `inactivity-unproven` depending on whether the daemon could be proven stopped. |
| `validated` | `reached` | `forbidden` | the boundary was crossed and `post-update` succeeded; `committed=1`. This is the terminal success state. |

Once `attempt_boundary=reached`, `old_binary_policy` is always forced to
`forbidden` (`scripts/dogfood.sh:532-535`) regardless of what the marker
said before — the script never trusts a "the old binary is fine to run"
claim after the point of no return.

`marker_retained_binary_trusted` (set in `load_boundary_state()`,
`scripts/dogfood.sh:373-379`) is `1` only when the SHA-256 of the file
currently at `~/.local/bin/tracedecay` matches `retained_binary_sha256`
from the marker. If someone replaced the installed binary out from under a
pending marker, this flag drops to `0` and
`require_inactive_recovery_before_preparing()`
(`scripts/dogfood.sh:494-528`) refuses to execute it, falling back to the
freshly built source binary for recovery instead.

## 2. The backup contract

Two mutually exclusive modes, gated by `TRACEDECAY_DOGFOOD_BACKUP_PLAIN`
(`scripts/dogfood.sh:538-557`):

- **Checksummed backup (default).** `TRACEDECAY_DOGFOOD_BACKUP` must name a
  directory containing `backup-manifest.json`
  (`tracedecay migrate backup-profile --to <dir> --backup-id <id>`, CLI
  definition at `src/cli.rs:1218-1225`). Before crossing the boundary,
  `scripts/dogfood.sh:689-697` rehearses it with
  `tracedecay migrate rehearse-profile-backup --backup <dir> --restore <tmp>`
  (`src/cli.rs:1227-1234`) — a full restore-and-verify into a throwaway
  directory, deleted immediately after. This is the safe, default path but
  re-reads and re-writes the entire profile twice.
- **Plain backup** (`TRACEDECAY_DOGFOOD_BACKUP_PLAIN=1`, owner-authorized
  fast path added 2026-07-31, `scripts/dogfood.sh:538-557`).
  `TRACEDECAY_DOGFOOD_BACKUP` must name a directory holding a plain `cp -a`
  profile copy at `<dir>/profile`, with `<dir>/profile/global.db` present.
  No manifest, no rehearsal (`scripts/dogfood.sh:687-697` skips the
  rehearsal step entirely when this flag is set). The script only checks
  that the copy *looks like* a profile (directory + `global.db` file
  exist) — it does not verify contents. This mode exists because the
  checksummed path's two full profile read/write passes outlasted the
  maintenance window at the profile size hit during this recovery.

Both modes require the backup to already exist; `cargo dogfood` never
creates one implicitly.

## 3. The recovery ladder actually used (attempts 7-11)

In order, this is what got a stuck forward-only boundary back to a clean
`validated` state:

1. **Zero-writer proof.** Before touching anything, confirm no process
   still holds the managed daemon or an authority lease on the live
   stores. `scripts/dogfood.sh`'s own recovery path does this
   automatically via `post-update --mode dogfood-recover-inactive`
   (`require_inactive_recovery_before_preparing()`,
   `scripts/dogfood.sh:494-528`, and the `cleanup_install()` failure branch
   at `scripts/dogfood.sh:620-658`), which runs the retained/staged/source
   binary just far enough to prove the managed service is stopped and
   record `managed_daemon=inactive` (or `inactivity-unproven` if that
   proof itself fails).
2. **Plain backup.** `TRACEDECAY_DOGFOOD_BACKUP_PLAIN=1` with
   `TRACEDECAY_DOGFOOD_BACKUP=<dir>` pointing at a `cp -a` copy already
   taken of `~/.tracedecay` (`<dir>/profile/global.db` present), per
   section 2.
3. **Restore the pre-boundary `global.db`.** The registry — which projects
   are enrolled, their storage locations, graph scopes, artifacts — lives
   inside `global.db` and does not need to be reconstructed from scratch;
   it is *in* the restored file. What actually matters is that enrollment
   is durable on-disk independent of the registry database: every project
   root carries an enrollment marker
   (`read_enrollment_marker`/`write_enrollment_marker`,
   `crates/tracedecay-runtime-core/src/storage.rs:392-410` and callers),
   and the registry is a **derived index** that
   `crates/tracedecay-migrate/src/registry.rs` can rebuild from those
   markers plus each project's `STORE_MANIFEST_FILENAME` manifest
   (`reconstruct_registry_from_store_manifest[_inner]`,
   `crates/tracedecay-migrate/src/registry.rs:1236-1475`,
   `RegistryReconstructionStatus::{Eligible,Blocked,Stale,Retired}` at
   `crates/tracedecay-migrate/src/registry.rs:22-27`). If the restored
   `global.db` predates a project's most recent enrollment, that project
   is not lost — it is `Stale`/`Blocked` in the reconstruction report and
   gets picked back up by the next step.
4. **`tracedecay init` rebuilds the derived project graph.** Re-running
   `tracedecay init` (`Commands::Init`, `src/cli.rs:172-181`) against each
   affected project root re-derives the code graph and project state from
   source plus the on-disk markers; it does not require the registry to
   already agree, because the registry itself is reconstructible (step 3).
   `doctor`'s registry-reconstruction check
   (`src/doctor.rs:1662-1675`, driven by
   `RegistryReconstructionStatus` and
   `diff_registry_reconstruction_report`) is the read-only way to confirm
   the registry now matches on-disk reality before moving on.
5. **Rerun `cargo dogfood`.** Once the daemon is proven inactive, the
   profile is backed up, `global.db` is consistent (restored or
   reconstructed), and `tracedecay init` has rebuilt the derived graph,
   `cargo dogfood` runs the normal flow end to end and should reach
   `outcome=validated`.

## 4. Failure classes fixed en route

- **Unenrolled admission.** Host-admission code paths assume a project has
  an enrollment marker (`src/application/host_admission.rs:1226-1246`
  writes one if missing, mirroring what CLI init / daemon first-touch open
  / enrollment-root repair do in production). When a project's marker is
  missing or unreadable, `resolve_enrolled_layout()`
  (`crates/tracedecay-runtime-core/src/storage.rs:990-1019`) surfaces a
  denial rather than guessing, telling the caller to "open it through the
  daemon so the registry can resolve and repair its identity." The fix
  direction is: never admit a project on a best-effort path-derived guess
  when the marker is absent — go through the daemon-owned repair path
  instead.
- **Missing derived store.** Because the registry is derived
  (`crates/tracedecay-migrate/src/registry.rs`, section 3 above), a
  restored or stale `global.db` that is missing a store's registration is
  a reconstruction-eligible condition, not data loss — `tracedecay init`
  plus `doctor`'s reconstruction diff (`src/doctor.rs:1662-1675`) recreate
  the missing rows from the enrollment marker and store manifest already
  on disk.
- **Whole-table reads vs. the 10K materialization bound → rowid paging.**
  The SQL channel underlying the global DB materializes an entire result
  set before yielding row one and rejects anything past `MAX_QUERY_ROWS`
  (`10_000`) or 64 MiB (`crates/tracedecay-global-db/src/schema_contract/invariants.rs:40-49`).
  Authority-invariant audits page with a keyset cursor at
  `AUDIT_PAGE_ROWS = 128` (`invariants.rs:49`) and observation scans at
  `OBSERVATION_AUDIT_PAGE_ROWS = 48` (`invariants.rs:52-58`) instead of one
  unbounded `SELECT`. The migration path uses the same pattern —
  `MIGRATION_QUERY_PAGE_ROWS = 256`
  (`crates/tracedecay-migrate/src/hermes/copy.rs:14`) drives keyset-paged
  `SELECT rowid, ... WHERE rowid > ?1 ... ORDER BY rowid LIMIT ?2` queries
  (`crates/tracedecay-migrate/src/hermes/resolution.rs:225-252` and
  `crates/tracedecay-migrate/src/hermes/fingerprint.rs:59-81`), with a
  separate hard ceiling `MAX_MIGRATION_MATERIALIZED_ROWS = 1_000_000`
  enforced by `ensure_materialized_row_room()`
  (`crates/tracedecay-migrate/src/hermes/copy.rs:15,45-53`). Any recovery
  or audit code that issues a raw unbounded `SELECT` against these stores
  will eventually hit the 10K/64MiB ceiling on a large enough profile;
  the fix is always keyset (`rowid`-cursor) paging, not raising the limit.
- **Rollback-under-own-authority → retire runtime first.** A branch sync
  or corrupt-store repair that fails mid-flight cannot roll back or
  replace the database while this process still holds it mounted in the
  process-wide store-runtime registry — the deletion fence refuses any
  database the process still holds an authority for, and the failure
  previously surfaced as "this process already holds an incompatible
  database authority or deletion fence."
  `retire_branch_runtime_after_failed_sync()`
  (`src/tracedecay/lifecycle/branches.rs:378-410`) and the corrupt-branch
  repair path in `recover_corrupt_branch_or_fail()`
  (`src/tracedecay/lifecycle/recovery.rs:423-441`) both close the runtime's
  code-graph mount via `DaemonSessionRuntimeRegistryV1::close_code_graph_paths`
  *before* attempting rollback/replacement. The general rule: retire your
  own runtime's hold on a store before any rollback or repair that touches
  the same store's files.
- **Interactive-host activation = `ActivationDeferred` warning, not a
  failure.** `doctor`'s per-integration state machine treats a component
  that is staged but waiting on a human to activate it inside an
  interactive host (e.g. approving/reloading an editor extension) as
  `State::ActivationDeferred`, reported as a warning
  ("is staged and waiting on interactive host activation", not a failure
  (`src/doctor.rs:265-268`), distinct from `State::Missing`/`State::Corrupt`
  which fail. During recovery this means a doctor warning about deferred
  activation for an interactive host integration is expected and does not
  block declaring the dogfood recovery successful — only `fail`-level
  states (`Missing`, `Corrupt`, `OwnershipConflict`) do.

## 5. See also

- `scripts/dogfood.sh` — authoritative source for every gate described
  here.
- `.claude/skills/dogfooding-tracedecay/SKILL.md` /
  `.codex/skills/dogfooding-tracedecay/SKILL.md` — day-to-day dogfood
  usage; see its "Boundary recovery" section for the pointer back to this
  runbook.
- `src/cli.rs` — `migrate backup-profile`, `migrate rehearse-profile-backup`,
  `post-update --mode`, `init` command definitions.
- `crates/tracedecay-migrate/src/registry.rs` — registry reconstruction
  from on-disk enrollment markers and store manifests.
