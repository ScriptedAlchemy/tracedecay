# PR13 advisory direct tests

`workload-v1.json` is a legacy behavioral fixture validated by the checked-in
Draft-07 `schema-v1.json`. It is not an acceptance packet, gate manifest, CI
receipt, or pass snapshot. The application four-pillar test owns canonical
advisory behavior; the root runtime test reads authentic GitHub/CI/proximity
captures and exercises production typed boundaries without a composite fixture
loader.

Run static fixture lint:

```sh
python3 benchmarks/pr13-advisory-milestone/validate_packet.py
```

Direct product completion requires empty `provider_gaps` plus the named Rust
tests and normal CI. Legacy `ci_gate_status` is compatibility input only:
`awaiting_ci` means `pending`, `failed` means `fail`, and no checked-in value
may claim `passed`.

```sh
python3 benchmarks/pr13-advisory-milestone/validate_packet.py --strict
python3 benchmarks/pr13-advisory-milestone/validate_packet.py --strict \
  --junit target/nextest/ci/junit.xml
```

Pagination/CAS and proximity tiering are product journeys covered by named
Rust tests; no owner receipt or PR-specific evidence artifact is created.
