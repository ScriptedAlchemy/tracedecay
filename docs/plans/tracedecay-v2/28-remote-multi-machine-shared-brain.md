# TraceDecay V2 Remote Multi-Machine Shared Brain Plan

## Status / role

PR16 fully delivers the remote shared-Brain product. It builds on the PR4 authoritative store boundary and the intervening capture, projection, query, application, API, privacy, configuration, and observability work. No distributed-authority requirement is deferred.

## Outcome

Enrolled machines share one logical Brain through authenticated TraceDecay APIs while each mutable shard has exactly one fenced daemon writer. Clients remain useful offline through a remote offline-capture spool and verified read cache, without opening or copying authority databases.

## Owns

- Brain, node, shard-placement, authority-epoch, enrollment, and revocation contracts.
- Authenticated remote routing and API-only client behavior.
- Fenced authority transfer, standby promotion, reconnect, and split-brain prevention.
- Verified read replicas and caches with provenance, watermark, and lag.
- Remote offline-capture spool and idempotent replay.
- Cross-machine repository identity, coverage reporting, backup, restore, and failover.

## Does not own

- Network-filesystem access to SQLite, WAL, SHM, payload roots, or generation files.
- Client-side SQL, database credentials, database URLs, or a database fallback mode.
- Multi-primary writes, last-write-wins merging, clock-based conflict resolution, or automatic offline promotion.
- A mandatory connectivity vendor or hosted control plane.
- Hidden replication, coverage, privacy, or authority degradation.

## Required behavior

### Single-writer authority

- Place every mutable shard under one daemon authority identified by Brain, shard, generation, placement revision, and monotonically increasing fence epoch.
- Admit writes only through authenticated application commands carrying the expected authority and idempotency identity.
- Persist lease, epoch, outbox, checkpoint, and publication evidence before acknowledging authority changes.
- Reject stale, partitioned, revoked, or previously authoritative writers after promotion.
- Never expose authority database paths or bytes to clients.

### API-only clients and verified reads

- Clients use the official API for queries, commands, progress, cancellation, and health.
- The [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) gateway runs
  in the enrolled daemon on the node that owns the live workspace and local
  analyzer processes. It reaches remote clean-generation authority only through
  authenticated application APIs.
- A read cache or replica is accepted only with a signed or authenticated manifest binding Brain, shard, generation, schema, privacy policy, watermark, and authority epoch.
- Responses declare remote coverage, cache age, lag, unavailable shards, and pending local observations.
- Stale or unverifiable caches may support explicitly stale reads but never writes, promotion, or healthy coverage claims.

### Remote offline-capture spool

This PR16 product is distinct from the PR6 daemon host-admission spool, which
bounds local non-replayable provider/host events before canonical capture on
the authority daemon. The remote offline-capture spool holds sanitized canonical
offline events for enrolled remote nodes and later fenced replay; it never
contains unsaved LSP documents, overlays, analyzer state, or dirty-overlay
diagnostics.

- Local hooks send bounded `HookEvent`s to the enrolled node-local daemon. That
  daemon applies the canonical sanitizer and owns the remote offline-capture
  spool when shard authority is unreachable; hooks never sanitize durable
  payloads or append remote offline-capture spool records.
- Unsaved LSP documents, dirty-overlay diagnostics, document versions, analyzer
  process state, and raw JSON-RPC frames are never remote offline-capture spool
  records. Authority loss makes their remote durable coverage partial or
  unavailable; it does not create a database or analyzer fallback.
- Remote offline-capture spool frames carry deterministic observation identity,
  node identity, repository/worktree identity, privacy policy, ordering evidence,
  and integrity checks.
- Reconnect replays idempotently through the current authority and deletes frames only after durable acknowledgement.
- Overflow, corruption, policy change, revocation, and rejected replay remain visible and recoverable; no empty local database is created as fallback.

### Repository and scope identity

- Correlate clones through verified Git repository evidence and explicit checkout, worktree, ref, and snapshot identities.
- Never merge projects by hostname, directory name, or absolute path alone.
- Preserve local-only and remotely eligible scopes end to end; remote enrollment cannot weaken existing privacy policy.
- Dirty document content remains node-local by default and is never placed in a
  verified read cache, replica, trace, backup, failover payload, or remote
  analyzer request. A remote analyzer requires an explicit capability, policy
  grant, and privacy disclosure.
- [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)'s
  session-only overlay feedback and concurrent-agent proximity computation
  stay node-local for the same reason; only durable saved-content feedback,
  GitHub-ingested review evidence, and CI-localization evidence are fenced
  through the shard authority. No GitHub write path exists at any stage.

### Backup and failover

- Create authority-owned consistent backups with manifests covering database families, payloads, generations, epochs, checkpoints, and repository identities.
- Restore into isolated staging, verify integrity and references, then publish under a higher fenced epoch.
- Promote a standby only after proving the old authority is fenced and the standby has the required durable frontier.
- Rejoining old authorities remain read-only until explicitly reseeded.
- Node revocation immediately blocks commands, replay, cache refresh, and promotion credentials.

### Operations

- PR16 application/API contracts and the then-shipped Settings, CLI, API, and
  Doctor surfaces expose topology, authority, placement, lag, remote
  offline-capture spool, replica, backup, and failover state from one
  application model. PR18 adds equivalent
  SDK bindings and parity when the SDKs ship.
- Human and structured health output use the same findings, coverage, and remediation identities.
- Connectivity profiles are replaceable transports beneath the authenticated TraceDecay protocol.

## Acceptance

- Multi-process and multi-host fixtures prove exactly one accepted writer across startup races, partitions, lease expiry, process death, reconnect, and promotion.
- A stale authority cannot commit or publish after any higher epoch is visible.
- Offline events replay exactly once in order; crash, duplicate, corruption, overflow, revocation, and privacy-change cases preserve evidence.
- Remote LSP fixtures prove overlays and analyzers stay on the workspace node,
  clean diagnostic publication is fenced through the owning shard authority,
  and authority loss never places unsaved content in the remote offline-capture
  spool or republishes stale cached diagnostics as current.
- Cache and replica fixtures reject wrong Brain, shard, generation, epoch, schema, policy, digest, and watermark claims.
- Repository fixtures correlate verified clones while separating unrelated repositories, worktrees, refs, and local-only scopes.
- Backup, staged restore, promotion, rollback, and old-authority rejoin tests never expose a partial generation or two writers.
- All surfaces shipped by PR16 report identical topology and coverage truth,
  including partial, stale, unknown, and unavailable states; PR18 SDK
  conformance proves the same values when SDK bindings ship.
- Negative tests prove no client, hook, cache, replica, or offline path opens an authority database or uses a network filesystem.
- [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)
  PR16 fixtures prove unsaved dirty overlays and concurrent-agent proximity
  computation stay node-local and never enter the remote offline-capture spool,
  verified read cache, replica, trace, backup, or failover payload.
- Plan 37 PR16 fixtures prove durable saved-content feedback,
  GitHub-ingested review-thread/comment/reply evidence, and CI-localization
  evidence are fenced through the owning shard authority and never travel
  through overlay or proximity paths.
- Plan 37 PR16 restart, failover, and promotion fixtures preserve fenced
  feedback and ingested-evidence watermarks, tombstones, and authority epochs
  without republishing stale cached state as current.
- Plan 37 PR16 retention, deletion, authorization, and privacy-policy change
  fixtures on fenced evidence fail closed; possessing a remote cache handle or
  replica manifest never bypasses recheck.
- Plan 37 PR16 fixtures report remote partial, stale, unknown, and unavailable
  coverage identically on every PR16 surface without inventing local durability.
- Plan 37 PR16 negative fixtures prove unsaved LSP content, dirty-overlay
  diagnostics, and session-only overlay feedback never become remote durable
  records.
- Plan 37 PR16 acceptance reuses the remote offline-capture spool boundary
  defined above; the PR6 daemon host-admission spool and the PR16 remote
  offline-capture spool remain distinct products with separate scope.
