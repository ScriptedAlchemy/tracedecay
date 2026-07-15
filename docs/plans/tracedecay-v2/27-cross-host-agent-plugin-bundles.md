# TraceDecay V2 Cross-Host Agent Integration Plan

## Status / role

PR6 establishes the canonical host-integration model and working Claude Code, Codex, Cursor, and Hermes adapters. PR13 completes lifecycle management, Doctor, conformance, packaging, and cutover for every supported host.

## Outcome

TraceDecay ships one host-neutral integration catalog and thin host-native adapters. Each host keeps its native strengths and reports unsupported capabilities explicitly while using the same daemon, authorization, privacy, memory, and tool semantics.

## Owns

- The canonical host-integration manifest and deterministic per-host projections.
- Claude Code, Codex, Cursor, and Hermes hook, tool-discovery, command, skill, and agent adapters.
- Capability negotiation and explicit host-difference reporting.
- Safe install, update, repair, uninstall, backup, and restore of TraceDecay-owned host configuration.
- Host Doctor checks and cross-host conformance fixtures.

## Does not own

- Product use-case definitions already owned by domain, catalog, application, policy, memory, or workflow components.
- Database access, daemon authority, or host-specific copies of durable TraceDecay state.
- A requirement that MCP be installed; the CLI and daemon API are the baseline.
- Workflow JavaScript, incremental PR-series scripts, Markdown task parsers, rewrite-plan executors, progress ledgers, or generated plan state.
- Silent emulation of a capability the host cannot support.

## Required behavior

### Canonical integration catalog

- Define each integration capability once with stable identity, privacy/effect class, required daemon/API support, and host availability.
- Generate or render only thin host-native registration artifacts from that catalog.
- Pin installed artifacts to a compatible TraceDecay protocol and catalog revision and report skew clearly.
- Keep host-local files free of copied product logic and durable project/session/fact state.

### Host adapters

- Translate native lifecycle and tool events into canonical sanitized observations with provider-native identity and ordering evidence.
- Invoke only public CLI or daemon APIs; hooks and host processes never open TraceDecay databases.
- Preserve parent/subagent lineage, working directory, repository/worktree identity, tool outcomes, cancellation, and compaction boundaries when the host exposes them.
- Bound hook latency and payload size; enqueue or signal durable daemon work rather than performing it in the hook.
- Remain useful without MCP and expose compact fallback commands and help.

### Capability differences

- Publish a tested capability view for each host using `supported`, `degraded`, or `unavailable` with a reason.
- Never infer unsupported events, lifecycle controls, permissions, or task semantics.
- Preserve provider-native workflows as observations unless the user explicitly imports them into a TraceDecay product workflow.

### Lifecycle safety

- Discover existing user configuration and ownership before mutation.
- Use atomic writes, ownership markers, backups, conflict detection, and rollback.
- Preserve unrelated user configuration and refuse ambiguous ownership.
- Make install and update idempotent; make repair explicit and receipt-backed; remove only TraceDecay-owned state during uninstall.
- Keep service-manager ownership and daemon lifecycle separate from host registration files.

### Doctor and conformance

- Doctor is read-only: it reports installation ownership, version skew, endpoint reachability, hook delivery, capability availability, and actionable causes.
- Repair is a separate confirmed operation using the same preflight evidence.
- Human and structured output share stable finding identities and remediation references.
- Conformance uses native host fixtures and processes rather than source-text inspection of host applications.

## Acceptance

- Claude Code, Codex, Cursor, and Hermes install, update, repair, backup/restore, and uninstall fixtures preserve unrelated configuration and recover from interruption.
- Every supported native event reaches one canonical observation exactly once; unavailable events remain explicit.
- Host processes and hooks pass negative tests proving they cannot open stores or become daemon writers.
- MCP-present and CLI-only paths produce equivalent authorized product behavior.
- Version-skew, missing binary, dead daemon, stale registration, ownership conflict, and partial-install Doctor fixtures return stable causes without mutation.
- Cross-host handoff preserves repository/worktree, session, parent/subagent, privacy, and provenance identity.
- Repository checks reject workflow JS, Markdown plan parsers, rewrite executors, copied product catalogs, and host-local durable-state mirrors.
