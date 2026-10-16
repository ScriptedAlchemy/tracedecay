# Storage read execution

Where a SQLite read waits, and who decides it. Callers never pick a thread:
every read reachable from async code is async at the storage boundary, and the
storage layer moves the blocking part to the native blocking pool once.

## Problem

Every SQLite read runs on a dedicated reader thread
(`tracedecay-rusqlite-runtime/src/reader/worker.rs`). The calling thread only
waits: on reader-pool admission (a `Condvar`), on the `Begin` round trip, on
the reply, and on the 5 ms end grace when the lease drops. When the caller is an
async fn on a native asupersync worker, each of those waits parks the worker
and every task queued behind it. N concurrent reads park N workers.

`exact_sql::native_thread_reader_waits()` counts the reads that start on a
native thread. On `origin/master` e5a8ede6ea the four acceptance suites read:

| suite | native reader waits | native writer waits |
| --- | --- | --- |
| `//crates/tracedecay:daemon_suite` | 1137 | 0 |
| `:mcp_suite` | 11003 | 0 |
| `:session_suite` | 4 | 0 |
| `:unit_test` | 5533 | 0 |

All but 17 of those go through `StoreRuntimeClientLease::dispatch_read`, called
inline from async fns in `tracedecay-session-memory` (external-source store and
fact store) and `tracedecay-global-db` (observation adapter). The projection
drain loop (`RuntimeExternalSourceStore::read_pending_projection`) alone
accounts for 14,300. The 17 others are `AuthorizedScopeSetSqliteStorage::read`
called from async invocation code, and the remote credential lookup that the
remote API's pre-body admission extractor runs inline.

Writes already have the right shape. `ExactSqlHandle` offers sync and `_async`
pairs, and the async forms await the writer's `async_channel` reply. The typed
write, `dispatch_submit`, is async at every layer. Reads had no async path
except the runtime-core engine `Connection`, which hopped through its own
`run_blocking` (`cx.spawn_blocking`).

Constraints:

- Per-site `spawn_blocking` wrappers at callers do not scale and drift (the
  long-poll guard series, #4066).
- Moving whole requests with `off_native_thread` loses task-local admission
  scopes and disconnect cancellation (#4078).
- The sync Work and Workflow storage ports already run on the pool through
  `off_native_worker`, one decision at request dispatch. They contribute zero
  counted waits.
- AGENTS.md: complete cutovers, no compatibility shims, typed failures.

## Usage (caller's view)

An async caller awaits. It never names a thread, a pool, or a context.

```rust
// tracedecay-session-memory/src/external_source_store.rs
let probe = Arc::new(ExternalSourceRuntimeProbe::from_control(request.control()));
let outcome = self
    .runtime
    .dispatch_read(request, probe)
    .await
    .map_err(|failure| dispatch_error("pending projection", failure))?;
```

```rust
// tracedecay-daemon-service/src/invocation/lsp.rs
let scope_set = owner.scope_set_storage?.read(scope_set_id).await.ok().flatten()?;
```

A sync port family that already runs on a pool thread (the remote protocol,
inside the API edge's `spawn_blocking`) bridges with `native_runtime::block_on`.
`block_on` refuses with a typed error on a native thread, so the bridge cannot
park a worker. On a pool thread the read's own hop runs inline, so the bridge
costs no second thread.

```rust
// tracedecay-store-runtime/src/remote_query.rs
let outcome = native_runtime::block_on(target.dispatch_read(request, probe))
    .map_err(|_| RemoteExactObservationQueryErrorV1::AuthorityUnavailable)?
    .map_err(|_| RemoteExactObservationQueryErrorV1::AuthorityUnavailable)?;
```

## Shape

The data crossing the boundary does not change. `RuntimeReadRequestV1` and
`RuntimeReadOutcomeV1` are owned and `Send`. `ExactSqlStatement` and
`ExactSqlRows` are owned and `Send`. The probe becomes
`Arc<dyn RuntimeRequestProbeV1>`, which `dispatch_submit` already takes, so the
read can outlive the caller's borrow.

```rust
// tracedecay-rusqlite-runtime/src/repository/attachment.rs
impl RepositoryRuntimePhysicalAttachment {
    /// Typed read. The only place a typed read decides where it waits: the
    /// reader pool's admission, round trips and end grace run on one
    /// blocking-pool thread, inline when the caller already is one.
    pub async fn dispatch_read(
        &self,
        request: RuntimeReadRequestV1,
        probe: Arc<dyn RuntimeRequestProbeV1>,
    ) -> Result<RuntimeReadOutcomeV1, RepositoryDispatchError>;
}

// tracedecay-runtime-core/src/shard_runtime/registry/attachment.rs
pub trait PhysicalRuntimeAttachment: Send + Sync {
    fn dispatch_read<'a>(
        &'a self,
        request: RuntimeReadRequestV1,
        probe: Arc<dyn RuntimeRequestProbeV1>,
    ) -> StoreRuntimeRegistryFuture<'a, RuntimeReadOutcomeV1>;
}

// tracedecay-runtime-core: StoreRuntimeClientLease and DatabaseRuntimeClientV1
pub async fn dispatch_read(
    &self,
    request: RuntimeReadRequestV1,
    probe: Arc<dyn RuntimeRequestProbeV1>,
) -> Result<RuntimeReadOutcomeV1, StoreRuntimeRegistryFailure>;

// tracedecay-rusqlite-runtime/src/exact_sql/mod.rs
impl ExactSqlHandle {
    pub async fn query_async(&self, statement: ExactSqlStatement, priority: OperationPriorityV1, max_wait: Duration)
        -> Result<ExactSqlRows, ExactSqlError>;
    pub async fn begin_read_snapshot_async(&self, priority: OperationPriorityV1, max_wait: Duration)
        -> Result<ExactSqlReadSnapshot, ExactSqlError>;
    pub async fn begin_health_read_snapshot_async(&self, max_wait: Duration)
        -> Result<ExactSqlReadSnapshot, ExactSqlError>;
}
impl ExactSqlReadSnapshot {
    pub async fn query_async(self: &Arc<Self>, statement: ExactSqlStatement, budget: ExactSqlStatementBudget)
        -> Result<ExactSqlRows, ExactSqlError>;
}
```

Load-bearing decisions:

- **One hop primitive.** Every async read moves its blocking body with
  `native_runtime::spawn_blocking`, which runs inline on a pool thread
  (`ON_BLOCKING_POOL`). A nested read under an `off_native_worker` handler
  therefore never queues behind its own pool thread. The engine `Connection`'s
  `run_blocking` used `cx.spawn_blocking`, which lacked that rule. It is
  deleted, and the engine awaits the `ExactSqlHandle` async forms.
- **The hop lives with the readers.** The typed read clones the attachment's
  `Arc<ReaderPool>` under its state lock and hops with the pool, not the
  attachment, so an in-flight read extends exactly what the sync read
  extended. The `ExactSqlHandle` closures are already `Arc`-shared and
  `'static`.
- **The sync typed read is deleted at the lease, client and trait.** Its only
  remaining sync caller (the remote query port) bridges with `block_on`, per
  migrate-callers-then-delete-legacy-apis. The engine `Connection` stays the
  async facade for SQL reads.
- **Sync families keep their family-level decision.** Work and Workflow
  storage ports stay sync and keep running under `off_native_worker`. The
  remote family keeps its API-edge `spawn_blocking`, which now also covers the
  pre-body credential admission. A storage type with async callers
  (`AuthorizedScopeSetSqliteStorage`) exposes async methods: reads through
  `query_async`, the compare-and-swap through the writer's
  `begin_immediate_async`.
- **The counter stays the detector.** `native_thread_reader_waits` keeps
  counting and logging any sync read that starts on a native thread. The test
  hazard total and the native-root daemon test assert it stays zero next to
  `native_thread_writer_waits`, per encode-lessons-in-structure.

Interface depth: callers see one `.await` and an `Arc` probe. Behind it the
storage layer hides which thread waits, the inline-on-pool rule, and the
reader pool's admission, round trips and end grace.

## Synthesis decision

Three design runners (two on Fable 5.1, one on Opus) and a Sonnet cross-judge.

- Candidate 1: async reader actors. `async_channel` replies, awaitable
  admission, worker self-return, and every sync read deleted.
- Candidate 2: an async boundary over the existing pool, with one hop inside
  the storage layer.
- Candidate 3: a `!Send` `Parkable` witness required by every parking entry,
  minted only on pool threads or by a runtime check.

The cross-judge scored candidate 1 highest on depth and latency and lowest on
deliverability. It recommended staging candidate 1 behind candidate 2's
signatures.

Base: candidate 2. All three runners converged on its public surface (async
`dispatch_read` at four layers with an `Arc` probe). It is the only shape whose
counter falls with each small PR, and it touches no reader-pool machinery.

Grafts:

- From candidate 1: no `cx` parameter on the read. Pool-thread sync callers
  bridge with `block_on` instead of a second sync entry forwarded through four
  layers, which the cross-judge flagged as a pass-through.
- From candidate 2: the scope-set store finding. The engine moves onto the
  inline-on-pool primitive.
- From candidate 3: attribute every residual counted wait from backtraces
  before fixing it. That is how the scope-set and remote-admission sites were
  found.

Rejected:

- Candidate 3's witness. `Parkable::here()` is a runtime check behind a type.
  It adds a parameter to about 15 entries and about 10 funnels, and it turns
  today's park into a production refusal on any path the suites do not cover.
- The abandon-latched probe (candidates 2 and 3). The typed reads are short
  point lookups, and `ExternalSourceRuntimeProbe` never interrupts. An
  abandoned read finishes on the pool in milliseconds and releases its lease.
- Candidate 1's async reader pool, for now. It rewrites admission, startup,
  end settlement and limbo accounting at once. The signatures above admit it
  later without touching a caller.

## Tradeoffs accepted

- We accept one blocking-pool round trip per async read (a queue and a oneshot
  wake) in exchange for never parking a native worker. The engine `Connection`
  already paid this on every SQL read.
- We accept that a pool thread waits for the whole read, bounded by the reader
  pool's own admission (5 ms for typed dispatch), in exchange for leaving the
  reader pool unchanged.
- We accept that a caller who drops its future leaves the read to finish on the
  pool, in exchange for no new probe wrapper.
- We accept that sync read methods stay public for sync port families that run
  on pool threads. The counter and the hazard assertions catch a native caller.

## Alternatives considered

- **Async Work and Workflow storage ports (RPITIT).** About 24 traits and 150
  sync service methods would cascade to async, and `&dyn` port use breaks plain
  RPITIT. They contribute zero counted waits because they already run on the
  pool. Largest diff, no counter movement.
- **Async reader actors (candidate 1).** The deepest interface and no hop. It
  carries the most risk, and it can land later behind the same signatures.
- **A parking witness on every sync entry (candidate 3).** It encodes the rule
  in signatures, but the sync funnels mint it with a runtime check. Its value
  over the counter is small next to its signature churn.

## Implementation reconciliation

Record accepted deviations here as the PRs land.

## Open questions and risks

- The projection drain issues one read per pending projection (up to 256 per
  pass). Each read now pays a pool round trip. If paired timing shows a cost,
  read a page of pending projections per hop rather than tuning the pool.
- The engine `ReadSnapshot` can drop on a native worker and wait the 5 ms end
  grace there. That wait is not counted. It is a candidate for the async reader
  follow-up.

## PR sequence

1. Typed reads (rusqlite-runtime, runtime-core, session-memory, global-db,
   store-runtime, project test support). Async `dispatch_read` at four layers
   with the hop in `RepositoryRuntimePhysicalAttachment`. Migrate every caller.
   Delete the sync typed read above the physical attachment. Adds this
   document.
2. Exact SQL reads (rusqlite-runtime, runtime-core, daemon-service,
   agent-hosts). `ExactSqlHandle` async read forms own the hop. The engine
   `Connection` and `ReadSnapshot` await them, and `run_blocking` is deleted.
   The scope-set store turns async.
3. Remote admission and the guard (api, tracedecay). The pre-body credential
   admission runs behind the API edge's hop. Reader waits join the test hazard
   total and the native-root daemon assertion. Fixes #4085.

## Next implementation step

Make `RepositoryRuntimePhysicalAttachment::dispatch_read` async over the
admitted reader pool, then lift the trait, lease and client signatures to match
`dispatch_submit`.
