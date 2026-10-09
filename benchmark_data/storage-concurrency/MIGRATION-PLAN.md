# SQLite to native Turso migration plan

Audited 2026-10-09 at TraceDecay baseline `5fe50528b3`. Target: native Turso
**0.8.0**, source commit `2829ee1662bd01d79f60c2c170af684a2b50af86`.
This is an implementation plan, not an installed backend or a completed data
migration. The default SQLite store remains authoritative.

## Executed evidence and immediate blocker

The isolated native probe uses the official `pyturso==0.8.0` wheel, SHA-256
`62bf87c6966f6b1d9c8b71ae060779758cbc55ee649567cef4399a15c7a4c65b`, checks
engine version, and exercises `BEGIN CONCURRENT` in MVCC mode. Its full results
are in [results.json](turso-prototype/results.json). The wheel's source commit
has not been independently attested; the pinned source compatibility audit and
the executed wheel are distinct evidence.

Installing the **actual production ledger DDL** fails in both WAL and MVCC
modes during `exact_schema_install`:

```text
DatabaseError: Parse error: CREATE INDEX on WITHOUT ROWID tables is not supported
```

The first incompatible production statement is
`td_runtime_writer_outbox_ordering_v1` at
`crates/tracedecay-rusqlite-runtime/src/ledger/schema.rs:57`, on the outbox
table ending in `WITHOUT ROWID` at line 55. Earlier checkpoint and idempotency
tables use the same layout at lines 16 and 36; the inbox does too at line 91.
Removing only that first index would neither make later indexes work nor make
checkpoint UPDATE, outbox state transitions, pruning, and UPSERT safe. The
pinned [Turso compatibility matrix](https://github.com/tursodatabase/turso/blob/2829ee1662bd01d79f60c2c170af684a2b50af86/COMPAT.md)
documents the restricted layout and missing FTS5/hooks. It also disallows
mixed SQLite/Turso access to one database across processes.

On deliberately reduced **ordinary ROWID** tables, the probe held two begun
and mutated transactions concurrently. Independent rows both committed; a
pinned read snapshot survived its peer's commit. The singleton checkpoint
case instead failed during checkpoint mutation with `Write-write conflict`.
Full rollback discarded the tentative effect and receipt; a fresh transaction
retry produced contiguous receipts `[1, 2]`. Fresh-process reopen, integrity,
idempotent replay, and conflicting replay checks passed. These are not a
complete TraceDecay schema, production writer, throughput result, forced
crash test, or power-loss guarantee.

The follow-up probe also installs the actual **four-ledger schema adapted to
ordinary ROWID**, removing only its four layout suffixes and preserving every
column, constraint and index. SQLite 3.53.4 with the original schema and native
Turso 0.8.0 with the adaptation reject the same 34 isolated invalid writes and
pass rollback, stale checkpoint CAS, contiguous retry receipts, representative
INSERT/UPDATE/UPSERT/DELETE, and checked fresh-process reopen. This lane is
sequential per engine; overlap is proved only by the reduced model above. It
does not run Rust receipt codecs, binding/policy validation, or full domain
protocols. Native process handoff in this Python probe required releasing SDK
references and garbage collection; the precise retention mechanism is unproven.
Actual Rust cursor/statement/snapshot lock release remains an adapter check.
See [verification.txt](turso-prototype/verification.txt) for the final uncached
execution. Schema adaptation is now demonstrated for these four tables; the
complete schema, security and search migration is still unfinished.

## Semantics that remain mandatory

The existing same-shard writer is an ordering authority, not just a connection
wrapper. `writer/transaction.rs:102` starts an immediate outer transaction,
uses request savepoints, verifies authority before commit, calls the original
request probe's `try_begin_commit` at line 204, and publishes committed
watermarks only after the outer commit. `ledger/checkpoint.rs:45` obtains the
next sequence and line 124 verifies exactly one compare-and-swap update.
`ledger/sqlite.rs:106` rejects noncanonical persisted JSON. Paths in this
paragraph are under `crates/tracedecay-rusqlite-runtime/src`.

Preserve these behaviors through the cutover:

- One durable receipt sequence per shard/incarnation, with no gaps from a
  rolled-back transaction, replay, failed validation, or conflicting request.
  Preserve authority-epoch fencing, exact original replay receipts, digest
  conflicts, and the fair admission/batch compatibility rules.
- Domain mutations, checkpoint, idempotency receipt, inbox/outbox effects,
  cursor CAS, and projection receipts share their existing atomic boundary.
  No external effect is repeated by retrying a database transaction.
- Cancellation remains request-local, unrelated requests retain their
  savepoint behavior, and a caller dropping its future does not fabricate a
  rollback or a committed receipt. Commit admission and post-commit
  uncertainty retain their actual typed states.
- Frozen watermark reads, retained snapshots, foreground and health admission,
  drain/quiescence, opened-file identity, ownership leases, maintenance
  exclusion, and dirty-marker CAS survive restart and replacement races.
- Query results retain scope, redaction, ranking, ordering, tie breaks,
  keyset pagination, temporal generation coverage, and unavailable states.
  Security denial must remain denial at compilation and execution.

Same-shard MVCC does not remove the checkpoint conflict. Begin with bounded
whole-transaction retries from fresh snapshots for the closed, replayable
operation executors. Handle conflicts from both mutation and COMMIT. Reverify
the original binding, authority, deadline, cancellation, idempotency identity,
and checkpoint on each attempt; never retry only the failed statement or
allocate sequences outside the durable transaction. Preserve the original
probe: do not reset an already-claimed commit boundary to make retries work.
`crates/tracedecay-store/src/runtime/ports.rs:31` is that probe contract. Commit
conflict behavior after this boundary must be proven with its production
implementations before automatic retry is admitted. Unknown commit outcomes
require receipt lookup/recovery, not blind replay.

If singleton conflicts erase the workload benefit, keep serialized ordered
commits for that workload. A late commit arbiter or different receipt order is
not automatically safe: its interaction with the MVCC snapshot, checkpoint
row versions, outbox ordering, and frozen watermarks must be demonstrated
before changing the protocol. Independent-shard parallelism remains useful
without that change.

## Complete caller and data surface

Port one canonical connection/transaction/query authority and wire every
production caller. Extend the existing `db::engine::{Connection, Executor,
QueryExecutor, Transaction, ReadSnapshot, Row, Value}` boundary rather than
introducing a second application-facing database abstraction. Its connection
currently carries `ExactSqlHandle` directly
(`crates/tracedecay-runtime-core/src/db/engine/connection.rs:5`). The runtime's
ledger, repository executors, and operation traits also expose rusqlite
transactions/savepoints/rows; replacing only the connection constructor leaves
these callers attached to SQLite.

| Caller/data family | Concrete starting points | Required migration work |
| --- | --- | --- |
| Writer/readers, exact SQL, admission, watermark and maintenance | `crates/tracedecay-rusqlite-runtime/src/writer.rs:510`, `writer/transaction.rs:102`, `reader/worker.rs:398`, `repository/attachment.rs:459`, `exact_sql/guard.rs:40`, `connection/mod.rs:664` | Port actual execution and guard capabilities, typed errors, snapshots, statement caches, shutdown and checkpoint policy. Install the chosen engine at registered attachment, not a parallel unmounted adapter. |
| Runtime ledgers and closed domain operations | `crates/tracedecay-rusqlite-runtime/src/ledger/schema.rs:3`, `persistence.rs:67`, `repository/mod.rs`, `work/schema.rs`, `workflow/schema.rs`, `remote/schema.rs` | Preserve rows and receipts, all constraints/indexes, cursor and lifecycle CAS, work/run/effect journals, handoff/admission/recovery state, and remote replay semantics. |
| External source authority/projection | `crates/tracedecay-rusqlite-runtime/src/repository/external_source.rs:74` | Convert all four WITHOUT ROWID tables, not only the runtime ledger. Retain receipts/frontiers/mutation digests, secondary indexes, pruning and owner-bound replay checks. |
| Registered profile, project, session, memory, code and remote mounts | `crates/tracedecay-runtime-core/src/shard_runtime/registry/attachment.rs`, `crates/tracedecay-store-runtime/src/session_registry/mounts.rs:585`, `session_registry/maintenance.rs:658`, `remote_replay_transaction.rs:397` | Migrate each registered store under its exact profile/project/shard identity; cut over lifecycle, reset/admission, discovery, recovery and shutdown callers together. |
| Global authority schemas and transactions | `crates/tracedecay-global-db/src/schema_stages.rs`, `schema_contract/invariants/triggers.rs:40`, `observation/schema.rs:182`, `git_index_transactions/schema.rs:123`, `native_integration/schema.rs:119` | Port final-shape admission and exact SQL schema fingerprints, observation/cursor/retrieval authority, integration journals, immutable receipts and audit invalidation triggers. No resetting populated authority as a migration shortcut. |
| Memory and retrieval | `crates/tracedecay-runtime-core/src/db/memory_v2/schema/baseline.rs:72`, `crates/tracedecay-session-memory/src/fact_store/candidates.rs:125`, `crates/tracedecay-session-temporal-store/src/retrieval/queries.rs:471`, `crates/tracedecay-lcm/src/schema.rs:101`, `crates/tracedecay-sessions/src/runtime/store_access/sessions.rs:694`, `crates/tracedecay-dashboard-api/src/graph_structure_api.rs:586` | Migrate canonical rows and all dependent FTS/search/admission/render paths as described below. Include redaction/purge, temporal publication and derived rebuild behavior. |
| Graph, code index, lexical artifacts | `crates/tracedecay-graph-db/src`, `crates/tracedecay-code-index/src`, `crates/tracedecay-code-index-runtime/src`, `crates/tracedecay-query/src/retrieval/lexical/projection/artifact.rs:228` | Audit shared engine calls plus independent artifact connections. Grafeo GQL MATCH is a different engine and must not be rewritten as SQL FTS. Lexical artifact BM25 is implemented in Rust, not SQLite FTS5. Preserve content-addressed publication, mmap/read budgets and resumable staging. |
| Direct opens, immutable snapshots, backup/folding and diagnostics | `crates/tracedecay-runtime-core/src/sqlite_read_snapshot.rs:211`, `sqlite_snapshot_materialize.rs:38`, `sqlite_snapshot_connection.rs:18`, `crates/tracedecay-query/src/retrieval/lexical/projection/artifact/reader.rs:460`, `crates/tracedecay-rusqlite-runtime/src/content_digest.rs:56`, `remote/identity.rs:96`, `crates/tracedecay-cli/src/commands/profile_storage.rs:198` | These bypass ordinary registered dispatch. Route owned Turso stores through the migrated engine. Replace SQLite backup/WAL folding/SHM assumptions with a verified snapshot/export/recovery path. Preserve logical digest byte/type encoding and reset refusal behavior. |
| Foreign SQLite ingestion and health | `crates/tracedecay-sessions/src/runtime/hosts/opencode.rs:547`, `crates/tracedecay-maintenance/src/retention/storage_report.rs:455`, `crates/tracedecay-rusqlite-runtime/src/connection/mod.rs:603` | Foreign providers remain externally owned SQLite files. Keep a clearly foreign read-only SQLite adapter if required; never use it to open an owned Turso/MVCC file. Preserve no-sidecar diagnosis, source identity and cancellation. |
| Composition and caller capabilities | `crates/tracedecay-application`, `tracedecay-daemon-service`, `tracedecay-session-runtime`, `tracedecay-project`, `tracedecay-agent-hosts`, `tracedecay-maintenance`, `tracedecay`, `tracedecay-cli` | Migrate registered callers and imports, CLI reset/doctor/export commands, MCP/dashboard/hooks and background jobs. Remove old owned-store routes and dependencies only after the final production caller moves. |

Include connection-local projection state in schema adaptation:
`temp.observation_projection_output_state` and
`temp.observation_projection_output_state_meta` at
`crates/tracedecay-global-db/src/observation_projection/state.rs:569` and 580;
`temp.observation_projection_rebuild_retained_outputs`,
`temp.observation_projection_rebuild_cleared_outputs`,
`temp.observation_projection_predecessor_cleared_outputs`, and
`temp.observation_projection_rebuild_preexisting_outputs` at
`observation_projection/rebuild.rs:1927`, 1932, 2035 and 2351. These temporary
tables are execution state, not durable rows to export, but their constraints,
connection affinity, reset and transaction behavior must port together with
the persisted `observation_source_presence` authority at
`observation_projection/schema.rs:294`. Recreate
temporary state through production callers on the migrated connection.

This is a caller-family audit, not a claim that every inline test open is a
production bypass. Compilation and behavior of all listed families must be
checked after the cutover; file-name scans alone are not acceptance evidence.

## Search and trigger contracts

There are four production FTS5 virtual tables, all external-content indexes.
None explicitly selects a tokenizer, so preserving FTS5 behavior includes the
default tokenizer and its phrase/term/column-filter rules.

| FTS index | Maintaining authority | Observable query semantics |
| --- | --- | --- |
| `memory_v2_assertion_payloads_fts(content)` | `crates/tracedecay-runtime-core/src/db/memory_v2/schema/baseline.rs:72`; insert/delete triggers at 77/82 and immutable payload update trigger at 88 | Memory candidates at `crates/tracedecay-session-memory/src/fact_store/candidates.rs:125`, duplicate detection at `fact_store/crud/add.rs:86`, dashboard at `crates/tracedecay-dashboard-api/src/graph_structure_api.rs:586`. MATCH joins payload rowid to eligible current owner/project facts; bm25 ascending with timestamp/ID tie breaks. `fact_store/scoring.rs:205` normalizes negative FTS5 scores and mixes them with other relevance components. Different rank signs/scales change results. |
| `lcm_raw_messages_fts(index_text, role, kind, model, tool_names)` | `crates/tracedecay-lcm/src/schema.rs:101`; insert/delete/update triggers at 106/111/121 | General sessions search uses BM25 weights **10/2/1/1/1** at `crates/tracedecay-sessions/src/runtime/store_access/sessions.rs:694`; workflow detection uses MATCH at `runtime/workflow/workflow_state.rs:45`. LCM grep at `crates/tracedecay-lcm/src/query/grep.rs:262` confines matching to body through the canonical column filter, sanitizes phrases/special characters at `query.rs:749`, and retains LIKE fallback for CJK, emoji and risky punctuation. Role/model/tool metadata must not become body hits. |
| `session_occurrences_fts(index_text)` | `crates/tracedecay-global-db/src/session_temporal_schema.rs:651`; canonical insert/delete/update triggers in `schema_contract/invariants/triggers.rs:1206` | All occurrence FTS query arms in `crates/tracedecay-session-temporal-store/src/retrieval/queries.rs:471` onward preserve source provider, session, generation, body-length constraints, time/keyset order, membership and current-coverage filters. These queries sort by temporal authority, not BM25. |
| `session_summary_nodes_fts(summary_text)` | `crates/tracedecay-global-db/src/session_temporal_schema.rs:656`; insert/delete/update triggers in `schema_contract/invariants/triggers.rs:1235` | Summary and stitched query arms in `crates/tracedecay-session-temporal-store/src/retrieval/queries.rs:512` onward preserve publication availability, current/historical coverage and provider attribution. LCM summary grep at `crates/tracedecay-lcm/src/query/grep.rs:320` uses the same table. |

FTS admission and doctor depend on exact schema and shadow tables, not only
MATCH returning hits: `crates/tracedecay-global-db/src/session_temporal_schema/admission.rs:19`
inventories FTS shadow names; `crates/tracedecay-session-temporal-store/src/doctor_health.rs:422`
checks docsize membership and MATCH probes. Projection receipt validation also
joins the FTS virtual table at `projection/receipts.rs:903`. Port these with
the search authority and remove obsolete FTS-shape checks in the same slice if
an approved new format replaces them.

The audit found **no production SQL invocation of FTS5 `snippet()`** in this
checkout. Its absence in Turso is a compatibility gap, but is not the concrete
current snippet caller blocker. TraceDecay builds match-centered/bounded
snippets in Rust (`crates/tracedecay-lcm/src/query.rs:932`,
`query/session.rs:294`) and applies canonical derived-content rules
(`retrieval_content.rs:28`). Preserve these and hydration/redaction authority;
do not substitute a new engine's highlights for canonical content.

Do not drop ordinary triggers merely because the target supports basic trigger
syntax. Besides FTS maintenance, triggers enforce immutable facts, assertions,
payloads, receipts, anchors and lineage; owner/identity binding; permitted
redaction/purge transitions; projection/audit invalidation; dirty rollups;
summary convergence; and integration/workflow state transitions. Canonical
families include `crates/tracedecay-runtime-core/src/db/retrieval_anchor_schema.rs:104`,
`db/memory_v2/schema/final_authority.rs:81`,
`crates/tracedecay-global-db/src/schema_contract/invariants/triggers.rs:113`,
`observation_projection/schema.rs:111`, `observability_rollup/schema.rs:70`,
`stack_delivery.rs:180`, and `crates/tracedecay-lcm/src/summary_convergence.rs:68`.
Differential tests must cover abort/rollback and invalidation, including writes
that are legal SQL but forbidden domain transitions.

## Security, strict SQL guards and cache invalidation

`crates/tracedecay-rusqlite-runtime/src/connection/mod.rs:930` installs
reader/writer/maintenance authorizers. Reader connections are query-only;
ordinary writers cannot arbitrarily attach databases, change destructive
schema, create virtual tables/triggers or load extensions. Exact SQL has a
separate stricter transaction-control and database-lifecycle policy at
`exact_sql/guard.rs:40`: temporary tables/indexes are allowed, temporary
triggers/views are denied, ATTACH/DETACH require the actor's fixed lifecycle
capability, and pragmas are allowlisted.

The authorizer at `exact_sql/guard.rs:62` captures table names from top-level
authorized INSERT actions (`accessor.is_none()`). Its update hook at line 89
marks `applied` for **any INSERT into those captured tables**; the hook has no
accessor filter. Trigger inserts into a different, uncaptured table do not
mark it, but a same-table trigger INSERT can. `exact_sql/mod.rs:1071` uses that
fact to publish a logical last-insert-rowid
for a handle sharing the writer connection. Replacing it with total changes,
SQL text matching, or a trigger's last rowid changes behavior for ignored
inserts, trigger effects and other callers. Preserve the current behavior
through a verified native execution result or engine hook, including a
differential case where an ignored top-level INSERT invokes a trigger that
successfully inserts into the same captured table.

Progress handlers enforce execution deadlines, shutdown and repeatedly
checked authority (`exact_sql/guard.rs:118`), and request-local write
cancellation (`connection/mod.rs:1074`). Cleanup clears handlers/update hooks
and restores the canonical authorizer on success, error and unwind. Prepared
statements are cached (`exact_sql/mod.rs:1044`, `1260`), while the guard changes
per operation. Authorization is a compilation boundary; the
[SQLite authorizer contract](https://www.sqlite.org/c3ref/set_authorizer.html)
also requires the right policy during automatic reprepare. The target must
invalidate/revalidate prepared programs when guard authority or schema
changes; a program compiled under exact-SQL privileges cannot later execute
under reader/ordinary-writer privileges. Test cached execution after policy
restoration, after DDL, and after authority revocation.

Native 0.8.0 does not provide drop-in parity for these hooks. Resolve this by
adding verified engine enforcement/observation support, or replacing the
entire controlled SQL boundary with engine-supported structured operations
that enforce equivalent policy. Do not use a hand-written SQL token filter,
ignore failed hook registration, disable guards, or claim successful opening
establishes security parity. The hook gap is an engineering blocker; weaker
authority is not a default migration choice.

## Engineering sequence and acceptance

1. **Make the complete target schema executable.** Use ordinary ROWID tables
   with equivalent NOT NULL, composite primary/unique keys, CHECK and
   foreign-key constraints. Materialize the implicit NOT NULL constraint of
   every WITHOUT ROWID primary-key column, including composite keys whose
   declarations appear nullable. Simply removing the layout token can allow
   NULL keys in ordinary composite-primary-key tables. Ordinary INTEGER PRIMARY
   KEY also auto-generates a rowid from NULL, so preservation requires
   differential NULL-insert tests rather than assuming a NOT NULL declaration
   makes that conversion equivalent. This changes physical layout, not identity
   or receipt values. Include external source and temporary projection tables,
   every secondary index and
   final-shape validator; retain explicit payload/occurrence rowids because
   their joins depend on them. Compare complete schema installation,
   mutation/UPSERT/delete/prune behavior, null rejection, uniqueness,
   foreign-key failures, trigger aborts and query plans. Pin the Rust engine
   and build inputs; do not infer C or Python API parity from a Rust version.

2. **Implement the canonical execution backend completely.** Move actual
   connection, transaction/savepoint, query/value/error and prepared-program
   behavior through the current engine boundary. Wire production repository
   executors and registered callers. Provide authorizer-equivalent guards,
   INSERT observation, cancellation/interruption, exact limits, file identity,
   immutable diagnosis and MVCC recovery before selecting the target engine.
   Replace SQLite-specific WAL checkpoint/page-size/busy/VM telemetry with
   truthful target outcomes; preserve budgets and typed unavailability rather
   than pretending SQLite counters still exist.

3. **Complete search parity or implement an explicitly approved search change.**
   Run the four full FTS schemas and all query/render/doctor/admission paths
   against a corpus covering Unicode tokenization, quoted phrases, column
   filters, infix/LIKE fallbacks, weighted ranks, duplicate detection,
   temporal joins, stable ties, pagination, redaction and purge. Rebuild derived
   indexes solely from canonical eligible content within the same publication
   authority. Do not publish a mount whose exact reads work but normal
   retrieval is unavailable.

4. **Introduce overlapping transactions only where measured.** Keep fair
   admission, health capacity, shard identity and bounded concurrency. Prove
   overlapping begun **and mutated** same-shard transactions, then measure
   the full ledger/checkpoint protocol under disjoint and conflicting writes,
   concurrent snapshots, search maintenance, retries and cancellation. Report
   retries/aborts, throughput and p50/p95/p99 including admission and full
   retry time. A queue accepting multiple callers is not overlapping database
   execution. Do not assume independent-row probe throughput predicts stores
   that update the singleton checkpoint every operation.

5. **Perform a supported data conversion under exclusive registered authority.**
   Close admission and drain all exact handles/read snapshots/background
   workers for the exact store family; fence other processes and old engine
   access. Use a consistent source snapshot, never a live main-file-only copy
   omitting WAL or MVCC state. Stream every canonical table and original
   ledger JSON/receipt/digest into a fresh target staging file; validate typed
   rows, constraints, watermarks, outbox/inbox identities, UTF-8/value fidelity
   and logical content before publication. Invalid SQLite TEXT bytes are a
   known target compatibility issue: fail with an exact diagnostic instead of
   silently replacing bytes. Rebuild derived search/artifact data through
   canonical publication, and preserve their row identity/coverage links.

   Extend the existing registered admission/initialization and migration
   authority for conversion state, file identity and recoverable switch
   phases. Fsync target and containing directory; publish the engine/format
   authority and new active file coherently; reopen under target admission
   before allowing clients. A crash before publication retains the sole
   active source; a crash after publication resumes the target. Recovery must
   distinguish these states without exposing two writable authorities. Any
   necessary staging is removed after completion; do not retain a rollback
   database, rename the source to an indefinite backup, or delete the sole
   active durable copy during cleanup. SQLite and native MVCC processes may
   not share one active file. This plan authorizes no production conversion.

6. **Verify and finish the cutover in one delivery slice.** Test interruption
   before admission, while opening, during SQL, during retry and around commit;
   rollback and revoked authority; same-key conflicts; contiguous receipts;
   old snapshots/new writes; cross-shard independence; replacement identity;
   shutdown; fresh-process reopen; forced process termination at switch and
   transaction boundaries; remote replay; redaction/search/doctor; and every
   registered profile/project/session/worktree journey. Run final Bazel
   build/lint/focused and full relevant suites, then the real-file benchmarks
   on the final backend. Delete obsolete owned-store SQLite paths, shadow
   migration routes and dependencies. Foreign SQLite readers may remain with
   explicit ownership boundaries. No feature flag should advertise incomplete
   production behavior.

## Smallest outstanding decision and blockers

ROWID layout with equivalent constraints, complete backend wiring and retries
that honor existing receipts are semantics-preserving engineering choices.
They do not require inventing a new product contract. Security, transaction
ordering and redaction guarantees remain mandatory.

The smallest product/data decision is **whether exact existing search
behavior must remain compatible** or whether an explicitly specified new
search contract may replace it. Under the current instruction to preserve
search, exact behavior is the requirement: native 0.8.0's different FTS API
is not an authorized substitution. The viable path is engine-level FTS5 parity
or another proven compatible implementation before cutover. If changed search
is desired, first present concrete corpus differences, ranking/tokenization
rules, duplicate-detection consequences, pagination effects, index conversion
and redaction/purge behavior for approval. Keeping a hidden SQLite search
sidecar would add a second transactional/publication authority and does not
resolve a complete native-only migration.

Current blockers are therefore complete schema adaptation, FTS5 search
compatibility, hook/security and cached-program parity, complete native
transaction/cancellation integration, owned-store snapshot/backup/recovery
conversion, and a verified full data/caller cutover. No production backend
adapter or migration was created by this plan. The probe establishes real
native concurrent transactions and exposes the singleton conflict; it does
not establish that replacing TraceDecay's current durable engine is ready or
beneficial.
