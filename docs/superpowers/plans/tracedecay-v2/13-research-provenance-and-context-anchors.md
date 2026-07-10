# TraceDecay V2 Research Provenance and Context Anchor Plan

> **For implementation agents:** Use this document before re-running broad discovery. Recover the anchored evidence first, verify its current coverage/freshness, then update the manifest rather than replacing history with an unanchored summary.

**Goal:** Make every architectural claim, failure lesson, user-intent conclusion, subagent research contribution, and future implementation decision retrievable from stable TraceDecay session/thread/message/agent/workflow/Git anchors.

**Non-goal:** Commit private transcript content, rely on expiring response handles, claim that a search query is a stable identity, or pretend current subagent attribution is more precise than the evidence supports.

## 1. Why this is required

This planning run exposed the exact failure the anchor model must solve:

- The parent planning thread and many child sessions are now searchable.
- Coordination/system records such as `Codex sub-agent started: /root/plan_domain_store_crates` appear copied into multiple child sessions.
- Current child session metadata often has `parent_tool_use_id: null`.
- The same parent request/title is copied into child transcripts, so title or `role=user` is not proof of human authorship or task ownership.
- `sessions_for` returns no branch-correlated session for the active planning branch or PR #410; `workflows` returns no run despite known agent work.
- Search results are ranked, capped, live-changing projections. A query string can rediscover an anchor, but cannot replace a stable row/entity ID.
- Response handles are explicitly expiring and therefore cannot be the only citation in a multi-month rewrite plan.

V2 must preserve useful uncertainty: “candidate child context recovered by artifact evidence” is better than a false exact assignment.

## 2. Stable anchor contract

```rust
pub struct ResearchContextAnchorV1 {
    pub anchor_id: ResearchAnchorId,
    pub purpose: LogSafeText,
    pub provider: ProviderId,
    pub host: Option<HostInstanceId>,
    pub session_id: SessionId,
    pub thread_id: Option<ThreadId>,
    pub turn_id: Option<TurnId>,
    pub message_id: Option<MessageId>,
    pub source_store_id: Option<SourceStoreId>,
    pub agent_instance_id: Option<AgentInstanceId>,
    pub parent_session_id: Option<SessionId>,
    pub parent_tool_use_id: Option<ToolInvocationId>,
    pub workflow_run_id: Option<WorkflowRunId>,
    pub workflow_agent_label: Option<WorkflowAgentLabel>,
    pub goal_id: Option<GoalId>,
    pub project_id: Option<ProjectId>,
    pub repository_id: Option<RepositoryId>,
    pub worktree_id: Option<WorktreeId>,
    pub ref_id: Option<RefId>,
    pub commit_id: Option<CommitId>,
    pub pull_request_id: Option<PullRequestId>,
    pub occurred_window: Option<TimeInterval>,
    pub source_observation_ids: Vec<ObservationId>,
    pub evidence_class: EvidenceClass,
    pub confidence: Confidence,
    pub expected_subject: LogSafeText,
    pub retrieval_recipe_id: RetrievalRecipeId,
    pub snapshot: VectorWatermark,
    pub coverage: CoverageReport,
}
```

Rules:

- Provider-native session/message/turn/tool/goal/run IDs remain aliases and retrieval keys; canonical IDs do not erase them.
- A message anchor requires stable native/message/store identity. Text, timestamp, ordinal, or content hash alone is only a candidate matcher.
- A subagent-task anchor requires provider-declared parent/tool/agent linkage or an evidence assertion. Copied system text is not direct ownership evidence.
- Git correlation names produced, observed, encountered, branch-active, or time-overlap relation explicitly.
- Every anchor stores the captured store/index/ref watermarks and a coverage report. Re-resolution reports drift; it does not mutate the old claim.
- Secret/sensitive content is never embedded in the anchor. Authorization is re-evaluated when resolving payloads.
- `ResearchAnchorId` is durable. Response-handle IDs, browser URLs, search ranks, and temporary filesystem paths are optional hints only.

## 3. Research bundle manifest

```rust
pub struct ResearchBundleManifestV1 {
    pub manifest_id: ResearchManifestId,
    pub schema_version: SchemaVersion,
    pub created_at: UtcMicros,
    pub created_by: ActorRef,
    pub parent_plan: DocumentRef,
    pub repository: RepositoryRef,
    pub base_commit: CommitId,
    pub plan_commit: Option<CommitId>,
    pub catalog_digest: CatalogDigest,
    pub store_watermarks: VectorWatermark,
    pub private_corpus: Option<PrivateCorpusManifestRef>,
    pub git_snapshot: GitTruthManifest,
    pub anchors: Vec<ResearchContextAnchorV1>,
    pub agent_contributions: Vec<ResearchContributionV1>,
    pub unresolved_attribution: Vec<AttributionGap>,
    pub retrieval_recipes: Vec<RetrievalRecipeV1>,
    pub redaction_report: RedactionReport,
    pub digest: ManifestDigest,
}
```

The manifest is append-only/versioned. A later implementation agent adds a new version when sessions are backfilled, PRs merge, refs move, or attribution improves. It never edits an earlier evidence class from inferred to observed without a superseding assertion.

## 4. Current planning anchor registry

These are retrieval IDs, not quoted transcript content.

### 4.1 Parent planning thread

| Purpose | Provider/session anchor | Retrieval |
|---|---|---|
| Total rewrite/redesign request, additive user corrections, lead synthesis, plan edits, verification and publication | Codex session `019f4906-a411-7a11-ad3f-0d58deb0e847` | `lcm_load_session` by exact session ID; `message_search` with this `parent_session_id` for child discovery. |

### 4.2 Planning and review child sessions

| Contribution/artifact evidence | Session anchor | Recovery status |
|---|---|---|
| Early architecture/dashboard mutation-parity review | `019f490d-a83e-79d2-86ad-e797a112a6e3` | Direct assistant finding recovered; exact collaboration task relation should be rechecked. |
| Early historical/theme audit queries | `019f490d-5f3c-71b0-a0c4-18478c410d74` | Tool-query evidence recovered; task label not treated as canonical. |
| Capture and projectors crate plans; provider/Turn/workflow/goal evidence | `019f4933-0ae3-7463-b1e4-c0905b042b86` | Artifact/tool evidence; current metadata identifies Codex subagent nickname `Tesla`. |
| Query and policy crate plans | `019f4933-2dd7-79d3-9dda-5f2de386404d` | Artifact/tool evidence; parent session known. |
| Hooks and tool-catalog crate plans | `019f4940-790d-77b2-8faf-c67c0cbb95fa` | Artifact/tool/final-result evidence; nickname `Maxwell`. |
| Application and API crate plans | `019f4940-a3ec-7502-884a-dbb28b1adbf0` | Artifact/tool evidence; nickname `Gibbs`. |
| Dashboard/frontend plan | `019f4940-c336-7e02-b3e2-0f6a3836639e` | Direct final-result and artifact evidence. |
| Backend 01–08 cross-review | `019f494b-bb6c-7271-af99-2e177b915cf8` | Artifact/tool evidence and explicit reviewer scope. |
| Root compatibility/migration plan | `019f4951-47c1-7640-8d20-7eda62cbb984` | Direct assistant progress plus artifact evidence. |
| Application/API/frontend cross-review | `019f4952-0231-7093-90dd-7ab2773a7493` | Artifact/tool evidence and explicit reviewer scope. |
| Primary-source retrieval/search research and real-world evaluation design | `019f4964-ebb8-7112-975a-6f2f4bca17a8` | Direct final result with linked primary sources, metric/holdout design, and implementation recommendations. |
| Official public API/SDK plan and agent-direct contract research | `019f496a-fae5-7ff3-a301-f4f7e59fe4db` | Direct artifact/tool/final evidence; plan 17 is the bounded output. |
| Private-corpus secret-safety audit and scan remediation | `019f4975-6869-78c2-9f23-dbfa7df6f524` | Direct scanner/result evidence; private corpus remains outside Git and the plan records counts and digests, never matched values. |
| Existing redaction-path and bypass audit | `019f497e-73a2-7702-b247-0bf0703ef6ef` | Direct source/audit evidence for plan 18's fragmented-detector and bypass inventory. |
| Primary-source secret detection, pseudonymization, logging, and key-lifecycle research | `019f497e-9178-7631-9349-1ab7f8b4da9d` | Direct research/final evidence; plan 18 is the bounded output. |
| System defragmentation, convergence, and extension architecture | `019f4984-a11d-7850-94b4-fa130da08e95` | Direct artifact/tool/final evidence; plan 19 is the bounded output. |
| Backend plans 01–08 convergence review | `019f4984-e2c8-7fb3-ae59-7feebcd084cf` | Explicit reviewer scope plus artifact/diff evidence; final lead review resolves any remaining cross-plan issue. |
| Application, API, frontend, search, scope, and privacy convergence review | `019f4985-045c-7d72-a1d9-c9029d5a8eef` | Explicit reviewer scope plus artifact/diff evidence; final lead review resolves any remaining cross-plan issue. |
| Final whole-system architecture coherence audit | `019f4997-4a3d-7ed2-bbc6-d0cce8ae041d` | Read-only 21-file flow/ownership/contract audit; exact findings drove the final anchor/thread/adapter/query/privacy/route corrections. |
| Final plan publication-quality audit | `019f4997-6c24-7451-a2e8-688d2ddd86de` | Read-only 21-file numbering/type/client/cutoff/current-state audit; exact findings drove the final PR/client/native-row/baseline corrections. |
| Configuration control-plane plan | `019f49ba-73ba-7483-9cc0-4226ab4bae8c` | Provider-declared child session `/root/plan_configuration_control_plane`; plan 20 is the artifact, including redactor controls and autonomous-curation policy. |
| CLI/MCP/output unification, Hermes Kanban audit, and canonical task-graph plan | `019f49c0-0d00-7210-bb9f-1085a4635007` | Provider-declared child session `/root/plan_cli_mcp_surface_unification`; plan 21 plus the official/local Hermes research and plan 24 are bounded outputs from successive tasks in the same child thread. |
| Current CLI/MCP/source inconsistency audit | `019f49c0-3992-7551-b9b4-764217ee5a84` | Provider-declared child session `/root/audit_cli_mcp_inconsistencies`; read-only 104-MCP/CLI/dashboard/renderer evidence drove plans 14/21. |
| Incremental Context Scout and suggestion-envelope plan | `019f49ca-265b-7771-b062-989e43c577f3` | Provider-declared child session `/root/plan_incremental_context_scout`; plan 22 is the artifact and includes task/material-sibling integration. |
| Session/LCM temporal retrieval audit and plan | `019f49cc-f04b-7990-a4c7-5f44856d7fae` | Provider-declared child session `/root/plan_session_temporal_retrieval`; plan 23 and its twelve live failure cases are the bounded outputs; the same child later performed an independent task-graph review. |

The initial domain/store author session is not assigned here with false precision. Current LCM copied coordination events into multiple child sessions and left `parent_tool_use_id` null. The plan files and parent thread preserve the work; V2 must repair this attribution class before claiming an exact child owner.

### 4.3 Private chronological corpus

The corpus itself remains outside Git:

- Manifest: `/fast/tracedecay-redesign-research/manifest.json`.
- Secret-scanned/redacted native `role=user` corpus: 34,305 rows; SHA-256 `ff55fb9158a111a7ad28e2c448784f0d942987cd505576c4d03643a2a74a4429`.
- Secret-scanned/redacted best-effort human subset: 9,941 rows; SHA-256 `18fa47e16340177d2996674018d66e81625d234f12c25eadbb4d904dec6aa458`.
- Frozen cutoff: 2026-07-09 23:15:42 UTC.
- Both primary files and manifest are mode `0600`.

This is a private corpus reference, not a distributable PR fixture. `gitleaks 8.30.1` and parsed-value credential detectors were run; conservative redaction removed marker/credential-shaped values and examples while preserving row identity/order. An authenticated-URL alert from serialized-line scanning was rejected as a cross-field false positive after parsed-value validation. Phase 0 derives separately reviewed synthetic/minimal-redacted regression fixtures; it never promotes this corpus directly.

### 4.4 Git and delivery anchors

| Subject | Stable anchor or query | Evidence note |
|---|---|---|
| Publication-base master | commit `9f7a110805edf226bb0d665d6f4ff5c4f03c6163` | Includes merged #415/#417/#419/#420/#422 at crate version 0.0.47; the plan branch is rebased to this or a newer accepted base before final checks. |
| Legacy store adoption | PR #405; merge commit `e35279586d6a0886856a26842ef17ce51e83da05` | Current-master migration input. |
| Hermes user-profile consolidation | PR #407; branch `codex/hermes-user-profile-only` | Open future-master input. `sessions_for` returns historical branch-active sessions; latest exemplars include `019f3ff1-7f85-7812-8255-77481331c0a9` and `019f3ff1-d87f-7f40-9cff-275e15bf589a`. |
| Copied subagent prompt query semantics | PR #410; head `a40b01f714359759b3d0d0ae0c746ad00ef7e72f`; master commit `f4494c3ad7c354637ed5cafde7ad43af8926ca9b` | Merged current-master input; historical `sessions_for`/`workflows` zero remains a capture/correlation coverage fixture. |
| Foreign skill ownership/remediation | PR #411; head `35350972439090f6a5279e521a3c70d59427967f`; merge `e0b3cc36a355b1fcddf87b0b08f49a69ded8585d` | Merged current-master input. |
| Safe daemon upgrade drain | PR #412; merge commit `99ad19bc12b817f9959f740c40f0dbd5e286f16c` | Current-master lifecycle invariant. |
| Releases containing audited fixes | PR #413 merge `bd8fd012fe5e7980c2c308b18c47b7493ddc702f` (v0.0.46); PR #416 merge `9709866100bb29ad630ea5852b40e525fe13f72d` (v0.0.47) | Current-master packaging/version inputs; release PR layout is not an architecture source. |
| Semantic move-symbol capability | PR #414 merge `cd5ef58ccb165fb1df84f98a31a1db880957e299` | Generated capability/tool/API parity and safety/preview/impact fixture. |
| Release PR integrity guard | PR #415; merge `6b339ea06878e2c8fce703c839184a5bd21c7159` | Merged publication-integrity base input. |
| Identity split visibility | PR #417; merge `bccb6bea38adf18dfb0cf0f8987c144fc73f6a37` | Merged status/reconciliation base input; matches the plan-19 live split-store probe. |
| Pending 0.0.48 publication | PR #418; branch `release-plz-2026-07-10T01-03-19Z`; head `e870c4b8478205bf4ce2c00e366953d8830ff6b3` | Open/`UNSTABLE` publication snapshot for merged #414/#411 changes; import only after merge, tag, package, and digest verification. |
| Race-safe move-symbol writes | PR #419; head `109d31c3698fbd6a4b50324afd2b30feff8309f3`; merge `66584b4dbdee920204cbcf4cf42d0dbc308559e4` | Merged command/precondition/filesystem/rollback base input. |
| MCP daemon hot-swap routing | PR #420; head `7f84436ca7ab18732ff344ac9a93169e83813a68`; merge `6b05327f67cefb8e11b0ad8bca60e0f921c524e1` | Merged composition/lifecycle/current-client input: proxy authority before local store open, per-request reconnect, no uncertain write replay, and explicit new-session/tool-schema refresh boundary. |
| MCP generation-scoped tool refresh | PR #422; head `9487230ceaa46ca57aee01c45406c7bf24e29ddc`; merge `9f7a110805edf226bb0d665d6f4ff5c4f03c6163` | Merged input: negotiate `tools.listChanged`, notify a long-lived client once per daemon generation including same-version restarts, bound non-evicting client dedupe, and direct recovery at the stale host or daemon. |
| Memory FTS direction and retrieval telemetry | PR #423; branch `codex/fact-retrieval-ranking-telemetry`; head `c3b7780ea741806bf551629eed91e9323637b89a`; base `9f7a110805edf226bb0d665d6f4ff5c4f03c6163` | Open future-master input at final audit. Replaces absolute-value FTS5-rank conversion with monotonic negated-BM25 normalization; adds exact operational evidence versus unrelated V2-plan facts, rare-term coverage, explicit-search counters, untracked context enrichment, and analytics assertions. Refresh merge/CI state before implementation. TraceDecay `pr_context` could not inspect it because both explicit worktree/root requests hit the selected-versus-legacy identity cutover conflict; live GitHub plus bounded Git diff supplied the fallback evidence. |

### 4.5 Cross-project and worktree failure anchors

| Subject | Session anchor | Evidence note |
|---|---|---|
| Rsbuild/Rspack falsely treated as absent after combined lookup | `019f42c9-623a-7cc0-95c1-f073eaa05a4d` | Agent fell back to installed package sources. |
| User correction and multi-step registry recovery | `019f4323-f569-74c0-9988-ea3851d14fd7` | Project-list cap and separate searches preceded successful direct project graph queries. |
| Tokenization root cause for project search | `019f4325-57ef-7a53-b6a0-5c583c759301` | One contiguous `LIKE` pattern for `rsbuild rspack`; exact source/root-cause evidence. |
| Registered graph versus local-package fallback | `019efb4d-4508-7182-961b-9b30c739baa7` | Rspack graph found while Rsbuild was reported absent; source classes must remain distinct. |
| Cross-project copied workflow conclusion | `019f1568-f9de-75c1-9870-7cee46944adc` | Representative clustering/dedup evaluation input. |
| PR/code context resolved base checkout rather than intended worktree | `019f3edc-6a4e-7d80-b181-8f6d1e657859` | Exact explicit-worktree/ref/snapshot regression anchor. |
| Session search still constrained by provider `project_key` | `019f2538-0fd9-7362-a50b-96e36130643b` | Profile activity versus project-attribution design anchor. |
| Claude first-CWD cross-worktree misattribution | `019f2524-534d-7bd1-a3b1-675f242dcc0e` | Per-Turn/message location evidence regression anchor. |
| Missing code-index hint suppressed session/memory capability | `019f1204-5575-72a1-a2d1-ab5c6d1b310d` | Per-domain capability and hint-routing regression anchor. |

The current planning replay added one direct contract failure: `message_search(project_scope="all_registered")` found these cross-project session IDs, but `lcm_load_session` was active-project-only and rejected a project selector. Until global stable-ID routing ships, discovery snippets plus native transcript/source locators may be needed to recover the exact payload. Plan 16 makes this search-to-load sequence a cutover gate.

### 4.6 Hermes Kanban and task-graph research anchors

| Subject | Stable anchor | Evidence note |
|---|---|---|
| Registered local Hermes source | TraceDecay project `proj_99472b542e35cdb6`; root `/fast/projects/hermes-agent`; commit `732a9ffc572ad2703fbd25cc8a21c9f3f9c10d69`; package `0.16.0` | Local source/test audit anchor. It is a fork snapshot and differs materially from current upstream; do not infer latest behavior from it. |
| Official Hermes source/provenance | [NousResearch/hermes-agent](https://github.com/NousResearch/hermes-agent); audited upstream head `540f90190f50f9518bf36632a724e0e58877a10b`; MIT license/Nous Research notice | Pin repository/commit/file/access date before adapting code. Preserve license notice for copied material; prefer contract-level clean implementation where designs diverge. |
| Official Kanban reference | [Kanban feature reference](https://github.com/NousResearch/hermes-agent/blob/main/website/docs/user-guide/features/kanban.md); [v0.15 Kanban maturation record](https://github.com/NousResearch/hermes-agent/blob/main/RELEASE_v0.15.0.md) | Durable task/attempt/handoff/claim/retry/model/worktree/decomposition/swarm/dashboard behavior and evolution. Documentation is evidence, not a substitute for pinned source/tests. |
| Ambient-board ownership failure | [Hermes issue #21877](https://github.com/NousResearch/hermes-agent/issues/21877) | Documents global current-board selection causing cross-profile dispatch, writes, token spend, and notifications. TraceDecay forbids ambient board ownership and per-board canonical stores. |
| Cross-repository fan-out/fan-in usage | Hermes session `20260617_210811_5cd728` | Rspack/Rsbuild/React Router plugin evidence: five parallel triage tickets, synthesis fan-in, implementation children, multiple executor/model routes, dependencies, blockers, and board/assignee ambiguity. |
| Board/store/current-selection confusion | Hermes session `20260617_020912_188f3e` | Multiple board DBs/backups/recovery artifacts and unset board selectors; migration, scope, corruption, and UI mental-model regression anchor. |

These native Hermes session IDs currently resolve through profile-wide/provider search rather than reliably through the registered code-project shard. Treat that mismatch as plan-16/23 routing evidence. Plan 24 must retain exact source locators and migrate only sanitized task/attempt/handoff metadata with explicit ownership; it must not import old board DBs as parallel live authorities.

## 5. Retrieval recipes

### Parent or child session replay

```bash
tracedecay tool lcm_load_session \
  --session-id 019f4906-a411-7a11-ad3f-0d58deb0e847 \
  --provider codex --limit 100
```

Page with the returned `after_store_id`. Do not substitute `message_search` snippets for lossless replay.

### Discover child context under the parent

```bash
tracedecay tool message_search \
  --query 'docs superpowers plans tracedecay v2' \
  --provider codex --scope subagents_only \
  --parent-session-id 019f4906-a411-7a11-ad3f-0d58deb0e847 \
  --limit 50 --format json
```

Treat copied coordination/system records as candidates. Confirm contribution through message/tool/artifact evidence and provider-declared linkage.

### Recover Git-correlated sessions and workflows

```bash
tracedecay tool sessions_for --git-ref branch \
  --value codex/hermes-user-profile-only --relation all --limit 50
tracedecay tool workflows --branch codex/hermes-user-profile-only --limit 50
```

An empty result is recorded as capture/index/correlation coverage, never proof that no agent worked.

### Rebuild semantic/live PR context

```bash
tracedecay tool pr_context --args \
  '{"base_ref":"origin/master","head_ref":"origin/codex/session-query-dedupe","format":"markdown"}'
gh pr view 410 --json headRefOid,baseRefOid,files,statusCheckRollup,updatedAt
```

Record both heads, merge base, fetched/index timestamps, changed-file digest, coverage, and disagreement. Never cite an expiring TraceDecay response handle as the durable source.

### Recover historical failure/intent rows

Use `message_search` for discovery, then persist exact session/message/store IDs and replay with `lcm_load_session`. Queries used by this plan include:

- `disk fills graph database non-SQLite garbage`
- `doctor foreign installation skills stale update refuses`
- `structured marker version re-parses every provider transcripts`
- `git graph code graph thread graph agent graph timeline holographic memory`
- `compatibility fallbacks old MCP instance`

Search query/rank is a recipe, not the anchor.

## 6. Product and API requirements

- `GET /api/v2/research/manifests/{id}` returns safe metadata, anchor coverage, current resolution state, and authorized payload links.
- `POST /api/v2/research/anchors/resolve` resolves IDs at a frozen watermark without mutating counters or evidence.
- `POST /api/v2/research/manifests` is a preview/apply command with classification, secret scan, ownership, and audit receipt.
- Explorer, Causal Loom, Turn inspector, agent graph, Git/delivery view, Hint Lab, Evolution Studio, and plan inspector can open/copy an anchor.
- A plan/document inspector lists the evidence bundles and agent contributions that produced it, plus unresolved attribution.
- Export emits anchor IDs, native aliases, source watermarks, evidence class, coverage, and retrieval recipes; payload inclusion is separately authorized.
- If an anchor is deleted/expired/redacted, the non-content provenance skeleton and reason remain.
- Every plan implementation task starts with “resolve referenced manifest at current state” and records drift before editing code.

## 7. Phase 0 implementation task

### PR 2A: Research manifest and anchor fixtures

**Files:**

- Create `crates/tracedecay-domain/src/research.rs`.
- Create redacted `tests/fixtures/v2/research-anchor-manifest.json`.
- Create `tests/v2_corpus_suite/research_anchors.rs`.
- Extend compatibility inventory with session/message/agent/workflow/Git anchor capabilities.

- [ ] Define stable anchor and manifest schemas, evidence classes, safe display, and authorized resolution.
- [ ] Add fixtures for exact message, parent/child agent, missing parent tool-use ID, copied coordination event, workflow run, branch-active session, produced commit, observed commit, deleted/redacted payload, and expired response handle.
- [ ] Prove a copied subagent prompt or system event cannot become direct authorship evidence.
- [ ] Prove resolution is deterministic at a frozen watermark and reports drift at current state.
- [ ] Prove no secret/payload/query literal enters catalog or safe anchor export.
- [ ] Add manifest digest, supersession, redaction, retention, and deletion skeleton tests.
- [ ] Add current planning anchor manifest as a private local artifact; commit only the sanitized schema/fixture.

## 8. Acceptance gates

- Every nontrivial master-plan claim class maps to at least one stable anchor or an explicit unresolved-evidence entry.
- Every subagent-authored plan maps to a provider session or a documented attribution gap plus artifact evidence.
- No committed plan depends solely on an expiring response handle, search rank, branch name, mutable path, or unpinned remote URL.
- A fresh agent can recover the parent plan session, a child contribution, one failure case, one Git change, and one user-intent row using only this plan and supported TraceDecay tools.
- Retrieval reports exact store/index/ref watermarks and never silently falls back to another project/profile/provider.
- Research manifests are versioned, privacy-safe, exportable, and inspectable in the Brain/Explorer/Loom.

## 9. Definition of done

- The plan set index links this document and its current anchor registry.
- Master Phase 0 includes PR 2A before implementation contracts harden.
- Root migration inventory includes legacy/native session IDs, goals, workflow runs, Git correlation, response handles, and anchor coverage.
- Failure regression matrix references anchor IDs/recipes rather than untraceable prose alone.
- Current planning worktree remains plan-only; private transcript corpora are not staged.
