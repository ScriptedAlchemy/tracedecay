# PR13 host conformance packet

`workload-v1.json` is a compact behavioral contract validated by the checked-in
Draft-07 `schema-v1.json`. Authentic event payloads stay in the Hook V2 fixture
tree and are decoded by production typed decoders in
`tests/pr13_host_bundle_acceptance.rs`.

Packet lint runs only fixed, allowlisted static checks with timeouts:

```sh
python3 benchmarks/pr13-host-conformance/validate_packet.py
```

Strict milestone acceptance is separate and intentionally fails only while
required host lanes remain unavailable:

```sh
python3 benchmarks/pr13-host-conformance/validate_packet.py --strict
```

The packet stores CI gate IDs, never command arguments. CI resolves those IDs
to the Draft-07 schema test, production decoder test, AST structural test,
shared minimal no-secret kernel test, real PR12 transport/SSE parity test, and
no-default-features lite grammar contract. This static preparation does not run
Cargo.

Parent-run gate commands are allowlisted in `validate_packet.py`. The PR12
runtime gates target real Git preview/apply CLI, MCP, and HTTP paths, the Axum
SSE response stream, and the LSP gateway. The PR13 gates target Draft-07
schemas, production host decoders, AST structure, the shared no-secret kernel,
receipt-backed Doctor inspection, and the lite grammar build. Cursor-native
extension gates run its TypeScript check, package tests, and VSIX build; the
static gate binds the language-client command, bounded one-root forwarding,
gateway no-echo merge, reconnect/clear/teardown behavior, receipt-backed
install and Doctor registration, and supported capability declaration.

First-party host bundles are binary-embedded only. Their contract covers
manifest schema/version/capability, digest corruption, ownership, explicit
confirmation, receipts, backup, interrupted recovery, and rollback. External
or third-party bundle loading is rejected.

Capture paths are platform-neutral only where the provider wire schema is
platform-neutral. Installed-path and lifecycle claims remain explicit in the
host install contracts. Installing the generated VSIX into an external Cursor
desktop process remains an explicit strict gap.
