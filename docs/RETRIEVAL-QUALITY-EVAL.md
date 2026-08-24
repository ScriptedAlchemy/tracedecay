# Holographic Memory — Retrieval & Entity Quality Evaluation

This document maps the current project-memory recall pipeline (candidate
generation, scoring, fusion) and the entity extractor that feeds it, then
records which of the previously-identified quality risks still hold against
the current implementation.

The retrieval pipeline described here lives in
`crates/tracedecay-runtime-core/src/store/memory/{candidates.rs,scoring.rs,search.rs}`
— async, transaction-scoped, `project_memory_*`-prefixed functions operating
over the `memory_v2_*` SQLite tables
(`crates/tracedecay-runtime-core/src/db/memory_v2/schema/`). This is a full
rewrite of an earlier single-crate `src/memory/{retrieval.rs,store.rs}`
pipeline; none of that module layout exists anymore, and this document no
longer cites it.

---

## 1. Current retrieval pipeline

### Candidate generation

`project_memory_search_candidates_tx` (in `candidates.rs`) unions three SQL
channels, each capped at `SEARCH_CANDIDATE_ARM_LIMIT` (1,000) rows:

1. **FTS5 BM25** — `project_memory_fts_candidates_tx` queries the
   `memory_v2_assertion_payloads_fts` virtual table (SQLite FTS5, default
   `unicode61` tokenizer, **no stemmer configured**) joined through
   `memory_v2_current_facts` / `memory_v2_facts` / `memory_v2_assertion_payloads`,
   filtered by owner, trust floor, and optional category. The FTS index only
   covers fact **content** (not tags or entities).

   The query text is built by `project_memory_fts_query`: every query token is
   quoted, and — **not documented in earlier versions of this doc** — any
   token with 4 or more characters gets a trailing `*`, turning it into an
   FTS5 prefix query; all tokens are OR'd together. A query token like
   `install` therefore prefix-matches `installing`/`installs` in the index,
   even though FTS5 itself is not stemming.

2. **Entity match** — `project_memory_entity_candidates_tx` tokenizes the
   query text and also adds the whole normalized/lowercased query string as
   one more term, then matches each term against every fact's `$.entities`
   JSON array (extracted via `json_each`) with either an exact
   case-insensitive match or a `LIKE '%term%'` broadening (escaped for SQL
   wildcards). No stopword list is applied to these terms.

3. **Newest-facts baseline** — `project_memory_newest_candidates_tx`: every
   trust/category-eligible fact, ordered by `updated_at DESC`, capped at the
   arm limit. This ensures a query with no lexical/entity match at all still
   has a bounded pool of recent facts to rank (they will usually fail the
   recall gate below unless the query has no tokens).

4. **Graph-assist expansion** (new since the rewrite, not present in the old
   pipeline) — `project_memory_graph_assist` in `search.rs`: when the memory
   graph runtime is mounted, it expands the FTS/entity candidate roots with
   facts related through the code/entity graph
   (`super::graph::project_memory_graph`), and reports coverage as
   `ProjectMemoryFactSearchGraphCoverageV1::{Complete, Degraded, NotMounted}`
   on the result page. If the graph is unavailable, missing, or over budget,
   search degrades gracefully to the SQL-only candidate set rather than
   failing.

### Recall gate

Ranking (`project_memory_rank_facts_tx` in `search.rs`, `Search` arm) drops a
candidate if the query has tokens and it scored **zero on both** the FTS
component and the Jaccard component — i.e. it needs at least a partial BM25
match (with coverage) or a shared token to survive. There is still no
stopword list applied here.

### Per-candidate scoring

`project_memory_search_scores` (in `search.rs`, backed by `scoring.rs`)
computes:

- **`fts`** = `project_memory_fts_component(normalized_bm25, coverage)` =
  `normalized_bm25 * (0.5 + 0.5 * coverage)`.
  - `normalized_bm25` comes from `project_memory_normalize_fts5_ranks`:
    SQLite's `bm25()` rank (negative; lower is a better match) is negated and
    floored at 0, then divided by the **maximum** relevance across the
    candidate set for that query — a max-relative normalization. This
    replaces the collection-size-dependent `1/(1+|bm25|)` formula from the
    pre-rewrite pipeline.
  - `coverage` = `project_memory_term_coverage`: the fraction of query tokens
    present in the fact's token set, where a match is either exact or (for
    query tokens of 4+ characters) a prefix match against a fact token — the
    same prefix-tolerance the FTS query itself uses.
- **`jaccard`** = `project_memory_jaccard`, the exact Jaccard similarity
  between `project_memory_tokens(query)` and `project_memory_fact_tokens(fact)`
  (fact tokens are content + tags + entities, tokenized identically).
- **`holographic`** = `project_memory_holographic_score`: FHRR-2048 binding of
  content + normalized entities via `HolographicEncoder`
  (`crates/tracedecay-runtime-core/src/memory/encoding.rs`, unchanged in
  spirit from the pre-rewrite code) — every token still becomes a
  deterministic SHA-256-derived coefficient vector; there is no semantic
  embedding model. The raw FHRR similarity is rescaled with
  `midpoint(sim, 1.0)` (i.e. `(sim + 1) / 2`), so the usable band is still
  `[0, 1]` with an unrelated-content floor near 0.5.
- **`trust`** = the fact's persisted `trust_score` (a `Confidence` in
  `[0, 1]`; see the companion `TRUST-DECAY-SEMANTICS.md` for how it changes).
- **`temporal_decay`** = `project_memory_temporal_decay` — unchanged formula
  (see the trust-decay doc): `0.5^(age_days / 365)`, clamped to `[0.10, 1.0]`.
- **`retrieval_count`** = the fact's stored retrieval counter, feeding the new
  usage-boost term below.

### Fusion

`project_memory_combined_score` (in `scoring.rs`):

```text
relevance   = 0.40 * fts + 0.30 * jaccard + 0.30 * holographic
usage_boost = 1.0 + min(0.02 * ln(1 + retrieval_count), 0.50)
score       = relevance * trust * temporal_decay * usage_boost
```

The `0.40 / 0.30 / 0.30` fusion weights are unchanged from the pre-rewrite
pipeline. **`usage_boost` is new and was not described in earlier versions of
this document**: the more often a fact has previously been retrieved
(`retrieval_count`, incremented on every successful search hit — see
`project_memory_update_retrieval_projection_tx`), the more its score is
nudged upward, on a `ln(1 + n)` curve capped so the multiplier never exceeds
`1.5×` (`RETRIEVAL_REINFORCEMENT_CAP = 0.50`, weight
`RETRIEVAL_REINFORCEMENT_WEIGHT = 0.02`). In practice the cap is reached only
at an unrealistically large retrieval count; for ordinary usage the boost is
a small, smoothly-increasing nudge, not a hard step.

The final score is stored as millionths and clamped at
`MAX_PROJECT_MEMORY_SEARCH_SCORE_MILLIONTHS = 1_500_000` (i.e. 1.5), which is
exactly the ceiling the formula can reach (`relevance = 1, trust = 1,
decay = 1, usage_boost = 1.5`).

Hits are ordered by score descending, tie-broken by `updated_at` then fact id
(`rank_and_seek`), with a resumable cursor
(`ProjectMemoryFactSearchCursorV1`) for pagination. Each hit also carries a
human-readable `why` string with every raw component
(`fts=…, coverage=…, jaccard=…, holographic=…, trust=…, temporal_decay=…,
retrieval_count=…`).

### Entity-graph modes bypass fusion entirely

`Probe`, `Related`, and `Reason` queries resolve candidate fact ids from
`project_memory_exact_entity_candidates_tx` (an **exact** normalized-entity
match only — no `LIKE` broadening in these modes) and then score every
surviving fact as `score = trust`, `holographic = 1.0`, `fts = jaccard = 0`.
These modes still carry **no query-relevance signal** — a fact is ranked by
trust alone once it matches the requested entity/entities, same as the
pre-rewrite pipeline.

## 2. Entity extraction (`crates/tracedecay-runtime-core/src/memory/entities.rs`)

`extract_entities` combines five sources, in document order, de-duplicated
case-insensitively:

- quoted spans (`"…"` / `'…'`, with apostrophe-as-delimiter suppressed
  between two alphanumeric characters so possessives/contractions don't pair
  into a giant span),
- `aka` / `a.k.a.` / `also known as` aliases,
- code-like tokens: file paths, `::`-qualified Rust symbols, `tracedecay_*`
  tool names, and `snake_case`/`camelCase` identifiers,
- multi-word capitalized phrases, and
- **single-token capitalized proper nouns** (e.g. `Postgres`, `Tokio`,
  `Kubernetes`, `Database`), filtered by a sentence-function-word stoplist
  (`is_common_sentence_word`: articles, pronouns, auxiliaries, etc.).

Every candidate is additionally shape-checked (`is_valid_entity`): at most 80
characters, at most 6 words, and — for anything that isn't a single trusted
code token — every word must be a clean identifier-ish token (no stray
sentence punctuation).

### Risk A — resolved

An earlier version of this document flagged entity extraction as brittle
because (1) the leading-verb exclusion list matched only exact base forms
(`Prefer`, not `Prefers`), so `"Prefers Tokio for async runtime"` was captured
verbatim as the noisy entity `"Prefers Tokio"` instead of `"Tokio"`, and (2)
single capitalized tokens were never extracted at all (only ≥2-word
capitalized sequences), so `probe("Tokio")` / `probe("Postgres")` could never
reach a fact that only mentioned the bare noun.

Both are fixed in the current `entities.rs`:

- `is_non_entity_leading_word` now matches every common inflection of the
  ~14 leading verbs (`add/adds/added/adding`, `prefer/prefers/preferred/
  preferring`, `use/uses/used/using`, etc.), case-insensitively, not just the
  base form. `"Prefers Tokio"` now strips the leading verb and yields
  `"Tokio"` (plus the remainder phrase `"Prefers Tokio"` itself no longer
  survives verbatim).
- `push_capitalized_sequence` / `push_single_capitalized` now also emit a
  bare single capitalized token as its own entity (still filtered by
  `is_common_sentence_word`), so `Postgres`, `Tokio`, `Kubernetes`, and
  `Database` are all extractable as entities in their own right, not only as
  part of a ≥2-word phrase.

Both fixes are covered by unit tests in `entities.rs` itself — notably
`leading_verbs_match_across_inflections`,
`leading_verb_no_longer_swallows_following_entity`,
`single_capitalized_proper_nouns_are_extracted`, and
`verb_led_multiword_phrase_exposes_head_noun` — and the code comments
explicitly cite "Risk A" as the motivation for the change.

## 3. Remaining considerations

The rest of the original risk list still describes real architectural
properties of the current pipeline, verified against the source above. The
**specific fixture measurements** from the original evaluation run (exact
score tables against an 8-fact corpus) are not reproduced here: the scoring
formula changed enough — coverage-weighted and max-normalized BM25, plus the
new `usage_boost` term — that those old numbers would not replay verbatim
against the current code. Re-run the reproduction steps in §4 with the
current binary before citing exact scores again.

- **No stemming/morphology across FTS, Jaccard, and holographic atoms.**
  FTS5 still uses the default `unicode61` tokenizer with no stemmer;
  `project_memory_tokens` / the holographic tokenizer still key on the exact
  lowercased surface string. The new prefix-wildcarding and coverage-prefix
  matching (§1) now cover the common case of one token being a literal prefix
  of another (`install` → `installing`), but not compound/word-order
  differences (`backup` vs. `back up`) or non-suffix inflection.
- **Trust remains a hard multiplicative gate.** `score = relevance * trust *
  temporal_decay * usage_boost` — a low-trust, on-topic fact still needs a
  proportionally larger relevance or usage-count advantage to outrank a
  higher-trust, less-relevant peer.
- **The holographic signal is still a lexical hash, not a semantic
  embedding.** Atoms are still deterministic SHA-256 hashes of literal token
  text (`crates/tracedecay-runtime-core/src/memory/encoding.rs`), with the
  same `(sim + 1) / 2` floor. It remains a narrow re-ranking signal on top of
  an already lexically-filtered candidate set, not an independent semantic
  channel.
- **`Probe` / `Related` / `Reason` still ignore query relevance** and rank
  purely by trust once a fact matches the requested entity/entities (§1).
- **Cross-fact duplicate/supersession detection is still separate from
  retrieval.** `add_fact`'s conflict classification
  (`store/memory/crud/add.rs`) still only reports a signal
  (e.g. `possible_conflict`) at write time; nothing marks an older fact
  superseded automatically, and `search`/`probe`/`reason` do not consult
  supersession state. A dedicated memory-curation review/apply flow now
  exists (`store/memory/curation/{review.rs,apply.rs}`) that can act on
  near-duplicate/contradiction signals, but it is a distinct
  operator-or-automation step, not something the retrieval path applies
  implicitly.
- **Entity-candidate broadening is still unnormalized.**
  `project_memory_entity_candidates_tx` still `LIKE`-matches every raw query
  token (no stopword removal) against each fact's entity list, so common
  words can still match unrelated entity substrings.

## 4. Proposed follow-ups

### Add a retrieval-ranking eval scenario family
Cover the trust-vs-relevance tradeoff and the entity-graph modes with
regression scenarios (seed an on-topic low-trust fact and an off-topic
high-trust fact sharing one token; assert ordering) exercised through the
`fact_store_search`/`fact_store_probe`/`fact_store_reason` MCP tools (or their
CLI equivalents), the same way the existing memory eval harness already
drives the shipped binary for hygiene contracts.

### Re-balance fusion: gate trust, don't multiply it
Replace the hard `relevance * trust` product in
`project_memory_combined_score` with a trust **gate + gentle nudge** (e.g.
`relevance * (0.5 + 0.5 * trust)`), keeping the `DEFAULT_MIN_TRUST` floor as
the hard exclusion while stopping trust from burying an equally-relevant
fresher fact. Add unit tests on `project_memory_combined_score` covering that
tradeoff explicitly (the existing tests in `scoring.rs` cover the ceiling and
the shipped BM25/coverage math, but not a trust-vs-relevance ordering case).

### Make the holographic signal earn its weight
Either drop the `(sim + 1) / 2` floor in favor of a rescale that doesn't
compress unrelated content toward 0.5, or reduce the holographic weight until
a real embedding model replaces the SHA-256 atom keys in
`HolographicEncoder`. Add a test asserting two semantically-similar-but-
lexically-different facts score above two lexically-similar-but-unrelated
ones; until it passes, the channel is decorative for ordering purposes.

### Add morphology to the lexical channels
Configure `memory_v2_assertion_payloads_fts` with a stemming tokenizer (e.g.
`porter`) and apply the same stemming in `project_memory_tokens` and the
holographic tokenizer, so `install`/`installing`/`installs` and
`backup`/`backups`/`back up` collapse consistently rather than relying on the
narrower prefix-wildcard heuristic.

### Wire supersession into retrieval
When the memory-curation flow marks a fact superseded/merged, have
`search`/`probe`/`reason`/`related` exclude or down-rank the superseded fact
by default, so a stale duplicate stops surfacing indefinitely once curation
has acted on it.

## 5. Reproducing this evaluation

The tool surface changed along with the retrieval rewrite: memory operations
are now separate MCP tools (`fact_store_add`, `fact_store_search`,
`fact_store_probe`, `fact_store_reason`, …) rather than one `fact_store` tool
with an `action` field, and the CLI's `tracedecay tool <name> --args '<json>'`
dispatches any of them by name (short or `tracedecay_`-prefixed). A
reproduction along the same lines as before now looks like:

```bash
tracedecay tool fact_store_add --args \
  '{"content":"Database backups run via pg_dump every night","category":"project","trust":0.3}'
tracedecay tool fact_store_add --args \
  '{"content":"Acme Corp uses Postgres for its primary database","category":"general","trust":0.5}'
tracedecay tool fact_store_search --args '{"query":"database backup","limit":5}'
```

Run against a scratch project (a temporary `HOME` and an initialized project
directory, as before) so the reproduction never touches a real `.tracedecay/`
store. The exact commands above have not been re-executed as part of this
rewrite — treat them as a starting point to reproduce and re-measure the
"Remaining considerations" in §3 against the current binary, not as verified
output.
