# PR9 search-quality direct verification

Direct Rust contracts cover deterministic extraction and generations,
projection replay after restart, exact non-demotion, lexical ranking,
graph/Git/diagnostic/test joins, coverage and abstention, and V1 import
parity against the real sanitized search-quality corpus. There is no static
acceptance snapshot, packet, gate manifest, owner receipt, or promotion
authority in this directory; the legacy locked-acceptance scaffolding was
removed when the delivery model shifted to direct contract execution.

Developer quality evaluation is Linux-only. Normal Linux/macOS/Windows CI owns
default-feature product support.

Run the direct contracts through Cargo, for example:

```sh
cargo nextest run --all-features --no-fail-fast -E 'binary(search_quality_suite)'
```
