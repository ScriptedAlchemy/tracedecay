# V2 generation and Rust metaprogramming rules

Status: accepted architecture decision.

## Decision

Use ordinary Rust first. Custom macros or generators are allowed only when one
typed product authority can deterministically replace multiple hand-maintained
product adapters or make a stable invariant structurally impossible to violate.
Reducing syntax is not enough.

The current ceiling is:

- one private `macro_rules!` family for validated scalar/identity types;
- deterministic product-adapter generation from an existing typed canonical
  product contract;
- no new procedural macro or custom attribute macro;
- no YAML, JSON, or Markdown source of truth parallel to Rust product types;
- no generator platform, architecture model, or rewrite-control framework.

This is a ceiling, not a target.

## Allowed generation

A product generator is acceptable only when:

1. Its input is the canonical typed product contract already used at runtime.
2. Its outputs are thin adapters such as CLI/MCP/API metadata, SDK bindings, or
   host package files; business logic remains in application/domain code.
3. The same input produces byte-identical output across clean builds.
4. Unknown fields, duplicate IDs, illegal grants, unsafe paths, and unsupported
   host capabilities fail before output is published.
5. Generated files contain no secrets, private paths, timestamps, machine-local
   cache state, or developer environment assumptions.
6. Direct conformance tests compare product semantics, not just snapshots.
7. The generator deletes more duplicate production authority than it adds.
8. The generated path works under ordinary stock Cargo on supported platforms.

Build scripts must not perform network access, probe mutable host installations,
depend on a developer-only Cargo wrapper, or write outside Cargo's declared
output directory. Runtime host probing and installation belong to typed daemon
or application operations, not compilation.

## Validated identities

Private declarative macros may keep construction and deserialization validation
identical for nominal scalar types. Keep them local to the owning domain module.
Do not export a generic newtype framework or move domain validation into macro
metadata.

## Operation and surface catalogs

CLI, MCP, HTTP, SDK, and dashboard bindings may project from one ordinary typed
operation catalog. Start with structs, enums, functions, and table-driven tests.
The catalog may own stable names, request/response schemas, effects, availability,
and binding metadata. It must not own business logic, authorization decisions,
storage, rendering, analytics, or transport execution.

Product bindings generated from this catalog are allowed. Generated plan views,
PR registries, rewrite status, or implementation ownership maps are not.

## Dynamic workflows

PR17 dynamic workflows are typed runtime product data submitted through the
public application/SDK contracts. The daemon validates them and uses the shared
operation, scheduler, history, lease, effect, artifact, event, and store
authorities. JavaScript/TypeScript may call an SDK but is not a workflow
definition language or execution runtime.

They must not:

- parse plan Markdown or this roadmap;
- dispatch or track V2 implementation PRs;
- act as a developer plan executor, progress tracker, or completion ledger;
- create a second scheduler, database writer, event log, task identity, or
  authorization path;
- ship Claude-specific workflow JavaScript as a canonical implementation.

## Procedural-macro admission

A procedural macro may be proposed only after a minimized ordinary-Rust spike
shows the invariant cannot be preserved with maintained libraries and normal
types. The proposal must:

- leave Rust structs/enums as the sole field and variant authority;
- delete at least 500 non-test lines or 65% of the repeated implementation;
- retain at least 300 net deleted production lines after support code;
- preserve actionable nested errors and existing valid wire behavior;
- reject duplicate keys, unknown fields/variants, and illegal tagged fields;
- cover supported Serde forms and compile-fail unsupported forms;
- add no runtime reflection or metadata registry;
- add at most 5% clean-build cost and 200 ms unrelated incremental-build cost;
- own no persistence, rendering, transport, authorization, digest, or business
  behavior.

At most one derive crate and one sealed derive may be admitted. Custom attribute
macros remain out of scope.

## Rejected targets

Do not add custom generation for:

- sanitization, trust, transaction, recovery, or authorization logic;
- error taxonomies served by maintained derives;
- Markdown rendering or task-plan parsing;
- small enum conversions, control flow, test DSLs, or schema mirrors;
- architecture inventories, compatibility inventories, baseline packets,
  source snapshots, plan receipts, slice DAGs, rewrite workflows, or generated
  implementation declarations;
- any mechanism that models delivery instead of delivering product behavior.

## Review evidence

Every generator or macro change must state the invariant it owns, duplicate
authority removed, ordinary-Rust alternative, production lines/dependencies
added and removed, build/diagnostic/API costs, deterministic and cross-platform
tests, and deletion boundary. Without that evidence, use ordinary Rust.
