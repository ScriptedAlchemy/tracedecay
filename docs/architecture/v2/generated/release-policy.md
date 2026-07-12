<!-- Generated from architecture-boundaries.toml; do not edit. -->
# V2 Release and Deletion Policy

## Release waves

1. `contracts-and-inventory`
2. `shadow-and-backfill`
3. `surface-cutover`
4. `v1-retirement`

## Mandatory gates

- Compatibility: Every V1 read, write, action, error, and retained-data family has a generated disposition plus passing semantic parity and migration receipts.
- Rollback: Keep non-disposable V1 stores and the old shell read-only until the release-cell rollback window closes and a restore drill succeeds.
- V1 removal: Remove a V1 route only after inventory ownership, shadow parity, cutover, rollback-window, stale-client rejection, and negative-code receipts pass.

## Deletion waves

- **D0 (PR 22A):** semantic-duplicates
- **D1 (PR 10):** store-and-capture-writes
- **D2 (PR 16):** query-forks
- **D3 (PR 23):** policy-forks
- **D4 (PR 24):** application-transport-forks
- **D5 (PR 32):** legacy-dashboard
- **D6 (PR 37):** v1-live-system
