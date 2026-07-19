# TraceDecay V2 Cross-Project and Worktree Scope

## Status / Role

Status: active product plan.

Role: PR15 makes repository, project, checkout, worktree, ref, commit, and global
activity scope consistent across query, CLI, MCP, HTTP, LSP, and UI consumers. It
introduces the one typed identity and authorization boundary that later Git-stack,
worktree, and agent-proximity features must consume.

## Outcome

An explicit target always reaches the intended authorized project or code snapshot.
`RepositoryId`, `ProjectId`, `WorktreeId`, `BranchRef`, and `CommitId` are typed,
relationship-checked values; a path, display name, host label, provider key, or branch
label is a locator or observation only and can never become identity.
Project facts and sessions remain project-wide across branches and worktrees; only code
graphs select branch/worktree snapshots. Cross-project results load exactly without CWD
choreography or storage knowledge. `QueryCollection` and `WorkspaceCollection` provide
revisioned, authorized query convenience above typed `ProjectId`; they reference existing
authorities and sources without copying them or becoming identity, ownership, storage, or
authorization authorities.

## Owns

- Shared scope resolution and the authorized, revisioned scope-set contract
  consumed by federated query execution.
- Canonical typed repository, project, worktree, ref, commit, snapshot, and project
  relationships, including relationship proof and stale/ambiguous outcomes.
- The intersection of independently granted project-data, code-snapshot, and exact
  worktree scope. A scope set is a capability result, never a discovery result.
- Typed `QueryCollectionId`, `WorkspaceCollectionId`, immutable collection revisions,
  canonical member ordering, membership snapshots, and policy-bound collection resolution.
- Explicit-target, ambiguity, partial-coverage, freshness, and distributed-cursor rules.
- External worktree discovery, safe visibility, cleanup eligibility, and daemon cleanup.

## Does not own

- Project fact/session storage, user-profile storage, code indexing, ranking, transport
  route catalogs, UI components, task graphs, plan executors, or agent schedulers.
- Worktree creation, provisioning, branch deletion, repository mutation, or task-driven
  authority expansion.
- Git index, hunk, and commit evidence or explicit index transactions; those are owned by
  [Plan 36](36-git-aware-change-context-and-index-transactions.md) and consume this plan's
  resolved repository/worktree identity.
- Provider `project_key`, process CWD, host profile, path hash, branch database, or store
  filename as public identity.
- A path, resolved symlink, display label, branch display name, remote URL, Git common-dir
  location, or collection title as an identity, authorization grant, cursor key, or
  cross-worktree equivalence proof.
- Collection membership as a grant, principal, owner, project registration, repository
  relationship, source-of-truth copy, storage route, or substitute for `ProjectId`.
- Mutation, audit, or rollback of collection source bindings and allow/deny policy. Plan 20
  owns those control-plane operations; this plan resolves their references and enforces
  authorization-safe scope behavior.

## Required behavior

1. One application resolver accepts typed `RepositoryId`, `ProjectId`, `WorktreeId`,
   `BranchRef`, and `CommitId`, plus bounded locators for repository, project, path,
   checkout, worktree, ref, commit, pull request, session,
   `QueryCollectionId`, and `WorkspaceCollectionId`. Every surface consumes the same
   resolved result and typed errors.
   Resolution returns an authorized `ResolvedScopeSet` with a stable digest,
   exact roots and immutable snapshots, relationship provenance, effective
   capabilities, policy epoch, and policy-safe coverage.
2. If the caller names a target, resolution succeeds, returns disambiguation candidates,
   or fails. It never substitutes the active checkout, CWD, first workspace, host home,
   cached project, newest store, or an empty store.
3. `current` is allowed only when no explicit target exists, and every response states the
   resolved project plus code snapshot when code is involved.
4. Project facts, project sessions, messages, and LCM are stored and queried project-wide.
   All branches and worktrees share that authority. Account-wide sessions use the
   user/profile store. Projectless Hermes remains owned by typed `UserProfileId`; it is
   never assigned a synthetic `ProjectId`, inferred into a project, or made project-owned
   by collection membership. A federated response may contain separate project and
   user-profile authority lanes, and every result retains exactly one owning authority.
   No worktree-local fallback store is created.
5. Code queries resolve the requested branch/worktree/ref to an immutable indexed
   snapshot. Dirty, untracked, stale, base-only, missing, or rebuilding coverage is shown;
   the result never implies live working-copy coverage it does not have.
6. Multi-token project lookup matches tokens, aliases, credential-free remotes, paths,
   and verified repository relationships independently. Failure of one combined string
   is not proof that projects are unregistered.
7. Cross-project execution prunes unavailable shards, bounds concurrency and cost, and
   returns searched, stale, unavailable, denied, redacted, and truncated coverage.
   Partial success cannot be rendered as complete success.
   Eligibility, selection, retrieval, fusion/deduplication, hydration, and
   coverage are distinct phases. Authorization and privacy classification run
   before shard selection, statistics, graph expansion, telemetry, or coverage
   rendering; denied-root identity and counts may remain hidden.
   Each request freezes the selected collection kind and ID, collection revision,
   membership snapshot digest, authorization-policy digest, authorized scope-set digest,
   canonical-order digest, and per-shard snapshot/generation/continuation vector.
   Distributed cursors additionally bind those values, the ordered project/root ordinals,
   fusion/dedup profile, last total-order key, schema/catalog revision, policy epoch,
   expiry, and safe coverage summary. Every continuation reauthorizes every member before
   retrieval. Membership, authorization-policy, or root-generation drift returns a typed
   stale-or-revoked cursor error; replay never drops, adds, reorders, or silently advances
   roots to “latest everywhere.”
   Raw scores from heterogeneous shards are incomparable unless a versioned
   compatibility predicate and held-out calibration profile say otherwise.
   Federated query execution owned by Plan 05 uses deterministic rank-based
   fusion as the fallback and preserves per-shard ranks and provenance.
   Every surfaced numeric assessment declares `ordinal_rank`,
   `heuristic_score`, `calibrated_probability`, or `calibrated_interval` plus
   its producer/origin, scale and revision, evidence anchors, and coverage.
   Ordinal rank names its comparison set and deterministic components;
   heuristic scores are comparable only within the same versioned scale and
   never render as probabilities. Probability or interval semantics require a
   valid held-out calibration profile naming cohort, horizon, support, error,
   and drift validity.
8. Stable session, message, entity, and retrieval anchors route to their owner globally
   within the authorized profile. Exact load never requires changing CWD or supplying a
   store-local project key.
9. Project moves and worktree deletion preserve canonical project/session/fact identity.
   Code snapshot and local path aliases retain their time-qualified provenance.
10. TraceDecay discovers externally created worktrees from Git common-directory/admin
    records and observed work locations. It displays repository, path, branch, head,
    dirty state, holders, related sessions/PRs, provenance, ambiguity, and any
    typed association assessment with producer/origin, score kind, versioned
    scale or calibration revision, evidence anchors, and coverage. An
    uncalibrated worktree association is `heuristic_score` or `ordinal_rank`,
    never a probability.
11. TraceDecay never creates a worktree. Discovery or association never grants cleanup.
    No product tool, workflow, or automation creates Git branches or worktrees
    or deletes branches, moves refs, or rewrites history; PR15 owns scope resolution and
    safe worktree cleanup only.
12. Cleanup begins with a read-only daemon inspection. Dirty/untracked files, active
    holders, unpushed or unmerged commits, open or uncertain PRs, shared references,
    ambiguous identity, stale evidence, or missing authorization block cleanup.
13. A cleanup request pins the inspected worktree identity and evidence version. The daemon
    re-resolves Git identity and blockers immediately before removing only that worktree
    registration/root. Branch deletion is not part of cleanup.
14. Crash or uncertain cleanup outcome enters reconciliation and remains visible. Missing
    path alone never proves success or authorizes deletion.
15. Related-project suggestions are explicit and bounded. A query, hint, model, task title,
    or agent cannot silently expand one repository into all projects.
    Expansion requires an explicit caller action and a newly authorized
    `ResolvedScopeSet` digest.
16. Cross-project entities retain their owning project and generation.
    Bridge edges require explicit dependency/package metadata, a verified
    repository relationship, canonical external identity, or explicit selected
    scope, and record endpoint generations and provenance. Name, path, text,
    embedding similarity, host, or co-occurrence alone never merges entities or
    creates authority. Duplicate collapse requires the same authorized
    canonical entity/evidence/anchor identity; similar entities remain
    separate.
17. The [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) gateway
    resolves every LSP workspace folder through this same application resolver.
    Before PR15 it supports only PR12's explicitly resolved single-project
    admission; after PR15, nested roots and multi-root workspaces bind each
    document, analyzer session, code generation, and diagnostic to its exact
    owning folder.
18. An unresolved, ambiguous, denied, stale, or unsupported LSP folder remains
    unavailable with explicit coverage. The gateway never substitutes CWD, the
    first workspace folder, the active checkout, or another folder's analyzer
    or graph generation.
19. [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)'s
    branch-aware feedback cycle and concurrent-agent proximity resolve every
    repository/worktree/branch target through this same application resolver
    before PR15 single-root and after PR15 multi-root scope; neither creates a
    private scope resolver.
20. `QueryCollection` and `WorkspaceCollection` are persisted reference-only selections.
    A query collection member is `Project(ProjectId)` or
    `ProjectlessHermes(UserProfileId)`. A workspace collection member is
    `Project(ProjectId)` or `Root { project_id: ProjectId, root_id: RepositoryRootId }`.
    `ProjectlessHermes(UserProfileId)` is valid only for a verified projectless Hermes
    source. Members never contain copied project records, repository metadata, content,
    credentials, capabilities, paths, aliases, provider keys, store locators, or nested
    collections.
21. Collection publication canonicalizes and deduplicates member references before assigning
    membership ordinals. Membership sorts by `(authority_kind, encoded ProjectId or
    UserProfileId, member_kind, RepositoryRootId-or-zero)` using only immutable stored
    IDs. After root resolution, each query sorts executable roots by
    `(ProjectId, RepositoryRootId, CodeSnapshotId)` and binds that distinct
    `CanonicalResolvedRootOrderDigest` into the cursor. Names, paths, aliases, insertion
    order, discovery order, CWD, and current checkout are never ordering keys.
    The immutable collection revision stores the membership order/digest; the query
    snapshot stores the resolved-root order/digest.
22. Collection resolution first freezes one immutable `CollectionMembershipSnapshot`, then
    reauthorizes each source independently against the caller, requested capability,
    privacy class, and current policy before shard selection, statistics, telemetry, or
    existence-dependent work. Membership and collection ownership never grant access.
    Authorized members expose canonical coverage. A caller-supplied or previously visible
    member that is denied, missing, or revoked appears only as request-scoped
    `restricted_or_unavailable` coverage without canonical ID, label, path, stable token,
    cause split, hidden-member count, or evidence that distinguishes nonexistence from
    denial. Members not already visible to the caller do not affect public counts.
23. The optional default user collection is an unset-by-default convenience selector stored
    by Plan 20. Explicit target or explicit collection always wins. A missing, stale,
    ambiguous, or denied default returns a typed safe error and never falls back to CWD,
    first project, all projects, or newest collection. Selecting or changing a default
    cannot widen authority. Exact anchor load always routes by the result's owning
    `ProjectId` or `UserProfileId`, never by collection identity.
24. A Plan 20 source binding resolves to exactly one `Project(ProjectId)` or verified
    `ProjectlessHermes(UserProfileId)`. Binding dry-run freezes the canonical target,
    locator digest, `ResolvedScopeSet` digest, membership digest when applicable,
    authorization-policy digest, and policy epoch. Apply re-resolves all of them and fails
    closed on ambiguity, expiry, revocation, or drift. Missing, unregistered, and denied
    explicit targets use the same public `target_unavailable` shape; disambiguation
    candidates are returned only when the caller is authorized to see every candidate.

## Canonical identity and authorized query scope

### Typed identity contract

The domain crate defines these values as non-interchangeable newtypes. Their serialized
form is versioned and opaque outside the domain boundary; no API accepts an untyped string
where one of these values is required.

```text
RepositoryId = opaque stable product identifier
ProjectId    = opaque stable project identifier
WorktreeId   = opaque stable worktree identifier

BranchRef {
  repository_id: RepositoryId,
  full_refname: validated native Git refname
}

CommitId {
  repository_id: RepositoryId,
  object_format: Sha1 | Sha256,
  object_id: validated native Git object ID
}
```

`RepositoryId` is assigned only after a native Git repository proof is verified and is
never computed from a pathname, remote URL, repository label, host account, branch name,
or current commit. The proof records native Git object-format capability, a bounded
common-directory/admin-record observation, and object/ref evidence sufficient to reject
a mismatched repository; the proof itself is provenance, not a second object database.
Two clones remain distinct `RepositoryId` values unless a separately authorized,
explicit repository-linking operation proves and records their relationship. Automatic
cross-clone collapse is excluded from PR15 and requires Sol approval before it can be
designed.

`WorktreeId` is assigned to one verified `(RepositoryId, native worktree administrative
record)` relationship. A worktree move, symlink change, mount change, or label change
retains the ID after a successful relationship recheck; deletion followed by recreation
creates a new ID even if it reuses a path. A path and Git administrative directory are
time-qualified locator observations. They may help find a candidate, but are never a
primary key, uniqueness key, foreign key, cursor component, authorization subject, or
equality predicate.

`BranchRef` is a typed native ref locator, not a durable branch-owner identity. Its
full refname is validated by the fixed Git adapter, never inferred from a display label,
and every use pins the observed `CommitId` plus ref-generation evidence. `CommitId`
combines the repository and native object format with the object ID, so an object ID
cannot be replayed across repositories or SHA formats. A commit, ref, project, and
worktree relationship is valid only after the resolver proves it from the currently
pinned native snapshot.

### Scope request, authorization, and lifecycle

Every code-bearing request uses the following input and output boundary:

```text
AuthorizedScopeRequest {
  principal, operation, privacy_class,
  target: Project(ProjectId)
        | Repository(RepositoryId)
        | Worktree(WorktreeId)
        | Branch(BranchRef)
        | Commit(CommitId)
        | Collection(CollectionSelector)
        | Current,
  requested_capability: ProjectDataRead | CodeSnapshotRead | WorktreeRead,
  continuation: optional prior scope/cursor binding
}

AuthorizedScopeGrant {
  grant_id, principal, operation, requested_capability,
  authorized_project_ids,
  authorized_repository_ids,
  authorized_worktree_ids,
  policy_digest, policy_epoch, expires_at
}

AuthorizedResolvedScopeSet {
  project_lanes,
  code_roots: ordered exact {
    project_id, repository_id, worktree_id, branch_ref,
    commit_id, code_snapshot_id, generation, relationship_proof_digest
  }[],
  grant_digest, scope_digest, coverage, expires_at
}
```

`ProjectDataRead` may resolve project-wide facts, sessions, and messages after its
independent project authorization. It does **not** imply `CodeSnapshotRead` or
`WorktreeRead`. Code-bearing operations require an exact, independently authorized
`WorktreeId` and a pinned branch/ref/commit snapshot. A caller that has a project but no
authorized worktree receives `worktree_selection_required`; it never expands to every
known checkout, every worktree attached to a root, or the current worktree. A grant may
contain multiple explicit worktrees, but that set is frozen into the scope digest before
execution and is never derived from a collection or a path scan.

The resolver lifecycle is:

```text
Received
  -> TargetParsed
  -> CandidateLocated                 (locator input only)
  -> IdentityResolved
  -> RelationshipProven
  -> AuthorizationPinned
  -> SnapshotPinned
  -> ScopeFrozen
  -> Resolved

TargetParsed -> InvalidTarget
CandidateLocated | IdentityResolved -> TargetUnavailable | AmbiguousTarget
RelationshipProven -> RelationshipMismatch | SnapshotUnavailable
AuthorizationPinned -> ScopeDenied | WorktreeSelectionRequired
SnapshotPinned | ScopeFrozen -> ScopeStale | CapabilityUnsupported
```

Every terminal state carries policy-safe coverage. `AmbiguousTarget` may show candidates
only when the caller can view all of them. `ScopeStale` is required when a ref, commit,
worktree relationship, policy epoch, grant, collection membership, or root generation
changes after pinning; the client must resolve again and cannot retry against “latest.”

### Collections are selectors, never cross-worktree grants

`QueryCollection` and `WorkspaceCollection` remain reference-only candidate selectors.
Freezing a collection revision can produce `ProjectId` and `RepositoryRootId` candidates;
it cannot produce, infer, mint, or authorize a `WorktreeId`, `BranchRef`, `CommitId`, or
code snapshot. Resolution is intentionally ordered:

```text
CollectionSelected
  -> MembershipRevisionFrozen
  -> CandidateReferencesCanonicalized
  -> PerCandidateAuthorization
  -> ExplicitWorktreeIntersection
  -> CodeSnapshotsPinned
  -> AuthorizedResolvedScopeSet | PartialCoverage | ScopeDenied | ScopeStale
```

The `ExplicitWorktreeIntersection` step intersects collection candidates with the
caller’s pre-existing `AuthorizedScopeGrant.authorized_worktree_ids`. An empty
intersection is a safe no-code-scope result, not permission to enumerate or use adjacent
worktrees. A root member may narrow an already-authorized worktree to that root; it
cannot widen project access to another root or another worktree. A collection revision,
default collection, prior cursor, or previously visible member cannot transfer
cross-worktree authority. This rule applies equally to CLI, MCP, HTTP, LSP, dashboard,
agent hooks, daemon workers, and Plan 37 proximity consumers.

### Files, schemas, APIs, and migration

- `crates/tracedecay-domain/src/identity.rs` defines `RepositoryId`, `WorktreeId`,
  `BranchRef`, `CommitId`, native object-format/ref validation, relationship-proof
  descriptors, and non-path/non-label equality invariants. `scope.rs` consumes those
  types; `lib.rs` re-exports them.
- `crates/tracedecay-domain/tests/identity_scope_contract.rs` proves serialization,
  cross-repository replay rejection, worktree recreation, ref drift, and path/label
  non-identity.
- `src/application/scope/identity.rs`, `grant.rs`, and `resolver.rs` implement the
  lifecycle above. `ports.rs` exposes `RepositoryIdentityStore`,
  `NativeRepositoryInspector`, and `ScopeAuthorizer`; no query transport or collection
  store may bypass them.
- `crates/tracedecay-store/src/scope_identity.rs` and
  `src/global_db/scope_identity/{schema,store,migration}.rs` persist only the typed
  identity/proof records and redacted, time-qualified locator observations.
- `repository_identities(repository_id, object_format, identity_state,
  current_proof_digest, first_verified_at, last_verified_at)` is keyed only by
  `repository_id`.
- `repository_identity_proofs(proof_digest, repository_id, native_admin_record_digest,
  object_format, observed_object_evidence_digest, observed_at, coverage)` is append-only.
  It contains no copied Git objects, paths, credentials, or mutable refs.
- `worktree_identities(worktree_id, repository_id, identity_state,
  current_proof_digest, created_at, retired_at)` is keyed by `worktree_id`; a unique
  `(repository_id, current_proof_digest)` constraint prevents duplicate live identities.
- `worktree_locator_observations(observation_id, worktree_id, locator_kind,
  redacted_locator_digest, observed_at, valid_until)` is append-only and has no
  uniqueness or lookup authority. Raw paths are retained only when the applicable privacy
  policy permits them.
- `scope_resolution_cache(scope_digest, grant_digest, policy_epoch,
  root_vector_digest, expires_at)` caches only typed IDs and digests. It has no path,
  label, collection-title, or CWD column and is invalidated rather than migrated on any
  identity or policy mismatch.

The application API is:

```rust
pub trait ScopeResolver {
    fn resolve(
        &self,
        request: AuthorizedScopeRequest,
    ) -> Result<AuthorizedResolvedScopeSet, ScopeResolutionError>;
}

pub trait NativeRepositoryInspector {
    fn prove_relationship(
        &self,
        repository_id: RepositoryId,
        worktree_id: Option<WorktreeId>,
        branch: Option<BranchRef>,
        commit: Option<CommitId>,
    ) -> Result<RelationshipProof, RepositoryIdentityError>;
}
```

Legacy path-, alias-, provider-key-, and store-name-based routes enter only the migration
quarantine table. They may yield a human-visible locator hint after authorization, but
cannot create an identity record, collection member, grant, or scope cache entry. Migration
is idempotent and leaves all legacy records unresolved when a typed proof is absent.

### Tests, benchmarks, and release gates

- `tests/scope_suite/identity_resolution.rs` covers same-name repositories, identical
  clones, moved roots, symlinks, path reuse after deletion, detached heads, SHA-1/SHA-256
  object IDs, ref rename/deletion, worktree administrative-record reuse, and stale proof
  recovery.
- `tests/scope_suite/authorized_worktree_scope.rs` covers project-data versus code-snapshot
  grants, one/2/8/32 explicitly authorized worktrees, denied neighbors, `current`,
  continuation replay, authorization revocation, and every resolver lifecycle edge.
- `tests/scope_suite/collection_worktree_non_escalation.rs` proves every collection kind,
  default selector, cursor, and partially denied membership cannot grant, enumerate, or
  infer another worktree; its negative fixtures include same-root sibling worktrees and
  an authorized project with zero authorized worktrees.
- `benches/scope_resolution.rs` records cold and warm resolution latency and allocations
  for 1/8/32 projects × 1/8/32 candidate worktrees, plus collection intersection and
  stale-grant rejection. It reports per-phase timing and never substitutes a cache hit for
  an authorization recheck.
- PR15 cannot enable Plan 36 stack projection or Plan 37 cross-worktree proximity until
  the identity, authorization, and non-escalation suites pass with deterministic
  scope-digest replay across process restart.

## Collection implementation contract

### Domain and application files

- `crates/tracedecay-domain/src/scope.rs` defines `QueryCollectionId`,
  `WorkspaceCollectionId`, `CollectionRevision`, `CollectionMemberRef`,
  `CollectionMembershipSnapshot`, `MembershipSnapshotDigest`,
  `AuthorizationPolicyDigest`, `CanonicalMembershipOrderDigest`,
  `CanonicalResolvedRootOrderDigest`, `CollectionCoverage`, `CollectionCursorBinding`,
  and the typed stale/revoked errors. It exports them from
  `crates/tracedecay-domain/src/lib.rs`.
- `crates/tracedecay-domain/tests/collection_contract.rs` proves strict typed-ID parsing,
  member-kind constraints, canonical ordering, digest stability, and projectless-Hermes
  authority separation.
- `src/application/scope/mod.rs`, `src/application/scope/types.rs`,
  `src/application/scope/ports.rs`, `src/application/scope/collections.rs`, and
  `src/application/scope/resolver.rs` own the single use case. `CollectionStore` loads one
  immutable revision; `ScopeAuthorizer` reauthorizes one member; `CollectionScopeResolver`
  returns an `AuthorizedResolvedScopeSet` plus policy-safe coverage.
- `src/query/federated/mod.rs`, `src/query/federated/cursor.rs`,
  `src/query/federated/coverage.rs`, and `src/query/federated/execute.rs` consume the
  resolved snapshot. They cannot load mutable collection state or authorize members.

The required application signatures are:

```rust
pub trait CollectionStore {
    fn load_membership(
        &self,
        selector: CollectionSelector,
    ) -> Result<CollectionMembershipSnapshot, CollectionStoreError>;
}

pub trait CollectionScopeResolver {
    fn resolve_collection(
        &self,
        request: AuthorizedScopeRequest,
        snapshot: CollectionMembershipSnapshot,
        cursor: Option<CollectionCursorBinding>,
    ) -> Result<AuthorizedResolvedScopeSet, CollectionResolutionError>;
}
```

### Store schema and invariants

- `crates/tracedecay-store/src/collection.rs` defines `CollectionStore` persistence DTOs
  and append/load/CAS contracts; `crates/tracedecay-store/src/lib.rs` exports them.
- `crates/tracedecay-store/tests/collection_contract.rs` runs the same immutable-revision,
  deduplication, CAS, and foreign-reference suite against every store implementation.
- `src/global_db/collections/schema.rs`, `src/global_db/collections/store.rs`, and
  `src/global_db/collections/migration.rs` implement profile-global storage because a
  cross-project collection cannot live in one project database.
- `collection_definitions(collection_id, collection_kind, owner_profile_id, display_name,
  current_revision, created_at, updated_at)` stores collection metadata.
- `collection_revisions(collection_id, revision, membership_digest,
  canonical_order_digest, created_by, created_at)` is append-only with primary key
  `(collection_id, revision)`.
- `collection_revision_members(collection_id, revision, member_ordinal, member_kind,
  project_id, user_profile_id, repository_root_id)` has primary key
  `(collection_id, revision, member_ordinal)` and checks that exactly the columns required
  by `member_kind` are non-null. Exact duplicate prevention uses three partial unique
  indexes: project members on `(collection_id, revision, project_id)` where
  `member_kind='project'`; root members on
  `(collection_id, revision, project_id, repository_root_id)` where
  `member_kind='root'`; and projectless-Hermes members on
  `(collection_id, revision, user_profile_id)` where
  `member_kind='projectless_hermes'`.
- `collection_migration_imports(source_table, source_row_id, redacted_payload_digest,
  collection_id, revision, imported_at)` has primary key
  `(source_table, source_row_id, redacted_payload_digest)` and a foreign key to the
  successfully published collection revision. It is an append-only import ledger, not
  collection membership or queryable source data.
- Foreign keys validate typed references but do not cascade-delete collection history.
  Publication writes members, digests, and the current-revision CAS in one transaction.
  Revision and member rows are immutable; triggers reject update/delete. No table copies
  project, root, source, credential, capability, ownership, or authorization data.
- The optional default collection is not stored in this schema. Plan 20 stores
  `query.default_collection.v1`; Plan 16 accepts the resulting explicit selector exactly
  as it accepts a caller-supplied selector.

### Forward migration

`src/global_db/collections/migration.rs` adds the collection tables, the append-only
`collection_migration_imports` success ledger, and
`collection_migration_quarantine(source_table, source_row_id, reason_code,
redacted_payload_digest, quarantined_at)` without changing effective query scope. If the
schema-contract inventory contains both legacy
`saved_project_sets(set_id, owner_profile_id, display_name)` and
`saved_project_set_members(set_id, member_ordinal, project_id)`, it converts only rows whose
`project_id` already validates as canonical, sorts and deduplicates them transactionally,
and publishes one immutable `QueryCollection` revision per valid set. Missing canonical
IDs, duplicate source ordinals, malformed owners, and every path/alias/provider-key legacy
shape enter quarantine keyed by `(source_table, source_row_id)` and never become queryable.
If either named legacy table is absent, migration creates empty collection tables and
performs no backfill. It never reads CWD, path aliases, host profiles, store names, or
Plan 20 defaults. Re-execution detects converted source rows by
the `collection_migration_imports` primary key, verifies its referenced collection revision
still exists, publishes no duplicate revision, and verifies that neither the success
ledger nor collection tables copied project or source payloads.

### Tests and executable acceptance

- `tests/scope_suite/collection_resolution.rs` covers empty, duplicate, reordered,
  overlapping, moved, deleted, denied, and revoked membership; 1/2/8/32 roots;
  byte-stable order across process restarts; and no existence leak.
- `tests/scope_suite/collection_cursor.rs` covers policy change and revocation between
  pages, membership revision drift, root-generation drift, cursor tampering, expiry, and
  deterministic restart from a newly resolved snapshot.
- `tests/scope_suite/collection_authority.rs` proves collections cannot grant ownership,
  authorization, capability, project registration, storage routing, or synthetic
  `ProjectId`, and proves projectless Hermes remains under `UserProfileId`.
- `tests/scope_suite/collection_migration.rs` proves idempotency, canonical deduplication,
  named-table absence, quarantine, and zero duplicated source payloads.

```sh
cargo test -p tracedecay-domain --test collection_contract --all-features
cargo test -p tracedecay-store --test collection_contract --all-features
cargo test --all-features --test scope_suite collection_resolution
cargo test --all-features --test scope_suite collection_cursor
cargo test --all-features --test scope_suite collection_authority
cargo test --all-features --test scope_suite collection_migration
cargo check --all-features
```

## Acceptance

- PR15 tests same-name repositories, moved paths, symlinks, linked worktrees, detached and
  dirty heads, missing indexes, stale/locked/corrupt shards, duplicate legacy routes, and
  unauthorized neighbors.
- A frozen Rspack/Rsbuild/React Router-style fixture resolves token-wise, queries multiple
  repositories, preserves source class, and exact-loads every returned session/entity.
- Project facts and sessions are identical from two worktrees while their code queries
  select different declared snapshots.
- CLI, MCP, HTTP, LSP, and UI conformance returns the same resolution, ambiguity candidates,
  coverage, cursor binding, and errors.
- Federation fixtures cover 1/2/8/32 roots, frozen-vector pagination,
  same-name/path isolation, mixed ranking profiles, stale bridge endpoints,
  denied-neighbor privacy, authorization revocation, global-anchor duplicate
  collapse, source-selection recall, and worst-root retrieval/evidence strata
  without comparing uncalibrated raw scores.
- Direct score-contract fixtures reject missing producer/origin, score kind,
  comparison set or scale revision, evidence anchors, and coverage; prove
  ordinal and heuristic worktree/shard assessments never render as
  probabilities; and permit calibrated probabilities or intervals only when
  held-out cohort, horizon, support, error, and drift-validity metadata is
  present. Evaluation reports ranking quality and calibration error/coverage
  by source and shard cohort rather than averaging incomparable scales.
- LSP fixtures cover same-name repositories, nested roots, linked worktrees,
  symlink escapes, ambiguous folders, denied neighbors, and partial multi-root
  coverage without cross-folder document or diagnostic state.
- Worktree discovery is idempotent; safe cleanup blocks every unsafe case, revalidates at
  mutation time, preserves branches, and reconciles crash outcomes.
- Collection fixtures prove reference-only persistence, immutable revisions, canonical
  project/root ordering, per-source authorization on every page, policy-digest cursor
  binding, safe partial/denied coverage, optional-default behavior, and byte-identical
  `target_unavailable` responses for missing and denied targets.
- Source-binding fixtures prove dry-run/apply drift rejection and that allow/deny
  configuration can only restrict independently authorized `ProjectId` or verified
  projectless-Hermes `UserProfileId` scope.
- No public or internal PR15 operation creates a worktree or opens a worktree-local fact,
  session, or LCM database.
