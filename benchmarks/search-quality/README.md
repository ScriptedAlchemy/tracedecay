# PR9 search-quality direct verification

The historically named `pr9-production-acceptance-v1.json` points at
production APIs, direct Rust contracts, and the real sanitized search-quality
corpus. It is a static compatibility fixture, not an acceptance snapshot,
packet, gate manifest, owner receipt, or promotion authority. The direct tests
cover deterministic extraction and generations,
projection replay after restart, exact non-demotion, lexical ranking,
graph/Git/diagnostic/test joins, coverage and abstention, and V1 import parity.

Developer quality evaluation is Linux-only. Normal Linux/macOS/Windows CI owns
default-feature product support. Any legacy `pending_parent_gates` value is
interpreted only as `pending`; the validator never grants activation authority,
runs Cargo, or changes Git state.

Run the static validator:

```sh
python3 benchmarks/search-quality/validate_pr9_production_acceptance.py
```

List the direct commands represented by the static fixture:

```sh
python3 benchmarks/search-quality/validate_pr9_production_acceptance.py \
  --list-parent-gates
```
