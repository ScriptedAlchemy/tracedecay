# Advisory production report

## Result

- Configured GitHub Actions CI discovery now preserves `Stale` source access as a typed advisory state.
- The feedback composition maps that state to `ProviderEvaluationStateV1::Stale`, stale telemetry, and stale coverage.
- Stale/denied access stops before any CI provider request; GitHub remains read-only and credential/redaction boundaries are unchanged.
- Removed the unused CI discovery future assertion and obsolete staged `read_workflow_jobs` dead-code allowance; the method is exercised by bounded CI discovery.

## Evidence

- Red: `cargo test -p tracedecay-usecases --lib advisory::ci_runtime::production::discovery_tests::stale_ci_access_remains_stale_without_a_network_read -- --exact` failed with `Unavailable`.
- Green: the same exact test passed after the state cutover.
- `CARGO_TARGET_DIR=/tmp/tracedecay-advisory-prod-target cargo test -p tracedecay-usecases --lib advisory:: -- --nocapture` — 54 passed.
- `cargo fmt --check` — passed.
- `CARGO_TARGET_DIR=/tmp/tracedecay-advisory-prod-target cargo check -p tracedecay-usecases --all-features` — passed.

## Scope

- No live credential, GitHub, or CI mutation occurred.
- Existing bounded polling, read-only transport, authorization rechecks, idempotent retention, and teardown ownership remain unchanged.
