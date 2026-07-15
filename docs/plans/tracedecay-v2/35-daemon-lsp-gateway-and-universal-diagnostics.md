# TraceDecay V2 Daemon LSP Gateway and Universal Diagnostics Plan

## Status / role

Planned across PR9, PR11–PR13, PR15, and PR16. PR9 establishes
generation-bound diagnostic records, PR11 owns analyzer policy and
configuration, PR12 ships the single-project daemon LSP gateway and typed
routing operations, and PR13 ships Claude Code and other host-native
registration, lifecycle, Doctor, and conformance behavior. PR15 replaces the
bounded single-project admission with canonical multi-root project/worktree
scope, and PR16 defines remote-node placement without exporting unsaved
workspace authority accidentally.

This plan extends, rather than replaces, the code-intelligence ownership in
[25](25-code-intelligence-indexing-crate.md), the daemon and binding rules in
[21](21-cli-mcp-tool-surface-and-output-unification.md), and the host projection
rules in [27](27-cross-host-agent-plugin-bundles.md).

## Outcome

TraceDecay is the one Language Server Protocol endpoint registered with an
agent host for a workspace. The daemon serves an LSP 3.17 gateway that combines
TraceDecay's generation-bound code intelligence and managed diagnostics with
language-specific semantic results delegated to explicitly configured upstream
language servers.

Hosts no longer need competing per-language TraceDecay and analyzer plugins.
They connect to TraceDecay, while TraceDecay starts, supervises, and routes to
the appropriate analyzer behind one truthful, local-first protocol boundary.

## Owns

- A daemon-hosted, stateful LSP 3.17 gateway and its client-session lifecycle.
- A thin stdio bridge for hosts that launch an LSP command instead of connecting
  directly to a daemon socket.
- Capability negotiation, document synchronization, request routing,
  cancellation, deadlines, response ordering, and upstream analyzer lifecycle.
- Merging current upstream diagnostics with current TraceDecay-managed
  diagnostics without losing source, provenance, freshness, or severity.
- Exact clean-snapshot diagnostic reuse and isolated unsaved-document overlays.
- Typed gateway requirements and status consumed by Plan 27's universal host
  plugin projection, conflict checks, and Doctor findings.
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
  diagnostic persistence model, or host plugin manifest.
- A private project/worktree resolver or remote-authority topology; Plans
  [16](16-cross-project-repository-worktree-scope.md) and
  [28](28-remote-multi-machine-shared-brain.md) own those contracts.
- Completion, formatting, rename, code actions, or arbitrary vendor-specific
  LSP methods until a separate product requirement and conformance gate justify
  them.
- Publishing historical, stale, inferred, or cross-snapshot findings as if they
  were current editor diagnostics.

## Required architecture

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
  enablement. The broker composes typed snapshots from those two owners at
  runtime; it does not persist a third combined registry.
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
- The first shipped semantic request set is:
  `textDocument/definition`, `textDocument/references`,
  `textDocument/hover`, `textDocument/documentSymbol`,
  `workspace/symbol`, `textDocument/implementation`, and the standard prepare,
  incoming, and outgoing call-hierarchy methods.
- Upstream analyzer results are authoritative for language-specific type
  semantics. TraceDecay may add graph results only when they carry exact
  generation, file, symbol, and span evidence. It never upgrades a heuristic
  relationship into an analyzer fact.
- Results are normalized to canonical paths and UTF-16 LSP positions, deduped
  without erasing distinct provenance, deterministically ordered, bounded, and
  returned only for the requesting session's workspace and document version.
- If an upstream engine is unavailable, the gateway uses only operations that
  the active TraceDecay generation can answer truthfully. Unsupported or
  unknown results remain empty or unavailable according to the LSP method
  contract; the gateway never fabricates type information.
- Capability advertisement is derived from guaranteed gateway behavior plus
  negotiated upstream capabilities. Dynamic registration is used where the
  host supports it. Static capabilities are advertised only when every routed
  path has a valid fallback with the same semantic contract.
- Vendor-specific methods are not blindly proxied. Adding one requires a typed
  catalog entry, policy classification, bounded schema, direct tests, and
  explicit host capability projection.

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

## Host plugin projection and coexistence

- Plan 27's canonical host-integration catalog declares one
  `tracedecay-lsp` capability and projects the Claude Code plugin configuration,
  install metadata, compatibility range, and equivalent supported-host
  registration.
- Claude Code's `extensionToLanguage` map is projected from Plan 25's canonical
  language descriptors and Plan 20's bounded, non-sensitive per-language
  registration selection. Generated host artifacts contain no independent
  extension or language-ID authority and never copy analyzer executable
  references, arguments, initialization options, settings, or environment.
- The plugin launches only the stdio bridge and points it at the selected
  daemon/project. It does not package upstream analyzers or copied TraceDecay
  product logic.
- Installation discovers enabled host LSP plugins that claim any projected
  extension. Because Claude Code selects the first registered server for an
  extension, TraceDecay reports the exact conflict and requires explicit user
  confirmation before disabling or replacing a conflicting third-party
  registration.
- Install, update, repair, and uninstall preserve unrelated host configuration,
  back up affected TraceDecay-owned registration, and never remove a
  third-party analyzer or plugin without a separately confirmed operation.
- Users may select a bounded language subset. Universal means one gateway and
  contract across configured languages, not silently claiming every possible
  file extension.
- Hosts that cannot consume LSP retain CLI, MCP, hook, and daemon API access;
  capability reporting marks automatic editor diagnostics unavailable rather
  than emulating them.

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
  reuse, diagnostic additions/clears, partial coverage, dropped updates, and
  bridge reconnects without recording source text, symbols, paths, or messages.
- Trace identifiers connect bridge, gateway, upstream analyzer, diagnostic
  projection, and host publication events while preserving client isolation.
- Doctor reports daemon reachability, protocol/catalog skew, host registration,
  extension conflicts, analyzer availability, executable safety, workspace-root
  resolution, capability negotiation, engine crashes, cache freshness, and
  privacy-policy blockers.
- Doctor is read-only. Repair, analyzer configuration, plugin replacement, and
  registration mutation are separate confirmed operations with receipts and
  rollback.

## Delivery slices

### PR9: diagnostic and generation contracts

- Extend the canonical code-intelligence model with generation-bound diagnostic
  identity, evidence, freshness, clearing, and enclosing-occurrence attachment.
- Convert existing compiler and LSP diagnostic snapshots into the canonical
  model through application/store ports.
- Prove that dirty overlays cannot enter clean generations and that stale
  findings cannot cross snapshots.

### PR11: configuration and policy

- Move analyzer definitions and language routing inputs under the canonical
  language registry and configuration control plane.
- Define analyzer execution grants, environment policy, limits, privacy class,
  remote-analyzer prohibition by default, and per-language enablement.
- Expose typed engine and coverage state to application, Doctor, and
  observability consumers.

### PR12: daemon gateway

- Ship the daemon LSP session API, stdio bridge, upstream router, capability
  negotiation, supported semantic methods, managed diagnostic merge, and
  cancellation/backpressure behavior.
- Route new production host and dashboard behavior through the daemon gateway
  and disable bypass paths by default after parity. Any bounded compatibility
  path names its PR19 deletion condition and cannot remain a second authority.
- Prove that the bridge and every other LSP client process cannot open a
  writable TraceDecay store.

### PR13: host integration

- Project and package the universal Claude Code plugin from the canonical
  language and host-integration catalogs.
- Add conflict-aware install/update/repair/uninstall, compatibility pinning,
  Doctor checks, and real Claude Code protocol fixtures.
- Add equivalent capability projections for other hosts only where their native
  LSP extension mechanism passes the same conformance contract.

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
- Definition, references, hover, document/workspace symbols, implementation,
  and call hierarchy match direct upstream results on representative projects,
  with deterministic exact TraceDecay graph augmentation where available.
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
- Installation detects competing extension claims, requires confirmation for
  replacement, preserves unrelated configuration, and rolls back interrupted
  mutations.
- PR15 multi-root fixtures preserve exact per-folder project/worktree/generation
  scope and reject CWD, first-folder, active-checkout, symlink, or ambiguous
  fallback.
- PR16 remote fixtures keep dirty overlays and analyzer processes node-local,
  fence durable clean-diagnostic publication through the shard authority, and
  never spool or cache unsaved source.
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
