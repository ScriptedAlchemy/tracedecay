# PR10 locked semantic-search evaluation packet

This packet freezes the PR10 evaluation shape without claiming that locked
acceptance has run. It audits delivered callable boundaries and direct
regressions, then keeps activation disabled until the parent executes the
locked quality and runtime gates.

`workload-v1.json` pins the real sanitized repository corpus and query bytes,
the production FastEmbed and vector-service boundaries, the exact-flat oracle,
equal-budget hybrid and reranking candidates, cohort/generation-bound
calibration and abstention, byte-identical PR9 fallback, current/10x resource
strata, and cold offline rollback. It also pins library-first/default-equals-all
feature behavior, local versioned-manifest SHA-256-verified model bytes,
asynchronous semantic projection, non-blocking exact/lexical/graph search during
indexing, omission of every non-current generation, strict-semantic typed
unavailability, and atomic visibility of a complete compatible generation.

OS matrix execution (Linux/Windows/macOS default-feature product lifecycle) is
owned by PR13 host CI, not this eval packet.

The profile matrix deliberately keeps ANN, late interaction, and quantization
as evidence-gated research candidates. The exact-flat production scan is the
semantic oracle. Neither aggregate quality nor public benchmark rank can
activate a candidate.

`result-pending.json` stays non-authoritative: no measured locked samples,
fallback digest, locked report, or promotion evidence. Parent-gate receipts may
record truthful `executed_contract` or `blocked` states, but outcome remains
`pending` and semantics stay disabled until a locked accepted report exists.

Run the local contract without Cargo:

```text
python3 benchmarks/pr10-semantic-search/validate_packet.py
python3 -m unittest discover -s tests/search-quality -p "test_*.py"
```

Strict mode is a negative acceptance check:

```text
python3 benchmarks/pr10-semantic-search/validate_packet.py --strict
```
