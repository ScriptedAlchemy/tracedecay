# Explorer 2: port surface, store identity, resolution

Worktree: /fast/projects/tracedecay/.claude/worktrees/agent-a9a062786c04623b1 at e5a8ede6ea (origin/master).
All paths below are relative to `crates/` unless absolute. Census scripts (re-runnable):
`/tmp/claude-1000/-fast-projects-tracedecay/c9698727-3461-47d1-82ba-d60f5d96485a/scratchpad/storage-arch/e2/traits.py <crate>/src`
(trait name, file:line, top-level method count, sync/async shape) and
`.../e2/refs.py <repo> Name1,Name2` (impl sites + reference counts per crate, prod vs test).
Prior grounding (/fast/tmp/arch-native-exec/ground-*.md) is assumed: ExactSqlHandle reads sync-only, writer
thread + reader threads with std mpsc replies, NATIVE_THREAD_WRITER_WAITS, RPITIT async ports backed by
async engine `Connection`, ~15 per-crate offload helpers. Not re-derived here.

---------------------------------------------------------------------------------------------------

## Components Found

- tracedecay-store (`tracedecay-store/src/lib.rs:1-5`): "persistence contracts and DTOs only"; deps are
  domain + temporal-query + serde/sha2/flate2 only (`tracedecay-store/Cargo.toml`), no engine, no executor.
  Holds 19 `pub trait`s plus the closed-enum runtime protocol (`RuntimeReadRequestV1`,
  `RuntimeSubmitRequestV1`, `RepositoryWritePayloadV1`, ...).
- tracedecay-contracts: application ports and services. 24 sync Work/Workflow storage ports implemented
  by three rusqlite-runtime structs; services (`WorkAttemptService<S>` etc.) are sync generics over them.
  Deps: domain, policy, tool-catalog, serde (no engine, no executor) (`tracedecay-contracts/Cargo.toml`).
- tracedecay-runtime-core `shard_runtime/registry.rs`: process registry of published shard runtimes
  (`StoreRuntimeRegistry`), `StoreRuntimeClientLease` (registry.rs:120), the physical attachment trait
  (`registry/attachment.rs:70`), resolver/publisher ports (`registry/ports.rs:98,115`).
- tracedecay-runtime-core `db/` : `Database` facade (`db/connection.rs:62`), purpose adapters minting
  `RetainedExactSqlCapability` (`db/purpose.rs:9-120`), `DatabaseRuntimeClientV1` (`db/connection/registry.rs:60`).
- tracedecay-runtime-core `config.rs`, `storage.rs`, `storage/{layout,identity,paths_and_io,manifest}.rs`,
  `worktree.rs`: profile root, path constants, project-id minting, repository-identity marker, layout.
- tracedecay-store-runtime: "single owner of store, shard, and session lifecycle for a daemon"
  (`tracedecay-store-runtime/src/lib.rs:1-16`). `DaemonSessionRuntimeRegistryV1`
  (`session_registry.rs:2661`), `LocalStoreRuntimeResolverV1` (`store_locator_resolver.rs:117-`),
  `StoreWriterGates` (`writer_gate.rs`), standalone process-global registry (`standalone_session.rs`),
  shutdown joins (`store_shutdown.rs`), retained memory execution port (`retained_memory.rs`), remote_* actors.
- tracedecay-daemon-identity: profile identity record (`profile_identity.rs`), daemon authority lock/record
  election (`authority.rs`), client connection discovery (`connection.rs`).
- tracedecay-project: `TraceDecay` project handle; store-layout identity resolution
  (`project/lifecycle/identity.rs`), init/open (`project/lifecycle/mod.rs`), standalone registry join
  (`project_store_runtime.rs`).
- tracedecay-global-db `project_registry.rs` + `schema_stages.rs:55-106`: the durable registry of projects,
  aliases, store instances, graph scopes, store artifacts in `global.db`.
- Daemon composition: `tracedecay/src/daemon/engine.rs:50-110` (`DaemonEngine`),
  `tracedecay/src/daemon/branch_admin.rs:448-490` (`StoreAdministration`),
  `tracedecay/src/daemon/database_owner_registry.rs` (`DatabaseOwnerRegistry` keyed by `ProjectServerKey`),
  `tracedecay/src/daemon/project_open_owners.rs:320-620` (per-route owner mounting incl. Work registration),
  `tracedecay-daemon-service/src/invocation/registrars.rs:1044` (`DaemonWorkRuntimeRegistrar::register`).

---------------------------------------------------------------------------------------------------

## Port census

### A. tracedecay-store (19 traits, 143 top-level methods)

Shape key: "async" = every method returns `impl Future<Output=..> + Send` (RPITIT; none use `async fn`
in the trait, none are object safe). "sync" = plain return.

| trait | def | methods | shape | prod impls | prod refs by crate (test refs) |
|---|---|---|---|---|---|
| ConfigurationRevisionStore | configuration.rs:241 | 6 | async | global-db `GlobalDbConfigurationControlStore<'_>` (configuration/store/revision.rs:65) | global-db 3 (10) |
| DiagnosticStore | diagnostics/ports.rs:10 | 5 | async | application `DiagnosticsStore<'_>` (diagnostics_store.rs:911) | application 8 |
| GitIndexTransactionStore | git_index_transactions.rs:230 | 13 | **sync** | code-index-runtime `DaemonGitIndexTransactionStore`, `SharedDaemonGitIndexTransactionStore` (git_transactions/store.rs:263,677) | code-index-runtime 25 |
| ProjectMemoryGraphStore | memory/graph.rs:204 | 1 | async | session-memory `DatabaseFactStore<'_>`, `ProjectFactStore<'_>` (fact_store/mod.rs:820,1063) | session-memory 5, automation-runtime 2 |
| FactStore | memory/traits.rs:35 | 9 | async | session-memory `DatabaseFactStore<'_>`, `ProjectFactStore<'_>` (fact_store/mod.rs:126,906) | session-memory 15 (10) |
| ProjectMemoryFactStore: FactStore | memory/traits.rs:89 | 30 | async | same two (fact_store/mod.rs:310,929) | automation-runtime 18, session-memory 15, privacy 1 |
| NativeIntegrationStore | native_integration.rs:122 | 17 | **sync** | agent-hosts `DaemonNativeIntegrationStore`, `Shared...` (native_integration/store.rs:277,728) | agent-hosts 5, application 4, daemon-service 1 |
| ObservationStore | observation/mod.rs:1163 | 7 | async | global-db `GlobalDbObservationStore` (observation_adapter.rs:1471); sessions `MemoryObservationStore` (admission/mod.rs:831) | sessions 6, global-db 4, 5 others 1-2 (tracedecay tests 23) |
| ObservationProjectionStore | projection.rs:726 | 5 | async | global-db `GlobalDbObservationStore` (observation_adapter.rs:2447) | global-db 4 (tracedecay tests 21) |
| RetrievalAnchorDispositionStore | retrieval_anchor.rs:389 | 5 | async | runtime-core `Database` (db/retrieval_anchor_authority.rs:481) | runtime-core 3 |
| GraphPublicationStoreV1 | runtime/graph_publication/store.rs:13 | 12 | **sync, `&mut self`** | rusqlite-runtime `GraphPublicationExactSqlStorage` (repository/graph_publication/exact.rs:542); graph-db `RecordedAuthority` (registry/publication.rs:2771) | graph-db 26, store-runtime 6 |
| RetainedGraphStoreLeaseV1 | runtime/identity.rs:413 | 3 | sync | runtime-core `CanonicalCodeGraphStoreLeaseV1`, `CanonicalGraphStoreLeaseV1` (shard_runtime/registry/graph.rs:87,113) | graph-db 14, store-runtime 10 |
| RetainedGraphStoreOwnerAttachmentV1 | runtime/identity.rs:426 | 4 | sync | runtime-core `CanonicalGraphStoreOwnerAttachmentV1` (graph.rs:127) | graph-db 17 |
| RuntimeRequestProbeV1 | runtime/ports.rs:30 | 5 | sync | 13 prod impls (writer BatchProbe, DatabaseCheckpointProbe, RuntimeFactProbe, RuntimeObservationProbe, ExternalSourceRuntimeProbe, QueryProbe, Replay*Probe, GraphPublication/RetirementProbe, RecoveryRuntimeProbe, graph-db TestProbe) + 21 test impls | rusqlite-runtime 37, store-runtime 15, runtime-core 12 |
| SessionTemporalCapabilityProvider | session/common.rs:25 | 1 | sync | session-temporal-store `SessionTemporalStore<'_,..>` (store.rs:543) | |
| SessionTemporalOperation | session/common.rs:390 | 0 | marker | | |
| SessionRefreshStore | session/refresh.rs:715 | 14 | async | `SessionTemporalStore` (store.rs:587) | session-memory 2, session-runtime 1 (tracedecay tests 7) |
| SessionRetrievalStore | session/retrieval.rs:220 | 4 | async | `SessionTemporalStore` (store.rs:559) | |
| TranscriptStore | transcript.rs:113 | 2 | async | session-memory `GlobalDbTranscriptStore<D>` (transcript.rs:112); sessions `RestartableStore` (runtime/ingest/scheduler.rs:232) | sessions 7, host-admission 2 |

Totals: 11 async traits / 88 methods; 8 sync traits / 55 methods. Of the sync ones, only three are
persistence ports (GitIndexTransactionStore 13, NativeIntegrationStore 17, GraphPublicationStoreV1 12 = 42
methods); the rest are lease/probe/capability helpers. Every persistence trait has exactly one production
implementation (two for TranscriptStore/ObservationStore/FactStore where a wrapper or in-memory admission
store exists). The "port" layer is one-impl-per-trait almost everywhere.

The second, non-trait store surface in tracedecay-store is the closed-enum runtime protocol:
- `RuntimeReadOperationV1` (runtime/ports.rs:269): CurrentWatermark, MaintenanceTelemetry, ReaderHealthLease,
  TemporalHealth, GraphStats, GraphNode, GraphSearch, GraphQuickCheck, Repository{op}.
- `RepositoryReadOperationV1` (runtime/repository_read.rs:44): Profile | Project | ExternalSource | Code;
  `ProjectReadOperationV1` (:251) Fact | Observation | Diagnostics | RetrievalAnchor; `ProfileReadOperationV1`
  (:236) CurrentConfiguration | ConfigurationRevision.
- `RepositoryWritePayloadV1` (runtime/operation.rs:777-793): 15 variants (Configuration, Fact, Observation,
  ObservationBatch, ObservationCursorAdvance, RemoteObservationReplay, RemoteWriterFenceInstall, Diagnostics,
  ExternalSource, ExternalSourceBatch, ExternalSourceProjection, ExternalSourceAcquisition,
  RetrievalAnchorDisposition, RetrievalAnchorDerivative, GitIndexTransaction); `required_durability()` is
  always `DurabilityClassV1::Full` (operation.rs:817-819).
- These enums are `Serialize/Deserialize` with `deny_unknown_fields` and comments call them a
  "store-protocol API" (repository_read.rs:262-265): a wire shape, not just an in-process call.
- Read is SYNC: `StoreRuntimeClientLease::dispatch_read(&self, RuntimeReadRequestV1, &dyn Probe) -> Result`
  (runtime-core shard_runtime/registry.rs:1117-1130) -> `PhysicalRuntimeAttachment::dispatch_read`
  (attachment.rs ~250, sync) -> `RepositoryRuntimePhysicalAttachment::dispatch_read`
  (rusqlite-runtime repository/attachment.rs:543-567: lock state, `readers.acquire_for_dispatch`,
  `begin_snapshot`, `snapshot.execute`), i.e. the caller thread parks on the reader-worker reply.
  Submit is ASYNC: `dispatch_submit_authorized(...).await` (registry.rs:1072-1101), requires an ambient
  asupersync `Cx` (`native_runtime::cx().ok_or(RuntimeUnavailable)` registry.rs:1078-1081).
- Production `dispatch_read` callers (7 sites): global-db observation_adapter.rs:1800; session-memory
  fact_store/runtime.rs:257 (one helper used by current/as-of/lineage queries); session-memory
  external_source_store.rs:783,821,849,914; store-runtime remote_query.rs:137. Through
  `DatabaseRuntimeClientV1::dispatch_read` (db/connection/registry.rs:108-114).
  Example of the #4085 shape: `impl FactStore for DatabaseFactStore` `async fn query_fact_current`
  (session-memory fact_store/mod.rs:163-170) calls the sync `runtime::query_fact_current` inline when a
  retained runtime exists, otherwise awaits `begin_memory_read_transaction` (async engine). Two read paths
  for one port method, chosen at runtime by `retained_fact_runtime(self.db)?`.

Engine/wire leaks onto the "engine-free" tracedecay-store surface:
- SQL text exported as contract constants: `CANONICAL_BODIES_TABLE_SQL`, `LOAD_CANONICAL_BODY_SQL`,
  `UPSERT_CANONICAL_BODY_SQL` (canonical_body.rs), `GENERATION_DIAGNOSTICS_SCHEMA_DDL`,
  `RETRIEVAL_ANCHORS_SCHEMA_DDL`, `CLEAR_CURRENT_DIAGNOSTIC_PUBLICATION_SQL`, `CLEAR_CURRENT_DIAGNOSTICS_SQL`
  (schema.rs; re-exported lib.rs:238-241).
- SQLite WAL budget in the contract: `WAL_SOFT_LIMIT_BYTES = 32 MiB`, `WAL_HARD_LIMIT_BYTES = 256 MiB`
  (runtime/operation.rs:31-32), `WalBudgetV1`.
- SQLite corruption detection by error-message substring in the contract crate:
  `error_chain_is_sqlite_disk_image_corrupt` matches "database disk image is malformed", "file is not a
  database", "malformed database schema", "SQLITE_CORRUPT" (projection.rs:687-702); used by
  `ProjectionStoreError::storage` to choose `Corrupt` vs `Storage` (projection.rs:644-666).
- `PROJECTION_TERMINAL_RETRY_MICROS = i64::MAX` (projection.rs:101) parks a projection queue row forever;
  consumed by global-db observation_projection/state.rs:302, rebuild.rs:282,1198,1217.
- Grafeo file naming: `graph_store_locator_path` derives `<stem>.grafeo` beside the relational file
  (runtime/identity.rs:507-527).
- `StoreShardScopeV1::Code{project_id, repository_id, scope: CodeShardScopeV1::{Worktree, Branch, Snapshot}}`
  (runtime/identity.rs:116-152) puts worktree/branch identity into the store-shard id type even though no
  physical store exists for it (see resolver below).

### B. tracedecay-contracts storage ports

24 sync Work/Workflow storage ports, 77 methods, all `Send + Sync` sync methods returning `Result`:

| port | def | m | prod impl (rusqlite-runtime unless noted) |
|---|---|---|---|
| WorkAttemptStoragePort | work_attempt.rs:85 | 10 | WorkSqliteStorage (work_attempt.rs:88) |
| WorkSynthesisAdmissionStoragePort | work_attempt/synthesis_admission.rs:25 | 3 | WorkSqliteStorage (work_attempt.rs:453) |
| WorkAttemptEvidenceReadPort | work_artifact_hydration.rs:52 | 1 | WorkSqliteStorage (work_attempt.rs:484) |
| WorkAttemptEffectStoragePortV1 | work_attempt_effect.rs:160 | 3 | WorkSqliteStorage (work/effect_holder.rs:18) |
| WorkDuplicateAdjudicationPortV1 | work_duplicate_adjudication.rs:160 | 3 | WorkSqliteStorage |
| WorkLeakAdjudicationStoragePortV1 | work_leak_adjudication.rs:233 | 2 | WorkSqliteStorage |
| WorkOwnerObservationStoragePortV1 | work_owner_observation.rs:135 | 2 | WorkSqliteStorage; application ConflictMarkerStorage |
| WorkPlacementStoragePort | work_placement.rs:50 | 4 | WorkSqliteStorage |
| WorkProductAttemptAdmissionPortV1 | work_product/attempt_admission.rs:186 | 3 | WorkSqliteStorage |
| WorkProductEventPortV1 | work_product/mutation/contracts.rs:147 | 2 | WorkSqliteStorage (+ blanket `&E`) |
| WorkEvidenceReadPortV1 | work_product/query.rs:111 | 2 | WorkSqliteStorage (+ `&E`) |
| WorkHistoryReadPortV1 | work_product/query.rs:280 | 1 | WorkSqliteStorage (+ `&H`) |
| WorkGraphReadPortV1 | work_product/read.rs:659 | 1 | WorkSqliteStorage, AuthorizedWorkProductReadStorageV1 (+ `&P`) |
| WorkEvidenceRootReadPortV1 | work_evidence.rs:114 | 1 | WorkSqliteStorage, AuthorizedWorkProductReadStorageV1 |
| WorkAttemptReceiptReadPortV1 | work_evidence.rs:155 | 1 | WorkSqliteStorage, AuthorizedWorkProductReadStorageV1 |
| WorkRetryStoragePortV1 | work_retry.rs:318 | 2 | WorkSqliteStorage |
| WorkRunControlStoragePort | work_run_control.rs:109 | 9 | WorkSqliteStorage |
| WorkflowDefinitionAuthorityPort | workflow_coordination.rs:195 | 5 | WorkflowSqliteAuthority (workflow.rs:110) |
| TaskHandoffAuthorityPort | workflow_coordination.rs:1060 | 2 | WorkflowSqliteAuthority |
| WorkflowEffectAuthorityPortV1 | workflow_effect.rs:650 | 3 | WorkflowSqliteAuthority |
| WorkflowFanOutCensusStoragePort | workflow_fan_out_census.rs:76 | 6 | WorkflowSqliteAuthority |
| WorkflowRunStoragePort | workflow_run.rs:53 | 5 | WorkflowSqliteAuthority |
| WorkflowArtifactStorePort | workflow_run.rs:367 | 2 | WorkflowSqliteAuthority |
| HandoffOpenAuthorityPort | handoff.rs:659 | 4 | HandoffOpenSqliteAuthority (handoff.rs:101) |

Reference counts (prod, by crate): every port is referenced only by contracts (2-18) + rusqlite-runtime
(2-5) + occasionally daemon-service (WorkAttemptStoragePort 5, WorkAttemptEffectStoragePortV1 5,
WorkflowFanOutCensusStoragePort 6, WorkflowRunStoragePort 8, WorkflowArtifactStorePort 3) or application
(WorkOwnerObservation 5). Test refs concentrate in rusqlite-runtime (Workflow* 17-32 each).
Services that take them as generics (`tracedecay-contracts`): WorkAttemptService<S> (work_attempt.rs:533,
methods status/list/request_cancellation/resume/mark_running/... all sync), WorkPlacementService<S>
(work_placement.rs:140), WorkRunControlService<S> (work_run_control.rs:273), WorkArtifactHydrationService<S>
(work_artifact_hydration.rs:122), WorkflowRunService<P> (workflow_run.rs:165), WorkflowDefinitionService<P>
(workflow_coordination.rs:368), TaskHandoffService<P> (:1111), HandoffOpenService<A,T> (handoff.rs:1019).
Construction sites in daemon-service/src/invocation/work/*: outcome.rs 19, workflow_run_control.rs 14,
preparation.rs 8, request_dispatch.rs 6, attempt_operations.rs 3, evidence_retrieval.rs 2,
intelligence.rs 2, workflow_fan_out.rs 1; application/src/work/registered.rs 4.

Concrete backing: `WorkSqliteStorage { retained: RetainedExactSqlCapability }` (rusqlite-runtime
work.rs:34-36), `WorkflowSqliteAuthority { retained }` (workflow.rs:49-51; constructor validates schema,
`load_definition_source` issues `handle().query(ExactSqlStatement::new("SELECT ...".to_owned(), ..),
Duration::from_secs(5))` workflow.rs:70-110). `RetainedExactSqlCapability { handle: ExactSqlHandle,
_guard: Arc<dyn Send+Sync> }` (repository/retained_exact_sql.rs:11-34). So the sync contracts port is
backed by raw SQL strings on the exact-SQL writer/reader threads (ground-3).

Other contract ports touching storage (shape): StoreSizeTelemetryPort (storage/telemetry.rs:432, async,
impl SqliteStoreSizeTelemetryPort + maintenance GuardedStoreTelemetryPort), ProjectRegistryReadPort
(project_registry.rs:217, async, impl daemon-service DaemonProjectRegistryReadService), ProfileIdentityReadPort
(profile_identity.rs:6, sync), CommitFactPort/CurrentFactsPort/FactAsOfPort/FactCurrentPort/FactLineagePort/
RetrievalAnchorPort (memory/canonical.rs:81-386, async, 1 method each), ObservabilityRecordPort/QueryPort
(observability.rs:47,51, async), StorageDoctorPort (doctor/sources.rs:2113, async), GitIndexTransactionPort
(git/transactions.rs:372, sync), NativeIntegrationPort (git/native_integration.rs:372, sync), NativeWorktreePort
(git/worktree.rs:496, sync), AuthorizedScopeSetPort (git/worktree.rs:489, sync), ~20 sync Remote*PortV1
(remote/*.rs) incl. RemoteReplaySpoolPortV1 (impl RemoteSqliteStorageV1) and RemoteReplayTransactionPortV1
(impl store-runtime DaemonRemoteReplayTransactionAuthorityV1). Contracts total: 125 `pub trait`s (non-test src); the
storage-adjacent sync ones dominate the Work/Workflow/Git/Remote families, async dominates retrieval/
feedback/doctor/memory.

### C. runtime-core / store-runtime kernel ports

- `StoreRuntimeResolver` (shard_runtime/registry/ports.rs:98): `resolve`, `resolve_graph`, both
  `Pin<Box<dyn Future>>` but the only prod impl `LocalStoreRuntimeResolverV1` does all work synchronously
  inside the future (`resolve_key` reads `/proc/self/mountinfo` per call, store_locator_resolver.rs:219-230;
  canonicalize/symlink_metadata/marker read).
- `ShardRuntimePublisher` (ports.rs:115): `publish`, impl `LifecycleShardRuntimePublisher` (ports.rs:124).
- `PhysicalRuntimeAttachment` (attachment.rs:70, 11 methods, mixed): snapshot/opened_file_identity/drain/
  close_and_join/exact_sql_handle/storage_page_counts/dispatch_read are sync; run_bounded_incremental_compaction,
  run_checkpoint, dispatch_submit are boxed futures taking `&asupersync::Cx`. Prod impl: rusqlite-runtime
  `RepositoryRuntimePhysicalAttachment` (ports.rs:462). Every default body returns
  "physical runtime has no ... port" errors (attachment.rs:100-190), i.e. a capability-by-failure trait.
- `VerifiedGraphRuntimePortV1` (shard_runtime/verified_graph.rs:11, 6 sync methods incl.
  `publish_verified_manifest`, `reconcile_verified_manifest` returning `Result<VerifiedGraphSnapshot, GraphDbError>`):
  graph-db types (GraphGenerationManifest, GraphDbError) on the kernel port.
- `RemoteRecoveryProjectLifecycle` (store-runtime session_registry.rs:81): one boxed-future method,
  installed by the root.
- `registered_schema::Installer` (runtime-core ports.rs:21-234): a global `fn` pointer slot registered by
  store-runtime at the top of `DaemonSessionRuntimeRegistryV1::open()`; unregistered = fail-closed open
  (runtime-core lib.rs:28-35). Process-global mutable port.

### D. Sync-port-over-async-DB bridges (third execution pattern)

Besides "sync port parks on exact-SQL reply" (Work/Workflow) and "async port parks inside its poll on
dispatch_read" (FactStore/ObservationStore), there is "sync port fronted by a dedicated std::thread actor that
`native_runtime::block_on`s the async RegisteredGlobalDb":
- `DaemonGitIndexTransactionStore` (code-index-runtime git_transactions/store.rs:1-30, 81-84, thread at :169,
  block_on at :433): bounded `sync_channel` capacity 64, 5 s per-call timeout; one actor per project
  sessions.db path (`GitIndexTransactionStoreRegistry::ensure` keyed by `database.db_path()`,
  git_transactions/registry.rs:38-68).
- `DaemonNativeIntegrationStore` (agent-hosts native_integration/store.rs:1-30, 130-147, block_on :567):
  same design, one actor per project DB (`NativeIntegrationStoreRegistry`, native_integration/registry.rs:84-110).
- `DaemonRemoteReplayTransactionAuthorityV1` (store-runtime remote_replay_transaction.rs:29 CHANNEL_CAPACITY
  128, thread :79, `native_runtime::block_on(lease.runtime_client().dispatch_submit(..))` :389,406,564).
=> thread count scales with mounted projects x actor kinds; each actor runs its own block_on root (outside
the guard per ground-1).

---------------------------------------------------------------------------------------------------

## Flow

### F1. cwd -> profile root
1. Process boundary resolves `ProfileRoot::from_env()` once (runtime-core config.rs:78-112):
   `TRACEDECAY_DATA_DIR` else `$HOME/.tracedecay`; `TRACEDECAY_GLOBAL_DB` pins global.db anywhere
   (config.rs:199-204). Nextest fixture profiles get `<data>/nextest/<hash>` (config.rs:295-330).
   Daemon may get `--profile-root` (`from_env_with_data_dir`, config.rs:85-91).
2. Profile identity: `profile_identity::load_or_create(profile_root)` (daemon-identity profile_identity.rs:79-...)
   reads/creates `<profile>/profile-identity.json` -> `{brain_id, profile_id}`; every `StoreShardIdV1`
   carries both and the resolver rejects a mismatch (store_locator_resolver.rs:254-259).

### F2. cwd -> project root (discovery)
3. `ProfileRoot::discover_project_root(start)` (config.rs:246-268) walks up to the git worktree root,
   accepting a dir when `has_path_local_profile_store` (shard `projects/<path_local_id>/tracedecay.db` exists)
   or, at the worktree root, `has_repository_profile_store` (the `.git/tracedecay-project.json` marker names a
   shard dir this profile holds) (storage/layout.rs:127-150). Ambient roots ($HOME, `/`) refused
   (config.rs:216-218). Discovery deliberately uses the path-local id, not the repository id
   (layout.rs:94-106 doc).

### F3. project root -> project id (identity)
4. Daemon/CLI open calls `TraceDecay::resolve_store_layout_for_authority` (tracedecay-project
   lifecycle/identity.rs:86-170):
   a. `refuse_retired_checkout_layout` (storage.rs:256-273): a checkout `.tracedecay/{enrollment.json,
      config.json,tracedecay.db}` => typed ResetRequired.
   b. `storage::resolve_persisted_layout` (layout.rs:198-222): (i) `.git` common-dir marker
      `<git_common_dir>/tracedecay-project.json` {schema_version 1, project_id, git_common_dir}
      (identity.rs:17-20, 30-125) wins; (ii) else a shard already minted at
      `projects/<default_profile_project_id(root)>/` with tracedecay.db or store_manifest.json.
   c. `git_common_dir_outcome` (worktree.rs:66-80); a blocked concurrent topology walk is a retryable
      `REPOSITORY_DISCOVERY_DEFERRED` route error, never "not a repo" (identity.rs:96-107).
   d. Registry: `RegisteredGlobalDb::resolve_project_store_by_identity(root, git_common_dir)` over
      global.db `project_aliases` (path alias + `git-common-dir:` alias, project_registry.rs:31,1587).
   e. First touch only (`allow_default_identity`): refuse OS-temp roots unless the profile is itself temp
      (identity.rs:131-143; global-db `ephemeral_root_rejection` project_registry.rs:251), try
      `adopt_moved_nongit_project`, else `default_profile_sharded_layout`.
5. Id minting is a single function: `default_profile_project_id(root)` = `proj_` + first 16 hex of
   sha256(canonical path of `repository_identity_root(root)` or root) (layout.rs:80-85,113-118).
   `repository_identity_root` returns the primary checkout for a linked-worktree ROOT whose common dir is
   a plain `<repo>/.git` (worktree.rs:109-155); a subdirectory, bare repo, or submodule is its own identity.
6. Init persists identity to the `.git` marker (`write_repository_identity_marker`, identity.rs:197-229)
   and `registered_enrollment_roots` self-heals it on open (global-db project_registry.rs:2109-2155).
   Marker conflict (copied `.git` whose original common dir is live and names the same id) fails closed
   (identity.rs:89-118).

### F4. project id -> registered root
7. `RegisteredGlobalDb::upsert_code_project(project_id, root, git_common_dir, remote, default_branch)`
   (project_registry.rs:750-955) is "the one door" minting `code_projects`/`project_aliases` rows; refuses
   ephemeral roots; two project ids claiming one root or common dir => typed ResetRequired.
   Tables (global-db schema_stages.rs:55-106): code_projects(project_id PK, canonical_root, git_common_dir,
   ...), project_aliases(alias_path PK -> project_id), store_instances(store_id, project_id, store_kind,
   storage_mode, store_relpath, manifest_relpath), graph_scopes(graph_scope_id, project_id, store_id,
   branch_name, parent_scope_id, writable), store_artifacts(store_id, artifact_kind, relpath, size_bytes,
   schema_version).
8. Enrollment roots: `registered_enrollment_roots` collects [root, layout.project_root, registry context
   roots] and filters via `enrolled_project_roots`, which maps every candidate through
   `repository_identity_root` (layout.rs:44-66), so linked worktrees collapse to the primary checkout.

### F5. project id -> store path (resolution)
9. `DaemonSessionRuntimeRegistryV1::project_session_store(project_id, roots)` (store-runtime
   session_registry/mounts.rs:1488-1495) registers `LocalProjectEnrollmentAuthorityV1{project_id, roots}`
   with the resolver then mounts. `register_project_authority` accepts an identical re-registration and
   REFUSES a different root set for the same project (`DuplicateProjectAuthority`,
   store_locator_resolver.rs:166-187).
10. `LocalStoreRuntimeResolverV1::resolve_key_inner` (store_locator_resolver.rs:249-336) maps
    `StoreShardScopeV1`:
    - Profile -> `<profile>/global.db`
    - ProfileMemory -> `<profile>/user-memory.db` (session-memory memory/user.rs:7)
    - ProfileSessions -> `<profile>/user-sessions.db`
    - RemoteNode{node_id} -> `<profile>/remote/nodes/<sha256(node_id)>/remote.db` (:338-365)
    - Project{id} -> `<profile>/projects/<id>/tracedecay.db`; ProjectSessions{id} -> `.../sessions.db`
      via each enrollment root: canonicalize, local-FS check, `require_matching_repository_identity`
      (marker must name the same id; absent marker accepted) (:745-755), then
      `storage::profile_sharded_layout` (:409-468). Path never comes from the root, only from the id.
    - Code{..} -> `MissingCodeStoreAuthority`: "Code scopes are namespaces inside their project graph
      runtime, never physical stores" (:332-336).
    Every resolve requires the profile root and enrollment root on a local filesystem (network FS =>
    typed unavailable; Linux reads mountinfo, macOS statfs MNT_LOCAL, Windows drive type) (:935-1290).
11. `resolve()` (:474-546) additionally checks the opener's `DatabaseAuthority.canonical_database_path()`
    equals the locator, requires an existing canonical regular file for Open and a prospective (absent)
    file for Initialize.
12. Graph pairing: `resolve_graph_key` (store_locator_resolver/graph.rs:17-55) maps the relational locator
    to `<store_root>/<stem>.grafeo`; allowed only for Project, ProjectSessions, ProfileMemory,
    ProfileSessions (runtime-core registry/graph.rs:1030-1042). Code scopes must go through
    `retain_code_graph_store` on the project key (graph.rs:231) -> namespace in the project graph.
13. Shard runtime publication: `StoreRuntimeRegistry` singleflights by `StoreRuntimeKey{shard_id,
    incarnation}` (registry.rs:84-117), `LifecycleShardRuntimePublisher::publish` builds a `ShardRuntime`
    + `RepositoryRuntimePhysicalAttachment` (writer thread + reader pool), creating initialized schema for
    Profile/session/RemoteNode scopes (ports.rs:400-458). Capacity: `MAX_PROJECT_CODE_OPEN_RUNTIMES = 8`,
    default 4 (registry/capacity.rs:11-12) for Code-scoped runtimes; profile shards are pinned
    (`is_profile`, registry.rs:107-113).
14. Client access: `StoreRuntimeClientLease` (registry.rs:120-245) holds `Arc<ShardRuntime>`,
    `Arc<dyn PhysicalRuntimeAttachment>`, locator, `opened_file_identity`, optional `DatabaseAuthority`, and a
    map of database facade attachments. `Database` (db/connection.rs:62-66, `unsafe impl Send/Sync` :70-71)
    wraps a lease; `DatabaseRuntimeClientV1` exposes dispatch_submit (async, needs write authority) and
    dispatch_read (sync) (db/connection/registry.rs:60-125). Purpose adapters
    (`work_storage`, `workflow_storage`, `authorized_scope_set_storage`, `handoff_open_storage`,
    `remote_storage`, ... db/purpose.rs:9-120) mint a `RetainedExactSqlCapability` per call after checking
    the handle's binding+locator equal the registered ones.

### F6. Daemon composition (where ports get wired)
15. `DaemonEngine` (tracedecay/src/daemon/engine.rs:50-110) -> `store_administration: StoreAdministration`
    (branch_admin.rs:448-490) holding:
    - `session_runtime_registries: HashMap<PathBuf(profile root), {identity, OnceCell<Arc<DaemonSessionRuntimeRegistryV1>>}>`
      (branch_admin.rs:330-336); opened lazily via `DaemonSessionRuntimeRegistryV1::open(identity)`
      (branch_admin.rs:349-358; store-runtime mounts.rs:136,147).
    - `gate: Arc<StoreWriterGates>` (writer_gate.rs: daemon RwLock + per-data_root {owner, content}
      tokio Mutexes; comment says hook and memory writes bypass it, writer_gate.rs:32-40).
    - `project_servers: DatabaseOwnerRegistry` keyed by `ProjectServerKey{owner: StoreOwnerKey,
      project_root, scope_prefix}` with route aliases `ProjectRouteKey{profile_root, global_db_path,
      project_path, scope_prefix}` (project_open_admission.rs:22-38). `StoreOwnerKey{profile_root,
      global_db_path, project_id, store_root, graph_db_path}` (session-runtime store_owner.rs:7-13).
      Doc: "distinct linked worktrees retain exact root-bound servers over one shared StoreOwnerKey"
      (project_open_admission.rs:28-31).
    - `git_index_transaction_services`, `native_integration_services` (actor registries, section D),
      `session_temporal_refresh_schedulers`, `store_telemetry_sampling`, `session_sync_service`,
      `host_admission_brokers`, automation schedulers keyed by ProjectServerKey.
16. `DaemonSessionRuntimeRegistryV1` (store-runtime session_registry.rs:2661-2719) owns: resolver,
    `StoreRuntimeRegistry` (kernel), `GraphDbRegistry`, profile_database / profile_memory / profile_sessions
    owners, remote nodes, `project_owners: BTreeMap<ProjectId, ProjectRuntimeOwnerStateV1>`
    (:293-295, states Opening/Ready{sessions, memory}/ReplacingSessions/RecoveryRequired/Retiring/Faulted
    :886-898), per-project-shard code graph publication gates ("every worktree/branch scope of a project
    stages into the one shared staging store", :2690-2701), schema convergence, `reset_required_stores`.
    Ceilings: MAX_RETAINED_PROJECT_RUNTIME_OWNERS = 4096 (:107, comment: memory is "ungoverned",
    "idle-project hibernation is follow-up work"), PROJECT_GRAPH_OWNER_ADMISSION_DEMAND = 3 (:114),
    MAX_RETAINED_GRAPH_DB_OWNERS = 2 + 3*4096 (:121).
17. Project open owners (tracedecay/src/daemon/project_open_owners.rs:320-330): `database =
    graph.db().clone()` (project tracedecay.db) and `session_db = server.project_session_db()` (project
    sessions.db, a `RegisteredGlobalDbLeaseV1`). Work: `invocation.work_runtime_registrar().register(
    project_root, session_db, WorkAuthority{project, repository, worktree,...}, ...)`
    (project_open_owners.rs:566-605) -> `DaemonWorkRuntimeRegistrar::register` keyed by project_root
    (daemon-service invocation/registrars.rs:1044-1135). So Work/Workflow rows live in
    `projects/<id>/sessions.db`, registration is per worktree route.
18. Per request: handlers call `registered.database.work_storage()` / `.workflow_storage()`
    (daemon-service invocation/work/*.rs, handoff.rs:226-249, work_attempt_exec.rs:565) -> fresh
    `WorkSqliteStorage` -> contracts service -> sync port -> exact SQL. The whole Work request future runs
    under `native_runtime::off_native_thread(request)` (invocation/work.rs:157, #4052) and
    observability_producer.rs:243.
19. Store adapters for async ports are borrowed views constructed ad hoc (`DatabaseFactStore::new(&db)`,
    `GlobalDbObservationStore`, `SessionTemporalStore`, `DiagnosticsStore`, `GlobalDbConfigurationControlStore`):
    ~40 production construction sites across ~30 files (privacy_remediation.rs 7, global-db
    configuration/store.rs 4, session-memory fact_store/envelope.rs 3, application feedback 4, mcp handlers
    4, daemon project_open_owners 2, ...). There is no single composition root for store ports.
20. Second instantiation path: non-daemon processes (CLI init/open, branches, doctor, storage report,
    privacy remediation, profile refresh journeys) construct `DaemonSessionRuntimeRegistryV1` themselves:
    `join_standalone_session_registry` (store-runtime standalone_session.rs:20-41, process-global
    `SingleFlightWeakRegistry<PathBuf, ..>` keyed by canonical profile root; called from tracedecay-project
    lifecycle/mod.rs:280,522,717, branches.rs:58) and direct `::open` at cli commands/storage.rs:243,
    tracedecay/src/doctor.rs:66, daemon branch_admin.rs:354 + session_runtime_shutdown.rs:56,113,224,
    privacy_remediation.rs:688,808,916,1015, daemon-service retained_owner/profile_refresh_journeys.rs:122,247.
    Exclusion between those and the daemon relies on file locks (Grafeo exclusive lock for the profile
    session-relation graph, standalone_session.rs:3-6; daemon-authority.lock; lifecycle.lock).

---------------------------------------------------------------------------------------------------

## Store Inventory (from source; identity/layout angle)

Profile root `P` = `$TRACEDECAY_DATA_DIR` or `$HOME/.tracedecay` (config.rs:7-19, 78-112).

| store / artifact | engine | path | unit | opener/owner | notes |
|---|---|---|---|---|---|
| profile registry/config | SQLite (registered schema) | `P/global.db` (or `$TRACEDECAY_GLOBAL_DB`) | per profile | DaemonSessionRuntimeRegistryV1 `profile_database` (RegisteredGlobalDbOwnerV1) | shard scope Profile; pinned in kernel registry; holds code_projects/aliases/store_instances/graph_scopes/store_artifacts, configuration revisions. No paired .grafeo (Profile not graph-eligible). |
| profile sessions | SQLite + Grafeo | `P/user-sessions.db`, `P/user-sessions.grafeo` | per profile | `profile_sessions` owner; session-relation graph exclusive file lock | ProfileSessions scope |
| profile memory | SQLite + Grafeo | `P/user-memory.db`, `P/user-memory.grafeo` | per profile | `profile_memory` owner | conversations without a code project |
| remote node | SQLite | `P/remote/nodes/<sha256(node_id)>/remote.db` | per Remote Brain node | remote_nodes map; MAX_RETAINED_REMOTE_NODE_OWNERS = MAX_REGISTERED_REMOTE_NODES | startup remounts every discovered remote.db (session_registry.rs:123-130) |
| project graph/facts | SQLite | `P/projects/<proj_id>/tracedecay.db` (+ -wal, -shm) | per project id (= per repository) | project open (root crate, project_server_lifecycle.rs not read) via `graph.db()` | Project scope; RetrievalAnchorDispositionStore, facts, observations (?), diagnostics live here per ProjectReadOperationV1 |
| project graph | Grafeo | `P/projects/<id>/tracedecay.grafeo` (+ Grafeo WAL sidecar) | per project | GraphDbRegistry | all Code{worktree/branch} scopes are namespaces inside it (registry/graph.rs:1070-1087) |
| sealed code-graph generations | Grafeo | `P/projects/<id>/tracedecay.sealed/<64hex>/{generation.grafeo, sealed.json, sealed.checked}`, `.staging-*` | per published generation | graph-db sealed_store.rs:210-216,500-511 | census tracks head/superseded/abandoned staging/unrecognized (sealed_store.rs:513-530) |
| graph replay pool | files | `P/projects/<id>/tracedecay.graph-replay/` | per project | code-index-retention | GRAPH_REPLAY_POOL_ACQUIRE_BUDGET 50 ms (code_index_generations.rs:132) |
| project sessions/config/work | SQLite + Grafeo | `P/projects/<id>/sessions.db`, `sessions.grafeo` | per project | `project_owners[id].sessions` (RegisteredSessionOwnerV1) | ProjectSessions scope; hosts sessions, project configuration, Work/Workflow/handoff tables, git-index transactions, native integration journals; `store_manifest.sessions_schema_digest` records admission |
| code-index scope | files | `P/projects/<id>/code-index-v1/<sha256(canonical worktree root)>/` with `active-code-generation-v1.json`, `code-generations-v1/<generation files>`, retention receipts/quarantine, `freshness_witness.v1`, `scope-root.v1` | **per worktree root** | code-index-runtime scheduler | scope_roots.rs:254-267, code_index_generations.rs:137-164,790-859; stranded-scope collection only after 7 days idle (DEFAULT_STRANDED_SCOPE_MINIMUM_AGE_SECS, :124) |
| code generation segments | files | `P/projects/<id>/code-index-v1/code-generation-segments-v1/` | per project (shared across worktree scopes) | code-index-retention | content-addressed so identical files across worktrees are stored once (:794-815) |
| text artifacts | files | `code-index-v1/code-text-artifacts-v1/`, `<scope>/code-text-artifact-staging-v1/` | per project / per scope | | storage_report.rs classifies these as text_artifacts |
| misc per project | files | `store_manifest.json`, `branch-meta.json`, `response-handles/`, `lcm-payloads/`, `dashboard/` (automation_runs.jsonl, artifacts), `hook-delivery-spool/`, `hook_analytics.jsonl`, `sync.lock`, `.branch-add.lock`, `.dashboard.durable-directory.lock` | per project | various | StoreLayout::new (storage/paths_and_io.rs:850-884) |
| repository identity marker | JSON | `<git_common_dir>/tracedecay-project.json` | per repository (shared by all linked worktrees) | init/open | schema_version 1, private 0600, atomic write (identity.rs:197-229) |
| profile identity | JSON | `P/profile-identity.json` | per profile | daemon-identity | brain_id + profile_id; missing in existing profile => ResetRequired (profile_identity.rs:84-101) |
| daemon authority | lock + JSON | `P/daemon-authority.lock`, `P/daemon-authority.json` (Windows: `P/daemon-authority/`) | per profile | daemon-identity authority.rs:26,93-117 | single-daemon election, epoch CAS |
| other profile files | | `P/lifecycle.lock` (lifecycle_lease.rs:12), `P/daemon.sock`, `P/remote-tls.sock`, `P/daemon.out.log`, `P/daemon.err.log`, `P/config.toml`, `P/host-components/`, `P/agent_managed/`, `P/maintenance/` (cold_store CHECKPOINT_DIRECTORY), `P/response-handles/` + `P/hook_analytics.jsonl` for unenrolled checkouts | per profile | | |
| retired checkout layout | | `<checkout>/.tracedecay/{enrollment.json,config.json,tracedecay.db}` | | never read; ResetRequired; reset deletes | storage.rs:233-273 |

Growth/retention seen from this angle: the only per-worktree on-disk unit is the code-index scope dir plus
its sealed generations; scope collection requires a Git worktree inventory (bounded by
`MAX_GIT_WORKTREES_PER_SCOPE_INVENTORY`, scope_roots.rs:269-280) and 7 days of quiet. Registry rows have a
reap planner (`plan_registry_reap` / `apply_registry_reap`, project_registry.rs:1935,2074). `storage_report`
(maintenance retention/storage_report.rs:1-25) samples sizes by `stat`; its header records "The owner's profile
was 91GB". Store sizes are otherwise not bounded by any identity-level policy.

Failure containment at the identity layer:
- A bad `.git` marker (unparseable, wrong schema, conflicting live common dir) fails identity for every
  worktree of that repository (Config error) (identity.rs:30-125).
- Resolver refusals are typed `LocalStoreLocatorUnavailableReasonV1` (MissingProjectEnrollmentAuthority,
  EnrollmentProjectMismatch, InvalidEnrollment, network FS, symlinked profile root, ...).
- `reset_required_stores` keeps a refused shard unmounted and re-admits on each open
  (session_registry.rs:2703-2706).
- DuplicateProjectAuthority is per registry process: a second, different root set for one project id is
  refused until the first is withdrawn.

---------------------------------------------------------------------------------------------------

## Why "each agent worktree gets its own project store" (source answer)

Source says it should not, and the mechanism is consistent:
- `resolve_persisted_layout` checks the `.git` common-dir marker first (layout.rs:206-211); all linked
  worktrees share `<repo>/.git/tracedecay-project.json` (identity.rs:11-20). This repo's marker:
  `/fast/projects/tracedecay/.git/tracedecay-project.json` -> `proj_a5b3d7e3ebe14ca7`,
  git_common_dir `/fast/projects/tracedecay/.git` (read-only check).
- Fallback minting collapses linked-worktree roots to the primary checkout (layout.rs:113-118,
  worktree.rs:144-155). History: "fix(storage): stop minting project shards for unenrolled checkouts"
  615f0854b5 (2026-09-12), "fix(runtime): recognize nested linked worktree roots" a4f6df93e7 (2026-09-03),
  "delete identity/config legacy paths; refuse old layouts" 937b85d334 (2026-09-25).
- Enrollment roots collapse to the primary checkout (layout.rs:44-66).

What IS per worktree (each agent worktree multiplies these inside ONE project shard / ONE daemon):
1. `ProjectServerKey` server: one `McpServer` + owners + automation scheduler per worktree root over the
   shared `StoreOwnerKey` (project_open_admission.rs:22-38; database_owner_registry.rs:1-25).
2. Work runtime registration per project_root with `WorkAuthority{.., worktree_id}`
   (registrars.rs:1044-1135).
3. Code-index scope dir `code-index-v1/<sha256(worktree root)>/` with its own generation files
   (scope_roots.rs:254-267), kept >= 7 days after the worktree is gone.
4. A `Code{Worktree|Branch}` graph namespace inside the project `tracedecay.grafeo`, each with its own
   verified head and sealed generations under `tracedecay.sealed/` (registry/graph.rs:1070-1087);
   detached linked worktrees get a `detached-worktree/<16hex>` graph scope (worktree.rs:176-195).
5. Global registry `project_aliases` rows per worktree path (path alias + shared git-common-dir alias).
Inference: an observed "30+ project stores, one per agent worktree" is most likely 30+ per-worktree scopes/
servers/namespaces in one shard, or roots that are not linked-worktree roots (non-git dirs, subdirectory
projects, bare/submodule shapes where `primary_checkout_root` returns None, worktree.rs:117-125). The live
profile could not confirm: `~/.tracedecay` was re-created 2026-10-16 07:31 with only `lifecycle.lock`,
`maintenance/`, and `projects/proj_a5b3d7e3ebe14ca7/` (dashboard, hook-delivery-spool, hook_analytics.jsonl;
no global.db) - i.e. hooks materialize the shard dir without a daemon. No daemon process was running
(`tracedecay serve` proxies only).

---------------------------------------------------------------------------------------------------

## Boundaries (crate dependency edges relevant here)

- tracedecay-store -> domain, temporal-query (no engine, no executor).
- tracedecay-contracts -> domain, policy, tool-catalog (no engine, no executor; asupersync only in tests).
- tracedecay-runtime-core -> capture, domain, graph-db, private-fs, rusqlite-runtime, store
  (`tracedecay-runtime-core/Cargo.toml`). Contains BOTH the kernel shard registry and the `Database` facade
  and the path/identity layer. Cannot depend on global-db (cycle) so store lifecycle sits above it
  (runtime-core lib.rs:20-26).
- tracedecay-store-runtime -> asupersync, tokio(rt,sync), contracts, code-index, code-index-retention,
  code-index-runtime, daemon-identity, domain, global-db, graph-db, hooks, private-fs, runtime-core,
  rusqlite-runtime, session-memory, session-runtime, session-temporal-store, sessions, store, tool-catalog
  (`tracedecay-store-runtime/Cargo.toml`). It is the hub: nearly every storage family below it.
- tracedecay-daemon-identity -> contracts, daemon-protocol, domain, private-fs, runtime-core.
- tracedecay-project -> 29 tracedecay crates, including store-runtime and global-db.
- tracedecay (root) composes daemon state; `RemoteRecoveryProjectLifecycleV1` stays in root
  (store-runtime lib.rs:20-22).
- Executor at the boundary: store contracts are executor-agnostic but async-shaped (RPITIT `Send` futures);
  kernel boundary mixes sync `dispatch_read` with `Cx`-requiring async `dispatch_submit`; writer gates are
  tokio mutexes (writer_gate.rs:44); actor bridges use std threads + `native_runtime::block_on`.

---------------------------------------------------------------------------------------------------

## Non-Obvious Things

1. One port family, four execution classes: (a) async RPITIT ports whose impls call sync `dispatch_read`
   inside the poll (FactStore current/as-of/lineage, ObservationStore); (b) async ports over the async engine
   `Connection` (same FactStore, other methods / non-retained path); (c) sync contracts ports over exact SQL
   (Work/Workflow/Handoff/ScopeSet/Remote spool), run under `off_native_thread`; (d) sync ports fronted by
   per-DB std actor threads with their own `block_on` (git index tx, native integration, remote replay).
   Which class a call takes is decided per impl, sometimes per call (`retained_fact_runtime(self.db)?`).
2. Read/write asymmetry at the kernel: `StoreRuntimeClientLease::dispatch_read` is sync,
   `dispatch_submit_authorized` is async and refuses without an ambient asupersync `Cx`
   (registry.rs:1078-1081). Any caller of both must straddle executors.
3. `StoreRuntimeResolver::resolve` is a boxed future that does only synchronous filesystem work,
   including reading `/proc/self/mountinfo` on every resolve (store_locator_resolver.rs:219-230);
   mountinfo size scales with host mounts (Bazel sandboxes, ground-2).
4. `StoreShardScopeV1::Code` exists in the shard-id type, the kernel capacity budget
   (`MAX_PROJECT_CODE_OPEN_RUNTIMES`, `is_project_code_capacity_exempt`), and leases, yet the resolver refuses
   it as a physical store ("never physical stores of their own", store_locator_resolver.rs:332-336). A
   historical artifact of per-worktree graph sharding (graph.rs doc: "instead of recreating per-worktree
   graph sharding", store_locator_resolver/graph.rs:13-16).
5. `GraphPublicationStoreV1::retire_verified_head_replay` doc (graph_publication/store.rs:50-61) still
   describes "the per-generation code-graph namespace [where] every code generation is the permanent head",
   while `code_graph_namespace` (registry/graph.rs:1070-1080) says that layout was retired by issue #836
   (generation deliberately absent). The retirement method encodes the old layout's escape hatch.
6. tracedecay-store, the "contracts only" crate, exports SQL DDL/DML strings, SQLite WAL limits, SQLite
   corruption string-matching, and `.grafeo` naming; its read/write enums are serde wire types.
7. Work rows live in the project `sessions.db`, not in `tracedecay.db` (project_open_owners.rs:322,
   registrars.rs:1047), alongside sessions, configuration, git transactions, native integration journals.
   A sessions-shard WAL/checkpoint problem (PR #4097) therefore stalls Work, git tx, native integration too.
8. `register_project_authority` refuses a different root set for an already-registered project id
   (store_locator_resolver.rs:166-187). Init passes the raw root `vec![project_root]`
   (tracedecay-project lifecycle/mod.rs:299) while open paths pass collapsed `enrollment_roots`
   (lifecycle/mod.rs:539,734): init from a linked worktree followed by open in the same registry process
   looks able to hit DuplicateProjectAuthority (inference, not reproduced).
9. Every store port adapter is a borrowed `<'a>` view over `&Database` built at the call site (~40 prod
   sites). There is no place to swap an implementation or count calls per port.
10. `Database` has `unsafe impl Send/Sync` because the compiler's Send proof overflows on the registry
    attachment (rust-lang/rust#159228) (db/connection.rs:68-77); same workaround noted in ports.rs publish.
11. Multiple store-registry owners can exist in different processes: daemon, plus standalone CLI/hook/doctor
    registries. Mutual exclusion is via lock files (daemon-authority.lock, lifecycle.lock, Grafeo exclusive
    lock), not via a single owner process; ~12 direct `DaemonSessionRuntimeRegistryV1::open` sites.
12. Hooks create `projects/<id>/` (hook-delivery-spool, hook_analytics.jsonl, dashboard) before any
    global.db or daemon exists (live listing), because `resolve_persisted_layout` accepts the `.git` marker
    alone (layout.rs:206-211).
13. `MAX_RETAINED_PROJECT_RUNTIME_OWNERS = 4096` replaced 8 after enrolment stalled at 3 projects; the
    comment states resident memory per project owner is "ungoverned" and capacity eviction cannot reclaim
    a mounted project (session_registry.rs:88-107).
14. `ProjectRouteKey` includes `global_db_path`, so the same project path under a pinned
    `TRACEDECAY_GLOBAL_DB` is a different route (project_open_admission.rs:33-38).
15. The writer gate comment admits that hook and memory writes go straight to the store without the gate
    (writer_gate.rs:32-40); the gate serializes logical owner/content operations, not physical writes.

---------------------------------------------------------------------------------------------------

## Files Read

tracedecay-store: src/lib.rs, Cargo.toml, memory/traits.rs:1-90, transcript.rs:100-144,
configuration.rs:225-271, git_index_transactions.rs:225-299, native_integration.rs:115-231,
runtime/graph_publication/store.rs, runtime/ports.rs:1-140,269-285, runtime/identity.rs:1-180,477-527,
runtime/repository_read.rs:44-60,236-275, runtime/operation.rs:777-830, projection.rs:90-110,630-766.
tracedecay-contracts: Cargo.toml, work_attempt.rs:80-140 (+fn index), census of all src traits.
tracedecay-rusqlite-runtime: work.rs:1-50, workflow.rs:30-110, repository/retained_exact_sql.rs,
repository/attachment.rs:540-600.
tracedecay-runtime-core: lib.rs:1-45, Cargo.toml, runtime_identity.rs, config.rs:1-330, storage.rs (all),
storage/layout.rs (all), storage/identity.rs (all), storage/paths_and_io.rs:850-905, worktree.rs:1-200,
db/purpose.rs:1-120, db/connection.rs:1-120, db/connection/registry.rs:60-125,
shard_runtime/registry.rs:1-260,1060-1180, registry/attachment.rs:100-274, registry/ports.rs:1-140,440-681,
registry/capacity.rs:1-80, registry/graph.rs:1020-1087, verified_graph.rs:1-40, ports.rs (index).
tracedecay-store-runtime: lib.rs, Cargo.toml, session_registry.rs:1-140,293-330,886-917,2661-2719 (+fn index),
session_registry/mounts.rs:355-380,1488-1520, store_locator_resolver.rs:30-130,140-218,219-600,740-765,
store_locator_resolver/graph.rs:1-90, writer_gate.rs:1-140, retained_memory.rs:1-60, standalone_session.rs,
store_shutdown.rs:1-60, remote_replay_transaction.rs:1-40 (+thread/block_on grep), remote_query.rs:1-30,120-145,
remote_credentials.rs:1-30.
tracedecay-project: project_store_runtime.rs, project/lifecycle/identity.rs (all), lifecycle/mod.rs:260-330
(+fn index).
tracedecay-daemon-identity: lib.rs, profile_identity.rs:1-120, authority.rs (constants grep).
tracedecay-global-db: project_registry.rs:700-800,2100-2155 (+fn index), schema_stages.rs:55-111,
registered.rs:600-645, observation_adapter.rs:1775-1810.
tracedecay-session-memory: fact_store/mod.rs:120-175, fact_store/runtime.rs:215-265, memory/user.rs:7.
tracedecay-code-index-runtime: git_transactions/store.rs:1-90,663-675, git_transactions/registry.rs:30-110.
tracedecay-agent-hosts: native_integration/store.rs:1-30,130-147, native_integration/registry.rs:70-110.
tracedecay-code-index-retention: code_index_generations.rs:117-175,790-859 (+path greps), scope_roots.rs:254-280.
tracedecay-graph-db: sealed_store.rs:195-230,495-530.
tracedecay-maintenance: retention/storage_report.rs:1-200.
tracedecay-daemon-service: invocation/types.rs:832-847, invocation/registrars.rs:1040-1135,
invocation/work/evidence_retrieval.rs:1-45, invocation/work.rs:150-160.
tracedecay (root): src/runtime_ports.rs:1-80, daemon/engine.rs:1-175, daemon/branch_admin.rs:330-380,448-490,
daemon/database_owner_registry.rs:1-80, daemon/project_open_admission.rs:20-52,
daemon/project_open_owners.rs:318-330,540-620.
AGENTS.md:225-260. Live host (read-only ls): ~/.tracedecay, ~/.tracedecay/projects/proj_a5b3d7e3ebe14ca7,
/fast/projects/tracedecay/.git/tracedecay-project.json, `ps` for tracedecay processes.

---------------------------------------------------------------------------------------------------

## Open Questions / gaps

- project_server_lifecycle.rs was not opened (refused by instruction): the daemon's exact project-open
  sequence (which `TraceDecay::open_*` variant, which registry instance, how `ProjectServerKey` is minted,
  where `upsert_code_project` is called per route) is inferred from callers around it, not traced.
- Which tables live in `tracedecay.db` vs `sessions.db` beyond Work/Workflow/git-tx/native-integration
  (facts and observations per `ProjectReadOperationV1` binding validation) was not exhaustively mapped; other
  explorers own per-store schema.
- The "30+ project stores, one per agent worktree" observation could not be checked against a live profile
  (profile re-created today; no global.db). Source predicts one shard per repository; the per-worktree
  multiplier is scopes/servers/namespaces. Need the old profile's `projects/` listing (or registry rows) to
  tell whether extra shards came from non-worktree roots, subdirectory projects, or a pre-collapse era.
- Whether DuplicateProjectAuthority (init raw root vs open collapsed roots) actually fires in production was
  not reproduced.
- `RegisteredGlobalDb` is used for both global.db and every sessions.db (registered schema); how the 24 h
  project-open grant freeze (#4100) and schema convergence interact with `reset_required_stores` and
  `ForegroundProjectOpenAdmission` (session_registry/maintenance.rs) was not traced.
- The count of `ExactSqlHandle` sync call sites per Work port method was not re-measured (ground-3 gives 70
  sync begin_immediate/deferred sites in rusqlite-runtime).
