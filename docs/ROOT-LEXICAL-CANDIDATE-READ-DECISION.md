# Decision needed: Lexical clause semantics and the candidate page

Root-scope message search now refuses at the candidate read instead of the
record read. The record-read fix (see `ROOT-DERIVED-RECORD-READ-DECISION.md`)
moved the boundary; it did not invent this one. The candidate read is refusing
because the Lexical clause proposes a population three orders of magnitude
larger than the query's answer, and because nothing downstream can tell the
answer apart from the rest of that population. Both halves of that are design
decisions about retrieval semantics, so this memo measures and states options;
it does not choose.

## Observed failure

`message_search` for `resident limit warm sessions` (limit 3) on the operator's
profile root returns `application.retained.budget-refused` after 18.5 s, naming
the boundary the typed diagnostic now carries: `CandidateReadExhausted`, limit
256, consumed 256 with more available. The daemon's Hotpath gauge
`session.retrieval.budget.candidate_read` fired once and no
`temporal_query.candidates.visible` gauge fired, so the refusal precedes
ranking.

`SemanticResourceCeilings` (limit 3) returns `tool_dispatch_deadline_exceeded`
(retryable) on three attempts across 90 minutes. That is not this defect: its
Lexical clause matches 73 occurrences, well inside the 256 page. Hotpath places
the cost outside retrieval entirely. Over a 1,978 s window,
`mcp.server.tools_call.dispatch` averaged 23.68 s across 186 calls and
`mcp.hook_runtime.total` averaged 23.60 s across 179 — so essentially the whole
tool latency is admission-side, before dispatch. Inside it,
`daemon.context_scout.lifecycle_lookup` averaged 19.80 s (p95 32.04 s) across
179 calls, and `daemon.context_scout.lookup.unresolved` counted 153 — every
lookup resolved nothing. A lookup that returns no authority should not cost 20 s
of a tool's deadline; that is its own mis-sized cost and its own investigation.
The concurrent code-index rebuild is the ambient load (`code_index.restore
.segment_decode` 14,812 calls, `global_db.observation_apply.derive.alias` 13,902
calls at 56 ms each), which is why the terminal is correctly retryable.

## Measurement

Read-only against the operator's live store
(`~/.tracedecay/projects/proj_a5b3d7e3ebe14ca7/sessions.db`, 16 GB, 1,606
sessions, 1,606 active generations, 199,503 occurrences).

### Candidates per clause for `resident limit warm sessions`

`plan_candidates` emits five clauses for this query. Only one of them proposes
anything, and it proposes almost the whole store:

| Clause | Match term | Rows available |
| --- | --- | --- |
| ExactMessage | phrase `"resident limit warm sessions"` | 0 |
| Phrase | not planned (query is unquoted) | — |
| Lexical | `"resident" OR "limit" OR "warm" OR "sessions"` | **41,064** |
| Summary | phrase, over `session_summary_nodes_fts` | 0 |
| Span | phrase, over member occurrences | 0 |
| Burst | phrase, over member occurrences | 0 |

The same four terms joined with `AND` match **2** occurrences in 2 sessions —
those two are the query's answer. The OR clause spans 1,468 of the 1,606
sessions.

Per-term document frequency explains it: `limit` appears in 33,187 occurrences
(16.6% of the store) and `sessions` in 9,826 (4.9%), against `resident` 429
(0.2%) and `warm` 1,230 (0.6%). Joining terms with OR discards the selectivity
of the rare terms and inherits the frequency of the commonest one.

The clause SQL is not the mis-sized cost. Both forms are index-driven and fast:
a recency-ordered page of 256 over the OR match takes 0.106 s; the complete AND
result takes 0.001 s. The mis-sized quantity is the *population*, 41,064 rows
proposed for a 2-row answer.

### What a page of 256 actually contains

Every candidate clause orders by recency (`ORDER BY o.knowledge_at DESC,
o.session_id, o.occurrence_id`), because that ordering is the cursor keyset.
So the page the read takes today, and refuses to hand back, is the 256 most
recent rows of the OR match:

| Page of 256 over the OR match | Rows containing all four terms | Rows containing `resident` | Rows containing `warm` | Distinct sessions |
| --- | --- | --- | --- | --- |
| recency-ordered (today's keyset) | 0 | 0 | 1 | 8 |
| bm25-ordered | 0 | 170 | 96 | 75 |

The two true hits sit at recency rank 37,718 and 39,653 of 41,064 — near the
oldest end — and at bm25 rank 290 and 5,968. The two pages share zero rows.

This is the load-bearing finding: **a bm25 page of 256 does not contain the
answer either.** bm25 over an OR match rewards a document that repeats a common
term as readily as one that covers all four, so the answer lands just outside
the page (290) and far outside it (5,968).

Nothing downstream recovers it. `candidate_score` is a per-channel constant —
every Lexical candidate scores 400 regardless of how many query terms it
matched — and no candidate row in the temporal path carries a bm25 or
term-coverage score at all. Term coverage is not represented anywhere: not in
the clause (OR flattens it), not on the candidate, not in the ranker.

### Relaxation ladder for the same query

| Terms required | Rows | Fits the 256 page |
| --- | --- | --- |
| 4 of 4 (AND) | 2 | yes, 128× over |
| any 3 of 4 | 67 | yes |
| any 2 of 4 | 3,539 | no |
| 1 of 4 (OR, today) | 41,064 | no |

### Generality

This is not one pathological query. Every multi-word query overruns the page
under OR and lands inside it under AND:

| Query | OR rows | AND rows |
| --- | --- | --- |
| `resident limit warm sessions` | 41,231 | 2 |
| `candidate read budget refusal` | 39,007 | 33 |
| `span burst membership record read` | 38,427 | 0 |
| `dashboard contracts generate` | 18,655 | 280 |
| `worktree lock cleanup` | 13,931 | 92 |
| `SemanticResourceCeilings` (one term) | 73 | 73 |

Two residual cases matter for the options below. `dashboard contracts generate`
matches 280 rows under AND, above the 256 page, so AND alone does not remove
truncation. `span burst membership record read` matches 0 under AND; relaxing
one term gives 0, 6, 5, 0, 0 depending on which is dropped, against 38,427 for
full OR — so a graduated relaxation is precise where OR is not.

The 280-row case also spans 84 distinct sessions, and the bm25 page of 256 spans
75. Both are inside `MAX_TEMPORAL_PARTICIPANTS` (256), so neither option below
moves the participant freeze; one candidate carries one session, so a page
capped at 256 candidates can never present more than 256 participants.

## Options

### (a) The candidate read truncates instead of refusing

At the page limit, take the page and mark coverage partial with a typed
omission (`candidate page truncated at 256; more available`) rather than
returning `BudgetExceeded`. A search over a large store normally has more
matches than one page; refusing is the wrong terminal for a normal condition.

- Cost: no new query cost. One page instead of a refusal.
- Coverage contract: `coverage.total()` stops meaning "the whole result-eligible
  population" and starts meaning "the population inside the admitted page",
  with the omission carrying the difference. That is a real contract change —
  the record-read slice just narrowed `result_eligible_anchors` to the anchors
  candidate channels proposed, and this widens the same counter's meaning to
  "proposed, within the page". Both the counter and the omission have to be
  read together or coverage becomes a lie.
- Measured consequence for the observed query, **taken alone**: the request
  succeeds and returns 256 rows of which **0 match the query**, labelled
  partial. An operator asking for `resident limit warm sessions` is handed the
  8 most recent sessions' chatter about "limit" and told some results were
  omitted. Ordering that page by bm25 instead changes the rows but not the
  outcome: still 0 of 256 match all four terms. This is arguably worse than
  today's refusal, because a refusal is honest about having found nothing
  usable.
- Ordering by bm25 also conflicts with the cursor keyset, which is recency. A
  relevance-ordered page needs its own continuation encoding or paging stops
  being stable across generations.

### (b) Lexical semantics: all terms first

Plan the Lexical clause as all-terms (AND) and fall back to a broader form only
when the strict form does not fill the page.

- Cost: 0.001 s versus 0.106 s for the observed query; 2 rows proposed instead
  of 41,064, so the read completes with coverage genuinely complete and the
  later clauses (Summary, Span, Burst) actually get to run. Today the Lexical
  clause consumes all 256 slots before those three clauses are reached, so a
  common token silently starves every other channel — a fact currently
  invisible in coverage.
- Coverage contract: unchanged and honest. `coverage.total()` keeps meaning the
  whole result-eligible population, because the population now fits.
- The fallback trigger as phrased ("OR when AND yields fewer than the page") is
  measurably wrong: AND yields 2 < 256 for the observed query, so the fallback
  would fire and restore all 41,064 rows. The trigger has to be **AND yields
  zero**, and the fallback should be a graduated relaxation (drop one term, then
  two) rather than a jump to full OR: 2 → 67 → 3,539 → 41,064 for this query,
  and 0 → ≤6 for `span burst membership record read`.
- Recall risk: a term the user misspelled or a synonym now excludes the whole
  document instead of contributing a weak match. That is the honest trade, and
  the graduated fallback bounds it.

### (c) Both

(b) sizes the population; (a) guarantees the read never refuses for the residual
case where even the strict population exceeds the page (`dashboard contracts
generate`, 280 rows).

- Cost: the union of the two, which is (b)'s query saving plus (a)'s omission
  plumbing.
- Coverage contract: the (a) change, but now it fires on genuinely large
  *relevant* result sets rather than on every multi-word query. The partial
  label becomes rare and meaningful instead of universal.

### (d) Raise `candidate_limit` — rejected

41,064 candidates for a 2-row answer is not a budget that is too small; it is a
population that is wrong by four orders of magnitude. Raising the ceiling to
admit them would also raise the participant freeze past
`MAX_TEMPORAL_PARTICIPANTS` (1,468 sessions touched), multiply hydration, and
still rank the answer below 41,062 irrelevant rows because no term-coverage
signal exists. Rejected.

## Lean

**(c), with (b) as the correctness fix and (a) as the truthfulness fix, and (b)
landing first.**

(b) is what makes the query answerable at all: it turns a 41,064-row population
into a 2-row population that fits the page 128× over, keeps coverage honest,
costs 100× less, and lets the Summary/Span/Burst clauses run instead of being
starved. (a) alone does not answer the query — measured, a page of 256 contains
zero matching rows under either ordering — so shipping (a) first would convert a
truthful refusal into a confidently-labelled wrong answer.

(a) is still required, because `dashboard contracts generate` proves a strict
population can exceed the page, and "more matches than one page" must be a
partial-coverage result, not a refusal. It should land after (b), when the
partial label is rare enough to mean something.

Two amendments to the options as posed: the (b) fallback trigger must be
"strict form yields zero", not "fewer than the page", and the fallback must be a
graduated relaxation rather than full OR. Both are measured above.

If (a) is taken, note that bm25 ordering is not free: it conflicts with the
recency cursor keyset every clause uses, and it would be the first relevance
signal in the temporal candidate path, which today ranks by per-channel constant
alone. Adding term coverage to the candidate and to `candidate_score` is the
smaller change and belongs with (b).

## Fixed on the way

`validate_clause` refused an over-long clause with
`BudgetExceeded { resource: "candidate clause bytes", accounting: None }`, so a
query longer than the admitted metadata field cap reached the operator as
`budget-refused` with no ceiling and no length — the same blindness the record
budgets had before. It now carries
`ReadBudgetAccounting::requested(clause_cap, clause.value.len())`, covered by
`root_candidate_preparation_sizes_the_clause_byte_refusal`.

Two candidate-producer refusals still report `accounting: None`: `candidate
filter scans` and `candidate bytes`. Both are reachable, neither has a test, and
neither is reachable from the existing retrieval fixtures — their anchors carry
no owner kind, so the root authority predicate excludes them from every
candidate clause. Sizing those two needs a candidate fixture wide enough to
exhaust a scan ceiling, which is its own slice.
