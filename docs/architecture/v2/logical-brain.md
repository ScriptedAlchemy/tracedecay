# ADR-001: One Logical Brain and Canonical Boundaries

## Status
Accepted for V2 Phase 0. `architecture-boundaries.toml` is the machine-readable authority.

## Context
V1 places overlapping identity, session, LCM, graph, policy, tool, dashboard, and migration semantics in adjacent modules. Package count alone does not remove those duplicate meanings.

## Decision
V2 is one profile-wide `BrainId`, initially one binary and daemon authority, with at most twelve Rust packages including root, the official Rust client, and the explicitly admitted pure `tracedecay-workflow` kernel. The canonical planes are domain, sanitized capture, store, deterministic projection/code indexing, query, policy, capability catalog, application, and thin transports. Root-private hook, presentation, API, host-deployment, workflow compiler/engine, and remote-Brain transport adapters remain modules, not packages. Any package beyond the named workflow admission requires two production consumers or a demonstrated dependency/capability/publication firewall, an ADR, and a merger/deletion alternative.

One concept has one owner; one effect has one application command. Configuration, status, errors, capability bindings, visual semantics, experiment lifecycle, scheduler mechanics, and rendering use their declared shared registries/pipelines without erasing domain types. Extensions enter only through versioned, budgeted SPIs for providers, projectors, query operators/rankers, policy evaluators, renderers/lenses, executors, secret detectors, storage drivers, and host bundles. Root/V1 anti-corruption adapters accept no new callers and carry a deletion PR.

Replacement receipts report handwritten/generated code, packages, dependencies/features, public items, stores/indexes, workers, build/runtime footprint, and stored bytes separately. At parity, handwritten replacement plus adapters must be smaller than the V1 code deleted; production files target 400 lines and require a temporary waiver above 800.

## Rejected alternatives
- One giant SQLite file or one generic `core/common/services` crate: hides ownership and failure boundaries.
- A crate per plan or adapter: breaches the package ceiling without a firewall.
- Permanent V1 wrappers or transport-owned business behavior: preserve semantic duplication.
- Early process/service splitting: contracts must first support a later split without requiring it.

## Compatibility, rollback, and removal gates
Every V1 surface and state family needs an inventory disposition, owner, parity fixture, cutover receipt, rollback dependency, and deletion wave. V1 remains available behind bounded compatibility/shadow paths until semantic parity and restore drills pass. Removal requires stale-client rejection, closed rollback window, adapter traffic at zero, and negative-code/footprint receipts.

## Consequences
Architecture lint checks the DAG and transport isolation. Generated owner, dependency, and release views are review artifacts; edits go to the TOML authority.
