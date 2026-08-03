# TraceDecay V2 Remote Multi-Machine Shared Brain Plan

## Status / role

Status: active product plan.

PR16 delivers the remote shared Brain as one production journey. It builds on
the existing daemon-owned store, capture, query, API, settings, and
health surfaces; it does not ship distributed-authority scaffolding that is
unusable until a later PR.

Earlier enrollment, topology, spool, replica, backup, failover, fixture, and
packet artifact names are historical evidence, not standalone prerequisites
or mandatory recreation targets. Only actually independently released public
APIs retain protocol compatibility; persisted remote records use the
fresh-store rule. All other retention is judged by the direct offline capture,
fenced replay, query, backup/restore, failover, platform, and regression
behavior below.

No remote enrollment/spool/replica/backup format is established on
`origin/master` or in a published package/release. Pure source-only/internal
enrollment helpers, wire-visible V2 enrollment revisions, spool files, replica
journals, backup manifests, checkpoints, and receipts take their final shape
in place. Only the exact final persisted shape is accepted; any other database,
store, spool, file, or projection returns typed `ResetRequired` and requires
explicit reset or recreation. No storage reader, migration, backfill, dual
write, or census path exists. Authenticated protocol negotiation remains
separate for actual independently released nodes.

## User outcome

An enrolled machine can keep capturing permitted observations while
disconnected, replay them safely when authority returns, query the shared
Brain with honest local/remote coverage, create a verified backup, restore it
through isolated staging, and fail over to a standby without admitting two
writers or losing deletion state.

## End-to-end production journey

1. **Enroll and capture offline.** An authenticated node is enrolled into one
   typed Brain/node identity with explicit shard placement/revision,
   capabilities, transport profile, and revocable credentials.
   Enrollment records repository/worktree/ref/snapshot identity through the
   same verified Git relationship model as PR15; hostname, path, or directory
   name cannot correlate projects. Local hooks send bounded events to the
   node-local daemon. When the owning shard authority is unreachable, that
   daemon applies the canonical sanitizer and appends eligible canonical
   observations to the bounded remote offline-capture spool.
2. **Reconnect and replay through the fence.** The node discovers the current
   authenticated authority and replays pending frames with deterministic event
   identity, enrollment revision, node and repository/worktree identity,
   ordering evidence, schema/kind, sanitizer revision, payload length, capture
   evidence, replay
   attempt, and causal/sequence context. Frames visibly progress through
   captured, pending, admitted,
   duplicate, rejected or quarantined, acknowledged, and garbage-collection
   eligible states. The authority atomically deduplicates admission with the
   canonical effect and returns the original durable receipt for duplicates.
   A frame is eligible for garbage collection only after durable
   acknowledgement.
3. **Query the shared Brain.** The node queries only through authenticated
   TraceDecay application APIs. The response combines authoritative remote
   results with any verified read replica/cache whose authenticated manifest
   binds Brain, shard, generation, schema, watermark, and authority epoch. It
   declares cache age/lag, pending local
   observations, unavailable shards, and partial/stale/unknown coverage.
   Integrity, authenticity, freshness, completeness, authorization, and
   coverage remain separate claims. A stale or unverifiable cache may serve an
   explicitly stale read but cannot accept writes, authorize promotion, or
   appear healthy.
   The LSP gateway and analyzers stay on the enrolled node that owns the live
   workspace; they reach remote clean-generation authority only through these
   APIs. Clean durable diagnostics publish through the owning fenced shard.
4. **Back up and stage restore.** The current authority creates a consistent,
   authenticated backup manifest over the required database families,
   payloads, generations, repository identities, checkpoints, source
   frontier, artifact inventory, byte/count totals, lineage, and typed
   stale/partial coverage. Restore writes
   only to a non-serving isolated staging location, verifies destination bytes,
   generations and reference closure, and reapplies current tombstones,
   deletion, quarantine, retention, authorization, and project-scope state
   before it can be published. A pre-publication failure rolls back staging
   without exposing a partial generation.
5. **Fence and fail over.** Promotion acquires a higher epoch with an
   authority-store compare-and-swap, installs that epoch at every durable
   mutation and publication sink, proves the old authority is fenced, and
   verifies the standby has the required durable frontier. Only then may the
   staged generation publish atomically and serve. The old authority remains
   read-only on rejoin until explicitly reseeded.

The Settings, CLI, API, dashboard, and Doctor surfaces expose this same
journey and application model: enrollment, current authority and placement,
spool state, replay receipts, query coverage, backup verification, staged
restore, and failover/rejoin state. Human and structured output use the same
finding and remediation identities.

## Authority, authentication, and replay constraints

- Each mutable shard has exactly one daemon writer identified by Brain, shard,
  generation, placement revision, and monotonically increasing fence epoch.
  Wall-clock lease expiry may aid liveness but is never fencing proof.
- Authority acquisition/transfer uses compare-and-swap, persists lease, epoch,
  outbox, checkpoint, placement and publication evidence before
  acknowledgement, and admits writes only through authenticated application
  commands carrying expected authority and idempotency identity. Startup race,
  partition, reconnect, process death, or lease expiry cannot authorize two
  writers.
- A higher epoch must be durably installed at every selected-model sink before
  writes begin. Mutation, replay, receipt, outbox, publication, cache/replica
  manifest, backup, restore publication, diagnostics publication, and other
  durable effects reject an older or mismatched epoch. A sink that cannot
  prove its fence is unavailable, not best-effort.
- Clients use application APIs for commands, queries, progress, cancellation,
  and health. They never receive authority database paths, bytes, credentials,
  URLs, or a client-side SQL/network-filesystem fallback.
- Verified read replicas/caches retain provenance, generation, watermark, lag,
  and epoch. Possessing a cache handle or manifest never bypasses current
  authorization, project scope, retention, or deletion.
- The remote offline-capture spool is distinct from PR6's daemon
  host-admission spool. Hooks never own durable sanitization or spool writes.
  Unsaved LSP documents, document versions, overlays, dirty-overlay
  diagnostics, raw JSON-RPC frames, analyzer state, and session-only agent
  proximity never enter the spool, read cache, replica, trace, backup,
  failover payload, or remote analyzer request.
- Dirty document content stays node-local. A remote analyzer requires an
  authenticated configured endpoint and explicit user enablement.
- Durable saved-content feedback, GitHub-ingested read-only
  thread/comment/reply evidence, and CI-localization evidence publish only
  through the owning fenced shard and retain watermarks, tombstones,
  retention, and authority epoch across restart, backup, restore, promotion,
  and failover. No GitHub write path exists.
- Replay states remain visible as captured, pending, admitted, duplicate,
  rejected, quarantined, acknowledged, or garbage-collection eligible.
  Overflow, corruption, sequence gaps, lost acknowledgements, policy change,
  revocation, and rejected replay are truthful recoverable states; they never
  create an empty local authority database.
- Before replay or restore admission, current deletion, tombstone,
  quarantine, retention, authorization, and exact project scope are evaluated.
  Older captured or backed-up content cannot resurrect deleted data, bypass a
  newer policy, or republish stale GitHub/CI/feedback evidence.
- Repository correlation uses verified Git identity plus explicit
  project/worktree/ref/snapshot identity. Hostname, path, directory name, CWD,
  or enrollment alone cannot merge projects or widen scope.
- Enrollment and daemon locality confer no Git or GitHub mutation authority.
  PR16 has no GitHub post, update, reply, resolve, dismiss, push, rebase,
  force-push, or autonomous repository mutation path.
- Node revocation immediately blocks commands, replay, cache refresh, backup
  access, restore publication, and promotion credentials. Delayed packets from
  a revoked or formerly authoritative node fail closed.
- Connectivity profiles remain replaceable transports beneath the
  authenticated TraceDecay protocol; no capability depends on one vendor or
  hosted control plane.
- Multi-primary, last-write-wins, replicated-SQLite, CRDT, Merkle-DAG,
  wall-clock, or lease-timeout convergence is never canonical mutation
  authority. Content-addressed structures may support immutable spool
  integrity, deduplication, or gap evidence only.

## PR16 implementation defaults

- Build the first delivery on existing HTTP/SSE, rustls, and the daemon-owned
  rusqlite runtime path. The retired libSQL compatibility/runtime path is not a
  remote seam to revive. These foundations replace no semantics: TraceDecay
  still owns authentication, revocation, authority fencing, single-writer
  admission, replay identity, coverage, backup/restore verification, and
  failover.
- At the concrete integration that needs them, consider `reqwest` plus
  `eventsource-stream` for remote streaming, `tokio-util` for cancellation,
  `zstd`/`tar` for backup payloads, `object_store` for an admitted object
  backend, and Hickory for discovery. Admit one only when it deletes named
  transport, stream, cancellation, archive, backend, or discovery code and
  passes compile-time, binary-size, memory, connection, cancellation, and
  recovery budgets.
- If admission fails or no shipped journey yet needs the integration, retain
  the existing path or report the capability unavailable. Do not add
  speculative transport layers, HMAC/attestation machinery, local signatures,
  trust roots, or another authority protocol; remote authentication and every
  durable fence remain mandatory.

## Implementation slices

1. **Connect enrollment to offline capture.** Extend the shipped node-local
   daemon and authenticated API path so an enrolled node captures sanitized,
   integrity-protected frames only when the owning authority is unreachable.
   Include the bounded spool persistence needed by this path; do not create a
   standalone spool schema or framework milestone.
2. **Replay into the fenced write path.** Install authority epochs at all
   durable sinks, route reconnect through the current authority, make
   deduplication plus canonical effect atomic, and expose durable replay
   receipts and pending/rejected/quarantined health.
3. **Serve remote query with verified coverage.** Route normal query,
   diagnostics publication, and exact loads through authenticated application
   APIs. Accept cache/replica material only when its authenticated manifest
   matches Brain, shard, generation, schema, watermark, and
   authority epoch, and merge it without overstating freshness.
4. **Complete backup, staged restore, and failover.** Produce backups from the
   fenced authority, verify and policy-replay them in non-serving staging, then
   exercise higher-epoch promotion, atomic publication, rollback before
   publication, and read-only old-authority rejoin from the same operational
   surfaces.
5. **Keep protocol and storage separate across the journey.** An actual
   independently released public CLI/API/dashboard/Doctor protocol may retain
   its documented compatibility surface. Local capture, stored generations,
   repository identity, retention/deletion, diagnostics, spools, caches,
   replicas, backups, and restores accept only their exact final persisted
   shape. Any other shape returns `ResetRequired` before interpretation and
   requires explicit reset or recreation; it never gains a reader, migration,
   backfill, dual write, or census path. PR18 adds SDK bindings without
   replacing these PR16 APIs.

## Replacement and deletion

- Remove any remote path that opens, copies, or mounts authority databases,
  SQLite WAL/SHM files, payload roots, or generation files on a client.
- Remove database URLs, client-side SQL, replicated-SQLite fallback,
  multi-primary/LWW mutation, automatic offline promotion, and lease-timeout
  claims of exclusive authority.
- Remove standalone enrollment, topology, spool, replica, backup, or failover
  contract phases that do not participate in this journey. Retain exact final
  schema validation and explicit reset/recreation in the first callable
  enrollment/capture, replay, query, backup/restore, or failover slice that
  uses persisted state.
- Remove duplicated provider-specific or Plan 37 acceptance matrices. Durable
  saved-content feedback, read-only GitHub-ingested evidence, and CI
  localization use the same fenced replay/query/backup/failover path; overlays
  and session-only proximity remain node-local.

## Direct acceptance

One enrolled-node scenario disconnects from its owning authority, captures
sanitized eligible observations, reconnects after an authority epoch change,
replays duplicates idempotently to exactly one canonical effect and receipt,
queries the result through authoritative and verified cached/replica paths with
pending/local/remote coverage, publishes clean LSP diagnostics while overlays
remain node-local, creates a verified expiring/refreshable backup, restores it
in isolated staging under newer deletion state, promotes the
standby under a higher installed fence, and proves the old authority cannot
commit or publish before or after rejoin.

Focused failure cases cover spool overflow/corruption, lost acknowledgement,
sequence gaps, replay crash/restart, node revocation,
delayed old-writer packets, startup/promotion races, partition and process
death, sink fence failure, wrong Brain/shard/generation/epoch/schema/watermark
cache manifests, interrupted backup/restore publication,
newer tombstone/quarantine state, insufficient standby frontier, rollback, and
unavailable shards. Every surface must show the same partial, stale, unknown,
unavailable, or recovery-required truth. Compatibility checks cover only an
actually independently released public local/API protocol; every non-final
stored-data input returns `ResetRequired` before interpretation. Negative
checks prove unsaved overlays and analyzer state never become durable remote
records and no client or offline path opens authority storage. The final PR16
check is the relevant ordinary all-feature repository test run, not a separate
acceptance gate; PR16 adds no benchmark harness or placeholder baseline.

## Not in PR16

- Multi-primary or eventual-authority convergence, CRDT mutation authority,
  last-write-wins conflict resolution, automatic partition promotion, or a
  mandatory hosted control plane.
- SDK bindings, which ship with PR18.
- Unsanitized capture, durable/remote dirty overlays, hidden replication or
  coverage degradation, or any Git/GitHub mutation.
