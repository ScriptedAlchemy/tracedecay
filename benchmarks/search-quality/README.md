# PR9 search-quality production acceptance

`pr9-production-acceptance-v1.json` binds every PR9 acceptance area to
checked-in production APIs, direct Rust contracts, and the real frozen
search-quality corpus. It covers deterministic extraction and generations,
projection replay after restart, exact non-demotion, lexical ranking,
graph/Git/diagnostic/test joins, coverage and abstention, and V1 import parity.

OS matrix execution (Linux/Windows/macOS default-feature product lifecycle) is
owned by PR13 host CI, not this eval packet. Promotion stays
`pending_parent_gates` until required product gates and a locked accepted report
exist. The validator never fabricates accepted locked evidence, never runs
Cargo, and never changes Git state.

Run the static validator:

```sh
python3 benchmarks/search-quality/validate_pr9_production_acceptance.py
```

List the commands reserved for the parent:

```sh
python3 benchmarks/search-quality/validate_pr9_production_acceptance.py \
  --list-parent-gates
```
