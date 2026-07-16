# TraceDecay V2 Cross-Host Agent Integration Plan

## Status / role

PR6 establishes the host-neutral integration catalog model, working Claude
Code, Codex, Cursor, Hermes, and Kiro observation adapters, canonical event
semantics, and conformance fixtures. PR13 completes packaging, registration,
conflict handling, install/repair/uninstall, one configured-language TraceDecay
LSP plugin for Claude Code, the Cursor desktop native-diagnostics adapter,
Cursor cloud/Codex/Hermes/Kiro hook/MCP/CLI or typed unavailable paths, host
install/registration/protocol conformance findings and fixtures, and cutover for
every supported host. PR14 owns canonical Doctor presentation, diagnosis, and
remediation orchestration that invokes PR13 lifecycle operations without
redefining repair mechanics.

## Outcome

TraceDecay ships one host-neutral integration catalog and thin host-native adapters. Each host keeps its native strengths and reports unsupported capabilities explicitly while using the same daemon, authorization, privacy, memory, and tool semantics.

## Owns

- The canonical host-integration manifest, capability catalog, and deterministic
  per-host projections. PR6 delivers the model, observation adapters, event
  semantics, and fixtures; PR13 delivers packaging, registration, conflict
  handling, install/repair/uninstall, one configured-language TraceDecay LSP
  plugin for Claude Code, the Cursor desktop native-diagnostics adapter, and
  Cursor cloud/Codex/Hermes/Kiro hook/MCP/CLI or typed unavailable path
  projections.
- Claude Code, Codex, Cursor, Hermes, and Kiro hook, tool-discovery, command,
  skill, and agent adapters where each host supports those capabilities.
- Capability negotiation and explicit host-difference reporting.
- Host lifecycle operation mechanics: install, update, repair, uninstall,
  backup/restore, explicit confirmation, receipts, and rollback/recovery for
  TraceDecay-owned host configuration (PR13). PR13 owns the operations; it does
  not own canonical Doctor presentation.
- Host install, registration, and protocol-conformance findings/state and
  cross-host conformance fixtures (PR13). PR14 in
  [Plan 11](11-dashboard-frontend.md) owns the canonical Doctor kernel/UI,
  dashboard views, diagnosis, and remediation orchestration that invokes these
  PR13 lifecycle operations without redefining repair mechanics.

## Does not own

- Product use-case definitions already owned by domain, catalog, application, policy, memory, or workflow components.
- Database access, daemon authority, or host-specific copies of durable TraceDecay state.
- A requirement that MCP be installed; the CLI and daemon API are the baseline.
- Workflow JavaScript, incremental PR-series scripts, Markdown task parsers, rewrite-plan executors, progress ledgers, or generated plan state.
- Silent emulation of a capability the host cannot support.
- GitHub REST/GraphQL identity, finding ownership, comment posting, or a
  second durable finding store; ingestion delegates to the read-only adapter
  path and Plan 09/Plan 37 advisory findings.

## Required behavior

### Canonical integration catalog

- Define each integration capability once with stable identity, privacy/effect class, required daemon/API support, and host availability.
- PR13 projects one configured-language TraceDecay LSP plugin for Claude Code
  from [Plan 25](25-code-intelligence-indexing-crate.md) language descriptors
  and [Plan 20](20-configuration-control-plane.md) bounded, non-sensitive
  per-language registration selection, following
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) for gateway,
  provider, and duplicate-analyzer policy. One plugin covers the configured
  language subset; PR13 does not ship one plugin per language or project the
  LSP plugin to every host. PR6 defines the host-neutral catalog model and
  observation-adapter contracts only; it does not ship, package, register, or
  install the LSP plugin. Host artifacts define no independent language,
  extension, or analyzer authority and never copy analyzer commands,
  arguments, initialization options, settings, or environment.
- PR13 generates or renders thin host-native registration artifacts from that
  catalog and pins installed artifacts to a compatible TraceDecay protocol and
  catalog revision, reporting skew clearly.
- Keep host-local files free of copied product logic and durable project/session/fact state.

### Host adapters

- Decode native lifecycle and tool events into bounded canonical `HookEvent`
  envelopes with provider-native identity and ordering evidence. The daemon
  owns sanitization and creation of durable observations. PR6 owns Claude Code,
  Codex, Cursor, Hermes, and Kiro observation adapters; PR13 owns one
  configured-language TraceDecay LSP plugin for Claude Code, the Cursor desktop
  native-diagnostics adapter, and other host-native registration/packaging only.
- Invoke only public CLI or daemon APIs; hooks and host processes never open TraceDecay databases.
- PR13's one configured-language TraceDecay LSP plugin for Claude Code launches
  only the thin bridge; it never starts analyzers, opens LSP connections
  itself, or owns diagnostic routing, gateway lifecycle, or duplicate-analyzer
  policy — those remain in
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- Preserve parent/subagent lineage, working directory, repository/worktree identity, tool outcomes, cancellation, and compaction boundaries when the host exposes them.
- Bound hook latency and payload size; enqueue or signal durable daemon work rather than performing it in the hook.
- Remain useful without MCP and expose compact fallback commands and help.

### Capability differences

- Publish a tested capability view for each host using `supported`, `degraded`, or `unavailable` with a reason.
- Report host-specific LSP capability differences explicitly, following
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md)'s
  capability-specific host model: Claude Code gets one configured-language
  TraceDecay LSP plugin and the full gateway; the Cursor desktop
  native-diagnostics adapter reuses/ingests native diagnostics and publishes
  TraceDecay-only findings; Cursor cloud and Codex use hooks/MCP/CLI instead of
  a degraded LSP session; Hermes and Kiro report hook/MCP/CLI or typed
  unavailable paths with tested typed outcomes rather than implying full LSP.
  All other supported hosts receive the same explicit capability reporting or a
  tested unavailable path.
- Never infer unsupported events, lifecycle controls, permissions, or task semantics.
- Preserve provider-native workflows as observations unless the user explicitly imports them into a TraceDecay product workflow.
- Host adapters are the delivery mechanics for [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)'s
  advisory feedback-cycle result on every host as part of the PR11–PR13
  milestone. PR13 hook, MCP, and CLI contexts deliver the same typed result;
  Claude Code receives the full LSP gateway projection defined by
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md); Cursor
  desktop receives the native-diagnostics adapter projection; non-LSP hosts
  receive hooks/MCP/CLI paths. This plan owns transport and registration
  mechanics; Plan 09 owns the result contract and Plan 37 owns the
  architecture.
- Existing GitHub PR review comments are ingested through a read-only GitHub
  adapter/application path at PR13 and surfaced as advisory findings. This
  plan does not post, update, or resolve GitHub comments and does not claim
  GitHub API identity, finding ownership, or durable finding storage.

### Lifecycle safety (PR13)

- Discover existing user configuration and ownership before mutation.
- Discover existing extension claims before registration. Replacing a
  conflicting plugin requires explicit confirmation, preserves third-party
  configuration, and has a tested rollback.
- Use atomic writes, ownership markers, backups, conflict detection, and rollback.
- Preserve unrelated user configuration and refuse ambiguous ownership.
- Make install and update idempotent; make repair explicit and receipt-backed; remove only TraceDecay-owned state during uninstall.
- Keep service-manager ownership and daemon lifecycle separate from host registration files.

### Host conformance findings (PR13) and Doctor remediation (PR14)

- PR13 emits read-only host install, registration, version skew, endpoint
  reachability, hook delivery, capability availability, and protocol-conformance
  findings with stable identities and remediation references consumable by PR14
  Doctor.
- PR13 owns confirmed repair/install/update/uninstall operation mechanics—preflight
  evidence, explicit confirmation, receipts, backup/restore, and
  rollback/recovery. PR14 owns canonical Doctor presentation, diagnosis, and
  remediation orchestration that invokes those PR13 operations; PR14 does not
  redefine repair mechanics.
- Conformance uses native host fixtures and processes rather than source-text
  inspection of host applications.
- PR13 LSP conformance, limited to LSP-capable hosts (Claude Code), runs against
  real supported host processes, including initialization, document lifecycle,
  cancellation, shutdown, and reconnect. The Cursor desktop native-diagnostics
  adapter has separate conformance coverage. Cursor cloud, Codex, Hermes, and
  Kiro prove hook/MCP/CLI or typed unavailable paths instead of LSP session
  conformance.

## Acceptance

- PR13 install, update, repair, backup/restore, and uninstall fixtures for
  Claude Code, Codex, Cursor, Hermes, and Kiro preserve unrelated configuration
  and recover from interruption. Claude Code fixtures include the
  configured-language TraceDecay LSP plugin; Cursor desktop fixtures include
  the native-diagnostics adapter; Cursor cloud, Codex, Hermes, and Kiro fixtures
  prove hook/MCP/CLI or typed unavailable paths without assuming full LSP
  registration.
- Every supported native event reaches one canonical observation exactly once; unavailable events remain explicit.
- Host processes and hooks pass negative tests proving they cannot open stores or become daemon writers.
- MCP-present and CLI-only paths produce equivalent authorized product behavior.
- Version-skew, missing binary, dead daemon, stale registration, ownership conflict, and partial-install host-conformance fixtures return stable causes without mutation; PR14 Doctor consumes the same finding identities for kernel/UI presentation and remediation orchestration that invokes PR13 lifecycle operations.
- Cross-host handoff preserves repository/worktree, session, parent/subagent, privacy, and provenance identity.
- PR13 Plan 37 delivery fixtures prove hook/MCP/CLI, Claude LSP, and Cursor
  native-diagnostics paths publish semantically equivalent advisory results
  where capabilities overlap; read-only GitHub ingestion fixtures prove ingested
  review threads surface without posting; security fixtures prove host
  processes cannot claim GitHub finding ownership or bypass authorization;
  truncation/clear/remap fixtures prove host adapters preserve finding IDs,
  cursors, and dirty-overlay non-durability.
- Repository checks reject workflow JS, Markdown plan parsers, rewrite executors, copied product catalogs, and host-local durable-state mirrors.
