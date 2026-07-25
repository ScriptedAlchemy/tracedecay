# PR12 transport-boundary direct test fixture

This directory supplies a checked-in real fixture to the direct product test
for the callable PR12 application boundary. It is not an acceptance packet,
snapshot, gate manifest, or separate evidence authority. Product acceptance
comes from direct tests and normal CI, with pending work reported as pending.

`goldens/application-surface-parity.json` is consumed by
`tests/api_application_parity.rs`. Together they require:

- identical request/result contracts and binding identities for Git preview,
  Git apply, feedback diagnostics, feedback get, feedback expand, and feedback
  list across CLI, MCP, and HTTP dispatch;
- all four feedback reads to reach a callable owner and conceal an unknown
  request handle as `not_found_or_not_authorized`, never `unavailable`;
- HTTP and SSE to project the same canonical feedback payload; and
- concealed HTTP problems to omit binding identity.

The compile workload remains executable, but its measurement is explicitly
pending execution. Static validation does not run Cargo:

```sh
python3 benchmarks/pr11-application-boundary/validate_compile_baseline.py \
  benchmarks/pr12-transport-boundary/compile-baseline.json
```

An authorized measurement run may pass `--run`; the runner prints a candidate
and never rewrites the checked-in baseline.
