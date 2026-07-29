# PR13 host conformance direct tests

`workload-v1.json` is a legacy compact behavioral fixture validated by the
checked-in Draft-07 `schema-v1.json`. It is not an acceptance packet, platform
gate manifest, CI aggregation authority, or checked-in pass snapshot.
Authentic event payloads stay in the Hook V2 fixture tree and are decoded by
production typed decoders in `tests/pr13_host_bundle_acceptance.rs`.

Static fixture lint:

```sh
python3 benchmarks/pr13-host-conformance/validate_packet.py
```

Direct product tests require unavailable host-capture lanes to remain explicit
and authentic host payloads to pass the production decoders. Normal
Linux/macOS/Windows CI independently builds and tests the default-feature
product on each supported platform; this directory does not aggregate JUnit,
mint platform evidence, or turn CI output into PR-specific receipts. Runtime
JUnit remains a CI artifact rather than validator-owned gate state.
