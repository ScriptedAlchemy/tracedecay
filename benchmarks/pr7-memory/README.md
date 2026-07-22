# PR7 memory, fact, anchor, and migration benchmark

Delivery-focused owner acceptance for the PR7 memory/fact/provenance slice.
Evidence is canonical JSON plus SHA-256 digests of the source commit/tree,
workload/config/toolchain pins, and executed gate receipts. This directory does
not recreate the removed measurement scaffold (`src/store/memory_benchmark.rs`).

## Artifacts

| Path | Role |
|---|---|
| [workload-v1.json](workload-v1.json) | Versioned workload/config pin |
| [gate-manifest-v1.json](gate-manifest-v1.json) | Predeclared exact/no-op/migration/restart/privacy/anchor gates |
| [issue_receipt.py](issue_receipt.py) | Serial gate runner + canonical owner receipt writer |
| [owner-receipts-v1.json](owner-receipts-v1.json) | Content-addressed executed receipts |
| [evidence-index.json](evidence-index.json) | `current_acceptance` pointer or explicit blockers |
| [result-provisional.json](result-provisional.json) | Historical local measurement snapshot only |

## Gates (serial)

1. `exact` — production evidence-assembly exact member drilldown
2. `no_op` — checked-in provider fixtures through the production exact no-op path
3. `migration` — PR7 `user_version` 18→latest memory schema migration contract
4. `restart` — retrieval-anchor disposition replay survives restart
5. `privacy` — concrete secret redaction + project/worktree isolation
6. `anchor` — Git-topology retrieval-anchor contract on checked-in domain fixtures

## Owner evidence rule

`current_acceptance` is set **only** when every predeclared gate is
`executed_passed` **and** the worktree is a clean logical snapshot
(`git status --porcelain` empty). Otherwise the index keeps
`current_acceptance: null` and records exact pending/failed blockers.

```bash
python3 benchmarks/pr7-memory/issue_receipt.py \
  --manifest benchmarks/pr7-memory/gate-manifest-v1.json \
  --out benchmarks/pr7-memory/owner-receipts-v1.json \
  --log-dir benchmarks/pr7-memory/logs \
  --evidence-index benchmarks/pr7-memory/evidence-index.json \
  --workload benchmarks/pr7-memory/workload-v1.json \
  --wait-aggregate
```

Use `--wait-aggregate` so focused Cargo gates never overlap an aggregate
admission lane.
