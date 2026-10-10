# Fresh-store reset inventory

The exact operator-profile inventory is retained locally outside the repository.
The authorized draft PR publishes only this aggregate record. No database file,
payload, credential, or exact operator-profile target list is included.

The read-only snapshot found eight SQLite main files and sixteen WAL and SHM
sidecars, totaling 37,428,228,832 logical bytes. It also found eleven Grafeo main
or sealed-generation files and three registered project shards. Selected
immutable, query-only registry metadata resolved the project links; immutable
reads omit WAL and can be stale. Every referenced path existed at inspection.

No database was deleted and no daemon was stopped. Before any real reset,
re-inventory the exact targets, obtain action-time confirmation, and use the
canonical exclusive profile lifecycle authority. Existing contents are
explicitly disposable; no conversion or preservation is required.

The current complete-wipe implementation omits two existing profile artifacts,
`user-memory.sealed` and `user-memory.verified`. Running that CLI alone would
leave those artifacts. A complete fresh-start reset must include confirmed
sidecars, graph generations, and associated state without removing unrelated
profile configuration or host source archives.

Tests use fresh isolated temporary files. They do not reset the operator
profile. Registered native cutover remains unfinished, independently of reset
permission and the approved logical-deletion contract.
