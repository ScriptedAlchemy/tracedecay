# Native storage implementation and caller cleanup

The native Rust implementation is an explicit experiment with pinned Turso
0.8.0 at `2829ee1662bd01d79f60c2c170af684a2b50af86`. SQLite remains the
registered production store. The target is fresh native databases; existing contents need no conversion or
preservation. The inventoried operator databases have not been reset.

## Replaced inserted-ID API

Analytics, graph publication, observations, and the clone fixture obtain their
inserted IDs with `INSERT ... RETURNING`. The ID belongs to the statement that
performed the insert. The engine and ExactSql `last_insert_rowid` methods,
shared atomic ID state, result fields, update-hook tracker, and publication
code are deleted. There is no compatibility alias.

`execute_returning` uses the admitted writer operation. An analytics caller
that stops waiting does not cancel an admitted insert. Existing tests verify
this behavior, batch rollback, and input ordering of returned batch IDs.
Authority and SQL guards continue to apply throughout execution.

One-shot `execute_returning` owns a deferred transaction in both backends. It
commits after the returned rows are fully materialized and rolls back on error.
Independent source review found that dropping an unfinished successful
`RETURNING` statement otherwise commits its writes even when the Rust caller
returns a row or byte budget error. Real-file regressions exceed 10,000 rows
and 64 MiB, check zero mutations through a reader and reopen, and verify a
subsequent write. Ordinary queries and retained transaction statements keep
their existing semantics. The before/fix/after evidence is in
[returning-budget-verification.txt](returning-budget-verification.txt).

## Native execution and ledger

`tracedecay-turso-runtime` implements bound values, guarded SQL, deadlines,
cancellation, transactions, savepoints, snapshots, checkpoint operations,
and Unix descriptor-bound main-file identity. Its SQL policy and function
resolver reject extension loading, arbitrary transaction control, temporary
triggers, attachments, and protected pragmas. Request and returned-row budgets
bound client materialization.

The native ledger uses four ordinary ROWID tables with the canonical keys,
checks, and indexes. Checkpoint compare-and-swap, canonical receipt codecs,
idempotency, inbox/outbox bookkeeping, and bounded revoked-receipt pruning
share the transaction. The native writer uses per-request savepoints and the
original commit probe. It verifies Full synchronization before ledger setup.
Diagnostics publication and history are implemented. The other 17 repository
payload variants explicitly return `Unsupported`.

MVCC is an explicit writer-owned opt-in. Real-file Rust tests open two
transactions before writing independent rows and retain an older reader
snapshot across their commits. A shared-checkpoint conflict requires rollback
and a fresh transaction. Retrying produces sequence two and survives reopen.
The native ledger writer remains serial. These tests do not claim overlapping
production receipt commits or native throughput improvement.

The existing runtime-core engine owns backend dispatch, row/value conversion,
retained transactions, and snapshots. Native foreground and background reader
admission uses the canonical reader budget. Ordinary query cancellation and
admitted-write continuation have separate, tested behavior.

## Migrated search callers

Memory candidates and duplicate detection, LCM raw and summary grep, session
search and unfinished-session detection, temporal retrieval, and dashboard
fact matching select SQL from the attached backend. Native ranked searches
materialize standalone indexed hits and scores before joins. Canonical scope,
owner, eligibility, generation, and keyset filters precede the final limit.
Hydration still uses the owning store's canonical content authority.

The four native indexes cover memory payloads, raw messages, occurrences, and
summary nodes. Raw-message weights are 10/2/1/1/1. LCM body search supplies all
five indexed columns and confines terms to `index_text`. Native matching uses
quoted exact terms instead of SQLite single-token prefix expansion. Native
scores have the opposite sign and direction. Candidate ranking can differ,
including the memory duplicate check's existing eight-candidate cap.
These search changes were explicitly approved for the native experiment.

Only SQLite FTS synchronization triggers are replaced by native indexing.
Ordinary invariant and privacy triggers remain. Native doctor results are
partial because a full posting-membership check is unavailable.

## Blocked production cutover

Pinned Turso accepts an unknown `PRAGMA secure_delete` without implementing it.
The historical strict privacy probe fails. The user subsequently approved
logical deletion without SQLite deleted-page scrubbing. Native memory schema
installation and both purge callers now skip that setting; the raw adapter
pragma still returns `Unsupported`. SQLite retains its setting and readback.
Fresh native memory schema installation and reopen pass. Native final-memory
admission now refuses because exact native schema inventory verification is
unfinished. [SECURE-DELETE.md](SECURE-DELETE.md) records the approved limitation
and the unverified native production purge journeys.

[DATABASE-RESET-INVENTORY.md](DATABASE-RESET-INVENTORY.md) records aggregate
inventory counts. The exact operator reset targets remain local outside the
published repository. No operator database was deleted and no daemon was
stopped. Action-time exact-target confirmation remains required.

A native registered physical attachment and ordered writer actor are also
unfinished. Ordinary native facade operations use separate connections and
do not reproduce the production ExactSql writer's shared command ordering.
There is no native default factory, registered mount, or advertised complete
backend. SQLite attachment, snapshot/export, maintenance telemetry, recovery,
and the remaining closed operation executors still need migration.

Windows descriptor adoption is unsupported. Complete sidecar confinement,
hard bounds on internal materialized FTS work, all eight temporal query
variants at runtime, and native crash or power-loss recovery remain unproved.

## Pstack workflow

The requested poteto-mode was read from the current local pstack-codex
checkout at `7a3ca8698ebf822dcf5351e26db6fd9a014cf94f`. The personal plugin
installation was not changed. Its figure-it-out workflow separates native
file behavior, migrated caller journeys, API deletion, and final workspace
checks. Independent reviewers inspect authority, ordering, search, and the
decision trail. The settled execution boundary does not need a second design
competition. The user later authorized a dedicated branch, draft PR, and
remaining-work issue if full replacement cannot be completed. Merge and
deployment remain unauthorized.

The throughput checkpoint preserves privacy and identity before cutover,
assigns separate source ownership, serializes shared Bazel operations, and
checks each bounded unit. Model the Domain led to statement-owned rows and
per-shard receipts. Migrate Callers Then Delete Legacy APIs removed ambient
IDs without aliases. Prove It Works requires real-file engine and caller
tests. Explain the Number classifies the earlier three fastbuild shard runs
as inconclusive for choosing an engine or claiming a speedup.

The independent review found missing rank columns in native LCM grep. Both
queries now materialize scores and use the correct backend ordering. Tests
exercise raw and summary ranking across all three sort modes, with scope
filters before the limit. A separate LIKE fixture was bounded to a session
after the existing policy correctly refused its unbounded request.

See [decisions.tsv](decisions.tsv) for decisions and corrections,
[NATIVE-SEARCH.md](NATIVE-SEARCH.md) for engine search findings, and
[the earlier measurements](../MEASUREMENTS.md) for observed SQLite workloads.
The historical [migration plan](../MIGRATION-PLAN.md) predates this implementation.
Final verification is recorded separately from those earlier checks.
