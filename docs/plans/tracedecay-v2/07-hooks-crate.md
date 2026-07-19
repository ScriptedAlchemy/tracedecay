# V2 host hooks and cross-worktree event boundary

## Status / Role

- Status: current host behavior is captured and hardened in PR6; the V2 hook
  cutover and single-root worktree-event path land in PR13.
- PR11 supplies application and policy decisions. PR12 supplies stable
  daemon/API surfaces. PR15 supplies canonical multi-root/worktree admission.
  PR17 may add Plan 24 `TaskId` placement and ready-commit joins. PR18 freezes
  public command, MCP-tool, route, and SDK spellings; hook wire names remain
  provider-private compatibility inputs.
- Hooks are thin host adapters. They emit bounded typed events to
  `tracedecayd`, optionally append the exact safe envelope to the one bounded
  replay spool, and render a bounded daemon response. They never coordinate
  peer worktrees directly.

## Outcome

Codex, Claude Code, Cursor, Hermes, Kiro, and supported daemon/MCP
notifications feed one daemon-owned event authority. A daemon-issued session
binding lets a hook report task placement, worktree epoch, ref/commit, tool,
edit, test, and conflict observations without treating a path as identity,
opening a business database, running synchronization, or writing another
worktree.

The transport is at-least-once and replayable. Daemon acceptance, durable
spool acceptance, downstream processing, and effect completion are separate
typed dispositions. Only a daemon receipt may claim an application effect.

## Owns

- Provider-specific wire decoding, event-name mapping, matcher handling, and
  legal response rendering.
- `HookEventEnvelopeV2`, `WorktreeEventV1`, the host capability matrix, bounded
  transport encoding, and the transport-only replay-spool record.
- Bounded IPC to `tracedecayd`, strict serialization limits, fair replay, and
  provider-safe behavior when the daemon is unavailable.
- Direct fixtures for each supported host event, capability claim, replay
  disposition, and response contract.
- Hook-process latency, payload-size, spool, replay, and privacy telemetry that
  contains no prompt, command, path, source, tool arguments, test log, or tool
  output.

## Does not own

- Database reads or writes, transcript ingestion, sync, catch-up, project
  discovery, source parsing, sanitization, indexing, projection, query, policy
  evaluation, readiness, placement, merge planning, or hint selection.
- A business-event queue, second event authority, embedded daemon, writable
  fallback store, or spool database. The append-only replay spool defined
  below is the sole transport exception: it stores only validated
  `HookEventEnvelopeV2` bytes plus checksum and offset, and cannot be queried
  as product state.
- Task plans, boards, dependency calculation, workflow execution, attempts,
  leases, agent steering, merge application, conflict resolution, or
  end-of-turn task completion.
- Host installation/bundle management, tool catalogs, config mutation, network
  services, or generic command execution.
- Generated provider inventories, generated conformance matrices, source
  parsers, workflow JavaScript, or plan-derived code.

## Canonical event contract

The hook can emit a cross-worktree event only after daemon admission returns a
signed, session-bound `HookScopeBindingToken`. Admission may inspect a host
workspace locator transiently through Plan 16, but neither the token, event
identity, spool, nor receipt contains a path. A hook that has not obtained a
binding reports `UnboundScope` and relies on authoritative host/Git catch-up;
it never hashes a path into `WorktreeId` or persists a path for later replay.

```rust
pub struct HookEventEnvelopeV2 {
    pub schema_version: HookEventSchemaVersion,
    pub event_id: HookEventId,
    pub producer: HookProducer,
    pub provider_session_id: ProtectedSessionId,
    pub task_id: Option<TaskId>,
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub worktree_id: WorktreeId,
    pub worktree_epoch: WorktreeEpoch,
    pub source_sequence: Option<ProviderSequence>,
    pub ordering: HookOrdering,
    pub observed_at: UtcMicros,
    pub capability_snapshot: HostWorktreeEventCapabilitiesRevision,
    pub authorization_epoch: AuthorizationEpoch,
    pub binding_token: HookScopeBindingToken,
    pub event: WorktreeEventV1,
    pub idempotency_key: IdempotencyKey,
    pub payload_digest: HookPayloadDigest,
}

pub enum WorktreeEventV1 {
    TaskPlaced {
        placement_revision: TaskPlacementRevision,
    },
    WorktreeEpochChanged {
        previous: WorktreeEpoch,
        current: WorktreeEpoch,
        reason: WorktreeEpochChangeReason,
    },
    RefAdvanced {
        ref_id: GitRefId,
        previous: Option<CommitId>,
        current: CommitId,
    },
    CommitObserved {
        commit_id: CommitId,
        parent_ids: Vec<CommitId>,
        readiness: CommitReadinessObservation,
    },
    ToolLifecycle {
        tool_call_id: ToolCallId,
        tool_id: CatalogToolId,
        phase: ToolPhase,
        effect_receipt_id: Option<EffectReceiptId>,
    },
    EditObserved {
        file_id: FileId,
        ranges: Vec<ChangedRange>,
        content_digest: ContentDigest,
        saved: bool,
    },
    TestLifecycle {
        test_run_id: TestRunId,
        test_ids: Vec<TestId>,
        phase: TestPhase,
        outcome: Option<TestOutcome>,
        receipt_id: Option<TestReceiptId>,
    },
    ConflictObserved {
        conflict_id: ConflictId,
        class: CrossWorktreeConflictClass,
        other_worktree_id: Option<WorktreeId>,
        file_ids: Vec<FileId>,
        symbol_ids: Vec<SymbolId>,
        observed_at: UtcMicros,
        expires_at: UtcMicros,
    },
    ConflictCleared {
        conflict_id: ConflictId,
        clearing_evidence: ConflictClearingEvidenceId,
    },
}
```

The envelope shape is versioned across delivery slices. Before PR17,
`task_id` is always absent, `TaskPlaced` is unconstructable, and
`CommitObserved.readiness` is `NotEvaluated`; the capability snapshot rejects
any producer claiming otherwise. PR17 enables those fields/variants only after
the Plan 24 ID, placement, and readiness contracts exist. Older decoders reject
the newer capability revision rather than interpreting task/ready state.

`TaskId` is present only when the daemon-issued binding independently
authorizes that Plan 24 task/worktree relation. It is never inferred from a
branch, worktree, session, card title, path, or commit. A hidden or denied task
omits the field; possession of it grants no task read or mutation capability.

`RefAdvanced` and `CommitObserved` carry native Git object/ref identity from
Plan 36. `ToolLifecycle` never carries arguments, stdin, environment, output,
or command text. `EditObserved` carries canonical `FileId`, ranges, and digest,
never source or path. `TestLifecycle` carries typed IDs and terminal state,
never logs. Conflict events are advisory observations; they create no lock,
assignment, dependency, merge decision, or peer message.

Every enum is exhaustive and `deny_unknown_fields`. A V1 record remains
decodable only for the bounded migration window. Unknown versions are
quarantined as content-free replay failures; unknown variants are never
coerced into another event.

## Host capability matrix

`HostWorktreeEventCapabilitiesV1` records, for each host and exact host
version, every event family as `Native`, `ReceiptDerived`, `DaemonDerived`,
`Unavailable`, or `Prohibited`. It also pins provider-event names, ordering
support, response legality, maximum bytes, hard deadline, and the fixture
digest. A hook may emit only a `Native` or `ReceiptDerived` family; the daemon
alone emits `DaemonDerived` ref, commit, readiness, and conflict observations.
`Unavailable` is truthful absence, not permission to infer from command text.

The PR13 minimum matrix is:

- **Claude Code:** session-start and post-tool/stop boundaries are native;
  tool lifecycle is native; edit and test lifecycle are receipt-derived only
  when typed tool identity is present; ref, commit, and conflict are
  daemon-derived; dependency readiness is unavailable until PR17.
- **Codex:** session-start and post-tool/turn boundaries are native; tool
  lifecycle is native; edit and test lifecycle are receipt-derived only when
  typed tool identity is present; ref, commit, and conflict are daemon-derived;
  dependency readiness is unavailable until PR17.
- **Cursor:** session/workspace, after-file-edit, and supported shell/tool
  boundaries are native; saved edits are native; tests are receipt-derived;
  ref, commit, and conflict are daemon-derived; dependency readiness is
  unavailable until PR17. Native editor diagnostics remain Plan 35 evidence,
  not hook events.
- **Hermes:** terminal receipt and turn completion/ingestion are native; tool
  terminal state is native; edit/test events are receipt-derived only when
  their typed IDs are present and otherwise unavailable; ref, commit, and
  conflict are daemon-derived; dependency readiness is unavailable until PR17.
- **Kiro:** prompt/session/workspace boundaries are native; tool, edit, and
  test families remain unavailable until a checked-in native fixture proves
  their exact event and ordering contract; ref, commit, and conflict are
  daemon-derived; dependency readiness is unavailable until PR17.

All five hosts obtain `ProjectId`, `RepositoryId`, `WorktreeId`, and epoch only
from the daemon binding token. PR17 may add an optional authorized `TaskId` to
any host without changing the host's event-family support. No host may claim
native ref/commit/conflict authority from parsing a shell command.

## Routing, replay, debounce, and backpressure

- The hook routes only to the daemon authority named by the binding token.
  `WorktreeId` and epoch choose the logical stream; provider/session and
  `source_sequence` choose its ordering domain. No event is broadcast to a
  peer hook, worktree, agent, or LSP session.
- Per producer/session ordering is preserved when the provider supplies a
  sequence. Otherwise `ordering = Unknown`; arrival time never manufactures a
  total order. Daemon projectors order ref/commit facts by native Git
  identities and retain unknown ordering for unrelated tool/edit/test events.
- Duplicate `(event_id, payload_digest)` is `ExactDuplicate`. Reuse of an
  `event_id` with another digest is `IdempotencyConflict` and writes no product
  state. A stale worktree or authorization epoch is `StaleEpoch`; it cannot be
  rebound to the current path, ref, task, or worktree.
- Edit observations for the same `(worktree_epoch, file_id, content_digest)`
  coalesce for 75 ms with a 250 ms maximum wait. Tool and test progress
  coalesce for 100 ms with a 500 ms maximum wait, but every terminal event is
  retained. Ref advances, commit observations, conflict observed/cleared, and
  epoch changes bypass debounce.
- One envelope is at most 16 KiB. It contains at most 64 changed ranges, 128
  test IDs, 64 file IDs, 64 symbol IDs, and 16 commit parents. A replay batch
  contains at most 64 records and 256 KiB. Oversized input is rejected before
  IPC or spool append and cannot be truncated into another semantic event.
- The transport spool is an append-only, checksummed, length-prefixed log with
  one writer lease per host process and daemon-only acknowledgement/compaction.
  Bounds are 4,096 records or 32 MiB per host, 1,024 records or 8 MiB per
  producer/session, and 24 hours maximum age. Reaching any limit returns
  `SpoolFull`; unacknowledged records are never overwritten or silently
  evicted.
- Replay is FIFO within one producer sequence, fair round-robin across
  sessions, at most four sessions concurrently and one in-flight batch per
  session. The daemon reauthorizes the binding, task visibility, worktree
  epoch, ref/commit identity, privacy revision, and event capability before
  applying each replayed record. Permanent denial or stale epoch produces a
  tombstone receipt and compaction; transient saturation leaves the record
  pending.
- Normal daemon admission reserves a high-priority class for epoch, ref,
  commit, conflict, and terminal test/tool receipts. Progress/edit traffic
  cannot starve it. Saturation returns `Backpressured` with retry class and
  never a raw broken pipe interpreted as acceptance.
- Cross-worktree encoding plus enqueue adds at most 5 ms at warm p95 and 20 ms
  at p99. A local daemon acknowledgement has p95 <= 25 ms and p99 <= 75 ms.
  At 25 ms without acknowledgement the hook switches to spool append; the
  complete event path has a 100 ms hard deadline or the provider's stricter
  measured PR6 deadline. Replay never runs on the synchronous hook path.
- Optional guidance still fails open with no injected text. Event disposition
  remains visible in telemetry and later status; failure to emit guidance is
  never reported as event acceptance.

## Required behavior

- **PR6 — baseline:** preserve current supported Codex, Claude Code, Cursor,
  Hermes, and Kiro event semantics in direct redacted fixtures. Unknown events
  remain explicit and harmless.
- **PR6 — measurements:** record real hook wall time, daemon round-trip time,
  payload bytes, timeout, and disposition without recording message content.
- **PR6 — failure:** prove existing hooks do not corrupt state when duplicated,
  reordered, interrupted, or invoked while the daemon is unavailable.
- **PR13 — signal path:** decode one host event, validate the capability and
  bounds, resolve a daemon-issued binding, assign idempotency/order identity,
  send or spool `HookEventEnvelopeV2`, and stop. Session-start and file-change
  hooks signal required work; they do not perform sync.
- **PR13 — daemon authority:** `tracedecayd` owns durable capture,
  sanitization, canonical scope resolution, sync, database transactions,
  projections, conflict/proximity calculation, query freshness, policy
  evaluation, and receipts.
- **PR13 — acknowledgement:** `Accepted` means the daemon durably accepted the
  exact event. `AcceptedForReplay` means only that the transport spool durably
  accepted it. Neither means projection, test, merge, or task work completed.
- **PR13 — unavailable daemon:** optional guidance fails open; eligible bound
  events use the bounded replay spool and unbound/oversized/full-spool events
  rely on authoritative host/Git catch-up. The hook creates no business writer.
- **PR13 — response:** render only application-approved, sensitivity-safe
  guidance supported by that host event. No hook-local reranking, readiness,
  conflict, merge, or policy fallback.
- **PR13 — isolation:** one busy session cannot block another. Bounds and
  admission classes above are mandatory.
- **PR13 — migration:** shadow V2 against current host behavior, cut over one
  provider/event family at a time, and retain a direct rollback switch until
  parity receipts pass.
- **PR15 — multi-root:** every root gets an independent Plan 16 binding and
  event stream. A multi-root host message is split only after canonical
  resolution; denied or ambiguous roots cannot be counted, renamed, or folded
  into an admitted neighbor.
- **PR17 — task joins:** optional `TaskId` placement and ready-commit events
  require Plan 24 version/readiness evidence and remain observations. They do
  not schedule work or authorize the cross-merge effect bound by Plan 21.

## Files and dependency order

- `crates/tracedecay-hooks/src/event.rs` owns the V2 envelope and exhaustive
  event codec; `binding.rs` owns the opaque daemon-issued scope token;
  `capabilities.rs` owns the checked-in host matrix; `transport.rs` owns IPC
  admission; and `spool.rs` owns the append/replay/compaction mechanics.
- Existing provider decoders in `src/hooks/{claude,codex,cursor,kiro}.rs`,
  `src/hooks/post_tool_use.rs`, and the Hermes boundary lower provider wire
  events into the crate contract. They retain no canonical identity or
  application logic.
- `src/daemon/hook_events/{mod,admission,replay,projector}.rs` owns daemon
  binding validation, class-aware admission, replay acknowledgement, and
  dispatch to Plan 09 application operations.
- `src/mcp/hook_events.rs` becomes a compatibility decoder only and loses
  sync, branch, path, command parsing, and effect planning when each family
  cuts over.
- Contract tests live in
  `crates/tracedecay-hooks/tests/{event_contract,spool_contract,host_capabilities}.rs`;
  integration tests live in
  `tests/hooks_lsp_suite/{worktree_event_test,hook_replay_test,hook_backpressure_test}.rs`;
  redacted native fixtures live under
  `tests/fixtures/host_events/<host>/worktree-events-v2.json`.

Dependency order is fixed:

1. **M7.1 — schema/binding:** land exhaustive IDs/events, path-free codec,
   capability matrix, and signed binding token; exit requires round-trip,
   unknown-version, bounds, and path/secret canaries.
2. **M7.2 — transport/spool:** land send-or-spool disposition, fair replay,
   checksums, quotas, expiry, acknowledgement, and crash recovery; exit
   requires at-least-once replay with no duplicate logical event.
3. **M7.3 — daemon admission:** land epoch/authorization recheck,
   idempotency, priority classes, and application receipts; exit requires no
   hook-local sync, Git, task, conflict, or merge effect.
4. **M7.4 — host cutover:** prove the checked-in matrix for all five hosts,
   shadow each family, then delete its path/command-derived compatibility
   planner.
5. **M7.5 — PR15/PR17 extension:** enable independent multi-root bindings,
   then optional TaskId/ready-commit joins after their owner contracts pass.

## Acceptance

- PR6 fixtures assert exact supported event mappings and provider response
  legality against current host probes.
- PR13 tests cover every envelope variant and capability state; duplicate,
  reordered, concurrent, malformed, oversized, unknown, timed-out, cancelled,
  daemon-down, daemon-restart, stale-epoch, authorization-revoked,
  spool-full/expired/corrupt, replay-gap, and slow-consumer cases.
- A five-host matrix fixture fails if an adapter emits an unavailable family,
  parses shell text into Git authority, omits ordering limitations, or
  advertises a capability without a direct native fixture.
- Replay tests kill the hook before append, during append, after fsync, during
  daemon send, after daemon commit, and before acknowledgement. Restart
  produces one logical daemon event and one stable receipt.
- Multi-root tests prove same-name repositories, linked worktrees, moved
  paths, symlink swaps, detached refs, stale epochs, hidden neighbors, and
  authorization narrowing never rebind an event or reveal hidden root counts.
- Integration tests prove hooks never open TraceDecay databases, run
  sync/catch-up, scan project files, load models, invoke Git, calculate
  readiness/conflicts, apply a merge, write a peer worktree, or start child
  workflows.
- Privacy/schema tests prove prompts, commands, tool args/output, test logs,
  credentials, private paths, source, reasoning, task narrative, and hidden
  peer identity do not enter event/spool/telemetry/error bytes.
- Performance tests enforce every envelope, queue, spool, debounce, replay,
  and latency bound above under 1/8/32 concurrent sessions and daemon restart.
- Cutover tests compare current and V2 event dispositions, daemon receipts,
  and rendered guidance before each provider family switches.
- Architecture tests reject database, store-adapter, query, policy-runtime,
  executor, Git mutation, LSP, workflow-JavaScript, peer-transport, and
  generated-inventory imports from hook adapters.
