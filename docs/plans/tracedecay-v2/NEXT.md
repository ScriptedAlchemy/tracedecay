# PR6: provider coverage and event normalization

PR5 completed the production Claude observation path. PR6 moves every remaining
supported session source onto that same sanitizer, daemon authority, durable
cursor, replay, and projection contract while preserving provider-native facts.

## Current branch status

The current branch contains the host-neutral integration catalog, the remaining
provider adapters, bounded daemon host admission, fair provider scheduling,
atomic projection and staged bounded rebuild, typed hook telemetry, and
executable native host fixtures. Focused direct tests cover catalog completeness,
provider replay and no-op behavior, admission failure and lifecycle cases,
projection convergence, cancellation, and telemetry-fixture readiness.

PR6 is not complete until the remaining correctness review is closed, the full
Linux, Windows, all-feature, Clippy, and workspace gates pass, and a clean
attested provider-observation benchmark creates the current acceptance artifact.
The benchmark index intentionally has no `current_acceptance` until that run.

## Product slice

Cover the currently supported Codex, Cursor, Hermes, Kiro, and Cline-like
sources. Reuse the shipped Claude observation contracts and rolling JSONL
scanner where their semantics match. Provider adapters remain small and retain
native identity, ordering, usage, tool, agent, reasoning-visibility, and source
generation evidence.

The shared path is:

```text
provider source or daemon-admitted host event
  -> bounded discovery and framing
  -> provider parse with explicit coverage
  -> canonical event plus provider evidence and relations
  -> mandatory sanitizer and receipt
  -> daemon-owned atomic observation/cursor/projection commit
  -> deterministic V1 compatibility projection and bounded replay
```

## Required behavior

- Implement every supported provider listed above in this PR; none remains on a
  private durable write, sanitizer, cursor, or projection path.
- Keep one provider-neutral observation envelope and typed relations for
  session, thread, Turn, message, agent/subagent, tool invocation/result,
  compaction, usage, Git, and workflow evidence. An absent or unsupported native
  fact stays unknown rather than being inferred.
- Preserve the native record identity and ordering domain. File offsets,
  provider sequences, timestamps, and content hashes remain distinct cursor
  evidence and are never compared under the wrong rule.
- Derive public source and observation identity without absolute paths, CWD,
  hostnames, mutable display labels, or database row IDs.
- Detect append, truncation, replacement, rotation, incomplete tails, malformed
  records, unknown versions, and duplicate delivery. Advance only through the
  last completely framed and dispositioned record.
- Parse structured fields before scanning values. All providers use the PR5
  classification, sanitizer, receipt, safe error, and sink-firewall path.
- Commit observation, receipt, durable source cursor, projection enqueue, and
  any provider coverage state atomically through the already-open daemon
  authority. No adapter, hook, client, or recovery path opens another writer.
- Use authoritative provider transcripts for catch-up when they are replayable.
  A source that is not replayable may use a bounded daemon host-admission spool
  on the local daemon authority with checksummed frames, explicit overflow/
  corruption state, and delete-after-commit semantics. The host-admission spool
  admits non-replayable provider/host events before canonical capture; it is
  not an LSP overlay mechanism and does not provide PR16 remote offline
  behavior. Hooks never own a spool database or storage authority.
- Make exact duplicate input a durable no-op: no observation, cursor, frontier,
  projection, or compatibility-view write. A conflicting identity fails without
  overwriting evidence or advancing progress.
- Bound discovery, records, decoded values, per-source work, total pass work,
  queue depth, memory, retries, and projection batches. Backpressure and partial
  coverage are typed outcomes; fair rotation prevents one source from starving
  another without writing scheduling state for a fully covered pass.
- Isolate a failed source. Successfully committed work remains available, the
  failed source keeps its prior frontier, and retry resumes without replaying an
  acknowledged suffix.
- Project canonical events deterministically into existing searchable V1 views
  with provider-compatible identity and content. Projector effects and
  checkpoints commit together; rebuild and incremental replay converge.
- Keep project sessions project-wide and user activity profile-wide. Resolve
  linked worktrees through canonical project identity; missing or ambiguous
  authority fails closed without a fallback store.
- Preserve explicitly exposed reasoning only with its provider visibility and
  retention state. Never infer hidden reasoning or treat protocol echoes as
  authored messages.
- Capture direct, redacted host-event fixtures for Codex, Claude Code, Cursor,
  Hermes, and Kiro so PR13 can later replace hook execution without guessing
  current event or response semantics. PR6 does not move query, model, sync, or
  storage work into hooks.
- Preserve provider-native Git evidence without interpreting or acting on it.
  [Plan 36](36-git-aware-change-context-and-index-transactions.md) owns the PR7+
  provenance, read-only semantic evidence, safe transaction, and surface phases;
  no phase autonomously mutates branches, worktrees, refs, or published history.
- Record bounded per-provider parse/commit/replay, no-op, backlog, and resource
  baselines for later PR20 comparison. A severe regression or unbounded path
  found here is fixed here, not deferred.

## Direct tests

- golden parse and normalized-event fixtures for every supported provider;
- stable identity and canonical encoding across restart, path relocation, and
  scan order;
- append, partial tail, malformed frame, oversized frame, truncation,
  replacement, rotation, unknown version, and unsupported native fact;
- exact duplicate, conflicting duplicate, reordered input, late input, and
  repeated daemon admission;
- sanitizer redaction/rejection/quarantine before every durable or visible sink;
- crash before commit and after commit-before-acknowledgement, followed by exact
  retry and restart catch-up;
- bounded backlog, fair multi-source progress, daemon host-admission spool
  overflow/corruption where applicable, cancellation, and daemon backpressure;
- deterministic incremental/rebuild projection and atomic effect/checkpoint
  rollback;
- provider-specific tool, usage, agent lineage, compaction, reasoning
  visibility, and event mapping without invented equivalence;
- missing daemon, stale authority, ambiguous scope, linked worktree, and
  concurrent-client cases without another database writer;
- direct host fixtures distinguish supported, degraded, unavailable, and
  unknown events and legal responses;
- stock Linux and Windows format, compile, Clippy, focused, and workspace tests.

## Prohibited scope

- no new observation schema, sanitizer, database authority, compatibility store,
  provider-local durable queue, or hook-side sync path;
- no universal event shape that discards provider evidence or invents ordering,
  authorship, reasoning, tool, or agent semantics;
- no plan parser, tracker, PR executor, generated provider inventory, workflow
  JavaScript, source-derived architecture model, or planning CI;
- no PR7 memory/fact model, PR8 temporal retrieval, PR9 code indexing, PR10
  semantics, PR11 policy/catalog, PR12 surface rewrite, PR13 hook cutover, or
  PR16 remote offline-capture spool, remote enrollment, or cross-node replay.

## Done

PR6 is complete when every supported provider produces sanitized immutable
observations through one daemon-owned atomic path; provider-native identity and
relations survive replay; partial, duplicate, replacement, backlog, crash, and
restart behavior is gap-free and bounded; V1 projections remain compatible;
host-event baselines are executable; and no provider or hook retains another
durable writer, sanitizer, cursor, or projection authority.
