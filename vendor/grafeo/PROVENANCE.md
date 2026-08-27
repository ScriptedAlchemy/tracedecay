# Vendored dependency provenance — grafeo

TraceDecay's graph boundary (`crates/tracedecay-graph-db`) sits on grafeo. Three
of grafeo's crates are vendored here and wired in through `[patch.crates-io]` in
the workspace root `Cargo.toml`, because the published 0.5.42 release cannot
serve TraceDecay's tiered-storage path without local fixes.

`grafeo-adapters` and `grafeo-storage` are **not** vendored. They stay on
crates.io and pick the patched crates up transitively — `cargo tree -p
tracedecay-graph-db` shows `grafeo-adapters v0.5.42` depending on
`grafeo-core v0.5.42 (…/vendor/grafeo/grafeo-core)`.

## Source

| field | value |
| --- | --- |
| upstream project | Grafeo |
| upstream URL | <https://github.com/GrafeoDB/grafeo> |
| upstream revision | `3bc891129ddec91b97a0613c9032b06d489695c4` (from each crate's `.cargo_vcs_info.json`) |
| version | 0.5.42 |
| retrieved from | the crates.io registry cache, i.e. the published `.crate` payloads |
| retrieved | 2026-08-27 |
| licence | Apache-2.0 — see `LICENSE-APACHE` |

The published `.crate` payloads carry no licence file of their own; the manifests
declare `license = "Apache-2.0"`. `LICENSE-APACHE` here is the canonical
Apache License 2.0 text, added to satisfy the redistribution term.

Integrity anchors — the crates.io checksums of the exact payloads these trees
were expanded from. They are also the `checksum` lines that this vendoring
removed from `Cargo.lock`, so the lockfile diff is itself a second witness:

| crate | sha256 of the published `.crate` |
| --- | --- |
| `grafeo-common` | `b5f446c25eeedab9cccaabc85060dfa08dac2d1041974244864f36436b81ca5a` |
| `grafeo-core` | `9e185dd750843637a2d99e56100c08ef4cc34ce4a7b5f9cb9566f873bbf54c90` |
| `grafeo-engine` | `ede967e6b0a16396c91752febdf037ab04ca69b851f6fb43565ee5f30586ee0d` |

The first commit in this directory is the pristine expansion of those three
payloads, with only three per-crate files dropped: `.cargo-ok` (a cargo cache
marker), and `Cargo.lock` (a packaged lock that a path dependency never reads).
`Cargo.toml.orig` and `.cargo_vcs_info.json` are kept — the first shows what the
upstream workspace manifest looked like before cargo normalised it, the second
pins the revision. **Every local edit therefore shows up as a diff against that
first commit**, which is what makes this tree auditable without re-downloading.

## Why a fork rather than an upstream fix

The owner's call: patch in-repo, do not upstream. The defects sit in a feature
(`tiered-storage`) that TraceDecay is the one exercising, the fixes are small and
local, and waiting on a release would block the memory work.

## Not a workspace member

These crates are listed in the root `[workspace] exclude`, not in `members`.
They are reached only through `[patch.crates-io]`, matching how
`vendor/tree-sitter-rust` is wired. Consequences worth knowing:

* `cargo test -p grafeo-common` from the repo root does **not** work — a patched
  package is in the dependency graph but is not a member.
* To run a vendored crate's own suite, run cargo from inside its directory.
  Each vendored manifest carries an empty `[workspace]` table so cargo treats it
  as a standalone package there instead of refusing to build a non-member. The
  `exclude` entry says the same thing from the other side and keeps holding if a
  manifest is ever refreshed without it; the `[workspace]` table is the one that
  works when the checkout is itself nested inside another repository, such as a
  git worktree under `.claude/worktrees/`.
* `grafeo-core` and `grafeo-engine` also carry a nested `[patch.crates-io]`
  pointing at their vendored siblings, so a standalone run inside them tests the
  patched arena rather than pulling the registry copy. Cargo honours `[patch]`
  only in the root manifest of the build it is driving, so those sections are
  inert — and warning-free — when this workspace builds them as dependencies.
* Their dev-dependencies never enter the workspace lockfile, and `--workspace`
  never selects them, so the clippy gate lints exactly what it linted before.
* One thing the vendoring does change: cargo applies `--cap-lints allow` to
  registry sources but not to path ones, so warnings inside these crates are now
  printed during a normal build. There is one at 0.5.42 — an unread
  `Session::buffer_manager` field in grafeo-engine. It is upstream's, it is not
  denied (lint flags after `--` reach only the packages cargo is linting), and
  it is deliberately left alone so the diff against upstream stays about the
  defects.

## Local patches

Every edit is marked in the source with a `TraceDecay patch` comment, so
`rg 'TraceDecay patch' vendor/grafeo` enumerates them.

### 1. An epoch's arena may span more than one chunk

*Files: `grafeo-common/src/memory/arena.rs`.*

`Arena::alloc_value_with_offset` — the allocator behind grafeo-core's
`tiered-storage` feature — only ever allocated into `chunks.first()`, and
`read_at` / `read_at_mut` only ever read from it. `HotVersionRef.arena_offset`
is a `u32`, and upstream read it as an offset inside that one chunk. With
`DEFAULT_CHUNK_SIZE` at 1 MiB and `NodeRecord` at 32 bytes, an epoch could hold
about 32 Ki node records; the next allocation returned
`AllocError::InsufficientSpace` forever after. `LpgStore` hardcodes
`ArenaAllocator::new()`, so no caller could raise the chunk size either.

The `u32` is now a **flat address** over the arena's chunks:
`chunk_index * chunk_size + offset_within_chunk`. Readers divide by the stride
to recover both halves. Consequences:

* Every chunk in `Arena::chunks` must be exactly `chunk_size` bytes, or the
  stride is a lie. A general `Arena::alloc` big enough to need a larger chunk
  now parks it in a new `oversized` list instead, which `alloc` still allocates
  from and which `stats()` and `total_used()` still count. Nothing addresses
  `oversized` by offset, and nothing needs to.
* `chunks` is append-only for the arena's lifetime, so an address stays valid
  exactly as long as it did before.
* A full chunk grows the arena by one chunk rather than failing. The ceiling is
  now the 4 GiB a `u32` can address — 4096 chunks at the 1 MiB default — and
  reaching it is a typed `InsufficientSpace`, not a panic.
* A value too large to sit inside one chunk is still `InsufficientSpace`: an
  allocation may not straddle two chunks, or the flat address would not
  describe it. A type whose alignment does not divide the chunk size is
  `InvalidAlignment` for the same reason.

Upstream's `test_alloc_value_with_offset_insufficient_space` asserted the old
ceiling and is replaced by `test_alloc_value_with_offset_spans_chunks` (the
allocation now succeeds in a second chunk) plus
`test_alloc_value_with_offset_value_larger_than_chunk` and
`test_alloc_value_with_offset_exhausts_flat_address_space`, which keep the two
surviving error paths covered. The regression proper is
`test_alloc_value_with_offset_beyond_one_chunk_of_records`: 98 Ki 32-byte
records into a 1 MiB-chunk arena, every one read back.

### 2. Arena exhaustion is a value, not a process abort

*Files: `grafeo-core/src/graph/lpg/store/node_ops.rs`, `.../edge_ops.rs`.*

`LpgStore::create_node_versioned` and `create_edge_versioned` turned the typed
`AllocError` into `.expect(…)`, so the defect above surfaced as a panic in
`node_ops.rs` rather than as an error a caller could see. Both now delegate to
new `try_create_node_versioned` / `try_create_edge_versioned` methods returning
`Result<_, AllocError>`. The infallible names are kept and still panic, because
their signatures are fixed by the `GraphStore` trait and changing that would
break every implementor in grafeo-engine and grafeo-adapters; their panic
message now carries the typed error.

In the fallible form the arena allocation is hoisted ahead of every store
mutation, so a failure leaves nothing behind but a consumed id — upstream would
have registered the node's labels first.

`batch_create_edges` keeps its panic deliberately: a failure half-way through
the batch has already written adjacency and version state for the earlier
edges, and there is nothing to unwind it with. Its message carries the typed
error too.

### 3. `SectionType::LpgStore` stays `mmap_able: false` — investigated, not changed

*No files changed. This entry records the finding.*

The flag looks like the thing standing between the LPG store and a disk tier.
It is not. Tracing it:

* `mmap_able` is read in exactly one place, `grafeo-engine`'s
  `SectionConsumer::build`, where it becomes `can_spill()` and the guard at the
  top of `spill()`. Nothing else in the three crates consults it.
* `spill()` serializes the section, writes and mmaps a spill file, and then
  hands the `PageFetcher` to `Section::swap_to_mmap`. That is the requirement:
  a section must be able to *serve reads from a mapping*.
* `LpgStoreSection` does not override `swap_to_mmap`, so it inherits the
  default, which returns `SpillError::NotSupported`. Flipping the flag would
  make `can_spill()` claim a capacity the section does not have: the buffer
  manager would pick it for eviction, pay a full serialize plus a file write
  plus an mmap, get `NotSupported` back, delete the file, and free nothing.
* It could not be made to work by overriding `swap_to_mmap` either, not without
  a new on-disk representation. `LpgStoreSection::deserialize` *replays* its
  block format — `create_node_with_id`, `set_node_property`, edge by edge — into
  a live `LpgStore` whose records sit in MVCC arenas and hash chains. There is
  no view-over-bytes to point at a mapping.
* `grafeo-engine/src/database/compact_tiered.rs`, the real disk tier, never
  consults `mmap_able` at all. It is hardwired to `CompactStore`, whose columnar
  codecs hold `Bytes::slice` views into a `Bytes::from_owner(mmap)` — so its
  "deserialize" is a pointer re-base and mmap-backed reads are free. That is why
  `SectionType::CompactStore` is the one data section already flagged
  `mmap_able: true`.

So the disk tier for graph data is reachable today, through compaction into
`CompactStore` — which is what the `graph-disk-tier` feature in
`crates/tracedecay-graph-db` already turns on. `LpgStore` is the hot mutable
overlay in that design, and its flag is correct. Upstream's own
`spill_non_mmap_section_returns_not_supported` test asserts this behaviour.

## Refresh procedure

1. Fetch the new version's `.crate` payloads and expand them over these
   directories, dropping `.cargo-ok` and `Cargo.lock` as the first commit did.
2. Read `git diff` for the four patched files. If upstream has fixed the arena
   addressing or made the LPG store mmap-servable, drop the corresponding patch
   and this entry rather than merging both.
3. Re-record the revision, the retrieval date, and the three checksums above.
4. Verify, in this order:

   ```sh
   (cd vendor/grafeo/grafeo-common && cargo test --features tiered-storage --lib)
   cargo test -p tracedecay-graph-db --features test-helpers
   cargo test -p tracedecay-graph-db --features test-helpers,graph-tiered-storage
   ```

   The second run is the no-default-change gate; the third is the one that
   fails outright on unpatched grafeo.
5. Commit the refresh separately from any re-patching.
