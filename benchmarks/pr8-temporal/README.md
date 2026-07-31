# PR8 session-temporal benchmark

> **Historical evidence only.** Preserve the authentic provider fixtures,
> sanitization provenance, and measurements in this directory. Current
> requirements come only from the `docs/plans/tracedecay-v2/` hierarchy; exact
> commands, test names/counts, snapshots, receipts, attestations, PR packets,
> and gates below are not rebuild instructions. Validate current behavior directly.

Linux measurement harness for PR8 temporal retrieval latency phases.
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
| [evidence-index.json](evidence-index.json) | Legacy pointer to the provisional measurement (`current_acceptance` is deprecated and always null) |
| [result-provisional.json](result-provisional.json) | Linux measurement + observed focused-test outcomes |

## Commands

```bash
scripts/run-pr8-temporal-benchmark.sh --dry-run
scripts/run-pr8-temporal-benchmark.sh --run   # Linux only; exit 64 elsewhere
scripts/run-pr8-temporal-benchmark.sh --refresh-contract
cargo bench --bench session_temporal --all-features -- --run
```

Dry-run is Cargo-free. `--run` isolates `HOME` and `TRACEDECAY_DATA_DIR` and
measures: `rebuild_activate`, `exact_replay`, `compact_rank`, and `late_hydrate`.
`--refresh-contract` requires a clean source commit, performs that same real
measurement without accepting caller-supplied values, and publishes the
workload and result as one hash-checked pair. The refreshed provenance records
the source commit and mode, warmups, measured repetitions, and record counts.

## Observed focused tests

Recorded only when executed in the same capture window:

```bash
cargo test --test session_suite --all-features temporal_derived_evidence:: -- --test-threads=1
```

Quantiles are descriptive nearest-rank sample labels, not inferential claims.
