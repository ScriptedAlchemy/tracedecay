# Explorer 1: SQLite execution core (rusqlite-runtime + runtime-core shard runtime)

Tree: worktree agent-a9a062786c04623b1 at origin/master e5a8ede6ea. PR #4097 diff read once (saved beside this file as pr4097.diff).
Paths below are relative to `crates/` unless absolute. "RR" = `tracedecay-rusqlite-runtime/src`, "RC" = `tracedecay-runtime-core/src`.
Prior grounding (/fast/tmp/arch-native-exec/ground-*.md) is not repeated. Two of its facts are now stale. `recv_writer_reply` no longer calls `block_in_place`. It only counts native-thread waits and then calls `async_channel::recv_blocking` (RR/exact_sql/mod.rs:116-129). Read waits are now counted too (`note_native_thread_reader_wait`, RR/exact_sql/mod.rs:144-155, landed with #4083/#4084).

## Headline numbers

- **Per writable SQLite store (one "attachment")**, `RepositoryPhysicalAttachmentFactory::attach_opened` (RR/repository/attachment.rs:158-309) starts:
  - 1 writer OS thread, named `tracedecay-rusqlite-writer` (RR/writer.rs:563-566), with its own `rusqlite::Connection`.
  - `min_per_hot_shard` = 2 general reader threads, named `tracedecay-rusqlite-reader` (RR/reader/worker.rs:421-422).
  - 1 reserved-health reader thread (RR/reader/pool/mod.rs:521-524).
  - That is **4 threads and 4 SQLite connections at rest**. The general lane grows on demand to `max_per_hot_shard` = 8, so the worst case is 1+8+1 = **10 threads and connections per store**. Burst readers retire after 60 s idle, but only lazily, inside a later acquire (RR/reader/pool/mod.rs:907-909, 1066-1103).
  - Each reader whose rollback outruns 5 ms spawns a short-lived "deferred return" thread (RR/reader/pool/lease.rs:75-107).
- **A read-only attachment** (`attach_read_only`, RR/repository/attachment.rs:50-97) has readers only: 3 threads at rest.
- **Per daemon**: there is one process-wide `StoreRuntimeRegistry`. tracedecay-store-runtime/src/session_registry/mounts.rs:192-198 builds it with `StoreRuntimeRegistryConfig::default()`. Writers per daemon equal the number of open writable SQLite stores. A daemon with P mounted projects holds:
  - Profile level: `global.db`, plus `user-sessions.db` and `user-memory.db` when touched, plus one `remote.db` per Remote Brain node.
  - Project level: 2×P files (`projects/<id>/tracedecay.db` and `projects/<id>/sessions.db`).
  - Each file costs 4 to 10 threads.
- **Mounted projects are never evicted.** `MAX_RETAINED_PROJECT_RUNTIME_OWNERS = 4096` (tracedecay-store-runtime/src/session_registry.rs:89-107). Its doc comment says capacity eviction "cannot reclaim a mounted project" and that idle-project hibernation is follow-up work. So 30 mounted projects means ≥ 60 + 1..3 SQLite stores, ≥ 244 rusqlite threads at rest, and up to ~630 under read bursts.
- **The registry's only capacity budget never applies.** `project_code_open_runtime_budget` (default 4, max 8: RC/shard_runtime/registry/capacity.rs:11-12, 111-142) counts only `StoreShardScopeV1::Code` entries (RC/shard_runtime/registry.rs:114-116; open.rs:289-300). The resolver refuses Code scopes as physical stores (tracedecay-store-runtime/src/store_locator_resolver.rs:330-334: "Code scopes are namespaces inside their project graph runtime, never physical stores"). The budget therefore never counts a real SQLite runtime. `eviction_idle` defaults to ZERO and only feeds that dead path.
- **Reader budgets are validated but never enforced.** `ReaderBudgetV1.min_global/max_global` (8/32), `open_project_runtimes` (4, max 8), and `AdmissionConfigV1.global_queue_max_bytes` (64 MiB) are validated in tracedecay-store/src/runtime/operation.rs. A workspace grep shows they are otherwise only echoed into telemetry (RC/shard_runtime/telemetry.rs:324,340,492; RC/runtime_telemetry/store_runtime.rs:12,136). Nothing caps total readers or queue bytes across stores.
- **Live daemon, read-only `/proc` inspection** (PID 1423753, `tracedecay daemon run`, started 2026-10-16 07:30:18Z):
  - 108 threads. 16 are `tracedecay-rusq` (comm is truncated, so writers and readers look alike): 4 stores × 4 threads. The rest: 48 `tracedecay-inde`, 24 `tokio-rt-worker`, 16 `tracedecay-cpu-`.
  - SQLite families open: `global.db`, `user-sessions.db`, `projects/proj_a5b3d7e3ebe14ca7/{tracedecay.db,sessions.db}`.
  - Each family has 4 WAL fds (one per connection), 1 shm fd, and 6-7 db fds (connections plus `OpenedDatabaseFile` pins).
  - **Every one of these fds points at a deleted inode.** `~/.tracedecay` was recreated at 07:31 and now totals 124 KB, while the daemon still holds:
    - `sessions.db` at 63.4 GB
    - `user-sessions.db` at 45.7 GB
    - `global.db` at 2.58 GB, with a 64.9 MB WAL (above the 32 MiB `journal_size_limit`, so a checkpoint is pending)
    - three 32 MiB WALs
  - That is ≈112 GB held by unlinked files. Nothing in the runtime notices a store unlinked under it. Only writer start and attach verify file identity (see Non-Obvious #13).

## Components Found

- **PersistentWriter** (RR/writer.rs:440-847): handle to the single writer actor thread of one shard. It owns the bounded mpsc senders (product writes, exact SQL, incremental vacuum, checkpoint, shutdown), the `Admission` byte/op accounting, and `watch` channels for checkpoint status and pressure.
- **Writer Worker** (RR/writer/worker/mod.rs:303-937): the thread body. It owns the connection (inside `WriterCheckpointController`) and runs the scheduling loop, batching, checkpoints, and page-cache sizing.
- **ingress** (RR/writer/worker/ingress.rs): `wait_for_work`, which polls the asupersync channels with a thread-unpark waker and `thread::park`s. Also `select_auxiliary_work` (round-robin between exact SQL and vacuum) and duplicate attach/reject.
- **writer/transaction.rs** (RR): `process_batch`. It runs one `BEGIN IMMEDIATE` per execution batch with one savepoint per request, records the ledger, commits, then publishes the watermark.
- **admission/** (RR/admission.rs, admission/queue.rs): `Admission` caps per-shard ops and bytes, with a reserved health slice of 1 op / 64 KiB. `FairQueue` is a deficit round-robin keyed by `(client_id, priority)`: health first, foreground weight 4, background weight 1 (queue.rs:5-6, 226-276).
- **checkpoint/** (RR/checkpoint/{controller,driver,types}.rs): the writer-owned WAL policy. It samples the WAL with `wal_checkpoint(NOOP)` after every batch and runs PASSIVE at the soft limit (32 MiB) and above. It sets `hard_drain_required` when an attempt at or above the hard limit (256 MiB) is incomplete. It runs TRUNCATE at shutdown.
- **connection/** (RR/connection/mod.rs): opens connections and applies PRAGMAs, limits, the authorizer, file-identity pinning (`OpenedDatabaseFile`), and `WriterPageCache`. It also provides the immutable and verified readers used for foreign host DBs.
- **reader/** (RR/reader/{pool/mod.rs, pool/lease.rs, worker.rs, locator.rs}): `ReaderPool` has a General and a ReservedHealth lane, uses a Condvar for capacity, and checks out LIFO. Each reader worker is one thread with one connection, running a command loop. A lease is RAII: one `BEGIN DEFERRED` snapshot per lease. `ReaderCheckpointBlockers` publishes active snapshots to the writer's checkpoint controller.
- **exact_sql/** (RR/exact_sql/mod.rs, command.rs, guard.rs): `ExactSqlHandle`, the arbitrary-SQL transport.
  - Writes go to the writer actor's exact-SQL channel. Each write has a sync form (parks the caller) and an `_async` form.
  - Reads go to the reader pool and are sync only.
  - `ExactSqlTransaction` is an interactive transaction executed on the writer thread.
- **ledger/** (RR/ledger/*): the runtime writer ledger installed in every registered store, made of `td_runtime_writer_checkpoint_v1` and `td_runtime_writer_idempotency_v2` (schema.rs:3-37). It handles idempotent replay of typed submits. Pruning runs only on authority-epoch supersession, at most 256 rows per commit (prune.rs).
- **persistence.rs / operation.rs** (RR): the bridge from a writer savepoint to `StorageOperationExecutor` (in production, `ConcreteRepositoryWriteExecutor`, RR/repository/mod.rs:89-187: configuration, fact, observation, cursor, remote replay, diagnostics, external source, retrieval anchors, git index) plus the ledger record.
- **schema_version.rs** (RR): the per-component ledger in table `tracedecay_schema_version(component, version)`. It adopts legacy stamps: `user_version` for graph, and `session_schema_migrations`, `session_temporal_schema_migrations`, and `global_schema_migrations`.
- **work.rs + work/** (RR): `WorkSqliteStorage` over a `RetainedExactSqlCapability` (RR/repository/retained_exact_sql.rs). It implements the sync Work ports with sync `begin_immediate`/`begin_deferred`/`query`. `workflow.rs`, `handoff.rs`, `work_attempt.rs`, `work_run_control.rs`, `remote/`, and `repository/scope_set.rs` follow the same pattern.
- **handoff.rs** (RR): `HandoffOpenSqliteAuthority`, the `handoff_open_grants_v1` table, accessed through sync `execute_tx`/`query_tx` from workflow.rs.
- **RepositoryRuntimePhysicalAttachment** (RR/repository/attachment.rs:350-685): one shard's writer `Arc` plus reader pool, with `dispatch_submit` (async), `dispatch_read` (sync), `run_checkpoint`, `run_bounded_incremental_compaction`, `drain`, and `close_and_join`.
- **StoreRuntimeRegistry** (RC/shard_runtime/registry.rs + registry/*): the process-wide singleflight map from `(shard_id, incarnation)` to a published `ShardRuntime` plus its physical attachment. It covers open, retire, evict, close, destructive maintenance reservations, and graph owner leases.
- **ShardRuntime** (RC/shard_runtime/shard.rs): in-memory lifecycle state (`RuntimeMaintenanceStateV1`) plus counted client and operation lifetime leases, pinned-profile flag, and last activity.
- **LifecycleShardRuntimePublisher** (RC/shard_runtime/registry/ports.rs:123-150): builds the physical attachment, always with `AdmissionConfigV1::default()`. It installs the final schema (Initialize) or verifies it (Existing), then publishes.
- **StoreRuntimeClientLease** (RC/shard_runtime/registry.rs:~640-1130): the counted client token. Through it callers get `authorized_exact_sql_handle`, `telemetry_read_handle`, `dispatch_submit_authorized` (async), and `dispatch_read` (sync, line 1117).
- **db::Database** (RC/db/connection.rs:59-63 + connection/registry.rs): the facade holding `DatabaseClientLeaseV1{StoreRuntimeClientLease, access}`. It yields engine `Connection`s and purpose adapters (RC/db/purpose.rs: work, workflow, scope-set, handoff, remote, remote recovery).
- **db::engine** (RC/db/engine/*): async `Connection`/`ReadConnection`/`Transaction`/`ReadSnapshot` over `ExactSqlHandle`. Writes use the `_async` forms. Reads run `run_blocking` → `cx.spawn_blocking` (connection.rs:318-331).
- **db/migrations.rs** (RC): the graph schema (project `tracedecay.db`, profile `user-memory.db`). It is create-only at `SCHEMA_VERSION = 39` (line 52). Any other stamp or shape is `ResetRequired`.
- **db/schema_version.rs** (RC): binds the RR schema-version ledger to the async engine.
- **sqlite_read_snapshot*.rs** (RC): side-effect-free inspection of whole SQLite families (reflink, else copy, else SQLite online backup into private scratch). Production users are tracedecay-maintenance `retention/storage_report.rs` and `retention/orphan_stores/collection.rs`. No serving path uses it.
- **store_telemetry.rs** (RC): file-size stat of db/wal/shm plus `PRAGMA synchronous/page_size/journal_mode` through the `Database`.
- **telemetry/store_size.rs** (RR): `SqliteStoreSizeTelemetryPort`. Store size comes from `page_count`/`freelist_count`. Table growth comes from a full `dbstat` scan on the reserved health reader (RR/reader/worker.rs:631-664).

## Flow

### A. Opening a store (registry → physical attachment)
1. A caller (for example the store-runtime mount, tracedecay-store-runtime/src/session_registry/mounts.rs:216-230 for the profile shard) calls `StoreRuntimeRegistry::open(request)` (RC/shard_runtime/registry/open.rs:405). `begin_or_join_open` does singleflight on `StoreRuntimeKey{shard_id, incarnation}` and allocates a new process authority epoch (`allocate_authority_epoch`, open.rs:839-848; a process-global AtomicU64 that increases on every open).
2. The capacity check runs only for Code scopes (open.rs:289-300), so in practice it never fires (see Headline).
3. The open is spawned with `self.inner.operations.retain(move |cx| async move { build_runtime(...) })` (open.rs:338). `RuntimeOperationTaskOwnerV1::retain` uses `self.cx.spawn` (RC/operation_task_owner.rs:119-125), so the open runs as a native async task.
4. `build_runtime` (open.rs:508-570): `resolver.resolve(key, mode, authority)` returns a `BoxFuture`, but `LocalStoreRuntimeResolverV1::resolve_key_inner` is fully sync. It canonicalizes paths, checks the local filesystem, reads the repository identity marker, and stats files (tracedecay-store-runtime/src/store_locator_resolver.rs:249-336, 473-...).
5. `LifecycleShardRuntimePublisher::publish` (ports.rs:126-150) calls `attachment.attach(&executor, &request, AdmissionConfigV1::default())` **synchronously inside the async block** (line 149). This dispatches to `RepositoryPhysicalAttachmentFactory::{attach, initialize, attach_read_only}` (ports.rs ~255-270).
6. `attach_opened` (RR/repository/attachment.rs:158-309):
   - It pins the file (`OpenedDatabaseFile::pin`, an open fd with inode identity) and clones the pin for the writer locator and the reader locator.
   - `PersistentWriter::start_with_checkpoint_blockers` (attachment.rs:221; writer.rs:478-595):
     - It validates admission and builds channels of `capacity = per_shard_queue.max_operations (2048) + health ops (1)` for both product and exact SQL. Vacuum and checkpoint channels hold 1.
     - It mints a `Cx` with `Budget::INFINITE` from the runtime handle and spawns the writer thread.
     - It then blocks the caller in `started_rx.recv()` (writer.rs:567) until the writer opens its connection, applies PRAGMAs, verifies identity, sizes the page cache, and disables auto-checkpoint (worker/mod.rs:328-439).
   - `ReaderPool::start_with_checkpoint_control` (attachment.rs:241; pool/mod.rs:482-526) spawns 2 general and 1 health worker synchronously. Each `worker::spawn` (reader/worker.rs:409-488) blocks on `startup.recv()` (line 476) while the thread opens the connection, verifies identity, and probes `PRAGMA schema_version`. That probe runs WAL-index recovery before any write can race it (comment at 446-450).
   - It checks that writer, readers, and the pinned file share one inode identity and that the path still names it.
7. Back in `publish_lifecycle_runtime` (ports.rs:153-191):
   - Initialize installs the final schema through an authorized connection. Graph scopes (Code, ProfileMemory, Project) use `crate::db::migrations::create_schema_connection`. Profile, ProfileSessions, and ProjectSessions use the registered `ports::registered_schema` installer. RemoteNode uses `REMOTE_NODE_LOCAL_SCHEMA` (ports.rs:407-458).
   - Existing opens of Code, ProfileMemory, and Project verify the v39 final shape and map mismatches to `ResetRequired` (ports.rs:193-260). Profile, ProfileSessions, ProjectSessions, and RemoteNode get **no** schema check at open.
   - The runtime then transitions to Ready and is published. The open-join waiters (`watch` on `OpenState`) get a `StoreRuntimeClientLease`.

So steps 4 to 6 (path canonicalization, thread spawns, and four sync startup recvs that each include a SQLite open, PRAGMAs, and WAL recovery) all run inside one poll of a native async task.

### B. Typed write (RuntimeSubmitRequestV1)
1. `DatabaseRuntimeClientV1::dispatch_submit` (RC/db/connection/registry.rs:84-104) → `StoreRuntimeClientLease::dispatch_submit_authorized` (RC/shard_runtime/registry.rs:1072-1101) → `RepositoryRuntimePhysicalAttachment::dispatch_submit` (attachment.rs:467-493) → `PersistentWriter::submit_authorized` (writer.rs:660-745).
2. `submit_authorized` validates, checks the probe and authority, then calls `Admission::reserve` (per-shard ops/bytes; the health lane is separate) and `try_send` on the bounded channel. A full channel returns a Saturation outcome immediately; it never waits for room.
3. The caller `await`s an asupersync oneshot (writer.rs:736-740). This is the async path; no thread parks.
4. Writer loop (worker/mod.rs:484-832):
   - It `drain_ingress`es into `FairQueue`, de-duplicating by `operation_id`. A follower whose payload matches attaches to the leader; a conflicting one gets `DuplicateOperationInFlight` (ingress.rs:219-263).
   - Checkpoint commands run first (560-566), then hard-drain handling (567-615), then one auxiliary command (617-688), then product work.
   - `drain_fair` (730) takes everything queued, health first and then deficit round-robin.
   - If the first item can coalesce, the batch dwells. The window is 2 ms for foreground (≤128 ops, ≤1 MiB) and 10 ms for background (≤512 ops, ≤4 MiB) (tracedecay-store/src/runtime/operation.rs:21-32; worker/mod.rs:85-112, 732-770). Health, isolated, and interrupted requests never dwell.
   - `build_batches` splits by priority and compatibility (1117-1172). `process_execution_batch` runs one `BEGIN IMMEDIATE`, a savepoint per request, the executor, `ledger::record_runtime_commit`, the commit, and `publish_committed` for the watermark (worker/mod.rs:1017-1050; writer/transaction.rs).
   - After every batch the loop runs `fit_page_cache` and `run_scheduled_checkpoint` (802-803).
5. Settlement replies through the oneshot. A corrupt-classified error or a failed watermark publish faults the whole writer (`WriterState::Faulted`, transaction.rs:141-161, 245-256, 345, 384). Every later submit then returns `Unavailable{Faulted}` (writer.rs:688-690).

### C. Exact-SQL write (engine `Connection` or sync `ExactSqlHandle`)
1. Engine (RC/db/engine/connection.rs): `execute`, `execute_batch`, `execute_returning`, and `transaction*` call the `_async` methods. The future awaits an `async_channel` reply, and no thread parks.
2. Sync (`ExactSqlHandle::execute/execute_batch/validate/begin_immediate/...`, RR/exact_sql/mod.rs:347-748): `enqueue_writer_request` (`try_send` on the exact-SQL channel; full means Busy) is followed by `recv_writer_reply` (116-129), which calls `recv_blocking` and **parks the calling thread**. On a native runtime thread it also increments `NATIVE_THREAD_WRITER_WAITS` and logs an error with a backtrace.
3. The writer serves one exact-SQL command per loop iteration through `select_auxiliary_work`. When the product queue is non-empty it serves product writes and exact SQL alternately (ingress.rs:175-196; worker/mod.rs:617-688, 831).
4. **An interactive transaction pins the only writer thread.** `begin_immediate` sends `WriterCommand::BeginTransaction` carrying a std `sync_channel` receiver. The writer thread enters `run_transaction` (RR/exact_sql/command.rs:474-...), loops on `receiver.recv_timeout(≤25 ms)`, and runs each command the caller sends.
   - Between caller round trips, nothing else on that shard writes: no product batches, no checkpoints, no vacuum.
   - Limits: idle 30 s, total 120 s (RR/exact_sql/mod.rs:33-35). An `AuthorizedLongLease` transaction renews them on progress (command.rs:596, 733-740).
   - Taking the write lock retries `SQLITE_BUSY` with 1 ms sleeps for up to 64 ms (command.rs:55-141).
5. Each statement in the transaction (`dispatch_with_policy`, mod.rs:1061-1067), `attach_database` (809-818), `commit` (969-972), and `rollback` (1011-1017) is another `recv_writer_reply` park in the sync form.

### D. Reads
1. **Engine async reads**: `Connection::query`, `read_snapshot`, `health_read_snapshot`, and `ReadSnapshot::query` call `run_blocking("SQL reader", …)`, which is `cx.spawn_blocking` (RC/db/engine/connection.rs:174-185, 237-252, 318-331; snapshot.rs:31-46). A blocking-pool thread then runs the sync path in step 2. The default wait is `READER_WAIT = 5 s` (connection.rs:17). Health reads use the same 5 s.
2. **Sync exact-SQL query** (`ExactSqlHandle::query_with_priority`, mod.rs:430-439) → `ReaderPool::execute_exact_sql_query` (pool/mod.rs:555-565):
   - `acquire_lane` (877-1060): a Condvar `wait_timeout` in 5 ms quanta up to `max_wait`.
   - `lease.execute_exact_sql_query` → `start_snapshot` → `WorkerClient::begin` (`send Begin`, then `recv()` unbounded, worker.rs:179-196) → `execute_exact_sql_query` (`recv()` unbounded, worker.rs:232-259).
   - On drop: `finish_snapshot` sends End and calls `recv_timeout(5 ms)`. On timeout it hands off to a deferred-return thread that waits `recv_timeout(2 s)` (lease.rs:336-361, 390-470).
   - So one query costs **3 cross-thread round trips** (Begin, Query, End) plus lease bookkeeping.
3. **Typed read** (`RuntimeReadRequestV1`): `StoreRuntimeClientLease::dispatch_read` (sync, RC/shard_runtime/registry.rs:1117-1130) → `RepositoryRuntimePhysicalAttachment::dispatch_read` (attachment.rs:543-568) → `acquire_for_dispatch`, which waits only `ACQUISITION_POLL_QUANTUM = 5 ms` (pool/mod.rs:58, 864-870) → `begin_snapshot` → `SnapshotLease::execute` → `WorkerClient::execute`. That last call counts `NATIVE_THREAD_READER_WAITS` and then polls `recv_timeout(5 ms)` until a reply arrives or the probe is interrupted (worker.rs:215-230, 388-406).
   - Production callers: RC/db/connection/registry.rs:113, tracedecay-store-runtime/src/remote_query.rs:137, tracedecay-session-memory/src/external_source_store.rs:783/821/849/914, tracedecay-session-memory/src/fact_store/runtime.rs:257, tracedecay-global-db/src/observation_adapter.rs:1800. #4085 counts its 2,683 native-worker waits here.
4. **Lanes and priority** (pool/mod.rs:69-77, 173-211, 843-860):
   - Health reads use ReservedHealth (ceiling 1).
   - Foreground reads use General up to `max_per_hot_shard` (8).
   - Background reads use General up to 8 − 2 (`FOREGROUND_RESERVED_GENERAL_WORKERS`).
   - During writer hard-drain pressure (`CheckpointPressure::BlockGeneral`), **every General acquisition waits** in 5 ms quanta and then fails Saturated at `max_wait` (pool/mod.rs:923-950). Only the health lane stays open.
5. **Reader worker** (worker.rs:490-679): one connection with `query_only=1`. Each lease opens `transaction_with_behavior(Deferred)` and serves commands on a per-snapshot channel until End (rollback) or Shutdown. While idle, the worker wakes every 30 s to return thread heap (`IDLE_THREAD_COLLECTION_WAIT_V1`, tracedecay-domain/src/process_heap.rs:148).
6. **Reads inside an exact-SQL transaction** (`ExactSqlTransaction::query`, `Transaction::query`) run on the writer thread, not on a reader.

### E. Checkpoint (current master)
1. Every connection gets `wal_autocheckpoint=0` (writer) and `SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE` (all modes) (RR/connection/mod.rs:781, 809). The writer is the only checkpointer.
2. After every product batch and every exact-SQL command, `run_scheduled_checkpoint` (worker/mod.rs:859-878) runs:
   - `sample_wal` is `PRAGMA wal_checkpoint(NOOP)` for `log_frames` × (page_size + 24) + 32 bytes (RR/checkpoint/driver.rs:63-86).
   - Below soft (32 MiB) with no hard drain pending, nothing happens.
   - Otherwise it runs `PRAGMA wal_checkpoint(PASSIVE)` (controller.rs:75-90, driver.rs:88-107).
   - `complete` means `!busy && checkpointed >= log_frames` (types.rs:144-148).
   - If an attempt at ≥ hard (256 MiB) is incomplete, `hard_drain_required = true` and `CheckpointPressure::BlockGeneral` is published (controller.rs:155-189; worker/mod.rs:948-965).
3. **Hard drain** (worker/mod.rs:567-615):
   - Waiting product writes have their probes polled (cancelled ones settle) and stay queued.
   - Exact-SQL and vacuum commands that arrive also stay queued, because the branch `continue`s before the auxiliary step.
   - General-lane readers block (D.4).
   - The writer retries the scheduled checkpoint every `HARD_CHECKPOINT_RETRY_INTERVAL = 100 ms` (line 69) and bumps `checkpoint_hard_retry` telemetry.
4. **Idle WAL is never revisited.** The idle `wait_for_work` (worker/mod.rs:697-722) has no deadline unless a hard drain is pending. A WAL above soft but below hard stays pending until the next write arrives. Requested checkpoints (`CheckpointHandle::trigger_authorized`, writer.rs:103-146; channel depth 1, full means Busy) run first in the loop. They first call `checkpoint_blockers.await_released_snapshots()`, which **parks the writer thread** up to 2 s on a Condvar (worker/mod.rs:889; pool/mod.rs:159-170).
5. Shutdown: `truncate_at_shutdown` runs `wal_checkpoint(TRUNCATE)` with empty blockers (controller.rs:99-110; worker/mod.rs:836-845). A failure leaves the writer Faulted.
6. File shrink: `journal_size_limit = RETAINED_WAL_BYTES = 32 MiB` on Writer and Maintenance connections (connection/mod.rs:31-58, 815-823). SQLite applies it when the log resets.
7. `ExactSqlHandle::checkpoint_wal_truncate` is a separate path: `WriterCommand::CheckpointWalTruncate` runs `PRAGMA wal_checkpoint(TRUNCATE)` as an exact-SQL command (command.rs:385-415). Its caller is RC/db/connection/retained_maintenance.rs:57.
8. **PR #4097 (open, branch fix/sweep-sessions-checkpoint) changes** exactly these points:
   - `CheckpointMode::Passive` becomes `Restart`, and the driver SQL becomes `PRAGMA wal_checkpoint(RESTART)` (controller.rs:85, driver.rs:96, types.rs:120,158). With `busy_timeout=0` it never blocks. It reports `busy` while any reader holds a WAL read slot, so a hard drain holds until the next write is guaranteed to restart the log.
   - The PR's live diagnosis: PASSIVE never restarts the log. Readers keep slots 1 and 2 almost continuously. A health-lane reader admitted at the newest frame lets PASSIVE report "complete", so the drain lifts and the next write appends past hard and re-arms. That cycle produced 245,264 hard-retry wakes and a 229 MB WAL on the `project_sessions` shard.
   - It also changes `runtime_telemetry/store_runtime.rs::checkpoint_snapshot` to report blockers of a Pending or Interrupted outcome while pressure is `open`.
   - It adds a test, `hard_wal_pressure_holds_writes_until_the_wal_log_can_restart` (attachment.rs tests).
   - It does **not** add an idle checkpoint timer or count readers outside the attachment, and RESTART also governs the soft band.

### F. Close and lifetime of a store handle
1. Ownership chain:
   - Registry entry (`RegistryEntry::{Opening, Ready, Evicting, Retiring, Committing, Faulted, DurabilityUncertain}`, RC/shard_runtime/registry/open.rs:672-701) → `StoreRuntimeOwnerAttachment` → `PublishedShardRuntime{ShardRuntime, Box<dyn PhysicalRuntimeAttachment>}` (RC/shard_runtime/registry/attachment.rs:228-255).
   - The attachment owns `Arc<PersistentWriter>` and a `ReaderPool` (an Arc around `PoolInner`).
   - Clients hold `StoreRuntimeClientLease`, a counted token. `Database` holds `DatabaseClientLeaseV1`. `RetainedExactSqlCapability` holds a type-erased client guard (RR/repository/retained_exact_sql.rs:10-30).
   - An `ExactSqlHandle` holds only a cloned writer `Sender` and **Weak** reader-pool closures (RR/exact_sql/mod.rs:194-279), so a handle cannot keep readers alive.
2. Eviction for project and profile SQLite stores never happens through capacity (Headline). Removal is explicit: the retirement and destructive-maintenance paths in RC/shard_runtime/registry/{retirement,destructive,close}.rs, driven from store-runtime project retirement (not traced, see Open Questions) and from shutdown (`cancel_opens_for_shutdown` + close).
3. `RepositoryRuntimePhysicalAttachment::drain` (attachment.rs:570-634):
   - It closes admission and calls `writer.begin_drain()` (drops the writer-side senders) and `readers.begin_shutdown_drain()`.
   - It then **sleep-polls** every 5 ms up to `ATTACHMENT_DRAIN_TIMEOUT = 5 s` (lines 38-39, 591-618) until the writer `Arc` is unique with queue 0, and the reader pool is quiescent with no leases or limbo and a strong count of 1.
   - Then `writer.shutdown_and_join()`. This joins the writer thread after it empties its queues and runs the TRUNCATE checkpoint.
4. `PersistentWriter::drop` also drains and joins (writer.rs:841-847). `PoolInner::drop` shuts down and joins every reader thread (pool/mod.rs:399-417).

## Store Inventory (SQLite stores this runtime serves)

All stores share these properties unless a field says otherwise:
- Engine: SQLite (bundled rusqlite 0.40.1, `bundled,cache,hooks,limits,trace`; THREADSAFE=1 and FTS5 asserted at open, RR/connection/mod.rs:964-988).
- Writer: one dedicated OS thread per attachment.
- PRAGMAs on all connections: `busy_timeout=0`, `foreign_keys=1`, `trusted_schema=0`, QPSG on, `NO_CKPT_ON_CLOSE`.
- Writer PRAGMAs: `journal_mode=WAL`, `synchronous=NORMAL`, `wal_autocheckpoint=0`, `journal_size_limit=32 MiB`. `auto_vacuum=INCREMENTAL` is set **only when the file is zero-length at writer open** (RR/connection/mod.rs:620-622, 797-814).
- Page cache: writer cache = max(SQLite default, 4 × number of B-trees) pages (WriterPageCache, mod.rs:862-928). Readers use the SQLite default (~2 MB). No mmap.
- Limits: `SQLITE_LIMIT_LENGTH` 64 MiB, `VDBE_OP` 25M, `ATTACHED` 0 (4 for Maintenance) (mod.rs:990-1018).
- Every writable store carries the runtime ledger tables.

Per-store details:

1. **Profile authority store**: `<profile_root>/global.db` (`GLOBAL_DB_FILENAME`, RC/config.rs:19; resolver store_locator_resolver.rs:268-280).
   - Unit: one per profile.
   - Scope: `StoreShardScopeV1::Profile`. It is pinned (`pinned_profile = true`, ports.rs:132-133) and mounted at daemon start (mounts.rs:216-230).
   - Readers: the general lane through engine `spawn_blocking`, or `dispatch_read` on the caller thread (tracedecay-global-db observation_adapter.rs:1800).
   - Schema: the registered global/session installer at Initialize; per-component `tracedecay_schema_version` rows afterwards (observation migrations, session-temporal, codex echo, and others in tracedecay-global-db). No schema check at existing open.
   - Growth bound: none in this layer. Live: 2.58 GB plus a 64.9 MB WAL.
2. **Profile sessions**: `<profile_root>/user-sessions.db` (RC/storage.rs:24).
   - Unit: one per profile. Scope `ProfileSessions`, opened lazily (mounts.rs:677-760).
   - Schema: the same registered installer, then LCM, git-correlation, and workflow-index components.
   - Growth bound: none in this layer. Live: 45.7 GB.
3. **Profile memory**: `<profile_root>/user-memory.db` (RC/storage.rs:26).
   - Unit: one per profile. Scope `ProfileMemory`, opened lazily (mounts.rs:895-960).
   - Schema: graph v39 create-only, mismatch means ResetRequired. Not open in the live daemon.
4. **Remote node**: `<profile_root>/remote/nodes/<sha256(node_id)>/remote.db` (resolver 339-363).
   - Unit: one per Remote Brain node. Scope `RemoteNode`. Schema `REMOTE_NODE_LOCAL_SCHEMA`. Bounded by `MAX_REGISTERED_REMOTE_NODES`.
5. **Project store**: `<profile_root>/projects/<project_id>/tracedecay.db` (RC/storage/paths_and_io.rs:859; layout.rs:69-75).
   - Unit: one per project id. The id is a hash of the repository identity root, so **linked worktrees collapse onto the repository's id** (layout.rs:89-127, 198-222).
   - Scope `Project`. Writer: the attachment writer. Sync Work/Workflow/handoff/scope-set/remote adapters park their caller threads on it (RC/db/purpose.rs).
   - Schema: graph v39 create-only, verified on existing open (ports.rs:193-260).
6. **Project sessions**: `<profile_root>/projects/<project_id>/sessions.db` (paths_and_io.rs:861).
   - Unit: one per project. Scope `ProjectSessions`. Schema: the registered installer plus per-component stamps.
   - Live: 63.4 GB. This is PR #4097's shard with the 229 MB WAL.
7. **Code scopes** (`StoreShardScopeV1::Code{project, repository, Worktree|Branch|Snapshot}`) are not SQLite stores. They are namespaces in the project's Grafeo graph (store_locator_resolver/graph.rs:9-24; RC/shard_runtime/registry/graph.rs:1047-1087). Out of this angle.
8. **Foreign host DBs** are opened read-only, outside the registry: OpenCode via `open_verified_read_snapshot`, Cursor via `open_verified_immutable_reader`, Hermes via `open_immutable_reader` (tracedecay-sessions/src/runtime/hosts/{opencode.rs:597, opencode_snapshot.rs:86, cursor_composer/sqlite.rs:115, hermes/state_db.rs:599}). An `open_verified_read_snapshot` connection holds a `BEGIN DEFERRED` snapshot on the foreign WAL for its lifetime.
9. **Maintenance snapshots**: `sqlite_read_snapshot::open_foreign_in` and `materialize` copy whole families into scratch by reflink, else copy, else online backup (RC/sqlite_read_snapshot.rs:181-276, 698-733). The online backup steps 128 pages at a time and sleeps 1 ms on busy (lines 256-266). Callers are maintenance storage_report.rs:449-458 and orphan_stores/collection.rs:788.

Failure containment per store (all SQLite stores):
- A per-request non-corrupt error rolls back that request's savepoint only.
- A corrupt-classified error, a failed watermark publish, a page-cache resize failure, a non-transient checkpoint driver error, a TRUNCATE failure at shutdown, or a panic in the loop (`catch_unwind`, worker/mod.rs:442-458) sets `WriterState::Faulted`. Every later write to that **whole shard** then returns `Unavailable{Faulted}`, and queued work is rejected (worker/mod.rs:520-559). Nothing in RR restarts a faulted writer.
- Readers keep serving a faulted shard.
- A reader worker that fails its snapshot end is retired and replaced (`retire = true`, lease.rs:342-357).
- The hard drain (E.3) freezes all general reads and writes on a store while any reader of that file holds a WAL slot. Sources include a health-lane snapshot, a reader in another process, a maintenance online backup, or an OpenCode-style verified snapshot (that one only on foreign files).

Growth and retention bounds this layer owns:
- WAL: soft 32 MiB, hard 256 MiB, file trimmed to 32 MiB on reset.
- Ledger idempotency rows: unbounded within one authority epoch. They are pruned 256 per commit only after an epoch advance; each process reopen advances the epoch.
- Queues: 2048 ops / 16 MiB per shard.
- Reader threads: 10 per shard.
- **No byte bound on any database file** and no cross-store or global bound. `auto_vacuum` is INCREMENTAL only on stores created fresh by this runtime. `run_incremental_vacuum` (worker/mod.rs:1060-1115) silently does nothing on a non-incremental file.

## Every place a caller parks a thread waiting on a reply

| # | Site | What parks | Bound | Counted? |
|---|------|-----------|-------|----------|
| 1 | `recv_writer_reply`: sync execute, execute_returning, validate, execute_batch, checkpoint_wal_truncate, release_connection_memory (writer half), begin_immediate/deferred/long_lease, each transaction statement, attach_database, commit, rollback (RR/exact_sql/mod.rs:116-129 and 458, 499-513, 693-700, 740-748, 809-818, 969-972, 1011-1017, 1061-1067) | Caller thread, `async_channel::recv_blocking` | None | `NATIVE_THREAD_WRITER_WAITS` only when on a native runtime thread |
| 2 | Sync exact-SQL `query_with_priority` → `acquire_lane` Condvar (pool/mod.rs:877-1060) | Caller | `max_wait` (callers pass 5 s; Work passes 5 s, RR/work/sql.rs:62) | Reader-wait counter on entry (mod.rs:437) |
| 3 | `WorkerClient::begin`, `pin_exact_sql`, `execute_exact_sql_query`: `recv()` (worker.rs:179-259) | Caller | **Unbounded** | Covered by #2's single count |
| 4 | `begin_read_snapshot*` / `begin_health_read_snapshot` (mod.rs:558-573), then `ExactSqlReadSnapshot::query_with_budget` (mod.rs:781-790) | Caller | acquire `max_wait`; replies unbounded | Counted once per call |
| 5 | Typed `dispatch_read`: `acquire_for_dispatch` (5 ms) + `begin` `recv()` + `execute` `recv_timeout(5 ms)` loop (worker.rs:215-230, 388-406) | Caller | Probe deadline only | `note_native_thread_reader_wait` in `WorkerClient::execute` |
| 6 | Lease drop → `finish_snapshot` `recv_timeout(5 ms)` (lease.rs:336-361); deferred-return thread `recv_timeout(2 s)` (lease.rs:390-470) | Caller ≤5 ms, then a spawned thread | 5 ms / 2 s | No |
| 7 | Engine async reads (`Connection::query`, `read_snapshot`, `ReadSnapshot::query`) → `cx.spawn_blocking` (RC/db/engine/connection.rs:318-331) | One blocking-pool thread runs #2-#4 | 5 s acquire, then unbounded replies | Not counted (the blocking pool has no current runtime handle) |
| 8 | `PersistentWriter::open` `started_rx.recv()` (writer.rs:567) and reader `worker::spawn` `startup.recv()` (worker.rs:476), ×(1 + 3) per attach. Runs inside the native open task (ports.rs:149) | **Native worker thread** | Unbounded (SQLite open + WAL recovery) | No |
| 9 | Store/table size telemetry `recv_timeout(reply_bound)` (worker.rs:261-310); release memory `recv`/`recv_timeout` (worker.rs:312-350) | Caller | `reply_bound` / 5 ms quanta | No |
| 10 | `RepositoryRuntimePhysicalAttachment::drain`: `thread::sleep(5 ms)` loop ≤5 s, then `writer.shutdown_and_join()` thread join (attachment.rs:591-628) | Caller | 5 s + writer drain + TRUNCATE | No |
| 11 | `PersistentWriter::drop` / `PoolInner::drop` thread joins (writer.rs:841-847; pool/mod.rs:399-417) | Whoever drops the last Arc | Writer drain | No |
| 12 | Writer thread itself: `wait_for_work` `thread::park`/`park_timeout` (ingress.rs:96-105); `run_transaction` `recv_timeout(≤25 ms)` while an interactive transaction is open (command.rs:513); busy retry `sleep(1 ms)` ≤64 ms (command.rs:136-138); `await_released_snapshots` Condvar ≤2 s per requested checkpoint (worker/mod.rs:889) | Writer | As noted | Telemetry spans only |
| 13 | Online backup step sleep 1 ms (RC/sqlite_read_snapshot.rs:256-266) | Maintenance caller | Until done | No |
| 14 | Async paths (`submit_authorized`, `*_async`, `CheckpointTicket::wait`, `bounded_incremental_vacuum`, `release_connection_memory_async` reader half via `cx.spawn_blocking`) | Nothing parks except the spawn_blocking half | n/a | n/a |

## Files Read

- RR: lib.rs; writer.rs (all); writer/worker/mod.rs (1-1180); writer/worker/ingress.rs (all); writer/transaction.rs (excerpts 100-160, 240-260, 330-440); checkpoint/{mod,controller,driver,types}.rs (all); connection/mod.rs (15-66, 480-1030); reader/mod.rs; reader/pool/mod.rs (1-1305 skim, 1-1000 read); reader/pool/lease.rs (157-365 + grep); reader/worker.rs (120-690); exact_sql/mod.rs (1-760 + grep of the rest); exact_sql/command.rs (55-145, 241-300, 474-530); admission.rs (1-80); admission/queue.rs (5-40, 221-276); persistence.rs (1-80); ledger/schema.rs; ledger/prune.rs (1-60); schema_version.rs (1-200); work.rs; handoff.rs (1-60); repository/attachment.rs (38-690); repository/retained_exact_sql.rs; repository/mod.rs (89-187 grep); telemetry/store_size.rs (1-80); native_runtime.rs (grep only).
- RC: shard_runtime/mod.rs; shard_runtime/shard.rs (1-80 + outline); shard_runtime/registry.rs (1-140, 995-1160, 1475-1545); registry/capacity.rs (1-140); registry/open.rs (1-120, 286-300, 338, 505-700, 825-869); registry/ports.rs (60-215, 405-470); registry/attachment.rs (outline); registry/close.rs (1-40); db/engine/{mod,executor,connection}.rs, engine/snapshot.rs and engine/transaction.rs (grep); db/connection.rs (1-120); db/connection/registry.rs (1-140); db/purpose.rs (1-140); db/migrations.rs (1-200 + outline); db/schema_version.rs (1-80); storage.rs (1-140); storage/layout.rs (60-222); storage/manifest.rs (1-60); sqlite_read_snapshot.rs (1-80, 690-735 + grep); store_telemetry.rs (grep); operation_task_owner.rs (grep 119-139); config.rs (grep).
- Others: tracedecay-store/src/runtime/operation.rs (1-60, 240-380); tracedecay-store/src/runtime/identity.rs (116-250); tracedecay-store-runtime/src/store_locator_resolver.rs (230-520); store_locator_resolver/graph.rs (1-60); session_registry.rs (89-135); session_registry/mounts.rs (170-230 + grep); tracedecay-lcm/src/schema.rs (400-480 grep); PR #4097 body and diff.
- Live: `/proc/1423753/{fd,task/*/comm}` and `stat -L` of its fds; `ls`/`du` of `~/.tracedecay`.

## Boundaries

- Crate dependencies: tracedecay-rusqlite-runtime depends on tracedecay-domain, tracedecay-contracts, tracedecay-store, asupersync, tokio (rt only, for `native_runtime::tokio_handle`), and rusqlite. tracedecay-runtime-core re-exports `native_runtime` and wraps RR types (registry.rs:73-79). tracedecay-store-runtime supplies the resolver (`LocalStoreRuntimeResolverV1`) and decides which shards open, plus their mounts and ceilings.
- Inputs to RR are typed and closed: `RuntimeSubmitRequestV1`, `RuntimeReadRequestV1`, `RuntimeRequestProbeV1` (cancellation/deadline polled, never notified), `AdmissionConfigV1` (always default in production, ports.rs:149), and `VerifiedStoreLocatorV1` plus a pinned fd. Exact SQL is the escape hatch: arbitrary SQL behind an authorizer (`exact_sql/guard.rs`) and write authority (`ExactSqlWriteAuthority`).
- Three execution classes sit at the storage boundary:
  1. Async typed submit and async exact-SQL writes: no thread parks.
  2. Async engine reads: a blocking-pool thread parks, plus a reader worker thread.
  3. Sync ports (Work, Workflow, handoff, scope-set, remote, `dispatch_read`): the caller thread parks on every round trip.
  All three end on the same writer thread and the same reader workers.
- Out of angle, handed to others: Grafeo graph files and `GraphDbRegistry`; LCM payload directories and response handles; session ingest and projection drains; maintenance retention; doctor and reset; store manifest content; `daemon/project_server_lifecycle.rs` (forbidden).

## Non-Obvious Things

1. **Two capacity knobs look like bounds but do nothing.**
   - `StoreRuntimeRegistryConfig.project_code_open_runtime_budget` counts only Code entries, which are never SQLite runtimes.
   - `ReaderBudgetV1.{min_global, max_global, open_project_runtimes}` and `global_queue_max_bytes` are validated and then only reported.
   - The real ceiling is `MAX_RETAINED_PROJECT_RUNTIME_OWNERS = 4096` mounted projects with no eviction. Its doc says so (session_registry.rs:89-106).
2. **The writer thread is also the transaction executor for interactive exact-SQL transactions.** A sync Work/Workflow transaction blocks the writer between the caller's round trips (≤30 s idle, ≤120 s total). That caller is often a native or blocking-pool thread that is itself waiting on a reader or another writer. Product batches, checkpoints, and vacuum on that shard all stall meanwhile.
3. **The hard drain also blocks general readers** (BlockGeneral, pool/mod.rs:923-950). WAL pressure on one store therefore freezes all non-health reads and writes on that store until every WAL reader slot clears. Before #4097 the drain could thrash (PASSIVE completes, drain lifts, next write re-arms). With #4097 (RESTART) it can instead hold indefinitely under steady readers, which a later reviewer should weigh.
4. **Idle stores never checkpoint.** The scheduled checkpoint runs only after a write; idle `wait_for_work` has no timer unless a hard drain is pending. A WAL between 32 and 256 MiB on a quiet store stays put, and so does its file size, until the next write. The live `global.db-wal` is 64.9 MB.
5. **`CheckpointBlockers` only knows this attachment's own readers** (`ReaderCheckpointBlockers`, pool/mod.rs:79-171). These are invisible to it: readers from a second attachment (for example a read-only attachment of the same file), maintenance online backups, and other processes. So "pending, blockers: []" is a normal state, not a contradiction. PR #4097 fixes only the case where the snapshot dropped blockers.
6. **A one-shot read costs three thread hops.** Begin, Query, and End each cross threads, and a Deferred snapshot is opened and rolled back per query (lease.rs:195-206, 283-303). An engine async read adds a fourth hop through `spawn_blocking`.
7. **Async-shaped open, sync body.**
   - `StoreRuntimeResolver::resolve` returns a BoxFuture, but the local resolver does sync filesystem and identity work.
   - `ShardRuntimePublisher::publish` calls the sync `attach` (thread spawns plus four blocking startup recvs, each including SQLite open and WAL recovery) inside a native task (ports.rs:142-150).
   - On a 63 GB `sessions.db` with a 200 MB WAL, the first reader's schema probe runs WAL-index recovery while a native worker is parked.
8. **`AdmissionConfigV1::default()` is hard-wired for every shard** (ports.rs:149). The operator-configurable WAL budget, batch windows, and reader budgets are never configured per store in production.
9. **`auto_vacuum=INCREMENTAL` only for files that are zero-length at writer open** (connection/mod.rs:620-622, 799-804), plus `configure_fresh_auto_vacuum` in graph `create_schema` (RC/db/migrations.rs:180-215). Stores created by older binaries keep their original mode, and live compaction (maintenance `run_bounded_incremental_compaction`) is then a no-op on them.
10. **Schema evolution is reset-only.**
    - The graph schema (`tracedecay.db`, `user-memory.db`) is created at v39 and verified at every existing open. A mismatch yields `ResetRequired` (migrations.rs:1-5, 210-264).
    - Session and global stores get no check at open. Their components (lcm, git_correlation, workflow_indexing, session-temporal, observation migrations) read and record versions later, in their own convergence code, and also refuse on mismatch (for example `LcmError::ProfileResetRequired`, lcm/schema.rs:458-470).
    - The v39 shape still carries the retired outbox/inbox tables, because "a store cannot change shape in place" (migrations.rs:54-120), while `RETIRE_RUNTIME_EFFECTS_V1` drops them on schema install (ledger/schema.rs:39-47).
11. **The ledger epoch is a process-global counter.** It advances on every open of any shard (open.rs:839-848), so idempotency rows become prunable only after reopen, at 256 rows per commit.
12. **Writer startup is not "open then serve".** The writer reports Ready before readers start. The attachment then verifies that writer, readers, and the pin share one inode, and that the path still names that inode (attachment.rs:263-288). After that, nothing re-verifies the path, so an unlink or replacement of the store directory goes unnoticed. The live daemon is writing to deleted `~/.tracedecay` files right now.
13. Thread names are truncated to 15 chars (`tracedecay-rusq`), so `/proc` cannot tell writer from reader threads. Only the per-shard telemetry snapshot (`RepositoryRuntimePhysicalSnapshot`) can.
14. The only active reaper of idle general readers is the 60 s burst retirement, and it runs **only during a later acquire** on that same pool (pool/mod.rs:907-909, 1066-1103). A pool that bursts to 8 and then goes quiet keeps 8 threads and connections indefinitely.
15. `ExactSqlHandle::release_connection_memory` (sync) dispatches `PRAGMA shrink_memory` through the writer as an exact-SQL `ExecuteBatch` (mod.rs:498-513). The reader half goes worker by worker. A worker in the middle of a snapshot is skipped and reported busy.

## Open Questions (not traced)

- Who drives project-shard retirement and close in production. `daemon/project_server_lifecycle.rs` is forbidden. registry/retirement.rs and close.rs mechanics were only skimmed. So whether any idle project's `tracedecay.db`/`sessions.db` attachment is ever closed before daemon shutdown is unconfirmed beyond the session_registry.rs doc comment.
- Whether a read-only attachment and a read-write attachment of the same file can coexist in one registry. `retained_database_key` only dedups writer-present entries (open.rs:672-701). If they can, the second pool's readers are invisible blockers.
- Which `Database` (`tracedecay.db` or `sessions.db`) backs `registered.database.work_storage()/handoff_open_storage()` in tracedecay-daemon-service. The migrations v39 comment says handoff grants live in the project session store.
- The "30+ project stores, one per agent worktree" incident versus the code: `default_profile_project_id` collapses linked worktrees to the repository identity root (layout.rs:113-127, 198-222). That code path cannot mint one store per worktree. Whichever path did mint them (a missing `.git` marker, non-git checkouts, `path_local_profile_project_id`, older binaries) needs another explorer.
- Why the live daemon (PID 1423753) holds only deleted store files, which suggests a wipe or reset at about 07:31 while it ran. Who deleted them was not investigated, by instruction. The redesign should note that nothing detects it.
- The exact behavior of a faulted writer at the registry level (`RegistryEntry::Faulted` / `DurabilityUncertain`) and whether anything reopens it.
- The `ports::registered_schema` installer body (OnceLock-registered from a higher crate) and the per-component convergence order for session and global stores.
