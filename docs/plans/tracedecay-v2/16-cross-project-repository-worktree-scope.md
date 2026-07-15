# TraceDecay V2 Cross-Project, Repository, and Worktree Scope Plan

> **Accepted-base refresh delta (audit 29 / packet 30):** registry/alias-aware
> `tracedecay_project_context` session-project resolution and Hermes-home
> prefix-containment rejection (PR #453) are preserved; the per-project
> projection fan-out requires per-shard reconciliation so "searchable from every
> touched repo" survives a partial fan-out failure. See
> [`30-baseline-refresh-candidate-packet.md`](30-baseline-refresh-candidate-packet.md)
> §5, §7.1 and FM-162.

**Status:** implementation plan; product code is out of scope for this pull request.

**Parent plan:** [`../2026-07-09-tracedecay-brain-rewrite.md`](../2026-07-09-tracedecay-brain-rewrite.md)

**Purpose:** make multi-repository, multi-project, multi-checkout, and multi-worktree use a native TraceDecay behavior rather than a sequence of registry lookups, path guesses, store switches, and retries.

**Publication snapshot:** [master §2.6](../2026-07-09-tracedecay-brain-rewrite.md#26-current-master-accepted-changes) plus [plan 13](13-research-provenance-and-context-anchors.md) are normative. Untracked branch graphs and divergent session variants are first-class identities; consolidation uses bounded indexed family lookups; registry healing cannot steal aliases/paths or resurrect retired owners; search reads never repair; graph peers disable mmap; applied-manifest retirement is restart-safe and lifecycle-fenced. Historical failed explicit-worktree/root PR context, zero-file search, and branch/session lookup remain required cross-domain partial/fallback fixtures.

## 1. Product invariant

An agent or person must be able to ask one question about one named repository, several related repositories, every registered project, a particular worktree, or a historical ref and receive:

1. Results from the intended scopes, never a silent fallback to the active checkout.
2. One stable identity for every repository, checkout, worktree, ref snapshot, session, message, agent, and code entity.
3. Explicit coverage: searched, skipped, unavailable, stale, truncated, redacted, and permission-denied scopes, carried as one typed record — plan 01's `CoverageReportV1` (shard dispositions, freshness watermarks, unknown-coverage flag) — rather than per-transport prose.
4. A direct retrieval path from a summary/search hit to the exact underlying object, even when the object lives in another project shard.
5. The same selector, error, cursor, and response semantics in the official API, CLI, MCP, dashboard, hooks, and generated SDKs.
6. A compact answer by default and full provenance on demand.
7. Plan 22's Context Scout may expand across projects only through a pinned authorized project-set version; a model cannot turn current scope into All, and sibling task/agent activity is silent without a material typed relation.
8. Plan 23's temporal search and context assembly use the same resolved scope before ranking and return stable cross-shard anchors; current/as-of logic cannot repair a wrong project/worktree/ref after retrieval.
9. Plan 24's initiative/plan/work-item graph pins one authorized project-set version and exactly one writable workspace binding per attempt. Other repositories are read-only; a required multi-repository write decomposes into one fenced child work item per writable target plus explicit integration gates. A board, executor, agent, CWD, current checkout, or task title cannot add a repository, change the primary writable worktree/ref/snapshot, or copy a task to repair scope after dispatch.
10. Plan 28 may place the same logical Brain/repository on several enrolled machines. Repository identity is global within the Brain; checkout/worktree/path identity is node-scoped; every routed result names authority/replica/cache coverage without exposing storage topology as the user's scope.
11. Plan 24 task/attempt records may associate with externally created worktrees and plan 11 may display and act from that ticket context, but TraceDecay never creates or provisions a worktree. Discovery, confirmation, ownership provenance, and cleanup authorization are separate facts. Plan 26 observes their lifecycle without becoming mutation authority.

“Current project” remains a convenient default. It is never an invisible constraint and never overrides an explicit repository, worktree, path, PR, branch, session, or agent reference in the request.

Host profile is not data scope. `HostProfileId` identifies an installation/configuration target; it never selects a TraceDecay `ProfileId`, Brain, database root, fact partition, or query boundary. All named Hermes profiles bind the same user-global TraceDecay profile and stores used by Codex, Claude, and Cursor. Each provider invocation supplies an immutable logical workspace/root set from that session; process CWD, first/last workspace, cached project, `HERMES_HOME`, and host-profile directory are provenance only. A projectless invocation resolves explicitly to profile/zero-project semantics rather than manufacturing a project.

## 2. Historical evidence and exact anchors

The following sessions are regression fixtures, not anecdotes:

| Retrieval anchor | Observed behavior | Required regression |
|---|---|---|
| `session:019f42c9-623a-7cc0-95c1-f073eaa05a4d` | An agent concluded that TraceDecay had no registered Rsbuild/Rspack sibling project and fell back to installed package sources. | A multi-token resolver and related-project suggestion must find both repositories or return disambiguated candidates; it must not equate one failed search string with registry absence. |
| `session:019f4323-f569-74c0-9988-ea3851d14fd7` | The user corrected the agent: Rsbuild and Rspack were in the registry. `project_list` was initially capped at 25, `project_search "rsbuild rspack"` returned none, and the agent needed separate searches before querying both graphs. | One request resolves a project set and executes the intended federated code query; caps and omitted projects are explicit; no manual list-then-filter loop. |
| `session:019f4325-57ef-7a53-b6a0-5c583c759301` | Root-cause analysis found project search used one contiguous `LIKE "%rsbuild rspack%"` pattern rather than token-aware matching. | Exact, token, alias, remote, path, fuzzy, and relationship-aware resolution are separately evaluated and explained. |
| `session:019efb4d-4508-7182-961b-9b30c739baa7` | Earlier React Router plugin work found Rspack but reported Rsbuild unregistered, then mixed a sibling graph with local installed-package inspection. | Results identify source repository, snapshot, and fallback provenance per evidence item; missing one related repo does not silently change evidence class. |
| `session:019f1568-f9de-75c1-9870-7cee46944adc` | A copied/recovered workflow repeated the same partial cross-project conclusion. | Copied workflow/session results cluster under the canonical investigation and do not inflate confidence or evaluation counts. |
| `session:3277c0dd-4388-4a99-9665-96eefe31918a` | A natural-language request for a nearby Rsbuild React Router repository succeeded only after separate registry queries. | `resolve_scope` accepts natural language, returns the dedicated repository directly, and offers adjacent Rsbuild/Rspack/React Router scopes as optional related context. |
| `session:f6e02b68-dcb8-4fd4-975e-9ad5895d2a9d` | A contamination investigation had to prove that separate registered repositories used separate stores and that an incorrect “federation” theory came from the model rather than TraceDecay. | Every result exposes store/project/repository identity and evidence origin; the system can prove cross-store isolation without raw database inspection. |
| `session:019f3edc-6a4e-7d80-b181-8f6d1e657859` | PR context resolved the base checkout/branch while the user intended a different worktree. | Worktree and ref snapshot are first-class selectors; a branch-sensitive operation refuses an ambiguous active/base mismatch. |
| `session:019f2538-0fd9-7362-a50b-96e36130643b` | Session search remained constrained through `sessions.project_key`, making provider-local attribution act like a public project boundary. | Provider keys remain provenance aliases only; profile activity owns canonical session discovery and zero/one/many repository attribution. |
| `session:019f2524-534d-7bd1-a3b1-675f242dcc0e` | Claude’s first CWD could misattribute later cross-worktree messages. | Location is an interval-valued observation per Turn/message; no session-wide first-CWD overwrite. |
| `session:019f1204-5575-72a1-a2d1-ab5c6d1b310d` | “No project index found” guidance suppressed otherwise available session and memory capabilities. | Capability availability is domain-specific; absent code graph does not disable profile activity, messages, memory, Git, or registry discovery. |
| PR #425 / unresolved historical session anchor: branch `codex/explicit-store-consolidation`, final head `d3bb28b57bef6f7fa513ff4b0645ce5e31a97872`, merge `de3d05dc` | At the planning snapshot, explicit root/worktree/branch lookup stopped before semantic Git/session analysis because the same checkout had healthy selected and legacy stores. The selected lane reported zero automation files while the legacy lane reported 3,470; path/current-project selection could not decide authority. | Resolution returns both candidates and typed coverage. The now-merged consolidation freezes/backs up both, identifies holders by file identity, preserves aliases/remapped LCM edges, reconciles every table/collision, verifies exhaustively, and publishes marker/registry atomically before the exact recipe is retried; no CWD/path/newest-mtime/empty-lane inference. |

Current supported-surface reproduction also exposed a composability break: `tracedecay_message_search(project_scope="all_registered")` can return a session from another registered project, but `tracedecay_lcm_load_session` is active-project-only and rejects project selectors. Search-to-exact-retrieval must therefore be a required end-to-end conformance test, not two independently green tools.

The frozen Rspack/Rsbuild/React Router family is one named cross-repository regression slice in the heterogeneous scope corpus because it exercises all of these evidence sources together; it is not a required live checkout, product dependency, or sole conformance authority:

- `rsbuild-plugin-react-router`: product code, tests, branch, PR, benchmark, and agent worktree.
- `rsbuild`: plugin API, dev-server, environment, `loadBundle`, middleware, and memory-filesystem contracts.
- `rspack`: compiler, output filesystem, asset emission, runtime, resolver, and bundler behavior.
- `react-router`: framework semantics and upstream version behavior.
- synthetic benchmark repositories: downstream performance and integration evidence.
- support or fork repositories: reproducible failures, stacked branches, and published canaries.

## 3. Canonical scope vocabulary

Do not expose storage topology as product scope. Use these identities:

| Identity | Meaning | Cardinality/notes |
|---|---|---|
| `ProfileId` / `BrainId` | Privacy/ownership profile and its logical Brain across zero or more enrolled machines. | Local-only remains valid; one Brain can contain many repositories, nodes, placements, and stores. |
| `BrainNodeId` | Stable TraceDecay enrollment/key identity for one node. | Hostname, path, IP, or VPN/Tailscale identity is only transport evidence. |
| `RepositoryId` | Durable source-control lineage, independent of checkout path. | Derived from evidence-scored remote/common-history identity; ambiguity is preserved. |
| `ProjectId` | Registered TraceDecay product unit and policy/config boundary. | Usually maps to one repository, but may represent a non-Git workspace or explicit subproject. |
| `CheckoutId` | One node-scoped filesystem checkout of a repository. | Main checkout and worktrees are peers; clean snapshots may dedupe by verified immutable manifest. |
| `WorktreeId` | Node-scoped Git worktree identity, including git common dir and admin record. | Path is a versioned local alias, not repository identity; dirty overlays never merge by branch name. |
| `TaskWorktreeAssociationId` | Versioned relation between a plan-24 work item/attempt and one externally created worktree. | May be inferred, confirmed, rejected, reassigned, or released; association is not ownership or cleanup authority. |
| `WorktreeCleanupIntentId` | One CAS-pinned daemon cleanup request over a worktree identity and eligibility snapshot. | Never a path or client-side delete instruction; terminal receipt/failure remains auditable. |
| `RefId` | Branch/tag/remote-ref lineage. | Ref name is mutable; observations are time-qualified. |
| `CodeSnapshotId` | Immutable code/Git state used for a graph query. | Normally commit/tree plus index/config/parser generation; distinct from `GraphGenerationId`. |
| `ProjectSetId` | Saved or ephemeral set of repositories/projects. | Supports related systems and benchmarks. |
| `CollectionId` | User-saved heterogeneous entity/query collection. | May cross projects. |
| `SessionId`, `AgentId` | Activity identities in the profile activity domain. | Attribution to repositories/worktrees is zero, one, many, or unknown over time. |
| `OrchestrationObservationId` | Read-only identity for a captured provider-native orchestration/workflow observation. | It may correlate with sessions/agents/projects but never grants execution authority or aliases a native workflow run. |
| `WorkflowDefinitionVersionId`, `WorkflowRunId` | Plan-32 native dynamic-workflow definition/run identities. | A run pins one exact definition version; scope attribution may span repositories, but neither ID is a provider observation or static operation-workflow ID. |
| `OperationId`, `OperationStepId` | Plan-09 static `OperationWorkflowDefinitionV1` execution identities. | These application-owned recipes may span shards but are neither provider observations nor Plan-32 user-authored definitions/runs; they inherit declared operation scope and never mint `WorkflowRunId`. |
| `InitiativeId`, `PlanId`, `WorkItemId`, `ExecutionAttemptId`, `ExecutorRegistrationId` | Canonical plan-24 work/execution identities in the profile activity domain. | Scope may span zero/one/many repositories; an attempt pins one exact writable binding plus authorized reads. Boards and executor queues do not mint scope identity. |

`project_key`, transcript CWD, path hash, graph database filename, store directory, branch database, and provider-local project fields are aliases/provenance. They never become the primary public selector.

## 4. `ScopeSelectorV2`

Every transport generates or consumes the exact `ScopeSelectorV2`, `ScopeRootV2`, and `ScopeTargetV2` definitions owned by plan 01 §14. This plan adds resolution, federation, and UX requirements but defines no second selector type; compile-time schema-digest tests fail if any transport or SDK copy diverges.

The human-readable JSON below shows a two-repository plus worktree-locator request with the exact field set; the contract generator freezes the final discriminated-union spelling. `include`, `ScopeExpr`, root-local snapshot fields, and transport-specific `project_key` do not exist:

```json
{
  "version": 2,
  "roots": [
    { "kind": "repository", "target": { "kind": "canonical", "ref": "repo_7c12e9a14fe44f77a0f9c29b3760a812" } },
    { "kind": "repository", "target": { "kind": "canonical", "ref": "repo_2b198ef27e36492e92bd15df8a85241d" } },
    {
      "kind": "worktree",
      "target": { "kind": "locator", "locator": { "kind": "worktree_path", "value": "/worktrees/feature", "repository_hint": null } }
    }
  ],
  "exclude": [],
  "time": { "as_of": "2026-07-09T20:00:00Z" },
  "activity_attribution": "overlap",
  "coverage": "allow_partial",
  "freshness": { "max_age_seconds": 300, "on_stale": "report" },
  "traversal": { "kind": "related", "max_depth": 1 },
  "ambiguity": "return_candidates",
  "limits": { "max_projects": 20, "max_shards": 40, "max_graph_nodes": 10000 }
}
```

Supported selector kinds:

- `CurrentInvocation` and `AllAuthorized { profile_id }`.
- `Profile`, `ProjectSet`, `Collection`, `Repository`, `Project`, `Checkout`, and `Worktree`.
- `Ref` (including branch/tag locators), `Commit`, `CodeSnapshot`, `GraphGeneration`, and `PullRequest`.
- `Session`, `Thread`, `Turn`, `Agent`, `Goal`, `Workflow`, `AutomationRun`, `Initiative`, `Plan`, `WorkItem`, `ExecutionAttempt`, and `Executor`.
- `SavedView` and typed `GraphNeighborhood`.

Resolution modes:

- `ScopeTargetV2::Canonical` is an exact stable ID.
- `ScopeTargetV2::Locator` runs token-aware path/name/alias/remote/branch/PR resolution; `ambiguity` controls error versus candidates.
- `traversal` may propose or explicitly include bounded graph-related scopes; it never silently adds them.
- `AllAuthorized { profile_id }` is the only All root.

## 5. Resolution algorithm

The resolver is a shared application service, not duplicated transport code:

A single `ScopeRootV2::Profile` root resolves before any transport project discovery; a canonical query predicate may select activity rows whose owner is `DeclaredScope::Profile` or `DeclaredScope::ZeroProject`. Fact, LCM, message-search, and other profile-activity calls reach the authorized profile owner without synthesizing `--project`, consulting CWD/session workspace/host home, registering or initializing a project, or opening a project shard. A canonical authorized read selector may explicitly include Profile plus Project roots and resolves through normal federation with per-root coverage. Compatibility `memory_scope=user` and `storage_scope=user` may lower to the single-root request only in the migration adapter; combining either scalar alias with compatibility `project_id`, `project_key`, `project_path`, `project_root`, `project_scope`, or nested `project_selector` fails validation with the exact conflicting fields.

For `CurrentInvocation`, immutable provider session/workspace context is evaluated independently from installed host-profile ownership. The exact host-profile home itself is a typed excluded project candidate, not a projectless-to-project fallback; a repository physically beneath that directory remains eligible only when normal registration/canonical resolution proves it. Ambient `HOME`, `HERMES_HOME`, provider helpers, previous-session state, and process CWD cannot select the TraceDecay profile or override the installed/configured `HostProfileRef`.

1. Parse typed IDs, URLs, filesystem paths, Git remotes, PR/branch/commit syntax, and natural-language tokens.
2. Normalize paths without erasing symlink, casing, mount, or historical-alias evidence.
3. Resolve exact stable IDs first.
4. After authorization selects the privacy domain/key epoch, query the catalog's versioned privacy-domain-keyed exact alias-routing digests; verify every selected candidate against canonical alias evidence in its owner shard.
5. Run token-aware matching over keyed token/quoted-phrase routing digests with per-token `AND` across an `OR` field set; no literal path/remote/alias enters the catalog.
6. Add typo/fuzzy candidates from bounded keyed n-gram routes only below the exact/token confidence threshold, then verify in authorized owner shards.
7. Add relationship evidence: node-local shared Git common dir, credential-free normalized remote/forge alias, verified immutable commit/tree/object-format and ancestry evidence, registered project family, previous co-query/co-investigation, dependency, generated artifact, PR/base/head, and explicit user project set. Remote equality or common history alone cannot collapse forks.
8. Score candidates with field/evidence explanations; never promote recency over an exact ID/path.
9. If one candidate exceeds the calibrated exact threshold, resolve it.
10. Otherwise return bounded candidates with stable IDs, discriminators, and a ready-to-resubmit selector.

Across nodes, `RepositoryIdentityProofV1` records which immutable Git objects and normalized aliases were actually verified, object format, shallow/partial/replacement/graft/rewrite limitations, contradictions, confidence, and source nodes. Ambiguity requires a versioned adopt/split receipt. Same path/name, hostname, branch, or remote string never silently merges identities.

Disambiguators include owner/repository, canonical path, worktree path, branch/ref, head commit, last indexed time, store health, default branch, and reason matched. Credential-bearing remotes are redacted before rendering.

### 5.1 Explicit-target rule

If the prompt/command names a repository, project, path, worktree, branch, PR, session, or agent:

- The transport extracts it as a candidate selector.
- The application service resolves or rejects it before query execution.
- It never executes against `current` merely because resolution failed.
- A failure response includes candidates and one exact retry request/command.

### 5.2 Current-default rule

If no target is present, `current` may resolve from the invocation CWD/host context. Every response still reports:

```text
scope: current -> project:proj_rsbuild_plugin_react_router
worktree: /.../dev-rebuild-optimizations (branch codex/dev-rebuild, head abc1234)
```

## 6. Registry and store architecture

`catalog.db` owns store manifests, stable entity-to-owner routes, and versioned privacy-domain-keyed exact/token/ngram alias-routing digests. Canonical alias values/history remain in authorized `activity.db` or project owners. `activity.db` owns canonical provider activity. Project shards own repository/project-scoped code, delivery, knowledge, and policy projections.

Alias-routing digests are versioned records, not bare hashes: every digest row carries the privacy-domain key epoch/ID, tokenizer/normalizer version, digest algorithm version, and catalog generation, so key rotation or tokenizer change re-derives routes instead of silently corrupting resolution. Plan 02 owns the storage schema for these route tables.

Required behavior:

- A registry watcher reconciles Git worktree admin data, filesystem presence, remotes, canonical alias evidence, keyed catalog routes, tombstones, and store manifests without eagerly opening every shard. Resolution prunes through the routing index and opens only authorized candidate owners for literal/evidence verification.
- Plan 28's catalog additionally routes logical entities to authority/replica/cache placement and maps node-scoped checkout/worktree/path aliases to one verified `RepositoryId`. Physical placement changes do not mint product identity; stale placement/authority epochs produce typed coverage instead of fallback to the caller's local checkout.
- Discovery is idempotent and provenance-bearing. A transient worktree path does not create a new repository identity.
- Stale/deleted checkout rows are historical aliases with state, not active results and not silent garbage.
- Duplicate stores for one canonical repository enter conflict/quarantine workflow; reads expose the conflict rather than picking newest mtime.
- Renames and moved paths preserve adoption receipts and prior stable IDs.
- A non-Git project remains addressable without fabricated repository identity.
- Domain availability is per store/capability. A missing graph can coexist with healthy sessions, facts, Git observations, or automation data.
- Registry garbage collection has preview, evidence, retention, apply receipt, and rollback; it does not delete stores merely because a path is absent.
- External host/project stores are source-owned read-only evidence unless a separate TraceDecay import decision approves a bounded sanitized copy. Registry retirement can remove only TraceDecay-owned routes/projections with receipts; it never mutates or deletes Hermes-owned transcripts, board databases, caches, or backups.

### 6.1 Task-associated worktree discovery and lifecycle

Plan [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md) remains the task/attempt/event authority. Plan [`11-dashboard-frontend.md`](11-dashboard-frontend.md) renders related worktrees and cleanup state inside ticket/PR views. Plan [`26-observability-accounting-and-usage.md`](26-observability-accounting-and-usage.md) consumes lifecycle/cleanup events and latency/failure counters. This plan owns identity correlation and cleanup eligibility. Plan 02 persists all of it in the existing profile activity/task graph, relation, operation, outbox, audit, and retention families; there is no worktree-cleanup database or service.

TraceDecay discovers worktrees created externally by an agent, user, executor, Git client, or IDE. It does not expose a create/provision operation. Discovery ingests bounded, idempotent observations from:

- `git worktree list --porcelain`-equivalent Git-library inventory, git-common-dir/admin records, repository identity, path alias, branch/ref, HEAD/tree, locked/prunable state, and last-seen generation;
- hook/tool `workdir`, provider CWD intervals, thread/subagent lineage, plan-24 workspace bindings and attempts, produced commit/artifact evidence, branch/commit ancestry, and delivery PR head/base/merge observations;
- explicit user/agent association commands and imports/backfills, each retaining creator/source provenance rather than claiming TraceDecay created the checkout.

Correlation is deterministic and versioned. Exact `WorktreeId`/Git admin identity and an attempt's sealed workspace binding dominate; verified common-dir/repository, branch/commit/PR, temporal CWD/tool use, and thread/task evidence contribute registered score features. Every candidate returns the feature contributions, provenance anchors, algorithm version, confidence, contradictions, and `Candidate | Ambiguous | Inferred | Confirmed | Rejected | Superseded` state. Equal or conflicting candidates stay ambiguous. Repeated observations and the same association command insert-or-read the same result. `confirm`, `reject`, and `reassign` use expected association revision and idempotency; reassign releases the prior relation and appends a successor rather than moving history. Reconciliation/backfill reruns the frozen algorithm over retained observations and reports changed candidates without overwriting prior decisions.

One canonical relation connects work item, optional attempt, repository, checkout, worktree, branch/ref/commit, and optional PR. `workspace_binding` remains execution authority; association is descriptive navigation/provenance. Cleanup authorization is a separate explicit grant backed by external creator/source ownership proof or an independently verified safe-policy proof. Inferred or confirmed association, matching branch names, task assignment, process CWD, or PR linkage cannot grant cleanup authority.

The daemon maintains evidence-bearing live references, not a blind scalar refcount: active task, attempt, lease, resource reservation, holder, branch, PR, checkout, explicit hold, and other association subjects. A rebuildable count projection accelerates lists, but every cleanup decision rechecks reference rows at one frozen activity/catalog/Git/delivery watermark. Archive and verified merged-PR events enter through the ordinary task/delivery journal and outbox, then recompute eligibility. They do not create a triage task, auto-archive a different ticket, or imply that cleanup is safe.

`WorktreeCleanupEligibilityV1` is `Eligible | Blocked | Unknown | AlreadyAbsent` with exact triggers, evidence watermark, identity/lifecycle generation, cleanup-grant proof, policy version, expiry, and typed blockers. Any of these blocks cleanup:

- dirty index, working copy, untracked files, or incomplete dirty-state observation;
- active attempt, lease, reservation, executor/holder, or nonterminal external effect;
- commits not proven reachable from the configured push remote, or not proven merged into the intended destination;
- open/unmerged PR or delivery state that is absent, stale, conflicting, or not authoritative;
- another live task/attempt/branch/PR/checkout/hold/shared association reference;
- ambiguous repository/worktree identity, missing cleanup grant, stale watcher/delivery evidence, unreachable remote, or any unknown required proof.

`inspect` is a read-shaped preview-as-evidence operation. It returns the exact blockers, reference subjects, Git/PR proofs, proposed effect, branch disposition, eligibility digest, expiry, and one safe next request. It never performs cleanup. `diagnose` explains ambiguous/stale/orphan/candidate state and reconciliation/backfill options. `request-cleanup` is a confirmed daemon workflow requiring `WorktreeId`, `WorktreeCleanupIntentId` or current inspect digest, expected lifecycle/association versions, cleanup grant, and idempotency. Immediately before mutation the daemon re-resolves Git common-dir/admin identity and rechecks every blocker; clients never execute `git worktree remove`, recursively delete a supplied path, or receive a deletion recipe.

Cleanup removes only the proven worktree root/admin registration represented by the intent. Branch deletion is not part of this effect and requires a separately named future command if ever supported. Success, already absent, blocked-at-recheck, failed, and unknown-after-effect each append a task graph event, operation/audit/outbox receipt, and identity disposition. Crash or uncertain external outcome enters reconciliation and cannot be reported successful from missing-path evidence alone.

Stale/orphan discovery is conservative. Missing path observations preserve identity/history. A candidate becomes stale only after the configured evidence horizon, and orphan discovery requires agreement among Git admin/common-dir, registry, holder, task/attempt/reference, branch/PR, and checkout evidence. Retention may compact superseded scoring/current projections after the cursor/reconciliation horizon, but association/grant/cleanup decisions and safe receipts follow task/audit retention. Absence alone never creates cleanup authority or an intent.

## 7. Cross-project session and activity model

Provider activity is ingested once into the profile activity journal. Repository/worktree/project attribution is a temporal evidence relation:

- Session starts in repository A, later Turns run in worktree B.
- Parent agent is in A while a subagent is spawned for B.
- A tool call queries code in C without changing the host CWD.
- A user discusses D while no local checkout is active.
- One workflow coordinates A, B, and C.

The projection stores each observation’s location evidence independently: provider CWD, tool `workdir`, resolved checkout/worktree, Git common dir, branch/head, explicit tool selector, referenced entity, and confidence. It never overwrites the whole session with the first or last CWD.

Activity filters support:

- `produced_in`: the actor wrote/changed an artifact in the scope.
- `executed_in`: the Turn/tool ran in the scope.
- `queried`: the actor intentionally queried the scope.
- `discussed`: the message referenced the scope.
- `observed`: the session encountered evidence from the scope.
- `overlap`: any evidence interval overlaps the requested scope.
- `primary`: highest-confidence task scope, with reason and abstention.

Search hits return the activity entity from `activity.db` plus relations to project entities. They do not require guessing which project shard contains a second copy of the transcript.

## 8. Federated code and Git graph

Code graph federation is query planning over immutable snapshot generations, not a physical mega-database join by default.

### 8.1 Snapshot selection

For each selected repository/worktree:

1. Resolve requested as-of/ref/commit/working-tree state.
2. Select one compatible code-graph generation and report parser/index generation.
3. For dirty worktrees, represent index and working-copy overlays separately from committed base.
4. Refuse to imply live working-copy coverage when only the base commit is indexed.
5. Report staleness and commits/dirty changes since snapshot.

### 8.2 Cross-repository edges

Typed cross-repository edges include:

- package dependency and lockfile resolution.
- plugin/host API use.
- generated or published artifact provenance.
- fixture/vendor/submodule/fork lineage.
- Git cherry-pick/backport/patch equivalence.
- PR head/base and downstream test/benchmark relation.
- symbol/type/API reference when supported by language/package resolution.
- session/tool/agent observation and produced/impacted evidence.
- manually curated “related system” project sets.

Every edge has source evidence, captured time, valid interval, confidence, algorithm/version, and snapshot endpoints. The query planner may expand through these edges only when the user asks for related/cross-project context or a bounded policy selects it; it never silently rewrites a one-project query into All.

### 8.3 Federated ranking

Normalize per-shard scores only after preserving channel/raw scores. Global merge applies:

- exact ID/symbol/literal priority.
- scope intent and explicit-target boosts.
- repository diversity when the query asks for multiple projects.
- per-repository caps to prevent a large repo dominating.
- snapshot/freshness penalties with explanation.
- duplicate/fork/generated-source clustering.
- optional dependency/graph reranking.

## 9. Search-to-retrieval contract

Every search result card includes:

- canonical `entity_ref` and domain `RetrievalAnchorId` resolving to `RetrievalAnchorRecordV1`.
- source profile/repository/project/worktree/ref/snapshot.
- event and ingest time.
- provider/origin/audience/kind.
- one-line rank reason and channel scores.
- exact projection/source watermarks.
- representative/duplicate cluster information.
- permitted expansions and related entities.

The following sequence must work across any authorized shard:

```text
search -> result RetrievalAnchorId -> authorized RetrievalAnchorRecordV1 -> exact entity/Turn/message/session load
       -> adjacent context -> source observation/payload -> export manifest
```

It must not require changing CWD, restarting MCP in another worktree, discovering a private store, or replaying the original search. A retrieval reference is location-independent and remains valid across project rename/worktree deletion; retention/redaction may later return an explicit tombstone.

Distributed cursors bind query digest, scope-set generation, per-shard cursor/watermark, ordering, policy/ranker version, and expiration. Resume never silently searches a different project set after registry change.

## 10. CLI contract

Common flags are generated from `ScopeSelectorV2` for every compatible command:

```text
--scope current|all|<stable-id-or-handle>
--project <id|name|path>        repeatable
--repo <owner/name|remote|id>   repeatable
--worktree <path|id>            repeatable
--ref <branch|tag|commit>
--project-set <id|name>
--related                      propose/confirm related expansion
--as-of <time|commit>
--freshness <duration>
--partial allow|deny
--explain-scope
```

Examples:

```sh
tracedecay search "memory filesystem loadBundle" \
  --repo rstackjs/rsbuild --repo web-infra-dev/rspack

tracedecay code impact loadBundle \
  --project rsbuild --related --explain-scope

tracedecay sessions search "dev runtime not ready" \
  --project-set rsbuild-react-router-system

tracedecay session show session:019f42c9-623a-7cc0-95c1-f073eaa05a4d

tracedecay scope resolve "rspack rsbuild react router" --json

tracedecay task-graph worktrees list --work-item <id>
tracedecay project worktree association diagnose --worktree <id>
tracedecay project worktree cleanup inspect --worktree <id>
tracedecay project worktree cleanup request --worktree <id> --preview-digest <digest> --expected-lifecycle-generation <n> --confirm <token>
```

Rules:

- Long and short output begin with resolved scope and partial/stale state.
- `--json` returns the same typed response as MCP/API, not parsed terminal markdown.
- Ambiguity exits with a typed nonzero code and candidate retry commands.
- `--all` is explicit, bounded, cancellable, and reports omitted scopes.
- Commands that cannot support a selector say so through capability discovery before execution.
- A target stable session/message/agent ID is globally routable; no project flag is required.
- Worktree discovery/list/association/confirm/reject/reassign/diagnose and cleanup inspect/status/request commands resolve to typed IDs through the daemon. No command creates a worktree or accepts a path as deletion authority; cleanup request consumes the pinned inspect/intent evidence and expected versions.

## 11. MCP contract for agents

Every read tool uses the same optional generated `scope` object; globally routable `RetrievalAnchorId` values need only `anchor`:

```json
{
  "query": "Rspack outputFileSystem and Rsbuild loadBundle contract",
  "scope": {
    "version": 2,
    "roots": [
      { "kind": "repository", "target": { "kind": "locator", "locator": { "kind": "remote", "value": "rstackjs/rsbuild" } } },
      { "kind": "repository", "target": { "kind": "locator", "locator": { "kind": "remote", "value": "web-infra-dev/rspack" } } }
    ],
    "exclude": [],
    "activity_attribution": "overlap",
    "coverage": "allow_partial",
    "freshness": { "on_stale": "report" },
    "traversal": { "kind": "exact" },
    "ambiguity": "return_candidates",
    "limits": { "max_projects": 20, "max_shards": 40, "max_graph_nodes": 10000 }
  },
  "explain": ["scope", "rank", "coverage"]
}
```

Agent-facing requirements:

- Capability discovery returns supported scope kinds, maximums, freshness, mutation class, and exact replacement when a tool is superseded.
- The server may recommend one directly callable request, not “call project list, inspect 25 rows, search each token, copy a project ID, then retry.”
- Natural-language repository references are resolved in the same call when unambiguous.
- Related-project suggestions are compact and actionable: stable ID, reason, one-line summary, estimated query cost.
- Tool output defaults to concise Markdown, while JSON remains explicit for programmatic chaining.
- Every partial/truncated response returns a cursor or stable retrieval/export handle.
- A globally returned `session`, `message`, `turn`, `agent`, `entity`, or `retrieval` ref is accepted by the exact-load tool regardless of the MCP server’s startup CWD.
- Mutation tools require exact resolved scope, optimistic version/precondition, preview when destructive, and a receipt.
- Worktree tools expose cursor-paged discovered candidates, task/attempt associations, confirm/reject/reassign, stale/orphan diagnosis, cleanup inspect, request, and operation status. The MCP client receives eligibility evidence and an `OperationRef`; it never receives or runs a filesystem deletion command. An inferred/confirmed candidate without a separate cleanup grant is read-only.

## 12. Official API and SDK relationship

The official API plan in [`17-official-public-api-and-sdks.md`](17-official-public-api-and-sdks.md) owns transport, authentication, versioning, schemas, and SDK generation. This plan owns scope semantics. The API must not invent an HTTP-only selector or expose `project_key` as a primary field.

Minimum generated bindings from the plan 17 contract IR; no plan or adapter maintains a second route registry:

- `POST /api/v2/scopes:resolve`.
- `GET /api/v2/scopes/{id}` and lazy children/relations.
- `POST /api/v2/query` with `ScopeSelectorV2`.
- `POST /api/v2/search` with cross-domain/project profiles.
- `POST /api/v2/entities:batch` for bounded exact hydration of globally returned refs.
- `GET /api/v2/sessions/{global-ref}` and `/messages/{global-ref}`.
- `POST /api/v2/graph/neighborhood|path|impact`.
- `GET /api/v2/coverage` and projection/index watermarks.
- `POST /api/v2/subscriptions` followed by `GET /api/v2/subscriptions/{id}/events`; query/selector stays in the authenticated body and `Last-Event-ID` binds its digest.
- Cursor-paged worktree discovery/association/reference/cleanup-intent reads; read-shaped association diagnosis and cleanup inspect; expected-version association confirm/reject/reassign; and confirmed cleanup request/status. The generated route set contains no create/provision-worktree operation and no client path deletion body.

## 13. Dashboard and Brain behavior

The shell scope control is hierarchical but does not mirror store directories:

```text
All TraceDecay
├── Saved systems
│   └── Rsbuild + Rspack + React Router
├── Repositories
│   ├── rstackjs/rsbuild
│   │   ├── main checkout
│   │   └── worktree codex/...
│   ├── web-infra-dev/rspack
│   └── rstackjs/rsbuild-plugin-react-router
├── Activity without repository attribution
└── Unavailable/stale scopes
```

UX requirements:

- All is the default Observatory/Brain view; individual project is a zoom, not a separate product.
- Command palette accepts project, remote, path, branch, PR, session, agent, and symbol handles.
- Scope chips show inclusions/exclusions and exact worktree/ref snapshot.
- Same-name repositories show owner/path/head rather than ambiguous display names.
- Cross-project results use color/shape redundantly and retain accessible text labels.
- Coverage inspector explains skipped/unavailable/stale/redacted scopes and how to repair them.
- Selecting a result from another repository never changes global scope invisibly; it opens an inspector or offers an explicit focus/expand action.
- Saved systems/project sets are shareable by stable ID and may define default cross-repository graph lenses.
- Timeline can follow an agent as it moves between worktrees and queries sibling repositories.
- All/Brain renders one repository with node-scoped checkout/worktree children and a topology overlay; it never duplicates the repository because a laptop and server use different paths.

## 14. Agent proximity and hint integration

Agent presence and work claims use repository/worktree/ref/snapshot identities, not path-string equality. Cross-project nearness includes:

- two agents touching related plugin/host repositories for the same defect.
- two worktrees on the same PR or branch ancestry.
- one agent querying an upstream API while another changes the downstream adapter.
- parent/subagent work split across repositories.
- separate support/benchmark repos validating the same candidate branch.

Hints remain opt-in-by-policy and high precision:

> Related work: agent `A` is testing `rsbuild-plugin-react-router` PR 83 while agent `B` is querying `rsbuild` dev-server APIs. Inspect `claim:wc_…`; coordinate only if your artifact or test scope overlaps.

The hint never dumps other prompts or adds sibling repositories to the query silently. It provides stable anchors, overlap reason, freshness, and one action.

## 15. Errors and one-step recovery

Errors are typed domain values shared across transports:

| Error | Required payload |
|---|---|
| `ScopeAmbiguous` | candidates, match evidence, discriminators, ready-to-submit selectors. |
| `ScopeNotFound` | parsed target, fields searched, related authorized candidates, discovery freshness. |
| `ScopeUnavailable` | domain/store unavailable reason, healthy domains, repair/refresh action. |
| `SnapshotNotFound` | requested ref/as-of, candidate commits/generations, current indexed snapshot. |
| `ScopeStale` | index and source watermarks, observed drift, refresh request, stale-read option. |
| `PartialCoverageDenied` | unavailable/skipped scopes and exact request to permit partial. |
| `CrossDomainRefUnroutable` | original ref, catalog generation, tombstone/migration evidence; this is a correctness bug for live retained entities. |
| `CostLimitExceeded` | estimated shards/nodes/bytes/time, pruned alternative, resumable export option. |
| `PermissionDenied` | redacted scope identity when allowed, policy reason code, no existence leak beyond policy. |

No error tells the agent to update/restart unless protocol/catalog handshake proves that is the actual remedy. No error recommends a command whose preconditions fail.

## 16. Performance and operational limits

All-scope is not implemented by opening every database and concatenating rows:

- Catalog statistics and domain watermarks prune shards before open.
- Profile activity answers global session/agent/tool/message discovery without project fan-out.
- Project aggregate projections answer common All dashboards.
- Scope sets cache immutable resolution by catalog generation.
- Federated plans cap concurrent shard readers and use cancellation/deadlines.
- Per-shard results are bounded; global top-k merge is deterministic.
- Large exact exports become manifest-backed jobs with progress and resumable chunks.
- Query explain reports planned/visited/pruned/skipped shards and estimated/actual cost.
- Hot hook paths never perform federated fan-out; they read a bounded precomputed nearby/capability snapshot.
- Remote plans route to one current authority or verified replica/cache per shard under the requested consistency. They never mount or inspect remote database files; network loss, stale cache, and pending local observations are explicit coverage dimensions.
- Worktree discovery is incremental over changed Git admin/common-dir generations and activity/delivery outbox ranges. Candidate scoring, reference projection, stale/orphan discovery, and eligibility are bounded/indexed by repository/worktree/task/attempt/branch/PR; no request scans every checkout or task.

Targets at the current local corpus scale:

- exact stable-ID route p95 <= 20 ms without payload load.
- exact project/worktree resolution p95 <= 30 ms.
- token-aware registry search p95 <= 75 ms over 10,000 registry rows.
- warm two-repository lexical query p95 <= 250 ms for top 20.
- warm All aggregate dashboard p95 <= 300 ms.
- cancellation observed by every shard within 50 ms.
- all responses remain correct when one shard is corrupt, locked, migrating, absent, stale, or version-incompatible.

Targets are benchmark gates, not architectural promises; adjust only from measured corpus evidence.

## 17. Privacy and authorization

- Scope resolution filters candidates before rendering; fuzzy/related matching must not reveal unauthorized names or paths.
- Cross-profile queries are forbidden by default and require a separately authenticated federation design.
- Repository remotes redact embedded credentials and private host details according to output audience.
- Agent proximity summaries use safe declared summaries, not raw prompts or hidden reasoning.
- Search indexes and caches remain inside the source privacy/key domain.
- Saved project sets cannot grant access; they resolve only authorized members at execution time and report redacted omissions.
- Export manifests record scopes, redactions, policy version, watermarks, and requester identity.

## 18. Evaluation corpus and experiments

### 18.1 Named frozen Rspack/Rsbuild/React Router regression slice

Create redacted fixtures for:

1. “Find how Rsbuild reads server bundles from Rspack memory FS.”
2. “Compare plugin code with upstream Rsbuild API and Rspack implementation.”
3. “Which repository owns this symbol/API?”
4. “Follow the agent from plugin worktree to upstream queries and benchmark repo.”
5. “Find all sessions related to PR 83 across source, benchmark, and support repos.”
6. “Show downstream tests affected by an upstream Rsbuild/Rspack change.”
7. “Load the exact session returned by All-project message search.”
8. “Resolve `rsbuild rspack` despite token separation.”
9. “Distinguish `react-router`, `rsbuild-plugin-react-router`, and generated fixture copies.”
10. “Query a dirty feature worktree, not the main checkout graph.”

Fixture promotion runs plan 18 secret scanning and plan 15's sanitization-receipted promotion command; raw private transcript or corpus content is never committed.

This pack is one named frozen regression slice, not the sole scope/product conformance authority. The full corpus must also include unrelated ecosystems and repository shapes: independent repositories, monorepo-plus-package scopes, upstream/fork/downstream relations, non-Git projects, missing provider activity, dirty and detached worktrees, unavailable stores, and authorized/unauthorized neighbors. Live Rspack/Rsbuild/React Router checkouts, stores, or provider partitions are optional and never gate conformance; the redacted fixture remains mandatory.

### 18.2 Scope resolution labels

For each real prompt, label:

- intended scope set.
- acceptable related suggestions.
- prohibited silent expansions.
- expected snapshot/ref/worktree.
- expected domain availability.
- correct abstention/ambiguity behavior.
- minimum decisive evidence anchor.

Metrics:

- exact-scope resolution accuracy.
- wrong-project/worktree/ref rate.
- mean correction/retry calls before useful query.
- cross-project omission rate.
- successful search-to-exact-load rate.
- stale/partial disclosure accuracy.
- useful related-scope suggestion precision.
- duplicate/fork/generated-copy rate.
- p50/p95 latency, opened shards, bytes/tokens, and peak memory.

### 18.3 Adversarial matrix

- same repository name under different owners.
- renamed/moved repository with old aliases.
- main checkout plus many parallel worktrees.
- detached HEAD and deleted branch.
- dirty worktree whose graph covers only base commit.
- stale registry row and missing checkout.
- duplicate legacy stores.
- repo without code index but with sessions/facts.
- session touching zero, one, and several repositories.
- copied subagent prompts in several stores.
- corrupt/locked/migrating project shard.
- 10,000+ registry rows with capped output.
- path case/symlink/mount differences on supported platforms.
- unauthorized private repository adjacent to authorized public projects.
- externally created agent/user/executor/Git/IDE worktrees with missing or conflicting creator provenance.
- one worktree inferred for two tasks, rejected then reassigned, or tied across branch/PR evidence.
- archived work item and merged PR triggers arriving duplicated, reordered, before association confirmation, or while a shared reference remains.
- dirty/untracked worktree, active lease/holder, unpushed commit, unmerged commit, open/stale PR, unreachable remote, and unknown Git admin state.
- stale path with live Git admin record, orphan admin record with missing path, changed inode/common-dir between inspect and request, crash during cleanup, and inferred association without cleanup grant.

## 19. Implementation slices

Integrate these into the parent PR sequence; do not create a second parallel roadmap.

### Companion requirements for PR 3R/33R — Split-store identity reconciliation

- Import #425's canonical path, source-family freeze, holder/write-reservation, dual-backup, deterministic-confirmation, table-disposition/collision, remapped-LCM-edge, verification, ledger, marker/registry, and doctor-recovery fixtures under the one plan-12 controller.
- Prove an explicit repository/project/worktree/ref/session selector returns both conflicting candidates plus per-domain coverage until the reconciliation receipt publishes; it never bypasses the conflict by choosing the current path or an empty lane.
- After atomic cutover, replay the exact branch/session/search-to-load recipes and preserve old store/project IDs as routed aliases/tombstones rather than orphaning their retrieval anchors.

### Companion requirements for PR 8A — Canonical scope resolver

- Add `ScopeSelectorV2`, repository/project/checkout/worktree/project-set IDs, aliases, match evidence, ambiguity, and typed errors to domain/catalog.
- Import legacy identity/adoption fixtures.
- Add token-aware and relationship-aware registry resolver with generated CLI/MCP/API schemas.

### Companion requirements for PR 12B — Federated planner and routed retrieval

- Resolve selectors to domain shards/snapshots.
- Add shard pruning, partial/stale coverage, deterministic distributed cursor, and global stable-ID routing.
- Prove All-search result to exact session/message/entity load across project boundaries.

### Companion requirements for PR 17A — Profile activity and temporal attribution

- Remove public query dependence on provider `project_key`.
- Project per-observation CWD/worktree/ref/explicit-query attribution and activity evidence roles.
- Backfill without duplicating transcript bodies across project shards.

### Companion requirements for PR 18A — Code graph generation federation

- Add snapshot selection, working-tree overlay state, cross-repository edge contracts, normalized merge, and source/freshness explanations.
- Ship the Rspack/Rsbuild/React Router code-query fixtures.

### Companion requirements for PR 19A — Delivery and project-system relations

- Relate worktrees, branches, commits, PRs, forks, support repos, benchmarks, published artifacts, and upstream/downstream project sets.

### Companion requirements for plan 24/11/26 — Task/worktree lifecycle

- Ingest external worktree inventory plus hook/tool/CWD/thread/attempt/branch/commit/PR evidence into deterministic candidate correlation with provenance, confidence, ambiguity, idempotent infer/confirm/reject/reassign, and reconciliation/backfill.
- Add canonical association/reference/eligibility projections and archive/merged-PR trigger consumers over the existing task graph/outbox. Blocked cleanup remains visible in ticket/PR diagnostics and observability; it does not become task triage.
- Expose paged ticket-related worktrees, association decisions, cleanup inspect/diagnose/request/status, and lifecycle SSE deltas through generated API/CLI/MCP/dashboard bindings. TraceDecay creates no worktree; separate cleanup authorization and current safe-policy evidence are mandatory.
- Test dirty/active/unpushed/unmerged/shared/unknown blockers, CAS/idempotency, daemon re-probe, crash reconciliation, stale/orphan retention, branch preservation, stable output, and transport parity.

### PR 24G — Transport parity and agent ergonomics

- Generate common scope flags/objects/errors for official API, CLI, MCP, and SDKs.
- Add capability discovery and one-step retry payloads.
- Remove active-CWD routing from globally addressable retrieval refs.

### PR 25B — All/system scope UI

- Ship hierarchical scope picker, saved project systems, coverage inspector, disambiguation, explicit focus/expand behavior, and deep-link state.

### PR 31L — Scope and federation lab

- Replay resolution, shard plan, snapshot selection, ranking, related expansion, partial failure, and transport rendering from the same fixture.
- Compare candidate resolvers/rankers without mutating registry, stores, facts, activity, or policy analytics.

## 20. Definition of done

- [ ] One versioned `ScopeSelectorV2` and one resolver implementation feed every transport.
- [ ] Every public operation declares supported scope kinds through capability discovery.
- [ ] Explicit targets never fall back to current project/worktree/ref.
- [ ] Multi-term repository queries resolve token-wise and produce useful disambiguated candidates across unrelated repository naming schemes; the frozen `rsbuild rspack` fixture covers one named case.
- [ ] A heterogeneous project set spanning at least three repositories can be saved and queried in one request without requiring any named live checkout or provider partition; the frozen Rspack/Rsbuild/React Router set remains regression coverage.
- [ ] All-project session search results load exactly by stable ID without changing CWD or supplying a store-local key.
- [ ] Sessions/Turns moving across repositories and worktrees retain temporal attribution.
- [ ] Code queries name exact repository/worktree/ref/snapshot and expose stale/dirty/partial state.
- [ ] Externally created worktrees are discovered from Git and activity/delivery evidence with creator/source provenance, deterministic candidate scores, ambiguity, confirmation/rejection/reassignment, and reconciliation/backfill; no TraceDecay create/provision operation exists.
- [ ] Ticket/task and PR views show typed related worktrees, ownership/cleanup-grant state, reference counts with underlying subjects, and cleanup eligibility/diagnostics without creating triage work.
- [ ] Archive and merged-PR triggers are idempotent eligibility inputs. Dirty, active, unpushed, unmerged/open-PR, shared-reference, ambiguous, stale, unknown, and missing-grant states block request-cleanup.
- [ ] Cleanup is daemon-only, expected-version/idempotent, re-probes identity and blockers, preserves branches, and emits intents/receipts/failures/reconciliation evidence. Clients never delete a supplied path.
- [ ] Cross-repository edges are typed, versioned, evidence-backed, bounded, and explainable.
- [ ] Same-name, moved, deleted, duplicate-store, no-index, corrupt-shard, and unauthorized cases pass.
- [ ] #425/V2 split-store consolidation preserves both backups and remapped edges, publishes one verified marker/registry route, then resolves every former selected/legacy ID and exact session/branch recipe without CWD/path guessing.
- [ ] CLI, MCP, HTTP, generated SDK, dashboard, and hook capability metadata pass conformance snapshots.
- [ ] Errors include candidates and one executable retry request; impossible remediation is prohibited.
- [ ] All scope is bounded, cancellable, resumable, and truthful about searched/skipped/unavailable/redacted coverage.
- [ ] Search-to-retrieval, query-to-export, and result-to-source-observation chains retain stable anchors.
- [ ] Real chronological/project/provider holdouts meet wrong-scope, useful-suggestion, and latency gates before default cutover.
- [ ] V1 active-project/store-local routing is removed at cutover; retained V1 data remains read-only for rollback without stale-client emulation.
