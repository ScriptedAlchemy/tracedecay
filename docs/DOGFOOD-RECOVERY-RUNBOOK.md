# Dogfood Recovery Runbook

How to recover `cargo dogfood` if it fails partway through installing a new
binary. Documents the forward-only binary policy, the backup contract, the
recovery ladder, and the failure classes that strand a run. Sourced from
`scripts/dogfood.sh`, `.claude/skills/dogfooding-tracedecay/SKILL.md`, and
the runtime source cited inline. Do not read `~/.tracedecay` directly to
verify this document — every claim below traces to a file in this repo.

## 1. The forward-only binary policy

There is no on-disk state file tracking dogfood's progress across runs.
Each `cargo dogfood` invocation is self-contained: it stages a candidate
binary, atomically installs it, then runs `post-update --strict --mode
dogfood-forward-only` (`scripts/dogfood.sh:454`). In-run bookkeeping lives
entirely in shell variables local to the script process
(`replacement_active`, `boundary_reached`, `committed`,
`scripts/dogfood.sh:310-312`) — nothing is written to `~/.tracedecay` to
remember where a prior attempt left off.

The forward-only guarantee — never let an older binary reopen a store a
newer binary has already touched — is enforced by the binary itself at
open time (a schema/version identity check), not by a marker file. An old
binary either matches what a store expects or is refused there, typed and
observable, rather than by a script-side ceremony.

### What `cleanup_install()` does on failure

`cleanup_install()` (`scripts/dogfood.sh:356-415`) is an `EXIT`/`HUP`/
`INT`/`TERM` trap that only acts if a candidate replaced the installed
binary during this invocation (`replacement_active=1`) and the run did not
reach `committed=1`:

- **Before the new binary was installed** (`boundary_reached=0`): the
  script restores the pre-existing `installed_binary` and `staged_binary`
  from the copies it took at `scripts/dogfood.sh:436-447`
  (`restore_path()`, `scripts/dogfood.sh:316-326`). Safe to just rerun
  `cargo dogfood`.
- **After the new binary was installed but before `post-update` finished**
  (`boundary_reached=1`, set at `scripts/dogfood.sh:453`): the script does
  **not** restore the previous binary — running an older binary against a
  store the new one may already have touched is exactly what the
  forward-only policy forbids. Instead it runs
  `post-update --mode dogfood-recover-inactive` against whichever binary is
  available (installed, then staged, then the candidate,
  `scripts/dogfood.sh:367-380`) to prove the managed daemon is stopped, then
  prints recovery instructions to stderr
  (`scripts/dogfood.sh:381-395`). Recover forward: fix or rebuild a newer
  binary and rerun `cargo dogfood`.

## 2. The backup contract

Two mutually exclusive modes, gated by `TRACEDECAY_DOGFOOD_BACKUP_PLAIN`
(`scripts/dogfood.sh:291-303`):

- **Checksummed backup (default).** `TRACEDECAY_DOGFOOD_BACKUP` must name a
  directory containing `backup-manifest.json`
  (`tracedecay migrate backup-profile --to <dir> --backup-id <id>`, CLI
  definition at `src/cli.rs:1218-1225`). Before installing the new binary,
  `scripts/dogfood.sh:423-431` rehearses it with
  `tracedecay migrate rehearse-profile-backup --backup <dir> --restore <tmp>`
  (`src/cli.rs:1227-1234`) — a full restore-and-verify into a throwaway
  directory, deleted immediately after. This is the safe, default path but
  re-reads and re-writes the entire profile twice.
- **Plain backup** (`TRACEDECAY_DOGFOOD_BACKUP_PLAIN=1`, the owner-authorized
  fast path, `scripts/dogfood.sh:291-297`).
  `TRACEDECAY_DOGFOOD_BACKUP` must name a directory holding a plain `cp -a`
  profile copy at `<dir>/profile`, with `<dir>/profile/global.db` present.
  No manifest, no rehearsal (`scripts/dogfood.sh:423` skips the rehearsal
  step entirely when this flag is set). The script only checks that the
  copy *looks like* a profile (directory + `global.db` file exist) — it
  does not verify contents. This mode exists for profiles large enough
  that the checksummed path's two full read/write passes outlast the
  available maintenance window.

Naming a backup is optional (see below), but when one is named, both modes
require it to already exist; `cargo dogfood` never creates one implicitly.

A backup is optional insurance, not a gate. With `TRACEDECAY_DOGFOOD_BACKUP`
unset, dogfood proceeds and warns on stderr that it is running without one
(`scripts/dogfood.sh:285-290`); the forward-only policy still recovers
forward (rung 1 below), but rungs 2 and 3 have nothing to restore from.
Naming a backup that is incomplete is still refused outright — that is a
misconfiguration, not an opt-out.

## 3. The recovery ladder

In order, this is what takes a stuck forward-only run back to a clean
install:

1. **Zero-writer proof.** Before touching anything further, confirm no
   process still holds the managed daemon or an authority lease on the
   live stores. `scripts/dogfood.sh`'s own failure path does this
   automatically via `post-update --mode dogfood-recover-inactive`
   (`cleanup_install()`, `scripts/dogfood.sh:376-380`), which runs the
   retained/staged/candidate binary just far enough to prove the managed
   service is stopped.
2. **Plain backup.** `TRACEDECAY_DOGFOOD_BACKUP_PLAIN=1` with
   `TRACEDECAY_DOGFOOD_BACKUP=<dir>` pointing at a `cp -a` copy already
   taken of `~/.tracedecay` (`<dir>/profile/global.db` present), per
   section 2.
3. **Restore the pre-failure `global.db`.** The registry — which projects
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
   profile is backed up, and `global.db` is consistent (restored or
   reconstructed), `cargo dogfood` runs the normal flow end to end and
   should install and validate cleanly.

## 4. Failure classes that strand a run

- **Unenrolled admission.** Host-admission code paths assume a project has
  an enrollment marker (`src/application/host_admission.rs:1226-1246`
  writes one if missing, mirroring what CLI init / daemon first-touch open
  / enrollment-root repair do in production). When a project's marker is
  missing or unreadable, `resolve_enrolled_layout()`
  (`crates/tracedecay-runtime-core/src/storage.rs:990-1019`) surfaces a
  denial rather than guessing, telling the caller to "open it through the
  daemon so the registry can resolve and repair its identity." A project with
  no marker must never be admitted on a best-effort path-derived guess; it
  goes through the daemon-owned repair path instead.
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
  unbounded `SELECT`. The profile-consolidation path uses the same
  pattern — `MIGRATION_QUERY_PAGE_ROWS = 256`
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
  database the process still holds an authority for, surfacing as "this
  process already holds an incompatible database authority or deletion fence."
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
  `State::ActivationDeferred`, reported as a warning ("is staged and waiting
  on interactive host activation") rather than a failure
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
