# tracedecay-application seams

Written by the application mover during the one-shot crate split
(`docs/superpowers/plans/2026-07-31-one-shot-crate-split.md`).

The assigned task was the final mass move: relocate the root `src/application/`
tree (115 files, 85,608 lines) into `crates/tracedecay-application/src/runtime/`
behind a `src/application.rs` shim, mirroring what the sessions mover did for
`src/sessions/`.

**That move was not performed. It is structurally blocked, and executing it
would have broken the whole workspace.** This document records the evidence, the
cycle answers the task asked for, and the destination that does work.

What *was* landed is the `framed_log` flip (see "Kernel edge removal" below),
which was the prerequisite step in the assignment and is a real improvement on
its own.

## Crate check status

| Target | Status |
| --- | --- |
| `cargo check -p tracedecay-application` | **green** |
| `cargo check -p tracedecay-application --all-features` | **green** |
| `cargo check -p tracedecay-runtime-core` | **green** (after the flip) |
| `cargo check -p tracedecay-domain -p tracedecay-hooks` | **green** |

These were green before this mover ran and are green after. Performing the
assigned move would have taken `tracedecay-application` from 0 to ~1,000
unresolved-path errors, and because it is the crate that nearly everything else
links, it would have taken `runtime-core`, `rusqlite-runtime`, `hooks`,
`global-db`, `sessions`, `migrate`, `query`, `semantic`, `api`, `sdk`,
`agent-hosts`, `dashboard-api`, `search-eval` and the root binary down with it —
including lanes that are landing concurrently.

---

## The blocking fact: two different layers share one name

The assignment assumed `src/application/` and `crates/tracedecay-application/`
are two halves of one module that drifted apart. They are not. They are
**different architectural layers that happen to share a word**.

- `crates/tracedecay-application` is the **ports-and-contracts crate at the
  bottom of the stack**. It depends only on `tracedecay-domain`,
  `tracedecay-policy`, and `tracedecay-tool-catalog`. It defines traits like
  `WorkStoragePort`, `WorkflowDefinitionAuthorityPort`, `StoreSizeTelemetryPort`
  and `AuthorizedScopeSet`, which the storage and runtime crates *implement*.
  Fourteen workspace crates depend on it.

- `src/application/` is the **product use-case layer at the top of the stack**.
  It orchestrates the SQLite engine, the session runtime, the global database,
  the daemon and MCP surfaces. It is a *consumer* of everything the bottom crate
  is a *contract* for.

Moving the top layer into the bottom crate inverts the stack. Every one of the
1,007 outward references below becomes either a cycle or an unresolved path.

### Seam census

`crate::application::…` self-references are excluded (they would become
`crate::runtime::…`). `[kernel]` means the module now lives in
`tracedecay-runtime-core`; `[root]` means it is still in the root binary crate;
`[crate]` is a direct workspace-crate reference.

**Total outward references: 1,007** across 31 top-level modules and 115 files.

| Count | Target | Why it blocks |
| ---: | --- | --- |
| 301 | `crate::errors` [kernel] | cycle via runtime-core |
| 168 | `crate::sessions` [root] | `tracedecay-sessions` depends on application |
| 72 | `crate::db` [kernel] | cycle via runtime-core |
| 64 | `crate::config` [kernel] | cycle via runtime-core |
| 64 | `crate::global_db` [root] | `tracedecay-global-db` depends on application |
| 55 | `crate::tracedecay` [kernel] | cycle via runtime-core |
| 35 | `crate::storage` [kernel] | cycle via runtime-core |
| 22 | `crate::types` [kernel] | cycle via runtime-core |
| 22 | `crate::mcp` [root] | not extracted yet |
| 21 | `crate::daemon` [root] | not extracted yet |
| 19 | `crate::store` [kernel] | cycle via runtime-core |
| 16 | `crate::semantic_code` [root] | `tracedecay-semantic` depends on application |
| 14 | `crate::diagnostics_publication` [root] | not extracted yet |
| 13 | `crate::memory` [kernel] | cycle via runtime-core |
| 11 | `crate::search_eval` [root] | `tracedecay-search-eval` depends on application |
| 11 | `tracedecay_query` [crate] | depends on application |
| 10 | `crate::code_index` [root] | not extracted yet |
| 9 | `crate::request_identity` [root] | not extracted yet |
| 8 | `crate::migrate` [root] | `tracedecay-migrate` depends on application |
| 7 | `crate::privacy` [kernel] | cycle via runtime-core |
| 4 | `tracedecay_sessions`, `crate::user_config`, `crate::repository_provenance` | mixed |
| ≤3 each | 30 further targets | mixed |

### Nothing in the tree can move on its own

Only three modules have *no* direct outward reference — and each one still
depends, inside the tree, on a module that does:

| Module | Direct seams | Intra-tree dependency | Verdict |
| --- | ---: | --- | --- |
| `anchor_resolution` | 0 | `memory` (16 seams) | blocked |
| `source_authorization` | 0 | `configuration` (40 seams) | blocked |
| `mod.rs` | 0 | — | movable, but it is 33 lines of `pub mod` declarations |

The movable set is the empty set. This is not a case where a subset lands and
the rest is catalogued; the tree is one coupled unit anchored to the kernel.

> Method note: a flat `crate::([a-z_]+)` regex undercounts, because it misses
> members of braced `use crate::{a::…, b::…}` groups. The first pass of this
> census used one and wrongly reported `anchor_resolution`,
> `code_index` and `source_authorization` as free-standing. The numbers above
> come from a brace-expanding pass that also follows intra-tree coupling.

---

## Cycle answers

The assignment asked two specific questions.

### 1. `tracedecay-runtime-core` — does it depend on `tracedecay-application`?

**It did, and that direct edge is now removed.** It was two things:

- `framed_log` — the crash-safe fsync/append/rename primitives.
- `runtime_core::types` re-exporting `application::source_edit` result types.

Both are resolved (see below), and `tracedecay-application` is gone from
`crates/tracedecay-runtime-core/Cargo.toml`.

**This does not make `application → runtime-core` legal.** runtime-core still
reaches application transitively:

```
tracedecay-runtime-core → tracedecay-rusqlite-runtime → tracedecay-application
```

That edge is load-bearing hexagonal design, not an accident:
`tracedecay-rusqlite-runtime` is the SQLite *adapter* that implements
application's ports (`WorkStoragePort`, `WorkProjectionReadPort`,
`WorkflowDefinitionAuthorityPort`, `TaskHandoffAuthorityPort`,
`StoreSizeTelemetryPort`, `AuthorizedScopeSet`) across `work.rs`, `workflow.rs`,
`repository/scope_set.rs` and `telemetry/store_size.rs`. Breaking it would mean
relocating the port definitions out of the crate whose entire purpose is to own
them. The 600 kernel-directed references in the tree stay blocked.

### 2. `tracedecay-global-db` — is its dependency on application real?

**Yes, real and unremovable in this direction.** `crates/tracedecay-global-db`
uses `tracedecay_application::{WorkService, WorkProjectionReadService,
WorkflowDefinitionService, TaskHandoffService, now_micros}` in
`src/registered.rs`, plus `src/session_temporal/{cursor_keys,hydration}.rs` and
its test modules. `global-db` composes application's services over its own
storage; the direction is correct as it stands.

Therefore `tracedecay-application` cannot depend on `tracedecay-global-db`, and
the 64 `crate::global_db` references stay blocked. Same answer, same shape, for
`tracedecay-sessions` (168), `tracedecay-semantic` (16),
`tracedecay-search-eval` (11), `tracedecay-query` (11) and
`tracedecay-migrate` (8).

---

## Kernel edge removal (landed)

Commit `refactor(kernel): drop runtime-core edge into application`.

`tracedecay-domain::framed_log` already owned the *pure* half of the framed-log
primitives (`checksum`, `CHECKSUM_BYTES`, `partial_tail_matches_prefix`) while
`tracedecay-application::framed_log` owned the *I/O* half (`sync_directory`,
`atomic_write`, `append_durable`, `with_owned_temp_publish`, …). One concept,
split across two crates, with the kernel forced to reach up into the contract
crate for an fsync call.

The I/O half moved down into `tracedecay-domain::framed_log`, joining its other
half. `tracedecay-domain` has no workspace dependencies at all, so no cycle is
reachable from it, and every existing consumer already links it.

- `tracedecay_application::framed_log` and the flat re-exports
  (`DirectorySyncPolicy`, `sync_directory`, `atomic_write`, …) are kept as a
  compatibility surface, so **no caller path changed** — including
  `tracedecay-hooks` and `crates/tracedecay-sessions`, which this mover does not
  own.
- `runtime_core::types` no longer re-exports `application::source_edit`; that
  module is a pure façade, so the re-export moved to the root `crate::types`
  shim, which now unions both halves.

**Deviation from the assignment:** the task specified moving `framed_log` into
`tracedecay-runtime-core`. It went into `tracedecay-domain` instead, because
domain is the actual bottom of the stack, already owned half of this exact
module, and — decisively — routing it through runtime-core would have forced
`tracedecay-hooks` (a small crate depending only on application and domain) to
take on the entire kernel, with its `rusqlite`, `gix` and full-feature `tokio`
trees, to keep calling `atomic_write`. The stated goal (delete the
runtime-core → application edge) is met either way.

---

## Recommended destination

The tree wants a crate **above** the kernel, not the contract crate below it.
A new crate — `tracedecay-usecases`, say — depending on `runtime-core`,
`sessions`, `global-db`, `query`, `semantic`, `search-eval`, `migrate`, `hooks`
and `code-index` is fully acyclic, because none of those depends on it.

Counted against the same census, that destination resolves **905 of the 1,007
references** by dependency alone. The 102 that remain are all of one kind — root
modules that have not been extracted yet — and none of them is a cycle:

| Count | Target |
| ---: | --- |
| 22 | `crate::mcp` |
| 21 | `crate::daemon` |
| 14 | `crate::diagnostics_publication` |
| 9 | `crate::request_identity` |
| 4 | `crate::repository_provenance`, `crate::user_config` |
| 3 | `crate::analytics_bridge`, `crate::diagnostics_store` |
| ≤2 each | `ast_grep_search`, `agents`, `diagnostics`, `git_query`, `git_intelligence`, `dashboard`, `context`, `graph`, `application_surface`, `graph_semantic_capabilities`, `production_semantic_authorities`, `retention`, `name`, `diagnostics_query` |

That is a normal aftermath work-list of the same shape the sessions and
runtime-core movers left behind — a tenth the size, and reachable by a
mechanical move rather than an architecture change.

Choosing the destination is a plan-level decision, so this mover stopped here
rather than creating a workspace crate that other lanes' manifests know nothing
about.

### If the tree must land in this crate regardless

Then the ports have to be inverted first, module by module: the crate declares a
trait, the root implements it and registers it at composition time. The 600
kernel-directed references are the bulk of that work, and `crate::errors` (301)
is the single highest-leverage one. `crates/tracedecay-runtime-core/src/errors.rs`
is 389 lines and depends only on `thiserror`, `tracedecay-automation` (no
workspace deps) and one `From<tracedecay_lsp::analyzer::AnalyzerRuntimeError>`
impl, so it *could* follow `framed_log` down into `tracedecay-domain` if that
one impl moved into `tracedecay-lsp`. Note this unblocks no module on its own —
every module carrying `crate::errors` also carries `crate::db`, `crate::config`,
`crate::storage` or `crate::sessions` — so it is only worth doing as step one of
a full inversion, not as a standalone win.
