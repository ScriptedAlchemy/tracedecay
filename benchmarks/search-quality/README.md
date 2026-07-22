# PR9 search-quality production acceptance

`pr9-production-acceptance-v1.json` binds every PR9 acceptance area to
checked-in production APIs, direct Rust contracts, and the real frozen
search-quality corpus. It covers deterministic extraction and generations,
projection replay after restart, exact non-demotion, lexical ranking,
graph/Git/diagnostic/test joins, coverage and abstention, V1 import parity,
and Linux/Windows/macOS execution lanes.

This directory contains production-acceptance preflight plus optional parent-gate
execution receipts. Linux contract gates may be recorded as `executed_passed`;
Windows/macOS may remain `pending_unsupported_host` when those runners are
unavailable. Promotion stays `pending_parent_gates` until every required platform
gate and a locked accepted report exist. The validator never fabricates accepted
locked evidence, never runs Cargo, and never changes Git state.

Run the static validator:

```sh
python3 benchmarks/search-quality/validate_pr9_production_acceptance.py
```

List the commands reserved for the parent:

```sh
python3 benchmarks/search-quality/validate_pr9_production_acceptance.py \
  --list-parent-gates
```

The earlier contract-only run report was removed because it presented missing
locked execution as a terminal result. The development fixture contract remains
useful for integrity and abstention oracles, but it is not promotion authority.
