# TraceDecay V2 Remote Multi-Machine Shared Brain Plan

## Status / role

PR16 fully delivers the remote shared-Brain product. It builds on the PR4 authoritative store boundary and the intervening capture, projection, query, application, API, privacy, configuration, and observability work. No distributed-authority requirement is deferred.

## Outcome

Enrolled machines share one logical Brain through authenticated TraceDecay APIs while each mutable shard has exactly one fenced daemon writer. Clients remain useful offline through a sanitized event spool and verified read cache, without opening or copying authority databases.

## Owns

- Brain, node, shard-placement, authority-epoch, enrollment, and revocation contracts.
- Authenticated remote routing and API-only client behavior.
- Fenced authority transfer, standby promotion, reconnect, and split-brain prevention.
- Verified read replicas and caches with provenance, watermark, and lag.
- Durable offline event spooling and idempotent replay.
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
- A read cache or replica is accepted only with a signed or authenticated manifest binding Brain, shard, generation, schema, privacy policy, watermark, and authority epoch.
- Responses declare remote coverage, cache age, lag, unavailable shards, and pending local observations.
- Stale or unverifiable caches may support explicitly stale reads but never writes, promotion, or healthy coverage claims.

### Offline event spool

- Local hooks sanitize and append canonical events to a durable bounded spool when authority is unreachable.
- Spool frames carry deterministic observation identity, node identity, repository/worktree identity, privacy policy, ordering evidence, and integrity checks.
- Reconnect replays idempotently through the current authority and deletes frames only after durable acknowledgement.
- Overflow, corruption, policy change, revocation, and rejected replay remain visible and recoverable; no empty local database is created as fallback.

### Repository and scope identity

- Correlate clones through verified Git repository evidence and explicit checkout, worktree, ref, and snapshot identities.
- Never merge projects by hostname, directory name, or absolute path alone.
- Preserve local-only and remotely eligible scopes end to end; remote enrollment cannot weaken existing privacy policy.

### Backup and failover

- Create authority-owned consistent backups with manifests covering database families, payloads, generations, epochs, checkpoints, and repository identities.
- Restore into isolated staging, verify integrity and references, then publish under a higher fenced epoch.
- Promote a standby only after proving the old authority is fenced and the standby has the required durable frontier.
- Rejoining old authorities remain read-only until explicitly reseeded.
- Node revocation immediately blocks commands, replay, cache refresh, and promotion credentials.

### Operations

- Settings, CLI, API, SDK, and Doctor expose topology, authority, placement, lag, spool, replica, backup, and failover state from one application model.
- Human and structured health output use the same findings, coverage, and remediation identities.
- Connectivity profiles are replaceable transports beneath the authenticated TraceDecay protocol.

## Acceptance

- Multi-process and multi-host fixtures prove exactly one accepted writer across startup races, partitions, lease expiry, process death, reconnect, and promotion.
- A stale authority cannot commit or publish after any higher epoch is visible.
- Offline events replay exactly once in order; crash, duplicate, corruption, overflow, revocation, and privacy-change cases preserve evidence.
- Cache and replica fixtures reject wrong Brain, shard, generation, epoch, schema, policy, digest, and watermark claims.
- Repository fixtures correlate verified clones while separating unrelated repositories, worktrees, refs, and local-only scopes.
- Backup, staged restore, promotion, rollback, and old-authority rejoin tests never expose a partial generation or two writers.
- All product surfaces report identical topology and coverage truth, including partial, stale, unknown, and unavailable states.
- Negative tests prove no client, hook, cache, replica, or offline path opens an authority database or uses a network filesystem.
