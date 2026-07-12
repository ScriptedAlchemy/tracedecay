# TraceDecay V2 Remote Multi-Machine Shared Brain Plan

**Status:** normative cross-cutting plan. This pull request contains plans only.

**Decision:** TraceDecay supports one logical Brain across multiple machines without making Tailscale, a hosted vendor, a network filesystem, or a particular database service mandatory. Tailscale is one optional private-connectivity profile. The first supported distributed topology uses one fenced TraceDecay authority per mutable shard, reached through the official authenticated application/API protocol. SQLite files and WAL families remain local to their owning process and host.

## 1. Outcomes

- A workstation, cloud server, and other enrolled nodes can share one `BrainId` while retaining explicit node, checkout, worktree, privacy, and provenance identity.
- Equivalent clones on different machines correlate to one canonical repository through verified Git evidence, never through absolute paths or names.
- All/Brain, Explorer, retrieval, task coordination, hints, and automation see one truthful authorized system view with explicit remote coverage, lag, cache age, and pending local work.
- Hooks remain local, bounded, and usable during network loss. Sanitized observations queue durably and replay idempotently.
- Local-only, metadata-only, and remotely eligible data can coexist in one Brain without silently weakening privacy.
- Backup, restore, standby promotion, node revocation, and reconnect cannot create two accepted writers.
- Operators can inspect and control topology through Settings, CLI, API, and generated SDKs. Ordinary MCP tools expose compact health and coverage; operator mutations require an explicit optional component and grant.

## 2. Non-goals and prohibited shortcuts

- No SQLite database, `-wal`, or `-shm` file is opened through NFS, SMB, SSHFS, Taildrive, a Tailscale-mounted filesystem, or another network filesystem. SQLite WAL requires same-host shared memory and its locking assumptions are unsafe over many network filesystems.
- Each mutable shard's placed daemon is its only SQLite opener. `RemoteAuthorityOnly` clients have no database files; `DedicatedServiceIdentity` authority/replica nodes keep stores under a service identity and ACL that client identities cannot read, while the authenticated API/socket remains reachable. A same-user node is explicitly degraded and cannot claim that SQLite locks or file mode alone prevent direct reads.
- No client receives database paths, database credentials, SQL access, or a database URL. Remote clients call TraceDecay use cases.
- No implicit multi-primary writes, last-write-wins state, clock-based conflict resolution, automatic offline authority promotion, or merge-by-hostname.
- No assumption that the same remote URL means the same repository: forks, mirrors, upstream aliases, shallow clones, replacements, grafts, and rewritten history remain explicit evidence cases.
- No upload of unsanitized source records, protected-quarantine plaintext, credentials, hidden reasoning, or data whose sync policy forbids the destination.
- No new production crate solely for distribution. Reuse domain contracts, store repositories, application use cases, the root API boundary, and official clients; the root owns remote transport adapters.
- No immediate libSQL/Turso dependency. A later remote-engine or local-first adapter requires an ADR, identical semantic/fault/privacy gates, and proof that it reduces rather than duplicates the authority protocol.

## 3. Topology and authority model

One binary can run in five declared roles:

| Role | Canonical writes | Reads | Offline behavior |
|---|---|---|---|
| `Standalone` | Local authority for all placed shards | Local | Fully local |
| `Authority` | Exactly the shards assigned to its current fenced epoch | Local plus federated reads | Continues for locally authoritative shards |
| `RemoteClient` | None directly; submits commands/observations to authority | Authority or bounded local cache | Cached reads and durable local capture spool only |
| `ReadReplica` | None | Signed snapshot plus canonical tail to a declared watermark | Read-only at last verified watermark |
| `Standby` | None until an explicit fenced promotion receipt | Verified restore/replica state | Never self-promotes |

`ShardPlacementV1` binds a logical shard to one `StoreAuthorityId`, `AuthorityEpoch`, schema/catalog/privacy versions, allowed replicas, and a placement version. Every command, append, snapshot, tail, and receipt carries those values. In particular, signed sync receipts carry their own `ShardId`, `PrivacyDomainId`, authority/node/epoch, placement version, accepted causal frontier, committed vector watermark, batch digest, and distinct upload-manifest digest; verification never reconstructs signed claims from a mutable placement row or stream head. A stale epoch is rejected before mutation. Promotion increments the authority epoch and publishes a recovery receipt; the previous authority can never resume writes under its old lease.

The first release does not pretend that an epoch number can fence a disconnected machine by itself. An authority may continue canonical writes while isolated, so standby promotion is prohibited until exclusivity is positively proven by one of: a signed graceful-shutdown/fence receipt from the old authority; verified revocation of an external exclusive storage/compute lease that physically prevents that node from serving or writing; or a configured independent quorum lease service whose term has expired and cannot be renewed by the old node. Mere unreachability, elapsed wall time, operator belief, or a newer catalog row is insufficient. Without proof, recovery waits for the authority or creates an explicitly separate forked Brain for forensic/export use; it never promotes under the same `BrainId`.

Supported first-release deployments:

1. `Local`: current single-machine behavior.
2. `Remote authority`: canonical profile/activity/project stores live on an enrolled server; clients use the official API.
3. `Remote authority + read cache`: a client retains an encrypted, bounded, watermark-bound cache and local capture spool. The cache is never authority or backup.
4. `Hybrid placement`: whole `(ShardId, PrivacyDomainId)` placement units stay local or remote; row-level split authority is forbidden. Project shards may differ. The profile activity shard has one placement and uses the most restrictive applicable ordinary activity sync policy; if any ordinary activity class is `NeverSync`, that activity authority stays on the designated local node and remote clients reach it only when authorized and available. Protected quarantine is already a separate local-only domain. All/Brain reports omitted and unreachable units rather than presenting partial data as complete.

Multi-primary canonical replication is deferred. If later required, it must implement the same repository/application contracts behind consensus or an equally explicit leader/fencing protocol and pass split-brain, partition, revocation, restore, and deterministic projection gates.

## 4. Canonical identity and Git correlation

Plan 01 is the sole owner of `BrainId`, `BrainNodeId`, `NodeEpoch`, `StoreAuthorityId`, `AuthorityEpoch`, `CheckoutId`, `EventDotV1`, bounded `CausalFrontierV1`, `ShardPlacementV1`, signed `SyncReceiptV1`, `RepositoryIdentityProofV1`, `AuthorityFenceProofV1`, `AuthorityRecoveryReceiptV1`, `CacheAccessManifestV1`, `CacheGrantSnapshotV1`, remote cursor bindings, and remote coverage. This plan consumes those exact canonical shapes and never redeclares transport-local or storage-local variants.

`BrainNodeId` is a TraceDecay enrollment/key identity. Hostnames, local paths, IP addresses, Tailscale node IDs, and OS user names are observations or transport evidence, never canonical node identity. Re-enrollment after key loss creates a new node epoch and auditable adoption link.

Repository reconciliation uses an evidence-scored proof:

1. Normalize credential-free remote/forge identities and record aliases without tokens, user info, or signed query parameters.
2. Record Git object format and local object evidence.
3. Verify immutable shared commit/tree identities and ancestry where objects are available.
4. Distinguish fork, upstream, mirror, rewritten, shallow, partial, grafted, and replacement-object cases.
5. Resolve to an existing `RepositoryId` only above a locked confidence threshold and with no contradictory evidence.
6. Return candidates and require an adoption receipt when ambiguous. Never silently merge or split.

One canonical `RepositoryId` may have many node-scoped `CheckoutId` and `WorktreeId` aliases. `git-common-dir` and paths identify a checkout/worktree only on that node. Clean snapshots can be reused by repository + commit + index-manifest digest; dirty overlays remain distinct by node/worktree/content digest.

## 5. Replication units and consistency law

Replication moves semantic TraceDecay artifacts, not mutable database pages:

- sanitized immutable observation batches;
- canonical event-tail manifests;
- signed read snapshots and projection manifests;
- authority-built and signed immutable Git/code graph packs and content-addressed blobs eligible for the destination;
- authority-built immutable semantic-vector generations only when their manifest fully pins and matches repository/snapshot, source-text/sanitizer/privacy digest, vector schema/metric/dimension/quantization, FastEmbed runtime ABI, exact embedding artifact/model/tokenizer digest, representation config, generation builder version, and destination capability/catalog/schema versions;
- tombstones, retention proofs, revocations, membership, and placement revisions;
- acknowledgement, gap, quarantine, and conflict receipts.

`EventDotV1` and bounded causal frontiers describe replication provenance. They do not replace deterministic `ObservationId`, evidence causality, per-shard sequence, or query vector watermarks.

A frontier contains at most 1,024 sorted unique `(BrainNodeId, NodeEpoch, max_sequence)` components. Enrollment admission refuses growth beyond the bound until every member of the compaction's frozen membership epoch has a terminal disposition: current authorities/replicas acknowledge, while a positively fenced/revoked node receives a signed tombstone disposition without acknowledgement. Retired node epochs compact only after their accepted sequence, revocation generation, disposition, and backup horizon are covered; reconnecting nodes older than that floor must re-seed from a signed snapshot and cannot upload an omitted epoch. An offline current member still blocks; a destroyed or revoked member cannot block forever.

Consistency rules:

- same observation ID + same canonical digest: idempotent success;
- same observation ID + different digest: quarantine and visible collision, never overwrite;
- append-only assertions: union with provenance and supersession semantics;
- mutable facts/config/tasks/leases/membership: authority-only compare-and-swap with expected versions;
- immutable Git/code facts: address by verified repository/snapshot/commit/manifest identity;
- projectors: authority alone advances canonical checkpoints and publishes read generations;
- replicas/caches: read-only, manifest verified, and bounded by an explicit watermark;
- wall clocks: display and latency evidence only, never conflict authority.

## 6. Capture, offline spool, and synchronization

Local hooks never synchronously depend on remote connectivity. The capture path is:

1. Parse and sanitize locally before durable storage or transfer.
2. Classify the record against the resolved sync/privacy policy.
3. Frame an AEAD-encrypted append-only spool entry with node/source stream, monotonic sequence, previous digest, deterministic observation ID, payload digest, schema/privacy versions, destination placement, encryption-key epoch, nonce, and authenticated frame header. Plan 03 alone owns this format and key rotation.
4. Return `DurablyQueued` only after local durability; never claim canonical commit.
5. Upload bounded ordered batches with idempotency keys and causal frontier.
6. Authority validates enrollment, grants, placement/epoch, schema/privacy compatibility, continuity, digest, and policy before committing.
7. Authority returns plan 01's signed `SyncReceiptV1` only after canonical commit. Canonical signing bytes bind Brain, shard, privacy domain, authority and authority node/epoch, source node/epoch/stream/batch range, batch digest, distinct upload-manifest digest, placement/schema/registry/privacy-policy versions, accepted causal frontier, committed watermark, revocation generation, signing key ID/epoch, issued/expiry times, and nonce. Clients verify the byte-identical receipt, trust chain, revocation generation, expiry, and replay uniqueness before retiring local bytes; mutable placement/schema/registry/privacy/head state cannot fill an omitted field.
8. Client retires a spool range only after receipt verification and durable acknowledgement state.

Gaps, duplicate/reordered uploads, crash-before-commit, crash-after-commit-before-ack, disk-full, schema skew, revoked enrollment, changed placement, and policy tightening have distinct states and remediations. Rejected records remain locally visible and bounded; they are never silently discarded or endlessly retried.

Offline reads declare one of:

- `Authoritative` — answered at the current authority watermark;
- `BoundedStale { max_lag }` — cache/replica accepted within a requested bound;
- `OfflineCache` — explicit last verified watermark and cache age;
- `AsOfWatermark` — reproducible snapshot/vector frontier.

Pending local observations appear as a separate non-canonical overlay. Canonical commands fail closed offline. Complex task/config edits may be authored as non-authoritative validated bundles with expected versions, then submitted to the authority; conflicts return current versions and a repairable validation report, never last-write-wins.

Every cache/replica manifest binds a signed `CacheGrantSnapshotV1` containing the full immutable `CacheAccessManifestV1`: principal/node, exact resolved scope ID/digest, bounded allowed registry field IDs/payload classes, capability-grant set, policy version, privacy-policy digest, schema-registry digest, and capability-catalog generation/digest. Plan 02 persists that manifest by canonical digest beside the signed grant; offline validation never depends on an unavailable mutable authorization/catalog lookup. Offline reads stop and the cache locks when the grant expires or its manifest is absent, corrupt, or mismatched. Validation uses the maximum of current wall time and the persisted last trusted authority time advanced by monotonic elapsed time; wall-clock rollback/reboot without a trustworthy continuation locks rather than extends access. Policy/catalog tightening or revocation is immediate for connected nodes and bounded by `not_after` while disconnected. On reconnect, catalog/grant revalidation and tombstone/purge acknowledgement complete before cached content may serve again. UI/API report expired/locked/catalog-mismatched and pending purge acknowledgements explicitly.

## 7. Projectors, automation, hints, and coordination

Only the current authority runs canonical projectors, schedulers, curation, task leases, effectors, retention, and autonomous memory/skill evolution for its shards. This prevents repeated automation on two machines and makes unchanged-input admission globally enforceable.

Remote nodes may:

- capture and sanitize observations;
- evaluate a pinned hint/policy bundle locally for hook latency;
- request remote query/context suggestions asynchronously;
- upload delivery/outcome receipts;
- render read-only cache and pending overlays.

The first remote release deliberately has no remote code-extraction import protocol. Remote nodes upload sanitized code/Git observations; the current authority alone builds and signs canonical graph/index packs. Every transferable pack carries plan 01 `GraphPackManifestV1`; plan 02 verifies its authority/epoch/placement, repository/generation/snapshot/watermark, schema/catalog/privacy, byte digest/length, and signature, binds the exact pack set into each replica/cache manifest, and retains it through live/backup/rollback references. Missing/corrupt/unauthorized packs yield partial/locked coverage, never fallback. Replication may copy those authority-signed immutable packs outward to eligible replicas/caches, never accept an inbound pack as canonical truth. Remote extraction is reconsidered only by a later ADR with an untrusted-build verification protocol and cannot be inferred from generic artifact upload.

Plan 28 exclusively owns remote semantic-vector-generation replication and multi-host compatibility; plans 02 and 25 own only the local manifest/storage and eligible document/chunk inputs. Semantic vector generations follow the immutable-pack direction and are optional. A destination accepts one only when its `NativeFastEmbedRuntimeManifestV1` digest and representation-profile digest match and every vector-space-critical pin matches exactly: model/tokenizer revisions and artifacts, runtime ABI/build, target/CPU/execution provider, requested/actual threads/session/batch, determinism class, dimension, quantization, pooling, query/document prefix, truncation/maximum-input/overlap, metric, normalization, formatter/chunker, privacy/key epoch, source/input digest, and generation builder. Replicated rows retain `(document_id, chunk_id, vector)` identity; missing/colliding `chunk_id` rejects the pack. Any mismatch yields `incompatible`/`rebuild_required`, and `search.universal`/`code.search_symbols` either returns the typed strict-mode error or the byte-stable lexical fallback. It never loads, relabels, or rebuilds with an alternate model silently. FastEmbed embedding and BGE rerank artifacts, downloaded/imported bytes, native session state, and warm caches are machine-local and never replicated as Brain data. Each node independently obtains explicit install/import/download consent, verifies the exact artifact, and reports desired/activated/effective/observed state; only compatible immutable vector generations may sync.

The separately registered optional Codex Spark/app-server-style reranker does not change replication units. No model credential, provider cache, request/response payload, or inferred model state syncs through Brain replication. Remote execution is allowed only through the ordinary authorized application route with discovered capability, privacy/egress, exact model, cost/token/deadline/top-N budgets, and requested/actual route receipt; unavailable/timeout preserves pre-rerank order. It remains off by default, supplies no embeddings, and cannot replace the promoted FastEmbed embedding or native BGE reranker.

Every hint/suggestion records whether evidence was authoritative, bounded-stale, or local-pending. Nearby-agent coordination spans enrolled nodes only when authorized and sufficiently fresh; stale remote activity cannot trigger a definitive duplicate-work claim. Task leases and work claims are authority-fenced.

## 8. Query, coverage, and All/Brain

The query planner resolves logical scope before physical placement, then routes each shard to its authority, verified replica, or declared unavailable state. Global merge compares normalized rank/evidence contracts, never raw shard-local scores.

A single Profile-root activity query, optionally filtered by `DeclaredScope::Profile` or `DeclaredScope::ZeroProject`, routes directly to the one placed profile-activity authority regardless of the client's CWD, current project, host profile, or where any project shard is placed. An unavailable profile authority remains typed unavailable even when a local project shard is healthy; an unavailable project shard cannot suppress a healthy profile-only answer. Explicit multi-root queries may combine Profile and Project roots only after independent authorization/placement resolution and retain per-root coverage.

Every response coverage includes:

- `BrainId`, placement generation, and requested consistency;
- participating authority/replica/node identities and authority epochs;
- per-shard watermarks, cache ages, and sync lag;
- unreachable, unauthorized, local-only, stale, rebuilding, or policy-excluded scopes;
- pending local observation counts separately from canonical totals;
- whether a repository has unresolved cross-node identity candidates.

All/Brain uses the shared logical identity graph, so the same repository cloned on a laptop and server is one repository node with multiple checkout/worktree lenses. Topology is a first-class graph overlay: Brain nodes, authorities, shard placements, repository bindings, replica/cache edges, lag, pending spools, privacy boundaries, and health. Ordinary product graphs do not duplicate entities by host.

## 9. Security, privacy, and enrollment

Transport profiles are replaceable:

- authenticated HTTPS/mTLS over ordinary networks;
- LAN with the same application security;
- optional Tailscale or another VPN/private overlay;
- a correctly configured reverse proxy or tunnel.

Tailscale identity, grants, app capabilities, or device posture can narrow TraceDecay access and assist enrollment, but never replace application identity or widen TraceDecay grants. TraceDecay validates its own node key, scoped principal, `BrainId`, requested use case, project/privacy domain, and authority epoch.

Remote mode requires:

- explicit node enrollment with short-lived bootstrap material;
- long-lived node keys in the OS credential store, rotatable and revocable;
- TLS 1.3 or an equivalently authenticated protected channel;
- allowlisted listeners/authorities and pinned proxy trust;
- no wildcard CORS, no bearer token in URLs/logs/config exports, strict Origin/CSRF for browsers;
- stream closure and denial of new reads/writes after revocation;
- audit receipts for join, grant, policy, placement, revoke, promotion, restore, and key events.

Each domain resolves one sync class:

| Class | Remote behavior |
|---|---|
| `NeverSync` | Content and derived descendants remain on their local authority; only explicit non-sensitive availability may be reported. |
| `MetadataOnly` | Allowlisted identity/count/health metadata; no payload or reconstructable derivative. |
| `SanitizedEncrypted` | Sanitized eligible records over authenticated transport and encrypted at rest. |
| `FullEligible` | All sanitizer-approved fields allowed by domain and principal policy. |

Protected quarantine defaults to `NeverSync`. Policy can become stricter immediately; relaxation requires explicit activation and a fresh eligibility scan. The sender enforces policy before upload and the receiver revalidates it. Remote deletion propagates signed tombstones and purge proofs; an offline node cannot resurrect retired data.

## 10. Application, API, SDK, CLI, and MCP contract

Add application use cases, generated from the capability registry:

```text
brain.status.get
brain.topology.get
brain.nodes.list|get
brain.join|leave
brain.nodes.rotate|revoke
brain.placements.list|plan|apply|verify
brain.sync.status|run|pause|resume|repair
brain.replicas.list|seed|verify|retire
brain.backup.status|verify
brain.failover.plan|promote|verify
brain.repositories.candidates|adopt|split
```

This is the closed catalog family owned by plan 08 and implemented by plan 09. `brain.join` is the only public enrollment/bootstrap workflow; it creates node enrollment and initial placement atomically or compensates both. `brain.leave` revokes the current node and retires its eligible cache/replica state after authority-transfer preconditions. CLI/HTTP/SDK/MCP/UI names below are generated bindings of these use cases, not additional operations.

Remote mutation operations use idempotency keys, expected versions, explicit effect classification, authorization, progress resources, audit receipts, and resumable operation IDs. Promotion and restore publication contend on one operation-scoped exclusive Brain/shard lifecycle lease, freeze expected placement/catalog/schema/privacy versions, and publish only through a higher-authority-epoch plus manifest CAS; kill/concurrency tests prove one winner. Promotion, restore publication, node revocation, placement changes, and destructive replica retirement are operator-only.

API additions include topology/node/enrollment/placement/sync/replica/backup/failover resources and SSE changes. The remote handshake binds protocol/schema/catalog/privacy versions, `BrainId`, node identity/epoch, authority epoch, grants, placement generation, and causal frontier. Incompatible clients receive a structured upgrade problem before data transfer.

CLI information architecture:

```text
tracedecay system brain status
tracedecay system brain join --authority https://brain.example
tracedecay system brain nodes list
tracedecay system brain sync status
tracedecay system brain repositories candidates
tracedecay system brain placements plan --file placement.yaml
tracedecay system brain failover promote --receipt <verified-standby-receipt> --fence-receipt <exclusive-fence-receipt>
```

`join` prints a plan and requires the existing effect/confirmation policy before changing identity or placement. Tailscale examples may use a MagicDNS URL, but command semantics never mention or require Tailscale.

The default MCP context component exposes compact `brain_status` coverage and actionable retrieval IDs. Enrollment, placement, revocation, repair, and promotion are absent unless the optional operator component is explicitly registered and granted. MCP never exposes sync chunks, key material, credentials, raw store locations, or SQL.

## 11. Dashboard experience

Add `/settings/brain` and `/observatory/sync`:

- interactive topology map of nodes, stores, authorities, replicas, caches, repository/checkouts, sync links, and privacy boundaries;
- node enrollment, grants, key/certificate age, revocation, last seen, host capability, and version compatibility;
- per-shard placement, authority epoch, watermark, cache/replica lag, pending spool, conflicts, quarantine, backup age, and restore eligibility;
- repository identity candidate comparison with remote aliases, shared immutable evidence, fork/shallow warnings, and explicit adopt/split action;
- connectivity-independent status: network unavailable, unauthorized, stale cache, pending local, authority unavailable, fenced old authority, schema skew, and policy exclusion are visually distinct;
- placement and failover plan preview with effects, but no simulated apply/rollback fiction; operations return real resumable receipts;
- deep links from every node/edge/alert into Explorer, Causal Loom, Privacy, configuration history, and the relevant operation trace.

All charts have tabular/accessibility equivalents. Sensitive paths, addresses, certificates, token material, and remote aliases are reduced/redacted according to principal and sink policy.

## 12. Backup, restore, failover, and disaster recovery

Authority backups are canonical; client caches and spools are not backups. A backup manifest binds:

- `BrainId`, authority and node epochs, placements, membership, revocations, grants, and schema/catalog/privacy versions;
- causal frontier, per-shard vectors, source gaps, allocation ledgers, projector checkpoints, task leases, automation admission state, and outbox heads;
- SQLite backup hashes/page counts plus graph/blob/snapshot/tombstone manifests;
- key references/epochs without exporting key plaintext;
- separately recoverable wrapped data-encryption keys: either an offline recovery-key bundle stored outside the authority or an external KMS/escrow reference with tested access policy. Backups never contain the unwrap secret beside wrapped keys, and key rotation retains every epoch needed by the declared recovery horizon;
- privacy scan and restore-eligibility receipts.

Restore occurs in isolated staging, proves recovery-key/KMS access after total authority-node loss, verifies all anchors and privacy gates, then publishes under a higher authority epoch. Every restored task lease, execution admission, and automation admission remains historical: before serving, recovery appends restore-fence/revocation events, clears active pointers, increments every affected task fence epoch, and marks uncertain external effects for mandatory reconciliation. Only a new post-recovery admission under the new authority epoch may execute. Standby promotion requires a current verified recovery receipt plus one of §3's positive exclusive-fence proofs. An old authority reappearing after a partition is read-only/quarantined until explicitly re-seeded. Connectivity loss never promotes a cache or client automatically.

Define and test declared RPO/RTO profiles for standalone, remote authority, and verified standby. Backup age and last successful restore drill are visible in Observatory.

## 13. Observability and quality gates

Registered metrics:

- connection/auth/handshake success and latency by non-sensitive transport profile;
- spool events/bytes/oldest age, enqueue failures, upload/ack latency, retry and rejected-class counts;
- replica/cache watermark lag, snapshot/tail bytes, cache age and eviction;
- dedupe, ID-digest collision, gap, conflict, quarantine, and tombstone acknowledgement counts;
- authority epoch mismatch, fenced-write attempt, split-brain attempt, revocation propagation, and old-authority reappearance;
- query latency/coverage by consistency mode, remote fan-out, partial/unavailable shard count;
- repository identity candidate/adoption/split rates and false-merge/false-split eval outcomes;
- backup age, verified recovery point, restore/promotion duration, achieved RPO/RTO.

No metric label contains repository URLs, paths, node names, addresses, tokens, record content, or unbounded IDs. Retrieval anchors route authorized operators to detailed evidence.

Required deterministic/fault cases:

1. same repository under different paths, machines, remotes, and worktrees;
2. forks, mirrors, upstream aliases, rewritten/shallow/partial clones, grafts, replacement objects, and SHA-format differences;
3. duplicate, reordered, gapped, delayed, and corrupted batches;
4. crash before/after spool fsync, canonical commit, acknowledgement, and local receipt persistence;
5. partitions, clock skew, listener/proxy changes, certificate/key rotation, enrollment revocation, and reconnect;
6. simultaneous old/new authority, lease expiry, restore, standby promotion, and old-authority reappearance;
7. schema/catalog/privacy mismatch, policy tightening, protected data, tombstone purge, and offline resurrection attempt;
8. corrupt/stale cache or replica, incomplete snapshot, missing blob/graph pack, and cache eviction;
9. remote query cancellation, SSE gaps/backpressure, mixed consistency, and partial authorization;
10. current TraceDecay failure where a selected worktree/project context resolves to an invalid database: routing must identify the exact node/store/placement, return a structured recovery, and never encourage raw database access.
11. local and remote profile/user fact, LCM, memory-status, and message-search requests from neutral, host-home, unrelated, and project CWDs with every healthy/unavailable profile-activity versus project-shard placement combination; logical route and coverage are identical and no project is initialized as fallback.

## 14. Implementation slices

| Slice | Deliverable | Depends on |
|---|---|---|
| PR 4H | `BrainId`, node/authority epochs, placement, frontier, lossless signed sync/fence/recovery receipts, signed cache-access manifest/grant, sync-policy, and repository-proof domain contracts plus canonical-byte/signature vectors | PR 4B/4C/4G |
| Plan 03 PR 7B — remote-extension phase | After the base spool contract in the same PR: add AEAD/key-epoch remote frame fields, policy revalidation, remote acknowledgement retirement, and partition tests in the sole capture-owned spool | PR 4H and the earlier base-spool tasks inside plan 03's single PR 7B; this is not a distinct or self-dependent PR |
| PR 6H | Authority/placement/membership metadata, lossless cache-access/grant and sync/fence/recovery receipt persistence, gap/conflict state, backup/recovery additions, restart/restore round trips and signature reverification; no spool implementation | PR 5A–6D, PR 4H |
| PR 12D | Authority/replica query routing, consistency modes, coverage, signed snapshot/tail verification | PR 12A–12C, PR 6H |
| PR 24S | Application/API/SDK/CLI node enrollment, topology, placement, sync, repository adoption, revoke, backup, and failover use cases | PR 12D, PR 24A–24D |
| PR 25I | Brain Settings and Sync Observatory topology/operations UX | PR 24S, PR 25A–25C |
| PR 33I | Existing-profile authority enrollment, cross-node repository correlation, placement/import receipts, cache/replica seeding | PR 24S, PR 33R/S |
| PR 36S | Multi-machine security, fault, scale, backup/restore, RPO/RTO, and stock-client release gate | PR 25I, PR 33I |
| PR 37L | Delete legacy path-based/store-file remote assumptions and temporary compatibility routing | PR 36S |

No slice enables remote mode before sanitizer, node identity, authority fencing, coverage, backup, revocation, and recovery tests exist. Local-only remains supported and does not require a network listener.

## 15. Primary references and evaluated prior art

- SQLite documents that WAL requires all processes on the same host and is unsuitable for network filesystems: [Write-Ahead Logging](https://www.sqlite.org/wal.html), [SQLite Over a Network](https://www.sqlite.org/useovernet.html), and [How To Corrupt An SQLite Database](https://www.sqlite.org/howtocorrupt.html).
- Git supplies the canonical remote, common-directory, and ancestry primitives used as evidence rather than identity by themselves: [`git remote`](https://git-scm.com/docs/git-remote), [`git rev-parse --git-common-dir`](https://git-scm.com/docs/git-rev-parse), and [`git rev-list`](https://git-scm.com/docs/git-rev-list).
- Tailscale identity, grants, app capabilities, HTTPS, and device posture are an optional deployment integration, not an application authorization substitute: [identity](https://tailscale.com/docs/concepts/tailscale-identity), [grants](https://tailscale.com/docs/features/access-control/grants), [app capabilities](https://tailscale.com/docs/features/access-control/grants/grants-app-capabilities), [HTTPS](https://tailscale.com/docs/how-to/set-up-https-certificates), and [device posture](https://tailscale.com/docs/features/device-posture).
- Turso/libSQL embedded replicas and Turso Sync are evaluated local-first/replication prior art, not an initial dependency: [embedded replicas](https://docs.turso.tech/features/embedded-replicas/introduction) and [Turso Sync](https://docs.turso.tech/sync/usage).

## 16. Definition of done

- Local, remote-authority, cached-client, read-replica, standby, and hybrid-placement semantics are contract-tested with exactly one writable authority per shard.
- Two clones with different paths correlate when verified and remain separate when fork/ambiguity evidence requires it.
- No test, documentation, CLI, or adapter opens SQLite/WAL remotely or requires Tailscale.
- Offline capture survives every acknowledgement crash boundary without loss or duplication; offline reads and pending overlays are unmistakable.
- Authorization, expiring offline grants, revocation, privacy classes, tombstones, positive external fencing, wrapped-key recovery after total authority loss, backup/restore, and promotion pass adversarial tests; unreachability alone never promotes.
- Every signed sync/grant/fence/recovery object round-trips domain -> store -> domain byte-identically across restart/restore; altering any shard/privacy/node/frontier/manifest/catalog/scope/field/payload/proof field fails signature reverification.
- Offline caches lock on a missing/corrupt access manifest, catalog-generation mismatch, expiry, or unacknowledged purge; recovery/promotion locks on a missing or unverifiable full `AuthorityFenceProofV1`.
- All/Brain and every transport report truthful placement, coverage, consistency, lag, and recovery actions.
- PR 36S publishes fault/scale/security/RPO/RTO evidence; PR 37L proves obsolete routing is deleted.
