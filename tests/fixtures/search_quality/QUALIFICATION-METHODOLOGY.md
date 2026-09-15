# Semantic qualification methodology

The decision rule that lets a semantic retrieval profile serve production
queries. The workload beside this file (`query-semantic-candidate-workload-v1.json`)
carries the rule's declared terms in its `qualification_methodology` block; this
document explains why each term is what it is. The rule is versioned, and
`QUALIFICATION_METHODOLOGY_VERSION` in
`crates/tracedecay-query/src/search_quality/candidate_output.rs` is the version
the build implements. A report or packaged asset scored under any other version
is refused rather than reinterpreted.

## Methodology 2 — paired effect with a practical threshold

A candidate profile qualifies only when all of the following hold on the
**held-out** partition:

| Term | Value | Where it is enforced |
| --- | --- | --- |
| Effect | `d(query) = nDCG@10(candidate) − nDCG@10(query-fallback)` | `evaluate::paired_metric_differences` |
| Stratum | `natural_language` | `qualification_methodology.effect_stratum` |
| Held-out partition | `validation` (tuned on `train`) | `evaluate::measure_paired_effect` |
| Need floor | ≥ 20 paired needs per partition | `candidate_output::validate_effect_stratum_fitness` |
| Practical effect | mean `d` ≥ 20 000 ppm (0.02 nDCG@10) | `evaluate::measure_paired_effect` |
| Confidence | two-sided 95% Student-t interval on mean `d`, lower bound > 0 | `evaluate::paired_interval_95` |

A tie, a negative mean, or an interval that reaches zero is **not qualified**.
Fewer than two paired needs produces no interval at all; that is reported as
`Pending` — an unmeasured workload — rather than a measured loss, so an
unmeasurable run cannot be mistaken for a refusal or a pass.

### Why a practical threshold and not a statistical one alone

The rule this replaced required a 1 ppm gain in the natural-language stratum
*mean*. Two things were wrong with it. A 1-part-per-million difference in an
averaged metric is indistinguishable from tie-breaking noise, and a stratum mean
can rise while individual needs get worse — the last packaged qualification did
exactly that, raising the mean while dropping one of `validation-006`'s labelled
targets out of the ranking entirely.

The threshold is therefore a *product* judgment, declared before measurement:
below 0.02 absolute nDCG@10 the ranking an agent actually reads does not change,
so the model runtime a semantic profile costs is not repaid. The confidence
interval is a separate, *statistical* judgment: the sample must actually support
the claim that the effect is above zero. Both must hold. Neither substitutes for
the other, and the per-need dropped-label floor still applies on top of both.

### Why the effect is paired

Each need is scored by both profiles on the same labels and the same corpus, so
`d(query)` removes the per-need difficulty that dominates an unpaired
comparison. The pairing is by `query_id`, and only needs both profiles scored
enter the sample.

### Why the held-out partition alone decides

The tuning partition is measured under the identical rule and retained in the
report as comparison evidence, never as qualification. A profile that only wins
where its calibration was tuned is therefore visible in the report instead of
silently qualifying.

## Policy freeze

`qualification_methodology.policy_freeze` pins the commit at which the threshold,
confidence level, partitions, and **every profile's tuning material** were fixed.
`validate_policy_freeze` recomputes each profile's material digest and refuses
the workload if any of them moved. This is the mechanism that stops the failure
mode this profile already has a history of: `calibration_threshold_ppm` was
re-tuned five times in one day to move a failing activation gate. After the
freeze, moving it breaks the pin, so the rule cannot be adjusted to fit the
held-out result it produces.

## Workload fitness

The effect stratum must carry at least the declared floor of independently
sourced needs in **both** partitions, and every one of those needs must carry
`need_provenance`:

- `source_kind` — `corpus_documentation` or `corpus_error_contract`
- `source_document_id` — a document in the declared corpus
- `source_quote` — text that must be findable in that document's own prose
- `judgment_rationale` — why the labelled anchors are the answer

`validate_need_provenance_against_embedded_corpus` verifies every quote against
the corpus bytes the package actually ships, so a citation cannot drift from the
document it claims. The floor is a fitness requirement, not statistical proof:
twenty needs is enough to make an interval meaningful, not enough to make a
narrow one.

Needs are authored from what the corpus documents *say about themselves* — a doc
comment or an error contract — and judged against the symbols that satisfy the
stated behavior. No need is written to make the lexical baseline fail, and no
need requires the baseline to stay wrong. Several needs use vocabulary the
baseline retrieves well; that is the point of a fair measurement.

## Activation gates

Qualifying evidence is necessary but not sufficient. Activation additionally
requires, and denies on:

1. **Workload fitness** — need floor met and provenance present and verifiable.
2. **Versioned methodology match** — the report's `methodology_version` equals
   both the workload's declared version and the build's constant.
3. **Held-out superiority** — `DirectEvaluationReportV1::validate_held_out_effect`
   re-derives the decision from the retained measurement against the workload's
   own declared threshold and confidence level. A passing report *status* is not
   accepted in its place.
4. **Cost and correctness contracts** — fallback byte stability, bounded typed
   cancellation, offline availability, resource budgets, and the protected-stratum
   regression floor.
5. **Evidence provenance** — workload, corpus, execution contract, profile
   material, and raw-output digests all bind to the current authorities.
6. **Packaged-asset admission** — only a validated native run may become a
   packaged asset. A fresh workload digest alone is explicitly insufficient;
   packaged schema 2 carries the methodology version, and schema 1 evidence is
   refused as an unsupported schema rather than reinterpreted.
