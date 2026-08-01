# Test-support features for the crate split (2026-08-01)

Companion to `2026-07-31-one-shot-crate-split.md`. That plan moved subsystems
out of `src/` and accepted breakage; lib targets converged first. This plan
covers the **`--all-targets` aftermath**: upstream test-only surfaces are
`#[cfg(test)]`-gated, so they vanish at the crate boundary and every downstream
crate's `lib test` target fails to resolve them.

## The convention

One uniform mechanism, three rules.

### 1. Feature name: `test-helpers`

Every crate that must expose a test-only surface to *dependent crates' test
builds* declares:

```toml
[features]
test-helpers = []
```

**Why `test-helpers` and not `test-support`.** `test-helpers` is already the
repo-wide majority and already has working dev-dependency wiring:
`tracedecay-global-db`, `tracedecay-code-extraction`, `tracedecay-temporal-query`,
`tracedecay-semantic`, and `tracedecay-query` all ship it today, and
`tracedecay-migrate` already consumes global-db through it. `test-support`
appears only as a proposal in `crates/tracedecay-usecases/SEAMS.md`. Adopting
the name already in the tree costs nothing and avoids a second vocabulary.

**Why not fold this into `test-transport`.** `crates/tracedecay-global-db/SEAMS.md`
suggested widening the kernel's existing `test-transport` instead. Rejected:
`test-transport` is a semantically distinct feature — it gates *daemonless
fixture runtimes and in-process transport doubles* and forwards
`tracedecay-rusqlite-runtime/test-transport`. A crate that only wants
`TestConnection` would be forced to pull the transport doubles too. The two
features stay separate; both are legitimate and a crate may declare both.

### 2. Gate form: `#[cfg(any(test, feature = "test-helpers"))]`

Applied to the item *and* to its re-export, and to every transitively required
sibling (private trait methods, enum variants, struct fields, initializers).
The negative form is `#[cfg(not(any(test, feature = "test-helpers")))]`.

The `any(test, ...)` shape is required, not stylistic: `test` alone keeps the
in-crate test build working with the feature off, and the feature alone is what
crosses the crate boundary. Dropping `test` would make the crate's own tests
depend on its own feature being enabled.

### 3. Wiring: dev-dependency on the same crate with the feature

The consumer re-declares the *same path dependency* under `[dev-dependencies]`
with the feature added:

```toml
[dev-dependencies]
tracedecay-runtime-core = { path = "../tracedecay-runtime-core", version = "0.1.0", features = ["test-helpers"] }
```

Cargo unifies features across the normal and dev dependency **only when a test
target is in the build**, so the lib/production build never sees the feature,
while every test target in that crate does. This is the pattern
`tracedecay-migrate` already used for `tracedecay-global-db`.

### What NEVER rides behind `test-helpers`

- **Production code.** If a non-test caller needs it, it is a normal `pub` item,
  not a feature-gated one. A gate is a statement that no shipped code path
  reaches the item.
- **Anything that changes production semantics when off.** Widening a gate must
  be observationally neutral for the default feature set. Verify with
  `cargo check -p <crate>` (no features) after every widening.
- **Feature-gated *behavior*.** `test-helpers` exposes existing test doubles and
  helper constructors. It must never switch an algorithm, relax an authority
  check, or bypass admission. `DatabaseAuthority::acquire_test` is `#[doc(hidden)]`
  behind `test-transport` precisely because it is a capability seam, not a
  helper — capability seams keep their own feature.
- **`mod tests` itself.** Test modules stay `#[cfg(test)]`. Only the *helpers*
  they share cross the boundary.

## Tier 1 — foundation (IMPLEMENTED)

### Measured effect

`cargo check -p <crate> --all-features --all-targets`, error counts:

| Crate | Before | After | Delta |
|---|---:|---:|---:|
| tracedecay-runtime-core | 0 | 0 | 0 |
| tracedecay-sessions | 52 | 29 | -23 |
| tracedecay-global-db | 53 | 33 | -20 |
| tracedecay-usecases | 175 | 160 | -15 |
| tracedecay-migrate | 41 | 30 | -11 |
| tracedecay-agent-hosts | 34 | 15 | -19 |
| **total** | **355** | **267** | **-88** |

### Gates widened

`crates/tracedecay-runtime-core/` (feature `test-helpers` added to `Cargo.toml`):

| File | Lines | Item |
|---|---|---|
| `src/db/engine/mod.rs` | 7, 20 | `mod statement`, `pub use statement::Statement` |
| `src/db/engine/mod.rs` | 9, 22 | `mod test_support`, `pub use test_support::TestConnection` |
| `src/db/engine/connection.rs` | 9 | `use super::Statement` |
| `src/db/engine/connection.rs` | 21, 23, 27 | `Runtime::{validate, last_insert_rowid, begin_deferred}` decls |
| `src/db/engine/connection.rs` | 54, 59, 73 | same three impls for `MigrationSqlHandle` |
| `src/db/engine/connection.rs` | 187, 197 | `Connection::{prepare, last_insert_rowid}` |
| `src/db/engine/connection.rs` | 218, 229 | `Connection::transaction`, the `Deferred` match arm |
| `src/db/engine/transaction.rs` | 17 | `TransactionBehavior::Deferred` variant |
| `src/db/engine/transaction.rs` | 24, 33, 37 | `Transaction::connection_runtime` field + initializer |
| `src/db/engine/transaction.rs` | 150 | `Transaction::last_insert_rowid` |
| `src/db/migrations.rs` | 359, 525 | `migrate_connection`, `migrate_test_connection_to_version` |
| `src/sqlite_read_snapshot.rs` | 218, 740 | `SnapshotDatabase::copied_bytes` field + initializer |
| `src/sqlite_read_snapshot.rs` | 287 | `SnapshotDatabase::copied_bytes()` |
| `src/sqlite_read_snapshot.rs` | 350, 408 | `SnapshotSet::{capture, database_count}` |
| `src/sqlite_read_snapshot.rs` | 463 | `sqlite_read_snapshot::open` |
| `src/sqlite_read_snapshot.rs` | 823 | `default_scratch_root` (required by `capture`) |

Note the closure: exposing `TestConnection` alone is not enough. Downstream
tests call `TestConnection::transaction()`, which needs `Connection::transaction`,
which needs `TransactionBehavior::Deferred`, which needs
`Transaction::connection_runtime`, which needs `Runtime::begin_deferred`. Any
future widening must chase the whole chain or the surface stays unusable.

`crates/tracedecay-sessions/` (feature `test-helpers` added):

| File | Line | Item |
|---|---|---|
| `src/runtime/lcm/payload.rs` | 112 | `write_external_payload` |
| `src/runtime/cursor_composer.rs` | 57 | re-export of `normalize_cursor_composer_observation` and `..._with_projected_message_id` from `tracedecay_capture` |

One dead-code annotation was needed:
`crates/tracedecay-runtime-core/src/db/engine/test_support.rs:33`
(`open_without_write_authority` is crate-private and only the in-crate engine
tests use it, so it is unused under `feature = "test-helpers"` alone).

### Dev-dependency wiring added

| Crate | Added |
|---|---|
| `tracedecay-sessions` | `runtime-core/test-helpers`; also `filetime` (its `runtime::vibe` and `runtime::workflow_ingest` tests backdate mtimes and the dev-dep was simply missing) |
| `tracedecay-global-db` | `runtime-core/test-helpers`, `sessions/test-helpers` |
| `tracedecay-usecases` | `runtime-core`, `global-db`, `semantic`, `sessions` — all `/test-helpers` |
| `tracedecay-migrate` | `runtime-core/test-helpers`, `sessions/test-helpers` (global-db already wired) |

Separately, `tracedecay-agent-hosts` declared `test-transport = []` — an empty
feature that forwarded nothing. Corrected to
`test-transport = ["tracedecay-runtime-core/test-transport"]`, which is what
`tracedecay-usecases` and `tracedecay-migrate` already do. That single line
accounts for 14 of the 19 errors that crate lost (`Database::publish_test_runtime`
and `DatabaseAuthority::acquire_test`).

## Tier 2 — follow-up (NOT in scope here)

Every remaining error is a **relocation or fixture-ownership** problem, not a
gate problem. No amount of feature work in the kernel fixes them.

### 2a. Root-owned test runtimes — owner: composition-root lead

`HostAdmissionTestRuntimeV1` / `ProjectScopedTestRuntimeV1` / `HostAdmissionScope`
still live in the root crate's `src/application/host_admission.rs`. The sessions
and migrate movers took the production half and left the test runtime behind.

- `crates/tracedecay-sessions/`: 13 errors — `runtime/claude_observation.rs:911`,
  `runtime/claude_observation_benchmark/runner.rs:12`, `runtime/cline_like.rs:25,1196,1263`,
  `runtime/cursor.rs:1802`, `runtime/cursor_composer/tests.rs:15`,
  `runtime/hermes/tests.rs:12`, `runtime/ingest/tests.rs:8`, `runtime/kiro.rs:1067,1139,1216`
- `crates/tracedecay-migrate/`: `src/consolidate/tests.rs:29`,
  `src/consolidate/tests/observation.rs:6`, `src/hermes.rs:402`
- `crates/tracedecay-usecases/src/host_admission.rs` and the root crate itself
  (`src/mcp/server.rs:294,492,501,519`, `src/tracedecay.rs:65,112`,
  `src/tracedecay/lifecycle.rs:101,115`)

**Recommendation:** move the test runtimes down to `tracedecay-sessions::admission`
behind that crate's new `test-helpers` feature, alongside the production half
that already moved. This is the single highest-leverage Tier 2 item — it is the
largest error class and it unblocks the root crate's own lib target.

### 2b. `FixtureGraph` / `GraphRuntimePort` test doubles — owner: usecases lead

`crates/tracedecay-usecases/src/edit.rs`: 40 × `E0277 FixtureGraph: GraphRuntimePort`
plus 16 × `dyn GraphRuntimePort` unsized errors and 5 ×
`GraphRuntimePort::{init, open_*, resolve_registered_configuration_layout}`
called as if the port were a concrete type. One root cause repeated: the
fixture predates the port trait. Must be rewritten against the current trait
and re-homed at the composition root. **Not a feature problem — do not attempt
to gate your way out of it.**

### 2c. Repo-root `#[path]` fixtures — owner: composition-root lead

- `crates/tracedecay-global-db/src/session_temporal/` `#[path]`-includes
  `tests/session_suite/lcm_schema/{mod,lcm_migration}.rs` from the repo root;
  those files say `crate::{sessions, db, errors, global_db}` and resolve against
  the *root* crate. 20 errors.
- `crates/tracedecay-migrate/src/consolidate/tests.rs:815-817` `#[path]`-includes
  three `src/global_db/schema*` files that no longer exist at that relative path.
  3 errors, all `couldn't read`.

**Recommendation:** re-home these under the owning crate's `/src` or `/tests`
and repoint the imports. They cannot be feature-gated.

### 2d. Test-only surfaces still in the root crate — owner: kernel/root split lead

These are `#[cfg(test)]` items that downstream crates name at a
`tracedecay_runtime_core::` path, but that never moved out of `src/`:

| Named as | Actually lives in | Consumers |
|---|---|---|
| `runtime_core::config::PinnedUserDataDir` | `src/config.rs:2311-2374` | usecases (3), agent-hosts (2), migrate (1) |
| `runtime_core::config::lock_user_data_dir_test_env` | `src/config.rs:2301` | usecases (1) |
| `runtime_core::branch::{BranchAdminAction, BranchAdminOutcome, BranchAdminReport, prepare_branch_admin_mutation}` | `src/branch/admin.rs` | migrate (1 import, 4 symbols) |
| `runtime_core::branch::gc_dead_branch_stores` | `src/branch.rs` | migrate (1) |
| `runtime_core::tracedecay::TraceDecayOpenOptions` | `src/tracedecay.rs` | migrate (1), usecases (5) |

**Recommendation:** these are *moves into the kernel*, and once moved each gets
the Tier 1 treatment (`#[cfg(any(test, feature = "test-helpers"))]` + the
dev-dep wiring). `PinnedUserDataDir` is the best first candidate: it is
self-contained, has three consumer crates, and `runtime_core::config` already
owns `USER_DATA_DIR_ENV` and `user_data_dir()`.

### 2e. Crate-boundary leftovers — owner: respective crate movers

- `crates/tracedecay-sessions/src/runtime/workflow_ingest/tests.rs` and
  `claude_observation.rs:1160` want `tracedecay_global_db`, but global-db
  *depends on* sessions. Genuine cycle: these tests must move up into global-db
  or the root, not gain a dev-dep.
- `crates/tracedecay-sessions/src/runtime/lcm/gc/tests.rs:20,33` — 2 × `E0117`
  orphan-rule violations, and `runtime/claude_observation.rs:1003` — `E0119`
  duplicate `HostAdmission for CapturePortSpy`. Fixture consolidation, local fix.
- `crates/tracedecay-global-db`: `RegisteredGlobalDb::observation_store` is named
  by 7 migrate tests but no longer exists; `RegisteredGlobalDb: SessionIngestAuthority`
  unsatisfied in 6 usecases tests. Trait-surface drift, owned by global-db.
- `crates/tracedecay-sessions/src/compatibility.rs:250-259` (`crate::lcm`
  constants), `runtime/lcm/raw.rs:1131` (`crate::user_config`) — root shims.
- `crates/tracedecay-agent-hosts/src/agents/cursor.rs:1734` — `AdvertisedToolV1`
  lost its `annotations` field. Unrelated schema drift, 1 error.

## Verification gate

For any crate touched by this convention:

```
cargo check -p <crate>                                  # production path unchanged
cargo check -p <crate> --all-features --all-targets     # test path resolves
```

The first command is not optional — it is the only thing that proves a widened
gate did not leak into the shipped build.
