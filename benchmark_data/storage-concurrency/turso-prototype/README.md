# Opt-in Turso transaction experiment

This standalone Bazel module loads the **actual Turso Rust engine** through the
official `pyturso` 0.8.0 native wheel. The wheel URL and SHA-256 are pinned in
`MODULE.bazel`; its embedded engine version is asserted at runtime. The wheel
currently targets macOS arm64 and CPython 3.10 or newer. The observed run used
CPython 3.11.15. Pass an explicit interpreter because Bazel's test PATH may
otherwise choose an incompatible system Python.

From this directory:

```sh
bazel test //:turso_probe \
  --test_env=TURSO_PYTHON=/path/to/python3.11 \
  --nocache_test_results --test_output=all --repo_env=DO_NOT_TRACK=1
```

The observed [results.json](results.json) and [verification.txt](verification.txt)
record the exact emitted result and successful command. These are evidence from
one bounded run, not performance measurements.

The module is excluded from the main workspace build. It has no credentials,
remote database calls, production database access, or default storage changes.
Its temporary real database files are removed when the run ends. The SHA-pinned
wheel downloads during Bazel repository setup, before the local test runs.

`BEGIN CONCURRENT` on two separate connections leaves both write transactions
active after both have mutated independent, explicitly keyed rows. Both then
commit, and a pinned snapshot retains its previous view. This proves overlapping
database transactions; the calls are coordinated sequentially and do not measure
parallel CPU execution or application throughput.

A reduced ROWID publication model then co-commits a domain effect, a singleton
checkpoint compare-and-swap, and an idempotency receipt. Its overlapping stale
transaction produces a `Write-write conflict` at the checkpoint `UPDATE`;
rollback discards its domain mutation and publishes no receipt. A complete fresh
transaction retries successfully, leaving checkpoint 2 and receipt sequences
1 and 2. Actual replay and different-digest requests pass through the reduced
admission function before mutation. A fresh process reopens the same file,
rechecks receipts/digests/idempotency, and runs `PRAGMA integrity_check`.

The model is **not a TraceDecay storage adapter**. Its ROWID tables are declared
separately from the compatibility probe. `turso_ledger_schema.sql` contains the
exact production runtime ledger DDL from
`crates/tracedecay-rusqlite-runtime/src/ledger/schema.rs`, with one provenance
comment added. Both WAL and MVCC probes reject that exact schema at installation:
`CREATE INDEX on WITHOUT ROWID tables is not supported`. This expected,
specifically asserted compatibility failure is emitted in the JSON result;
unknown errors fail the test. The prototype does not remove those indexes,
migrate the production schema, replace FTS5, or emulate rusqlite hooks.

The separate `ledger_rowid_schema.sql` experiment contains all four actual
runtime ledger tables and all five explicit indexes. Only the four
`WITHOUT ROWID` suffixes are removed; a runtime byte comparison checks that
the fixture preserves every original column, explicit `NOT NULL`, composite
primary key, unique index, and `CHECK` constraint. Production source and the
original negative compatibility probe remain unchanged.

Real SQLite files using the original schema and native Turso MVCC files using
the adapted schema reject the same 34 forbidden writes: 14 null primary-key
components, 14 invalid checked values, four duplicate primary keys, and two
collisions on unique effect indexes. Each rejection is checked for the expected
constraint error and absence of mutation. Both engines also execute checkpoint
compare-and-swap and `INSERT OR IGNORE` patterns from the ledger, roll back
mutations in all four tables, reject a stale checkpoint update, and retry with
contiguous receipt sequences 1 and 2. The actual idempotency table participates
in replay and different-digest admission. An outbox conditional update,
additional outbox/inbox UPSERTs, and deletes persist through a fresh process
reopen with `integrity_check=ok`.

These are SQL storage checks with representative JSON strings. The prototype
does not run Rust receipt decoding, binding validation, authority policy,
pruning policy, or the full outbox/inbox protocol. The adapted four-ledger probe
does not add another overlap proof; the independent-row and singleton-checkpoint
experiments above establish overlapping transaction lifetimes separately.
Each null or checked-value probe deletes that table's seed inside its
transaction before inserting the otherwise valid candidate, so seeded key
collisions cannot mask the intended constraint; rollback restores the complete
four-table baseline. Duplicate-key and unique-effect probes retain their seeds.
Before process handoff the test closes cursors, drops the SDK connection, and
collects unreachable Python references. In this constraint-heavy probe,
connection close alone initially left the native file locked; dropping the
references and collecting allowed the checked reopen to succeed. The exact
retention mechanism has not been established.

A successful test establishes these bounded transaction and compatibility facts.
It does not establish production suitability, full TraceDecay SQL/search/hook
compatibility, crash or power-loss durability, or a throughput improvement.
