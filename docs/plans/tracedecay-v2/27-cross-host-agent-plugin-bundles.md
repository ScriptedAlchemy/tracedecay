# TraceDecay V2 Cross-Host Agent Integration Plan

## Status and existing foundation

PR6 established one host-neutral integration manifest, native observation
adapters for Claude Code, Codex, Cursor, Hermes, and Kiro, explicit
Cline-family capability evidence, and daemon host admission. PR13 adds the
verified Kimi Code and OpenCode capabilities and turns that foundation into
installable, repairable host integrations that deliver the working feedback
journey. It does not add a second catalog or a generic connector framework.

**Status correction (2026-07-29).** Earlier reachability gaps in conflict
discovery, capability reporting, checked-in native evidence consumption, and
unsupported-host classification are closed through production callers. Do not
recreate their former helper, packet, file-layout, or test-target structure.

The retained product rules are:

- conflict discovery precedes confirmation, and changed ownership state makes a
  confirmed lifecycle plan stale;
- authentic checked-in provider fixtures carry origin, native version, and
  content digest and are consumed by the real sanitizer and host path;
- Cursor Cloud and every other unadmitted host report a typed unavailable
  reason rather than an empty supported component set; and
- shared branding, configuration shape, source layout, or extension ancestry
  never establishes a Cline-family route.

The remaining product work is the official lifecycle and feedback journey,
OpenCode duplicate-analyzer prevention, selected cross-platform lifecycle and
rollback combinations, real Cline-family route or unavailable proof, and the
Cursor Core component-ownership conflict. Exact test names, counts, generated
matrices, and intermediate registration scaffolding are historical evidence,
not acceptance requirements.

Only actually independently released host/provider protocols and names retain
compatibility. Pure transient PR13 source-only/internal helpers and
wire-visible V2 request revisions change in place. Generated host files, local
queues, checkpoints, receipts, and every other persisted V2 state accept only
their exact final shape; any other database, store, spool, file, or projection
returns typed `ResetRequired` and requires explicit reset or recreation. No
storage reader, migration, backfill, dual write, or census path exists.
Bundle/protocol negotiation remains separate for actual released-version skew.

## PR13 user outcome

A user can install TraceDecay into every supported host, make an edit or stop
an agent, and receive the same authorized feedback through the best native
surface that host actually supports. The user can also refresh and inspect
existing GitHub review threads through a read-only path. Install, update,
repair, and uninstall preserve unrelated configuration and recover safely from
interruption.

## End-to-end production paths

### Host install and repair

1. The lifecycle operation discovers the host, existing TraceDecay ownership,
   third-party claims, protocol compatibility, and the requested components.
2. A dry run reports exact files/settings it owns, conflicts, backups, and the
   rollback plan. Ambiguous ownership or a competing plugin requires explicit
   confirmation; discovery never grants authority.
3. Apply verifies the embedded bundle's schema, version, declared capabilities,
   content digest, and pinned configuration revision, writes only
   TraceDecay-owned state atomically, records a receipt, and verifies native
   registration. The digest detects corruption; it is not a separate origin or
   release-signing authority.
4. On failure, rollback restores the backup and leaves unrelated host
   configuration untouched. Repair replays the same ownership-aware operation;
   uninstall removes only TraceDecay-owned state.

### Edit and stop feedback

1. Plan 07 adapters signal real native saved-edit and stop boundaries.
2. The daemon runs Plan 09 feedback and Plan 22 Scout, including Plan 37
   diagnostics/impact, CI localization, read-only GitHub findings, and
   proximity.
3. Plan 27 routes the same result to the host's conforming surface:
   - Claude Code uses one configured-language TraceDecay LSP plugin plus
     hook/MCP/CLI fallbacks;
   - Cursor desktop uses its native diagnostics adapter and hooks/MCP/CLI;
   - Kimi Code uses its documented manifest-scoped or global `PostToolUse` and
     `Stop` hooks, MCP, and installed skills/commands;
   - OpenCode uses its documented local JS/TS plugin events (`file.edited`,
     `tool.execute.after`, `session.idle`/`session.status`, and LSP events),
     custom LSP configuration, MCP, custom agents/skills, commands (prompt
     templates), and instruction/rules content;
   - Cursor cloud, Codex, Hermes, and Kiro use hooks, MCP, or CLI where native
     support exists and otherwise return a typed unavailable result; and
   - each admitted Cline-family host uses an explicitly packaged hook, MCP, or
     CLI route only when checked-in native/version evidence proves it;
     otherwise discovery and delivery return the evidence-backed typed
     unavailable reason for that exact host and version.
4. Delivery records only bounded, content-free disposition and latency. It
   never infers adoption from display or timing.

### Read-only GitHub refresh

1. An explicit host, MCP, or CLI operation requests refresh for an authorized
   repository and pull request.
2. The adapter issues only allowlisted read operations, passes provider bytes
   and stable provider identity to daemon capture, and commits the canonical
   observation and refresh cursor atomically.
3. Overlapping pages and replay converge by stable comment/thread/version
   identity. A complete refresh can publish deletes; partial pagination,
   cursor drift, cancellation, authorization loss, or source drift preserves
   the previous complete generation and reports partial/stale/unavailable.
4. The same Plan 09 result surfaces remapped findings on supported host
   surfaces. Full bodies expand only through authorized evidence reads.

There is no GitHub mutation client. REST is limited to `GET`; GraphQL transport
may use HTTP `POST` only for the one shipped compile-time static audited
allowlisted `query`. Mutation documents, write-capable methods, and
write-capable or indeterminate credential scopes fail before network access.

## PR13 implementation slices

### Package the supported host paths

- Generate thin host-native artifacts from the existing integration manifest
  and pin them to compatible TraceDecay protocol and catalog revisions.
- Ship every PR13 first-party host bundle as a versioned embedded asset in the
  trusted TraceDecay binary. PR13 has no detached bundle signature, bundle trust
  root, delegated release key, or bundle-key revocation path. Content digests
  detect accidental corruption; schema, version, and capability checks enforce
  compatibility.
- Reject external and third-party bundle loading in PR13. If a real external
  distribution path is added later, that feature defines and validates its own
  trust model instead of predeclaring one here.
- Keep a mandatory MCP-free core path with CLI, hooks, skills, and daemon API
  bindings. Optional MCP companions install and uninstall independently and
  use the same binary, daemon, authorization, and application operations.
- Preserve host-native commands, tool discovery, skills, agents/roles, hooks,
  lifecycle matchers, and capability projections already accepted in the
  integration manifest. Packaging may split Core, Context MCP, and Operator
  MCP for eager-client limits, but component boundaries never change product
  semantics or make MCP a correctness dependency.
- Package one configured-language Claude Code LSP plugin; do not create one
  plugin per language or pretend every host supports LSP.
- Package Cursor desktop native diagnostics without starting a duplicate
  analyzer for the same language. Publish TraceDecay-only findings through the
  native adapter.
- Package Kimi Code's documented plugin manifest and global-hook forms without
  conflating their ownership or scope. Register only `PostToolUse` and `Stop`
  for PR13 feedback, install MCP and skills/commands through their native
  mechanisms, and preserve unrelated global hooks and plugin state.
- Package an OpenCode local JS/TS plugin for `file.edited`,
  `tool.execute.after`, `session.idle`/`session.status`, and LSP events, plus
  its native MCP, custom agents/skills, commands (prompt templates), and
  instruction/rules content. Agent-referenced prompt files are packaged within
  the owning agent component, and `AGENTS.md` remains instruction/rules
  content; neither creates a standalone prompt component. Its custom
  TraceDecay LSP configuration must detect an existing analyzer for each
  language. It either retains that analyzer and runs TraceDecay-only
  projection, or explicitly selects the daemon-managed analyzer through the
  lifecycle operation; it never starts both or silently disables another
  analyzer.
- Package a Cline-family route only for host/version combinations with
  checked-in native event and registration evidence. Keep all other
  Cline-family variants discoverable as typed unavailable with the exact
  missing capability; do not infer compatibility from shared branding,
  configuration shape, or extension ancestry.

### Ship lifecycle operations

- Edit Hermes YAML with `yaml-edit`; use each supported host's official
  SDK/plugin/registration API where it exists; and route every resulting
  change through the existing atomic-write, backup, compare-and-swap,
  lifecycle receipt, and rollback kernel. This replaces bespoke YAML/string
  patching and host-local write choreography while retaining ownership,
  unrelated configuration, exact prior state, dry run, confirmation,
  interrupted repair, and uninstall semantics.
- If a host has no proven official mutation API, or `yaml-edit` cannot
  round-trip the exact supported Hermes document without collateral change,
  keep the component unavailable/read-only and emit the typed remediation.
  Do not silently fall back to raw replacement or a second lifecycle engine.
- Provide callable install, update, repair, backup/restore, and uninstall
  operations with dry run, explicit confirmation, idempotency, receipts, and
  rollback.
- Treat Kimi Code plugin/global registrations and OpenCode plugin, LSP, MCP,
  agent, skill, command, and instruction/rules artifacts as lifecycle-owned
  components. Agent-referenced prompt files follow the agent component;
  command prompt templates follow the command component; and `AGENTS.md`
  follows instruction/rules ownership. Dry run, repair, and rollback preserve
  all unrelated host configuration and restore the exact prior analyzer
  selection.
- Use `configure -> dry-run -> confirm -> apply` for host lifecycle changes
  over Plan 20's protected configuration mutation. Apply uses the exact
  confirmed change plan and expected base revision; stale or unauthorized
  state commits nothing. Raw replacement rules never enter configuration or
  host artifacts.
- Detect extension and registration conflicts before mutation. Never disable,
  replace, adopt, install, or upgrade third-party software silently.
- Keep service-manager and daemon lifecycle separate from host registration.

### Ship GitHub review ingestion

- Use existing `ureq`, shared narrow typed Serde DTOs, and one compile-time
  static audited GraphQL query for the concrete GitHub path. This replaces a
  new provider client, dynamic GraphQL parser, and source-local response models
  while retaining the acquisition and publication contract below. `gh api`
  remains a manual troubleshooting fallback; schema/fixture drift returns
  typed partial, stale, or unavailable and preserves the last complete
  generation.
- Do not add Octocrab, `backon`, or `graphql-parser`; the refresh owner keeps
  its explicit bounded retry and `Retry-After` behavior.
- Implement the one concrete read-only GitHub review refresh needed by PR13.
  Do not introduce provider-neutral source connector catalogs, generic query
  capability projections, signed connector-selection schemas, or planner
  manifests for hypothetical providers.
- Preserve repository/PR/thread/comment/version identity, lifecycle, provider
  outcome, cursor, authorization, freshness, coverage, and canonical evidence
  anchors. Do not copy unsanitized bodies into lifecycle receipts or host
  artifacts.
- Return complete, partial, stale, denied, rate-limited, unavailable, and
  failed states honestly. No failed or partial refresh becomes a clean empty
  result.

The concrete GitHub path retains all accepted acquisition behavior:

- event hints are a low-latency wakeup, never proof of gap-free coverage;
- incremental polling overlaps the prior frontier, deduplicates stable
  object/version identity, and advances its cursor only after canonical
  capture commits every page;
- whole-root reconciliation stages one generation and publishes deletes only
  after complete pagination, current repository authorization, and a
  consistent complete scan;
- when GitHub cannot provide a stable snapshot token, exactly two complete
  scans must agree on ordered identity/version digests before publication;
- cursor expiry, source drift, cancellation, crash, incomplete pages, missing
  consistency, or mismatched scans preserve the last complete generation and
  report partial/stale coverage;
- event/poll races, duplicate events, restart replay, and overlapping pages
  converge to one canonical observation without claiming transport
  exactly-once delivery.

The refresh/status/query operations retain lexical, semantic, and graph
capability where the canonical observations support them, with explicit
freshness, coverage, latency/cost class, authorization, watermark, and evidence
drilldown. Event acceptance targets projected availability at p95 <= 5 s,
scheduled incremental refresh at p95 <= 60 s, and complete 10,000-item
whole-root refresh at p95 <= 15 minutes; stale begins at two minutes and hard
expiry at one hour unless a stricter validated configuration applies. A stale
or expired source remains visible and never becomes clean empty.

Each attempt receives a durable, content-free receipt with refresh identity,
mode, cursor before/after, generation, capture receipts, counts, disposition,
failure class, authorization/configuration/catalog identity, and next retry.
Replaying an idempotency key returns the existing terminal receipt. Bounded
full-jitter transport retry starts from one second and caps at five minutes;
`Retry-After` caps at 15 minutes. The built-in validated defaults permit eight
attempts and quarantine at eight, with validated limits no greater than 16.
Authorization loss, unsafe captured input, and invalid configuration
quarantine immediately; the same schema drift on three consecutive
attempts, poison data, exhausted retries, or a failed single cursor-recovery
reconcile quarantines the affected root while
preserving its last generation. Only an explicit authorized repair against a
newer valid configuration releases quarantine. Connector failures never enter
the host observation replay spool.

### Report native capability truth

- Report each delivery surface independently as supported, degraded, or
  unavailable with a stable reason. Failure of LSP cannot relabel a healthy
  hook/MCP/CLI path, and no adapter silently substitutes another host,
  backend, or transport.
- Preserve independent capability reporting for task-boundary signal,
  busy/composition signal, quiet mode, passive diagnostics, active message
  projection, local expansion, explicit feedback receipt, hook fanout, LSP or
  native-diagnostics fanout, and CLI fallback. Missing native capability is
  never inferred from prompt text or emulated from another host.
- Report Kimi Code manifest/global hook scope, `PostToolUse`, `Stop`, MCP, and
  skills/commands independently. Report OpenCode local-plugin edit/tool/
  session/LSP events, custom-LSP analyzer ownership, MCP, custom agents
  (including their referenced prompt files), skills, commands (prompt
  templates), and instruction/rules content independently; one healthy
  surface never conceals another surface's conflict or unavailable state.
- Emit host registration, version-skew, endpoint, hook-delivery, and protocol
  conformance evidence for PR14 Doctor consumption. PR13 owns the actual
  repair mechanics; PR14 owns diagnosis and orchestration.

## Replacement and deletion

- Delete source-only PR6 compatibility generators and duplicate integration
  scaffolds once all package projections consume the single existing manifest.
  Actual independently released protocol names and revisions follow their
  public support policy. Persisted manifests, generated host files, queues,
  checkpoints, and receipts never become a compatibility input: a non-final
  shape returns `ResetRequired`; branch history alone creates no public
  compatibility window.
- Remove the generic source-connector contract, generic capability catalog,
  future work/task/native-execution projection fields, exact type/file/schema
  inventories, enumerated fixture-directory manifests, Cartesian host
  matrices, fake-clock planning packets, and placeholder benchmark gates.
- Remove copied product logic and durable project/session/finding state from
  installed host files.
- Remove any adapter-local policy, fallback, feedback ranking, GitHub finding
  store, or host-local workflow authority.
- This removes only generic future scaffolding and planning inventories. It
  does not remove a supported component, host command/tool/skill/agent/hook,
  agent-owned referenced prompt file, instruction/rules content, LSP
  configuration, delivery capability, refresh/query mode, receipt/failure/
  quarantine behavior, lifecycle operation, configuration flow, rollback,
  compatibility obligation, or safety semantic.

## Direct acceptance

- Provider and observation acceptance uses only checked-in real native fixtures
  with recorded origin, native version, and digest, replayed through the real
  Plan 03 sanitizer and consuming path. Synthetic, lookalike, or invented
  protocol fields are non-binding and cannot establish host support.
- On each supported host, a real install followed by a real saved edit and
  stop boundary produces the same authorized Plan 09 feedback where
  capabilities overlap.
- Host-specific product tests exercise each supported host's actual native
  surface: Claude Code LSP, Cursor desktop diagnostics, Kimi Code native hooks
  and installed capabilities, OpenCode plugin/LSP events and analyzer
  ownership, and hook/MCP/CLI delivery or typed unavailable behavior for hosts
  without automatic editor diagnostics.
- OpenCode conformance starts the TraceDecay custom LSP with an existing
  language analyzer present and proves exactly one analyzer owns that language
  before, during, and after install, repair, rollback, and uninstall while
  TraceDecay findings still project.
- Core-only and each optional companion install, update, repair, and uninstall
  independently. Interrupted and repeated operations converge; corrupt
  content, schema/version/capability mismatch, protocol skew, ownership
  conflicts, and partial installs fail before unsafe mutation and roll back
  only TraceDecay-owned state.
- Platform-substrate tests cover path handling, atomic writes, file modes,
  process discovery, locking, interruption boundaries, and rollback on every
  supported operating system independently of host branding.
- Selected real host/OS/fault combinations cover each distinct native
  registration mechanism and lifecycle risk. They include Kimi Code and
  OpenCode, competing ownership, interruption on both sides of the commit
  boundary, partial component failure, restart, and feedback rollback without
  requiring a Cartesian host-by-OS-by-fault matrix.
- Official install, upgrade, repair, and uninstall operations—not hand edits or
  test-only installers—verify native registration, real feedback delivery,
  runtime receipt identity, unrelated configuration preservation, and
  ownership-aware removal.
- Every Cline-family fixture proves either one real packaged hook/MCP/CLI route
  end to end or an evidence-backed typed unavailable result for the exact
  host/version; family resemblance alone never satisfies acceptance.
- A real read-only GitHub refresh ingests existing comments/threads, survives
  duplicate pages and daemon restart, preserves the last complete generation
  on partial failure, and surfaces findings without posting, updating,
  resolving, dismissing, reacting to, or replying on GitHub.
- Event-hint, overlapping incremental, and whole-root refresh paths prove
  atomic cursor/generation publication, consistency-token or matching
  double-scan behavior, delete safety, freshness/coverage reporting,
  idempotent receipts, bounded retry, quarantine, and explicit repair.
- Configuration tests prove deny precedence, scope containment, authorization
  revocation, expiry, CAS/idempotency, and no ambient host/PATH/PID/CWD
  authority.
- Rejected GitHub writes make zero network calls, and host processes/hooks
  cannot open stores, widen project scope, or become daemon writers.
- Unavailable daemon, component, host API, GitHub authorization, rate limit,
  and protocol capability remain typed and do not trigger silent fallback.
- These host-specific journeys, platform-substrate tests, selected real
  combinations, and ordinary repository checks are the evidence. Exact file
  inventories, fixed test names/counts, giant matrices, and placeholder
  benchmarks are not deliverables.

## Later callable extensions

- **PR14:** Dashboard and Doctor call the shipped host status and confirmed
  lifecycle operations. They do not redefine install/repair mechanics.
- **PR15:** A callable read-only capability probe may expose optional GitHub
  Stacked PR preview state and hand off to standard Git/forge reads when it is
  absent or degraded. It never invokes provider mutation, rebase, or
  force-push.
- **PR17:** Host packages may add the callable Plan 24 work surface and Plan 32
  native Claude Code/Codex execution adapters, including the independently
  installable Work MCP companion. Discovery reports configured executables and
  protocol capability; Plan 32 alone admits, invokes, supervises, cancels, and
  records execution. No backend substitution, ambient-`PATH` authority, or
  host-local scheduler is allowed.

  The retained PR17 host projection includes canonical worktree context,
  addressed task-event ingress/delivery, hook/LSP/native-diagnostics fanout,
  CLI fallback, Plan 36 native-integration preflight/apply availability, and
  independently reported Claude Code CLI, Codex app-server, and policy-eligible
  Codex CLI capabilities. Non-repository task context remains first-class.
  Progress or host terminal text cannot mutate Plan 24/32 or synthesize
  success; apply is exposed only for the exact Plan 20/24/32/36-approved
  operation and never through shell fallback. Rebase, amend, force update,
  force push, destructive reset, and branch deletion remain unrepresentable.

  Host packages also preserve bounded ordered skill/hint/capability
  discoverability, task guidance, workspace/artifact instructions, progress
  and cancellation presentation, requested-versus-actual backend/model/effort,
  and explicit unavailable reasons. Claude-designated work requires native
  Claude Code; Hermes Anthropic cannot substitute. Codex app-server and CLI
  remain distinct and CLI is used only under explicit pinned fallback policy.

## Safety constraints retained

- Host lifecycle mutation is explicit, ownership-aware, atomic, receipt-backed,
  reversible, and limited to TraceDecay-owned state.
- GitHub access is read-only with no mutation client or hidden write path.
- Installed adapters contain no credentials, copied product logic, or durable
  business state and never open TraceDecay stores.
- Acquisition, delivery, and expansion retain the exact authorized
  project/repository identity.
- Unsupported capabilities remain honestly unavailable; no host or backend is
  silently emulated by another.
