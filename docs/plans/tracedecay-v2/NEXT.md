# PR7: memory, facts, and provenance

PR6 completed provider coverage and event normalization. PR7 builds the durable
memory slice on top of those sanitized observations: project and profile facts,
their evidence and corrections, curated trust, legacy migration, deletion
lineage, and generation-bound repository provenance anchors that lead any
authorized result back to the exact retained evidence that supports it.

## Current branch status

The current branch adds the PR7 domain fact and anchor contracts, the
`retrieval_anchors`/`retrieval_anchor_aliases`/binding schema with owner-bound
identity and immutability triggers, the `AnchoredObservationWrite` persist path,
the `memory_v2` fact store with assertions, supersession, trust, and proposals,
the `v19`-`v21` migrations, and the repository-provenance evidence path. The
compatibility fact authority carries legacy V1 memory-tool facts forward through
the same owner-bound contract.

This is in-progress work completing the application and store wiring. The
correctness review is not yet closed and the aggregate gates are not yet green.
Pending before completion: the application wiring compiles clean; the acceptance
test matrix from plans 13, 36, and 05 passes; Linux workspace tests, native and
Windows all-feature Clippy, and formatting gates pass; developer-build feedback
evidence is recorded for the changed compilation scope; and clean attested
memory/anchor baselines are indexed. This section is finalized only at
completion; it is accurate now with the pending items marked.

## Product slice

Establish project-wide facts, profile-wide user facts, and stable evidence
anchors for the observations already captured by PR5 and PR6. Facts carry their
supporting evidence, corrections arrive through supersession rather than
mutation, trust and curation are explicit, legacy V1 facts migrate through the
compatibility fact authority, deletion preserves lineage, and every anchor and
fact resolves back to exact retained evidence.

The shared path is:

```text
sanitized retained observation or fact assertion
  -> owner-bound fact/anchor identity in the retaining transaction
  -> immutable evidence, provenance, and repository coordinates
  -> supersession, contradiction, trust, and curation state
  -> daemon-owned atomic commit with idempotent replay
  -> authorization-rechecked resolution to exact evidence or a typed tombstone
```

## Required behavior

- Create every anchor in the same authoritative transaction as the retained
  sanitized evidence and its source identity for that target kind. Exact replay
  returns the existing anchor; a conflicting identity fails without overwriting
  evidence or advancing progress.
- Keep anchor and fact identity opaque. IDs never embed payload bytes and are
  never a search query, transport response handle, collection cursor, rank, file
  path, branch name, timestamp, or content hash.
- Recheck current authorization and privacy policy on every resolution. Possessing
  an ID never grants access and never leaks an unauthorized target's existence.
- Report resolution as `current`, `drifted`, `redacted`, `expired`, `deleted`,
  `unavailable`, or `ambiguous` with coverage. Resolution never silently switches
  owner, provider, project, session variant, or source generation.
- Record typed fact, assertion, evidence, contradiction, supersession, trust,
  curation, and as-of state. A correction supersedes; it does not edit prior
  evidence. Source and privacy-domain identity survive merge and hydration.
- Keep project facts and project sessions project-wide and user facts
  profile-wide. Resolve linked worktrees through canonical project identity;
  missing or ambiguous authority fails closed without a fallback store.
- Carry legacy V1 memory-tool facts forward only through the compatibility fact
  authority, preserving V1 tool behavior and mapping without a second fact model.
- Record generation-bound repository provenance as evidence only: canonical
  repository identity, checkout/worktree identity, canonical root, current ref
  when attached, HEAD object ID, index tree identity when available, path
  identity, dirty-state classification, and capture time. Missing, unborn,
  detached, conflicted, or partially readable state is explicit, never guessed.
- Treat refs, tags, symbolic refs, checkout paths, and ambient `HEAD` as routing
  inputs only. Resolve them to exact retained commit, tree, and blob objects or a
  receipt-bound index/worktree capture in the anchor transaction; ref movement
  never changes what an existing anchor means. PR7 copies no Git objects and adds
  no status, diff, staging, or commit tool.
- Retain source-anchor lineage on derived summaries, search documents, graph
  nodes, and reports. A derived object cannot become its own unsupported evidence
  source, and path or line similarity never upgrades mismatched evidence.
- Treat copied parent prompts, provider protocol records, and repeated
  coordination messages as related evidence only. They cannot establish direct
  human authorship or child-task ownership without provider linkage or an
  explicit attribution assertion.
- Remove payload access on deletion, redaction, or expiry according to policy
  while preserving the minimum safe tombstone and deletion lineage needed to
  explain the target state and prevent ID reuse.
- Commit every fact, assertion, anchor, provenance, and lineage write atomically
  through the already-open daemon authority. No client, hook, curation surface,
  or recovery path opens another writer.
- Record bounded fact-write, anchor-create, resolution, replay, and migration
  baselines for later PR20 comparison. A severe regression or unbounded path
  found here is fixed here, not deferred.

## Direct tests

- atomic observation-and-anchor creation, idempotent replay, rollback, native
  alias collisions, copied-prompt attribution, and unauthorized resolution;
- provenance preservation, contradiction, supersession, as-of knowledge, denied
  payloads, redacted frontiers, and unknown denominators through fact merge and
  hydration;
- rebuilding projections preserves anchor IDs and source lineage, and a search
  result resolves to its exact source observation after ranking or index versions
  change with drift and coverage reported;
- moving refs, rewriting a branch, or removing a checkout does not retarget
  retained commit/tree/blob or captured-state anchors; unavailable objects return
  a safe typed state rather than resolving against ambient `HEAD`;
- repository provenance records clean, dirty, detached, unborn, conflicted, and
  partially readable state explicitly and copies no Git objects;
- moving a project or deleting a worktree does not break a retained
  project/session or fact anchor;
- redaction, expiry, deletion, unavailable, and ambiguous targets return safe
  typed tombstones with deletion lineage and no payload bytes;
- legacy V1 fact migration through the compatibility fact authority preserves
  tool behavior, mapping, and history coverage without a second fact model;
- transport response handles and collection cursors cannot substitute for anchor
  resolution in fixtures or product contracts;
- daemon-only writer, missing daemon, stale authority, ambiguous scope, linked
  worktree, and concurrent-client cases without another database writer;
- repository search finds no research-ledger, research-manifest, plan-parser,
  compatibility-inventory, or plan-execution requirement in this slice;
- stock Linux and Windows format, compile, Clippy, focused, and workspace tests.

## Prohibited scope

- no research-management system, research manifest, research ledger, private
  corpus registry, or subagent roster;
- no new observation schema, sanitizer, database authority, or writable store
  beyond the owner-bound fact/anchor and compatibility fact contract; no
  client-side, hook-side, or recovery writer;
- no Git object database, status, diff, history, blame, staging, or commit tool,
  and no autonomous branch, worktree, ref, or published-history mutation;
- no transport response handle, collection cursor, rank, or path treated as
  durable evidence identity;
- no PR8 temporal/LCM retrieval, PR9 code indexing or read-only Git intelligence,
  PR10 semantic ranking, PR11 policy/application/index-mutation surface, PR12
  CLI/MCP/HTTP/LSP surface rewrite, or PR13 hook cutover;
- no GitHub API ingress, review-thread or comment ingestion, comment writes, or
  CI execution authority; those evidence classes belong to PR13.

## Done

PR7 is complete when every plan 13, 36, and 05 PR7 acceptance passes: anchors and
facts are created atomically with the retained evidence they support; idempotent
replay, supersession, trust, contradiction, and as-of knowledge are gap-free;
authorization is rechecked on every resolution; ref movement, project moves, and
worktree removal never retarget an anchor; deletion, redaction, and expiry return
safe tombstones with lineage; legacy V1 facts migrate through the compatibility
authority; repository provenance is generation-bound evidence only; no fact,
anchor, or curation path retains another durable writer; the workspace, Clippy,
and formatting gates pass; developer-feedback evidence is recorded for the changed
compilation scope; and clean attested memory and anchor baselines are indexed.
