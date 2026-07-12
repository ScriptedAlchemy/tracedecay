<!-- Generated from architecture-boundaries.toml; do not edit. -->
# V2 Convergence Scorecard Skeleton

| Metric | Detector | Target |
|---|---|---|
| canonical-ownership-coverage | `inventory-owner-bijection` | 100% |
| duplicate-authority-count | `stores-tables-owner-analysis` | 0 |
| unowned-store-table-count | `inventory-owner-completeness` | 0 |
| direct-canonical-writers | `source-writer-scan` | 0 |
| capability-coverage | `catalog-handler-bijection` | 100% |
| transport-conformance | `transport-fixture-matrix` | 100% |
| generated-contract-drift | `generated-view-byte-compare` | 0 |
| adapter-burn-down | `adapter-ledger-expiry-and-call-sites` | 0 expired |
| dependency-cycles-forbidden-imports | `cargo-metadata-and-source-policy` | 0 |
| complexity-debt | `complexity-delta-report` | 0 non-waived |
| rust-package-count | `cargo-metadata-package-count` | <=11 |
| negative-code-parity | `handwritten-line-disposition-delta` | 0 non-waived |
| definite-duplicate-bodies | `rust-labeled-duplicate-scan` | 0 non-waived >10 lines |
| dependency-artifact-footprint | `cargo-tree-feature-artifact-report` | 0 unjustified |
| runtime-build-footprint | `frozen-reference-benchmark` | binary/RSS/hot <=1.25x; clean <=1.5x |
| coverage-truth | `transport-partial-stale-unknown-fixtures` | 0 known omissions |
| scope-resolver-implementations | `scope-resolver-entry-scan` | 1 |
| query-semantic-implementations | `query-facade-source-scan` | 0 bypasses |
| policy-decision-implementations | `policy-evaluator-source-scan` | 0 outside bundles |
| redaction-entry-implementations | `privacy-sink-canary-matrix` | 0 bypasses |
| v1-traffic-after-cutover | `adapter-traffic-ledger` | 0 outside rollback drills |
| typed-id-boundary-coverage | `protected-boundary-compile-fail` | 100% |
| error-status-config-parity | `surface-mapping-exhaustiveness` | 100% |
| infrastructure-engine-count | `registered-engine-source-scan` | 0 unregistered |
| generated-binding-coverage | `manifest-binding-bijection` | 100% |
| replayability | `pinned-artifact-replay-suite` | 100% supported exact paths |
| hook-budget-conformance | `hook-latency-reference-benchmark` | 100% |
| projector-rebuild-determinism | `pinned-observation-rebuild-suite` | 100% |
