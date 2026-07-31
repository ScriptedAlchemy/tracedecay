# `tracedecay-code-index` Extraction Plan

> **Archived provenance — not current requirements.** This document records
> historical planning and execution evidence. Current scope and acceptance come
> only from [`00-plan-set-index.md`](../../../plans/tracedecay-v2/00-plan-set-index.md),
> [`NEXT.md`](../../../plans/tracedecay-v2/NEXT.md), and the applicable numbered
> V2 plan. Do not recreate its task checklists, file inventories,
> branch/worktree/SHA or commit protocol, Gate A/B, timing/JUnit receipts, exact
> test names/counts, generated-byte/source-shape checks, PR closure gates, or
> platform gate lattice.
> Pure source-only/internal extraction APIs change in place. Potentially
> deployed branch-era public APIs remain compatible until an authorized
> installed-client/host census proves absence. Any index manifest, generation,
> journal, checkpoint, or receipt potentially written by dogfood keeps
> backward-read/migration or rebuild recovery until the separately authorized
> registered-store/profile census proves absence.

**Goal:** Isolate extraction/index compilation, grammar/WGSL build ownership,
and focused search tests from unrelated root edits.

**Historical dependency assumption:** Domain DTO and query extraction preceded
this work. FastEmbed remained asynchronous and did not block project admission
or ordinary retrieval.

## Historical file and interface inventory

- Create `crates/tracedecay-code-index/{Cargo.toml,build.rs,src/lib.rs}`.
- Move `src/code_index/**`, `src/extraction/**`, and matcher core from
  `src/ast_grep_search.rs`.
- Keep `src/extraction_worker.rs`, daemon scheduling, MCP handlers, and surface
  wiring in root.
- Move language feature definitions, grammar dependencies, WGSL generation,
  fixtures, benches, and package includes to the new crate.
- Move `code_index_suite`, `extraction_suite`, `search_quality_suite`, and
  `semantic_search_suite` where they no longer relink root.

Public handoff:

```rust
pub trait CodeIndexReadPort: Send + Sync {}
pub trait CodeIndexWritePort: Send + Sync {}
pub use tracedecay_code_index as code_index;
```

The crate consumes domain identities/chunks and store/application ports. It
does not own daemon admission, scheduling, MCP transport, or project routing.
`semantic_code` moves only if dependency and timing evidence beats an explicit
measured deferral.

## Historical task checklist

- [ ] Add dependency-direction and root-inline-source architecture failures.
- [ ] Create the minimal manifest/feature matrix and move build ownership.
- [ ] Move extraction/index modules mechanically and preserve canonical digests.
- [ ] Decide semantic adapter ownership with same-host measurements.
- [ ] Relocate focused suites and preserve root façade and worker integration.
- [ ] Verify all language features, lite grammar, package contents, benches,
      and no unexpected WGSL/grammar reruns on unrelated root edits.

## Product outcome contributed

Extraction/index compilation and grammar/WGSL ownership moved away from
unrelated root edits while digest identity, query behavior, typed failures, and
non-blocking semantic acquisition remained equivalent. Current direct behavior
and acceptance live in the applicable numbered V2 plan.

## Historical migration, rollback, measurement, and deletion notes

No persisted schema changes. Commit manifest/build ownership, extraction move,
index move, semantic disposition, tests, and façade cleanup separately.
Rollback each with `git revert`.

The historical Gate A-index experiment used an identical warm private edit,
focused compile, rebuild-unit, and digest comparison; those thresholds are not
current acceptance. Delete old root modules,
build inputs, evidence-backed released feature aliases, and package entries only after callers and
package/default/all/lite gates pass.
