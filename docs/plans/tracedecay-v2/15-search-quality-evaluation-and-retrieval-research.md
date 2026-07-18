# TraceDecay V2 Search Quality

## Status / Role

Status: active product plan.

Role: PR9 ships the lexical retrieval baseline and measured evaluation harness. PR10
ships the native semantic channel and reranker. Semantic implementation is required in
PR10; runtime use remains configurable.

## Outcome

Search returns useful, correctly scoped evidence on the first page across real local
projects while preserving exact technical lookup, privacy, explanations, predictable
latency, and calibrated abstention.

## Owns

- Search-quality behavior, real-project evaluation cases, labels, metrics, and promotion
  decisions for lexical and semantic retrieval.
- Exact/phrase/BM25, typo tolerance, copy/echo control, deterministic fusion, diversity,
  bounded graph contribution, semantic recall, and bounded reranking quality.
- One concise reproducible quality and resource report for each promoted profile.

## Does not own

- Stable anchor identity, storage authority, project/worktree scope resolution, capture,
  projection, privacy policy, public transport routes, or UI rendering.
- A second query engine, corpus database, experiment platform, benchmark service, model
  downloader daemon, or plan-enforcement workflow.
- Public benchmark claims as substitutes for TraceDecay measurements.

## Required behavior

1. PR9 preserves exact IDs, quoted phrases, error text, paths, symbols, tool names, and
   config keys in a non-demotable tier before fuzzy or semantic ranking.
2. PR9 provides fielded BM25 over typed result grains, character-level typo recovery,
   query/tool/protocol echo penalties, representative-copy clustering, bounded diversity,
   deterministic pagination, and concise rank explanations.
3. Before tuning, the concise report freezes the supported decision, target
   population and unit, inclusion/exclusion/horizon, baseline and candidate
   revisions, outcome/oracle, primary metric, hard guardrails, named strata and
   support floors, uncertainty method, practical margin, and stopping rule. It
   appends immutable result anchors; it does not create a benchmark service,
   leaderboard, plan compiler, scheduler, or separate evaluation database.
4. Evaluation separates development, locked chronological and
   repository-disjoint evaluation, and forward confirmation. It records
   contamination and prior-access risk; near duplicates, forks, generated
   copies, and repeated issue families cannot cross partitions unnoticed. Real
   sanitized query episodes span multiple local projects, providers, exact
   queries, paraphrases, typos, temporal cases, wrong-scope near matches, hard
   negatives, and expected no-result cases.
5. Labels distinguish relevance, duplicate/echo noise, wrong scope, stale or superseded
   evidence, privacy eligibility, and correct abstention. Corrections append or supersede;
   they do not rewrite an already reported run.
6. Reports include Precision@1/3/5, Recall@5/10, MRR, nDCG@10, first-useful rank,
   no-answer precision, duplicate rate, wrong-scope rate, p50/p95 latency, peak RSS,
   index size, and incremental rebuild cost by meaningful stratum. Candidate
   retrieval reports oracle Recall@N before reranker metrics, channel ablations
   use equal candidate budgets, and abstention reports risk/coverage and AURC.
   Raw similarity, logits, score margins, and fused scores are not confidence.
7. Retrieval gates include per-query-family, language, repository,
   exact/no-answer/wrong-scope, stale, privacy, and low-coverage strata with
   intervals. Exact flat-vector scan is a production candidate and the quality
   oracle for any measured ANN candidate. Human or fixture labels remain
   promotion authority; LLM judges are secondary diagnostics only.
8. PR10 implements native in-process FastEmbed search with no Python, WASM, llama.cpp,
   external inference process, or separate model service. Disabled semantic configuration
   remains a fully supported lexical mode, but does not excuse missing PR10 implementation.
9. PR10 treats the currently supported Jina code model, one general FastEmbed
   comparator, and `BGERerankerV2M3` as reproducible candidates rather than
   predetermined winners. One current compatible code-specialized challenger
   may enter with pinned license/runtime evidence. Selection follows the locked
   TraceDecay report; public leaderboard rank cannot promote a model.
10. Models load once and reuse sessions. Document embeddings batch during indexing;
   unchanged documents reuse compatible vectors. Stored vectors record model, revision,
   dimensions, normalization, chunking, and schema version.
11. Model absence, corruption, incompatibility, refusal, timeout, or budget failure returns
   the declared lexical/pre-rerank order with a visible reason. It never substitutes an
   unmeasured model or crosses a privacy domain.
12. Configuration and dashboard controls expose lexical-only, semantic, and reranking
    modes plus resource limits. Activation changes are versioned and running queries stay
    pinned to their starting profile.
13. A semantic profile is promoted only when the locked real-project test improves the
    declared quality frontier without material exact-match, no-answer, wrong-scope,
    privacy, worst-stratum, latency, or memory regression.
14. Every code quantifier descriptor used by retrieval evaluation has a compact
    metric card: construct, formula, units, intended decision, confounds,
    supported languages, coverage, datasets, uncertainty, and prohibited
    interpretations. Extraction, clone, architecture, defect/rework, and
    test-impact claims use separate validation sets. Predictive claims require
    chronological and project holdouts, LOC/churn/prior-defect baselines,
    calibration, abstention, and worst-stratum results. Aggregate correlation
    never promotes a universal quality truth or a causal defect-reduction
    claim.

## Acceptance

- PR9 lexical tests cover exact technical strings, typo recovery, copies/echoes, stale
  evidence, wrong project/time, pagination, explanations, and no-result behavior.
- The same frozen queries compare production baseline, PR9 lexical, PR10 embedding, and
  PR10 reranked profiles with channel ablations.
- Candidate Recall@N precedes reranker evaluation; exact flat scan remains the
  oracle when ANN is evaluated, and protected exact/no-answer/wrong-scope,
  privacy, minority-language, calibration, and tail strata cannot be hidden by
  an aggregate score.
- PR10 tests model lifecycle, offline reuse after initial installation, batching,
  incremental reuse, vector incompatibility/rebuild, bounded reranking, cancellation,
  configuration, and truthful fallback.
- Only sanitized fixtures and aggregate reports enter Git; private query text and source
  payloads remain in their authorized stores.
- The report is reproducible from one documented command and contains enough raw aggregate
  evidence to verify the selected quality/resource frontier without a benchmark bureaucracy.
- No public leaderboard, universal rollout count, mandatory ANN algorithm, or
  uncalibrated score is a promotion gate.
