# Workspace Refactoring and API Migration

## Status / role

**Status split (2026-07-26).** The published read-only
`tracedecay_rename_preview` is implemented and reachable through the canonical
graph handler. That active rename-preview capability must not be replanned.
The apply-grade rename journey below is not certified by preview reachability,
and the compatibility-aware API-migration planner/apply journey is **not
implemented** (`ApiMigration` has no source implementation). The plan index
places Plan 34 in the PR11–PR12 band: the planner/apply journey is an
active-band deliverable that PR19 consumes, not a defect-driven exception or a
wholly later plan. Only PR19's temporary-alias deletion slices are SCOPE-OUT
for PR8–PR14 audits.

**Crate-extraction qualification (2026-07-28).** The
`tracedecay_api_migration_plan` and `_apply` names in the tool vocabulary do
not constitute a delivered API-migration implementation. The staged root
extraction in [Plan 12](12-root-compatibility-migration.md) must use ordinary
source moves, narrow typed ports, and explicit compatibility façades; it must
not depend on this planner/apply journey.

The canonical
[`docs/superpowers/plans/2026-07-28-v2-delivery-root-crate-breakup.md`](../../superpowers/plans/2026-07-28-v2-delivery-root-crate-breakup.md)
therefore keeps Plan 34 planner/apply in the PR12 product journey while query,
code-index, projector, port, and route extraction proceed independently under
Plan 12.

The canonical apply mechanism is Plan 09's journaled application edit
transaction, fed by graph-backed preview evidence and followed by the owning
formatter, diagnostics, verification, and optional Plan 36 Git operations.
The currently published `tracedecay_rename_preview` name remains a supported
surface. Other operation/type/module names in implementation history are
examples until separately shipped as public or persisted API; auditors should
verify the journeys and safety properties, not recreate a former refactor
schema registry or file inventory.

The published `tracedecay_rename_preview` surface above is release evidence and
keeps its compatibility obligation. Pure source-only/internal Plan 34
plan/apply request helpers, wire-visible V2 request revisions, and branch-local
V2 migration-plan files, staged mutations, journals, checkpoints, rollback
artifacts, and receipts change in place. Persisted state accepts only its exact
final shape; any other database, store, spool, file, or projection returns typed
`ResetRequired` and requires explicit reset or recreation. No storage reader,
migration, backfill, dual write, or census path exists. An API alias,
deprecation, or wrapper is retained only for an actually independently released
public predecessor. Source moves, PR sequencing, tests, and branch history
alone are not release evidence.

## User outcome

A user can preview and atomically apply either:

1. a symbol-aware workspace rename that preserves one symbol's identity and
   semantics; or
2. a bounded API migration that promotes primary APIs, moves complete consumer
   families, retains evidence-backed compatibility aliases/wrappers, replaces whole
   definitions where needed, and updates selected terminology without
   misrepresenting the change as a rename.

Both journeys fail closed on stale or ambiguous evidence, return a typed
manifest and receipt, and leave either the complete accepted edit or the exact
original workspace. Optional staging/commit is a separate explicit Plan 36
operation; refactoring never autonomously mutates branches, worktrees, refs,
history, tags, or remotes.

## End-to-end production path

### Pure symbol rename

1. `tracedecay_rename_preview` resolves the expected canonical project,
   worktree, node, qualified name, symbol kind, defining file, old name, scope,
   keep/exclude rules, and proposed repository-style name.
2. It classifies definitions, imports, re-exports, qualified/unqualified paths,
   annotations, generic arguments, constructors, patterns, trait declarations/
   implementations/resolved calls, inherent methods/calls, enum variants,
   tests, examples, and bindable documentation references. Text-only prose and
   arbitrary strings are reported but never guessed to be references.
3. The immutable preview records graph/repository revisions, per-file
   preconditions, every site and disposition, impact, hazards, formatter,
   diagnostics, affected tests, verification, and stable-value assertions.
4. The callable rename-apply operation consumes the exact preview ID and digest.
   `dry_run` performs the same revalidation and planning with zero writes.
5. One project-scoped edit transaction revalidates identity and freshness,
   materializes non-overlapping edits, captures all preimages, writes, formats,
   refreshes graph evidence, runs diagnostics and required verification, then
   commits one receipt or restores every preimage.

### Compatibility-aware API migration

1. The callable API-migration planner creates one dependency-ordered family
   using explicit operations for primary promotion, deliberate compatibility
   aliases or wrappers, whole-definition replacement, bound-symbol rename,
   selected terminology or delivery-name replacement, and protected stable
   values. It does not infer an untyped rewrite language.
2. Every compatibility alias/wrapper declares
   `stable_public_contract | temporary`, external consumer, owner, deprecation
   policy, actual release evidence, and—when source-only temporary—the exact
   PR19 deletion condition. A pure source-only or branch-era predecessor is
   replaced directly and is not an alias disposition. Missing required
   disposition blocks apply.
3. Primary production consumers move before old names are restricted to
   approved compatibility boundaries. Type aliases are used only when language
   semantics preserve compatibility; otherwise an explicit wrapper/conversion
   is required.
4. The callable API-migration apply operation consumes the immutable plan and
   digest through the same edit transaction, rollback, diagnostics,
   formatting, and verification path as rename.
5. Replanning may classify operations already satisfied, still pending, or
   invalidated and issues a new preview/digest. It never silently rebases stale
   evidence. Each deliberately sliced apply is atomic for its declared scope.
6. PR19 removes unreleased source-only aliases directly after their internal
   consumers migrate. Branch-era callable aliases change in place.
   Evidence-backed released aliases retain their declared
   compatibility/semantic-equivalence journey, and stable public aliases remain
   thin delegates to the primary implementation.

## Required behavior

### Identity, scope, and hazards

- A bare spelling is never sufficient for apply. Existing symbols carry node
  ID, expected qualified name/kind/file/old name; replacements carry expected
  definition digests.
- Scope cannot escape the canonical project. Symlinks, submodules, vendored or
  generated trees, unsupported macro expansions, and unlinked files are
  reported separately and require explicit supported policy.
- Planning reports callers, references, re-exports, affected files/tests/docs,
  namespace collisions, shadowing or changed resolution, ambiguous symbols,
  unresolved text, macro support, stale/incomplete graph evidence, and naming
  violations.
- Required ambiguous, stale, colliding, unsupported, overlapping, or
  out-of-scope evidence blocks apply. Optional skips require explicit caller
  acceptance and remain visible.
- Analyzer candidates from Plan 35 retain independent provenance. Graph truth
  remains authoritative for clean identity, canonical/historical/cross-project
  relations, bounded traversal, and test attribution. Stale or dirty overlay
  candidates cannot mint durable preview truth without save/replan against
  clean content.
- Plan 35 `prepareRename`/`rename` bind only to read-only candidate/preview
  operations. They never call rename/API-migration apply,
  `workspace/applyEdit`, or an opaque server command. The only admitted PR18
  diagnostic code action is Plan 35's no-edit handoff projection backed by
  Plan 17's public single-use token operation; it opens the owning
  investigation surface and cannot consume a refactor candidate or bypass this
  plan's manifest, transaction, policy, verification, rollback, or receipt.
  No general LSP code-action reservation remains.

### Manifest and dispositions

The typed manifest carries enough immutable identity to reauthorize and
revalidate the preview: operation and digest, project/worktree/repository/graph
revisions, affected file state, scope and keep/compatibility rules, old/new
symbol identity, impact and capability limits, every planned site and expected
change, protected values, and formatter/diagnostics/verification work. This is
a behavioral contract, not a mandate for an exact field or type inventory.

Each known site is exactly one of:

- `changed` — planned bound edit;
- `unchanged` — already correct or an approved compatibility surface;
- `skipped` — deliberately excluded/kept/generated/unsupported/unselected or a
  protected stable value, with reason; or
- `blocked` — unsafe until ambiguity, collision, unsupported required syntax,
  stale state, or invalid naming is resolved.

Markdown and JSON render the same typed result, counts, blocked sites, and
semantic outcome. The manifest is a product preview/receipt, not a generated
repository inventory or PR19 execution ledger.

### Stable-value safety

Wire fields/values, serialized names/discriminators, SQL table/column/index/
migration identifiers, persisted provider/event names, schema epochs,
deletion-lineage identifiers, hash domains/canonicalization labels, protocol
method/tool/command names, contract snapshots, and arbitrary string/byte
literals remain byte-identical by default.

An intentional protected-value change must select exact site IDs and expected
bytes, name the protected category, acknowledge the change, and run its own
verification. Normal provider-neutral source promotion uses
`assert_stable_value` and rolls back on any protected-byte change. Database row
or schema mismatch returns `ResetRequired` under Plan 12; this workflow never
substitutes source edits for stored-data conversion.

### Atomicity, recovery, and Git separation

- Apply acquires one workspace edit lease; verifies project/worktree,
  repository/graph/symbol/manifest/file/mode/scope/keep preconditions; recomputes
  hazards; captures formatter preimages; and journals before replacement.
- The preview records pre-existing diagnostics for affected files. Apply
  distinguishes introduced from pre-existing findings, runs caller-supplied
  verification only through repository command policy, and cannot downgrade a
  required gate without creating a new preview and digest.
- Write faults, formatter or diagnostic regressions, failed verification,
  cancellation, or process restart leave the complete accepted edit or restore
  all original bytes and modes. Transaction recovery closes before another edit
  is admitted.
- Any stale digest, revision, identity, scope, or rule rejects before writes
  and directs the caller to replan.
- Success includes final manifest, file digests, compatibility and stable-value
  results, formatter/diagnostic/verification outcomes, graph refresh status,
  and transaction receipt. Failure returns no success receipt and states
  whether no write occurred or rollback/recovery completed.
- Optional staging/commit reuses the manifest's exact Plan 36 `HunkRef`s and
  canonical `GitIndexTransaction` after workspace success and revalidation.
  This plan owns no second patch/index/receipt model.

## Implementation slices

### Working rename journey

- Upgrade preview to apply-grade identity, scope, site disposition, hazards,
  stable-value classification, impact, and immutable digest.
- Ship `tracedecay_rename_symbol` through the canonical transaction, formatter,
  diagnostics, affected-test, verification, rollback, and receipt owners.
- Support the complete Rust site set listed above and advertise another
  language/symbol kind only after its resolver and edit adapter pass the same
  product journey.

### Working API-migration journey

- Ship family plan/apply with operation dependencies, provider-neutral primary
  promotion, deliberate aliases/wrappers, complete-definition replacement,
  selected terminology cleanup, stable-value assertions, and sliced replan/
  resume.
- The provider-neutral observation journey migrates request, outcome, error,
  function, projection, session-message projection, and projector-version
  source names together while preserving the projector's persisted value.
- PR19 uses this journey to remove unapproved source-only V1/delivery names and
  internal consumers in place, changing branch-era names in place, and
  preserving only independently released public compatibility contracts.

### Supported adoption

- MCP and CLI expose the same typed preview/apply/result semantics only after
  implementation is callable.
- Catalog capability metadata reports supported language/symbol combinations;
  unsupported combinations fail closed with explicit reasons.
- Refactoring workflows compose graph impact, preview, apply, diagnostics,
  verification, and optional Plan 36 staging without copying those engines.

## Replacement and deletion

The working journeys replace manual multi-call edits and any copied resolver,
text replacement, transaction, diagnostics, formatter, catalog, or Git writer.
PR19 deletes unreleased source-only migration wrappers after named internal
consumer migration. Branch-era callable wrappers are removed in place; direct
equivalence tests do not establish publication. Evidence-backed independently
released public aliases remain. There is no general
patch language, autonomous rewrite framework, language-independent regex
fallback, generated-output editor, LSP `workspace/applyEdit` authority, or
source-level dual-write/shadow/lazy store migration.

## Direct acceptance

- Focused Rust cases cover the supported bound-site families, compatibility
  forms, family dependencies, selected terminology changes, protected values,
  unsupported/generated/unlinked syntax, ambiguity/collision/shadowing,
  stale or overlapping evidence, concurrent replan, cancellation/faults, and
  formatter/diagnostics/verification rollback. The suite may evolve with the
  implementation; no Cartesian packet matrix or fixed fixture inventory is
  required.
- Graph-only, analyzer-only, disagreement, stale-analyzer, overlay-vs-clean,
  provenance-preserving dedupe, and cross-project merge fixtures preserve graph
  authority and analyzer provenance.
- Preview and dry-run change zero files. Apply consumes the exact digest and
  rejects every stale precondition before writing.
- Successful rename leaves no in-scope bound old references. Successful API
  migration leaves old names only at manifest-approved compatibility sites and
  protected values byte-identical unless explicitly acknowledged.
- Every injected failure and Linux/Windows crash/restart point proves all
  scoped bytes and modes equal their preimages or the complete accepted edit,
  never a partial workspace.
- Formatting, diagnostics, affected tests, and requested verification pass
  before commit; MCP/CLI typed results agree.
- Optional staging/commit accepts exact revalidated hunks and never performs
  autonomous branch/worktree/ref/history/remote mutation.
- Ordinary aggregate repository checks pass after the end-to-end journeys; no
  separate acceptance gate is created.

## Not in this plan

- Database/schema conversion, reverse cutover, dual write, shadow read, lazy
  migration, or recovery reader. V2 rejects every non-final persisted shape
  with `ResetRequired`; explicit reset or recreation is the only transition.
- A permanent compatibility inventory, migration execution ledger,
  declaration-only scorecard, scaffold-only capability milestone, or
  planning-artifact acceptance gate.
