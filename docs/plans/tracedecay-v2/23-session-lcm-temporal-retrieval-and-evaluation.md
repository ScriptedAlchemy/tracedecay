# TraceDecay V2 Session, LCM, Temporal Retrieval, and Evaluation Plan

> **Accepted-base refresh delta (audit 29 / packet 30):** preserve compression
> replay identity (summarized messages with non-empty `lcm_summary_node_id`
> bypass raw re-ingest; PR #455) and the `user-turn-v1`→`user-turn-v2` sweep;
> **add** provider continuity across the `cursor`→`hermes` LCM-provider change so
> historical `cursor` records are migrated or intentionally queryable as two
> eras. See
> [`30-baseline-refresh-candidate-packet.md`](30-baseline-refresh-candidate-packet.md)
> §5, §7.6 and FM-165/FM-166.

> **For implementation agents:** Session recall quality claims require replayable real queries, frozen time cutoffs, stable retrieval anchors, manually judged results, explicit current-versus-historical semantics, per-stage explanations, and privacy/resource measurements. A larger index, embeddings, or a newer model is not evidence of better retrieval.

**Goal:** Make message, Turn, session, thread, agent, workflow, and LCM retrieval return the smallest useful and temporally correct context across every authorized provider, project, repository, checkout, worktree, branch, and profile shard, while preserving exact technical recall, history, provenance, privacy, stable anchors, and calibrated abstention.

**Architecture:** V2 separates immutable message occurrences from logical-message clusters, explicit temporal assertions from inferred similarity, candidate recall from truth/current-state resolution, and retrieval from context assembly. One domain `TraceQueryV1` plan runs lexical, fuzzy, semantic, entity, graph, summary-DAG, and time channels against typed documents; a temporal resolver and representative selector then produce explained, diverse results. Raw observations remain source truth. Summaries, embeddings, copied prompts, and model-generated suggestions are derived projections with exact source horizons and never become untraceable replacements.

**Decision:** Recency is a feature, never a truth rule. “Current,” “as of,” “show the evolution,” and “forensic/exhaustive” are explicit answer modes. A newer weak mention does not erase an older authoritative decision; an older exact lexical match does not outrank an explicit later correction when the user asks for current state. Contradictions remain visible, and uncertain supersession causes a conflict warning rather than a fabricated winner.

**Publication snapshot:** [master §2.6](../2026-07-09-tracedecay-brain-rewrite.md#26-current-master-accepted-changes) plus [plan 13](13-research-provenance-and-context-anchors.md) are normative. Divergent raw/session variants, bounded indexed consolidation lookup, conflict-safe registry healing, repair-free search reads, peer-safe graph checkpoints, and restart-safe retirement are required temporal-retrieval fixtures. Refresh source, open PRs, and live corpus before freezing implementation baselines.

---

## 0. Ownership and cross-plan contract

This file is the session/LCM/temporal specialization of the general retrieval plan in [`15-search-quality-evaluation-and-retrieval-research.md`](15-search-quality-evaluation-and-retrieval-research.md). Plan 15 owns shared IR research, corpus/qrel infrastructure, and promotion gates. This plan owns the additional contracts required to answer “which prior conversation matters now?” correctly: logical message identity, raw/summary lineage, validity and supersession, current-versus-historical modes, context assembly, and session-specific replay strata.

| Plan | Contract consumed or extended here |
|---|---|
| [`01-domain-crate.md`](01-domain-crate.md) | Owns canonical IDs, `ThreadId`, `TurnId`, `RetrievalAnchorId`/`RetrievalAnchorRecordV1`, bitemporal intervals, evidence, confidence, privacy, scope, the extended `CursorClaimsV1`, and the `TraceQueryV1` vocabulary including the optional `temporal` clause (`TemporalClauseV1`: `Current \| AsOf{valid_time, knowledge_time} \| Evolution \| Forensic`). This plan rides that clause and the plan 05 §6.1 registered attributes; it proposes exact session-temporal variants for that owner to define and adds no parallel AST. |
| [`02-store-crate.md`](02-store-crate.md) | Persists occurrences, logical clusters, temporal assertions, summary horizons, index manifests, judgments, replay receipts, vector watermarks, retention, and tombstones through its dedicated table families for `MessageOccurrenceV1`, `LogicalMessageClusterV1` (with retained revisions), `MessageCopyAssertionV1`, `TemporalAssertionV1`, `SummaryNodeV2`, and the activity shard's protected profile-evaluation family; plan 02 reconciles its `message_origin_assertions`/`message_representative_memberships` tables into this vocabulary and defines the keys and indexes. No query transport opens SQLite directly. |
| [`03-capture-crate.md`](03-capture-crate.md) | Captures provider-native messages, tool events, goals, locations, parent/child/workflow links, correction markers, and source order as sanitized immutable observations. It does not decide relevance or supersession. |
| [`04-projectors-crate.md`](04-projectors-crate.md) | Builds typed message/Turn/session/thread documents, occurrence/copy relations, summary/source DAGs, temporal assertion candidates, Git/worktree attribution, and representative-cluster projections with rebuildable lineage. |
| [`05-query-crate.md`](05-query-crate.md) | Owns parsing, candidate generation, fusion, temporal resolution, ranking, diversity, hydration, context assembly, explanations, pagination, and evaluation execution — all in its §5 module tree, against which §13 below states requirements. There is no second LCM query engine and no session-owned module tree. |
| [`08-tool-catalog-crate.md`](08-tool-catalog-crate.md) | Declares one generated search/context/replay/eval capability surface and effect/output metadata. Legacy `message_search`/`lcm_*` commands become compatibility bindings, not independent semantics. |
| [`09-application-crate.md`](09-application-crate.md) | Owns authorized search, context assembly, session replay, corpus/judgment, compare, and evaluation use cases; coordinates owner shards and partial coverage. |
| [`10-api-crate.md`](10-api-crate.md) | Exposes those use cases through versioned typed routes, cursors, subscriptions, problems, and generated schemas. |
| [`11-dashboard-frontend.md`](11-dashboard-frontend.md) | Owns the Search Quality Lab's session-temporal workspaces (§9), session/Turn explorer, summary-DAG and temporal lineage views, result explanations, judgments, comparisons, and saved investigations. |
| [`13-research-provenance-and-context-anchors.md`](13-research-provenance-and-context-anchors.md) | Owns durable research manifests and anchor resolution. This plan records native IDs and expiring response handles only as legacy discovery evidence until V2 anchors exist. |
| [`14-historical-failure-regression-matrix.md`](14-historical-failure-regression-matrix.md) | Owns the program-level failure ledger. Every failure class in this file gets a stable case ID and cutover receipt there. |
| [`15-search-quality-evaluation-and-retrieval-research.md`](15-search-quality-evaluation-and-retrieval-research.md) | Owns shared recall channels, qrels, metrics, ablations, and general Search Quality Lab. This plan adds session-temporal strata and gates. |
| [`16-cross-project-repository-worktree-scope.md`](16-cross-project-repository-worktree-scope.md) | Resolves immutable authorized project/repository/worktree/ref sets and federated shard routes before ranking. Query never repairs a wrong scope after retrieval. |
| [`18-secret-detection-redaction-and-private-data-safety.md`](18-secret-detection-redaction-and-private-data-safety.md) | Owns sanitization, protected content, keyed fingerprints, privacy-domain model/index isolation, query-log handling, deletion, and safe outputs. |
| [`19-system-defragmentation-convergence-and-extensibility.md`](19-system-defragmentation-convergence-and-extensibility.md) | Requires one event/query/config/catalog architecture and deletion of duplicate V1 message/LCM/search implementations after cutover. |
| [`20-configuration-control-plane.md`](20-configuration-control-plane.md) | Owns typed settings, source/effective provenance, activation, UI/CLI/MCP/API/SDK controls, privacy floors, and replayable configuration revisions. |
| [`21-cli-mcp-tool-surface-and-output-unification.md`](21-cli-mcp-tool-surface-and-output-unification.md) | Owns generated bindings, presentation/view renderers, Markdown-default/explicit-JSON rendering, stable envelopes, handles, pagination, and cross-transport parity. Plan 09 owns the semantic typed view models that every transport renders. |
| [`22-incremental-context-scout-and-suggestion-envelopes.md`](22-incremental-context-scout-and-suggestion-envelopes.md) | Consumes bounded retrieval/context envelopes for near-real-time suggestions. It cannot bypass temporal mode, privacy, coverage, attribution, or evaluation gates. |
| [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md) | Owns canonical task/plan/initiative/dependency/work-claim/executor semantics. This plan owns how those typed IDs and relations filter, rank, and assemble temporally correct context without sibling/global-board pollution. |

### Non-goals

- Do not infer or expose hidden chain-of-thought. Only provider-exposed messages, reasoning artifacts, summaries, tool events, and source evidence are eligible.
- Do not collapse history into one mutable “current message.” Occurrences and superseded assertions remain addressable subject to retention/privacy.
- Do not treat a content hash, embedding neighbor, same title, timestamp, copied prompt, or shared path as proof that two messages or sessions are identical.
- Do not make an LLM judge, summary model, or background hint model the truth source for relevance, authority, contradiction, or supersession.
- Do not make recency, retrieval frequency, click frequency, or prior hint use a self-reinforcing relevance signal without bounded evaluation and explanation.
- Do not maintain separate ranking semantics in `message_search`, `lcm_grep`, `lcm_expand_query`, dashboard search, hooks, CLI, MCP, or SDKs.
- Do not commit private transcript text, embeddings, qrels, or unredacted evaluation reports.

## 1. What current search actually does

This is a source audit, not a critique based on names or documentation.

### 1.1 `message_search`

Current anchors:

- `src/global_db.rs:626-676` defines a maximum pre-rerank fetch of 200, inventory downranking, and `session_fts_query`.
- `src/global_db.rs:4216-4400` executes the message search.
- `src/global_db.rs:1100-1145` defines `session_messages` and `session_messages_fts`.
- `src/mcp/tools/handlers/session.rs:396-590` parses filters, merges registered-project results, and builds output.
- `src/mcp/tools/handlers/session.rs:2353-2558` performs catch-up, registered-project fan-out, and selected-project routing.

Observed semantics:

1. Every whitespace token becomes a quoted prefix expression and tokens are joined with `OR`, not `AND`. A long conceptual query can match a row containing only one generic token.
2. FTS5 indexes `text`, `role`, `kind`, `model`, and `tool_names`. BM25 weights are `10, 2, 1, 1, 1`; the result score is the negated BM25 rank.
3. There is no temporal feature or current-state resolver. Timestamp is a tie-break only in the all-registered merge; single-shard search preserves BM25 order after inventory downrank.
4. There is no explicit authority, evidence, correction, supersession, contradiction, branch-state, or validity feature.
5. Hyphenated query terms add an exact lowercase substring predicate; other punctuation and phrase semantics are limited.
6. Provider, provider-level `project_key`, registered project, parent/subagent relation, message type, time range, Git branch/worktree/commit, and workflow run/agent are filters.
7. Search catches up provider transcripts by default. A nominal read can therefore perform ingestion unless `catch_up:false` is explicit.
8. Inventory/listing text is downranked after a 4x over-fetch. Related copies are deduplicated only when provider, parent/child family, and normalized content align; repeated rows inside a session and copies across unrelated/missing-link sessions, providers, or project shards remain.
9. `project_scope:all_registered` asks each shard for its local top K, then compares raw BM25 scores across independent corpora. Those scores are not calibrated across shard document frequencies, so “exact distributed top K” is not justified by matching sort keys alone.
10. Results contain full session/message records in JSON and a score without component explanation. They do not contain a durable V2 retrieval anchor, cluster identity, current/stale state, source coverage, or next cursor.
11. Limits clamp to 1–50. All-registered search reports searched/skipped project counts, but not a per-shard coverage/disposition ledger in the compact human result.
12. JSON output can exceed the global response limit and becomes an expiring response-handle envelope; Markdown is compact but still lacks paginated continuation and stable result hydration.
13. Merged PRs #445/#448 added a separate selected-profile user-session `message_search` dispatch and query-coupled catch-up when compatibility `storage_scope=user` is explicit. Project bypass/profile selection are parity fixtures; the sentinel, CLI/MCP allowlists, process-local singleflight, direct read-only DB open, optimistic `catch_up_performed`, and dedicated handler are V1 red fixtures. V2 queries one Profile root, retains `DeclaredScope::Profile`/`DeclaredScope::ZeroProject` provenance, uses one side-effect-free search application path, and exposes freshness only through an explicit durable refresh operation.
14. The 2026-07-11 catch-up audit quantified the hidden cost: four concurrent all-registered searches exceeded 397 seconds while the same read with `catch_up:false` took 1.85 seconds; 31 destinations repeatedly scanned a 1.15 GiB Hermes source, implying at least 35.7 GiB read per request before concurrency. This is FM-153. PR #447's merged V1 singleflight/multi-destination patch and PR #448's user-search catch-up are differential evidence only; V2 makes reads side-effect free, reports truthful coverage, and delegates explicit freshness to plan 09's durable operation kernel.

### 1.2 `lcm_grep`

Current anchors:

- `src/sessions/lcm/schema.rs:13-78` defines raw content-only FTS.
- `src/sessions/lcm/schema.rs:220-245` defines summary-node FTS over summary text, expand hints, and metadata.
- `src/sessions/lcm/query.rs:380-500` combines raw and summary candidates and applies inventory/session caps.
- `src/sessions/lcm/query.rs:1700-1955` executes raw and summary FTS/LIKE paths.
- `src/sessions/lcm/query.rs:3494-3542` defines relevance, hybrid, and recency order.

Observed semantics:

1. Raw LCM FTS is content-only. Role/source/time/relationship/Git filters are SQL predicates.
2. The default is relevance: BM25 rank first, user before assistant before tool as a tie feature, and recency after that.
3. Hybrid divides the negative BM25 rank by an age denominator using a fixed `0.001` per-hour decay. Recency is an explicit separate sort.
4. Raw and summary candidates are not calibrated in one candidate pool. Raw hits are fetched first; summaries are appended only if raw hits leave budget. For relevance/hybrid the later Rust sort does not compare raw and summary scores.
5. Scope `all` caps each session at three hits and reserves a tool-action slot when narration otherwise fills the cap. The response reports capped sessions.
6. Summary results disappear when raw-only filters such as role, time, or message type are active, because summary projections cannot prove those fields directly.
7. CJK, emoji, risky punctuation, and malformed quote shapes take a LIKE fallback with different ranking properties.
8. Parent/child copy dedup has the same bounded family/content rule as message search. Summary-source duplicates and copies across shards/providers remain separate.
9. Maximum page size is 100; there is no federated all-registered LCM search or stable cross-project discovery-to-load route.

### 1.3 `lcm_expand_query` and replay

Current anchors:

- `src/sessions/lcm/query.rs:647-795` assembles expand-query context.
- `src/sessions/lcm/types.rs:371-430` defines the request/response.
- `src/mcp/tools/handlers/session.rs:1-50` defines current content/prompt/result caps.

Observed semantics:

1. The request requires one provider and one provider-local session ID. It cannot start from a cross-project search result without the caller manually routing to the correct store.
2. Query selection is session-scoped and hard-coded to recency.
3. Summary candidates are selected before raw candidates. A summary can consume the result/context budget before the exact source message is considered.
4. Explicit `node_ids` bypass query selection and expand summary nodes directly.
5. `context_max_tokens` is currently used as a character budget (`context_max_chars`), so naming and accounting are not semantically exact.
6. The response exposes context truncation/pagination, but synthesis availability changes whether the host receives an answer or raw context.
7. No-match is a successful answer string, not a calibrated distinction among “no relevant evidence,” “wrong shard,” “not ingested,” “retained/locked,” “summary horizon missing,” or “query too narrow.”
8. Session replay returns head, tail, and deepest/recent summary nodes, but does not choose query-relevant Turns, correction chains, Git/code impacts, or adjacent agent/workflow evidence.

### 1.4 Current evaluation coverage

`tests/fixtures/message_search_eval_labeled.json` contains 12 synthetic queries: 10 positive and 2 negative. The live test recomputes Precision@1, Precision@3, and MRR. It covers branch-inventory downrank, relevance-over-recency for a rare synthetic term, provider/project filtering, assistant-versus-tool noise, multi-term coverage, case-insensitive prefix match, exact single hit, and absent/hyphen negatives.

That fixture is useful but insufficient:

- all positive queries currently have one desired first result or a tiny synthetic set;
- the corpus has no real corrections, supersession chain, current-versus-historical query, logical copy cluster across shards, stale summary horizon, cross-project routing, partial shard, semantic paraphrase, user-prompt recall, subagent duplication, or result-budget failure;
- it does not grade relevance 0–3, nDCG, Recall@K, calibration, duplicate rate, temporal accuracy, privacy, latency, tokens, cost, or per-project/provider strata;
- known live probes in Plan 15 are discovery evidence, not frozen qrels.

No production-default embedding or dense message-search channel was found in this audit. V2 must evaluate optional local semantic channels; it must not describe current lexical/LIKE behavior as semantic retrieval.

## 2. Real local failure evidence

The evidence table records safe native identifiers and behavior only. Resolve content through authorized tools and a Plan 13 research manifest; do not copy private payloads into fixtures.

| Case | Legacy retrieval anchor | Observed failure and required regression |
|---|---|---|
| `TD-SR-001 oversized-federated-json` | Response handles `rh_b99a83d4f0f58795e992c3dc` (`project`, 118,679 chars), `rh_e2b438b0decc65bfb5f32345` (`master`, 112,575), `rh_83d1cef2021b1be6d7d07d91` (`tracedecay`, 332,881), `rh_071bbb473324a37785b188ef` (`search`, 97,702) | Fifty federated JSON hits produce 97K–333K payloads and an expiring handle instead of a stable page. V2 returns compact typed results, stable anchors, coverage, and a cursor; hydration is explicit/batched. Handles are probe receipts only and may expire. |
| `TD-SR-002 tool-self-echo` | Hermes session `20260613_052858_6e6a5245`; Codex sessions `019ef660-82e4-7da2-a830-6cbddea82df5`, `019ef682-e7c9-7322-a0c5-c66d1e67cc66` | Exact search for `tracedecay_message_search` is dominated by generated tool handlers, tool definitions, tool-name rows, and prior calls. V2 intent/origin fields preserve explicit tool-history search while default intent recall penalizes schema/call self-echo. |
| `TD-SR-003 copied-agent-coordination` | Parent `019f19af-06d7-7ed1-a4d2-87516c0b2229`; copies in `019f1b5b-bf8e-7da2-ae80-75365c9b8351`, `019f1b5c-6307-7a50-8bbb-f34e3040a8fb`, `019f1b65-1bcf-70b0-8dd0-d3fb4bfa4dc6`, `019f1b67-ea10-7cd2-9f60-e105e71f8b0c`, `019f1b69-43fe-7cb0-b4d4-9cba642f6701`, `019f1b93-38dd-75d0-b64b-45e10b46fd4e`, `019f1b96-ecb9-7311-adc7-b36454ad8880` | The same coordination message appears as separate hits across a recursively branching swarm. V2 clusters occurrences without erasing provenance, reports representative plus hidden count, and distinguishes copy/forward evidence from independent decisions. |
| `TD-SR-004 duplicate-profile-copies` | Hermes sessions `20260601_233104_694851`, `20260601_234852_8d0c9d`, `20260602_023303_b56f1a`, `20260604_042350_3e5712`, `20260604_040853_0641d6ff` | Identical session/summary rows appear twice in federated results because logical records are present in more than one project/store route. V2 owner routing and occurrence identity prevent duplicate representatives while preserving all store observations for migration diagnosis. |
| `TD-SR-005 lcm-no-match-fallback` | Codex `019ef820-ded7-7d80-bb63-2daf326d4b73`, `019f106a-90f6-7a20-aa66-9be43423d8c7`, `019f2a8c-80a7-7552-a173-2509a33839b0`, `019ef341-5285-77f2-90e4-84da95f73e89`; Claude `agent-af2333cfd574347cd` | Agents repeatedly report no matching LCM summary/current-session context, then read files, query raw hits, or abandon TraceDecay. V2 distinguishes missing coverage from true no-answer and can assemble from federated raw/summary/source evidence. |
| `TD-SR-006 expand-query-provider-failure` | Hermes `20260616_202442_13091e` | `lcm_expand_query` failed in the Hermes adapter because an `agent` argument was forwarded twice. Provider conformance replay must prove the same typed request/result/fallback contract across hosts. |
| `TD-SR-007 cross-project-intent-noise` | Parent plan session `019f4906-a411-7a11-ad3f-0d58deb0e847`; scope failures `019f42c9-623a-7cc0-95c1-f073eaa05a4d`, `019f4323-f569-74c0-9988-ea3851d14fd7`, `019f4325-57ef-7a53-b6a0-5c583c759301` | Query `rspack rsbuild react router` surfaces the current query/request, store inventories, project-registry dumps, and plan narration ahead of many substantive cross-repository sessions. V2 resolves project sets first, penalizes query echoes/inventories, and diversifies by repository, Turn, and evidence kind. |
| `TD-SR-008 stale-versus-current` | Codex `019ee19a-7bad-7a72-b49e-2cd57839f708` | A later assistant result says PR #50 is superseded by split PR stacks. Current search has no typed link from the obsolete plan/PR claim to the replacement. Current mode must rank the replacement and warn/link history; historical mode must still recover the older state. |
| `TD-SR-009 memory-analogue` | `eval/scenarios/memory-ranking-supersession.json` | The retained old npm fact explicitly still outranks the newer pnpm replacement for an exact stale query. Session-temporal evaluation must include the analogous old-chat/new-correction failure and share supersession vocabulary with knowledge retrieval. |
| `TD-SR-010 local-lcm-sort` | Codex LCM session `019f4379-0ad3-7cc1-958b-16e610e7ec3a` | For query `rspack`, relevance/hybrid return the same early matches while recency returns later conclusions; all hits come from one session and the per-session cap drops two. V2 evaluation grades which Turn is useful for each intent instead of declaring one universal ordering correct. |
| `TD-SR-011 active-store-identity-conflict` | Selected shard `proj_ceaa713e40fef2b2`; legacy shard `proj_b4a8bbe4953823c4` | MCP and project-local CLI calls refuse to choose between two preserved identities with different session/message/LCM counts. Retrieval must return typed partial/unavailable shard dispositions and never silently select or merge conflicting stores. |
| `TD-SR-012 search-to-load-routing-gap` | Plan 13 cross-project anchors, including `019f2538-0fd9-7362-a50b-96e36130643b` | Federated `message_search` can discover a remote project session while `lcm_load_session` remains active-project scoped. Every V2 result anchor must hydrate/replay without CWD or manual store switching. |

These are seed cases, not a claim that the corpus is complete. Phase PR 13D must resolve them into durable private `RetrievalAnchorId`s at a frozen watermark and add more cases before any ranking work is promoted.

## 3. Canonical occurrence, logical-message, Turn, and thread model

### 3.1 Never discard source occurrences

```rust
pub struct MessageOccurrenceV1 {
    pub occurrence_id: MessageOccurrenceId,
    pub message_id: MessageId,
    pub provider_native_id: Option<NativeMessageId>,
    pub source_observation_id: ObservationId,
    pub source_instance_id: SourceInstanceId,
    pub owner_shard_id: ShardId,
    pub provider: ProviderId,
    pub session_id: SessionId,
    pub thread_id: Option<ThreadId>,
    pub turn_id: Option<TurnId>,
    pub agent_instance_id: Option<AgentInstanceId>,
    pub role: MessageRole,
    pub origin: MessageOrigin,
    pub audience: MessageAudience,
    pub occurred_at: Option<UtcMicros>,
    pub ingested_at: UtcMicros,
    pub source_order: SourceOrder,
    pub location_assertions: Vec<LocationAssertionId>,
    pub sanitization_receipt_id: SanitizationReceiptId,
    pub content_ref: PayloadRef,
}
```

- `message_id` is canonical only when deterministic provider/native evidence supports it; otherwise store supplies a durable allocation.
- Multiple source observations can attest to one provider event. They remain separate occurrences linked to one logical message.
- One occurrence can be copied/forwarded into another session. The receiving occurrence remains addressable and carries its own session/Turn/audience context.
- A provider-native ID collision across source instances is conflict evidence, not an `INSERT OR REPLACE` instruction.
- Occurrence storage — uniqueness over `(source_instance_id, source_observation_id, source_order)`, keys, and indexes — is defined by plan [`02-store-crate.md`](02-store-crate.md)'s occurrence table family; this plan owns only the semantics above.

### 3.2 Logical clusters are versioned assertions

```rust
pub struct LogicalMessageClusterV1 {
    pub cluster_id: LogicalMessageClusterId,
    pub revision: ClusterRevision,
    pub member_occurrences: NonEmpty<MessageOccurrenceId>,
    pub relations: Vec<MessageCopyAssertionV1>,
    pub representative_policy_version: RepresentativePolicyVersion,
    pub projection_watermark: VectorWatermark,
}

pub struct MessageCopyAssertionV1 {
    pub assertion_id: MessageCopyAssertionId,
    pub subject: MessageOccurrenceId,
    pub object: MessageOccurrenceId,
    pub relation: MessageCopyRelation,
    pub evidence: Vec<EvidenceRef>,
    pub confidence: Confidence,
    pub valid_time: TimeInterval,
    pub transaction_time: TimeInterval,
}
```

`MessageCopyRelation` includes `same_provider_event`, `forwarded_prompt`, `parent_child_copy`, `workflow_delegation_copy`, `compaction_replay_copy`, `store_replica_copy`, `summary_quotes_source`, and `possible_duplicate`. Only high-confidence identity relations share a logical representative. `possible_duplicate` is a ranking/diversity hint and never destroys independent evidence.

Cluster revisions are retained and queryable by transaction time: plan 02's cluster table family keeps prior revisions, so as-of replay of cluster-dependent features (copy-noise penalty, representative selection) reads the revision current at the knowledge-time cutoff rather than today's clustering.

Representative selection is query-dependent:

- direct human origin for user-intent recall;
- source occurrence for forensic replay;
- current parent Turn for active-thread navigation;
- tool event for “what command changed this?”;
- summary node for an explicit overview request;
- highest-authority current assertion for current-state recall.

The result returns the chosen representative, every hidden-member count by provider/session/project, and a stable expansion anchor.

### 3.3 Turns and threads are first-class retrieval grains

A `Turn` groups one initiating event, assistant/reasoning parts, tool invocations/results, edits, goals, spawned agents, handoffs, and terminal outcome under provider-declared or evidence-backed boundaries. It is not inferred only from alternating roles.

A `Thread` can span provider sessions, compaction continuations, resumed hosts, and handoffs. Canonical `RelationAssertionV1` records thread/session relation, evidence, and time; rebuildable `thread_session_index` accelerates lookup without copying all messages into one session. Retrieval can return:

- exact message;
- smallest sufficient Turn;
- session episode;
- thread evolution;
- agent/workflow slice;
- evidence bundle crossing Git/code/PR relations.

The query result always states which grain was ranked and which source anchors will be hydrated.

## 4. Temporal truth, correction, and supersession

### 4.1 Assertions, not overwritten facts

Messages can contain claims, decisions, preferences, plans, status observations, commands, or hypotheses. Projectors may extract **assertion candidates** but cannot silently promote model inference to truth.

```rust
pub struct TemporalAssertionV1 {
    pub assertion_id: TemporalAssertionId,
    pub subject: EntityRef,
    pub predicate: PredicateId,
    pub object: AssertionValue,
    pub scope: DeclaredScope,
    pub valid_time: TimeInterval,
    pub transaction_time: TimeInterval,
    pub status: AssertionStatus,
    pub authority: AuthorityClass,
    pub evidence: NonEmpty<EvidenceRef>,
    pub confidence: Confidence,
}

pub struct AssertionRelationV1 {
    pub relation_id: AssertionRelationId,
    pub predecessor: TemporalAssertionId,
    pub successor: TemporalAssertionId,
    pub kind: AssertionRelationKind,
    pub evidence: NonEmpty<EvidenceRef>,
    pub confidence: Confidence,
    pub decided_by: DecisionProvenance,
}
```

`AssertionRelationKind` includes `replaces`, `corrects`, `contradicts`, `refines`, `narrows_scope`, `extends_validity`, `revokes`, `reaffirms`, and `independent`. `AssertionStatus` includes `candidate`, `supported`, `current`, `superseded`, `revoked`, `conflicted`, and `unknown`.

### 4.2 Authority and evidence are typed

Authority is contextual and predicate-specific:

- a direct later human correction is stronger for user intent than an older assistant summary;
- an actual Git ref/check/merge observation is stronger for current repository state than chat narration;
- a committed command receipt is stronger for “what changed?” than a proposed plan;
- provider-native parent/child metadata is stronger for agent lineage than copied prompt text;
- a summary is useful navigation but not stronger source evidence than the messages it summarizes;
- an explicit “hypothesis” does not become a decision because it is recent;
- a stale high-authority decision can remain historically decisive while no longer current.

Authority never crosses scope automatically. A decision for one repository/worktree/branch cannot supersede a similarly worded decision for another.

### 4.3 Answer modes

Answer modes ride the optional `temporal` clause of `TraceQueryV1`. Plan [`01-domain-crate.md`](01-domain-crate.md) owns the exact clause type (`TemporalClauseV1::Current | AsOf{valid_time, knowledge_time} | Evolution | Forensic`); plan [`05-query-crate.md`](05-query-crate.md) §6/§11.4 plans and executes it. Evolution bounds, when requested, are expressed only through `TraceQueryV1.time`; the mode never gains a second `from`/`to` field. This plan defines no parallel `TemporalAnswerMode` AST — it supplies the mode semantics:

- `Current`: prefer assertions valid now at the frozen snapshot; collapse confident supersession chains to the current representative, but return a history/conflict warning and lineage anchors.
- `AsOf { valid_time, knowledge_time }`: evaluate only evidence valid at `valid_time` and known by `knowledge_time`; both timestamps are required per 05 §11.4 — a single-timestamp as-of conflates validity with knowledge and is rejected at validation. This mode supersedes this plan's earlier single-timestamp `Historical` mode. Never leak later corrections into ranking features or summaries.
- `Evolution`: rank change points and relation chains, not one winning document.
- `Forensic`: preserve occurrences and weak/contradictory evidence with minimal dedup; useful for audits and implementation archaeology.

Query intent can choose a mode only when confidence is high. Ambiguous queries return the current answer plus a compact “historical/conflicting evidence exists” section, or expose an explicit mode selector. They do not silently hide either side.

### 4.4 Recency is bounded and explained

Recency may affect:

- current-state queries after validity/authority constraints;
- active-agent/worktree proximity;
- recent session listings;
- ties among equivalent evidence;
- stale-summary risk;
- context budget selection for an active Turn.

Recency must not:

- override an explicit correction graph in the wrong direction;
- make a newer tool/schema echo outrank the original user request;
- turn ingestion delay into event recency;
- compare provider timestamps from incompatible clock domains without normalization/uncertainty;
- use store row IDs as cross-shard time;
- boost repeatedly retrieved stale evidence until it becomes self-reinforcing truth.

Every time feature states its source (`occurred`, `observed`, `ingested`, `valid_from`, `valid_to`, `source_order`) and uncertainty.

## 5. Query intent and retrieval plan

### 5.1 Intent classes

The session-retrieval intent profile rides `TraceQueryV1` unchanged: temporal mode and cutoff use the first-class optional `temporal` clause (§4.3; plan 01 owns `TemporalClauseV1`), and intent profile, grain, origin/audience/kind, provider, evidence-relation, assertion-status, and summary-freshness filters use the registered attribute keys specified in plan 05 §6.1. No parallel AST, fork, or session-only query type exists. The versioned, inspectable intent classes are:

- `exact_literal_or_identifier` — errors, paths, API/tool/config names, session/message/Turn IDs, branch/commit/PR identifiers;
- `original_user_request` — direct human prompt or correction, not copied child prompts/tool results;
- `decision_or_preference_current` — current authoritative assertion and its lineage;
- `decision_or_preference_as_of` — state at a declared historical cutoff;
- `evolution_or_why_changed` — correction/supersession/decision chain;
- `session_or_thread_recovery` — smallest sufficient Turn/session plus adjacent context;
- `agent_or_workflow_activity` — goal, work claim, spawned agents, tool/edit effects, handoff, outcome;
- `git_code_delivery_correlation` — sessions/Turns that proposed, produced, observed, reviewed, or were affected by a Git/code/PR artifact;
- `cross_project_causal_context` — federated evidence across resolved repositories/worktrees;
- `tool_history_or_debugging` — tool definitions/calls/results are intentional rather than noise;
- `thematic_exploration` — diverse broad results with lower current-state assumptions;
- `no_answer_validation` — caller wants proof of absence/coverage rather than nearest topical text.

The plan records intent probabilities, selected profile, original query, exact protected literals, aliases/entities, scope, temporal mode, budgets, and fallback policy. A caller can override mode/scope/grain without hand-authoring rank weights.

### 5.2 Candidate channels

Candidate generation is independently bounded and ablatable:

1. exact canonical/native/retrieval ID and exact phrase;
2. fielded lexical BM25 over typed origin, role, kind, entity, tool, Git, code, goal, and content fields;
3. character n-gram/edit-distance fuzzy channel for misspellings and partial identifiers;
4. entity/alias channel for projects, repositories, worktrees, refs, PRs, sessions, agents, tools, symbols, and facts;
5. temporal assertion/current-state index;
6. relation graph seeds and bounded typed expansion;
7. summary-DAG documents with source horizon/coverage;
8. optional privacy-domain local dense retrieval;
9. optional learned-sparse retrieval;
10. optional bounded local reranker — a post-fusion stage executed by plan 05's `rank/rerank.rs` (15 §4.4), listed here for ablation completeness rather than as a candidate channel;
11. explicit recent-activity listing channel for listing intent only — the `list_sessions`/`list_messages` list intents defined in plan 05 §6.2.

Each candidate carries channel rank, native score, normalized/calibrated score if available, matched fields/terms/entities, owner shard, index/profile/model versions, source/summary horizon, privacy eligibility, watermark, cap/truncation, and latency.

### 5.3 Federated scoring

Never compare raw shard BM25 scores as if corpus statistics were shared. Evaluate these baselines:

- rank-based reciprocal-rank fusion across owner shards;
- globally versioned field/document-frequency statistics with bounded staleness disclosure;
- per-shard score calibration trained only on frozen development judgments;
- exact-match tiers before any score normalization;
- two-stage global candidate fusion and local hydration.

The selected method must prove byte-stable results for a fixed shard layout and bounded, explained drift when the same logical corpus is repartitioned. Exact-match tiers remain invariant; other top-k changes must satisfy the locked overlap/nDCG and worst-stratum floors. Adding an unrelated large project cannot arbitrarily change top results for an exact scoped query. The production fusion order is defined once in plan 05 §11.3 — RRF over channels within each shard, then a calibrated cross-shard merge with exact-match tiers first — and plan 05 owns the fixed-layout determinism plus repartition-drift property tests (05 §17); this plan contributes session fixtures and ablation baselines.

### 5.4 Temporal resolution and ranking

Hard eligibility precedes ranking:

1. authorization/privacy and sanitized-output eligibility;
2. immutable resolved project/repository/worktree/ref set;
3. temporal cutoff and retention horizon;
4. provider/origin/kind/grain requirements;
5. required evidence relation (`produced`, `observed`, `proposed`, `copied`, and so on).

Then compute explained features:

- exact phrase/identifier and fielded lexical score;
- semantic/fuzzy/entity match;
- assertion validity at cutoff;
- supersession/current-state relation;
- authority/evidence/directness;
- query-to-project/ref/worktree/snapshot relevance;
- Turn/session/thread/agent proximity;
- source-versus-summary and summary freshness;
- copy/query/tool/protocol/inventory noise;
- diversity/novel evidence contribution;
- bounded recency appropriate to the chosen intent;
- confidence, contradiction, and coverage risk.

Rank explanations name feature contributions and hard exclusions. They never expose secret text, model hidden reasoning, or an unverifiable “AI relevance” number.

### 5.5 Diversity and representative policy

After exact-hit preservation:

- one logical cluster cannot fill the page through copied occurrences;
- cap per session/thread/agent/project/provider only when the selected intent benefits, and disclose every cap;
- preserve distinct correction-chain nodes for `Evolution` mode;
- preserve both sides of unresolved conflicts;
- prefer source messages over summaries for exact literals;
- prefer Turns over isolated messages when adjacent tool/code evidence is necessary;
- allow explicit `Forensic` mode to disable representative hiding.

### 5.6 Task, ticket, initiative, dependency, and work-claim context

Task/ticket context packets use the same temporal retrieval engine and the canonical graph from [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md). They are not a global-board text dump and do not invent a second task-specific search path.

There is no `TaskContextSelectorV1`, task query struct, or task-local scope/temporal/page/sort vocabulary. The plan 09 task-context use case accepts canonical `WorkItemVersionRefV1`, `DependencyVersionRefV1`, and `WorkClaimRefV1` values owned by plan 01 and used by plan 24, plus canonical `InitiativeId`, `ThreadId`, and `TurnId` values where needed, and losslessly lowers them into one `TraceQueryV1`: plan 16 produces `scope`, plan 01's `temporal` and `time` fields carry temporal semantics, and plan 05's registered task/relation attributes carry the canonical references. Saved task views persist that one canonical AST and its registry digest, never an application-only selector. Unknown or version-mismatched refs fail validation before candidate generation; they are not weakened to bare text filters.

The context packet contains only evidence relevant through an explained typed relation:

- current task/ticket statement and the supersession lineage of earlier statements;
- initiative membership and dependency edges valid at the cutoff;
- current/expired/conflicting work claims and agent owners;
- directly linked thread/Turn/session/workflow evidence;
- produced/observed Git/code/PR/check artifacts with relation class;
- blockers, decisions, outcomes, and handoffs backed by anchors;
- sibling-task summaries only when they share an initiative dependency, explicit handoff, overlapping work claim, affected artifact, or requested scope.

An unrelated global board row, similarly worded ticket, stale sibling plan, or other project initiative is a hard negative. Project/repository/worktree/ref scope and task/claim/dependency filters apply before ranking. A claimed task defaults to its own current context; broader initiative/global context is opt-in and visibly separated. If two agents claim overlapping artifacts, coordination evidence can surface compactly, but unrelated sibling work cannot consume the task context budget.

Current mode returns the active task decision/claim and links superseded task instructions. As-of (`AsOf`) mode reconstructs what the task packet contained at the cutoff without later status, dependency, branch, or sibling-summary leakage. Evaluation must include Rspack/Rsbuild/React Router initiatives whose tasks cross repository boundaries, same-title tickets in unrelated projects, stale board decisions, overlapping agents, and sibling summaries that are relevant by vocabulary but unrelated by typed dependency/scope.

## 6. LCM summary DAG and context assembly redesign

### 6.1 One raw source, one derived hierarchy

LCM is a projection/use case over canonical activity, not a second message store/search product. A summary node records:

```rust
pub enum SourceRangeRef {
    OccurrenceRange {
        thread_id: ThreadId,
        source_instance_id: SourceInstanceId,
        start: SourceOrder,
        end_exclusive: SourceOrder,
        source_watermark: VectorWatermark,
    },
    SummaryNode {
        node_id: SummaryNodeId,
        node_digest: ManifestDigest,
    },
    WholeThreadUnverified {
        thread_id: ThreadId,
        observed_watermark: VectorWatermark,
    },
}

pub struct SummaryNodeV2 {
    pub node_id: SummaryNodeId,
    pub thread_id: ThreadId,
    pub source_ranges: NonEmpty<SourceRangeRef>,
    pub source_watermark: VectorWatermark,
    pub temporal_horizon: TimeInterval,
    pub created_at: UtcMicros,
    pub provenance: SummaryProvenanceV1,
    pub sanitization_receipt_id: SanitizationReceiptId,
    pub content_ref: PayloadRef,
    pub claim_refs: Vec<TemporalAssertionId>,
    pub lossiness: SummaryLossiness,
    pub status: SummaryStatus,
}

pub enum SummaryProvenanceV1 {
    Generated {
        summarizer: SummarizerDescriptor,
        summarizer_policy: SummarizerPolicyRefV1,
        prompt_version: PromptVersion,
        model_run_receipt: PayloadRef,
        anchor_manifest: SummaryAnchorManifestV1,
    },
    ImportedUnverified {
        source_manifest: PayloadRef,
        source_node_key_digest: PrivacyDomainBoundLocatorDigest,
        missing_provenance_reason: NativeKindCode,
    },
}

pub struct SummaryAnchorManifestV1 {
    pub summary_node_id: SummaryNodeId,
    pub summary_content_digest: SanitizedOutputDigest,
    pub marker_map_digest: ManifestDigest,
    pub entries: BoundedVec<SummaryAnchorEntryV1, 256>,
    pub source_watermark: VectorWatermark,
    pub authorization_digest: AccessPolicyDigest,
    pub sanitization_receipt_id: SanitizationReceiptId,
    pub manifest_digest: ManifestDigest,
}

pub struct SummaryAnchorEntryV1 {
    pub marker: SummaryAnchorMarkerV1,
    pub anchor: RetrievalAnchorId,
    pub relation: SummaryAnchorRelationV1,
    pub source_range: SourceRangeRef,
    pub claim_refs: BoundedVec<TemporalAssertionId, 16>,
}
```

- DAG sources are exact raw ranges or prior summary nodes; cycles and missing source coverage fail validation.
- `SummaryStatus` includes `imported_unverified` for §12.1 V1 imports whose source coverage cannot be proven: such a node carries `SummaryProvenanceV1::ImportedUnverified`, a single whole-thread `SourceRangeRef` marked unverified, and no provenance-marker chips. It is stale for current mode by default, may navigate/expand, and never satisfies a source-coverage proof; DAG validation accepts missing provable coverage only in this status. A verified successor is generated from authorized sources and does not retrofit model/anchor provenance onto the imported row.
- New raw/correction evidence after the node horizon does not mutate the node. It marks the node stale for current mode and creates a successor summary if policy allows.
- Deleted/redacted/locked sources update eligibility and coverage; a non-content tombstone remains.
- Summary embeddings/indexes use the same privacy domain and versioned horizon.
- Summary text can retrieve sources but cannot independently prove a claim.
- Every generated summary has a validated anchor manifest. Consequential claims, decisions, corrections, blockers, code/Git effects, task state, and unresolved questions carry compact stable markers that resolve through the manifest to exact authorized sources. Narrative glue may cite a range-level marker; it does not need an opaque ID in every sentence.
- An anchor marker is navigation and provenance, not authority. Resolution rechecks authorization, retention, redaction, deletion, current owner routing, temporal cutoff, and manifest digest. Missing, stale, locked, or revoked anchors remain explicit omissions; the summarizer may not fabricate, silently drop, or replace them with uncited prose.
- Source coverage and claim support are separately measured. A summary can be fluent yet fail publication when required source ranges or consequential claims lack eligible anchors. Re-summarization publishes a successor node and manifest; it never rewrites prior anchor bindings.

### 6.1.1 Default compaction and LCM summarizer policy

The product default for TraceDecay-owned compaction, LCM summarization, and equivalent transcript-summary jobs is the cataloged `gpt-5.6-terra` model at `extra_high` reasoning effort. This is a versioned Plan 20 policy default, not a hard-coded provider call: every run pins requested and actual model identity/revision, reasoning effort, endpoint/runtime, prompt/schema versions, privacy eligibility, budget, source watermark, anchor-manifest digest, and terminal receipt. All Hermes profiles, Codex, Claude, and Cursor share the same TraceDecay user-profile policy; a host profile does not create a separate summary database or default.

An authorized higher-precedence policy may select another capable summarizer for offline, privacy, availability, cost, or compatibility reasons, but the UI/CLI/API must show the override and resulting capability gap. If Terra is unavailable or the content cannot legally reach its runtime, the system does not silently downgrade: policy chooses an explicitly cataloged fallback with a recorded reason, or produces deterministic evidence-only compaction with source anchors and `synthesis_unavailable`. Host-native compaction remains captured as provider evidence and is never mislabeled as a TraceDecay Terra run.

Publication requires structured-output validation for summary text, omissions, lossiness, claim refs, and `SummaryAnchorManifestV1`. Application canonicalizes the rendered marker occurrences and rederives `summary_content_digest` plus `marker_map_digest`; every marker in text must occur exactly in the manifest and every non-range manifest marker must occur in text. Open/replay rechecks both digests before rendering a chip. The model may propose marker-to-source relations only from the bounded source inventory supplied by the assembler; application code resolves and validates the canonical `RetrievalAnchorId`s. Model output cannot mint anchors, widen scope, read the database, or declare a source current.

### 6.2 Context assembly profile

`ContextAssemblyRequestV1` starts from a `TraceQueryV1`, stable anchor, current Turn, or explicit result set and declares:

- temporal answer mode/cutoff;
- requested grain and purpose (`resume`, `answer`, `hint`, `debug`, `compare`, `forensic`);
- project/provider/thread/agent scope;
- token, byte, result, latency, model, and graph budgets;
- source-versus-summary preference;
- required adjacent evidence kinds;
- diversity/copy policy;
- privacy/output capability;
- deterministic, recorded-result, or current-best-effort replay mode.

The assembler executes in plan 05's `context/assembler.rs` (05 §5); plan 09 composes and authorizes the assembly use case and owns packet publication, and plan 24 supplies task-graph selectors only. The assembler selects, in order:

1. decisive source anchor(s) and current/conflict lineage;
2. containing Turn and minimal adjacent messages;
3. necessary tool/edit/code/Git/PR/agent relations;
4. fresh summary coverage for omitted ranges;
5. bounded head/tail only when conversation framing requires it;
6. explicit omission/partial/locked/truncated ledger.

Token accounting uses one tokenizer descriptor and reports estimated/actual tokens separately from characters and bytes. `context_max_tokens` can never secretly mean characters.

### 6.3 Answer and synthesis contract

Retrieval and context assembly always return the same typed evidence envelope. Optional synthesis is a separate policy-selected stage:

- `evidence_only`: host receives context and synthesizes;
- `local_synthesis`: configured same-privacy-domain model returns a cited answer;
- `recorded_synthesis`: replay the historical answer/model result;
- `no_synthesis`: return result/context view only.

A synthesis failure never discards retrieved evidence. `NoAnswerReason` is a closed, versioned enum carried in the §7.2 page envelope's `no_answer_reason` field with exactly these variants:

- `no_relevant_evidence`;
- `scope_resolved_empty`;
- `shard_unavailable_or_conflicted`;
- `not_ingested_or_index_stale`;
- `retained_redacted_or_locked`;
- `historical_cutoff_excludes_evidence`;
- `all_candidates_below_threshold`;
- `budget_exhausted`;
- `synthesis_unavailable`.

## 7. Result, anchor, cursor, and output contract

### 7.1 Typed result view

```rust
pub struct TemporalSearchResultViewV1 {
    pub anchor: RetrievalAnchorId,
    pub grain: RetrievalGrain,
    pub representative: SafeResultSummary,
    pub logical_cluster_id: Option<LogicalMessageClusterId>,
    pub hidden_occurrence_counts: OccurrenceCounts,
    pub temporal_state: TemporalResultState,
    pub supersession_lineage: Vec<RetrievalAnchorId>,
    pub conflict_anchors: Vec<RetrievalAnchorId>,
    pub evidence_relation: EvidenceRelation,
    pub scope: ResolvedScopeSummary,
    pub occurred_at: Option<UtcMicros>,
    pub ingested_at: UtcMicros,
    pub explanation: RankExplanationViewV1,
    pub hydration: HydrationCapability,
}
```

Per plan 01's exposure rule, results carry only the `RetrievalAnchorId`. `retrieval_anchors.metadata_batch_get` at `POST /api/v2/retrieval-anchors:metadata-batch` loads bounded safe identity/state metadata without content; `retrieval_anchors.resolve` at `POST /api/v2/retrieval-anchors:resolve` performs separately authorized record/payload resolution at a frozen watermark. No result row, deep link, or export embeds the anchor record.

The referenced view types are concrete:

```rust
pub enum RetrievalGrain { Message, Turn, Session, Thread, AgentSlice, WorkflowSlice, EvidenceBundle }

pub struct SafeResultSummary {
    pub title: SafeLabel,
    pub snippet: Option<EligibleSnippet>,
    pub matched_fields: Vec<FieldId>,
    pub provider: ProviderId,
    pub origin: MessageOrigin,
}

pub struct OccurrenceCounts {
    pub total_hidden: u32,
    pub by_provider: BTreeMap<ProviderId, u32>,
    pub by_session: u32,
    pub by_project: BTreeMap<SafeProjectLabel, u32>,
}

pub enum TemporalResultState { Current, HistoricalValid, Superseded, Revoked, Conflicted, Unknown }

pub struct EvidenceRelation {
    pub kind: EvidenceRelationKind, // produced | observed | proposed | copied | ...
    pub evidence: Vec<EvidenceRef>,
    pub confidence: Confidence,
}

pub struct HydrationCapability {
    pub metadata_batch_get: bool,
    pub authorized_resolution: bool,
    pub cluster_expansion: bool,
    pub replay: bool,
    pub denied_reason: Option<HydrationDeniedReason>,
}

pub struct RankExplanationViewV1 {
    pub profile: RankingProfileRef,
    pub final_score: FiniteF64,
    pub components: Vec<ComponentScoreView>, // id, version, normalized, weight, contribution, state
    pub hard_exclusions: Vec<ExclusionReason>,
    pub temporal_features: TemporalFeatureSummary,
}
```

`RankExplanationViewV1` is the plan 09/21-rendered safe view of plan 05's `RankExplanation`/`ComponentScore` (05 §11.3); it adds no scores of its own and exposes no secret text, hidden reasoning, or raw hashes.

Every result can be hydrated, expanded to cluster members, opened in the Turn/session/thread/timeline, and used as a new query/context anchor without project/CWD switching.

### 7.2 Page envelope

Human-facing Markdown is the default. JSON/NDJSON is explicit. Plan 09 owns the one semantic typed view model; plan 21 owns its presentation/rendering and transport parity.

Stable fields include:

- `status`, `query`, `intent`, `temporal_mode`, `as_of`;
- `resolved_scope`, `profile`, `index_versions`, `watermark`;
- `results`, `summary`, `limit`, `returned`, `truncated`, `next_cursor`;
- `coverage`, `partial_shards`, `skipped_shards`, `unavailable_shards`;
- `caps`, `deduplicated_occurrences`, `warnings`, `no_answer_reason`;
- `latency`, `token_estimate`, and optional evaluation/profile identifiers.

No compact default includes raw metadata blobs, transcript paths, full message bodies, summary sources, embeddings, or every cluster member. Safe metadata uses canonical batch-get; content/record loading uses separately authorized canonical resolution by stable anchor.

### 7.3 Cursor binding

The signed cursor is plan 05 §9's encoding of the extended domain `CursorClaimsV1` (plan 01) — this plan defines no parallel cursor type. For session-temporal queries those claims bind:

- canonical query digest and intent/profile version;
- temporal mode/cutoff;
- immutable resolved scope-set ID/digest;
- authorization/privacy capability digest;
- catalog and owner-shard generations;
- index/model/ranker/cluster/summary versions;
- frozen vector watermark;
- last deterministic rank tuple;
- partial/unavailable shard dispositions.

A changed scope, authorization, index generation, or profile returns a typed stale-cursor problem. It never resumes against a subtly different corpus.

## 8. Real replay and evaluation program

### 8.1 Corpus families

Build a private, sanitized, versioned corpus from authorized local history:

1. explicit user recall requests and corrections;
2. later prompts whose answer depends on an earlier Turn/session;
3. reformulated/abandoned `message_search` and LCM queries;
4. searches followed by anchor load/use versus ignored/fallback behavior;
5. old/new decisions, facts, plans, branches, PRs, checks, and corrections;
6. direct user prompt versus copied subagent/delegation/tool/schema rows;
7. parent/child/workflow duplication and agent-overlap coordination;
8. raw versus summary-DAG versus compaction replay;
9. heterogeneous cross-project investigations, including one frozen Rspack/Rsbuild/React Router and sibling-plugin regression slice;
10. exact error/path/API/config/tool/session/commit/PR identifiers;
11. conceptual paraphrases, misspellings, aliases, and renamed projects;
12. expected no-answer, wrong-scope, partial-shard, locked/redacted, and stale-index cases;
13. provider-neutral conformance across at least four provider/source families, retaining Codex, Claude, Cursor, and Hermes fixtures where authorized data exists;
14. hint-engine retrieval envelopes and background-intelligence proposals from Plan 22;
15. task/ticket context packets, initiatives, dependencies, work claims, and relevant-versus-irrelevant sibling summaries across unrelated multi-repository systems, including the named frozen slice.

The corpus must be heterogeneous across repository ecosystems, project topology, provider/source family, activity volume, checkout availability, and failure state. It includes independent repositories, monorepo/package scopes, upstream/fork/downstream relations, missing live checkouts, provider-absent projects, partial/conflicted stores, and no-answer cases. Named live repositories and provider stores are optional evidence inputs and never a product-default gate; frozen redacted fixtures carry their regression coverage.

Queries are frozen at an `available_at` cutoff. Candidates, summaries, branches, facts, corrections, and labels created later cannot enter a historical replay. Replay also rebuilds index statistics (for example BM25 document frequencies) from the frozen `available_at < t` corpus, so ranking features cannot leak future corpus statistics.

The frozen research corpus this program draws on is pinned by its owner, plan [`13-research-provenance-and-context-anchors.md`](13-research-provenance-and-context-anchors.md): path set `/fast/tracedecay-redesign-research/*`, file mode `0600`, final user-message cutoff `2026-07-11T01:04:10.875Z`, integrity verified against plan 13's final manifest hashes. The manifest distinguishes the broad supported-surface capture from the 47-record active-session raw-rollout fallback: the original 28 prompts, 11 reconciliation prompts, and 8 final cross-check prompts. Session-temporal evaluation inputs derived from it cite that exact manifest version, and no private content from it enters the repository.

### 8.2 Minimum evidence gates

Before any V2 default ranking claim:

- at least 500 real query episodes;
- at least 12 authorized projects spanning at least four unrelated repository ecosystems and at least four topology/failure classes; no named repository or live checkout is required;
- at least 4 provider families and an explicit coverage report for every supported provider;
- at least 100 current-versus-historical/supersession cases;
- at least 100 cross-project/worktree/ref cases;
- at least 75 copied-message/subagent/workflow duplication cases;
- at least 75 raw-versus-summary/compaction/LCM assembly cases;
- at least 100 exact technical identifier/error cases;
- at least 100 no-answer/partial/locked/stale-index cases;
- at least 75 task/ticket/initiative/work-claim cases, including current-versus-superseded decisions and sibling/global-board pollution hard negatives;
- candidate pools and manual labels sufficient for at least 5,000 query-result judgments;
- independent double labels on at least 20% of judgments and every high-severity stale/current or privacy case.

Cases can overlap strata. A project/provider below minimum evidence remains `insufficient_coverage`; aggregate metrics cannot imply support.

Maintain:

- frozen regression set;
- chronological train/development/test split;
- held-out project/provider/time blocks;
- rolling recent set;
- adversarial exact/typo/paraphrase/no-result set;
- migration shadow corpus comparing V1 and V2;
- private full judgments plus redacted/synthetic committed fixtures.

### 8.3 Judgment schema

Human judgment is the source of truth. LLM judges may propose labels or disagreement summaries, but their model/prompt/version is recorded and their labels remain secondary until audited.

Grade relevance:

- `0 misleading_or_irrelevant`;
- `1 topical_but_not_actionable`;
- `2 useful_context`;
- `3 decisive_or_smallest_sufficient_anchor`.

Also label:

- `current`, `historical_valid`, `superseded`, `revoked`, `conflicted`, `unknown`;
- authority/evidence relation;
- correct project/repository/worktree/ref/provider/thread/agent;
- direct origin versus copy/echo/protocol/tool/schema/summary;
- duplicate cluster and preferred representative;
- smallest sufficient grain;
- stale summary or leaked future evidence;
- privacy eligibility;
- no-answer correctness;
- whether the result enabled the next action.

Each judgment is a plan 15 §5.4 `JudgmentRecordV1` row in the activity shard's protected profile-evaluation family. The labels above map to plan 15's typed `SecondaryLabelsV1` dimensions; this plan defines no second label vocabulary or judgment record.

Adjudication preserves both original labels/rationales. Do not rewrite old qrels silently; publish a superseding judgment version (`supersedes` on the new row).

### 8.4 Systems compared

Every report includes:

- current V1 `message_search`;
- current V1 `lcm_grep` relevance/hybrid/recency;
- exact/phrase/fielded BM25 baseline;
- current-state resolver off/on;
- copy clustering off/on;
- fuzzy channel off/on;
- entity/graph channel off/on;
- summary-DAG channel off/on;
- optional dense and learned-sparse channels independently;
- RRF/calibrated shard fusion alternatives;
- reranker off/on;
- context assembly strategies: current head/tail/summary versus minimal Turn/evidence bundle;
- every rank/temporal/model/config ablation.

### 8.5 Metrics

Retrieval:

- Precision@1/3/5;
- Recall@5/10/20;
- MRR and first-useful rank;
- nDCG@10 with 0–3 grades;
- exact-ID/phrase Recall@K;
- judged coverage and unjudged rate.

Temporal safety:

- temporal-current accuracy;
- historical-as-of accuracy and future-leak rate;
- supersession safety: stale/superseded top-1 and top-K rate;
- conflict detection recall/precision;
- authority/evidence relation accuracy;
- summary-horizon stale-hit rate;
- current-versus-historical mode calibration;
- current task decision/claim accuracy and superseded-task instruction rate;

Quality/coverage:

- duplicate occurrence and duplicate logical-result rate;
- direct-user/source versus query/tool/schema/protocol echo rate;
- wrong-project/worktree/ref/provider/session/agent rate;
- irrelevant global-board/sibling-task pollution rate and typed dependency/claim coverage;
- result/project/provider/session diversity after exact-hit preservation;
- correct abstention, false no-answer, and false confident-answer rate;
- Brier score/expected calibration error for relevance and no-answer;
- anchor resolution/hydration success;
- partial-shard disclosure accuracy;
- context sufficiency, source coverage, and citation resolution.

Operational:

- p50/p95/p99 candidate, fusion, temporal resolution, hydration, assembly, and end-to-end latency;
- tokens, bytes, CPU, peak RSS, model load/warmup, shard opens, cache hits;
- index size, update lag, rebuild time, and per-privacy-domain representation cost;
- optional model invocation count, latency, token/cost budget, timeout/fallback rate;
- cursor page stability and cancellation latency.

Report macro/micro and every primary stratum. Exact IDs, temporal safety, privacy, no-answer, and worst-project/provider regressions are hard gates that an aggregate gain cannot offset.

### 8.6 Initial promotion thresholds

Calibrate thresholds on frozen development data, then lock before test evaluation. Initial safety floors:

- exact canonical/native ID Recall@10: `1.00` on eligible frozen cases;
- high-confidence explicit supersession wrong-stale top-1: `0`;
- historical replay future leakage: `0`;
- privacy-ineligible result or explanation: `0`;
- stable-anchor hydration success: `1.00` for non-deleted eligible results;
- duplicate logical result rate in top 10: below `0.05` overall and no cluster may occupy more than one default slot;
- every partial/unavailable shard reflected in coverage: `1.00`;
- V2 must improve predeclared Precision@3/nDCG@10/first-useful rank over strong lexical and V1 baselines on untouched test without material worst-stratum regression, where “material” uses plan 15 §7.1's numeric definition: a worst-stratum nDCG@10 drop greater than `max(2 points absolute, 5% relative)` versus the locked baseline, or any no-answer-precision drop greater than 2 points.

Latency/token/model thresholds are hardware/profile-specific and live in Plan 20 configuration plus the frozen benchmark manifest, not hard-coded into ranking logic.

## 9. Search Quality Lab: session-temporal workspaces

Plan 11 exclusively owns routes, workspace composition, layout, panels, and interaction. This section specifies only temporal retrieval/evaluation read models, legal actions, explanations, and acceptance data consumed by that owner; its screen descriptions are non-normative inputs and cannot create a second frontend contract.

This plan's replay/lineage/copy/summary/evaluation surfaces are workspaces inside the one Search Quality Lab (plan 15 §8) — not a separate Search Lab and not separate message, LCM, memory, and hint debug pages.

### 9.1 Query workspace

- query editor with exact protected literals;
- intent, temporal mode, `as_of`, grain, scope, provider/origin/kind, and evidence-relation controls;
- frozen corpus/watermark/profile/config/model selectors;
- current engine, historical engine, recorded result, and compare profiles;
- equivalent generated CLI/MCP/API/SDK request.

### 9.2 Candidate waterfall

- one column/lane per lexical/fuzzy/entity/graph/summary/semantic/time channel;
- native and normalized score, match fields, shard, rank, latency, cap, and exclusion;
- fusion and representative selection transitions;
- raw shard score calibration/RRF explanation;
- query/tool/schema/inventory penalties and diversity decisions;
- click any row to open the stable anchor and authorized source.

### 9.3 Temporal lineage explorer

- assertion current/valid/conflicted state;
- correction/supersession/contradiction graph;
- valid-time and transaction-time lanes;
- “what the engine knew then” versus “what it knows now” split view;
- branch/worktree/commit/PR state overlays;
- authority/evidence/confidence inspector;
- stale-result warning and winning/current rationale.

### 9.4 Logical-copy and summary-DAG inspectors

- representative plus hidden occurrence tree by provider/session/project/store;
- provider-native/linkage/copy evidence and uncertainty;
- summary DAG source ranges, horizon, coverage, model/prompt version, lossiness, stale state;
- readable `[S1]` marker chips linked to the manifest entry, relation, exact source range, and current resolution state; source content resolves only on explicit authorized open;
- requested versus actual summarizer model/revision/effort, fallback reason, run receipt, anchor coverage, omissions, and stale/locked/redacted/revoked markers;
- raw-source expansion with exact omissions/truncation;
- context-assembly token/byte budget and selected/omitted blocks.

### 9.5 Evaluation workspace

- corpus/query/qrel/profile/config version browser;
- candidate-pool judgments, hard negatives, double-label disagreement, and adjudication;
- per-query and aggregate nDCG/MRR/Recall/precision/temporal/duplicate/calibration charts;
- project/provider/intent/time/scope/privacy strata heatmaps;
- regression list sorted by severity and largest rank/temporal change;
- then-versus-now replay with recorded source watermark and engine/config digests;
- redacted aggregate report and synthetic-fixture promotion.

Lab replay is read-only against frozen inputs. Judgment and corpus updates are explicit authorized commands with audit/version/secret scan; they do not mutate production messages, retrieval counters, or current search state.

## 10. Product surfaces and controls

Exact transport names derive from Plan 08/21's catalog. Anchor operations are exclusively `retrieval_anchors.metadata_batch_get`, `retrieval_anchors.resolve`, and `retrieval_recipes.execute`; evaluation operations are the complete plan 15 §0.1 `retrieval.*` family. Required surfaces:

Profile-activity search uses `ScopeRootV2::Profile` plus an optional canonical `DeclaredScope::Profile`/`DeclaredScope::ZeroProject` query predicate, not a storage flag or invented root. It routes directly to the placed profile-activity authority regardless of client CWD, current project placement, or host profile; a missing/unavailable profile session source returns explicit incomplete coverage and never an empty-success or project fallback. Project and profile roots may be queried together only through an explicit authorized multi-root selector, not by combining legacy user scope with a compatibility project field.

### CLI

- search messages/Turns/sessions/threads/agents/workflows with mode/scope/as-of/grain and keyset pagination;
- search/assemble a task packet by `WorkItemId`, initiative, dependency, work claim, thread, or Turn without broad-board leakage;
- batch-inspect safe anchor metadata, authorize exact anchor resolution, execute a retrieval recipe, or expand a logical cluster;
- assemble context from query/anchor/Turn with declared budgets;
- replay session/thread or one historical query at a frozen cutoff;
- inspect temporal lineage, rank explanation, coverage, and summary horizon;
- run/compare/evaluate frozen corpora and export safe aggregate reports;
- inspect/start/join/cancel a separately authorized provider freshness operation when required, with source frontier, target watermark, progress, records/bytes/source-open counts, leader/joiner state, partial failures, and terminal receipt; ordinary search never starts it;
- navigate Settings for retrieval profiles, models, budgets, temporal policy, and privacy.

### MCP

- one generated search family and one context-assembly family with compact Markdown default;
- stable anchors suitable for follow-up tool calls instead of huge inline records;
- explicit JSON for machine clients and NDJSON/page streaming where catalog capabilities allow;
- complete `limit`/`truncated`/`next_cursor`/coverage/no-answer shapes;
- no tool-local alternate LCM rank semantics.

### HTTP and SDK

- `POST /api/v2/search`;
- `POST /api/v2/retrieval-anchors:metadata-batch` for bounded safe metadata only;
- `POST /api/v2/retrieval-anchors:resolve` for authorized record/payload resolution;
- `POST /api/v2/retrieval-recipes:execute` for bounded versioned recipe execution;
- `POST /api/v2/context:assemble`;
- task/ticket context assembly through the same route with typed task/initiative/dependency/claim selectors;
- `POST /api/v2/sessions/{id}/replay` and `POST /api/v2/threads/{id}/replay`;
- `GET /api/v2/temporal-assertions/{id}/lineage`;
- plan 10 §8.5's generic experiment create/run/trace/comparison routes with `LabKindV1::SearchQuality` (plan 15 §9 evaluator; no separate session-lab endpoint or lifecycle);
- versioned reads for corpus versions, qrel versions, candidate pools, judgments, adjudications, evaluation reports, retrieval profiles, and generic Search Quality experiments/runs/stages/comparisons;
- direct commands for corpus/qrel create/freeze, pool create, judgment record/supersede, adjudication record, aggregate report publish, sanitized fixture promotion, and retrieval-profile publish/activate; Search Quality execution uses the generic experiment create/run/cancel/resume/retry/minimize family;
- generated TypeScript client and official SDK parity from Plan 17.

### Settings

Plan 20 exposes every configurable retrieval value in UI plus navigable CLI/MCP/API/SDK:

- default intent/answer-mode policies;
- lexical/fuzzy/semantic/entity/graph/summary channels;
- shard fusion/calibration profile;
- rank/diversity/copy/authority/recency features;
- per-intent time decay/thresholds;
- local model/tokenizer/index/reranker/synthesizer descriptors;
- privacy-domain eligibility and remote-model prohibition;
- candidate/result/context/latency/token/cost budgets;
- freshness requirements, automatic background-refresh policy, and resource budgets; these configure daemon operations and stale-result notices, never a per-query ingest side effect;
- provider/project/source inclusion;
- no-answer/conflict/stale-warning thresholds;
- evaluation corpus, rolling replay, and promotion gates.

Privacy floors and temporal safety invariants cannot be disabled by lower-precedence project/session overrides.

## 11. Privacy, retention, and security

- Only Plan 18 `Sanitized`/eligible content reaches lexical, summary, entity, vector, qrel, report, or model paths.
- Raw query literals can contain secrets. Treat query history as sensitive content; store protected refs or safe keyed features, never analytics plaintext by default.
- Logical-copy fingerprints are keyed inside one privacy domain. No global unkeyed content hash or cross-domain embedding similarity joins private messages.
- Dense/learned-sparse/rerank/synthesis models and indexes are local to the authorized privacy domain unless an explicit policy/config permits a remote processor and the content class is eligible.
- Catalog/federation metadata contains opaque locators, capability, counts, watermarks, and safe labels—not message/query/summary text.
- Qrels and rationales live in the activity shard's protected profile-evaluation family; this is not a separate physical shard. Committed fixtures are synthetic or minimally redacted and secret-scanned.
- Hydration rechecks current authorization, retention, redaction, and deletion. A stale cached result cannot reopen revoked content.
- Deletion/redaction propagates to lexical indexes, vectors, summaries, clusters, caches, context bundles, qrels, and reports; non-content provenance/tombstone remains where policy requires.
- Prompt injection in historical messages is content evidence, never active instruction. Context envelopes label source/origin and quote/sandbox retrieved material.
- Rank explanations reveal safe feature categories and matched safe fields, not hidden content, protected model prompts, raw hashes, or secrets.

## 12. Migration and cutover

### 12.1 Inventory/import

Import read-only, idempotently:

- V1 `sessions`, `session_messages`, FTS rows, provider metadata, parent/subagent/workflow links, Git spans, goals, tool events, and source offsets;
- LCM raw messages, external payload refs, summary nodes/sources, lifecycle/frontier/debt state, and redaction/lossiness markers;
- response-handle metadata as expiring operational evidence, never durable anchors;
- current eval fixture and live probe recipes;
- duplicate/identity-conflict stores as separate observed sources until explicit consolidation.
- shipped `user-sessions.db`/`user-memory.db` and legacy rows whose `project_key="user"` as compatibility sources; lower the sentinel only to typed Profile activity or ZeroProject when source evidence proves that ownership, preserve the original alias/provenance, and quarantine ambiguous/mixed/project evidence rather than retaining `user` as canonical identity.

External provider/host stores are read-only evidence inputs. A separately approved bounded import writes only sanitized observations into TraceDecay-owned storage with owner, reason, evidence, and rollback; it never mutates, relocates, or deletes the source. Hermes-owned transcripts, LCM data, board databases, caches, and backups remain Hermes-owned regardless of TraceDecay route or retention state.

Each imported row gets source/store identity, sanitization status, occurrence ID, owner route, temporal fields, and parity receipt. Missing provenance becomes `unknown`, not guessed. Two explicit escape hatches keep the import honest:

- V1 summary nodes whose source ranges cannot be proven import as `SummaryStatus::imported_unverified` (§6.1) rather than failing DAG validation or fabricating coverage; they stay stale for current mode until re-summarization over verified sources.
- Mandatory `sanitization_receipt_id`s for the ~388k imported raw rows (master-plan scale envelope) are minted by capture's bulk sanitizer path (plan [`03-capture-crate.md`](03-capture-crate.md)) as a costed, restartable backfill stage with its own throughput budget and progress watermark — never per interactive read.

### 12.2 Backfill

Rebuild in bounded phases:

1. occurrence/native identity and source coverage;
2. Turn/thread/agent/workflow/location relations;
3. logical copy clusters with evidence/confidence;
4. summary DAG source/horizon validation;
5. typed lexical/entity/time documents;
6. temporal assertion candidates and explicit relation imports;
7. optional semantic representations per privacy domain;
8. frozen replay corpus and V1/V2 shadow results.

All stages are restartable, versioned, and watermarked. Re-clustering or re-summarization publishes a new projection generation; it does not rewrite prior receipts.

Provider-history refresh/backfill is a daemon-owned plan-09 operation keyed by source frontier and target watermark. Capture scans each `SourceInstanceId` once, globally budgets records/bytes/wall time/RSS, commits sanitized observations plus source head atomically, and lets projectors materialize zero-to-many attributions. Equivalent concurrent callers join the same durable receipt. Cancellation returns the last committed cursor; malformed complete rows quarantine atomically so later valid rows remain ingestible; a query can continue against its prior watermark with explicit stale/partial coverage.

### 12.3 Shadow and cutover

- V1 remains authoritative while V2 executes read-only shadow queries at the same frozen eligible cutoff.
- Compare candidate/result anchors, current-state resolution, no-answer reason, coverage, output size, latency, and resource cost.
- Manually inspect every high-severity temporal/privacy/exact-ID regression.
- Cut over session/LCM reads only after PR 35I gates pass across CLI/MCP/API/dashboard/hooks and every supported provider host.
- There is no per-query fallback that silently changes semantics. During the bounded migration window, route generation selects one owner for a context and exposes the other as comparison evidence.
- After V2 default, archive V1 read-only for the plan-12 window; then PR 37I deletes live V1 message/LCM/search/ranking/render paths after restore/replay proof.

## 13. Implementation layout

Domain additions:

```text
crates/tracedecay-domain/src/
  activity/message_occurrence.rs
  activity/logical_message.rs
  activity/turn.rs
  activity/thread.rs
  temporal/assertion.rs
  temporal/relation.rs
  retrieval/grain.rs
  retrieval/result.rs
  retrieval/context_assembly.rs
```

The temporal answer-mode carrier is `TemporalClauseV1` in plan 01's query vocabulary (§4.3); this plan proposes no separate answer-mode domain file.

Projectors:

```text
crates/tracedecay-projectors/src/
  activity/turn_projector.rs
  activity/thread_projector.rs
  activity/copy_relation_projector.rs
  activity/representative_projector.rs
  temporal/assertion_projector.rs
  temporal/supersession_projector.rs
  lcm/summary_dag_projector.rs
  search/session_document_projector.rs
```

Query: this plan adds no query-crate modules of its own — plan 05 §5 is the single module authority for ranking, fusion, session/temporal, context-assembly, and evaluation-metrics code. Session/LCM requirements land in these 05-owned modules:

| Requirement (this plan) | Plan 05 module |
|---|---|
| Intent profiles (§5.1) | `session/intent.rs` |
| Candidate channels (§5.2) | `operators/{filter,fts,fuzzy,entity,vector,learned_sparse,graph,summary,time}.rs` |
| Federated fusion (§5.3) | `rank/rrf.rs` + `execute/merge.rs` (defined once in 05 §11.3) |
| Copy clustering and representative selection (§3.2, §5.5) | `rank/cluster.rs` |
| Temporal resolution (§4) | `session/temporal_resolver.rs` |
| Rank features and diversity (§5.4–§5.5) | `rank/{features,diversity}.rs` |
| Optional rerank stage (§5.2 item 10) | `rank/rerank.rs` |
| Explanations (§7.1) | `rank/explain.rs` + `explain.rs` |
| Hydration (§7.1) | `operators/hydrate.rs` |
| Context assembly (§6.2) | `context/{assembler,summary_horizon,token_budget}.rs` |
| Session corpus, temporal qrels, replay, metrics (§8) | `eval/{corpus,qrels,replay,metrics}.rs` — `eval/metrics.rs` is the single shared metrics implementation (plan 15 §9) |

Application/API/UI use the shared Plan 09/10/11/21 locations. Root V1 adapters live only under plan 12 compatibility paths and are deleted at retirement.

## 14. Test plan

### Domain/property tests

- occurrence/native-ID determinism and collision conflicts;
- copy relation evidence and uncertain cluster separation;
- representative stability under ingestion order and fixed shard layout, plus bounded/explained drift under shard repartition;
- valid/transaction-time interval laws;
- supersession/contradiction graph acyclicity where required and explicit conflict handling;
- current/as-of/evolution/forensic mode semantics;
- cursor/query/scope/watermark binding;
- taint/privacy wrappers cannot enter unsafe indexes/results.

### Store/projector tests

- duplicate source replay, rewrite, late event, missing parent link, and store replica import;
- copied parent prompts, compaction replays, workflow delegation, and independent same-text messages;
- summary DAG cycle/missing source/stale horizon/deletion/redaction;
- atomic node/content/source/claim/anchor-manifest/model-receipt publication and kill recovery; no partial summary becomes queryable;
- Terra `extra_high` requested/actual receipt, explicit cataloged fallback, privacy-ineligible route, model timeout, and anchored evidence-only output without silent downgrade;
- forged, duplicate, missing, out-of-range, unauthorized, stale, locked, redacted, deleted, and revoked summary markers; the model cannot mint or rewrite a `RetrievalAnchorId`;
- source correction/redaction/deletion transitively stales every descendant summary and produces immutable successor lineage rather than in-place edits;
- temporal assertion backfill, relation revision, conflicting authority, and scope isolation;
- crash/restart at every cluster/index/summary/eval publication boundary;
- concurrent capture/projector/query snapshots across many agents/shards.

### Retrieval tests

- exact identifiers/errors/paths/phrases remain first-class;
- OR-generic-term false positives and AND/phrase/intent alternatives;
- raw BM25 shard scores never compared without declared fusion;
- current query ranks explicit replacement above stale exact predecessor and links both;
- historical cutoff returns predecessor without future leakage;
- ambiguous conflict returns warning/both sides;
- copy clusters occupy one default result but forensic mode expands all;
- direct user request outranks copied subagent/tool/schema echoes;
- claimed-task packets exclude unrelated global-board/sibling work and preserve only typed initiative/dependency/claim/thread/Turn relations;
- current task decisions supersede stale instructions while historical packets remain cutoff-correct;
- summary never hides exact source; stale summary cannot answer current mode silently;
- project/worktree/ref/provider/agent/thread filters and relation evidence;
- all-registered search-to-hydrate/replay without CWD switching;
- partial/locked/conflicted shard and no-answer reasons;
- stable pagination under frozen watermark and stale-cursor failure;
- output views remain compact and transport-identical.
- search remains write-free under fresh, stale, missing, and partial provider coverage; requesting freshness yields a legal operation descriptor rather than hidden catch-up;
- 64 identical refresh requests join one operation and receive the same terminal coverage/error receipt; leader death/takeover, cancellation, skewed projection checkpoints, and malformed-middle-row recovery do not duplicate or skip committed observations.

### Evaluation/replay tests

- exact replay preserves the original summary text, anchor manifest, model/config/prompt/source watermark, and recorded fallback while enforcing current authorization on anchor open; current-best-effort lists every substituted model/source/config and publishes a successor rather than altering history;

- every qrel/corpus/replay artifact has a digest, cutoff, privacy receipt, anchor coverage, and engine/config versions;
- metrics recompute from live results rather than trusting fixture summaries;
- chronological/project/provider holdouts prevent leakage;
- LLM judge labels are distinguishable from human labels;
- TD-SR-001 through TD-SR-012 are replayable or explicitly blocked with coverage reason;
- V1/V2 shadow comparison resolves stable anchors, not snippets/ranks;
- hint/background envelopes consume the same rank/temporal/context contracts.

### Provider/transport/UI tests

- at least four heterogeneous provider/source-family fixtures normalize origin, role, Turn, parent/child, and tool results consistently; retained Codex, Claude, Cursor, and Hermes fixtures are named coverage, not required live providers;
- CLI/MCP/API/SDK produce equivalent typed JSON and Markdown default behavior;
- merged #445/#448 fixtures run user-scoped message search and LCM from `/`, Hermes home, unrelated CWD, and project CWD with no project route/handshake/init; missing registry/source reports typed unavailable coverage; every legacy scalar-user-plus-compatibility-project spelling, including `project_key`, fails identically; appended zero-project and registered-project source rows prove routing; 64 joiners receive the leader's exact terminal coverage/error; no-op/failure never reports refresh performed; and canonical Profile+Project reads remain valid;
- Markdown and JSON disclose limit/truncation/cursor/coverage/conflict/stale state;
- Search Quality Lab session-temporal then-versus-now replay is read-only and visually/accessibly testable;
- Settings changes produce versioned effective-config provenance and deterministic replay.

### Performance/security tests

- lexical-only and optional-model cold/warm benchmarks at current and 10x manifest scale;
- fan-out, cancellation, slow/missing/conflicted shard, and loaded-daemon concurrency;
- a frozen 30-project cold provider-history workload completes in ≤60 seconds with one source sweep, bounded source-open/read-byte/RSS counts, explicit progress, and second-refresh zero additions; the captured Hermes workload remains one fixture, while live Hermes availability is optional and non-gating; query latency is measured separately and includes no ingestion work;
- token/byte/result/context ceilings, durable retrieval-anchor recovery, and legacy response-handle regression behavior;
- secret/query-log leakage, keyed fingerprint isolation, prompt-injection labeling, deletion/cache invalidation, and unauthorized hydration.

## 15. PR sequence

These suffixes were unused in the plan set when authored. Recheck the master plan immediately before implementation.

### PR 13D — Frozen session-temporal corpus and current baselines

- Resolve TD-SR seed cases to private durable anchors and frozen cutoffs.
- Build session/LCM qrel schemas, temporal/copy/no-answer labels, metrics, V1 message/LCM baselines, and aggregate redacted reports.
- Do not change production ranking.

### PR 13E — Occurrence, logical-copy, Turn/thread, and summary-horizon projections

- Add typed occurrence/copy/Turn/thread documents and representative clusters.
- Validate/backfill summary DAG source ranges/horizons and duplicate-store observations.
- Shadow only; preserve raw occurrences and V1 routes.

### PR 14D — Temporal assertion resolver and intent-aware hybrid ranking

- Add answer modes, validity/supersession/conflict resolution, authority/evidence features, federated fusion, explanations, and diversity.
- Compare every channel/feature ablation on frozen development/test sets.

### PR 15C — Unified session retrieval and context assembler

- Replace separate message/LCM candidate semantics inside V2 query with one typed pipeline.
- Add minimal Turn/evidence bundles, exact token accounting, summary-source fallback, stable hydration anchors, and no-answer reasons.

### PR 24L — Application, API, CLI, MCP, and SDK bindings

- Ordering: after PR 15C and before plan 22 PR 24O; the scout consumes these authorized temporal retrieval/context views and cannot land a parallel session-search path.
- Expose search, anchor hydration, temporal lineage, context assembly, session/thread replay, and eval use cases from one catalog/view model.
- Preserve compact Markdown default, explicit JSON/NDJSON, stable cursor/coverage shapes, and legacy compatibility mappings.

### PR 31P — Search Quality Lab temporal and LCM replay workspaces

- Ship the Search Quality Lab's session-temporal workspaces: candidate waterfall, temporal lineage, copy cluster, summary DAG, context budget, corpus/qrel/pool version browsers, judgment supersession and adjudication, generic durable experiment run/cancel/resume/retry/minimize, aggregate report publication, scanned fixture promotion, retrieval-profile publish/activation, then-versus-now replay, and aggregate comparisons through plan 15 §0.1 plus plan 10 §8.5.
- Consume generated clients and shared evaluation artifacts only.

### PR 33E — V1 import, backfill, and shadow comparison

- Import every V1 session/LCM source with receipts, rebuild projections/indexes, and run frozen/rolling shadow evaluation.
- Treat conflicting store identities as separate partial sources until explicit consolidation.

### PR 35I — Session/LCM retrieval cutover

- Cut over only after exact, temporal, privacy, duplicate, coverage, output, provider, latency, and replay gates pass.
- Restart/rehandshake clients at route generation; no uncertain request replay or per-call semantic fallback.

### PR 37I — Retire duplicate V1 session/LCM/search paths

- Delete V1 FTS/ranking/LCM query/render/dashboard paths after archive restore and replay proof.
- Keep only versioned import readers required by the bounded archival policy.

## 16. Definition of done

- One query/temporal/context engine serves message, Turn, session, thread, agent, workflow, LCM, dashboard, hooks, CLI, MCP, API, and SDKs.
- Every result has a durable retrieval anchor and can hydrate/replay across projects without CWD/store switching.
- Raw source occurrences remain immutable/addressable; logical copies collapse only through evidence-backed versioned assertions.
- Current mode follows explicit valid-time/supersession/authority evidence; as-of mode has zero future leakage; evolution and forensic modes preserve history/conflict.
- Recency is intent-scoped, bounded, explained, and never the sole truth rule.
- Raw, summary, entity, semantic, graph, and time candidates have source horizons, versions, ablations, and rank explanations.
- TraceDecay-owned compaction/LCM summaries request cataloged `gpt-5.6-terra` with `ModelReasoningEffortV1::ExtraHigh` by profile default, record requested/actual/fallback routes, and never silently downgrade or conflate host-native compaction.
- Every V2 summary publishes atomically with a validated marker/anchor manifest; consequential claims resolve to exact authorized sources, stale/revoked markers remain explicit, models cannot mint anchors, and successor summaries preserve immutable lineage.
- Cross-shard ranking does not compare uncalibrated BM25 scores or hide skipped/conflicted stores.
- Default output is compact Markdown; explicit JSON/NDJSON uses the same typed view, stable pagination, coverage, and hydration anchors.
- At least 500 real query episodes and 5,000 manually grounded judgments satisfy the coverage gates; human labels remain authoritative.
- nDCG/MRR/Recall/precision, temporal accuracy, supersession safety, duplicate rate, abstention/calibration, coverage, latency, tokens, and cost are reported per project/provider/intent/time/scope stratum.
- TD-SR-001 through TD-SR-012 are regression fixtures with stable anchors or explicit unavailable-coverage receipts.
- The Search Quality Lab's session-temporal workspaces explain one result, one temporal lineage, one copy cluster, one summary DAG, one context assembly, and one then-versus-now replay without mutating production state.
- Task packets use canonical task/initiative/dependency/work-claim/thread/Turn filters, surface current-versus-superseded decisions, and exclude unrelated global-board or sibling work from the default context budget.
- All retrieval/config/privacy/model settings are visible and controllable through Settings plus generated CLI/MCP/API/SDK surfaces.
- Optional embeddings, learned sparse retrieval, rerankers, synthesizers, and background intelligence remain removable and cannot bypass lexical exactness, temporal safety, privacy, or replay evaluation.
- V1 message/LCM/search implementations are removed after cutover; no permanent dual semantics remain.
