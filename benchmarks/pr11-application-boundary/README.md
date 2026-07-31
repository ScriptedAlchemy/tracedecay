# PR11 application-boundary compile measurement

> **Historical evidence only.** Preserve the measurement provenance in this
> directory. Current requirements come only from the
> `docs/plans/tracedecay-v2/` hierarchy; exact commands, timings, snapshots,
> receipts, attestations, PR packets, and gate fields below are not rebuild
> instructions. Validate current application-boundary behavior directly.

`compile-baseline.json` pins the application-owner compile command and the
allowed authority boundary. Its timing is explicitly labeled as a historical
measurement, not a CI budget.

Validate the legacy measurement fixture without building:

```sh
python3 benchmarks/pr11-application-boundary/validate_compile_baseline.py \
  benchmarks/pr11-application-boundary/compile-baseline.json
```

To produce a reviewable candidate measurement without modifying the golden:

```sh
python3 benchmarks/pr11-application-boundary/validate_compile_baseline.py \
  benchmarks/pr11-application-boundary/compile-baseline.json --run
```

Normal all-feature workspace checks and direct boundary tests determine product
completion; this measurement creates no release gate or acceptance artifact.
