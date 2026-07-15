# V2 Rust metaprogramming budget

Status: accepted architecture decision.

Date: 2026-07-14.

## Decision

V2 uses custom Rust metaprogramming only when it removes a duplicate semantic authority or makes a stable product invariant structurally impossible to violate. Reducing syntax or hiding a second implementation is not sufficient.

The current budget is:

- Keep one private `macro_rules!` family for validated scalar and identity types.
- Keep one product-generation category that derives host adapters from a canonical product artifact.
- Add no procedural macro crate or custom attribute macro now.
- Build the future MCP/CLI operation catalog from ordinary typed runtime data before considering a macro DSL.
- Consider at most one future derive for recursive closed-wire validation, and only after a no-proc-macro spike fails the admission gates below.
- Do not generate architecture inventories, rewrite metadata, plan views, policy snapshots, or product declarations from a parallel YAML, JSON, or Markdown model.

This budget is a ceiling, not a target.

## Approved mechanisms

### Validated scalar identities

The private declarative macros in `crates/tracedecay-domain/src/research/id.rs` encode a stable boundary: construction and deserialization apply the same validation while each identifier remains a nominally distinct type.

Keep this mechanism local to the domain module. It may share a validator-parameterized internal implementation when that deletes real duplication, but it must not become an exported newtype framework or a procedural macro.

### Canonical product artifact to host adapters

The build-time generation in `build.rs` and `src/agents/plugin_bundle.rs` derives host-specific installation artifacts from the canonical agent catalog. This is the intended canonical-kernel/generated-adapter pattern because it deletes hand-maintained product copies.

Keep generated output deterministic and product-facing. If build support grows, split pure helpers by subsystem rather than creating a general generator platform.

## Typed operation catalog before macros

The current MCP and CLI operation surface repeats names, aliases, definitions, dispatch, availability, scope, and parity checks across several files. V2 should converge those facts into one typed runtime catalog as operations move behind application boundaries.

Start with ordinary Rust structs, enums, functions, and handler normalization. A macro that merely emits the current definition function and dispatch arm would shorten duplicated authorities without deleting them and is therefore rejected.

The catalog may drive MCP definitions, handler lookup, CLI exposure, aliases, availability, and table-driven tests. It must not become a god registry that owns business logic, rendering, authorization, analytics, and transport behavior.

## Closed-wire spike

`crates/tracedecay-domain/src/research/manifest/strict_wire.rs` is the only credible future procedural-derive candidate because it manually mirrors the fields, variants, and nesting of the actual wire types.

Before adding a procedural macro:

1. Preserve `ClosedJsonValue` duplicate-key rejection at the parse boundary.
2. Apply or verify `#[serde(deny_unknown_fields)]` throughout the participating wire tree.
3. Deserialize the checked value into the actual wire types rather than a private mirror model.
4. Use a maintained path-aware deserialization library if needed to preserve actionable nested errors.
5. Delete each corresponding manual `strict_*` function as the type declaration becomes authoritative.
6. Test nested structs and every enum representation for duplicate keys, unknown fields, unknown variants, and cross-variant fields.
7. Verify valid fixtures, canonical serialization, and manifest digests remain unchanged.
8. Measure deleted and added production lines plus clean and incremental build cost.
9. Stop if the approach requires another private copy of the complete wire model.

This spike must not block the next production slice.

## Procedural-macro admission gates

A single closed-wire derive may be proposed only when all of these are true:

- The no-proc-macro spike has a minimized test proving that required behavior cannot be preserved with Serde composition and an appropriate maintained library.
- Rust structs and enums remain the sole field and variant authority; no external schema or field-name inventory is added.
- At least 500 non-test lines, or 65 percent of the manual strict-wire implementation, are removed.
- Net production deletion remains at least 300 lines after counting the macro crate and support code.
- Duplicate known and unknown keys, recursively unknown fields, unknown variants, and fields from the wrong tagged variant are rejected.
- Existing valid fixtures, canonical serialization, and manifest digests remain identical.
- Nested error paths remain actionable.
- Unsupported Serde forms fail at compile time and are covered by compile-fail tests.
- `flatten`, tagging modes, untagged enums, custom deserializers, defaults, and rename behavior are explicitly supported and tested or explicitly rejected.
- There is no runtime reflection or metadata registry.
- Repeated measurements show no more than a 5 percent clean-build regression and no more than a 200 ms incremental-build regression for an unrelated edit.
- The derive owns no domain validation, digest, persistence, rendering, transport, or business behavior.
- The result deletes more code and authorities than it introduces and does not expand `tracedecay-domain` into a generic god crate.

If admitted, the maximum design is one procedural-macro crate, one derive, and one sealed runtime trait owned by `tracedecay-domain`. Custom attribute macros remain out of scope.

## Rejected macro targets

Do not add custom macros for:

- Proof-carrying sanitization or trust-boundary construction, which must remain visibly auditable.
- General domain-validation rules, which have distinct semantics and should remain typed functions and methods.
- Error taxonomies already served by maintained derives such as `thiserror`.
- Markdown rendering, which requires deliberate human-facing presentation over typed view models.
- Store rows, events, transactions, or recovery without repeated V2 production patterns and measured net deletion.
- Untyped automation artifact payloads; surviving artifacts should become versioned structs or enums.
- Small enum string conversions, early-return control flow, or test DSLs.
- Architecture snapshots, source inventories, plan receipts, rewrite workflows, or other systems that model the rewrite instead of delivering product behavior.

## Review rule

Every future custom macro or generator proposal must state:

1. The stable invariant it owns.
2. The duplicate authority it deletes.
3. Why ordinary Rust and maintained libraries are insufficient.
4. Production lines and dependencies added and removed.
5. Compile-time, diagnostics, debugging, and API-stability costs.
6. How the mechanism remains bounded to one subsystem.

Without that evidence, use ordinary Rust.
