# PR18 Public SDK Plan

**Goal:** Publish Rust, TypeScript, and Python SDKs for accepted PR12–PR17
operations without inventing lifecycle semantics.

## Files and interfaces

- Rust workspace SDK crate and generated wire types.
- TypeScript and Python package roots, generators, conformance fixtures,
  package metadata, examples, and release CI.
- Bind local daemon and PR16 remote transports to one operation catalog.

Generated types cover wire schemas. Handwritten façades cover authentication,
`RequestContext`, paging/cursors, SSE reconnect, cancellation, resume,
idempotency, typed errors, operation receipts, `TaskHandoffToken`, and host
handoff tokens. Names freeze only after each operation's production journey is
accepted.

## Ordered slices

1. Freeze accepted operation/schema manifest.
2. Generate Rust/TS/Python wire models deterministically.
3. Implement lifecycle façades and local transport.
4. Implement remote transport with identical semantics.
5. Add examples and cross-language golden conformance.
6. Package/install/publish dry runs and compatibility policy.

## Tests

Direct: each language performs authentication, scoped read/write, paging, SSE
resume, cancellation, idempotent retry, Work admission/control, receipt
inspection, and handoff against local and remote fixtures.

Negative: stale cursor/CAS, wrong scope, missing capability, unavailable remote,
disconnect, duplicate changed-input request, malformed event, unsupported
version, and partial result remain the same typed error/outcome in each SDK.

## Migration, rollback, measurement, deletion

Generated schemas are additive until the accepted major-version policy permits
removal. Rollback unpublishes or yanks a package release according to registry
policy but never changes server semantics. Measure generation, package size,
startup, paging/SSE overhead, and conformance duration. Delete private client
wrappers and aliases only after three-language local/remote conformance,
examples, package/install gates, semver review, and normal CI pass.
