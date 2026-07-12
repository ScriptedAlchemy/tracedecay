# ADR-002: Deterministic Identity and Evidence Vocabulary

## Status
Accepted for V2 Phase 0.

## Context
Path hashes, provider IDs, row IDs, response handles, and project attribution currently risk becoming competing identity or evidence.

## Decision
Stable source identities use versioned namespaced deterministic derivation over canonical eligible keys. Ambiguous real-world entities use persisted UUIDv7 allocation requests and an allocation/alias ledger; they are never minted from ambient CWD or transport strings. Explicit selectors bypass ambient defaults after access validation. `ScopeSelectorV2` and one pinned `ScopeResolutionV2` serve every surface.

The locked evidence vocabulary distinguishes immutable observations, canonical events/entities, bitemporal relation assertions, provenance, confidence, supersession, occurrence versus logical copy, visible versus hidden coverage, exact/inferred/correlated/temporal relations, and stable retrieval anchors. Correlation is never rendered as causation. Provider-exposed reasoning summaries may be retained under sensitivity policy; hidden chain-of-thought is neither captured, requested, reconstructed, inferred, nor represented.

Sessions, agents, Turns, messages, goals, workflows, initiatives, plans, and tasks are canonically profile-activity entities. Repository/project/worktree/ref attribution is temporal relation evidence, so an activity item can relate to zero or many projects without copying identity. Deterministic source/observation keys include source instance, rewrite generation, native position, parser/canonicalization versions, and privacy-domain-safe fingerprints. Unknown sanitized fields remain lossless evidence but gain no query meaning until registered.

## Rejected alternatives
- Canonical path hashes, database row IDs, provider strings, or response handles: unstable across moves, imports, and expiry.
- Required primary project IDs on activity: loses cross-project and unattributed truth.
- A fabricated global sequence: independent shard/source progress is a vector watermark.
- Capturing or reconstructing hidden reasoning: violates the evidence and privacy boundary.

## Compatibility, rollback, and removal gates
V1 IDs enter as aliases/provenance only. Migration retains allocation/remap ledgers and tombstones, proves dependent LCM/relation edges follow remapped IDs, and preserves ambiguous candidates. V1 identity derivation and fallback disappear only after every public caller consumes generated IDs/scope contracts and rename/worktree-deletion/restore fixtures pass.