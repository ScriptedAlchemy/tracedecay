# Design: share a sealed graph between worktrees at the same tree

Status: proposed. Tracks issue #2402. No runtime behavior changes with this
document.

## Problem

A linked worktree whose tree is byte-identical to a tree the primary already
sealed shares the text artifact, but seals its own full graph. On a
tree-sitter clone (#2402) that is 25.0 MB of a 26.3 MB increase, 95% of what
the worktree costs. On this repository's own clone it is about 1.8 GB per
linked worktree.

## Why the bytes differ today

- Linked worktrees share one project graph database, but each code scope
  projects into its own namespace. `CanonicalCodeGraphStoreLeaseV1`
  (`tracedecay-runtime-core/src/shard_runtime/registry/graph.rs`) keeps the
  namespace exact to the repository, worktree, and ref.
- Entity rows are minted without the worktree. Relation rows are not: a
  sealed relation's endpoints are `GraphEntityRef { projection, identity }`,
  and the row digest hashes that projection. So a container sealed in the
  primary's namespace has a different row sum than a cold build of the same
  tree in the worktree's namespace.
- The layered machinery can already serve a generation as a hard-linked base
  container plus a small delta (`GraphLayeredRowSpill`, `SealedLayer` in
  `tracedecay-graph-db/src/sealed_layer.rs`). It refuses a base from another
  projection (`GraphLayeredRowSpill::create`). On read, it rebuilds base
  relation endpoints with the layer's own projection
  (`generation_relation(self.projection, ..)`), so stored base relation rows
  are already read projection-relative.

## What was rejected, and why

- **#2440, adopt a sibling scope's generation.** Closed as "not in this
  shape". It scanned the sibling scope's files with `std::fs`, outside the
  store lock and validation, and skipped read errors. That is a second
  authority for which generation a worktree serves. It also regressed
  `readers_of_a_build_waiting_for_memory_do_not_spin_the_worker` and never
  evicted its shared map.
- **One content namespace shared by identical scopes.** Breaks the
  per-scope namespace invariant and exact worktree snapshot authority.
- **Namespace-free relation row identity** (proposed in #2402). Correct in
  shape, but it changes the canonical relation-row digest for every graph
  projection. Every sealed graph rebuilds and journal-replay digests move.
  AGENTS.md treats this as a byte-exact identity contract. That cost is out
  of proportion to the saving, so this design avoids it.

## Proposed shape: a foreign base, re-attested under the layer's projection

Keep every identity contract. Let the worktree's generation layer over the
primary's sealed container, and prove the base rows under the worktree's
own projection once, at seal time.

1. **Lookup through the store authority.** When a worktree generation is
   about to seal cold, ask the publication store, under its lock and
   validation, for a sealed, proven generation in the same project graph
   database whose decoded-content digest equals this generation's. That
   digest is the authority `SharedDecodedContentPoolV1` already uses to share
   decoded content across worktrees. No filesystem scan of another scope.
2. **Re-attest the base.** Recompute the base's row sum with relation
   endpoints in the worktree's projection: one streaming pass over the base
   row index, hashing rows, with no segment decode and no re-extraction.
   Record it in the layer receipt as the base row sum *under this
   projection*, next to the base's own projection. The recovered digest is
   still `recovered_digest_from_row_sum(worktree identity, row sum)`, so it
   equals what a cold build in the worktree's namespace records.
3. **Delta is the marker.** The delta carries only the worktree's
   generation-marker entity and hides the base's marker. Every other row is
   read from the hard-linked base container through the existing
   `SealedLayer` read path, which already rebuilds endpoints in the layer's
   projection.
4. **Retention is the hard link.** The layer pins the base container,
   attachment, and row index by hard link from creation
   (`GraphLayeredRowSpill::create`). Retiring the primary's generation does
   not free the bytes while a worktree layer holds them, and the last layer
   to retire frees them. There is no shared in-memory map to evict.

What changes in the code:

- `SealedBaseReceiptV1` records the base's projection and the re-attested
  row sum.
- `GraphLayeredRowSpill::create` and `SealedLayer::open` accept a base from
  another projection in the same graph database only when the receipt
  carries a re-attested row sum for the layer's projection. Without it they
  still refuse.
- The code-index seal path tries the foreign-base layer before a cold seal.
  If no match exists, or re-attestation fails, it seals cold. The failure is
  logged and typed, and the result is never a partial graph.

Expected cost for the #2402 case: about 0 MB of graph container for the
worktree (hard links), plus the delta and receipt. The re-attestation pass
cost is a hypothesis to measure with Hotpath. It should be bounded by one
hash per base row, far below a cold seal's extract and encode.

## Risks

- **Correctness of re-attestation.** If a base row's projection-relative
  encoding differs from what a cold build would emit in the worktree's
  namespace in any way other than relation endpoints, the digests diverge.
  The acceptance test below compares against a cold build, so a divergence
  fails loudly instead of serving wrong rows.
- **Cross-scope lock order.** The lookup reads another scope's publication
  under the store lock. It must take the same lock order as existing
  cross-worktree reads, so it does not add a deadlock edge with #2797.
- **Layer depth.** A worktree layer is itself a base for that worktree's
  next refresh. The existing changed-file-share rule
  (`LAYERED_MAX_CHANGED_FILE_SHARE_DENOMINATOR`) already re-seals cold when
  a delta grows past one eighth of the files, so the depth stays bounded.

## Minimal safe first step

The graph-db half, with one production caller in the same change:

1. `SealedBaseReceiptV1` gains the base projection and the re-attested row
   sum. `GraphLayeredRowSpill::create` accepts a foreign-projection base
   only with that row sum.
2. The code-index seal path uses it only for the exact case in #2402: a
   linked worktree whose decoded-content digest matches a proven sealed
   generation of the same repository in the same graph database. Every
   other case keeps today's cold seal.

Acceptance, as a public journey on the layered refresh fixtures:

- Seal the primary, `git worktree add` at the same tree, seal the worktree.
- The worktree generation's base container is a hard link to the primary's
  container (same inode), and its delta rows are only the marker.
- The worktree's recovered digest, rows, and query answers equal a cold
  build of the same tree in the worktree's namespace.
- After an edit in the worktree, its next generation diverges from the
  primary's and the primary's answers do not change.
- Retiring the primary's generation leaves the worktree's answers
  unchanged. Retiring the worktree frees the container bytes.
