# PR19 Adapter and Runtime Extraction Plan

**Goal:** After dependency inversion, extract rusqlite, MCP/LSP, daemon, and
remaining root adapters without changing product authority.

## Files and interfaces

- Extend `crates/tracedecay-rusqlite-runtime` and parity crates.
- Create runtime crates only for dependency-closed adapter families proven by
  the graph: MCP transport, LSP gateway, daemon runtime, and retained DB/global
  DB adapters.
- Modify workspace manifests, root façades, distribution/package includes,
  host bundles, and cross-platform CI.

Interfaces are the already accepted application/domain/store/API ports. New
runtime crates implement them; they do not redefine requests, receipts,
project identity, one-writer authority, or migration policy.

## Tasks and tests

- [ ] Produce a dependency/disposition ledger for every root adapter module.
- [ ] Complete Plan 34 apply and PR19 cutover prerequisites before extraction.
- [ ] Extract one dependency-closed runtime family per commit.
- [ ] Preserve rusqlite parity, daemon lifecycle, MCP/LSP transport, packaging,
      installation, and host construction.
- [ ] Retire compatibility aliases only after released-data cutover evidence.

Direct tests cover real daemon startup/shutdown, MCP/LSP requests, storage
parity, migration/backup/restore, package/install, and all supported hosts.
Negative tests cover dirty migration, lease/fence loss, unavailable registry,
corrupt archive, cancellation, partial restore, stale client, and platform
feature mismatch.

## Migration, rollback, measurement, deletion

Runtime extraction follows successful atomic one-writer cutover; it is never
the mechanism for cutover. Data migrations are resumable and roll-forward,
with backup verification before promotion. Code rollback is `git revert`;
runtime fallback never re-enables dual write or reverse cutover.

Measure each adapter-private edit, root leaf edit, package build, and focused
test compile. Delete root adapters, aliases, V1 paths, and expired archives
only when exact released-data, cross-platform, package, host, and rollback
receipts satisfy `pr19-cutover-runtime.md`.
