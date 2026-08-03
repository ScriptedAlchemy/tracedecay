# TraceDecay V2 Daemon LSP Gateway and Universal Diagnostics Plan

## Status and existing foundation

Completion and acceptance are owned solely by `00-plan-set-index.md`. This plan
owns portable protocol behavior and real negotiated client journeys:
generation-bound diagnostic identity, analyzer policy/configuration, the
single-project daemon LSP 3.17 gateway, transport bridge, analyzer broker,
typed navigation/diagnostic operations, and the versioned TraceDecay context
extension. Callable behavior must remain intact; declarations, file layouts,
and protocol fixture inventories are not milestones.

Standard LSP 3.17 compatibility and explicit negotiation with independently
deployed clients remain real external protocol obligations. The TraceDecay
context-extension's request/response schemas are absent from `origin/master`
and a published release, so branch-local V2 extension revisions and durable
diagnostic state change in place. Durable diagnostics accept only their exact
final persisted shape; any other database, store, spool, file, or projection
returns typed `ResetRequired` and requires explicit reset or recreation. No
storage reader, migration, backfill, dual write, or census path exists. An
actually independently released context-extension revision may retain separate
protocol negotiation; experimental version tags and fixtures alone do not
require a version bump.

The daemon gateway/session/broker and the application LSP runtime are the
canonical implementation path. Existing structs, files, protocol fixtures, and
compile packets are evidence, not a contract spine to recreate. PR13 must make
that gateway a real host-feedback surface: Claude Code packaging
and protocol conformance, OpenCode custom LSP configuration and conformance,
Cursor desktop native diagnostics, duplicate-analyzer handling, Plan 37
finding projection, and one-root worktree feedback. Plan 27 owns installation
and repair; this plan owns protocol and provider behavior.

**Shipped-design clarification (2026-07-26).** The supported local design is
the daemon gateway plus transport bridge in `src/lsp_bridge.rs`, analyzer
broker/adapters under `src/diagnostics/lsp/`, and the explicit
`tracedecay lsp servers|bridge` commands. The historical documentation design
for a top-level `src/lsp/` module, global `--no-lsp`, `TRACEDECAY_LSP`,
`TRACEDECAY_LSP_TIMEOUT`, or a generic `[lsp]` configuration block did not
ship and is not a missing requirement of this plan.

## PR13 user outcome

A user editing in an LSP-capable host receives current analyzer and TraceDecay
diagnostics, semantic navigation, post-edit impact, read-only GitHub review
findings, CI localization, and proximity cues through one daemon gateway.
Cursor desktop receives the equivalent TraceDecay-only findings through its
native diagnostics adapter. OpenCode receives them through its documented
custom LSP configuration and local JS/TS LSP events while retaining exactly
one analyzer per language. Kimi Code and other hosts without a conforming
editor protocol retain the same application behavior through hooks, MCP, and
CLI and report automatic editor diagnostics as unavailable rather than
emulating LSP.

## End-to-end production journeys

### LSP session and semantic evidence

1. The host launches the thin stdio bridge, which authenticates and forwards
   LSP JSON-RPC to one daemon session. The bridge opens no store, starts no
   analyzer, and owns no routing policy.
2. The daemon resolves one authorized PR13 workspace root, negotiates client,
   protocol, catalog, analyzer, and configuration revisions, then accepts
   document content.
3. Incremental open/change/save/close events maintain a versioned document
   snapshot. For supported languages the overlay keeps its prior Tree-sitter
   tree, applies `InputEdit`, reparses with that tree, and uses
   `changed_ranges` to bound overlay extraction. Unsaved overlays are isolated
   per client and never create durable code/vector generations. Saved content
   rejoins the Plan 25 clean-generation pipeline only after exact content
   identity matches; duplicate save/change notifications become a no-op.
4. The broker routes each supported method to an explicitly configured
   upstream analyzer and/or current TraceDecay graph provider. Requests carry
   deadlines and cancellation; stale or superseded results cannot publish.
   Provider state may be `indexing`, but navigation, exact/lexical/graph
   results, and analyzer diagnostics use the latest complete compatible
   generation without waiting for background indexing. Semantic results are
   omitted until their complete compatible generation atomically publishes.
5. Provider results are normalized to canonical files and UTF-16 positions,
   retain provenance/freshness/coverage, and enter the Plan 09
   semantic-evidence contract. Exact compatible clean results may be cached;
   overlays never enter the durable cache.
6. Current managed diagnostics publish idempotently by session, document
   version, and generation. Reconnect may redeliver current state, while stale
   state is cleared and cannot overwrite newer publication.

### Post-edit and advisory finding projection

1. A save lifecycle event or Plan 07 host signal invokes the one-shot Plan 09
   feedback cycle.
2. Plan 37 supplies diagnostics/impact, read-only GitHub review, CI
   localization, and proximity findings with stable IDs, evidence anchors,
   lifecycle, freshness, and coverage.
3. The gateway remaps a finding only to a provably current file/function range.
   Ranged findings publish through standard diagnostics; truthful root-level
   state uses a bounded standard notification rather than a fake line-zero
   diagnostic.
4. LSP data contains only the bounded allowlist needed to identify, clear, and
   expand the finding. Bodies, diffs, logs, source, task narrative, histories,
   cursors, receipts, and full evidence remain behind authorized Plan 21
   reads.
5. Resolution, deletion, authorization loss, head/content/generation drift, or
   supersession clears or republishes monotonically. Missing coverage never
   appears as a complete zero-finding result.

### Cross-worktree advisory feedback

PR13 subscribes its explicitly admitted root to the daemon-owned Plan 07
projection. It can publish current affected-symbol, conflict, and stale-epoch
cues. The gateway never reads hook spools, accepts hook events over LSP, or
connects to peer hooks, agents, or worktrees. Plan 24/36/37 calculate task,
Git, readiness, conflict, and proximity state before projection.

## Retained PR12 gateway capabilities

- Extend the current daemon gateway implementation in place rather than
  creating a parallel protocol model or conversion layer. Preserve negotiated
  TraceDecay extension behavior, field-level projection, stale-result
  suppression, broker ownership, and host conformance. Add no otherwise-unused
  protocol dependency merely to mirror a historical type layout.
- `async-lsp` remains a future candidate only if a measured conversion deletes
  gateway code, unifies rather than duplicates `lsp-types` versions, and
  passes the existing lifecycle, cancellation, reconnect, diagnostics, and
  resource fixtures. Otherwise keep the gateway; a weaker host remains typed
  unavailable instead of lowering the protocol contract.
- LSP 3.17 initialize, initialized, shutdown, exit, document
  open/change/save/close, cancellation, progress, push diagnostics, document
  pull diagnostics, and diagnostic refresh.
- Declaration, definition, type definition, implementation, references, hover,
  document symbols, workspace symbols, call hierarchy, signature help, and
  type hierarchy when the negotiated analyzer/provider can answer them.
- Incremental synchronization with save flush, exact response correlation,
  independent notification handling, startup readiness, cancellation, bounded
  restart, reconnect, and stale-result suppression.
- Push and pull diagnostic clients with field-level capability projection.
  Optional fields are omitted when unsupported. Clients unable to preserve
  version/data identity use another host surface rather than receiving unsafe
  findings. Push finding projection requires versioned publication and
  diagnostic data support; pull projection requires document diagnostics and
  diagnostic data support. Refresh is used only when the client advertises it.
- One daemon-owned broker across configured languages. Language descriptors
  supply static language facts; Plan 20 supplies executable/runtime
  configuration; Plan 06 decides eligibility. The broker creates no third
  registry.
- Current upstream diagnostics plus current TraceDecay diagnostics with
  distinct producer identity. Agreement never fabricates higher severity.
- Exact compatible clean-generation cache reuse and invalidation on any
  workspace, content, analyzer, configuration, policy, language, or generation
  identity change.
- Rename and prepare-rename remain unadvertised until Plan 34 can prevent raw
  `WorkspaceEdit` application. General code actions, formatting, completion,
  arbitrary vendor methods, and arbitrary command execution remain
  unavailable unless their owning plans later ship a callable, conforming
  operation.

### PR12 TraceDecay context extension

The gateway owns one versioned TraceDecay context extension over standard
LSP/JSON-RPC framing. `initialize` advertises it only through explicit
experimental capability negotiation with supported schema revisions,
projection kinds, limits, and opaque-expansion support. An unnegotiated or
incompatible request returns typed unavailable; the gateway never guesses
client support.

PR12 serves real compact diagnostics, impact, affected-test, and test-result
projections from the same canonical application reads as CLI/MCP/HTTP. Each
envelope binds authorized project/root/document scope, content and graph
generation, request/result revision, producer state, coverage, omissions,
bounded items, and opaque expansion handles. Handles are short-lived transport
references that reauthorize through Plan 21; they never replace finding/test/
anchor identity or grant access by possession.

The provider extension point accepts only a typed Plan 08 contribution whose
application handler is callable. PR13 adds GitHub review, CI localization, and
proximity contributions through that point; absent or ineligible providers
remain typed unavailable and do not alter the PR12 reader transport. The
gateway forwards no arbitrary method/payload and owns no graph, test, feedback,
review, CI, proximity, or evidence data.

## PR13 product delivery

### Prove real host protocol behavior

- Run Claude Code initialization, document lifecycle, navigation, diagnostics,
  cancellation, shutdown, reconnect, and incompatible-daemon cases against the
  packaged bridge.
- Detect extension claims that compete for the configured language set.
  Report exact conflict evidence to Plan 27; replacement requires explicit
  lifecycle confirmation, preserves third-party configuration, and rolls back.
- Add conforming LSP hosts only after they satisfy the same portable protocol
  contract. Do not lower the
  contract to accommodate a weaker host.

### Ship OpenCode custom LSP

- Register the packaged bridge through OpenCode's documented custom LSP
  configuration and consume its local JS/TS plugin LSP events for lifecycle
  evidence; MCP and plugin feedback remain independent Plan 27 surfaces.
- Detect OpenCode's existing analyzer selection per language before launch.
  When that analyzer remains selected, configure the broker for
  TraceDecay-only graph/advisory projection without launching an upstream
  analyzer. Selecting the daemon-managed analyzer requires explicit lifecycle
  confirmation and suppresses the overlapping host analyzer; never run both.
- Prove initialization, document lifecycle, navigation, diagnostics,
  cancellation, shutdown, reconnect, conflict detection, repair, and rollback
  through the same gateway contract as Claude Code.

### Ship Cursor native diagnostics

- Reuse or ingest Cursor desktop's native analyzer evidence with provenance
  instead of running a duplicate TraceDecay-managed analyzer for that
  language.
- Publish TraceDecay-only findings through the native diagnostics adapter.
  Cursor cloud, Codex, and Kimi Code remain hook/MCP/CLI paths unless a real
  conforming editor protocol becomes available.

### Project the complete PR13 feedback result

- Publish current post-edit diagnostics and impact plus remapped GitHub, CI,
  and proximity findings with conservative severity, authorized safe URLs,
  bounded related locations, stable finding/anchor identity, and explicit
  coverage.
- Preserve full evidence through Plan 21 expansion; LSP is an editor
  projection and semantic provider, not a query, finding, task, workflow, or
  evidence store.
- Keep dirty-overlay feedback session-only for the owning client and never
  publish it as durable LSP state.

### Operate the one-root worktree subscription

- Route by exact project, repository, worktree, epoch, ref, generation, file,
  symbol, and content identity.
- Coalesce repeated affected-symbol updates while allowing conflict, epoch,
  and clear events to bypass debounce. Reserve queue capacity for clears and
  urgent state.
- On cursor gap, saturation, or restart uncertainty, clear projections whose
  currentness is not provable, request one fresh daemon snapshot, and report a
  bounded unavailable/partial outcome.

## Compatibility and resource safety

- The stdio bridge remains transport-only and store-free. Direct socket
  registration may coexist only after proving equivalent authentication,
  lifecycle, cancellation, shutdown, and reconnect behavior.
- LSP sessions use exact project admission and path containment. Invalid URIs,
  symlink escapes, device paths, denied files, stale sessions, and oversized
  input fail before analyzer or graph access.
- Unsaved text is memory-only for the authorized client and may reach only an
  explicitly configured local analyzer. A remote analyzer requires an
  authenticated configured endpoint and explicit user enablement. Environment
  inheritance is allowlisted.
- Retain hard defaults of 4 MiB per JSON-RPC frame, 2 MiB per document,
  64 pending requests per session, 128 queued requests per engine, four
  concurrent root fan-outs, and eight admitted roots. A publication remains
  bounded to 200 diagnostics/256 KiB, with bounded messages, data, and related
  locations. Truncation reports counts and reasons.
- Dirty edits debounce for 75 ms with a 250 ms maximum; save flushes
  immediately. Publication coalesces for 50 ms with a 200 ms maximum while
  preserving clears, newest identity, and current error state.
- Retain the user-facing warm budgets: bridge initialization p95 <= 250 ms;
  navigation p95 <= 100 ms and p99 <= 250 ms; diagnostics p95 <= 500 ms and
  p99 <= 1.5 s, excluding separately reported cold analyzer/indexing work.
  Accepted cancellation suppresses queued unacknowledged publication within
  50 ms. A budget miss returns partial/stale/unavailable and clears unsafe
  state; it never extends a deadline or falls back to path matching.
- Unsupported, absent, indexing, stale, cancelled, timed-out, failed, partial,
  locked, denied, and unavailable remain distinct from a successful complete
  zero-finding result. Push and pull use protocol-valid clears/errors and never
  fake a diagnostic to report provider state.
- No gateway path applies edits, refreshes Git, triggers CI, writes GitHub,
  schedules work, mutates tasks, runs integration, or continues an agent.

## Replacement and deletion

- Delete bypass LSP implementations after parity; any bounded compatibility
  name with actual independent public release evidence delegates to the daemon
  gateway or returns an actionable negotiated upgrade. Pure source-only and
  branch-era callable names are replaced in place.
- Remove reserved future fields and predeclared PR17/PR18 variants from PR13
  writer schemas. Later callable features revise the current writer shape.
  An actually independently released public protocol may negotiate its
  documented revision at the transport boundary; persisted diagnostics never
  gain an old reader and return `ResetRequired` when their shape is non-final.
- Remove duplicate architecture/ownership prose, exact source-file and
  fixture inventories, standalone worktree milestone gates, generated protocol
  matrices that restate negotiation code, and placeholder benchmark packets.
- Do not remove any supported LSP method, host surface, lifecycle behavior,
  compatibility path, hard safety bound, or unavailable-state semantic.

## Direct acceptance

- Portable protocol tests exercise initialization, capability negotiation,
  document synchronization, UTF-16 positions, request correlation,
  cancellation, progress, push/pull diagnostics, navigation, shutdown,
  reconnect, bounded framing, and typed protocol errors independently of any
  one host package.
- Real negotiated client journeys cover Claude Code through the packaged bridge
  and OpenCode through its custom LSP configuration and local plugin events.
  Both receive current authorized analyzer and TraceDecay results through the
  same gateway; OpenCode retains exactly one analyzer per language through
  install, repair, rollback, and uninstall.
- Representative Rust, Python, and TypeScript workspaces match direct upstream
  semantic/navigation results where the negotiated provider supports them,
  with deterministic graph augmentation where current evidence permits it.
- Concurrent clients with conflicting unsaved versions remain isolated; no
  overlay becomes durable or visible to another client.
- Save, close, file removal, ref change, analyzer crash/restart, cancellation,
  timeout, daemon restart, and bridge reconnect converge idempotently and
  monotonically without stale publication.
- Missing analyzers degrade only their own capabilities. Graph-backed
  operations continue truthfully and unavailable/partial state never becomes
  fabricated semantic output.
- A real client negotiates the TraceDecay experimental capability,
  receives diagnostics, impact, affected-test, and test-result envelopes, and
  expands one omitted item through Plan 21. Scope/generation/coverage,
  cancellation, stale suppression, handle expiry/replay/revocation, and
  authorization are preserved; unnegotiated versions, arbitrary
  methods/payloads, and cross-scope handles are rejected.
- Provider journeys prove GitHub review, CI localization, and proximity
  appear through the unchanged extension only when their typed application
  contributions are callable, with truthful unavailable state otherwise.
- Real GitHub, CI, proximity, post-edit impact, affected-symbol, conflict,
  and stale-epoch findings project and clear correctly; full evidence remains
  available only through authorized expansion.
- Cursor desktop native diagnostics, OpenCode custom LSP, Kimi Code
  hook/MCP/CLI, and every other fallback expose the same application semantics
  where capabilities overlap. Unsupported automatic diagnostics are explicitly
  unavailable.
- Bridge/restart checks prove no bridge/client opens a writable store, no
  hidden peer/root is enumerated, no dirty overlay reaches a durable or remote
  sink, and no LSP action mutates source or external systems.
- Ordinary Linux, macOS, and Windows CI covers the platform substrate: URI/path
  normalization, process and transport lifecycle, framing, cancellation, and
  shutdown. Selected real client/platform combinations cover native packaging
  differences without a Cartesian host-by-OS matrix.
- Portable protocol tests, real negotiated client journeys, and ordinary
  repository checks are the evidence. Exact fixture inventories, prescribed
  test names/counts, implementing-PR ownership, and placeholder benchmarks are
  not requirements.

## Later callable extensions

- **PR14:** Dashboard and Doctor call the shipped gateway engine/status and
  lifecycle evidence. They do not redefine analyzer or host repair behavior.
- **PR15:** Enable callable multi-root admission and
  `workspace/didChangeWorkspaceFolders` after Plan 16 resolves every folder.
  Preserve independent roots, overlays, epochs, generations, diagnostics,
  coverage, authorization, and hidden-root isolation. Project current Plan 37
  stack/PR capability and drift findings through the same bounded diagnostics
  or root-notification path, with standard Git/other-forge fallback when the
  optional GitHub preview is unavailable.
- **PR16:** Keep live overlays and analyzers on the enrolled workspace node.
  Route durable clean evidence through Plan 28's fenced authority; failover
  cannot spool, replicate, or silently move unsaved content.
- **PR17:** Add an optional authorized task join and ready-commit cue only
  through shipped Plan 24/32/36 application reads. LSP remains a bounded
  projection; it cannot retrieve task history, mutate work, admit execution,
  or apply integration.
- **PR18:** Plan 17 owns two public token-consumption operations plus their
  Rust/TypeScript SDK and compatibility contracts; this plan owns only
  negotiated LSP production/projection of their actions. A
  feedback/diagnostic cue produces an opaque
  `open_investigation_handoff` token that opens Plan 09's owning investigation
  surface. A ready-commit/cross-worktree/task cue produces a distinct opaque
  `open_task_handoff` token that opens the owning application surface over Plan
  24 task identity/version/context semantics.

  Each action carries one 60-second, single-use, kind/destination/session/
  project/root/cue-or-task-version/authorization/policy/local-or-PR16-authority
  bound token. The owning public application operation reauthenticates and
  reauthorizes scope, expiry, use state, authority, and current owner version
  on consumption. Wrong kind, destination, scope, version, authority, expiry,
  replay, revocation, or authorization returns Plan 17's policy-safe
  non-enumerating shape; token possession reveals no cue, finding, task, or
  resource existence.

  `window/showDocument` is used only when supported and only to open the owning
  surface. LSP never consumes a token by retrieving investigation evidence or
  task bodies itself. Tokens contain no edit, task body, source, raw
  IDs/paths/URLs/query/arguments, credential, or evidence payload. The action
  cannot run Git/native-integration dry-run or apply, resolve conflict, refresh
  Git, mutate work, invoke a tool, schedule an agent, or call
  `workspace/applyEdit`.

  **PR18 direct acceptance.** Real negotiated clients produce both action/token
  kinds and consume each through Rust and TypeScript against a local
  daemon and a PR16-enrolled remote authority. The investigation token opens
  only the owning investigation surface and the task token only the owning task
  surface, with identical application semantics/error taxonomy across local
  and remote. Wrong-scope, wrong-kind/destination, expired, replayed,
  unauthorized, revoked, partial-authority, and unavailable-authority cases
  remain non-enumerating and perform no LSP-side task retrieval, edit, work
  mutation, Git/provider action, or arbitrary invocation.

## Safety constraints retained

- Daemon-owned state, transport-only clients, exact identity, monotone
  publication, isolated overlays, bounded resources, and truthful provider
  outcomes are mandatory.
- LSP is one supported surface, not the universal transport or a second product
  authority.
- Dirty overlay content never becomes durable or remote by default.
- No edit, GitHub write, CI rerun, Git mutation, workflow admission, or
  autonomous agent action is representable through this plan.
