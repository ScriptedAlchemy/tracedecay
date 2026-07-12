<!-- Generated from architecture-boundaries.toml; do not edit. -->
# V2 Release and Deletion Policy

## Release tiers

1. Domain contracts and registries.
2. Store, capture, projectors, code index, and capability catalog.
3. Query and policy.
4. Application and generated public contracts.
5. Root adapters and official clients.
6. Dashboard compositions and renderers.

A tier releases only after all lower-tier contract, architecture-lint, migration, privacy, and hermetic-test gates pass.

## Deletion waves

1. **contracts-and-inventory:** assign one owner/disposition/deletion PR to every V1 surface, store, schema, adapter, and duplicate semantic implementation.
2. **shadow-and-backfill:** run redacted corpus, migration, replay, semantic parity, scale, and restore receipts while V1 remains authoritative.
3. **surface-cutover:** atomically select V2 by release cell; preserve bounded V1 rollback routes and reject incompatible stale clients explicitly.
4. **v1-retirement:** after the rollback window, delete zero-traffic adapters, V1 writers/readers/routes/assets/dependencies and publish negative-code/footprint receipts.

## Mandatory gates

- Compatibility: every retained behavior/action/error/data family has a generated disposition and passing parity receipt.
- Rollback: non-disposable V1 sources and the old shell remain read-only until restore and downgrade drills pass.
- Removal: inventory, shadow parity, cutover, rollback-window, stale-client rejection, adapter-zero-traffic, and negative-code receipts all pass.

No compatibility adapter accepts a new caller. A failed or partial gate leaves the previous release cell authoritative.