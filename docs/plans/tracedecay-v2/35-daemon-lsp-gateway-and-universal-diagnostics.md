# TraceDecay V2 Daemon LSP Gateway and Universal Diagnostics Plan

## Status / role

Planned across PR9, PR11–PR13, PR14, PR15, and PR16. PR9 establishes
generation-bound diagnostic records, PR11 owns analyzer policy and
configuration, PR12 ships the single-project daemon LSP gateway and typed
routing operations, PR13 supplies gateway/provider conformance evidence,
duplicate-analyzer rules, and Cursor desktop native-diagnostics adapter
behavior consumed by Plan 27 packaging, and PR14 owns dashboard consumption and the
canonical Doctor kernel/UI. PR15 replaces the
bounded single-project admission with canonical multi-root project/worktree
scope, and PR16 defines remote-node placement without exporting unsaved
workspace authority accidentally.

This plan extends, rather than replaces, the code-intelligence ownership in
[25](25-code-intelligence-indexing-crate.md), the daemon and binding rules in
[21](21-cli-mcp-tool-surface-and-output-unification.md), and the host projection
rules in [27](27-cross-host-agent-plugin-bundles.md).

## Outcome

LSP-capable agent hosts—initially Claude Code, and additional hosts only after
they pass the same gateway conformance contract—connect to TraceDecay through
one daemon LSP gateway per workspace. That gateway is an LSP 3.17 endpoint
that combines TraceDecay's generation-bound code intelligence and managed
diagnostics with language-specific semantic results delegated to explicitly
configured upstream language servers.

Hosts that do not expose a reliable full LSP surface consume the same semantic
and diagnostic application contracts through capability-specific
native-diagnostics adapters or hook/MCP/CLI paths defined by
[Plan 27](27-cross-host-agent-plugin-bundles.md), rather than degraded or
universal LSP registration. Universal here means one typed product contract
across paths, not that every host registers the same protocol.

For LSP-capable hosts, competing per-language TraceDecay and analyzer plugins
are unnecessary. TraceDecay starts, supervises, and routes to the appropriate
analyzer behind one truthful, local-first protocol boundary for those sessions.

LSP is a daemon-internal semantic-evidence provider and host protocol adapter.
It is not a second graph, query engine, durable index, edit path, policy
authority, or universal product API. It is also not the universal transport
for [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)
findings: hooks, read-only ingested GitHub review threads, CI-localization
input, and concurrent-agent proximity use their own native transports over the
same daemon/application contracts described there.
[Plan 09](09-application-crate.md) owns the one typed, transport-neutral
semantic-evidence/provider contract and canonical provider-result
identity/compatibility semantics; this plan implements analyzer-backed
providers behind that contract, owns analyzer-provider cache storage,
admission, reuse, eviction, invalidation execution, and lifecycle, and is
the architectural center for every LSP-shaped gateway decision in the V2
plan set, so other plans link back to it instead of restating this
architecture.

## Owns

- A daemon-hosted, stateful LSP 3.17 gateway and its client-session lifecycle.
- A thin stdio bridge for hosts that launch an LSP command instead of connecting
  directly to a daemon socket.
- Analyzer-provider cache storage, admission, reuse, eviction, invalidation
  execution, and lifecycle keyed by the canonical Plan 09 provider-result
  identity tuple.
- Capability negotiation, document synchronization, request routing,
  cancellation, deadlines, response ordering, and upstream analyzer lifecycle.
- Merging current upstream diagnostics with current TraceDecay-managed
  diagnostics without losing source, provenance, freshness, or severity.
- Field-level LSP projection of [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)
  advisory feedback findings for IDE Problems publication.
- Exact clean-snapshot diagnostic reuse and isolated unsaved-document overlays.
- Typed gateway requirements, finding, and engine-state schema consumed by
  [27](27-cross-host-agent-plugin-bundles.md) host plugin projection, PR13
  conformance checks, and PR14 Doctor/dashboard surfaces.
- Telemetry and direct protocol conformance for the daemon gateway and bridge.

## Does not own

- Language-specific type checking, compilation, or semantic inference already
  performed by rust-analyzer, Pyright, TypeScript Language Server, gopls, or
  another upstream engine.
- Tree-sitter grammar, symbol, occurrence, relationship, test-attribution, or
  generation identity contracts owned by Plan 25.
- Database connections or writable fallback behavior in the stdio bridge,
  host plugin, hook, MCP server, dashboard, or other client.
- A second language registry, extension table, analyzer configuration store,
  or diagnostic persistence model.
- Host plugin packaging, install/update/repair/uninstall mechanics, and
  host-adapter projection; [Plan 27](27-cross-host-agent-plugin-bundles.md)
  owns those surfaces.
- Provider-result identity/compatibility semantics; Plan 09 owns the canonical
  identity tuple.
- Configuration source/digest semantics; Plan 20 owns those fields.
- Policy decision/revision/digest semantics; Plan 06 owns those fields.
- A private project/worktree resolver or remote-authority topology; Plans
  [16](16-cross-project-repository-worktree-scope.md) and
  [28](28-remote-multi-machine-shared-brain.md) own those contracts.
- Completion, formatting, or arbitrary vendor-specific LSP methods until a
  separate product requirement and conformance gate justify them.
- Applying `rename` or any other edit-shaped LSP result. This plan surfaces
  `prepareRename`/`rename` results as read-only candidate evidence routed to
  [Plan 34](34-workspace-refactoring-and-api-migration.md); Plan 34 owns
  preview, precondition, formatting, verification, and apply authority for
  every edit that originates from LSP candidate evidence. General
  `textDocument/codeAction` remains deferred until a separate typed
  candidate-consumption operation, policy, transactional apply owner, and
  acceptance fixtures are planned.
- Publishing historical, stale, inferred, or cross-snapshot findings as if they
  were current editor diagnostics.
- Facts, memory, Git history, proof that a test executed or a change was
  delivered, authorization or privacy policy authority, workflow scheduling, or
  durable temporal truth. Those remain owned by their existing product plans
  regardless of which transport surfaced the originating request.

## Required architecture

### Semantic-evidence provider boundary

- [Plan 09](09-application-crate.md) owns one typed, transport-neutral
  semantic-evidence/provider contract and canonical provider-result
  identity/compatibility semantics. This plan implements analyzer-backed
  providers behind that contract; it does not define a second,
  gateway-private evidence shape or duplicate the identity field list.
- Every provider result conforms to the canonical Plan 09 identity tuple.
  This plan solely owns cache storage, admission, reuse, eviction,
  invalidation execution, and lifecycle keyed by that tuple so that no two
  distinct inputs can alias onto one cached result.
- Catalog, dashboard, and observability consumers depend on typed application
  results and state, never the provider port directly. This plan is the only
  component that constructs analyzer-backed provider results, but Plan 09 is
  the only component that owns the contract's type, evolution, and identity
  semantics.

### Daemon authority and transport

- `tracedecayd` owns the LSP gateway, canonical clean-snapshot diagnostic state,
  upstream analyzer supervision, and access to code-intelligence application
  operations.
- `tracedecay lsp bridge --stdio` is a framing and transport adapter only. It
  forwards LSP JSON-RPC messages to one daemon LSP session and forwards daemon
  responses and notifications back to the host. It opens no project or profile
  database, runs no analyzer, and contains no routing or merge policy.
- The stdio bridge is the baseline Claude Code integration because it matches
  the normal plugin command lifecycle. Direct socket registration is optional
  and ships only after native-host tests prove equivalent initialization,
  cancellation, shutdown, authentication, and reconnect behavior.
- The daemon exposes a versioned typed session operation for the bridge rather
  than tunneling an unauthenticated arbitrary local socket. Protocol, catalog,
  project, workspace, and client revisions are negotiated before document
  content is accepted.
- A missing, incompatible, denied, or unhealthy daemon fails closed with one
  actionable startup failure. The bridge never starts an embedded TraceDecay
  database or private analyzer fallback.

### Session and workspace model

- Every client connection receives an isolated LSP session identity tied to an
  authorized repository, checkout/worktree, workspace folder set, ref, and
  clean source generation.
- Before PR15, PR12 admits one explicitly resolved registered project/worktree
  root. Multiple workspace folders are explicitly unavailable; the gateway
  cannot select CWD, the first folder, or an active checkout as a substitute.
- PR15 resolves every workspace folder through the canonical scope service.
  Multi-root admission succeeds with explicit per-folder scope and coverage or
  fails with bounded ambiguity/unavailability; no LSP-private resolver exists.
- Open-document state is keyed by client session, canonical file identity,
  document version, content digest, language descriptor revision, and analyzer
  configuration revision.
- Unsaved overlays are ephemeral and isolated per client. Two clients editing
  the same file cannot share document versions, overwrite each other's
  analyzer state, or publish one overlay's diagnostics to the other.
- Clean files may reuse daemon-managed analyzer sessions and diagnostic
  snapshots only when workspace, source generation, content, analyzer binary,
  initialization options, settings, and policy identities all match.
- Dirty-overlay diagnostics are never sealed into a clean code-intelligence
  generation. They may become durable only after capture observes the saved
  content and the normal sanitized generation pipeline verifies the same
  content identity.
- Workspace-folder additions, removals, ref changes, configuration changes,
  generation publication, and client shutdown invalidate only the state whose
  identity changed.

### Upstream analyzer broker

- The existing diagnostics LSP client and broker evolve into a daemon-owned
  upstream router rather than a second host-facing server implementation.
- Static analyzer-routing facts come only from Plan 25's canonical language
  descriptors: extension mapping, language ID, root markers, diagnostic mode,
  installation guidance, and capability expectations.
- Dynamic execution configuration comes only from Plan 20: executable
  reference, arguments, initialization options, settings, environment
  allowlist, privacy class, limits, restart policy, and per-language
  enablement. Eligibility and routing decisions come only from Plan 06. The
  broker composes typed runtime snapshots from those owners at admission time;
  it does not persist a third combined registry or duplicate config fields,
  grants, or policy digests.
- Analyzer commands cannot be supplied by an untrusted LSP request. They must
  resolve through authorized configuration, executable-path validation, and
  policy before process creation.
- The broker keys upstream processes by compatible workspace, language,
  analyzer, configuration, and overlay-isolation requirements. It shares safe
  clean state but creates isolated sessions where an analyzer cannot safely
  multiplex conflicting document overlays.
- Initialization waits for the upstream server's declared readiness and
  workspace loading behavior. Notifications are matched independently from
  request responses; an unrelated notification can never satisfy a pending
  JSON-RPC request.
- Crashes, malformed frames, oversized messages, stderr floods, startup
  failures, timeouts, and restart exhaustion produce stable engine state and
  Doctor evidence without terminating unrelated languages or client sessions.
- Backoff and restart limits are bounded. Cancellation propagates to the
  upstream request where supported and always stops downstream result
  publication for the cancelled host request.

### Capability negotiation and routing

- The gateway implements the standard LSP lifecycle and document synchronization
  methods required by Claude Code and other supported hosts.
- The supported semantic capability set is: `textDocument/declaration`,
  `textDocument/definition`, `textDocument/typeDefinition`,
  `textDocument/implementation`, `textDocument/references`,
  `textDocument/hover`, `textDocument/signatureHelp`,
  `textDocument/documentSymbol`, `workspace/symbol`, the standard prepare,
  incoming, and outgoing call-hierarchy methods, the standard prepare,
  supertypes, and subtypes type-hierarchy methods, `textDocument/diagnostic`
  alongside `publishDiagnostics`, and `textDocument/prepareRename` and
  `textDocument/rename`. General `textDocument/codeAction` remains deferred
  until a separate typed candidate-consumption operation, policy,
  transactional apply owner, and acceptance fixtures are planned.
- `prepareRename` and `rename` results are read-only candidate evidence routed
  to [Plan 34](34-workspace-refactoring-and-api-migration.md). The gateway
  never calls `workspace/applyEdit`, executes an opaque server command, or
  writes a file from these results; Plan 34 is the sole path that can convert
  a candidate into a canonical preview/manifest and `EditTransaction`.
- A method outside this set, or a request the active analyzer declares
  unsupported, returns an explicit typed capability-unavailable outcome. The
  gateway never guesses a fallback result or synthesizes a plausible-looking
  answer for a method it cannot truthfully answer.
- Upstream analyzer results are authoritative for language-specific type
  semantics. TraceDecay may add graph results only when they carry exact
  generation, file, symbol, and span evidence. It never upgrades a heuristic
  relationship into an analyzer fact.
- Results are normalized to canonical paths and UTF-16 LSP positions, deduped
  without erasing distinct provenance, deterministically ordered, bounded, and
  returned only for the requesting session's workspace and document version.
- If an upstream engine is unavailable, the gateway uses only operations that
  the active TraceDecay generation can answer truthfully. Unsupported,
  absent, indexing, stale, cancelled, timed-out, failed, or partial providers
  return typed unavailable or partial outcomes and never collapse to a clean
  empty result; the gateway never fabricates type information.
- Capability advertisement is derived from guaranteed gateway behavior plus
  negotiated upstream capabilities. Dynamic registration is used where the
  host supports it. Static capabilities are advertised only when every routed
  path has a valid fallback with the same semantic contract.
- Vendor-specific methods are not blindly proxied. Adding one requires a typed
  catalog entry, policy classification, bounded schema, direct tests, and
  explicit host capability projection.

### Merge authority

- Active-document type semantics may come from the admitted analyzer for that
  document version. The TraceDecay graph remains authoritative for stable
  symbol identity, generations, bounded traversal, history, cross-project
  evidence, and test attribution; an analyzer result never overrides those
  facts.
- Empty analyzer output is valid only for a supported, successfully completed
  request with complete coverage and no matches. When an analyzer is absent,
  still indexing, stale, cancelled, timed out, failed, or partial, the gateway
  reports that state explicitly instead of returning a clean empty result, and
  graph-backed operations keep answering from
  [Plan 25](25-code-intelligence-indexing-crate.md) evidence with their own
  freshness and coverage.
- Impact and affected-test results combine LSP-resolved references and call
  dispatch with graph, Git, and test-execution evidence. LSP evidence may
  contribute candidate sites; it never proves that a test executed or that a
  change was delivered.

## Universal managed diagnostics

### Canonical diagnostic identity

Every durable diagnostic is bound to:

- repository, checkout/worktree, ref, source revision, and immutable
  code-intelligence generation;
- canonical file identity, content digest, range encoding, and enclosing symbol
  occurrence when exact attachment is possible;
- producer kind and identity, analyzer and configuration revisions, diagnostic
  code, severity, message digest, and sanitization receipt;
- evidence class, collection time, freshness, and supersession or clearing
  evidence.

The display message remains sanitized product data. Raw analyzer stderr,
environment values, command lines, unsanitized source, and private host payloads
are not diagnostic messages or durable provenance.

### Diagnostic sources

The gateway may publish:

- current upstream compiler or language-server diagnostics;
- current TraceDecay structural, graph-integrity, policy, code-health, and
  generation-consistency diagnostics that have editor-meaningful file ranges;
- current application diagnostics produced by another cataloged, authorized
  analyzer whose evidence satisfies the same identity contract.

Runtime, storage, migration, configuration, session, or daemon-health findings
without a truthful source range remain Doctor or application findings. They are
not forced into fake editor positions.

### Merge and publication semantics

- Upstream push and pull diagnostics normalize into one canonical update
  stream before publication.
- Diagnostic identity includes producer provenance. Identical findings from
  the same logical producer and revision collapse; findings from distinct
  producers remain distinct unless a cataloged equivalence rule proves they
  represent the same evidence.
- TraceDecay does not raise severity merely because several producers agree.
  It preserves source severity and can expose agreement as provenance through
  other APIs.
- `publishDiagnostics` contains only diagnostics current for that client
  document version or exact clean generation. A newer version clears or
  supersedes the prior publication deterministically.
- Stale and historical diagnostics remain queryable through TraceDecay
  application APIs but are excluded from active LSP publication.
- Partial coverage is never represented as a clean result. Engine status,
  missing analyzers, timed-out languages, unsupported files, and dropped
  updates remain visible through typed status, Doctor, and observability
  surfaces.
- Repeated reads of an unchanged clean generation reuse the managed snapshot
  without rerunning an analyzer. Cache reuse is observable and is invalidated
  by every identity input listed above.

### Plan 37 feedback finding LSP projection

- Ingested PR review comments, CI-localization findings, and proximity
  warnings may surface through IDE Problems without becoming analyzer facts.
  They remain advisory Plan 09/Plan 37 findings projected through this gateway,
  not upstream compiler or language-server evidence.
- Each published `Diagnostic` includes: exact UTF-16 range and current
  enclosing-function mapping when available; `source` naming the producer;
  stable `code`; `codeDescription.href` to the original review or CI URL only
  when authorized; `data` carrying stable finding ID plus Plan 13
  `RetrievalAnchorId`, lifecycle/coverage state, and no full payload;
  `relatedInformation` with typed locations and bounded messages where the
  finding references additional sites.
- Severity is conservative: preserve upstream/analyzer severity where scored;
  default to Information for unscored review comments and proximity warnings.
  TraceDecay does not raise severity because several producers agree.
- Publication is bounded. Full text, diffs, and thread bodies expand only
  through authorized TraceDecay read operations
  ([Plan 21](21-cli-mcp-tool-surface-and-output-unification.md)
  `feedback_get`/`feedback_expand` and Plan 13 anchor resolution), never as
  hidden LSP payload.
- Clearing is deterministic: resolution, deletion, head SHA drift, content or
  generation change, or supersession removes or republishes the prior
  diagnostic exactly once for that client document version.
- Dirty-overlay feedback findings remain session-only for the authorized
  overlay owner and are never published as durable LSP diagnostics.

## Host plugin projection and coexistence

- [Plan 27](27-cross-host-agent-plugin-bundles.md) owns host plugin packaging,
  install/update/repair/uninstall mechanics, and host-adapter projection from
  the canonical host-integration catalog. This plan owns gateway/provider
  behavior, normalization, and duplicate-analyzer policy.
- Plan 27's canonical host-integration catalog declares one
  `tracedecay-lsp` capability and, for LSP-capable hosts that pass gateway
  conformance (Claude Code first; additional hosts only after the same gate),
  projects plugin configuration, install metadata, compatibility range, and
  conformant LSP registration. Non-LSP hosts use Plan 27 capability-specific
  paths instead of equivalent LSP registration.
- Claude Code's `extensionToLanguage` map is projected from Plan 25's canonical
  language descriptors and Plan 20's bounded, non-sensitive per-language
  registration selection. Generated host artifacts contain no independent
  extension or language-ID authority and never copy analyzer executable
  references, arguments, initialization options, settings, or environment.
- The plugin launches only the stdio bridge and points it at the selected
  daemon/project. It does not package upstream analyzers or copied TraceDecay
  product logic.
- Duplicate-analyzer detection records enabled host LSP plugins that claim any
  projected extension. Because Claude Code selects the first registered server
  for an extension, TraceDecay emits typed finding/conformance state for the
  exact conflict and requires explicit user confirmation before disabling or
  replacing a conflicting third-party registration. [Plan 27](27-cross-host-agent-plugin-bundles.md)
  owns install/update/repair/uninstall mechanics that consume that state.
- Users may select a bounded language subset. Universal means one gateway and
  contract across configured languages, not silently claiming every possible
  file extension.
- Automatic editor diagnostics are unavailable only when a host supports
  neither conformant LSP registration nor a native-diagnostics adapter; such
  hosts retain CLI, MCP, hook, and daemon API access rather than emulating
  editor diagnostics. Cursor desktop's native-diagnostics adapter is an
  available automatic diagnostics path, not unavailable.
- Host behavior is capability-specific rather than lowest-common-denominator.
  Claude Code registers the full TraceDecay LSP gateway through the stdio
  bridge where the host supports LSP registration. Cursor desktop reuses or
  ingests the native editor's own analyzer/diagnostic output where the host
  exposes it, avoids running a duplicate TraceDecay-managed analyzer for the
  same language, and submits provenance-bearing native evidence through
  application/provider admission instead of constructing a second provider
  result contract; it publishes only TraceDecay-only findings through a native
  diagnostics adapter rather than a competing LSP registration. Cursor cloud
  and Codex do not expose a reliable full LSP host surface, so they receive
  equivalent diagnostics and context through hooks, MCP, and CLI operations
  over the same application contracts rather than a degraded LSP session.
  Every difference is a typed, tested capability outcome reported through
  [Plan 27](27-cross-host-agent-plugin-bundles.md), not an assumption baked
  into client code.

## Policy, privacy, and resource safety

- LSP sessions use the same project admission, scope, authorization, privacy,
  and path-containment policies as other daemon clients.
- Requests for files outside authorized workspace roots, symlink escapes,
  device paths, invalid URIs, oversized documents, or stale session identities
  fail before analyzer or graph access.
- Unsaved document text is held only for the active session and sent only to
  explicitly authorized local analyzers. It is not persisted, logged, embedded,
  exported, or captured as a TraceDecay observation, and it is not sent to a
  remote analyzer by default.
- Remote or networked analyzers require an explicit policy capability and
  privacy disclosure. Analyzer environment inheritance is allowlisted rather
  than copied wholesale from the daemon or bridge.
- Per-session and per-engine limits bound document bytes, message bytes,
  pending requests, workspace scans, analyzer processes, restart rate, memory,
  CPU, queue depth, and diagnostic count.
- Backpressure is explicit. The daemon may coalesce superseded document changes
  and diagnostic updates, but it cannot reorder versions, silently discard a
  current error state, or acknowledge work it did not accept.

## Observability and Doctor

- Stable metrics cover session count, active languages, request method and
  outcome, latency, cancellation, queueing, analyzer startup, restarts, cache
  reuse, diagnostic additions/clears, partial coverage, dropped updates,
  provider conflicts, host delivery path, and bridge reconnects without
  recording source text, symbols, paths, or messages.
- Trace identifiers connect bridge, gateway, upstream analyzer, diagnostic
  projection, and host publication events while preserving client isolation.
- Plan 35 defines the gateway-specific finding and engine-state schema for
  daemon reachability, protocol/catalog skew, host registration, extension
  conflicts, analyzer capabilities and availability, coverage versus a genuine
  zero-finding result, indexing/degraded analyzer state, executable safety,
  workspace-root resolution, capability negotiation, overlay freshness, engine
  crashes, cache reuse and freshness, provider conflicts, host delivery path,
  and privacy-policy blockers, all without source, path, or message leakage.
- PR13 conformance checks and Plan 27 lifecycle mechanics consume that schema
  for host registration/protocol conformance only. PR14 owns the canonical Doctor kernel/UI, dashboard
  consumption/migration, and remediation orchestration surfaces built on the
  same schema; PR14 does not redefine Plan 27 repair/install/update/uninstall
  mechanics.
- Doctor remains read-only. Canonical analyzer-configuration mutation
  operations are owned exclusively by
  [Plan 20](20-configuration-control-plane.md). Host lifecycle
  mechanics—install/update/repair/uninstall, backup/restore, receipts, and
  rollback—are owned exclusively by Plan 27. PR14 Doctor remediation surfaces
  orchestrate confirmed operations without redefining either owner's mutations;
  plugin replacement and registration changes remain Plan 27 lifecycle
  operations.

## Delivery slices

### PR9: diagnostic and generation contracts

- Extend the canonical code-intelligence model with generation-bound diagnostic
  identity, evidence, freshness, clearing, and enclosing-occurrence attachment.
- Convert existing compiler and LSP diagnostic snapshots into the canonical
  model through application/store ports.
- Prove that dirty overlays cannot enter clean generations and that stale
  findings cannot cross snapshots.

### PR11: configuration and policy

- Consume and enforce [Plan 20](20-configuration-control-plane.md) canonical
  configuration fields/digest and [Plan 06](06-policy-crate.md)
  decision/revision/digest at analyzer admission. Bind execution grants to
  [Plan 25](25-code-intelligence-indexing-crate.md) static language descriptors
  (extension mapping, language ID, root markers, diagnostic mode, capability
  expectations). Plan 35 composes typed runtime snapshots from those owners at
  admission time; it does not persist a third combined registry or define
  duplicate configuration or policy fields.
- Expose typed engine and coverage state to application, PR13 conformance
  consumers and PR14 Doctor/dashboard consumers, and observability surfaces.

### PR12: daemon gateway

- Ship the daemon LSP session API, stdio bridge, upstream router, capability
  negotiation, managed diagnostic merge, and cancellation/backpressure behavior.
- Core PR12 gate: `textDocument/diagnostic` and `publishDiagnostics`,
  `textDocument/declaration`, `textDocument/definition`,
  `textDocument/typeDefinition`, `textDocument/implementation`,
  `textDocument/references`, `textDocument/hover`,
  `textDocument/documentSymbol`, `workspace/symbol`, and the standard prepare,
  incoming, and outgoing call-hierarchy methods.
- Later PR12 sub-slice gate: `textDocument/signatureHelp`, the standard prepare,
  supertypes, and subtypes type-hierarchy methods, and
  `textDocument/prepareRename` and `textDocument/rename` as read-only candidate
  evidence routed to [Plan 34](34-workspace-refactoring-and-api-migration.md).
  General `textDocument/codeAction` remains deferred.
- Install the canonical daemon gateway and disable or mark bypass paths by
  default after parity. Dashboard consumption and migration remain owned by
  PR14; any bounded compatibility path names its PR19 deletion condition and
  cannot remain a second authority.
- Prove that the bridge and every other LSP client process cannot open a
  writable TraceDecay store.

### PR13: host integration

- Supply gateway/provider behavior, duplicate-analyzer rules, and typed
  finding/conformance state consumed by [Plan 27](27-cross-host-agent-plugin-bundles.md).
  Plan 27 exclusively owns host plugin packaging,
  install/update/repair/uninstall mechanics, and host-adapter projection from
  the canonical host-integration catalog.
- Implement Cursor desktop native-diagnostics adapter behavior and
  duplicate-analyzer policy: reuse or ingest the editor's analyzer/diagnostic
  output, avoid a duplicate TraceDecay-managed analyzer for the same language,
  submit provenance-bearing native evidence through application/provider
  admission, and publish only TraceDecay-only findings through the native
  adapter rather than competing LSP registration. Cursor cloud and Codex remain
  hook/MCP/CLI capability paths.
- Expose compatibility pinning and host install/registration/protocol conformance
  evidence through Plan 35's gateway finding/state schema for Plan 27 and PR13
  conformance checks. Add real Claude Code protocol fixtures.
- Add conformant LSP capability projections for additional LSP-capable hosts
  only where their native LSP extension mechanism passes the same conformance
  contract.

### PR15: multi-root canonical scope

- Replace PR12's bounded single-project admission with Plan 16's canonical
  repository/project/worktree/ref resolver for every workspace folder.
- Bind documents, analyzer sessions, graph generations, diagnostics, and
  coverage to the resolved owning folder without CWD, first-folder, or
  active-checkout fallback.
- Prove same-name repositories, nested roots, linked worktrees, symlinks,
  ambiguous folders, denied neighbors, and partial multi-root coverage remain
  explicit and isolated.

### PR16: remote-node placement

- Keep the LSP gateway, unsaved overlays, and local analyzer processes in the
  enrolled daemon on the node that owns the live workspace.
- Route clean-generation reads and durable sanitized diagnostic commands to a
  remote shard authority only through Plan 28's authenticated API and fencing.
- Never place unsaved document content in the offline event spool, verified read
  cache, replica, trace, or failover payload. Sending it to a remote analyzer
  requires the explicit capability and privacy disclosure defined by this plan.
- Authority loss returns partial or unavailable coverage; it cannot create a
  local database writer, silently move an overlay, or publish cached diagnostics
  as current.

## Acceptance

- A real Claude Code session registers only the TraceDecay LSP plugin for the
  configured languages and receives Rust, Python, and TypeScript diagnostics
  through one daemon gateway.
- Declaration, definition, type definition, implementation, references,
  hover, signature help, document/workspace symbols, call hierarchy, and type
  hierarchy match direct upstream results on representative projects, with
  deterministic exact TraceDecay graph augmentation where available.
- `prepareRename` and `rename` return read-only candidate evidence routed to
  Plan 34 that matches direct upstream results; no gateway path applies a
  workspace edit or server command from that evidence.
- Analyzer notifications cannot be mistaken for request responses; startup
  waits for readiness, and cross-file operations pass after workspace indexing.
- Identical clean generations reuse diagnostics without analyzer work.
  Content, analyzer, settings, registry, policy, or generation changes
  invalidate exactly the affected cache entries.
- Concurrent clients with conflicting unsaved versions receive isolated,
  version-correct diagnostics and navigation. Neither overlay becomes durable
  or visible to the other client.
- Save, close, rename, delete, ref switch, workspace-folder change, analyzer
  crash, restart, cancellation, timeout, daemon restart, and bridge reconnect
  fixtures clear or republish diagnostics exactly once.
- Missing analyzers degrade only their languages. TraceDecay graph-backed
  operations remain truthful, engine coverage remains visible, and no fallback
  invents semantic or type information.
- Unsupported files, stale generations, partial indexing, redacted content,
  denied scope, symlink escape, oversized payload, malformed JSON-RPC, and
  protocol skew return bounded stable failures without leaking content.
- Duplicate-analyzer and extension-conflict fixtures emit typed
  finding/conformance state; Plan 27 lifecycle mechanics consume that state for
  confirmation, preservation, and rollback.
- PR15 multi-root fixtures preserve exact per-folder project/worktree/generation
  scope and reject CWD, first-folder, active-checkout, symlink, or ambiguous
  fallback.
- PR16 remote fixtures keep dirty overlays and analyzer processes node-local,
  fence durable clean-diagnostic publication through the shard authority, and
  never spool or cache unsaved source.
- Plan 37 feedback-projection fixtures cover ingested PR comments, CI findings,
  and proximity warnings surfacing through Problems with conservative severity,
  stable finding/anchor IDs in `data`, bounded `relatedInformation`, authorized
  `codeDescription.href`, deterministic clear/remap on head/content/generation
  change, truncation without hidden payload, and dirty-overlay non-durability.
- Linux, macOS, and Windows fixtures cover URI normalization, UTF-16 positions,
  process lifecycle, command discovery, socket/stdio behavior, path safety, and
  shutdown.
- Stock Cargo checks and focused protocol, daemon, diagnostics, Doctor, and host
  conformance tests pass with all features. PR12 records gateway latency and
  resource baselines for Plan 33's end-to-end optimization.

## Rejected designs

- **Diagnostics-only universal server:** rejected as the final product because
  claiming an extension would displace the native analyzer while losing hover,
  definition, references, implementations, symbols, and call hierarchy.
- **TraceDecay-native universal type analyzer:** rejected because syntax graphs
  do not replace language-specific type systems, build configuration, macro
  expansion, dependency resolution, or compiler semantics.
- **Host hooks or MCP as automatic diagnostics:** retained as complementary
  surfaces but rejected as the LSP replacement because they do not implement
  the host's document lifecycle or automatic post-edit diagnostic channel.
- **One independent LSP server per language inside TraceDecay:** rejected
  because it recreates competing registration, duplicate lifecycle, and
  duplicate diagnostic state instead of one daemon gateway.
- **Blind JSON-RPC proxy:** rejected because it bypasses typed capability,
  policy, privacy, bounds, provenance, and conformance requirements.
