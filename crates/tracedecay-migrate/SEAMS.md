# Migration crate seams

`crates/tracedecay-migrate` owns the whole migration subsystem. When the move
landed, every reference to a root-owned module had been rewritten to
`crate::root_seam::<module>` (see `src/root_seam.rs`) so the coupling surface
was one greppable name and `cargo check -p tracedecay-migrate` reported it
exactly.

**The kernel half of that seam is now closed.** `tracedecay-runtime-core` is a
dependency of this crate and the twelve kernel modules
(`storage`, `db`, `sqlite_read_snapshot`, `errors`, `lifecycle_lease`,
`config`, `branch`, `branch_meta`, `memory`, `open_store_holders`, `worktree`,
`git`) plus `tracedecay::current_timestamp` were repointed in place — 343
references across 40 files, no behaviour change.

| Measurement | Then | Now |
| --- | ---: | ---: |
| `cargo check -p tracedecay-migrate` | 279 | 64 |
| `cargo check -p tracedecay-migrate --all-features` | 279 | 68 |
| `crate::root_seam::` references | 513 / 18 modules | 94 / 5 modules |

Everything below is what is left, and **none of it can be closed from inside
this crate.** Each remaining seam is blocked on another lane of the split. The
seam name is kept deliberately so the compiler keeps naming the blockers.

---

## Blockers, in the order that unblocks the most

### 1. `global_db` — 32 refs — blocked by a **package cycle**

`tracedecay-global-db` already depends on `tracedecay-migrate`
(`crates/tracedecay-global-db/src/observation/schema.rs:117` uses
`tracedecay_migrate::durability::{session_authority_table_class,
StoreDurabilityClass}`). Adding `tracedecay-global-db` to this crate's
manifest is therefore a hard Cargo cycle, not a soft ordering problem.

Two ways out:

1. **Move `durability` below both crates.** The global-db side of the edge is
   three lines against one module. `crates/tracedecay-migrate/src/durability.rs`
   has no dependency this crate owns exclusively; re-homing it in
   `tracedecay-store` (which both crates already depend on) deletes the cycle
   and lets this crate take a normal `tracedecay-global-db` dependency, closing
   all 32 refs with a pure repoint. **Recommended** — it is the only option
   that needs no new API.
2. **Invert the edge with a port.** Define
   `RegisteredGlobalDbPort` / `RegisteredGlobalDbWriteTransactionPort` here and
   let `tracedecay-global-db` implement them (it already depends on this
   crate, so the direction is legal). This is ~25 async trait-object methods
   plus eight DTOs (`CodeProjectRecord`, `GraphScopeUpsert`,
   `ProjectRegistryContext`, `ProjectStoreContext`, `ProjectAliasRecord`,
   `StoreInstanceRecord`, `StoreInstanceUpsert`, `StoreArtifactUpsert`) that
   would have to be defined here and re-exported from `tracedecay-global-db`.
   It also churns roughly sixty `&RegisteredGlobalDb` signatures in this crate
   and every root call site that passes one. Not worth it if (1) is available.

Partial relief that needs neither: **15 of the 32 refs are test-only**
(`registry/tests.rs`, `consolidate/tests.rs`, `consolidate/tests/observation.rs`,
`consolidate/tests/temporal.rs`). Cargo permits a dev-dependency to close a
cycle, and this one is accepted — `tracedecay-global-db` as a
`[dev-dependencies]` entry here resolves cleanly (verified with `cargo tree`)
because dev-dependencies are not part of the lib's own dependency graph. That
closes the test-only refs without touching `durability`. It is not wired up
yet only because `tracedecay-global-db` does not currently compile either (it
is blocked on this crate through the same normal dependency), so it would buy
nothing until seam 2's kind of repoint lands there too.

### 2. `sessions` — 29 refs — blocked by `tracedecay-sessions` being red

`cargo check -p tracedecay-sessions` reports 220 errors, all of the same shape
this crate just fixed: `crate::application` (71), `crate::db` (35),
`crate::privacy` (21), `crate::errors` (10), `crate::worktree` (8),
`crate::store` (8), `crate::config` (7), `crate::global_db` (5) … i.e. the
sessions lane has not yet repointed at `tracedecay-runtime-core`. A red
dependency cannot be added: Cargo compiles dependencies first, so taking the
dependency now would replace this crate's 64 diagnostics with the sessions
crate's 220 and never reach this crate at all.

**Owed:** land the sessions lane's kernel repoint. Then this crate adds
`tracedecay-sessions` and repoints all 29 refs with no other edit. Note that
`sessions::{SessionRecord, SessionMessageRecord}` (`consolidate/tests.rs:38`,
`hermes.rs:409`) actually resolve to `tracedecay_store::{SessionRecord,
SessionMessageRecord}`, which this crate can already reach — they only wait on
the same import rewrite.

### 3. `daemon` — 21 refs — blocked by `daemon/store_runtime` having no owner

No `tracedecay-daemon` crate exists; `src/daemon/**` is still root-only. Eight
items are needed, and they are not all the same weight:

| Item | Refs | Shape |
| --- | ---: | --- |
| `daemon::profile_identity::load_or_create` | 8 | returns a daemon identity value held across calls |
| `daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1` (+`::open`) | 7 | **stored in a public struct field** (`registry.rs:115`) and returned from `consolidate/mod.rs:388` |
| `daemon::store_runtime::registry` | 1 import, 15 types | `consolidate/runtime.rs` builds and drives a whole `StoreRuntimeRegistry` |
| `daemon::code_index_scheduler::identity::IndexingIdentityV1::resolve` | 2 | value constructor |
| `daemon::QuiescedDaemonLifecycle::acquire` | 1 | RAII guard |
| `daemon::store_runtime::resolver::canonical_store_locator_digest` | 1 | pure function |
| `daemon::daemon_reachable` | 1 | pure probe |

Only the last two are port-shaped (a fail-closed `OnceLock` function pointer in
the `tracedecay_runtime_core::ports` house style). The rest are **types**, and
a port cannot produce a type — turning them into trait objects would change
this crate's public API and every root call site.

**Owed / recommended:** `crates/tracedecay-runtime-core/SEAMS.md` seam 1
already proposes moving `daemon/store_runtime/` **into**
`tracedecay-runtime-core` (it counts 53 references to `crate::db`, 14 to
`crate::storage` against 34 to sibling daemon modules, and needs it to fix its
own `--features test-transport` build). Doing that closes 15 of these 21 refs
here for free and is the single highest-leverage move in the whole split.
`profile_identity`, `code_index_scheduler::identity`, `daemon_reachable`, and
`QuiescedDaemonLifecycle` still need a home — a `tracedecay-daemon` crate, or
the kernel for the first three, which are pure identity/probe logic.

### 4. `agents` — 6 refs — blocked by `tracedecay-agent-hosts` being red

`cargo check -p tracedecay-agent-hosts` reports 194 errors, again the same
un-repointed `crate::errors` / `crate::db` shape. Same rule as sessions: the
dependency cannot be taken until that lane lands.

Three of the six refs are one pure function,
`agents::hermes::read_config_pinned_project_root` (`hermes.rs:154`,
`hermes.rs:313`, `hermes/resolution.rs:194`) — a YAML pin reader with no host
state. If the agent-hosts lane slips, that one is a candidate to move down
rather than wait. The other three (`agents::AgentIntegration`,
`agents::hermes::HermesIntegration`, `agents::expected_tool_perms`) are the
real host-integration surface and must wait.

### 5. `application::host_admission` — 6 refs — blocked by non-extraction

`host_admission` is **not** in `tracedecay-application`; it is still
`src/application/host_admission.rs` in the root crate. Four of the six refs are
test-only. The two library refs (`hermes/pipeline.rs:553-554`) build a
`HostAdmissionFacade` purely to hand to
`sessions::hermes::ingest_legacy_pinned_profile` on the next line, so they
close together with seam 2 — most cleanly by making "ingest a legacy pinned
profile into this target store" one call the sessions crate owns, with the
facade constructed on its side of the boundary.

---

## 6. Newly found gap: four kernel functions the kernel mover left behind

These are **not** listed as owed in `crates/tracedecay-runtime-core/SEAMS.md`,
so nobody currently owns them. Each is pure logic whose every dependency is
already inside the kernel, and each has heavy root callers
(`src/tracedecay/lifecycle.rs`, `src/tracedecay/diagnostics.rs`,
`src/tracedecay/scan.rs`), so they are moves, not copies:

| Function | Root home | Needs | Used here |
| --- | --- | --- | --- |
| `branch::sanitize_branch_name` | `src/branch.rs:198` | nothing | `consolidate/mod.rs:823`, `consolidate/prepare.rs:199` |
| `branch::detect_default_branch` | `src/branch.rs:103` | `gix`, kernel `branch::current_branch` | `consolidate/mod.rs:773` |
| `branch::resolve_branch_db_path` | `src/branch.rs:283` | kernel `branch_meta::BranchMeta` | `inventory/project.rs:362` |
| `config::is_generated_dir_segment` | `src/config.rs:289` | `GENERATED_DIR_SEGMENTS` | `inventory/project.rs:419` |

Two more are adapters the kernel mover deliberately left in the root shim
`src/storage.rs` because they take `global_db` types:
`storage::classify_registry_storage` (`inventory/scan.rs:161`) and
`storage::try_classify_project_storage_with_registry` (`registry.rs:16`). Both
are three-line wrappers over the already-`pub`
`tracedecay_runtime_core::storage::classify_registry_storage_fields`, so they
follow `global_db` (seam 1) rather than needing a design of their own.

## 7. `project_registry` — resolved, nothing owed

The earlier catalog flagged a `project_registry` ambiguity. There are no
`root_seam::project_registry` references in this crate; the only
`project_registry` tokens (`hermes/resolution.rs:133`, `:139`) are calls to
`RegisteredGlobalDb::project_registry_context_by_alias`, i.e. part of seam 1.
`primary_checkout_root` did move to `tracedecay_runtime_core::project_registry`
as the kernel's SEAMS.md records, but this crate never referenced it.

---

## Summary of what this crate owes vs. what it is owed

**Owes: nothing.** Every kernel reference is repointed; the crate's own code
is correct against the target architecture.

**Is owed**, in leverage order:

1. `daemon/store_runtime/` moved into `tracedecay-runtime-core` (kernel seam 1)
   — closes 15 refs here and unblocks seam 7.
2. `durability` moved to `tracedecay-store` — deletes the `global-db → migrate`
   cycle, closes 32 refs here.
3. The `tracedecay-sessions` kernel repoint — closes 29 refs here.
4. The `tracedecay-agent-hosts` kernel repoint — closes 6 refs here.
5. `application::host_admission` extracted out of the root crate — closes 6.
6. The four stranded kernel functions in section 6 moved into
   `tracedecay-runtime-core` — closes 5.
