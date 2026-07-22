# PR10 locked semantic-search evaluation packet

This packet freezes the PR10 evaluation shape without claiming that locked
acceptance has run. It audits delivered callable boundaries and direct
regressions, then keeps activation disabled until the parent executes the
locked quality and runtime gates.

`workload-v1.json` pins the real sanitized repository corpus and query bytes,
the production FastEmbed and vector-service boundaries, the exact-flat oracle,
equal-budget hybrid and reranking candidates, cohort/generation-bound
calibration and abstention, byte-identical PR9 fallback, current/10x resource
strata, Linux/Windows native-runtime strata, and cold offline rollback. It also
pins library-first/default-equals-all feature behavior, local
versioned-manifest SHA-256-verified model bytes, asynchronous semantic
projection, non-blocking exact/lexical/graph search during indexing, omission
of every non-current generation, strict-semantic typed unavailability, and
atomic visibility of a complete compatible generation.

The profile matrix deliberately keeps ANN, late interaction, and quantization
as evidence-gated research candidates. The exact-flat production scan is the
semantic oracle. Neither aggregate quality nor public benchmark rank can
activate a candidate.

`result-pending.json` contains no samples, metrics, fallback digest, locked
report, promotion evidence, or gate receipts. The only valid checked-in result
before the parent gates execute is `pending`, with semantics disabled.

Run the local contract without Cargo:

```text
python3 benchmarks/pr10-semantic-search/validate_packet.py
python3 -m unittest discover -s tests/search-quality -p "test_*.py"
```

Strict mode is a negative acceptance check:

```text
python3 benchmarks/pr10-semantic-search/validate_packet.py --strict
```

The validator parses the root feature manifest, inspects the callable
production function bodies, and verifies that named direct regressions invoke
the required search-during-indexing, non-blocking fallback, calibration,
exact-flat, and atomic-publication behavior. It does not accept source-path or
symbol-name scaffolding as evidence.

Strict mode exits with status 3 until the parent has executed and anchored all
library/default-feature, local-model, production FastEmbed/vector,
search-during-indexing, atomic-activation, saved-candidate, locked-holdout,
current/10x resource, Linux/Windows native-runtime, byte-stable fallback, cold
offline rollback, and aggregate gates. This packet must not be edited into an
`accepted` result; an accepted locked report is a later immutable parent-run
artifact.
