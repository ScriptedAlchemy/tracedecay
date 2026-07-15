# TraceDecay V2 Stable Anchors and Provenance

## Status / Role

Status: active product contract.

Role: PR7 establishes stable evidence anchors for captured observations. Later query,
search, API, and UI slices preserve and resolve those anchors. This plan does not
create a research-management system.

## Outcome

Any authorized result can lead back to the exact retained observation or entity that
supports it. The reference survives ranking changes, project moves, worktree removal,
and index rebuilds, while deletion and retention remain explicit.

## Owns

- `RetrievalAnchorId` identity and resolution semantics.
- Provenance relations such as `captured_from`, `produced`, `observed`, `executed_in`,
  `discussed`, `copied_from`, and `derived_from`.
- Evidence time, source generation, projection watermark, coverage, and drift state.
- Immutable Git evidence coordinates: canonical repository identity; commit,
  tree, and blob object identity; parent/side role; path identity; and retained
  index or worktree-capture watermark when no immutable Git object exists.
- Safe tombstones for expired, redacted, or deleted targets.
- Rules for distinguishing direct authorship from copied coordination text.

## Does not own

- Research manifests, research ledgers, private corpus registries, or subagent rosters.
- Plan validation, progress tracking, compatibility inventories, or implementation
  workflow enforcement.
- Physical storage schema, ranking, scope resolution, authorization policy, transport
  routes, or presentation.
- Embedded transcript payloads or alternate paths around current authorization.

## Required behavior

1. An anchor is a stable opaque ID, not a search query, response handle, rank, file
   path, branch name, timestamp, or content hash.
2. PR7 creates anchors in the same authoritative transaction as the retained sanitized
   observation and its source identity. Retry returns the existing anchor.
3. Each anchor records target kind, canonical owner, native aliases when available,
   occurred and ingested time, source generation, projection watermark, and evidence
   class. It does not copy the target payload.
4. Resolution rechecks current authorization and privacy policy. It never grants access
   because a caller possesses an ID and never leaks an unauthorized target's existence.
5. Resolution reports `current`, `drifted`, `redacted`, `expired`, `deleted`,
   `unavailable`, or `ambiguous` with coverage. It never silently switches owner,
   provider, project, session variant, or source generation.
6. Project moves, aliases, and worktree removal update routing, not anchor identity.
   A retained anchor remains globally routable within its authorized profile.
7. Derived summaries, search documents, graph nodes, and reports retain source-anchor
   lineage. A derived object cannot become its own unsupported evidence source.
8. Copied parent prompts, provider protocol records, and repeated coordination messages
   may be related evidence but cannot establish direct human authorship or child-task
   ownership without provider linkage or an explicit attribution assertion.
9. Retention removes payload access according to policy while preserving the minimum
   safe tombstone needed to explain the target state and prevent ID reuse.
10. Later query slices return anchors for exact results, omissions, and explanations;
    transport and UI layers pass them through without defining another reference type.
11. A Git anchor never treats a branch, tag, symbolic ref, checkout path, or current
    `HEAD` as immutable evidence. PR7 resolves routing inputs to exact retained Git
    objects or a receipt-bound index/worktree capture in the authoritative anchor
    transaction; ref movement cannot change what an existing anchor means.
12. Commit, tree, and blob anchors preserve native object identity and repository
    ownership. Patch hunks use the PR9 `HunkRef`, which references anchored sides (or
    captured mutable-state watermarks) plus native Git diff options and coordinates;
    it does not create a second content or provenance identity.
13. Git provenance, capture/projection watermarks, and later code-index generation
    watermarks remain separate typed evidence. Resolution reports each and any drift;
    path/line similarity cannot silently upgrade mismatched evidence.

## Acceptance

- PR7 tests atomic observation-and-anchor creation, idempotent replay, rollback, native
  alias collisions, copied-prompt attribution, and unauthorized resolution.
- Rebuilding projections preserves anchor IDs and source lineage.
- Moving refs, rewriting a branch, or removing a checkout does not retarget retained
  commit/tree/blob or captured-state anchors; unavailable objects return a safe typed
  state rather than resolving against ambient `HEAD`.
- Moving a project or deleting a worktree does not break a retained project/session
  anchor.
- Redaction, expiry, and deletion return safe typed tombstones with no payload bytes.
- A search result can resolve to its exact source observation after ranking or index
  versions change, with drift and coverage reported.
- Repository search finds no research-ledger, plan-parser, compatibility-inventory, or
  plan-execution requirement in this contract.
