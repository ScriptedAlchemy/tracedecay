# Transport-boundary compile baseline

> **Historical evidence only.** Current requirements come only from the
> `docs/plans/tracedecay-v2/` hierarchy; exact commands and fields below are
> not rebuild instructions.
> `crates/tracedecay/tests/product_surface_suite/api_application_parity.rs`
> verifies transport parity directly.

The compile workload remains executable, but its measurement is explicitly
pending execution. Static validation does not run Cargo:

```sh
python3 benchmark_data/pr11-application-boundary/validate_compile_baseline.py \
  benchmark_data/transport-boundary/compile-baseline.json
```

An authorized measurement run may pass `--run`; the runner prints a candidate
and never rewrites the checked-in baseline.
