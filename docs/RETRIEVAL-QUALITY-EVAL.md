# Holographic Memory — Retrieval & Entity Quality Evaluation

Task `t_3d566784` ("Evaluate retrieval and entity quality"). This document maps the
current recall pipeline, then reports **empirically measured** quality risks gathered by
driving the real `tracedecay` binary (`target/debug/tracedecay tool fact_store`) against a
throwaway fixture project, and proposes concrete follow-ups.

All evidence below is reproducible: every query/probe/reason result was produced by the
actual shipped binary against a freshly-initialized temp project (temp `HOME`, temp project
dir — the shared repo `.tracedecay/` was never touched). Per-signal scores (`fts_score`,
`jaccard_score`, `holographic_score`, `trust_score`) come straight from the `why` field the
search handler emits.

---

## 1. Current retrieval pipeline (with code references)

`FactRetriever::search` (`src/memory/retrieval.rs:36`) fuses four recall channels and one
ranking function:

1. **Candidate generation** — three channels are unioned:
   - FTS5 BM25 (`fts_candidates`, `retrieval.rs:328-398`). Score is `1/(1+|bm25|)`
     (`retrieval.rs:391-395`); query is an OR of quoted tokens (`build_fts_query`,
     `retrieval.rs:608-620`). FTS table uses the **default `unicode61` tokenizer with no
     stemmer** (`src/db/migrations.rs:1136-1139`).
   - Entity match (`entity_candidates`, `retrieval.rs:400-483`): a fact is a candidate if any
     extracted entity exactly equals, or `LIKE %term%`-matches, the whole normalized query or
     any query token (`retrieval.rs:428-429`).
   - Trust-bucketed baseline scan (`MemoryStore::list_facts`, limit·10).

2. **Recall gate** — if the query has tokens, a candidate survives only if it is in the FTS
   set **or** shares ≥1 token (len≥2) with the fact's content+tags+entities
   (`token_overlap > 0`, `retrieval.rs:81-87`). *No stopword list* is applied to these tokens.

3. **Per-candidate scoring** (`retrieval.rs:102-128`) computes four signals:
   - `fts_score` (from step 1),
   - `jaccard_score` — token-set Jaccard vs query (`retrieval.rs:675-688`),
   - `holographic_score` — FHRR-2048 binding similarity (`holographic_score_with`,
     `retrieval.rs:590-605`),
   - `trust_score` (stored) and `temporal_decay` (`retrieval.rs:712-719`).

4. **Fusion** (`combined_score`, `retrieval.rs:698-710`):
   ```
   relevance = 0.40·fts + 0.30·jaccard + 0.30·holographic   (weights retrieval.rs:19-21)
   score     = relevance · trust · temporal_decay
   ```
   Sorted by score, tie-broken by `updated_at` (`retrieval.rs:130-136`).

5. **Entity-graph modes** bypass fusion entirely:
   - `probe` / `reason` (`retrieval.rs:149-283`) return fact ids from entity joins and score
     them with `results_for_fact_ids` (`retrieval.rs:557-584`), which sets
     `score = trust_score`, `holographic_score = 1.0`, `fts/jaccard = 0.0` — i.e. **ranked by
     trust alone, no query-relevance signal**.

### Entity extraction (`src/memory/entities.rs`)
`extract_entities` (`entities.rs:13-39`) captures five shapes:
- quoted spans (`"..."` / `'...'`),
- `aka`/`also known as` aliases (`entities.rs:64-105`),
- code tokens that look like file paths, `::` Rust symbols, or `tracedecay_`/`tokensave_` tool
  names (`entities.rs:107-119, 217-238`),
- **multi-word capitalized sequences** — requires **≥2 consecutive capitalized words**
  (`extract_capitalized_names` / `push_capitalized_sequence`, `entities.rs:121-151`),
- minus a leading-verb exclusion list matched **by exact string**
  (`is_non_entity_leading_word`, `entities.rs:153-171`).

The `memory_entities` table carries **no score, weight, frequency, or salience column**
(`migrations.rs:1072-1079`) — entities are binary matchers used only for candidate
generation, never scored into the ranking.

### Holographic encoding (`src/memory/encoding.rs`)
`encode_fact` (`encoding.rs:36-66`) binds content + each entity under fixed roles and
averages. Atoms are **deterministic SHA-256 hashes of the literal token text**
(`deterministic_coefficients`, `encoding.rs:85-110`) — there is **no semantic model**, so
"install" and "installing" are as unrelated as "install" and "xylophone".

---

## 2. Measured quality risks (with evidence)

Fixture facts (8, controlled `trust`, category), with the entities the extractor actually
produced:

| # | content | trust | extracted entities |
|---|---------|-------|--------------------|
| F1 | Acme Corp uses Postgres for its primary database | 0.5 | `["Acme Corp"]` |
| F2 | The backend standardized on Postgres in 2023 | 0.5 | `[]` |
| F3 | Prefers Tokio for async runtime | 0.5 | `["Prefers Tokio"]` |
| F4 | Use pnpm for installing dependencies in this monorepo | 0.9 | `[]` |
| F5 | Use npm to install packages | 0.3 | `[]` |
| F6 | Always back up the database nightly before deploy | 1.0 | `[]` (aged 730 days) |
| F7 | Database backups run via pg_dump every night | 0.3 | `[]` |
| F8 | The deploy runs on Kubernetes with three replicas | 0.5 | `[]` |

### RISK A — Entity extraction is brittle; the entity-graph retrieval modes are largely blind to it
- The genuine entity in F3 is **Tokio**, but the extractor produced the noisy phrase
  **`"Prefers Tokio"`** because the leading-verb filter matches `Prefer` exactly but not
  `Prefers` (`entities.rs:153-171`). Result: `probe("Tokio")` → **EMPTY**; only the bogus
  `probe("Prefers Tokio")` → `[F3]` returns the fact. No user/agent would query that phrase.
- Single-token proper nouns (Postgres, Tokio, Kubernetes, npm, database) are **never**
  extracted (the ≥2-capitalized-word rule, `entities.rs:147`, drops them), so `probe`/`reason`
  cannot reach them. `reason(["database"])` → **EMPTY** against the 8-fact corpus, then
  returned a fact only after `database` was supplied as an **explicit** entity.
- `reason` is a strict entity-join with `HAVING COUNT(DISTINCT entity) = N`
  (`retrieval.rs:223-267`); it returns nothing unless every query concept was captured as an
  entity at write time. For a corpus where most facts have zero extracted entities, the entire
  multi-entity reasoning path is inert.

### RISK B — No stemming/morphology across FTS, Jaccard, and holographic atoms
- Query `database backup` vs facts F6 ("back **up** the **database**") and F7 ("database
  **backups**"): the token `backup` matches **neither** `backups` nor `back up`. All three
  candidate facts tied at `fts_score = 0.699` (only `database` matched). Query `install
  dependencies` could not prefer F4 ("**installing** dependencies") over F5 ("**install**
  packages") on the morphologically-related word.
- Root cause is shared by all three signals: FTS5 default tokenizer (`migrations.rs:1136`),
  the `tokenize`/`tokenize_text` helpers (`retrieval.rs:622`, `encoding.rs:112`), and the
  SHA-256 atom keys (`encoding.rs:31,85`) all key on the exact surface string.

### RISK C — Trust is a hard multiplicative gate that buries relevant low-trust facts
Final score is `relevance · trust · decay`. In `search("database backup")`:

| rank | fact | relevance* | trust | decay | **score** |
|------|------|-----------|-------|-------|-----------|
| 1 | F1 Acme/Postgres (**off-topic**) | 0.4815 | 0.50 | 1.00 | **0.2408** |
| 2 | F7 Database backups/pg_dump (**on-topic**) | 0.4935 | 0.30 | 1.00 | 0.1480 |
| 3 | F6 back up the database nightly (**on-topic**) | 0.4881 | 1.00 | 0.25 | 0.1221 |

\* recomputed from the binary's own per-signal scores: `0.40·fts + 0.30·jaccard + 0.30·holo`
(F1: 0.40·0.699+0.30·0.111+0.30·0.562=0.4815; F7: 0.4935; F6: 0.4881) — these reproduce the
emitted `score` exactly, confirming **relevance was actually F7 ≈ F6 > F1**, yet the
off-topic F1 ranked first purely on `trust × decay`. A fact needs `trust ≥ 0.62` to match a
`trust = 0.3` peer of equal relevance — the default floor is 0.3 (`DEFAULT_MIN_TRUST`,
`trust.rs:10`) and new facts start at 0.5 (`DEFAULT_TRUST`, `trust.rs:9`), so freshly-learned
facts are systematically disadvantaged against entrenched medium-trust ones.

### RISK D — The holographic signal is decorative
Two compounding effects make the 0.30 holographic weight nearly meaningless for ordering:
1. **Floor at 0.5**: `holographic_score_with` returns `midpoint(sim, 1.0).clamp(0,1)`
   (`retrieval.rs:604`) = `(sim+1)/2`, so even unrelated content scores ≈0.5 and a perfect
   match scores 1.0 — the usable band is [0.5, 1.0].
2. **Recall gate already filters lexically**: candidates only reach scoring if they share a
   token with the query (`retrieval.rs:81-87`), so holographic only re-orders an
   already-lexically-tied set. Across the `database backup` candidates, holographic ranged
   0.562–0.588 (weighted swing ≈ 0.008) — too small to change order against a 0.2 trust gap.
The unrelated query `butterfly migration` correctly returned nothing — but only because the
lexical gate rejected it; the holographic channel alone would have surfaced everything at ~0.5.

### RISK E — `probe`/`reason` ignore query relevance
`results_for_fact_ids` (`retrieval.rs:557-584`) sets `holographic_score = 1.0` and
`score = trust_score`. Any fact sharing an entity with the probe is returned ordered by trust
alone — a high-trust fact that merely co-occurs with the entity outranks the directly
on-topic one. These modes also do not apply the temporal or relevance signals, so stale
high-trust facts dominate.

### RISK F — Stale entities / supersession are report-only
Adding `Use pnpm instead of npm` after `Use npm to install packages` correctly emitted
`diff: possible_conflict` (similarity 0.9997, `diff.rs:86-100`) — but **both facts coexist**
and both remain reachable; nothing is deprecated, merged, or re-ranked. There is no
entity-level `updated_at`/salience and no supersession flag on `memory_facts`
(`migrations.rs:1050-1070`), so `probe`/`reason` entity joins return the superseded fact
indefinitely. The aged F6 (730 days, `temporal_decay` ≈ 0.25) still surfaced in `deploy` and
`database backup` results.

### RISK G — Entity candidate broadening is noisy and unnormalized
`entity_candidates` LIKE-matches the **whole normalized query string** and each raw query
token (no stopword removal) against `normalized_name` (`retrieval.rs:409-432`). Common query
words (`use`, `for`, `the`) become LIKE patterns that can match unrelated entity substrings,
and the whole-query match favors coincidental exact-string entities over conceptual ones. The
added candidates are then scored only through the lexical/holographic channels, which (per
Risk B/D) add little discrimination.

---

## 3. Proposed follow-ups (≥3, prioritized)

### F1 — Add a retrieval-ranking eval scenario family (highest value, lowest risk)
The existing `tests/memory_eval_test.rs` + `eval/scenarios/*.json` harness covers **hygiene
contracts only** (secrets, transient, supersession, dedup). It has **no ranking-quality
scenarios**. Add scenarios that assert ordering, e.g. `memory-ranking-trust-bias.json`:
seed an on-topic low-trust fact and an off-topic high-trust fact sharing one token, assert
`search()` ranks the on-topic fact first. Extend the harness with a ranking assertion kind
(`Assertion::SearchRank { query, top_fact_source, min_rank_gap }`) that calls
`tracedecay tool fact_store --args {action:search}` and checks the returned order — this is
the same subprocess path the harness already uses. This pins Risks A–E as regression tests.

### F2 — Re-balance fusion: gate trust, don't multiply it
Replace the hard `relevance · trust` product (`retrieval.rs:709`) with a trust **gate +
gentle nudge**, e.g. `score = relevance · (0.5 + 0.5·trust) · decay` (or min-trust filter
then mild boost). This keeps low-trust exclusion (via `DEFAULT_MIN_TRUST`) while stopping
trust from burying equally-relevant fresher facts. Pair with normalizing the FTS score to a
stable [0,1] range (the current `1/(1+|bm25|)` is collection-size-dependent). Add unit tests
on `combined_score` (`retrieval.rs:698`) covering the trust-bias case from Risk C.

### F3 — Fix entity extraction coverage and the brittle verb list
- Extend `extract_capitalized_names` to also capture **single high-salience capitalized
  tokens** (e.g. recognized tech/project names), or lower the ≥2-word threshold for
  capitalized tokens that are not sentence-initial — so Postgres/Tokio/Kubernetes become
  entities.
- Make `is_non_entity_leading_word` (`entities.rs:153`) stem-/prefix-match (accept `prefer`,
  `prefers`, `preferred`, `preferring`, `using`, `uses`) so phrase capture like
  `"Prefers Tokio"` does not swallow the real entity.
- Extract the **head noun** of a captured phrase as an additional entity (so `"Prefers
  Tokio"` also yields `"Tokio"`). Add unit tests in `entities.rs` for each.

### F4 — Make the holographic signal earn its weight
Either (a) remove the `(sim+1)/2` floor (`retrieval.rs:604`) and let raw FHRR similarity
(≈[-1,1], rescaled to [0,1] without midpoint compression) discriminate, or (b) if the floor
is needed to avoid negatives, drop the holographic weight from 0.30 toward 0.10–0.15 until a
real embedding model replaces the SHA-256 atom keys (which currently make the channel a
deterministic lexical hash, not a semantic signal — see `encoding.rs:85`). Add a test that
two semantically-similar-but-lexically-different facts score higher than two
lexically-similar-but-semantically-unrelated ones; until that passes, the channel is
decorative (Risk D).

### F5 — Add morphology to the lexical channels
Configure FTS5 with the `porter` stemmer (`tokenize='porter'` or a unicode61+stemmer config,
`migrations.rs:1136`) and apply the same stemming in `tokenize`/`tokenize_text`
(`retrieval.rs:622`, `encoding.rs:112`) so `install`/`installing`/`installs` and
`backup`/`backups`/`back up` collapse. Risk B. Guard with tests.

### F6 — Persist supersession so entity joins can skip stale facts
When `add_fact` classifies `PossibleConflict`, offer to mark the older fact superseded (a
`superseded_by`/`deprecated` flag on `memory_facts`) and have `probe`/`reason`/`search`
exclude or down-rank superseded facts. Today the signal is purely advisory (Risk F).

---

## 4. Reproducing this evaluation

The evidence above was produced with the shipped binary against an ephemeral fixture (no
cargo, no repo-DB mutation). Minimal reproduction:

```bash
BIN=target/debug/tracedecay
HOME=$(mktemp -d) PROJ=$(mktemp -d); mkdir -p $PROJ/src
echo 'pub fn m(){}' > $PROJ/src/lib.rs
cd $PROJ
HOME=$HOME TRACEDECAY_GLOBAL_DB=$HOME/.tracedecay/global.db $BIN init
HOME=$HOME TRACEDECAY_GLOBAL_DB=$HOME/.tracedecay/global.db $BIN tool fact_store --args \
  '{"action":"add","content":"Database backups run via pg_dump every night","category":"project","trust":0.3}'
HOME=$HOME TRACEDECAY_GLOBAL_DB=$HOME/.tracedecay/global.db $BIN tool fact_store --args \
  '{"action":"add","content":"Acme Corp uses Postgres for its primary database","category":"general","trust":0.5}'
HOME=$HOME TRACEDECAY_GLOBAL_DB=$HOME/.tracedecay/global.db $BIN tool fact_store --args \
  '{"action":"search","query":"database backup","limit":5}'
# observe the off-topic Postgres fact (trust 0.5) outrank the on-topic backups fact (trust 0.3)
```
