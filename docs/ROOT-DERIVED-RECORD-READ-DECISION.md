# Decision needed: group members in the root record read

Root-scope message search refuses with a record-read budget exhaustion whose
cost is mis-sized for the request. Fixing it changes what coverage counters can
observe, so the choice is a design decision rather than a defect repair. This
memo states the measurement and the options; it does not choose.

## Observed failure

`message_search` for a rare term over the operator's profile root returns
`application.retained.budget-refused` after 18–26 s. The query matches 18
occurrences in 2 sessions out of 1,454.

## Measurement

Measured read-only against the operator's live store
(`~/.tracedecay/projects/proj_a5b3d7e3ebe14ca7/sessions.db`, 16 GB, 1,454
sessions, 1,167 active generations) and against the daemon's Hotpath gauges.

Gauge deltas across one refused request placed the refusal precisely:
`temporal_query.candidates.generated` fired with 50 candidates and
`temporal_query.candidates.visible` never fired. Candidate planning and the
participant freeze therefore both succeeded; the refusal happens in the record
read between those two gauges.

The wall clock and the refusal are two separate defects.

The wall clock was the root span and burst candidate clauses, which scanned
every evidence row in the root with a correlated FTS subquery. That is fixed:
`ROOT_DERIVED_CANDIDATE_QUERY` now drives from the match (6.87 s → 0.01 s for
span, 5.09 s → 0.01 s for burst, byte-identical rows).

The refusal is the record read, and it survives that fix. For this query the
derived cohort is:

| kind  | candidate rows | member occurrences | largest group |
| ----- | -------------- | ------------------ | ------------- |
| burst | 12             | 1,689              | 270           |
| span  | 15             | 467                | 32            |

The derived-member arm of the record query
(`crates/tracedecay-session-temporal-store/src/retrieval/records.rs`) joins
`session_derived_evidence` → `session_derived_evidence_members` →
`session_occurrences`, so every member of every span and burst candidate enters
the record set: 2,156 records plus the direct occurrence records, against
`ExecutionLimits::default().record_limit` of 1,024. `commit_pulled_page`
reports `BudgetExceeded { resource: "record item count" }`, which now surfaces
as the `record_read_exhausted` stage.

2,156 records to rank 18 matched occurrences is the mis-sized cost. It scales
with how large the store's spans and bursts are, not with the request.

## Why those records cannot become results

`execute_temporal_candidate_export` excludes derived group anchors from
`visible_candidates`; a span or burst can never be a result. The group exists
to pull its member occurrences into the record read so that a member matching
on its own is ranked — but such a member already has its own Lexical or Phrase
candidate. The only consumer of the extra anchors is
`temporal_context_frames`, which counts them into the visible / hidden /
unknown coverage totals.

So the records the budget refuses over feed coverage counters, not results.
That is why narrowing the expansion is a semantics decision.

## Options

1. **Stop enumerating group members in the record read.** Read the group's own
   first and last occurrence and let members arrive through their own
   candidates. Smallest cost (27 records instead of 2,156 here) and removes the
   store-size coupling entirely. Changes coverage counts for derived evidence:
   members that match nothing are no longer counted as hidden.

2. **Bound the per-group expansion and report a typed omission.** Keep the
   member read but cap it per group, and emit an omission naming the truncated
   group instead of refusing the request. Preserves a coverage signal and keeps
   the refusal out of the common path, at the cost of a new omission kind and
   coverage totals that are explicitly partial rather than exact.

3. **Drop Span and Burst from the root-scope candidate plan.** Keeps them for
   single-session scope, where the group count is bounded by one session.
   Simplest, but root-scope retrieval loses derived evidence as a channel.

4. **Raise `record_limit`.** Rejected. The cost is mis-sized, not the limit;
   any ceiling large enough for 270-member bursts across a large root is large
   enough to make the read unbounded in practice.

## What blocks the hermetic test

The reproduction has to go through `SessionTemporalExecutionPort::execute`, not
`freeze`, because the refusal is in the record read. The
`participant_freeze` fixtures reach `freeze` only: `execute` returns
`Unavailable` on those fixtures even for a corpus that freezes cleanly, because
they seed no cursor signing key. A root fixture that can drive `execute` is a
prerequisite for whichever option is chosen. Shape it as 300 sessions, a rare
term matching a handful of them, and spans whose member totals exceed
`record_limit`; the test asserts the search returns its handful of hits.

Note also that `execute` maps many distinct failures through
`map_err(|_| SessionTemporalExecutionError::Unavailable)`, which is why
locating this needed a live daemon and gauge diffing rather than an error
message.

## Decision

Option 1. A derived group's record read is its own first and last occurrence;
interior members arrive through their own Lexical or Phrase candidates. Span and
Burst stay in the candidate plan, `record_limit` is unchanged, and occurrence
records are deduplicated by identity before the budget is charged, so a boundary
that also matched directly is one record and keeps its group provenance.

Coverage now counts the query-relevant result-eligible population — the distinct
anchors a channel proposed as results — rather than a group census. A member that
matched nothing was never a result this query could have returned, so it is not
a hidden omission.

Both prerequisites named above are resolved. The root fixture drives `execute`
(canonical observations and anchors, a seeded cursor signing key, and a published
relation projection), and `execute` no longer collapses distinct failures: a
budget refusal carries its stage, ceiling, and count, while deadline, cancel,
stale, storage, and reset stay separate typed states.

Measured on that fixture — 300 sessions, one matching message, a 2,000-member
span: the member-expanding read refuses with `RecordReadExhausted` (limit 1024,
consumed 1024 with more available); the boundary read returns the hit with
coverage totalling exactly the one query-relevant anchor. At the record-read
boundary a four-member span reads 2 records instead of 4, and a forty-member
span still reads 2.
