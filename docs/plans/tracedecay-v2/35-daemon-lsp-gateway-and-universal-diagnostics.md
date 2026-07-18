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
workspace authority accidentally. PR17 may add opaque TaskId join keys to
advisory editor projection; it does not add task authority or make LSP a task
retrieval transport. PR18, through
[Plan 17](17-official-public-api-and-sdks.md), freezes any public command,
route, MCP-tool, or SDK spelling used to open an investigation or task. Internal
Rust action/type identifiers in this plan are non-serialized implementation
placeholders, not public names or `executeCommandProvider.commands` values.

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
  advisory feedback findings for IDE Problems publication. When a finding is
  task-linked, the projection carries the opaque
  [Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md) `TaskId`
  and Plan 13 anchor IDs only as authorized join keys; LSP never stores task
  evidence or becomes a task-retrieval transport.
- Exact clean-snapshot diagnostic reuse and isolated unsaved-document overlays.
- Typed gateway requirements, projection-only `LspFindingProjectionV1`/`V2`,
  and engine-state schema consumed by
  [27](27-cross-host-agent-plugin-bundles.md) host plugin projection, PR13
  conformance checks, and PR14 Doctor/dashboard surfaces.
- Ephemeral delivery metadata needed to clear, deduplicate, authorize, and
  hand off a cue. The gateway and bridge may retain session/document/revision
  keys, finding/anchor join IDs, optional authorized `TaskId`, and opaque
  short-lived handoff tokens; they never persist canonical findings, task
  records, evidence, histories, payloads, pages, or expansion results.
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
- Applying `rename` or any other edit-shaped LSP result. Both rename methods
  remain unadvertised until [Plan 34](34-workspace-refactoring-and-api-migration.md)
  owns a typed candidate/preview, preconditions, formatting, verification,
  transactional apply, and host interception that prevents raw
  `WorkspaceEdit` application. General `textDocument/codeAction` remains
  unavailable except for the separately gated PR18 handoff-only action below.
- Publishing historical, stale, inferred, or cross-snapshot findings as if they
  were current editor diagnostics.
- Canonical finding/evidence storage, full evidence packets, chronology,
  synthesis revisions, task navigation, task-to-evidence relations, planner
  controls, leases, attempts, receipts, workflow control, or agent execution.
  Plan 09 owns the transport-neutral result and handoff contract, Plan 13 owns
  evidence anchors, Plan 24 owns task identity/navigation/planning, Plan 32 owns
  runtime execution, Plan 37 owns feedback finding architecture/lifecycle, and
  Plan 21 owns CLI/MCP/HTTP/LSP bindings and rendering.
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

### Protocol lifecycle and publication

- Formalize initialize/negotiation, initialized, synchronization, request,
  cancellation, shutdown, exit, reconnect, and stale-revision states.
  Cancellation guarantees suppression of stale downstream publication and
  only best-effort upstream execution stop.
- Publish diagnostics and semantic state idempotently and monotonically by
  session, document, and generation epoch. Duplicate or replayed updates
  converge, stale updates cannot overwrite newer state, and reconnect or
  restart may redeliver current state. Track produced, queued,
  bridge-acknowledged, superseded, and unknown-delivery states without claiming
  exactly-once network delivery.
- A bounded session TTL deterministically releases overlays, pending requests,
  and analyzer references without changing durable clean-generation evidence.

### Session and workspace model

- Every client connection receives an isolated LSP session identity tied to an
  authorized repository, checkout/worktree, workspace folder set, ref, and
  clean source generation.
- Before PR15, PR12 admits one explicitly resolved registered project/worktree
  root. Multiple workspace folders are explicitly unavailable; the gateway
  cannot select CWD, the first folder, or an active checkout as a substitute.
- PR15 resolves every workspace folder through the canonical scope service.
  Each request pins one canonical workspace-set revision and per-folder
  snapshot vector. The result names every admitted, denied, ambiguous, moved,
  removed, and unavailable root without exposing the identity or count of roots
  the caller cannot see. Folder-set transitions cancel work for removed roots,
  clear their diagnostics, preserve unaffected roots, and return explicit
  per-root coverage; they never report an atomic clean result when any admitted
  root is partial.
  Plan 16 resolves one owner or ambiguity for each document; no longest-prefix,
  CWD, first-folder, or active-checkout fallback exists. Workspace-wide fan-out
  merges through Plan 05 with per-root coverage.
- Every projected finding and handoff pins its canonical workspace-folder,
  project, repository, worktree, ref, source-generation, document, and content
  identities. A task spanning several roots does not erase the cue's source
  root. PR15 resolves each related root independently; ambiguous, moved,
  removed, denied, or partial roots return typed outcomes rather than being
  rebound to another folder.
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

- PR12's server-capability matrix is exact:
  `positionEncoding = "utf-16"`; `textDocumentSync.openClose = true`,
  `change = Incremental`, and `save = true`; document pull with
  `diagnosticProvider.interFileDependencies = true`,
  `workspaceDiagnostics = false`, full/unchanged reports, and
  generation-bound `resultId`; the enumerated semantic provider keys below;
  and no workspace-folder, rename, code-action, or execute-command capability.
- The PR12 push client prerequisites are
  `general.positionEncodings` containing UTF-16 or omitted (which LSP 3.17
  defines as implicit UTF-16),
  `textDocument.publishDiagnostics.versionSupport`,
  `textDocument.publishDiagnostics.relatedInformation`,
  `textDocument.publishDiagnostics.codeDescriptionSupport`, and
  `textDocument.publishDiagnostics.dataSupport`. The PR12 pull prerequisites
  are `textDocument.diagnostic` with `relatedInformation`,
  `codeDescriptionSupport`, `dataSupport`, and optional
  `relatedDocumentSupport`, plus
  `workspace.diagnostics.refreshSupport` when refresh is used. A missing
  optional diagnostic field capability removes that field. A client without
  versioned publication and diagnostic `dataSupport` on the push path, or
  without document-pull and diagnostic `dataSupport` on the pull path, does
  not receive Plan 37 finding projection until its host-specific adapter passes
  an equivalent stale-data and join-ID conformance gate.
- Legal PR12 protocol messages are `initialize`, `initialized`, `shutdown`,
  and `exit`; `textDocument/didOpen`, `didChange`, `didSave`, and `didClose`;
  `$/cancelRequest`; `window/workDoneProgress/create` and `$/progress`;
  `textDocument/publishDiagnostics`, `textDocument/diagnostic`, and
  `workspace/diagnostic/refresh`. Initialize advertises the maximal safe static
  subset. `client/registerCapability`/`client/unregisterCapability` is used
  only for a method whose client capability declares
  `dynamicRegistration = true`; otherwise an unsupported method remains absent
  until the next initialize.
- PR12 sets `workspaceFolders.supported = false` and
  `diagnosticProvider.workspaceDiagnostics = false`. It does not advertise
  `workspace/diagnostic`, `workspace/executeCommand`,
  `textDocument/codeAction`, rename, custom task/evidence/history methods, or
  arbitrary vendor methods. PR15 alone enables
  `workspaceFolders.supported = true`, and an opaque revisioned
  `changeNotifications` registration for
  `workspace/didChangeWorkspaceFolders` after the multi-root fixtures pass;
  `workspace.workspaceFolders = true` is the corresponding PR15 client
  prerequisite, not a server capability.
- Effective capability is the intersection of client support, gateway
  guarantee, upstream analyzer support, admitted project/language,
  policy/configuration, and active profile. Negotiation binds
  protocol/catalog/project/workspace/gateway/analyzer/config/policy/client
  revisions; incompatible drift renegotiates or fails typed-stale before work.
- The core PR12 semantic provider keys are
  `declarationProvider`, `definitionProvider`, `typeDefinitionProvider`,
  `implementationProvider`, `referencesProvider`, `hoverProvider`,
  `documentSymbolProvider`, `workspaceSymbolProvider` with
  `resolveProvider = false`, and `callHierarchyProvider`. The later PR12
  sub-slice adds `signatureHelpProvider` with only upstream-declared trigger/
  retrigger characters and `typeHierarchyProvider`. Each key is true/options
  only for the effective capability intersection and is absent otherwise;
  language-scoped changes use method-specific dynamic registration only when
  the client advertises it. `textDocument/diagnostic` and
  `publishDiagnostics` follow the separate diagnostic matrix above.
  Both `renameProvider.prepareProvider` and `renameProvider` are absent because
  LSP exposes preparation only through the rename provider, which necessarily
  advertises rename, and a standard rename response is a `WorkspaceEdit` that
  a host may apply without Plan 34 interception. They can ship only after
  [Plan 34](34-workspace-refactoring-and-api-migration.md) supplies a typed
  candidate/preview operation and each host proves raw edits cannot bypass
  `EditTransaction`.
- PR18 may add one diagnostic-scoped `textDocument/codeAction` quick-fix and
  `workspace/executeCommand` pair solely after Plans 21 and 17 add and freeze
  the binding. Its exact client prerequisites are
  `textDocument.codeAction.codeActionLiteralSupport` for `quickfix`,
  diagnostic `dataSupport`, and `window.showDocument.support`; its server
  capabilities are exactly
  `codeActionProvider = { codeActionKinds: ["quickfix"],
  resolveProvider: false }` and `executeCommandProvider.commands` containing
  only the two Plan-21/Plan-17-frozen public names. Before that coordinated
  gate neither capability is advertised. Plan-35-internal Rust variants
  `OpenInvestigation` and `OpenTask` are placeholders and are never serialized
  as command names. The action returns no edit, executes no analyzer/server
  command, and accepts only an opaque, session-bound `HandoffToken`, never IDs,
  paths, URLs, query text, or executable arguments. General code actions and
  commands remain unavailable.
- A method outside this set, or a request the active analyzer declares
  unsupported, returns an explicit typed capability-unavailable outcome. The
  gateway never guesses a fallback result or synthesizes a plausible-looking
  answer for a method it cannot truthfully answer.
- An admitted analyzer is authoritative only for the typed capability, exact
  workspace/document/configuration snapshot, and coverage it successfully
  completed. It is not authority for unsupported files, unreported generated,
  macro, or dynamic behavior, test execution, or another analyzer's domain.
  TraceDecay may add graph results only when they carry exact generation, file,
  symbol, span, edge-authority, and coverage evidence. It never upgrades a
  heuristic relationship into an analyzer fact.
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
  when it is credential-free HTTPS, matches the authorized repository/PR/check
  scope, and passes the publication-time disclosure check; `data` carrying
  only stable finding ID, Plan 13 `RetrievalAnchorId`, item/thread lifecycle,
  ingress provider outcome, and coverage;
  `relatedInformation` with typed locations and bounded messages where the
  finding references additional sites.
- `LspFindingProjectionV1` serializes exactly Plan 37's current allowlist.
  Its concise message renders a bounded observed/valid/expired freshness cue
  and complete/partial/unknown coverage summary from those owner-provided
  fields; it adds no private `Diagnostic.data` key. PR17 may introduce
  `LspFindingProjectionV2` with one optional, independently authorized opaque
  Plan 24 `TaskId` and typed temporal summary only after Plan 37's schema,
  allowlist, and fixtures are revised in the same coordinated change. Until
  that owner-plan gate passes, V2 is not serialized or accepted. `TaskId` is
  only a join key; it grants no task visibility, navigation, mutation,
  planning, lease, or execution authority.
- The allowlist excludes bodies, diffs, logs, source, task narrative, task
  graphs, dependencies, attempts, leases, receipts, chronology, synthesis
  revisions, cursors, response handles, command arguments, arbitrary JSON, and
  any full `EvidencePacket`. The gateway and bridge do not cache expansion
  responses after delivering or opening the owning surface.
- Severity is conservative: preserve upstream/analyzer severity where scored;
  default to Information for unscored review comments and proximity warnings.
  TraceDecay does not raise severity because several producers agree.
- Publication is bounded. Full text, diffs, and thread bodies expand only
  through authorized TraceDecay read operations
  ([Plan 21](21-cli-mcp-tool-surface-and-output-unification.md)
  `feedback_get`/`feedback_expand` and Plan 13 anchor resolution), never as
  hidden LSP payload.
- Clearing is deterministic and version-monotone: resolution, deletion, head
  SHA drift, content or generation change, or supersession removes or
  republishes the prior diagnostic idempotently for that client document
  version. Duplicates converge and stale updates cannot overwrite newer state.
- Diagnostics default to a concise cue plus bounded related locations and
  authorized expansion. Long explanations, chronology, and task narrative are
  pulled through typed application operations that compose Plan 05/23
  retrieval with Plan 24 identity, not pushed during execution; LSP remains an
  editor projection rather than query, task, or Doctor authority.
- Dirty-overlay feedback findings remain session-only for the authorized
  overlay owner and are never published as durable LSP diagnostics.

### Typed investigation cue and one-way handoff

- Plan 09 owns `InvestigationHandoffRequest`,
  `InvestigationHandoffResult`, `InvestigationAvailability`,
  `InvestigationScopeSnapshot`, `TemporalCoverageSummary`, and
  `AuthorizedInvestigationLink` through a coordinated addition to its existing
  feedback-cycle contract. Plan 35 owns only the adapter DTO
  `LspInvestigationCue`, its bounded LSP encoding, and `HandoffToken`; these
  types cannot land first or become a gateway-private replacement.
- `LspInvestigationCue` contains only a sanitized cue, finding ID,
  `RetrievalAnchorId`, optional V2 `TaskId`, canonical source-root/snapshot
  identity, lifecycle, observed/as-of/expiry/freshness values, complete/partial/
  unknown coverage counts, and authorized links. The cue is a one-way pointer:
  no callback mutates LSP state, and opening it cannot schedule, claim, lease,
  execute, cancel, retry, or complete work.
- Full evidence packets, GitHub thread/reply text, CI logs, chronology,
  synthesis revisions, task neighborhood/history/navigation, planner controls,
  and agent execution remain in typed application/dashboard/CLI-MCP surfaces.
  Plan 21's concrete `feedback_get` and `feedback_expand` plus Plan 13 anchor
  resolution are the full-evidence expansion path;
  `feedback_diagnostics` and `feedback_list` remain bounded views. PR17 task
  navigation and Plan 32 runtime controls remain separate typed operations;
  this plan invents no public task or execution command.
- PR12–PR17 actionable links are authorized credential-free HTTPS source URLs
  in `codeDescription.href` and typed IDs copied into explicit Plan 21 reads.
  Raw URLs receive publication-time authorization only because an LSP client
  opens them without a daemon callback; sensitive or revocable destinations
  therefore omit `codeDescription.href` and require the PR18 tokenized action.
  Authorization-revision changes clear the next publication, but no claim is
  made that a client cannot retain an already received public URL.
  After PR18, the restricted code action may mint a single-use
  `HandoffToken` with a 60-second TTL, bound to client session, actor,
  workspace folder, project/worktree, finding/anchor/task IDs, source
  generation, content digest, and authorization revision. Executing it
  rechecks authorization and uses `window/showDocument` only when the client
  advertised that capability; otherwise it returns the same typed handoff
  result for the host adapter to render.
- Authorization at publication is not transferable to expansion or tokenized
  opening. Projection, code-action creation, token execution, tokenized
  destination opening, every page/hydration/expansion, and every `TaskId`
  pivot recheck `RequestContext`, current capability grants, exact root scope,
  privacy/retention state, authorization revision, and URL safety. Revocation
  invalidates tokens immediately and clears links at the next monotone
  publication; expansion returns `DeniedOrMissing`, never cached content or
  clean empty success.

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
- Hard defaults are a 4 MiB JSON-RPC frame, 2 MiB document, 64 pending
  requests per session, 128 queued requests per engine, four concurrent root
  fan-outs, and eight admitted roots per request. A document publication is at
  most 200 diagnostics and 256 KiB serialized; each diagnostic message is at
  most 512 UTF-8 bytes, `data` at most 1 KiB, and
  `relatedInformation` at most eight locations with 256 UTF-8 bytes per
  message. Crossing a hard limit rejects or deterministically truncates before
  bridge write and reports total/returned/omitted counts and reason through the
  typed application/Doctor surface; LSP never hides a continuation page.
- Dirty changes debounce for 75 ms with a 250 ms maximum wait; save flushes
  immediately. Every accepted incremental edit is applied in order to a
  materialized document snapshot; only downstream analysis jobs for superseded
  snapshots may be coalesced. Publication coalesces for 50 ms with a 200 ms
  maximum wait and preserves the newest accepted error state.
- On the checked-in warm benchmark, bridge initialization is p95 <= 250 ms,
  navigation is p95 <= 100 ms and p99 <= 250 ms, and diagnostics are p95 <=
  500 ms and p99 <= 1.5 s, excluding separately reported cold upstream
  indexing. Accepted cancellation suppresses queued, not-yet-bridge-acknowledged
  publication within 50 ms; an already acknowledged notification is corrected
  only by a later versioned publication. Plan 33 owns later holistic
  optimization; these are Plan 35's
  correctness/UX admission budgets and baseline gates.
- Pull diagnostics and requests use protocol-valid outcomes:
  cancellation returns `RequestCancelled`; superseded document identity
  returns `ContentModified`; saturation or transient store lock returns
  `ServerCancelled` with standard `{ retriggerRequest: true }`. Retry delay and
  lock reason live only in bounded typed engine state, not JSON-RPC error data.
  A partial pull with verified findings returns those findings with per-cue
  coverage; a zero-finding partial pull returns `ServerCancelled` with
  `retriggerRequest: true`, never a clean empty report or fake diagnostic.
- Push synchronization and publication are notifications and return no LSP
  error. Oversized frames close the bridge session before dispatch. If a
  `didChange` sequence cannot be materialized in order within the 2 MiB
  document limit or queue bound, the daemon terminates that LSP session with a
  typed, content-free bridge close reason and never acknowledges preserved
  synchronization. For partial push results, the gateway publishes the
  verified subset with coverage in each real cue; when that subset is empty it
  publishes an empty versioned clear and exposes partial/locked/unavailable
  state only through the typed host-status/Doctor surface. Push-only clients
  therefore cannot infer complete-zero from empty-partial and do not pass the
  universal-diagnostics conformance gate without an equivalent native status
  adapter.
- `LspProjectionAvailability` maps application state separately for push and
  pull: `Current` publishes/returns the matching complete set; `Partial`
  follows the zero/nonzero rules above; `Stale` clears push state and returns
  pull `ContentModified`; `Blocked(StoreLocked)` may reuse an exact
  identity-matching current cache, otherwise clears push state and returns
  pull `ServerCancelled`; `DeniedOrMissing` clears links, tokens, and findings
  without revealing which case applies; `Unavailable` clears findings and
  exposes engine state. A lock or partial provider never becomes a new finding,
  fake source-range diagnostic, or clean result.
- `InvestigationAvailability` separately distinguishes `Ready`, `Partial`,
  `Stale`, `DisclosureLocked`, `DeniedOrMissing`, and `Unavailable`.
  `DisclosureLocked` is returned only when relation existence is independently
  visible under the current grant; otherwise it collapses to
  `DeniedOrMissing`. It neither creates a Plan 24 assignment nor a Plan 32
  lease. Proximity is advisory and never creates a file, task, or runtime lock.

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
  supertypes, and subtypes type-hierarchy methods. Both prepare-rename and
  rename remain unadvertised until Plan 34 interception and host conformance
  prove the returned `WorkspaceEdit` cannot bypass `EditTransaction`. General
  `textDocument/codeAction` remains unavailable before the coordinated PR18
  handoff-only gate.
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
- Ship `LspFindingProjectionV1` for concise feedback cues and prove all full
  evidence expands through Plan 21/Plan 13 operations rather than LSP.
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

### PR17: optional task join projection

- After Plans 24 and 32 ship, add `LspFindingProjectionV2` with an optional
  authorized opaque `TaskId` join key and separate typed Plan 24 task
  navigation. LSP cannot resolve task context, inspect task history, mutate a
  plan, or invoke Plan 32 admission/control.

### PR18: public handoff binding stabilization

- Plan 17 freezes any public command/route/tool/SDK spellings. Only then may
  conformant hosts advertise the Plan-21-bound handoff-only diagnostic code
  action and execute-command binding. PR18 adds no task/evidence storage,
  planner authority, general command channel, or bulk LSP retrieval.

## Exact implementation and evidence map

- Plan-09-owned application contract:
  a coordinated addition to the existing
  `crates/tracedecay-application/src/feedback/cycle.rs` defines
  `InvestigationHandoffRequest`, `InvestigationHandoffResult`,
  `InvestigationAvailability`, `InvestigationScopeSnapshot`,
  `TemporalCoverageSummary`, `AuthorizedInvestigationLink`, and the internal
  `OpenFeedbackInvestigation` use case.
- PR9 diagnostic model/store ports:
  `crates/tracedecay-domain/src/diagnostics.rs`,
  `crates/tracedecay-store/src/diagnostics/mod.rs`,
  `crates/tracedecay-store/src/diagnostics/ports.rs`,
  `crates/tracedecay-application/src/diagnostics/mod.rs`, and
  `src/migrate/consolidate/sqlite/diagnostics.rs` own generation-bound
  identity, persistence ports, application translation, and migration. The
  gateway consumes those APIs and defines no duplicate diagnostic record.
- Plan-35-owned gateway:
  `src/daemon/lsp_gateway/mod.rs`,
  `src/daemon/lsp_gateway/capabilities.rs`,
  `src/daemon/lsp_gateway/session.rs`,
  `src/daemon/lsp_gateway/projection.rs`,
  `src/daemon/lsp_gateway/handoff.rs`, and
  `src/daemon/lsp_gateway/limits.rs` define `GatewayCapabilitySet`,
  `LspFindingProjectionV1`, `LspFindingProjectionV2`,
  `LspInvestigationCue`, `LspProjectionAvailability`, `LspDeliveryState`, and
  `HandoffToken`.
- Transport-only bridge: `src/lsp_bridge.rs`. It owns framing, authentication,
  request correlation, and forwarding only.
- Owning non-LSP surfaces:
  `src/cli/feedback.rs` and `src/mcp/tools/handlers/feedback.rs` consume the
  Plan 09 result under Plan 21. Plan 11's existing
  `dashboard/code-diagnostics/src/CodeDiagnostics.tsx` and reusable
  `dashboard/lib/evidence/EvidenceExpansionDialog.tsx` own dashboard opening
  and expansion. These surfaces do not import gateway DTOs; exact public names
  remain owned by Plan 21/Plan 17.
- Broker/cache/storage:
  `src/diagnostics/lsp/broker.rs` evolves into the upstream broker;
  `src/daemon/lsp_gateway/provider_cache.rs` owns cache admission/eviction;
  `src/global_db/lsp_provider_cache.rs` and
  `src/migrate/consolidate/sqlite/lsp_provider_cache.rs` own the clean-result
  schema and migration. `Cargo.toml` adds the direct `lsp-types` protocol
  dependency and reuses existing `serde`, `serde_json`, `tokio`, and
  `criterion`; it adds `[[bench]] name = "lsp_gateway", harness = false`.
  `Cargo.lock` changes only for the resolved `lsp-types` graph. No second LSP
  runtime framework is added, and no client process gains a store dependency.
- Contract target: `tests/lsp_gateway_suite/main.rs` with
  `protocol.rs`, `limits.rs`, `backpressure.rs`, `diagnostics.rs`,
  `investigation_handoff.rs`, `authorization.rs`, `host_conformance.rs`,
  `multi_root.rs`, `remote_authority.rs`, and `performance.rs`.
  Protocol bytes live under `tests/lsp_gateway_suite/fixtures/claude-code/`;
  deterministic Rust/Python/TypeScript workspaces live under
  `tests/lsp_gateway_suite/fixtures/workspaces/`.
  `benches/lsp_gateway.rs`, `benchmarks/pr12-lsp-gateway/workload-v1.json`, and
  `scripts/check-lsp-gateway-benchmark.sh` own benchmark generation and
  threshold enforcement; reviewed results live at
  `benchmarks/pr12-lsp-gateway/baseline.json`, candidate output at
  `benchmarks/pr12-lsp-gateway/result-candidate.json`, and their shared schema
  at `benchmarks/pr12-lsp-gateway/schema-v1.json`.
  `.github/workflows/lsp-gateway-benchmark.yml` runs the benchmark and threshold
  script on the designated runner and uploads baseline/candidate metadata.

## Acceptance

- A real Claude Code session registers only the TraceDecay LSP plugin for the
  configured languages and receives Rust, Python, and TypeScript diagnostics
  through one daemon gateway.
- Declaration, definition, type definition, implementation, references,
  hover, signature help, document/workspace symbols, call hierarchy, and type
  hierarchy match direct upstream results on representative projects, with
  deterministic exact TraceDecay graph augmentation where available.
- `rename_and_prepare_are_not_advertised_without_plan34_interception` proves
  neither rename method can bypass Plan 34.
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
  fixtures prove idempotent, version-monotone convergence: duplicate delivery
  is harmless, stale publication cannot overwrite newer state, and reconnect
  may redeliver current diagnostics.
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
  V1 stable finding/anchor IDs, V2 optional independently authorized `TaskId`,
  bounded temporal/coverage summaries and `relatedInformation`, authorized
  `codeDescription.href`, deterministic
  clear/remap on head/content/generation change, lossless expansion through
  owning retrieval operations, truncation without hidden payload, and
  dirty-overlay non-durability.
- Named tests include
  `diagnostic_projection_never_embeds_full_evidence`,
  `lsp_task_id_is_join_key_only`,
  `lsp_handoff_never_executes_work`,
  `feedback_investigation_authorization_recheck`,
  `feedback_investigation_authorized_link_revocation`,
  `feedback_investigation_stale_partial_disclosure_locked`,
  `disclosure_locked_does_not_enumerate_hidden_relations`,
  `did_change_coalesces_but_save_flushes`,
  `incremental_changes_materialize_in_order_before_analysis_coalesces`,
  `queue_saturation_rejects_without_acknowledging`,
  `lock_after_version_change_clears_old_publication`,
  `push_and_pull_zero_partial_have_distinct_truthful_outcomes`,
  `oversized_notification_closes_without_desynchronizing`,
  `pr18_handoff_token_tampering_fails_without_edit`,
  `multi_root_denial_never_falls_back`, and
  `task_id_visibility_is_independent_of_finding_visibility`.
- Protocol fixtures assert the exact PR12/PR15/PR18 capability matrices,
  every client-capability permutation, PR15 `changeNotifications`,
  method-specific dynamic-registration permission and reconnect behavior,
  strict `Diagnostic.data` key/version allowlists, unauthorized `TaskId`
  omission, rejected bulk/task/custom methods, no gateway/bridge persistence
  after disconnect or restart, and authorization recheck at every handoff.
- Linux, macOS, and Windows fixtures cover URI normalization, UTF-16 positions,
  process lifecycle, command discovery, socket/stdio behavior, path safety, and
  shutdown.
- Required commands are
  `cargo test --all-features --test lsp_gateway_suite`,
  `cargo bench --bench lsp_gateway`,
  `scripts/check-lsp-gateway-benchmark.sh
  benchmarks/pr12-lsp-gateway/baseline.json`,
  `cargo check --all-features`, and `cargo test --all-features`.
  `workload-v1.json` fixes content hashes, operation mix, 5 warm-up rounds, 200
  measured operations per method, 5 independent processes, concurrency 1 and
  8, cache/analyzer warm-state boundaries, and bridge-receive through
  bridge-write clock boundaries. The threshold script gates only on the
  designated Linux x86_64 benchmark runner class recorded in the baseline;
  other CI hosts report without comparing. Percentiles are computed per
  independent process and the worst process must satisfy every absolute p95/
  p99 budget above; cancellation's maximum observed pre-acknowledgement delay
  must be <= 50 ms. The script also fails when the lower bound of a bootstrap
  95% confidence interval over process-level p95 values shows greater than 10%
  latency regression, or when median peak RSS regresses by both greater than
  10% and greater than 16 MiB. It checks every hard payload/queue limit and
  fails on either an absolute-budget violation or a regression-threshold
  violation. PR12 records the baseline for Plan 33's later end-to-end
  optimization.

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
