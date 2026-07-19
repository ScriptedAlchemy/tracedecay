# PR8 session-temporal benchmark

Workload schema 2 for the PR8 temporal retrieval resource harness.

## Fixtures

Provider-native Codex captures reused from
`tests/fixtures/provider_normalization/codex/`:

- `session_meta.input.json` and `agent_message.input.json` (same inputs as the
  PR5/PR6 `codex_production_observation_pipeline_v1` baseline)
- `thread_goal_updates.input.json` (redacted four-record production sequence)

Provenance and redaction are recorded in
[`fixtures/codex-sanitization-receipt.json`](fixtures/codex-sanitization-receipt.json).
Do not substitute golden lookalikes or invent protocol fields.

## Commands

```bash
scripts/run-pr8-temporal-benchmark.sh --dry-run
scripts/run-pr8-temporal-benchmark.sh --run   # Linux only; exit 64 elsewhere
cargo bench --bench session_temporal --all-features -- --run
```

Dry-run is Cargo-free and must not mutate the checkout. `--run` isolates
`HOME` and `TRACEDECAY_DATA_DIR`, enforces the optimized `[profile.bench]`, and
drives production Codex admit → CanonicalSessionTemporalProjector materialize →
`SessionRefreshService` / `SessionRetrievalService` phases:

`rebuild_activate`, `exact_replay`, `compact_rank`, `late_hydrate`, `member_expand`.

## Evidence

[`result-provisional.json`](result-provisional.json) stays provisional until a
valid Linux measurement is attested and promoted through
[`evidence-index.json`](evidence-index.json). Quantiles are descriptive
nearest-rank sample labels, not inferential claims.
