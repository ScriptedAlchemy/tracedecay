# Direct search-quality evaluation is evidence, not activation authority

The direct evaluator (`tracedecay_query::search_quality`, driven by
`tracedecay-search-eval`) runs production retrieval over a packaged workload and
corpus, then scores the result against checked-in labels. This note records what
that measurement may and may not be used for.

## The evidence-only boundary

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
so a schema-1 workload document can never be re-read as candidate evidence.
