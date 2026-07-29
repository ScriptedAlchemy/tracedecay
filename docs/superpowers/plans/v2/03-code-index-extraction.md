# `tracedecay-code-index` Extraction Plan

**Goal:** Isolate extraction/index compilation, grammar/WGSL build ownership,
and focused search tests from unrelated root edits.

**Dependencies:** Domain DTO and query extraction gates pass. FastEmbed remains
asynchronous and cannot block project admission or ordinary retrieval.

## Files and interfaces

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

## Tasks

- [ ] Add dependency-direction and root-inline-source architecture failures.
- [ ] Create the minimal manifest/feature matrix and move build ownership.
- [ ] Move extraction/index modules mechanically and preserve canonical digests.
- [ ] Decide semantic adapter ownership with same-host measurements.
- [ ] Relocate focused suites and preserve root façade and worker integration.
- [ ] Verify all language features, lite grammar, package contents, benches,
      and no unexpected WGSL/grammar reruns on unrelated root edits.

## Tests

Direct: unchanged fixture bytes produce identical extraction/index digests,
symbol/edge rows, tombstones, lexical/graph/semantic results, and generation
publication.

Negative: malformed grammar, unsupported language, cancellation, stale lease,
deleted source, unavailable model, partial generation, and non-Git root remain
typed and non-blocking.

Run focused crate checks/tests, exact lite-grammar journeys, canonical digest
tests, semantic fallback/activation tests, and root all-feature integration.

## Migration, rollback, measurement, deletion

No persisted schema changes. Commit manifest/build ownership, extraction move,
index move, semantic disposition, tests, and façade cleanup separately.
Rollback each with `git revert`.

Gate A-index uses the identical warm private index edit and requires at least
20% or 8s improvement, focused test compile without full root, no unrelated
WGSL/grammar rebuild, and byte-identical digests. Delete old root modules,
build inputs, feature aliases, and package entries only after callers and
package/default/all/lite gates pass.
