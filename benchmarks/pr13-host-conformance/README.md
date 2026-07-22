# PR13 host conformance packet

`workload-v1.json` is a compact behavioral contract validated by the checked-in
Draft-07 `schema-v1.json`. Authentic event payloads stay in the Hook V2 fixture
tree and are decoded by production typed decoders in
`tests/pr13_host_bundle_acceptance.rs`.

Packet lint:

```sh
python3 benchmarks/pr13-host-conformance/validate_packet.py
```

Platform evidence tests (cross-OS rejection + aggregation):

```sh
python3 benchmarks/pr13-host-conformance/test_platform_evidence.py
```

Strict acceptance requires:

1. `red_gaps` only lists unavailable host-capture lanes already marked
   `unavailable` in `hosts[]`.
2. Shared `ci_gate_id`s pass via OS-tagged nextest/cargo junit and/or executed
   allowlisted commands.
3. `platform_{linux,windows,macos}_lifecycle` are **default-feature** product
   lifecycle gates. Each requires OS-tagged proof
   (`linux:platform_linux_lifecycle`, etc.). Linux evidence alone leaves
   Windows/macOS unresolved until those CI artifacts arrive.

Checked-in `ci_gate_status` may only be `awaiting_ci` or `failed`. `passed` is
never checked in.

```sh
# Local honesty: strict fails while gates remain awaiting_ci
python3 benchmarks/pr13-host-conformance/validate_packet.py --strict

# One CI runner (does not close other OS platform gates):
python3 benchmarks/pr13-host-conformance/validate_packet.py --strict \
  --runner-os linux \
  --junit linux=target/nextest/ci/junit.xml \
  --npm-passed check --npm-passed test --npm-passed package \
  --gate-passed pr13_lite_grammar_contract \
  --gate-passed linux:platform_linux_lifecycle

# Full aggregation after Linux + Windows + macOS artifacts exist:
python3 benchmarks/pr13-host-conformance/validate_packet.py --strict \
  --junit linux=.../linux/junit.xml \
  --junit windows=.../windows/junit.xml \
  --junit macos=.../macos/junit.xml \
  --gate-passed linux:platform_linux_lifecycle \
  --gate-passed windows:platform_windows_lifecycle \
  --gate-passed macos:platform_macos_lifecycle \
  ...
```
