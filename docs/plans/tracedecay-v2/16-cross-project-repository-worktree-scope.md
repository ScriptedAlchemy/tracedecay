# TraceDecay V2 Cross-Project and Worktree Scope

## Status / role

Status: active product plan.

PR15 delivers one authorized multi-root journey across query, CLI, MCP, HTTP,
LSP, inventory, and safe native Git integration. This plan owns scope identity,
resolution, and coverage. [Plan 36](36-git-aware-change-context-and-index-transactions.md)
owns Git preflight, apply, receipts, and recovery over the exact resolved scope.

Earlier collection, stack, resolver, fixture, and migration artifact names are
historical evidence, not prerequisites or mandatory recreation targets.
Only actually independently released public selectors retain protocol
compatibility; persisted collection or stack records use the fresh-store rule.
All other retention is evaluated through the direct scope, inventory, Git,
platform, and regression behavior below.

No Plan 16 multi-root selector, collection, or stack contract is established on
`origin/master` or in a published package/release. Pure source-only/internal
selector helpers, wire-visible V2 selector revisions, and branch-local V2
collection/stack records finalize in place. Only their exact final persisted
shape is accepted; any other database, store, spool, file, or projection
returns typed `ResetRequired` and requires explicit reset or recreation. No
storage reader, migration, backfill, dual write, or census path exists.
Single-root behavior proven on released surfaces remains supported.

**Local identity correction (2026-07-26).** Repository-level project/store
identity now collapses a linked worktree onto its primary checkout through
`repository_identity_root` at the single `default_profile_project_id` minting
door. This does not merge worktree snapshot authority: exact worktree/ref
identity still selects graph generations. Durable profiles also refuse roots
under the OS temporary directory at `upsert_code_project`, and registry reaping
retains a dead-looking project row whenever its store directory still contains
data; it deletes registry rows only, never stores or session artifacts.

## User outcome

A user can name several repositories or worktrees directly or through a saved
`QueryCollection`/`WorkspaceCollection`, query them as one bounded scope,
continue paging over the same immutable snapshots, use the same roots in a
multi-root editor, inspect and safely clean up the authorized worktree
inventory, resolve an explicit branch stack, and select one exact
source/destination pair for safe Git preflight and apply.

Every result identifies its owning project and code snapshot. The user never
has to change CWD, know a store location, or trust a display name to reach the
right data.

## Worktree index identity and reuse

Plan 16 composes the incremental index delivered by Plans 25/31; it does not
create a second watcher, parser, chunker, vector store, or scheduler.

- Every linked worktree has an exact typed identity, fenced snapshot frontier,
  immutable code generation, freshness, and coverage. A shared Git common
  directory, object ID, branch name, path, or identical bytes never merges
  worktree or generation authority.
- Native `gix` worktree/index/tree reconciliation is the changed-path
  authority. Debounced filesystem events only wake that reconciliation.
  Dropped-event, rebase, checkout, reset, index-only, detached-head, and
  worktree-removal cases advance state only after a complete bounded
  reconciliation.
- Content-addressed parse, chunk, lexical, and vector artifacts may be reused
  across authorized worktrees when every content/descriptor/sanitizer/privacy/
  projection input matches. Logical occurrence, lineage, publication,
  authorization, cursor, and active-generation identities remain per
  worktree snapshot.
- One worktree's edit invalidates no other worktree. Query fans out over the
  latest complete compatible generation per selected root and reports roots
  that are indexing, stale, unavailable, or conservatively rebuilding.
  Semantic projection never blocks exact/lexical/graph results from ready
  roots.
- Scheduling is bounded and fair by worktree: interactive roots and queries
  receive priority without starving background roots, superseded snapshots are
  cancelled before publication, and queue/resource coverage remains explicit.

## End-to-end production journey

1. The user supplies explicit typed project, repository, worktree, ref, or
   commit targets, or selects one immutable revision of a saved
   `QueryCollection`/`WorkspaceCollection`. Collection members are references
   to existing `ProjectId`, `RepositoryRootId`, or verified projectless
   `UserProfileId` authorities; collection ownership or membership never grants
   project, root, worktree, stack, or code access. A path, CWD,
   workspace-folder URI, display name, host label, provider key, remote URL,
   branch label, Git common directory, collection title, or store name may
   locate candidates but is never identity or authority.
   Multi-token lookup evaluates tokens, aliases, credential-free remotes,
   paths, and verified repository relationships independently; failure of one
   combined string does not imply that projects are unregistered. `current`
   is valid only when the request contains no explicit target and the response
   always names the resolved project and code snapshot.
2. The application resolver freezes the selected collection revision and
   canonical member order when present, then proves each
   `ProjectId`/`RepositoryId`/`WorktreeId`/`BranchRef`/`CommitId` relationship,
   independently authorizes the requested capability per member and exact
   worktree, pins immutable code snapshots and generations, and freezes the
   ordered roots into one scope digest. Project-data, code-snapshot,
   worktree-inventory, stack-read, Git-preflight, and Git-apply grants remain
   separate. An explicit target resolves, returns policy-safe disambiguation,
   or fails; it never falls back to CWD, the active checkout, the first
   workspace folder, a cached project, all registered projects, or an
   empty/newest store or collection.
3. Query fans out only after exact per-root authorization. It
   bounds concurrency and cost, merges results deterministically while
   preserving per-root provenance, and returns searched, stale, unavailable,
   redacted, denied-without-enumeration, and truncated coverage. Partial
   execution is never rendered as complete. Selection, retrieval,
   fusion/deduplication, hydration, and coverage remain distinguishable.
   Heterogeneous raw scores remain incomparable unless a versioned
   compatibility and held-out calibration profile permits comparison;
   deterministic rank fusion preserves per-shard rank/provenance otherwise.
   Every numeric assessment states whether it is an ordinal rank, heuristic
   score, calibrated probability, or calibrated interval together with its
   producer, scale/revision, evidence, cohort where applicable, and coverage.
4. A distributed cursor binds the immutable ordered root set, scope and grant
   digests, collection ID/kind/revision and membership digest when used,
   canonical member/root orders, per-root
   snapshot/generation/continuation vector, query/fusion profile, last ordering
   key, expiry, and safe coverage summary. Every page reauthorizes every root.
   Membership, root, policy, grant, or generation drift returns stale or
   revoked; continuation never adds, removes, reorders, or silently advances
   roots to latest.
5. Stable session, message, entity, and retrieval anchors route to their owning
   project or user-profile authority globally. Project facts, sessions,
   messages, and LCM stay project-wide across branches/worktrees while code
   queries remain snapshot-specific. Bridge edges require explicit package or
   dependency metadata, verified repository relationships, canonical external
   identity, or the selected scope with endpoint generations and provenance;
   names, paths, similarity, host, and co-occurrence never merge authority.
6. The LSP gateway resolves every workspace folder through the same resolver.
   Each document, analyzer process, graph generation, diagnostic, and code
   action remains bound to its exact owning folder. Nested, ambiguous, denied,
   stale, or unsupported folders remain explicitly unavailable and cannot
   borrow another folder's analyzer or graph state.
7. The daemon reads native Git worktree administration records for each
   explicitly authorized repository and returns only the exact authorized
   worktrees. Inventory includes main/linked/bare kind, branch/ref, head,
   dirty/index/operation summary, detached/unborn/locked/prunable/admin state,
   holders, related sessions/PRs, association evidence, freshness, and
   coverage. It does not
   recursively scan parent directories, infer identity from paths, or expose
   hidden sibling labels, paths, counts, ordinals, or absence-versus-denial
   distinctions. An incomplete reconciliation cannot advance the complete
   inventory epoch, retire a worktree, treat a missing path as deletion, or
   claim complete inventory.
8. The same inventory supports explicit safe cleanup. Cleanup first pins a
   read-only inspection of exact worktree identity and evidence. Dirty or
   untracked files, holders, unpushed/unmerged commits, open or uncertain PRs,
   shared refs, ambiguity, stale evidence, or missing capability block it.
   Immediately before mutation the daemon re-proves identity and blockers and
   may remove only the exact worktree registration/root, never its branch.
   Crash or uncertain outcome remains reconciliation-required; missing path
   alone never proves success.
9. The user may bind visible inventory entries to an explicit
   `BranchStackId` revision. The reference-only branch-stack projection retains
   typed `BranchStackId`, `BranchStackRevisionId`, `StackNodeId`,
   `WorktreeInventorySnapshotId` and `WorktreeInventoryEpoch`, explicit acyclic
   dependency edges, canonical ordering, repository/ref/tip/worktree proofs,
   inventory epoch, and source as an explicit declaration or accepted task
   branch topology. Duplicate refs, self/cross-repository/missing-node edges,
   cycles, and ref/tip mismatch reject publication.
   It supports stacks with absent worktrees or no PR but never infers an edge
   from tracking refs, branch names, commit messages, paths, graph proximity,
   pull-request bases, or provider ordering. Partially visible topology cannot
   expose hidden nodes, invent transitive edges, or imply readiness.
   `AllAuthorized` is available only for stack read; preflight/apply name the
   exact nonempty source/destination node set and independently authorize every
   node and worktree.
   Authorized provider review-topology observations may join the projection
   for display and drift evidence but never create, remove, or reorder nodes or
   edges.
   Plan 37's PR15 stack fanout freezes this exact visible revision and
   reauthorizes every source and recipient at enqueue, delivery, and expansion.
   Scope resolution supplies bounded deterministic batches and truthful
   hidden/denied/partial coverage; it never broadens a recipient set during
   debounce, overflow drain, circuit-breaker recovery, or restart.
10. From the visible inventory or authorized stack revision, the user selects
   an exact source and destination
   in one repository. The resolver freezes both worktrees, refs, commits,
   snapshots, explicit independent-branch proposal or declared stack edge,
   inventory/stack revision, capabilities, grant, and policy into a
   Git-operation scope.
   `NativeIntegrationPreflight` is separate from
   `NativeIntegrationApply`; read or inventory access implies neither. Plan 36
   previews without mutating real Git state and applies only the exact,
   separately approved preview after full revalidation.
11. Query results, exact anchor loads, LSP diagnostics, inventory/stack
   entries, cleanup outcomes, Git
   previews, and receipts retain owning project/repository/worktree and
   generation identity. Exact load routes from that identity, never from CWD
   or collection identity.
12. The optional default collection and Plan 20 source bindings enter this
   same path as explicit selectors. Default selection never outranks an
   explicit target or widens authority. Source-binding dry-run freezes exact
   target, locator, scope, membership, and authorization; apply re-resolves and
   fails closed on ambiguity, expiry,
   revocation, or drift. Missing and denied explicit targets share a
   policy-safe unavailable shape unless all disambiguation candidates are
   visible.

## Identity and authority rules

- `RepositoryId`, `ProjectId`, and `WorktreeId` are opaque,
  non-interchangeable product identifiers. `BranchRef` binds a validated full
  native refname to one repository; `CommitId` binds an object format and
  object ID to one repository.
- Repository identity is assigned only after native Git relationship proof.
  Two clones remain distinct unless a separately authorized relationship is
  explicitly proven. A moved worktree retains identity after reproof; a
  deleted-and-recreated worktree receives a new identity even at the same path.
- Project facts, sessions, messages, and LCM remain project-wide across
  branches and worktrees. Code queries select exact worktree/ref snapshots.
  Account-wide and verified projectless Hermes data retain
  `UserProfileId` authority; no synthetic project or worktree-local fallback
  database is created.
- Project moves and worktree deletion preserve project/session/fact identity
  and time-qualified locator provenance. Related-project suggestions are
  bounded evidence only; expansion requires explicit caller action and a newly
  authorized frozen scope. Plan 37 feedback and concurrent-agent proximity use
  this same resolver and cannot create private scope or authority expansion.
- Project-data read, code-snapshot read, inventory read, Git preflight, and Git
  apply are independently authorized capabilities. A project grant never
  expands to all repositories or worktrees, and a repository grant never
  enumerates hidden worktrees.
- `QueryCollection` and `WorkspaceCollection` remain persisted,
  reference-only, immutable-revision selectors with canonical member ordering,
  CAS publication, optional defaults, and source-binding integration. Only
  their exact final persisted shape is admitted; any older or malformed
  saved-set returns `ResetRequired` and requires explicit reset or recreation.
  They may store
  references and display metadata, never copied project/source data,
  credentials, capabilities, paths, store locators, nested collections, or
  authorization.
- The branch-stack registry remains a reference-only projection used by
  inventory, read-only topology, Plan 37 proximity, and Plan 36 preflight/apply.
  Registry publication validates exact repository/ref/commit/worktree
  relationships and explicit acyclic edges. Stack identity, membership, or
  ordering never replaces Git identity or grants scope.
- Cross-project entities retain their owner and generation. Exact canonical
  evidence may deduplicate; similar names, paths, text, embeddings, hosts, or
  co-occurrence never merge identity or create bridge authority.
- Every terminal state carries policy-safe coverage. Candidate details are
  shown only when the caller may see every candidate. Hidden roots do not
  affect public labels, counts, telemetry, ranking statistics, or reason
  splits.

## Implementation slices

1. **Resolve real multi-root requests and saved scopes.** Extend the existing
   application resolver and supported surfaces to accept explicit target lists,
   `QueryCollection`/`WorkspaceCollection` revisions, optional defaults, and
   source bindings; prove typed relationships, authorize every member and
   capability, pin snapshots, and return one frozen scope plus truthful
   coverage. Fold final-shape identity/collection persistence into this
   callable path; legacy path-, alias-, provider-key-, and store-name routes
   remain unresolved locator evidence unless canonical/native proof succeeds.
2. **Execute federated query and exact continuation.** Feed the frozen scope
   directly to Plan 05 query execution, preserve root provenance and owner
   anchors, preserve score semantics, hydrate exact results, and bind
   pagination to the immutable collection/member/root vector. Unavailable or
   denied roots cannot contaminate healthy roots or be hidden behind a complete
   result.
3. **Serve multi-root LSP, inventory, and cleanup.** Bind workspace folders and
   documents to the same scope result, expose the complete/partial authorized
   native worktree inventory through the shipped
   application/CLI/MCP/HTTP/dashboard surfaces, and admit cleanup only from an
   exact fresh safe inspection with reconciliation for uncertain outcomes.
4. **Resolve stacks and hand exact scope to Git.** Publish and resolve
   reference-only stack revisions from explicit topology and native inventory
   proof, intersect nodes with exact grants, and pass only the frozen
   source/destination or declared visible edge to Plan 36. A stale scope,
   inventory, stack, ref, or policy requires a fresh resolution and preview;
   no consumer may rediscover roots or infer edges.
5. **Preserve released protocol compatibility on the production path.** An
   actual independently released explicit-target, default, source-binding,
   inventory, stack, or transport protocol may delegate to the shared resolver.
   Source-only and branch-era shapes change in place. Persisted saved sets
   always use final-shape admission; a mismatch returns `ResetRequired` rather
   than being converted, retained, or quarantined.

## Replacement and deletion

- Remove per-surface scope resolution, CWD/default-checkout fallback, store-path
  routing, and any query or LSP code that silently chooses the first root.
- Remove standalone delivery milestones whose only result is a collection or
  stack schema, port, table, fixture framework, or declaration.
  Retain the complete collection and branch-stack capabilities above by
  implementing their necessary storage/adapters inside the first production
  resolver, query, inventory, cleanup, or Git journey that calls them.
- Remove recursive worktree discovery and hidden-root counts. Inventory is a
  filtered observation of native Git state, not a discovery-based grant.
- Keep only independently released public protocol entry points as supported
  delegates to the shared resolver. Persisted saved sets have no compatibility
  route and share the canonical final-shape admission and authorization logic.

## Direct acceptance

One end-to-end test starts with explicitly authorized roots containing same-name
repositories, linked worktrees, nested editor folders, and one denied sibling.
It proves that CLI/MCP/HTTP/UI query fanout and LSP resolve the same immutable
root vector; pagination survives restart without scope drift; exact result
anchors load without CWD changes; the inventory exposes only authorized
worktrees; dirty, stale, rebuilding, unavailable, and denied roots produce
truthful policy-safe partial coverage; and no document, diagnostic, result, or
count crosses root authority.

The journey exercises explicit roots, immutable query/workspace collection
revisions, optional default and source-binding resolution, authorization change
between pages, score semantics, and exact owner-anchor loading. It inventories
linked/detached/unborn/locked/prunable worktrees, safely cleans one eligible
worktree without deleting its branch, reconciles a crash outcome, publishes and
resolves an explicit stack revision without exposing a denied node, and rejects
inventory/topology drift.

The same journey then selects one exact independent pair or visible declared
stack edge, receives a
non-mutating Plan 36 preflight, rejects stale scope or changed Git state, and
applies only an exact separately authorized eligible preview with a durable
terminal receipt. Focused negative cases cover path/CWD substitution,
authorization revocation between pages, hidden-root enumeration, incomplete
inventory, unsafe cleanup, collection membership/default/source-binding drift,
non-final saved-set admission, cyclic/inferred stack edges, ref/worktree recreation,
and ambiguous targets.

The PR15 stack-fanout branch drives bursts across visible and denied roots and
proves recipient/signal batches stay within 64/128; 250 ms readiness/
potential-conflict and 1,000 ms stack/PR/CI debounce never merge scopes;
material conflict/revocation transitions bypass debounce; deterministic
overflow and restart preserve every visible transition and per-root
watermark/coverage; and an open/half-open Plan 37 circuit breaker cannot reveal
or add a root, reuse stale stack authorization, or block truthful non-preflight
delivery. The final PR15 check is the relevant ordinary all-feature repository
test run, not a separate acceptance gate; PR15 adds no benchmark harness or
placeholder baseline.

## Not in PR15

- Creating or provisioning repositories, branches, or worktrees; branch
  deletion; cleanup beyond the exact safe worktree removal above; autonomous
  scope expansion; generic Git execution; rebase, amend, force-push, push,
  remote mutation, or any GitHub mutation.
- Persistent task graphs, execution planning, or agent scheduling, owned by
  later delivery slices.
