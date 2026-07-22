# PR11 application-boundary compile packet

`compile-baseline.json` pins the application-owner compile command and the
allowed authority boundary. Its timing is explicitly labeled as a historical
measurement, not a CI budget.

Validate the packet without building:

```sh
python3 benchmarks/pr11-application-boundary/validate_compile_baseline.py \
  benchmarks/pr11-application-boundary/compile-baseline.json
```

To produce a reviewable candidate measurement without modifying the golden:

```sh
python3 benchmarks/pr11-application-boundary/validate_compile_baseline.py \
  benchmarks/pr11-application-boundary/compile-baseline.json --run
```

The normal all-feature workspace checks remain the release acceptance gate.
