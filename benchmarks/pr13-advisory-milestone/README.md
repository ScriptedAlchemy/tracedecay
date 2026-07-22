# PR13 advisory milestone packet

`workload-v1.json` is a direct behavioral contract validated by the checked-in
Draft-07 `schema-v1.json`. The application four-pillar test owns canonical
advisory behavior; the root runtime test reads authentic GitHub/CI/proximity
captures and exercises production typed boundaries without a composite fixture
loader.

Run packet lint:

```sh
python3 benchmarks/pr13-advisory-milestone/validate_packet.py
```

Strict advisory acceptance requires empty `provider_gaps` plus direct
nextest/cargo junit (or `--run-gates`) evidence for every `ci_gate_id`.
Checked-in `ci_gate_status` may only be `awaiting_ci` or `failed` — never a
fabricated `passed` snapshot.

```sh
python3 benchmarks/pr13-advisory-milestone/validate_packet.py --strict
python3 benchmarks/pr13-advisory-milestone/validate_packet.py --strict \
  --junit target/nextest/ci/junit.xml
```

Pagination/CAS and proximity tiering are product journeys covered by named
Rust tests; completion is green CI evidence, not owner receipts.
