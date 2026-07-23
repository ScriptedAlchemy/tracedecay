# PR10 semantic-search developer evaluation

This directory contains a Linux-only developer workload and static fixture
validation. It is not a PR acceptance packet, gate, holdout, owner receipt, or
activation authority. `result-pending.json` reports the current evaluation
truthfully; semantic activation remains a separate product configuration
operation protected by runtime compatibility and rollback checks.

`workload-v1.json` records the real sanitized repository corpus and query bytes,
the production FastEmbed and vector-service boundaries, the exact-flat oracle,
equal-budget hybrid and reranking candidates, cohort/generation-bound
calibration and abstention, byte-identical PR9 fallback, current/10x resource
strata, and cold offline rollback behavior. It also records library-first/default-equals-all
feature behavior, local versioned-manifest SHA-256-verified model bytes,
asynchronous semantic projection, non-blocking exact/lexical/graph search during
indexing, omission of every non-current generation, strict-semantic typed
unavailability, and atomic visibility of a complete compatible generation.

Normal Linux/macOS/Windows CI owns default-feature product build, test,
package, install, and lifecycle coverage. Developer benchmarking stays
Linux-only.

ANN, late interaction, and quantization remain research candidates. The
exact-flat production scan is the semantic oracle. Neither a checked-in file
nor public benchmark rank can activate a candidate.

`result-pending.json` stays non-authoritative and reports `pending` until the
Linux workload executes. It never stores owner approval, reveal state, gate
receipts, or promotion evidence.

Run the local contract without Cargo:

```text
python3 benchmarks/pr10-semantic-search/validate_packet.py
python3 -m unittest discover -s tests/search-quality -p "test_*.py"
```

Strict mode verifies that pending input does not claim success:

```text
python3 benchmarks/pr10-semantic-search/validate_packet.py --strict
```
