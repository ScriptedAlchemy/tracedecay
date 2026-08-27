# Session-temporal benchmark

> **Historical evidence only.** Preserve the authentic provider fixtures,
> sanitization provenance, and measurements in this directory. Current
> requirements come only from the `docs/plans/tracedecay-v2/` hierarchy; exact
> commands, test names/counts, snapshots, receipts, attestations, PR packets,
> and gates below are not rebuild instructions. Validate current behavior directly.

Linux/macOS diagnostic harness for session-temporal retrieval latency phases.
This directory records descriptive sample quantiles and directly executed
behavioral test outcomes only. It is not an acceptance snapshot, receipt,
manifest, or gate.

## Fixtures

Provider-native Codex captures reused from
`tests/fixtures/provider_normalization/codex/`:

- `session_meta.input.json` and `agent_message.input.json`

Runtime sanitizer provenance for those fixtures is in
[`fixtures/codex-sanitization-receipt.json`](fixtures/codex-sanitization-receipt.json)
(observation sanitization provenance only). Do not substitute golden lookalikes.

## Artifacts

| Path | Role |
|---|---|
| [workload-v1.json](workload-v1.json) | Versioned workload/config pin |
| [evidence-index.json](evidence-index.json) | Points to current provisional evidence only after a clean refresh; retains stale captures separately |
| [result-provisional.json](result-provisional.json) | Historical single-session Linux capture; not evidence for the root-wide harness |
| `result-current.json` | Created only by a clean `--refresh-contract` run for the active harness |

## Commands

```bash
scripts/run-session-temporal-benchmark.sh --dry-run
scripts/run-session-temporal-benchmark.sh --run   # diagnostic on Linux or macOS
scripts/run-session-temporal-benchmark.sh --refresh-contract  # Linux only
cargo bench --bench session_temporal --all-features -- --run
```

Dry-run is Cargo-free. `--run` isolates `HOME` and `TRACEDECAY_DATA_DIR` and
measures: `rebuild_activate`, `exact_replay`, `compact_rank`, and `late_hydrate`.
`--run` prints diagnostic samples but never changes checked-in evidence.
`--refresh-contract` is Linux-only and the only publishing path: it requires a
clean source commit, performs that same real measurement without accepting
caller-supplied values, and publishes the workload and result as one
hash-checked pair before pointing the evidence index at it. The refreshed
provenance records the source commit and mode, warmups, measured repetitions,
and record counts.

## Observed focused tests

Recorded only when executed in the same capture window:

```bash
cargo test --test session_suite --all-features temporal_derived_evidence:: -- --test-threads=1
```

Quantiles are descriptive nearest-rank sample labels, not inferential claims.
