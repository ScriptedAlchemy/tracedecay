# TraceDecay V2 Remote Multi-Machine Shared Brain Plan

## Status / role

Status: active product plan.

PR16 delivers the remote shared Brain as one production journey. It builds on
the existing daemon-owned store, capture, query, privacy, API, settings, and
health surfaces; it does not ship distributed-authority scaffolding that is
unusable until a later PR.

## User outcome

An enrolled machine can keep capturing permitted observations while
disconnected, replay them safely when authority returns, query the shared
Brain with honest local/remote coverage, create a verified backup, restore it
through isolated staging, and fail over to a standby without admitting two
writers or losing deletion and privacy state.

## End-to-end production journey

1. **Enroll and capture offline.** An authenticated node is enrolled into one
   typed Brain/node identity with explicit shard placement/revision,
   capabilities, privacy policy, transport profile, and revocable credentials.
   Enrollment records repository/worktree/ref/snapshot identity through the
   same verified Git relationship model as PR15; hostname, path, or directory
   name cannot correlate projects. Local hooks send bounded events to the
   node-local daemon. When the owning shard authority is unreachable, that
   daemon applies the canonical sanitizer and appends eligible canonical
   observations to the bounded remote offline-capture spool.
2. **Reconnect and replay through the fence.** The node discovers the current
   authenticated authority and replays pending frames with deterministic event
   identity, enrollment revision, node and repository/worktree identity,
   ordering evidence, schema/kind, sanitizer/privacy revision, payload
   digest/length, integrity chain/segment root, capture evidence, replay
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
   binds Brain, shard, generation, schema, privacy policy, watermark, authority
   epoch, and artifact digest. It declares cache age/lag, pending local
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
   epoch/frontier, artifact inventory/root, per-artifact digests and
   byte/count totals, key revisions, lineage, creation/verification/
   expiry/refresh evidence, and typed stale/partial coverage. Restore writes
   only to a non-serving isolated staging location, verifies destination bytes,
   generations and reference closure, and reapplies current tombstones,
   deletion, quarantine, retention, authorization, and privacy-policy state
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

## Authority, privacy, and replay constraints

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
  privacy policy, and epoch. Possessing a cache handle or manifest never
  bypasses current authorization, retention, deletion, or privacy recheck.
- The remote offline-capture spool is distinct from PR6's daemon
  host-admission spool. Hooks never own durable sanitization or spool writes.
  Unsaved LSP documents, document versions, overlays, dirty-overlay
  diagnostics, raw JSON-RPC frames, analyzer state, and session-only agent
  proximity never enter the spool, read cache, replica, trace, backup,
  failover payload, or remote analyzer request.
- Remote eligibility never weakens local privacy. Dirty document content stays
  node-local by default. A remote analyzer requires a separate capability,
  policy grant, and privacy disclosure. Local-only and remotely eligible scope
  classifications remain explicit through capture, replay, query, cache,
  backup, restore, and failover.
- Durable saved-content feedback, GitHub-ingested read-only
  thread/comment/reply evidence, and CI-localization evidence publish only
  through the owning fenced shard and retain watermarks, tombstones, privacy,
  retention, and authority epoch across restart, backup, restore, promotion,
  and failover. No GitHub write path exists.
- Replay states remain visible as captured, pending, admitted, duplicate,
  rejected, quarantined, acknowledged, or garbage-collection eligible.
  Overflow, corruption, sequence gaps, lost acknowledgements, policy change,
  revocation, and rejected replay are truthful recoverable states; they never
  create an empty local authority database.
- Before replay or restore admission, current deletion, tombstone,
  quarantine, retention, authorization, and privacy rules are re-evaluated.
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
   matches Brain, shard, generation, schema, privacy policy, watermark, and
   authority epoch, and merge it without overstating freshness.
4. **Complete backup, staged restore, and failover.** Produce backups from the
   fenced authority, verify and policy-replay them in non-serving staging, then
   exercise higher-epoch promotion, atomic publication, rollback before
   publication, and read-only old-authority rejoin from the same operational
   surfaces.
5. **Preserve compatibility across the journey.** Existing local capture,
   query, settings, CLI/API/dashboard/Doctor, stored generations, repository
   identity, retention/deletion, diagnostics, and health contracts remain
   supported through the remote application model. Versioned enrollment,
   spool, cache/replica, backup and restore readers migrate or reject old data
   explicitly; they never silently reinterpret authority, privacy, identity,
   epoch, watermark, or deletion lineage. PR18 adds SDK bindings without
   replacing these PR16 APIs.

## Replacement and deletion

- Remove any remote path that opens, copies, or mounts authority databases,
  SQLite WAL/SHM files, payload roots, or generation files on a client.
- Remove database URLs, client-side SQL, replicated-SQLite fallback,
  multi-primary/LWW mutation, automatic offline promotion, and lease-timeout
  claims of exclusive authority.
- Remove standalone enrollment, topology, spool, replica, backup, or failover
  contract phases that do not participate in this journey. Retain every
  capability and fold its necessary persistence, migration, compatibility, and
  adapters into the first callable enrollment/capture, replay, query,
  backup/restore, or failover slice that uses it.
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
in isolated staging under newer deletion and privacy state, promotes the
standby under a higher installed fence, and proves the old authority cannot
commit or publish before or after rejoin.

Focused failure cases cover spool overflow/corruption, lost acknowledgement,
sequence gaps, replay crash/restart, node revocation, privacy-policy change,
delayed old-writer packets, startup/promotion races, partition and process
death, sink fence failure, wrong Brain/shard/generation/epoch/schema/policy/
digest/watermark cache manifests, interrupted backup/restore publication,
newer tombstone/quarantine state, insufficient standby frontier, rollback, and
unavailable shards. Every surface must show the same partial, stale, unknown,
unavailable, or recovery-required truth. Compatibility checks prove supported
older local/API/stored-data inputs migrate or fail explicitly without authority
or privacy drift. Negative checks prove unsaved overlays and analyzer state
never become durable remote records and no client or offline path opens
authority storage. The relevant all-feature aggregate gate is the final PR16
gate; PR16 adds no benchmark harness or placeholder baseline.

## Not in PR16

- Multi-primary or eventual-authority convergence, CRDT mutation authority,
  last-write-wins conflict resolution, automatic partition promotion, or a
  mandatory hosted control plane.
- SDK bindings, which ship with PR18.
- Any weakening of local privacy, any hidden replication or coverage
  degradation, or any Git/GitHub mutation.
