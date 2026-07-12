# TraceDecay V2 Search Quality, Evaluation, and Retrieval Plan

> **For implementation agents:** Search quality claims require a versioned corpus, qrels, frozen cutoff, competing baselines, per-stratum metrics, latency/resource measurements, and inspected regressions. “Added embeddings” is not a result.

**Goal:** Make message/session/Turn/agent/workflow/Git/code/memory/automation retrieval precise enough to supply useful context on the first page across TraceDecay's real local multi-project history while preserving exact technical search, privacy, explanations, stable retrieval IDs, temporal correctness, and calibrated no-result behavior.

**Decision:** Keep exact phrase/BM25 as the mandatory baseline. Evaluate character-fuzzy, entity/graph, local dense, learned-sparse, fusion, late interaction, cross-encoder reranking, expansion, recency, clustering, and diversification as independently removable stages. No neural component becomes default without measured real-world gains.

**Publication snapshot:** [master §2.6](../2026-07-09-tracedecay-brain-rewrite.md#26-current-master-accepted-changes) plus [plan 13](13-research-provenance-and-context-anchors.md) are normative. Refresh before corpus freeze. Treat divergent session variants, bounded indexed consolidation lookup, conflict-safe registry healing, repair-free search reads, peer-safe graph checkpoints, and restart-safe manifest retirement as accepted behavior; retain every pre-fix failure as a labeled regression.

## 0. Boundary and contract integration

- Plan 01 owns the exact `ScopeSelectorV2`, canonical entity IDs, domain `RetrievalAnchorId`/`RetrievalAnchorRecordV1`, evidence/provenance, time, sensitivity, and retention types. Search does not define `project_key`, `ScopeExpr`, `retrieval_ref`, or a ranking-only anchor.
- Plans 04–05 own typed search documents, representative clusters, candidate execution, fusion/ranking, distributed cursor, coverage, and explain. This plan supplies evaluation contracts and promotion gates; it does not become a second query engine.
- Plan 16 supplies cross-repository resolver/routing semantics and the Rspack/Rsbuild/React Router scope corpus. Search consumes the canonical resolved selector and preserves repository/worktree/ref/snapshot identity; it never repairs a wrong scope after retrieval.
- Plan 09 is the sole application boundary for universal search, benchmark/evaluation reads, label/corpus commands, and Search Quality Lab composition. Plan 10/17 expose those use cases through the one official API/catalog/generated SDK contract; plan 11 visualizes the same results.
- Plan 18 is authoritative for sanitizer/taint wrappers. Queries enter evaluation as protected refs or `Unclassified<T>` that are sanitized before persistence; candidates, qrels, summaries, embeddings, explanations, reports, problems, and exports contain only eligible wrappers or explicit redacted/unknown states.
- Plan 23 is the normative message/session/LCM temporal specialization and contributes logical-copy, summary-horizon, current/as-of/evolution/forensic, supersession, and context-assembly strata to this shared corpus/evaluation program. Plan 22 consumes promoted retrieval profiles through bounded anchored reads for the optional Context Scout; scout outcomes never become relevance truth automatically.
- Plan 24 consumes promoted retrieval profiles for task query, decomposition evidence, temporally correct context packets, prior-attempt recovery, and sibling-materiality selection. Task completion, route success, or a model's use of a packet is outcome evidence, never an automatic relevance label; packet qrels include mandatory, useful, redundant, stale, forbidden, and missing-with-coverage classes.
- Evaluation rows carry canonical `RetrievalAnchorId`s. Safe bulk labels use `retrieval_anchors.metadata_batch_get` at `POST /api/v2/retrieval-anchors:metadata-batch`; exact evidence inspection uses separately authorized `retrieval_anchors.resolve` at `POST /api/v2/retrieval-anchors:resolve`; a replayable recipe uses `retrieval_recipes.execute` at `POST /api/v2/retrieval-recipes:execute`. Evaluation code defines no alias, GET-by-anchor route, or combined metadata/payload hydration operation.
- Search evaluators are read-only against production retrieval state. Corpus runs use plan 10 §8.5's generic experiment lifecycle, which may persist immutable replay artifacts and explicitly granted model/egress cost but cannot update live profiles, judgments, counters, or indexes. Creating/freezing a corpus or qrel version, recording/superseding a judgment, adjudicating, publishing a report, promoting a synthetic fixture, publishing a profile, or activating it is a separate direct typed command with expected version, idempotency, sanitization evidence where applicable, and an audit receipt. Immutable versions are superseded, never edited or fictionally rolled back.

### 0.1 Canonical evaluation operation family

Plan 09 owns these application operations; plans 10/17 generate HTTP and SDK bindings from the same catalog entries, and plan 11 renders them. No lab-local action or query-engine helper is another mutation path.

| Class | Canonical operations | Contract |
|---|---|---|
| Artifact reads | `retrieval.corpus_versions.list/get`, `retrieval.qrel_versions.list/get`, `retrieval.candidate_pools.list/get`, `retrieval.judgments.list/get`, `retrieval.adjudications.list/get` | Return immutable version/cutoff/owner/member digests, supersession/adjudication lineage, protected-payload availability, and coverage; list metadata contains no query/rationale text. |
| Run/report reads | Generic experiment/run/cell/stage/comparison/comparison-cell/reduction `list/get` filtered to `LabKindV1::SearchQuality`; `retrieval.evaluation_reports.list/get` | Experiments return frozen inputs, operation state, explicit variant/evaluator/corpus-case/repetition/sweep coordinates, stages, metrics/strata/denominators, privacy/resource receipts, regressions, anchors, and zero-live-effect proof. Reports return immutable publication state. No retrieval-local run resource exists. |
| Profile reads | `retrieval.profiles.list/get` | Return immutable ranking/config/model/index manifests, promotion evidence, activation history, compatibility, and typed unavailable state. |
| Corpus/qrel commands | `retrieval.corpus_versions.create/freeze`, `retrieval.qrel_versions.create/freeze`, `retrieval.candidate_pools.create` | Create a new draft version or freeze its exact membership/cutoff/digest. Freeze is one-way; later evidence creates a successor. |
| Ground-truth commands | `retrieval.judgments.record/supersede`, `retrieval.adjudications.record` | Append human or explicitly secondary labels; correction points to the prior judgment; adjudication retains every source label/rationale and never rewrites disagreement. |
| Evaluation commands | Generic `experiments.create`, `experiment_runs.create/cancel/resume/retry/minimize`; `retrieval.evaluation_reports.publish` | Run a durable frozen Search Quality experiment through the one operation lifecycle and publish only a reviewed aggregate/redacted report whose metric rows bind the exact experiment run and artifact versions. Cancellation cannot erase completed work. |
| Promotion commands | Shared `experiments.fixtures.promote`; `retrieval.profiles.publish/activate` | Promote any evaluator fixture, including Search Quality, only through the typed synthetic/reviewed-sanitized source/cell/secret-scan receipt command; publish an immutable retrieval profile; activate only after locked gates and expected active-version CAS. Running queries remain pinned. |

Each command returns one ordinary command/operation receipt. Permission to label, adjudicate, run experiments, grant model/egress spend, publish reports, promote fixtures, publish profiles, and activate profiles is separate; a broad search-read grant conveys none of them.

## 1. Current supported-surface probe

The 2026-07-10 probe used `tracedecay tool message_search --catch-up false` over the live local store:

| Query | Useful behavior | Failure |
|---|---|---|
| Exact disk-full/non-SQLite corruption phrase | Found the exact user issue. | Prior tool command containing the query ranked first; copied assistant delegation also preceded/duplicated source evidence. |
| Paraphrased “storage corrupted because volume ran out of space during indexing” | Recalled many disk/build/cache failures. | The exact graph-store corruption case did not appear in top ten; topical vocabulary dominated intent. |
| Exact doctor/foreign-installation/update-refuses phrase | Found exact issue, implementation, and review context. | Workflow/delegation/implementation copies diluted the direct user evidence. |
| Paraphrased impossible-remediation query | Found related doctor/skill traffic. | Mixed exact issue with unrelated health, install, and task-notification rows. |
| Conceptual accidental duplicate-agent work query | Found useful shared-worktree/parallel-agent cases. | Repeated copied sessions and generic worktree results lowered precision. |
| Misspelled hint/subagent query | Found current exact misspelled request. | This was effectively a self-match and does not establish general fuzzy quality. |

Conclusion: current search is valuable forensic discovery for rare literals, but not yet a dependable best-context retriever. Exact technical recall must be preserved while improving origin/type precision, semantic paraphrase, diversity, temporal correctness, and abstention.

## 2. Primary research and applied decisions

- [Okapi at TREC-3 / BM25](https://www.microsoft.com/en-us/research/publication/okapi-at-trec-3/) — retain a fast, interpretable lexical baseline; identifiers, errors, paths, tool names, and quoted text strongly favor exact lexical evidence.
- [Dense Passage Retrieval](https://arxiv.org/abs/2004.04906) — evaluate dense recall for paraphrases against strong BM25; do not assume semantic superiority.
- [SPLADE v2](https://arxiv.org/abs/2109.10086) — test learned sparse expansion as an optional local semantic channel with inverted-index behavior and inspectable terms.
- [Reciprocal Rank Fusion](https://plg.uwaterloo.ca/~gvcormac/cormacksigir09-rrf.pdf) — use deterministic RRF as the first robust hybrid baseline before learned fusion.
- [Passage Re-ranking with BERT](https://arxiv.org/abs/1901.04085) — cross-encode only a bounded candidate set; measure precision gain versus latency/memory.
- [ColBERT](https://arxiv.org/abs/2004.12832) and [ColBERTv2](https://arxiv.org/abs/2112.01488) — benchmark late interaction as an optional middle point; index footprint and local inference cost are release gates.
- [Document Expansion by Query Prediction](https://arxiv.org/abs/1904.08375) — expansion may reduce vocabulary mismatch, but generated terms are versioned/explained and never quoted as source evidence.
- [ANCE](https://arxiv.org/abs/2007.00808) — mine hard negatives from real high-scoring false positives, wrong projects/times, copied prompts, and rejected results rather than random rows alone.
- [Maximal Marginal Relevance](https://aclanthology.org/X98-1025/) — diversify repeated turns/summaries/sessions while preserving distinct evidence.
- [HippoRAG](https://arxiv.org/abs/2405.14831) and [G-Retriever](https://arxiv.org/abs/2402.07630) — typed bounded graph expansion can recover multi-hop context; unrestricted GraphRAG is not the default.
- [BEIR](https://arxiv.org/abs/2104.08663) — heterogeneous domains vary materially; report project/provider/query-stratum results rather than one mean.
- [TREC relevance judgments](https://trec.nist.gov/data/reljudge_eng.html) and [pooled versus sampled judgments](https://www.nist.gov/publications/comparison-pooled-and-sampled-relevance-judgments) — pool candidates from every system and add sampled negatives to reduce evaluation bias.
- [Optimized Interleaving](https://www.microsoft.com/en-us/research/wp-content/uploads/2013/02/Radlinski_Optimized_WSDM2013.pdf.pdf) — use explicit UI interleaving only after offline/shadow safety; automatic hint contexts need stricter replay/A-B guardrails.

## 3. Retrieval document and identity model

Index separate typed grains instead of flattening all content into one FTS row:

- Native message/content part and representative message cluster.
- Turn with start context, user/agent exchange, tools, visible reasoning artifacts, goal/work-claim state, and output.
- Session/thread summary with exact source ranges/coverage.
- Agent instance, goal/task/work claim, workflow run/phase/agent, and handoff.
- Git ref/commit/diff/PR/check/review/release evidence.
- Code file/symbol/snapshot/diagnostic/test/impact evidence.
- Fact/version/entity/decision/contradiction/retrieval/feedback.
- Automation job/run/artifact/proposal/skill/version/outcome.

Every result carries a domain `RetrievalAnchorId` resolving to `RetrievalAnchorRecordV1`, plus stable canonical/native aliases, resolved `ScopeSelectorV2`/repository/worktree/ref/snapshot identity, project/profile/privacy owner, provider, occurred/ingested/valid time, origin/audience/kind, source range, representative/hidden-row membership, index/model version, evidence class, sanitization/redaction state, and coverage. Summary/embedding documents never replace source IDs or become the sole anchor.

## 4. Versioned candidate pipeline

### 4.1 Query understanding

- Preserve quoted phrases, exact IDs, error text, paths, symbols, API/tool/config names, and case-sensitive literals.
- Normalize Unicode and punctuation with visible tokenizer/version.
- Resolve provider/tool/project/branch/PR/session/agent/goal aliases into candidate entities without removing original tokens.
- Detect explicit time, scope, audience/origin, result kind, exact-versus-conceptual, and no-answer intent.
- Generate optional spelling/fuzzy/expansion terms as separate explained channels; never alter quoted evidence.

### 4.2 Recall channels

1. Exact ID/native alias and exact phrase.
2. Field-weighted FTS5 BM25 over typed fields.
3. Character n-gram/edit-distance fuzzy channel for typos and partial identifiers.
4. Entity/event/goal/tool/Git/code indexes.
5. Agent/session/work-claim/relation graph seeds and bounded typed expansion.
6. Summary DAG documents with source coverage and retained-time limits.
7. Optional privacy-domain local dense representation.
8. Optional learned-sparse/SPLADE representation.
9. Explicit recency/time-distance feature only when query/time profile warrants it.
10. Temporal-assertion/current-state index channel for session-temporal intents (specified in plan 23 §5.2; executed by plan 05's `session/temporal_resolver.rs`).
11. Explicit recent-activity listing channel for listing intents only — the `list_sessions`/`list_messages` list intents defined in plan 05 §6.2.

Each channel returns a bounded candidate list with channel rank/score, matching fields/terms/entities, index watermark, truncation, and latency.

### 4.3 Fusion, noise control, and diversity

- Start with the fusion contract defined once in plan 05 §11.3: deterministic RRF over declared channels within each shard, then the calibrated cross-shard merge with exact-match tiers first. Plan 05 owns byte determinism for a fixed layout and bounded repartition-drift tests (exact-match-tier invariance plus locked top-k-overlap/nDCG floors with explanations), not impossible byte-identical output after arbitrary repartition. Learned fusion is a later ablation requiring feature/version explanation.
- Cluster copied parent prompts, provider protocol/tool echoes, same-content cross-store rows, summary/source lineage, and repeated assistant status messages.
- Select representative by requested audience/origin/kind and evidence quality; report hidden counts and exact sanitized-native expansion.
- Penalize the active query/tool command echo, inventory listings, protocol notifications, and same-session repetition unless requested explicitly.
- Preserve exact identifier/phrase hits before semantic diversification.
- Apply bounded MMR/session-project-provider-agent diversity so ten near-identical children do not occupy the page.
- Use typed project/time/privacy constraints before ranking; do not ask a reranker to repair unsafe scope.

### 4.4 Bounded reranking

- Compare no reranker, local cross-encoder, and ColBERT-style late interaction over a fixed top-N pool.
- Inputs include query, typed result grain, safe content slice, entity/evidence features, time/scope, and cluster metadata.
- Secret-classified or locked content uses lexical/entity-only mode unless an explicitly authorized local model/index exists in the same privacy domain.
- Missing model or timeout falls back to the declared pre-rerank order with reason; it never returns a silently shorter set.

### 4.5 Bounded graph expansion

- Expand only high-confidence lexical/semantic/entity seeds.
- Legal relation kinds, direction, evidence class, confidence floor, time window, depth, node/edge budget, and privacy frontier are explicit.
- Multi-hop results explain the path. A graph neighbor is not relevant merely because it is connected.
- Agent proximity uses current `AgentPresence`/`WorkClaim`; historical search uses versioned events, not live state.

## 5. Real local multi-project evaluation corpus

The private qrel store belongs to the active profile and is never committed. A redacted/synthetic subset plus aggregate metrics is checked in.

The frozen research corpus this program draws on is pinned by its owner, plan [`13-research-provenance-and-context-anchors.md`](13-research-provenance-and-context-anchors.md): path set `/fast/tracedecay-redesign-research/*`, file mode `0600`, final user-message cutoff `2026-07-11T01:04:10.875Z`, integrity verified against plan 13's final manifest hashes. The manifest distinguishes the broad supported-surface capture from the 47-record active-session raw-rollout fallback: the original 28 prompts, 11 reconciliation prompts, and 8 final cross-check prompts. Evaluation inputs derived from it cite that exact manifest version, and no private content from it enters the repository.

### 5.1 Query sources

- Actual later human prompts that refer to an earlier issue, plan, session, PR, worktree, decision, fact, tool, or agent.
- Explicit “find/recall/show/go back to” requests and later corrections when the wrong session/project was returned.
- Search reformulations and abandoned queries.
- Successful retrieval-to-action sequences where the user/agent opened an anchor and continued relevant work.
- Agent duplication/coordination cases, branch/PR context, diagnostics, memory, automation, hints, provider integration, dashboard, and release incidents.
- Synthetic/adversarial variants: misspellings, abbreviations, renamed projects, old aliases, exact error/API strings, paraphrase, negation, expected no result, wrong-project near match, wrong-time superseded evidence.

For a query at time `t`, the candidate corpus is frozen to records whose allowed ingest/observation cutoff is `< t`. Later messages, summaries, labels, branches, or outcomes cannot leak into retrieval features.

### 5.2 Strata and holdouts

- Every registered project with sufficient data; small projects are grouped only for reporting, not omitted silently.
- Codex, Claude, Cursor, Hermes, and other supported provider families.
- Human/direct-user, subagent/delegated, assistant, tool-result, provider-protocol, summary, and unknown origin.
- Exact identifier/literal, phrase, typo, conceptual/paraphrase, temporal, cross-project, multi-hop, Git/PR, code/symbol, memory/fact, automation/skill, hint/tool-routing, nearby-agent, and no-result query.
- Recent versus old evidence, short versus long content, single versus many candidate sessions, healthy versus partial/retained/locked shard.

Use chronological train/dev/test splits plus full holdout of selected projects, providers, and later time blocks. Frozen test judgments are never used to tune weights, prompts, expansion, models, or thresholds.

### 5.3 Candidate pooling and hard negatives

- Pool top 20 from current search, exact/BM25, fuzzy, dense, learned sparse, RRF, graph, expansion, late-interaction, and cross-encoder variants.
- Add stratified random candidates to reduce pooling bias.
- Add hard negatives: same terms/wrong project, same project/wrong time, copied child prompt, current query/tool echo, superseded fact, observed-not-produced commit, nearby but disjoint agent, stale summary, protocol row, and same title/different session.
- Version the pool. New systems add candidates and new judgments without rewriting prior qrels.

### 5.4 Judgment contract

Label each query-result pair:

- `0 misleading_or_irrelevant`.
- `1 topical_but_not_actionable`.
- `2 useful_context`.
- `3 decisive_or_smallest_sufficient_anchor`.

Also label duplicate/echo/protocol noise, wrong project/provider/time/origin, stale/superseded, privacy-ineligible, relation/evidence requirement, and whether the result enables the next action. Record the smallest sufficient anchor grain: message, Turn, session, agent, goal, work claim, workflow, commit/PR, fact, code entity, or evidence bundle.

A substantial stratified subset is independently double-labeled. Report raw agreement and an ordinal agreement statistic; adjudicate disagreement while retaining both labels/rationales. Ambiguous cases remain distributions/unknown, not forced truth.

The judgment record is concrete, not deferred. These contracts land in plan 01's `tracedecay-domain::retrieval::evaluation`; this plan owns their semantics and plan 02 lowers them without replacement aliases:

```rust
pub enum JudgeRefV1 {
    Human(ActorRef),
    ModelSecondary {
        model: ModelCatalogEntryId,
        revision: ModelRevisionId,
        prompt_manifest: ManifestId,
    },
}

pub enum JudgeKindV1 { Human, LlmSecondary }
pub struct RelevanceGradeV1(u8); // constructor accepts only 0..=3

pub enum NoiseLabelV1 {
    Duplicate,
    QueryEcho,
    ToolEcho,
    ProtocolNoise,
    CopyOnly,
    SummaryOnly,
    StaleSummary,
}
pub enum ScopeAssessmentV1 {
    InScope,
    WrongProject,
    WrongRepository,
    WrongWorktree,
    WrongRef,
    WrongProvider,
    WrongThreadOrAgent,
    WrongOrigin,
    Ambiguous,
    Unknown,
}
pub enum TemporalAssessmentV1 {
    Current,
    HistoricalValid,
    Stale,
    Superseded,
    Revoked,
    Conflicted,
    FutureLeak,
    Unknown,
}
pub enum PrivacyAssessmentV1 { Eligible, RedactedUseful, Ineligible, Locked, Unknown }
pub enum EvidenceAssessmentV1 {
    Sufficient,
    MissingRequiredRelation,
    WrongRelation,
    CandidateOnly,
    Unknown,
}
pub enum NextActionAssessmentV1 { Enables, DoesNotEnable, Unknown }
pub enum NoAnswerAssessmentV1 {
    NotApplicable,
    CorrectAbstention,
    FalsePositiveResult,
    FalseAbstention,
    Unknown,
}
pub enum AnchorGrainV1 {
    Message,
    Turn,
    Session,
    Agent,
    Goal,
    WorkClaim,
    Workflow,
    CommitOrPullRequest,
    Fact,
    CodeEntity,
    EvidenceBundle,
}
pub struct SecondaryLabelsV1 {
    pub noise: BTreeSet<NoiseLabelV1>,
    pub scope: ScopeAssessmentV1,
    pub temporal: TemporalAssessmentV1,
    pub privacy: PrivacyAssessmentV1,
    pub evidence: EvidenceAssessmentV1,
    pub next_action: NextActionAssessmentV1,
    pub no_answer: NoAnswerAssessmentV1,
}

pub struct JudgmentRecordV1 {
    pub judgment_id: JudgmentId,                     // primary key
    pub corpus_version: CorpusVersionId,
    pub qrel_version: QrelVersionId,
    pub query_episode_id: QueryEpisodeId,
    pub anchor_id: RetrievalAnchorId,
    pub judge: JudgeRefV1,
    pub judge_kind: JudgeKindV1,
    pub grade: RelevanceGradeV1,
    pub secondary_labels: SecondaryLabelsV1,
    pub smallest_sufficient_grain: AnchorGrainV1,
    pub rationale_ref: Option<PayloadRef>,            // sanitized/private, receipt-bound
    pub labeled_at: UtcMicros,
    pub supersedes: Option<JudgmentId>,
}
```

- Uniqueness: `(qrel_version, query_episode_id, anchor_id, judge)`; required indexes: `(query_episode_id, anchor_id)`, `(corpus_version, qrel_version)`, and `(judge_kind, labeled_at)`.
- Retention/size envelope: append-only and never edited — corrections publish a superseding row via `supersedes`; at least 5,000 rows for the initial gate and tens of thousands over the program's life.
- Owning store: the active profile's activity shard, inside plan [`02-store-crate.md`](02-store-crate.md)'s protected evaluation table family. This is a privacy/authorization family, not another physical shard; committed fixtures remain redacted/synthetic only.
- Plan 23 §8.3 consumes the typed `SecondaryLabelsV1` dimensions above for session-temporal cases and defines no second label or judgment record.

### 5.5 Semantic goldens versus cardinality-faithful load/eval data

Two fixture lanes are intentionally separate. Small hand-reviewed semantic/provider goldens prove exact identity, cutoff, origin, provider-wire, ranking, explanation, privacy, and no-answer behavior; their compact manifest cannot support a scale, distribution, or promotion-evidence claim. A deterministic cardinality-faithful generator separately builds the current/10× load/eval corpus from a fixed seed and schema version. It emits unique, referentially linked project, message, event, symbol, branch/ref/snapshot, retrieval-anchor, relation, and payload identities, including declared cross-project and temporal links; reusing one placeholder ID or payload across nominal rows is a fixture failure.

The generator records expected count and histogram assertions for projects, providers, messages, events, symbols, branches, refs/snapshots, anchors/relations, payload sizes, origin/time/retention classes, fan-out, selectivity, and missing/partial/no-answer cases. Verification recomputes those distributions by decoding and joining the emitted records, then compares them to the checked expectations. Coverage is derived from record content and resolvable links, never accepted from manifest tags, filenames, generator branch labels, or requested-count metadata. Synthetic generated rows exercise load/evaluation mechanics but never count toward §7.1's real-query or human-grounded promotion minimums.

## 6. Metrics

### Retrieval quality

- Precision@1/3/5.
- Recall@5/10/20.
- MRR and first-useful rank.
- nDCG@10 using 0–3 grades.
- Correct abstention/no-answer accuracy and calibration (Brier score and expected calibration error, shared with plan 23 §8.5).
- Success within configured result/token/byte budget.
- Judged coverage and unjudged rate.

### Product correctness

- Duplicate/echo/protocol-noise rate.
- Wrong-project, wrong-provider, wrong-time, stale/superseded rates.
- Project/provider/session/agent diversity after exact-hit preservation.
- Retrieval-ID resolution validity and exact sanitized-native expansion.
- Useful-project coverage and privacy/redaction correctness.
- First-useful latency and bytes/tokens returned.

### Operational cost

- p50/p95/p99 stage and end-to-end latency.
- CPU time, peak RSS, model load/warmup, index size, build/update lag, shard opens, and cache hit.
- Query cancellation and fallback latency.
- Per-privacy-domain representation bytes and rebuild time.

Report macro and micro results, confidence intervals, every primary stratum, and the worst project/provider/query class. Aggregate improvement cannot hide a material regression in exact IDs, no-answer, privacy, or a low-volume provider.

## 7. Offline, shadow, and online evaluation

### 7.1 Frozen offline gates

- Current production search is baseline A.
- Fielded BM25/phrase/origin filtering is baseline B.
- Every added channel has an ablation and resource profile.
- Exact ID and exact phrase Precision@1/Recall cannot regress beyond an explicitly reviewed case.
- Candidate default must improve predeclared Precision@3, nDCG@10, first-useful rank, and duplicate rate on dev and untouched test, with no material worst-stratum/privacy/no-answer regression.
- Gates are calibrated on frozen development data, then locked before test evaluation; no absolute corpus-independent nDCG/recall floor is pre-committed. Plans 05 §11.3/§17, 06, 23 §8.6, and the master gate list cite this regime.
- **Material worst-stratum regression, numeric definition (cited by every other plan):** a worst-stratum nDCG@10 drop greater than `max(2 points absolute, 5% relative)` versus the locked baseline, or any no-answer-precision drop greater than 2 points.
- **Promotion evidence minimums (this plan owns the shared corpus; matching plan 23 §8.2, the regression matrix, and the master gates):** at least 500 real query episodes, and candidate pools plus manual labels sufficient for at least 5,000 human-grounded judgments, with independent double labels on at least 20% of judgments. A promotion claim on a smaller corpus is invalid regardless of scores.

### 7.2 Rolling local evaluation

- Maintain frozen regression, rolling recent, project holdout, provider holdout, temporal holdout, and adversarial typo/identifier/no-result sets.
- Mine hard negatives from opened-then-rejected results, reformulations, ignored/incorrect hints, explicit corrections, duplicate sessions, wrong project/time, and newer evidence that superseded a stale result.
- Refresh rolling sets under versioned cutoff; never edit frozen judgments to improve a score.

### 7.3 Shadow evidence

- Candidate rankers run against real eligible queries without changing delivered results/context.
- Store ranked stable IDs, explanations, resource cost, and coverage—not sensitive query text in analytics.
- Compare chosen/expanded/loaded anchors, later actions, reformulation, abandonment, and corrections as weak evidence requiring calibration/manual judgment.

### 7.4 Controlled online comparison

- Randomized interleaving is allowed only in explicit Search/Explorer UI with opt-in telemetry and reversible assignment.
- Automatic hook/hint retrieval uses historical replay and shadow first, then a narrow A/B with strict relevance/repetition/token/latency/correction guardrails.
- Never explore aggressively inside agent prompts or silently change the historical context of an in-progress workflow.
- Helpful/unhelpful labels and result actions are feedback events, not direct training truth.

## 8. Search Quality Lab

Inputs:

- Exact historical query/Turn anchor or sanitized synthetic query.
- Frozen corpus cutoff, projects/providers/origins/kinds/time/privacy.
- Candidate retrieval profile, tokenizer/index/model/graph/ranker versions, and budgets.
- Optional qrel/corpus version and compare profile.

Outputs:

- Query parse/normalization/expansion and resolved aliases/entities.
- Per-channel candidates, scores/ranks/matches/watermarks/caps/latency.
- Fusion, cluster/representative, diversity, graph path, rerank, exclusion, privacy, and fallback decisions.
- Exact final ranked IDs with one-line reason and native expansion.
- Per-query labels/metrics plus aggregate/per-stratum comparison and inspected regressions.
- Equivalent CLI/MCP/HTTP request and deterministic aggregate/redacted evaluation receipt.

The evaluator has no production write ports. The canonical commands in §0.1 perform corpus/qrel creation and freeze, judgment/supersession/adjudication, generic experiment run/cancel/resume/retry/minimize, aggregate report publication, sanitized fixture promotion, and profile publish/activation with stable anchors, optimistic versions, and audit.

## 9. Implementation file plan

This plan owns no module tree. Ranking, fusion, and evaluation-metrics code lives only in plan 05's `crates/tracedecay-query` tree (05 §5); there is no `src/retrieval/` directory and no second `eval/metrics.rs`. The capabilities this plan specifies are requirements on these 05-owned modules:

| Capability (this plan) | Plan 05 module |
|---|---|
| Query understanding (§4.1) | `ast.rs` parse/canonicalize, `operators/entity.rs` alias resolution, `session/intent.rs` intent detection |
| Exact ID/phrase channel (§4.2.1) | `operators/filter.rs` + `operators/fts.rs` |
| Fielded BM25 channel (§4.2.2) | `operators/fts.rs` + `rank/lexical.rs` |
| Character fuzzy channel (§4.2.3) | `operators/fuzzy.rs` |
| Entity channel (§4.2.4) | `operators/entity.rs` |
| Graph seeds and bounded expansion (§4.2.5, §4.5) | `operators/graph.rs` |
| Summary-DAG channel (§4.2.6) | `operators/summary.rs` |
| Optional dense channel (§4.2.7) | `operators/vector.rs` |
| Optional learned-sparse channel (§4.2.8) | `operators/learned_sparse.rs` |
| Recency feature (§4.2.9) | `rank/features.rs` |
| Temporal-assertion channel (§4.2.10) | `session/temporal_resolver.rs` |
| Listing channel (§4.2.11) | the 05 §6.2 list intents |
| Fusion (§4.3) | `rank/rrf.rs` + `execute/merge.rs`, defined once in 05 §11.3 |
| Copy clustering/representative selection (§4.3) | `rank/cluster.rs` |
| Diversity (§4.3) | `rank/diversity.rs` |
| Bounded rerank stage (§4.4) | `rank/rerank.rs` |
| Explanations (§8) | `rank/explain.rs` + `explain.rs` |
| Corpus/cutoff/pool/qrels/metrics/strata/agreement/ablation/report (§§5–7, §11) | `eval/{corpus,cutoff,pool,qrels,metrics,strata,agreement,ablation,report}.rs` — `eval/metrics.rs` is the single metrics implementation, shared with plan 23 §8.5 |
| Cardinality-faithful deterministic load/eval generation (§5.5) | `eval/load_fixture.rs` plus record-derived distribution assertions in `tests/retrieval_eval_load.rs`; semantic/provider goldens remain in focused channel/provider fixtures |

Companion ownership:

- Projectors build typed search documents, representative clusters, entities, summaries, and relation indexes.
- Store owns privacy-domain representation bytes/manifests and immutable eval artifacts.
- Policy chooses an approved retrieval profile for hints/memory/coordination; it does not execute search.
- Application owns Search/Explorer/evaluator reads, the generic experiment harness, and every §0.1 command.
- API exposes `POST /api/v2/search` for live bounded retrieval and uses plan 10 §8.5's generic experiment draft/create/run/cell/trace/comparison routes with `LabKindV1::SearchQuality` for every benchmark/evaluation, plus the complete versioned evaluation read/command family in plan 10 §8; no `/search/benchmark` or `search.benchmark.evaluate` duplicate exists. CLI/MCP/SDK bindings derive from the same catalog entries. Search Quality owns evaluator stages and metrics, not a lab-specific run/status/cancel endpoint.
- Dashboard owns Search Quality Lab, corpus/pool/qrel/judgment/adjudication/experiment/report/profile workspaces, explanations, and aggregate charts/tables. It invokes only generated operations and never edits an artifact locally.

## 10. PR sequence

### Companion requirements for PR 13A: Time-safe real-world eval harness and current baselines

- Implement the private corpus/qrel stores per the §5.4 `JudgmentRecordV1` contract, stable anchors, time cutoff, strata, pooling, labels, metrics, agreement, aggregate/redacted reports.
- Capture the six-query probe plus exact identifier, typo, no-answer, cross-project, provider, Git, memory, and nearby-agent fixtures.
- Keep those small semantic/provider goldens separate from the fixed-seed cardinality-faithful generator. Generate unique linked project/message/event/symbol/branch/ref/snapshot/anchor/relation/payload identities at current and 10× envelopes; recompute and assert every §5.5 distribution from decoded record content, and fail if a manifest tag is the only coverage evidence.
- Benchmark current production and fielded BM25 without changing default search.

### Companion requirements for PR 13B: Exact/phrase/BM25, origin/kind fields, self-echo, clustering

- Implement query understanding, exact preservation, fielded BM25, origin/audience/kind filters, query/tool-echo penalty, representative cluster, hidden counts, rank explanation.
- Prove native expansion and exact technical regression gates.

### Companion requirements for PR 13C: Fuzzy/typo and diversity

- Add character channel, alias handling, MMR/session-project-provider diversity, adversarial typo corpus, and resource caps.

### Companion requirements for PR 14A: Optional native FastEmbed semantic code channel

- Consume plan 31's frozen code corpus and compare exact/lexical/nonsemantic baselines against FastEmbed `JinaEmbeddingsV2BaseCode`; run `GTELargeENV15Q` as the required comparator under the same pool, resource, and snapshot budgets.
- Pin exact FastEmbed/runtime/model/tokenizer/chunker/dimension/metric/normalization/index/session/batch manifests; report exact-hit retention, wrong-scope/no-answer behavior, per-language/intent/worst-stratum quality, full/incremental build, cold/warm latency, session reuse, RSS, and bytes.
- Keep every semantic profile disabled by default until the predeclared promotion gates pass. Generic learned-sparse evidence may remain research-only but creates no second production code-embedding runtime.

### Companion requirements for PR 14B: Hybrid fusion, bounded graph, and hard-negative loop

- Add RRF profiles, typed bounded graph expansion, hard-negative mining, cross-project/provider/time holdouts, and ablations.

### Companion requirements for PR 14C: Optional native and model-assisted rerank

- Compare no rerank with native FastEmbed `BGERerankerV2M3` over at most the top 25 fused candidates; this is the no-external-process acceptance baseline.
- Separately benchmark an explicit opt-in registered Codex Spark/app-server-style model capability or equivalent discovered capability over a bounded top-N projection. Pin privacy/egress, token/cost, deadline/cancellation/concurrency budgets and requested/actual model receipts; permit no vector generation or relevance-label writeback.
- Ablate native versus model-assisted reranking on identical pools. Missing capability, substitution, refusal, timeout, malformed output, or budget failure preserves the exact pre-rerank order and never silently selects another route. Reuse plan 22 capability/gateway receipt conventions without coupling evaluation to Context Scout delivery.

### PR 31J: Search Quality Lab and qrel review

- Ship corpus/qrel version and candidate-pool browsers; append-only judgment/supersession/adjudication review; generic durable experiment create/run/cell/cancel/resume/retry/minimize; profile comparison; per-stage waterfall; labels/disagreement; metrics/strata/regressions; reviewed aggregate report publication; shared `experiments.fixtures.promote`; and immutable retrieval-profile publish/activation through the exact §0.1 generated operations.
- Consume the shared plan 09 lab result and plan 10/17 generated client; do not add a dashboard-only evaluator, qrel store, scope parser, or replay endpoint.

## 11. Verification commands and artifacts

- `cargo test -p tracedecay-query --test retrieval_exact --test retrieval_fuzzy --test retrieval_hybrid`.
- `cargo test -p tracedecay-query --test retrieval_cutoff --test retrieval_privacy --test retrieval_ids`.
- `cargo test -p tracedecay-query --test retrieval_eval --test retrieval_agreement --test retrieval_ablation`.
- `cargo test -p tracedecay-query --test retrieval_eval_load` — fixed-seed current/10× generation is byte-identical, all linked identities resolve uniquely, and record-derived count/distribution assertions match.
- `cargo bench -p tracedecay-query --bench retrieval_pipeline` on current and 10x manifest-derived corpora.
- Search/API/frontend E2E for native expansion, explanation, no-answer, partial/locked shard, label command, lab no-write, and deterministic aggregate export.

Every report records commit, corpus/qrel/cutoff, query/retrieval profile, tokenizer/index/model/ranker/graph versions, source watermarks, privacy mode, hardware, cold/warm state, and complete per-stratum metrics.

## 12. Definition of done

- Current exact search remains available, explainable, and non-regressed.
- Real local cross-project queries are frozen, time-safe, privately judged, stratified, and reproducible by stable anchors.
- Small semantic/provider goldens prove behavior without masquerading as load coverage; the deterministic load/eval generator has unique linked identities and record-derived cardinality/distribution assertions, and contributes no synthetic rows to promotion minimums.
- The default profile beats lexical baselines on predeclared precision/usefulness metrics without hiding worst-stratum, no-answer, privacy, latency, or resource regressions.
- Duplicate child prompts, protocol/tool/query echoes, wrong-project/time results, and stale summaries have explicit metrics and inspected regressions.
- Embeddings, learned sparse, graph expansion, late interaction, expansion, recency, and reranking remain separately ablatable/removable.
- Search Quality Lab explains one query and evaluates a corpus without mutating production search/usage counters.
- Only redacted/synthetic fixtures and aggregate reports are committed; private messages/qrels remain local and protected.
