# Direct search-quality evaluation is evidence, not activation authority

The direct evaluator (`tracedecay_query::search_quality`, driven by
`tracedecay-search-eval`) runs production retrieval over a packaged workload and
corpus, then scores the result against checked-in labels. This note records what
that measurement may and may not be used for, because the qualification gate it
used to feed no longer exists.

## What was deleted, and why

`feat(search-quality): qualify on a paired held-out effect` (`881d25579a`)
introduced a real activation gate: a predeclared practical-effect threshold of
20 000 ppm plus a two-sided 95% paired Student-t interval on the held-out mean

```text
d(query) = nDCG@10(candidate profile) - nDCG@10(query-fallback)
```

whose lower bound had to exceed zero. The rule travelled with the evidence as
`methodology_version`, and `policy_freeze` pinned the commit and every profile's
tuning material digest so the threshold could not be re-tuned to fit the result
it produced.

The same subtraction applies to the policy slice that survived that deletion.
`decision_policy` could express only `required_cancellation =
bounded_typed_cancelled` plus an unread `required_fallback_byte_stability`
flag. Candidate generation already proves cancellation fail-closed
(`prove_cancellation`) and already records fallback byte equality as
`fallback_stable` / `fallback_matches_expected`. The slice, the single-variant
`RequiredCancellationV1` stamp, and the report aliases `cancellation_bounded`
and `offline` (the latter was `fallback_matches_expected` copied) added no
observation. Schema 1 refuses `decision_policy` the same way it refuses a
methodology. The leftover `QUALIFICATION-METHODOLOGY.md` described that
deleted gate as if it were still the live rule; it is gone rather than kept
as a stub.

`refactor(retrieval): retire dense FastEmbed path for lexical/graph`
(`8e7952f91a`) deleted `native_qualification.rs` along with the whole semantic
runtime. That was not an oversight: the gate existed to decide whether a
*candidate* retrieval profile should be activated in place of the always-on
`query-fallback` lanes. With the dense lane gone, the packaged profile matrix
holds exactly one profile, `query-fallback` itself, so there is no candidate,
no paired difference, and nothing for a held-out interval to decide. Keeping a
statistical gate that can only ever compare a profile with itself would report
authority it does not have.

## The evidence-only boundary

Consequently:

- The exact/lexical/graph lanes are unconditionally part of production
  retrieval. No measurement activates them and none can switch them off.
- A `Pass` from `tracedecay-search-eval compare` means the checked-in labels
  were met on the packaged corpus at that commit. It does not qualify, activate,
  promote, or accept a retrieval profile, and no CLI, status surface, or caller
  may present it as doing so.
- `validate_workload_for_tuning` and
  `validate_need_provenance_against_embedded_corpus` attest that measurement
  *inputs* are fit and sourced, schema, execution contract, byte-exact corpus,
  partitions, labels, and every natural-language need's verbatim provenance
  quote. Verified provenance bounds what the measurement is worth; it grants no
  activation authority.
- The packaged natural-language needs are therefore evidence about ranking
  quality on conceptual queries, nothing more. Their `train`/`validation`
  partitions keep label authorship honest (labels are authored per partition and
  scored identically); they are not a held-out significance test.

## Schema ownership

Two independent schema numbers meet in this subsystem, and neither may be read
as the other:

| Document | Version | Owner |
| --- | --- | --- |
| `CandidateWorkloadV1` (`query-lexical-graph-workload-v1.json`) | 1 | the workload contract, pinned by `packaged::WORKLOAD_SHA256` |
| `ProductionCandidateOutputV1` (an evaluator run's candidate evidence) | 2 | the producer in `tracedecay-search-eval` |

The direct evaluator refuses candidate outputs whose `schema_version` is not 2,
so a schema-1 workload document can never be re-read as candidate evidence or as
a qualification record.

## Re-introducing a gate

Restoring an activation gate is a schema change, not a comment change. Workload
schema 1 carries no `methodology_version`, no practical-effect bound, no
`policy_freeze`, and no `decision_policy`, and `deny_unknown_fields` refuses
those keys outright, so a workload cannot smuggle a methodology back in. A
future gate needs, at minimum:

1. a second profile in the packaged matrix to serve as the candidate,
2. a versioned methodology declaration (`methodology_version`) travelling with
   the evidence,
3. a predeclared practical-effect bound and interval rule, and
4. a `policy_freeze` binding the commit and every profile's material digest.

Until all four exist, the honest report is evidence-only.
