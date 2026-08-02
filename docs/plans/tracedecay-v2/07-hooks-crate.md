# V2 host hooks and cross-worktree event boundary

## Status and existing foundation

PR6 captured the supported Claude Code, Codex, Cursor, Hermes, and Kiro event
semantics. PR13 adds the verified Kimi Code and OpenCode event capabilities
and replaces the remaining compatibility planners with thin, bounded adapters
that signal one daemon-owned feedback path. Hooks are transport adapters; they
do not own product state, synchronization, Git, feedback policy, or peer
coordination.

PR6 artifact names, inventories, matrices, packets, and intermediate file
layouts are historical implementation evidence, not prerequisites or
requirements to recreate. Only an artifact that remains a published public
wire/API compatibility contract is retained by name; current and future audits
assess the host behavior, wire compatibility, fresh-store reset, safety, and
regressions specified here.

The V1 envelope is the initial final wire format, not evidence for a V2
sibling. Pure source-only/internal pre-admission helpers and branch-local V2
host DTOs change in place. Every persisted spool/file projection accepts only
its exact final shape; any other shape returns typed `ResetRequired` and
requires explicit reset or recreation. No storage decoder, migration,
backfill, dual write, or census path survives. An actually independently
released public envelope may retain separate protocol negotiation. Fixture
revisions alone remain insufficient evidence.

## PR13 user outcome

After a supported saved edit or agent stop boundary, the host can receive the
same current TraceDecay feedback available through MCP, CLI, LSP, or native
diagnostics. A temporarily unavailable daemon does not block the host or
corrupt product state, and replay does not duplicate a logical event.

## End-to-end production path

1. The host adapter decodes a real native edit, tool, test, session, or stop
   event through the bounded V1 wire decoder and verifies that the host/version
   actually supports it. The envelope revision is checked before any field is
   interpreted, admitted, or spooled.
2. The adapter obtains canonical project, repository, worktree, and epoch
   identity from a daemon-issued session binding. Paths and branch labels are
   resolver inputs, never identity.
3. The adapter emits one bounded, content-free event with stable event,
   ordering, payload-digest, and idempotency identity.
4. It sends the event to `tracedecayd`. If an eligible bound event cannot be
   delivered within the host budget, it appends the exact validated bytes to
   the sole bounded transport replay spool and returns.
5. The daemon reauthorizes the binding and epoch, durably admits the event,
   schedules capture/synchronization and the Plan 09 one-shot feedback cycle,
   then returns a typed receipt and, only if it was already current before the
   hook deadline, bounded already-ready guidance.
6. The adapter renders that already-ready guidance only where the native event
   permits it. Model invocation, GitHub/CI acquisition, Context Scout, and
   feedback refresh always continue asynchronously; the synchronous hook never
   waits for them. Spool acceptance, daemon admission, feedback completion,
   and display remain distinct outcomes.

## PR13 implementation slices

### Native edit and stop signaling

- Cut over real saved-edit and stop/pre-stop boundaries for each supported
  host. Receipt-derived edit or test events are emitted only when the native
  event supplies typed identity; otherwise the capability is truthfully
  unavailable.
- Keep host deadlines and payload limits hard. Hooks do not search, invoke a
  model, run Git, scan a workspace, call GitHub, localize CI, calculate
  proximity, or synchronize data.
- Optional guidance fails open. A hook timeout or unavailable daemon never
  becomes a false clean result or a false accepted receipt.

The retained event families are session/workspace lifecycle, worktree-epoch
change, native ref/commit observation, tool lifecycle, saved edit, test
lifecycle, and advisory conflict observed/cleared. Events carry canonical IDs,
bounded ranges or typed record IDs, digest, terminal state, and receipt
references where available; they never carry source, paths, command text,
arguments, environment, output, or test logs. Conflict observations remain
advisory and create no lock, dependency, assignment, or merge decision.

Wire compatibility remains normative on this production path:

- V1 decoding is bounded before allocation and validates the exact envelope,
  payload, string, collection, and nesting limits. Structs use
  `deny_unknown_fields` where the V1 contract specifies closed fields, and
  wire enums are exhaustively matched; neither silently ignores a value that
  could change identity, ordering, effect, or terminal meaning.
- Every envelope carries an explicit schema revision. An older decoder rejects
  every newer revision before interpreting its payload. Unsupported or
  unknown versions are quarantined with bounded content-free reason and digest
  metadata; they are never admitted, projected, acknowledged as accepted, or
  replayed through a known decoder.
- An actually independently released public envelope may negotiate its
  documented protocol revision at the host boundary. That compatibility never
  makes an old spool readable: a non-final persisted spool returns
  `ResetRequired` before interpretation and requires explicit reset or
  recreation. There is no migration window, compatibility decoder, backfill,
  dual write, or census path for stored events. Final-shape replay remains
  idempotent and preserves the emitted revision in its receipt.

Host capability remains versioned and evidence-backed:

- Claude Code supplies session, post-tool, and stop boundaries plus native
  tool lifecycle; typed edit/test identity may be receipt-derived.
- Codex supplies session, post-tool, and turn boundaries plus native tool
  lifecycle; typed edit/test identity may be receipt-derived.
- Cursor supplies session/workspace and after-file-edit boundaries, native
  saved edits, and receipt-derived tests. Native editor diagnostics remain a
  Plan 35 surface rather than hook events.
- Hermes supplies terminal receipts, turn completion/ingestion, and terminal
  tool state; edit/test events require typed receipt identity.
- Kiro supplies its proved session/workspace/prompt boundaries. Tool, edit, or
  test events remain unavailable until a checked-in native event proves their
  exact ordering and response contract.
- Kimi Code's documented plugin contract supplies manifest-scoped or global
  `PostToolUse` and `Stop` hooks. `PostToolUse` may emit typed edit/test
  identity only when the native payload or owning receipt proves it; `Stop`
  supplies the native stop boundary.
- OpenCode's documented local JS/TS plugin event API supplies `file.edited`,
  `tool.execute.after`, `session.idle`/`session.status`, and LSP events.
  Saved-edit, post-tool, and stop/quiescence signals retain their native event
  identity and ordering rather than being inferred from prompts or terminal
  text.

Ref, commit, worktree epoch, and conflict truth is daemon-derived when the host
does not expose a typed native record. No adapter parses shell text to
manufacture Git authority. Unknown events remain explicit and harmless.

### Replay-safe daemon admission

- Preserve at-least-once transport with exact-duplicate convergence and
  explicit conflict for reuse of an event identity with different bytes.
- Reject stale bindings, stale worktree epochs, revoked authorization,
  malformed or oversized events, and unsupported host capabilities before
  product mutation.
- Keep one checksummed, append-only, quota- and age-bounded transport spool.
  It stores validated event bytes only, is not queryable product state, never
  overwrites unacknowledged records, and replays fairly across sessions.
- Reauthorize every replay against the current project/user binding,
  capability, and epoch. Permanent rejection receives a tombstone receipt;
  transient pressure leaves the record pending.

Retain the accepted operational bounds: one event is at most 16 KiB; one replay
batch is at most 64 records/256 KiB; the spool is bounded to 4,096 records or
32 MiB per host, 1,024 records or 8 MiB per producer/session, and 24 hours.
Replay is FIFO within a provider sequence and fair across sessions, with at
most four sessions and one in-flight batch per session. Unknown ordering stays
unknown rather than being inferred from arrival time.

Saved edits with the same worktree/file/content identity coalesce for 75 ms
with a 250 ms maximum. Tool/test progress coalesces for 100 ms with a 500 ms
maximum while every terminal event survives. Epoch, ref, commit, conflict, and
clear events bypass debounce and use reserved admission capacity so progress
traffic cannot starve them.

The synchronous path keeps the stricter measured host deadline and never
exceeds 100 ms. At 25 ms without acknowledgement, an eligible event switches
to spool append; replay never runs on the hook path. Backpressure is typed and
never interpreted as acceptance. Its response contains only the admission/
spool receipt, already-ready guidance, or a typed unavailable/backpressure
state; it never contains the result of work started by that hook.

### Feedback delivery and cutover

- Deliver only the single Plan 09 feedback result used by Plan 22 Scout,
  Plan 35 diagnostics, and Plan 37 GitHub/CI/proximity findings. Hook-local
  ranking or fallback logic is prohibited.
- Deliver newly computed feedback through the daemon-owned asynchronous host
  projection/read path. A later host callback, MCP/CLI/HTTP read, LSP/native
  diagnostic publication, or explicit receipt inspection may observe it; the
  original synchronous hook response is never held open for completion.
- Preserve distinct dispositions for rejected, backpressured, daemon-accepted,
  spool-accepted-for-replay, projected, effect-completed, and guidance
  displayed. Only the owning daemon/application receipt may claim its effect.
- Shadow the V2 path against current host behavior, cut over one proven native
  event family at a time, and retain a direct rollback switch until receipt
  parity passes.
- Delete each path/command-derived compatibility planner after its native
  family cuts over. No second hook authority or business-event queue remains.

## Replacement and deletion

- Remove the reserved pre-PR17 task-placement and ready-commit fields and
  variants from the PR13 event contract.
- Treat generated host matrices, exact schema/file inventories, milestone
  gates, and placeholder hook benchmarks as retired historical scaffolding.
  Checked-in native events remain evidence where needed to prove supported
  behavior and capability differences, but obsolete artifact names are not
  mandatory recreation targets.
- Remove hook-local sync, Git inference, conflict/proximity calculation,
  policy fallback, and writable-store access as each provider cuts over.
- This pruning removes no native event family, host capability, replay mode,
  response behavior, compatibility obligation, operational bound, or safety
  requirement.

## Direct acceptance

- A real saved edit and a real stop boundary on every supported host reach the
  daemon and can render the same bounded feedback result where that host
  supports active delivery. Kimi Code exercises manifest and global
  `PostToolUse`/`Stop` registration, and OpenCode exercises `file.edited`,
  `tool.execute.after`, `session.idle`/`session.status`, and LSP event delivery
  through a real local JS/TS plugin.
- Timing tests prove the synchronous response returns only a receipt or
  already-ready guidance within the host deadline while delayed model,
  GitHub/CI, Context Scout, and feedback work completes asynchronously and is
  observed through a later supported delivery/read path.
- Duplicate delivery, restart before/after spool append, daemon restart, stale
  epoch, revoked authorization, saturation, spool exhaustion, malformed input,
  and cancellation produce stable typed outcomes with no duplicate logical
  event.
- Wire fixtures prove bounded V1 decoding, exhaustive enum handling,
  `deny_unknown_fields` on closed V1 structures, older-decoder rejection of
  every newer revision, unknown-version quarantine, release-proven protocol
  negotiation at the host boundary, final-shape spool writes, and typed
  `ResetRequired` rejection of every non-final spool without duplicate
  admission.
- A daemon outage leaves the host responsive; eligible events replay later,
  while unbound, oversized, expired, or full-spool cases rely on authoritative
  host/Git catch-up and never create another writer.
- Hook event, spool, telemetry, and error bytes contain no prompts, commands,
  source, paths, tool arguments/output, test logs, credentials, or hidden peer
  identity.
- Content-free telemetry preserves hook wall time, daemon round-trip, bytes,
  queue/spool state, replay, timeout, capability, and disposition so latency
  and reliability remain observable without payload capture.
- Host adapters never open a TraceDecay database, run synchronization, invoke
  Git/GitHub/CI, write another worktree, schedule work, or continue an agent.
- The relevant host-hook integration tests plus the repository documentation
  review run through ordinary repository checks, not a PR13 acceptance gate;
  no standalone benchmark packet is an acceptance artifact.

## Later callable extensions

- **PR15:** a multi-root host resolves each root independently through Plan 16
  and obtains one binding and event stream per admitted root. Denied or
  ambiguous roots remain explicit and cannot be folded into a neighbor.
- **PR17:** after Plan 24 ships task identity and Plan 32 ships runtime
  receipts, a host may call their application operations with an independently
  authorized task join. The event contract then adds the retained task-placed
  and dependency-ready commit observations under a new capability revision.
  `TaskId` is present only when the daemon independently authorizes the exact
  task/worktree relation; readiness comes only from Plan 24/36 state and
  completion only from an owning Plan 32 receipt. The hook remains an
  observation transport and cannot schedule work, infer readiness, or
  synthesize completion.

## Safety constraints retained

- Hooks are bounded, replay-safe, content-free, and never durable product
  authorities.
- Unsaved or dirty source content is never placed in the replay spool or any
  durable feedback record.
- Capability absence, daemon failure, stale scope, and partial processing are
  reported honestly; none becomes clean or supported.
- No hook performs GitHub writes, Git history mutation, agent continuation, or
  peer-worktree coordination.
