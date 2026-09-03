# Workspace Refactoring

## Status / role

The published read-only `tracedecay_rename_preview` capability remains active
and supported. This plan owns its graph-backed evidence contract and the
apply-grade rename outcome still required for final V2.

The unreleased semantic plan/apply pair formerly described here is rejected and
removed. Tool vocabulary, typed requests, handlers, catalogs, tests, and plugin
guidance do not advertise it. Coordinated public API changes use explicit
ordinary source edits, graph impact evidence, diagnostics, affected tests, and
review. A compatibility alias or wrapper is retained only when an
independently released predecessor proves that obligation.

Plan 12 crate extraction continues to use ordinary source moves and narrow
typed ports. It does not depend on a general semantic rewrite engine.

## User outcome

A user can inspect a symbol-aware workspace rename preview that preserves exact
symbol identity and reports every known site, hazard, affected file, and test.
The future apply-grade journey must fail closed on stale or ambiguous evidence
and leave either the complete accepted edit or the exact original workspace.

Optional staging and commit remain separate Plan 36 operations. Refactoring
never autonomously mutates branches, worktrees, refs, history, tags, or
remotes.

## End-to-end production path

1. `tracedecay_rename_preview` resolves the expected canonical project,
   worktree, node, qualified name, symbol kind, defining file, old name, scope,
   keep/exclude rules, and proposed repository-style name.
2. It classifies definitions, imports, re-exports, qualified and unqualified
   paths, annotations, generic arguments, constructors, patterns, trait
   declarations and implementations, resolved calls, inherent methods, enum
   variants, tests, examples, and bindable documentation references.
3. Text-only prose and arbitrary strings are reported but never guessed to be
   references.
4. An apply-grade preview records graph and repository revisions, per-file
   preconditions, every site and disposition, impact, hazards, formatter,
   diagnostics, affected tests, verification, and protected stable values.
5. A callable rename operation consumes the exact preview identity and digest.
   Dry-run performs the same revalidation and planning with zero writes.
6. One project-scoped edit transaction revalidates freshness, captures all
   preimages, writes, formats, refreshes graph evidence, runs diagnostics and
   verification, then commits one receipt or restores every preimage.

## Required behavior

- A bare spelling is never sufficient for apply. Symbols bind node ID,
  qualified name, kind, defining file, and expected old name.
- Scope cannot escape the canonical project. Symlinks, submodules, vendored or
  generated trees, unsupported macro expansions, and unlinked files are
  reported separately and require explicit supported policy.
- Planning reports callers, references, re-exports, affected files and tests,
  namespace collisions, shadowing, changed resolution, ambiguous symbols,
  unresolved text, macro support, stale evidence, and naming violations.
- Required ambiguous, stale, colliding, unsupported, overlapping, or
  out-of-scope evidence blocks apply.
- Graph truth remains authoritative for clean identity, relations, bounded
  traversal, and test attribution. Dirty analyzer candidates cannot mint
  durable preview truth without save and replan against clean content.
- Plan 35 `prepareRename` and `rename` bind only to read-only candidate and
  preview operations. They never call an edit or `workspace/applyEdit`.

Each known site has exactly one typed disposition:

- `changed` — a planned bound edit;
- `unchanged` — already correct or an approved compatibility surface;
- `skipped` — deliberately excluded, generated, unsupported, or protected,
  with a reason; or
- `blocked` — unsafe until ambiguity, collision, unsupported required syntax,
  stale state, or invalid naming is resolved.

Markdown and JSON render the same typed result, counts, blocked sites, and
semantic outcome.

## Stable-value safety

Wire fields and values, serialized names, SQL identifiers, persisted provider
and event names, schema epochs, hash domains, protocol names, contract
snapshots, and arbitrary string or byte literals remain byte-identical by
default.

An intentional protected-value change selects exact site identities and
expected bytes, names the category, acknowledges the change, and runs its own
verification. Persisted shape mismatch returns `ResetRequired` under Plan 12;
source edits never substitute for stored-data conversion.

## Atomicity and recovery

- Apply acquires one workspace edit lease; verifies identity, revisions,
  symbol, manifest, file, mode, and scope preconditions; captures formatter
  preimages; and journals before replacement.
- Apply distinguishes introduced diagnostics from existing findings and runs
  caller-supplied verification only through repository command policy.
- Write faults, formatter or diagnostic regressions, failed verification,
  cancellation, or restart leave the complete accepted edit or restore all
  original bytes and modes.
- A stale digest, revision, identity, scope, or rule rejects before writes and
  directs the caller to replan.
- Optional staging and commit reuse Plan 36 authority after workspace success.
  This plan owns no second patch, index, or receipt model.

## Direct acceptance

- Focused tests cover bound site families, protected values, unsupported and
  generated syntax, ambiguity, collision, shadowing, stale and overlapping
  evidence, cancellation, write faults, and verification rollback.
- Preview and dry-run change zero files; apply consumes the exact digest.
- Successful rename leaves no in-scope bound old references.
- Every injected failure and crash/restart point proves exact preimages or the
  complete accepted edit, never a partial workspace.
- MCP and CLI typed results agree.
- Ordinary repository checks pass without a separate acceptance gate.

## Not in this plan

- A general semantic rewrite language or autonomous migration framework.
- Database conversion, dual write, shadow read, lazy migration, or a recovery
  reader.
- A permanent compatibility inventory, execution ledger, scaffold-only
  capability milestone, or planning-artifact acceptance gate.
- LSP edit authority or autonomous Git mutation.
