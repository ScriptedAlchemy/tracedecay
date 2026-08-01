# TraceDecay V2 roadmap

Status: active product rewrite. PR8 is complete. PR9/PR10 retrieval delivery
and PR12/PR13 production integration are active. PR14 has a substantial
implemented and focused-suite-verified checkpoint, but acceptance remains
blocked on the open Plan 11 journeys and on stable PR12/PR13 product contracts,
direct tests, and normal CI. The repository is not green.

This file is the sole authority for V2 precedence, rejected mechanisms,
delivery order, and acceptance. Numbered plans own semantic product behavior,
failure semantics, migrations, and direct acceptance; they are not independent
queues and do not require one crate-first pull request per document. `NEXT.md`
tracks current outcomes and blockers only. `GAP-LEDGER-PR8-PR14.md` and
`pr9/00-contract-spine.md` are historical records, not parallel authorities.

The `TraceDecay V2` roadmap name is independent of contract/schema versioning.
A predecessor proven on `origin/master`, in a published package/release, an
independently deployed client, a live host installation, or a live persisted
format requires the applicable compatibility, deprecation, reader/writer,
contract-version, or migration path. Pure source-only/internal contracts change
in place; PR sequence, branch history, tests, historical plans, and a `V1`
suffix alone do not establish publication. Wire-visible revisions remain
negotiated until an authorized installed-client/host census proves absence.
Anything potentially installed or written to a store, spool, file, or
persisted projection fail-closes as live and keeps compatibility,
backward-read, and migration/recovery until a separately authorized census
proves absence.

Core Work and the minimal Plan 24/32 runtime ship in PR14; PR17 retains
residual advanced workflow capability.
Dashboard acceptance is desktop-first, not desktop-only: desktop visual
baselines lead review while responsive, zoom, keyboard, forced-colors, and
accessibility function remain mandatory.

## Product outcome

TraceDecay V2 converges capture, sessions, memory, code intelligence, search,
policy, automation, tools, APIs, integrations, observability, and the dashboard
into one local-first Brain. Before remote delivery, one local daemon is the
physical database authority; PR16 generalizes this to exactly one fenced daemon
authority per mutable shard. Clients, hooks, MCP servers, dashboard handlers,
workers, and remote nodes use typed daemon/application operations; none opens a
fallback writable database.

## Completed foundation

PR4 delivered:

- canonical V2 domain and store boundaries;
- daemon-owned `GlobalDb` connection and transaction authority;
- atomic transcript batch, projection, cursor, and offset updates;
- restart catch-up, replay, and fail-closed project/user-store resolution;
- project-wide session/LCM storage shared across branches and worktrees;
- RAII rollback for database changes and external payload files;
- direct Claude, Cursor, Cline-like, concurrency, recovery, and Windows tests.

PR5 delivered:

- the production Claude parser through mandatory structured sanitization;
- path-independent observation, source, cursor, receipt, and payload contracts;
- atomic observation, receipt, cursor, projection-enqueue, and checkpoint state;
- deterministic projection into the existing searchable V1 session/message view;
- bounded replay, restart, duplicate, collision, partial-input, cancellation,
  stale-authority, migration, consolidation, and crash/retry coverage;
- a checked-in executed production benchmark with 30 measured repetitions and a
  verified exact no-op replay that performs no writes or durable work.

PR6 delivered:

- one complete host-neutral catalog and provider observation path for the
  supported Claude, Codex, Cursor, Hermes, Kiro, and Cline-family sources;
- bounded checksummed daemon host admission for non-replayable events, fair
  bounded scheduling for replayable sources, and typed failure/backpressure;
- atomic projection with staged bounded rebuild, provider-native identity and
  relation preservation, typed hook telemetry, and executable native host
  fixtures;
- an executable multi-provider benchmark harness and historical Linux
  measurement recorded by commit `05da230e`.

PR7 delivered the canonical project/profile memory and fact path, evidence and
provenance, corrections and trust, curation, migration, deletion lineage, and
dogfood hardening, retained by direct behavioral cargo tests (performance
baseline remains provisional/pending). PR8 delivered the shared
Session/LCM temporal-retrieval kernel, explicit refresh, stable temporal
pagination, summary lineage, and compatibility delegation. The active slice is
documented in [NEXT.md](NEXT.md).

**Delivered stabilization checkpoint (2026-07-27).** These are completed
sub-slices of the active delivery band, not acceptance of PR9–PR14:

- daemon shutdown cancellation now reaches startup transcript discovery,
  provider ingest, projection drain, and code-index reconciliation, and
  cancelled startup ingest does not run finalization or downstream backfill;
- configuration startup forward-repair materializes newly registered defaults
  into older stored snapshots, and the production configuration client now
  performs exact-project mutations, republishes the pinned snapshot, and records
  runtime activation;
- the dogfood path classifies terminal corruption/authority failures separately
  from retryable convergence, scopes convergence to the active project store,
  reuses a source-stamped dashboard build when inputs are unchanged, and reports
  stage timings;
- foreground code search serves the prior complete immutable generation without
  waiting for an in-flight refresh, and bundled-SQLite FTS blob corruption has a
  direct open-and-self-heal regression;
- the Memory V2 owner archive covers all 33 authoritative families with
  physical-adapter parity, referential closure, digest-bound cutover receipts,
  idempotent import, and public-read/cutover regressions; and
- the dashboard retains the daemon's production invocation executor, its
  daemon-hosted Settings mutation is directly tested, and its focused backend
  suite executes the diagnostics and workspace routes recorded below.

The same day also landed focused repairs for scope drift, temporal migration
atomicity, runtime authority fixtures/cleanup, host install coverage, generated
contracts, source-contract reachability, and dead-code result bounds. Those
clusters reduce known failures; they are not evidence of a completed full
suite.

**Open operational evidence (owner recorded, 2026-07-27).**

- `cargo dogfood` still does not exit successfully. Doctor currently reports
  `authority_audit_unavailable`, and Cursor Core has a component-ownership
  conflict. Plan 09 owns Doctor composition; Plan 27 and the PR12/PR13
  integration slice own host lifecycle/ownership repair. The Plan 27 host
  capability/lifecycle reachability guards closed on 2026-07-29 (recorded in
  [`GAP-LEDGER-PR8-PR14.md`](GAP-LEDGER-PR8-PR14.md) and
  [Plan 27](27-cross-host-agent-plugin-bundles.md)) do not close this Cursor
  Core ownership conflict, which remains open.
- Semantic search is disabled by an invalid configuration snapshot. Plan 20
  owns snapshot validity and forward repair; Plan 31 owns semantic activation.
  Exact, lexical, and graph retrieval remain the required available fallback.
- A live profile was observed 237 minutes stale (the index later reported
  285 minutes stale while this reconciliation was being checked). Plan 25 and
  the active incremental-indexing slice own cadence/freshness diagnosis; the
  new serve-during-refresh behavior does not close that issue.
- Repository tests and normal CI remain incomplete or failing. Focused local
  success does not establish PR9–PR14 acceptance, and unexecuted, skipped,
  empty-filter, or partial coverage must remain unresolved.
- Historical deleted-test and vacuous-verification incidents are summarized in
  `GAP-LEDGER-PR8-PR14.md`. Restored test names, counts, commit groupings, and
  CI chronology are historical evidence rather than roadmap requirements. The
  remaining product-relevant gap is direct generation rebuild after reopen.

Completed-slice names are historical implementation evidence, not instructions
to recreate a type, file layout, fixture filename, milestone, or gate. A deleted
or renamed mechanism does not mean the feature is missing. Later audits must
first map every retained product, semantic, migration, and recovery requirement
to its current canonical owner and direct behavior/regression evidence; only
missing callable behavior or a missing direct regression is a product gap.
Removed planning/evidence machinery is not unfinished product work and must not
be rebuilt.

That rule protects deleted scaffolds, not deleted assertions. A direct test that
covered retained shipping behavior is exactly the "missing direct regression"
case above, so the 2026-07-24 deletions were a restoration backlog against
known-good prior coverage, worked off on 2026-07-28. Restoring them changed no
delivered claim and authorized no reimplementation of the behavior they
asserted. The converse also holds and cost this audit one wrong finding: a
deleted test *path* is not a deleted assertion, so establish that coverage did
not simply move before filing it as lost.

## Delivery rules and practical safety baseline

Every remaining PR must leave a supported user journey working through a real
entry point, the daemon/application kernel, durable state or computation, and a
visible result. Prerequisite contracts ship inside the first journey that uses
them. Compatibility names delegate to the production kernel, and replaced paths
are deleted after the stated recovery boundary. Direct behavior, focused
failure/recovery tests, and normal CI are the evidence.

**Authoritative roadmap acceptance rule.** Never create or recreate
PR-specific acceptance snapshots, owner receipts, gate manifests,
clean/content-addressed checkout snapshots, signatures, attestations,
reveal/trust-root evidence, or giant gate scaffolds. Acceptance is direct
product tests plus simple Linux-only developer benchmarks/evals and truthful
pass/fail/pending summaries; normal Linux/macOS/Windows CI continues to support
the product's default features. This rule does not remove product-runtime
receipts for atomic effects, migrations, Git transactions, daemon operations,
or rollback, nor immutable code/vector/session generation identity and real
source/content digests.

The plan set has one minimal baseline. Numbered plans attach these checks to
the product operation that can actually fail; they do not create separate
privacy/security matrices, proof packets, attestations, recheck rituals, or
acceptance gates:

- Logs, telemetry, errors, and checked-in fixtures contain no credentials,
  prompts, private source, provider payloads, or equivalent sensitive content.
  Capture sanitizes before persistence.
- Every read, write, continuation, and expansion uses the exact authorized
  `ProjectId` or `UserProfileId`; paths, labels, CWD, and collection membership
  never substitute for identity or widen scope.
- Actual remote or network boundaries authenticate the caller and authority.
  PR16 additionally fences every mutable shard to one current daemon writer.
- Destructive Git, host-registration, and protected-configuration operations
  require an explicit preview/confirmation, stale-state compare-and-swap, a
  durable result, and rollback or forward recovery appropriate to the real
  commit boundary.

Migration, compatibility, recovery, and truthful partial/unavailable behavior
remain direct product requirements. Git never rewrites published history,
resolves semantic conflicts, or performs autonomous branch/ref mutation.

## Rejected and superseded decisions

This register is normative. A historical mention of one of these mechanisms is
evidence that it was considered, not permission to rebuild it. Each entry
records the rejected mechanism, the reason, and the retained replacement:

1. **libSQL as the local runtime is superseded.** Its compatibility driver and
   local runtime were removed after the rusqlite cutover. The
   `tracedecay-rusqlite-runtime` path and daemon-owned SQLite authority replace
   it; future remote work composes over that authority rather than reviving a
   libSQL runtime.
2. **Octocrab, `backon`, and `graphql-parser` are rejected for PR13.** They add
   provider-client, retry, and parser abstractions that the one narrow
   read-only GitHub/CI path does not need. Existing `ureq`, shared narrow typed
   Serde DTOs, one compile-time static audited GraphQL query, and explicit
   owner-local bounded retry are the runtime; `gh api` is manual
   troubleshooting only.
3. **PR-specific acceptance machinery is rejected.** Acceptance snapshots,
   owner receipts, gate manifests, clean/content-addressed checkout snapshots,
   signatures, attestations, reveal/trust-root evidence, and giant gate
   scaffolds were planning bureaucracy rather than product evidence. The
   authoritative acceptance rule above replaces them with direct product
   tests, simple Linux developer benchmarks/evals, truthful
   pass/fail/pending summaries, and normal cross-platform CI. Product-runtime
   receipts, authorized repository/worktree/state snapshots, immutable
   generations, and real content/source digests remain required.
4. **Local first-party signing, trust-root, and attestation systems are
   rejected.** No concrete local boundary requires a second origin or release
   authority. Actual remote/network boundaries still authenticate, native Git
   signing policy remains native Git's concern, and content digests may detect
   corruption without becoming signatures or attestations.
5. **The PR14/PR17 Work allocation is plan authority, not a user rejection.**
   The user did not enumerate the twelve PR14 workspaces and asked "ahat about
   kanban/task graph etc" on 2026-07-25 without a recorded answer accepting
   deferral. The written plan currently assigns Brain, Explorer, Loom, Sessions,
   Agents, Code, Knowledge, Delivery, Automations, Observatory, Costs, and
   Settings to PR14, while Plan 24 owns the persistent task/work graph and PR17
   adds Work. That allocation remains binding plan authority pending the open
   scheduling decision recorded in Plans 11/11c; it must never be attributed to
   the user. An independent or session-derived Kanban authority remains
   architecturally rejected regardless of delivery timing.
6. **The Cargo shim and `cargo-slot` are rejected.** The earlier local build
   shim was removed by explicit direction and is not product, contributor, CI,
   or release architecture. Stock Cargo behavior and portable repository
   configuration supersede it.
7. **Six separate GitHub pull requests for PR8–PR13 are superseded.** Draft PR
   #421 is the consolidated delivery vehicle. PR8–PR13 names remain useful
   product-slice and ownership labels, but they are not instructions to split
   the work back into six branches or pull requests.
8. **Synthetic or lookalike provider/observation fixtures are rejected as
   acceptance evidence.** Invented protocol fields can agree with invented
   adapters while proving no provider behavior. Only checked-in real native
   fixtures with recorded origin/version/digest, replayed through the real
   sanitizer and consuming path, are binding; synthetic data may exercise
   isolated non-provider value contracts but cannot satisfy provider or
   observation acceptance.
9. **Custom infrastructure is rejected where a mature maintained library or
   existing TraceDecay authority already owns the mechanism.** Do not add
   bespoke parsers, cursors, caches, retries, transports, or registries that
   duplicate those owners. Custom code is limited to TraceDecay-specific
   authorization, scope, and composition. A design-approved, measured product
   renderer used because admitted libraries do not satisfy the visual contract
   is not an alternate infrastructure authority.
10. **A synthetic Doctor remediation dispatcher is rejected.** The attempted
    lane lacked the operation owner, authorization, preview/confirmation,
    compare-and-swap, effect boundary, receipt, and rollback/recovery details
    needed to dispatch safely. Plan 09 composes only owner-supplied legal
    remediation operations; missing authority returns typed unavailable and
    remains with the named owning plan rather than being filled by a generic
    dispatcher.

### Frontend rejection record

The following entries come from the authoritative 2026-07-25 history brief.
Quoted text is user speech; plan/design consequences are labeled separately.

1. **Module Federation is rejected for the TraceDecay dashboard.** The user
   said it "shouldn't be using Federation" there. The replacement is one
   ordinary Rsbuild application and React tree.
2. **Vite and bundler-ADR ceremony are rejected.** The exact instruction was
   "use rsbuild. no adr. just pick rsbuild." Rsbuild is settled; do not reopen a
   Vite comparison or write an ADR to restate the choice.
3. **The pre-PR14 dashboard is rejected as an implementation base.** The user
   said to "gut the existing dashboard" for a fresh, industry-leading
   implementation. Retained API compatibility is not permission to restore its
   frontend composition or visual language.
4. **Foundation lanes styling or structuring the product are rejected.** The
   historical instruction was "no styling or strucutring" for foundation
   models. The named model has since been superseded, but one designated design
   owner still owns styling, layout, and dependency selection.
5. **Git-hash-tied record documents are rejected.** The user explicitly did
   not want records "all iver the place tied to git hashes" because they waste
   time and cause agents to redo work after commits. Never create per-commit
   acceptance/evaluation documents, screenshot-record manifests, or evidence
   packets. Direct tests, real-Chrome review, ordinary run output, and truthful
   status in the owning plan replace them. A landed commit identifier may
   annotate one authoritative implementation checkpoint; it does not create a
   separate record or establish acceptance.
6. **A dashboard that does not look world class is rejected.** Beauty is a hard
   acceptance criterion: "its importsnt that it looks really beautiful and
   functional" and "we wanna overhaul anything that isnt magnificent and
   beautful." Function and beauty are simultaneous requirements on every page.
7. **A generic, clinical, simple visual language is rejected.** This is the
   user's named failure mode, not permission to invent a specific palette,
   typeface, spacing system, or motion language on his behalf.
8. **Bottom-panel chrome that steals graph space is rejected.** Controls and
   evidence remain available without crowding the primary interactive field.
9. **shadcn adoption is rejected for now.** The current instruction is "dont
   use shadcn yet" and "just leave it for now"; this is a delivery-first hold,
   not a permanent ban or a ban on investigating compatibility.
10. **The sparse circular single-project Brain is rejected.** The circle
    communicated no readable property and the selected-project view was too
    sparse. Geometry must encode a named real measurement.
11. **A Brain with no visible live neurons after real dogfooding is rejected.**
    Real activity should visibly fire when agents work; missing activity must
    remain truthful rather than simulated.
12. **Bland vertical lists that consume large screen area are rejected.**
    Lists remain accessible equivalents where needed, but cannot be the bland,
    dominant visual treatment.
13. **"Bland UML" is rejected as the structural idiom.** Structural views must
    connect symbols, files, functions, callers/callees, sessions, facts, and
    surrounding types in a comprehensible field.
14. **Call chains drawn as service boxes are rejected.** They must show
    function/type-level callers and callees, not resemble a service-architecture
    diagram.
15. **The embedded/in-IDE browser is rejected as the visual verification
    surface.** Its viewport is too small. Use real Google Chrome, screenshot
    every page, and manually click through every interaction state.
16. **Anything not magnificent on any page is rejected.** The quality bar
    applies across the dashboard, not only to hero graph surfaces.
17. **Falsified UI is rejected categorically.** "All data must be fully wired
    through to the frontend with no falsified ui"; missing backend behavior is
    implementation work or a truthful typed unavailable state, never a
    plausible zero, fake health, or decorative activity.
18. **Vendored/forked Tailwind is an unconfirmed adjacent-workspace
    inference, not a TraceDecay user rule.** The cited rejection came from a
    bundler-benchmark workspace. TraceDecay may prefer upstream maintained
    integration under the general library-first rule, but no plan may quote the
    user as rejecting vendored Tailwind here without new TraceDecay-specific
    evidence.

**User-stated graph benchmark.** "Look at cosmograph.app i want visuals like
that. relaly beautiful" sets the visual benchmark. It does not select or require
the Cosmograph library; licensing, offline, accessibility, and product-authority
constraints still govern implementation.

**Unresolved desktop/responsive contradiction (owner: user/product owner).** On
2026-07-06 the user said "desktop resolution only please." On 2026-07-25 he
filed the hidden-below-1024px symbol-search bug and required verification at
320/768 widths with axe violations at zero. Neither instruction is silently
discarded. Until the owner decides, the working interpretation is desktop-sized
screenshots in real Chrome while functionality remains present below `lg`; this
is provisional, not a settled product-scope decision.

**Unattributed design axes.** The complete history contains no user statement
selecting typography, colour palette, dark/light preference, spacing scale,
motion, or easing. Specific choices on those axes are design-owner/agent plan
decisions and must never be presented as user preferences. The user's
"kinestetic synastisya" direction was immediately hedged with "i dont wuite
know but you get the idea"; it and "topography of the code base" are
impressionistic direction, not a literal visual or motion specification.

## Retained semantic ownership

These ownership rules prevent duplicate product behavior; they are not separate
delivery phases:

- Plan 01 owns external-source definition/binding identity; Plan 20 owns
  configuration; Plan 06 evaluates authorization; Plans 03/27 sanitize capture;
  and Plan 09 orchestrates effects. Definitions and connectors never become
  alternate identity or persistence authorities.
- Plan 05 owns shared query execution. Plan 23 owns temporal session/LCM truth
  and session-derived evidence spans; Plan 13 owns cross-domain
  `EvidenceSpanRecordV1` anchors. Plan 15 owns retrieval/quantifier evaluation,
  Plan 25 exact code generations and typed graph evidence, and Plan
  31 the optional semantic representation/search profile; exact lexical
  inclusion remains authoritative before semantic augmentation. Plan 16 owns
  authorized `QueryCollection` / `WorkspaceCollection` identity, membership,
  scope digests, and snapshot vectors; membership never grants ownership or
  authorization. Plan 24 references evidence IDs and anchors without copying
  their authority.
- Plan 35 owns LSP projection, including the versioned TraceDecay context
  extension carried over standard LSP/JSON-RPC framing, and short-lived
  one-way investigation and task-cue handoff projections; Plan 17 freezes the
  two public token-consumption operations and their Rust/TypeScript
  bindings; Plan 08 catalogs only typed, negotiated callable bindings; Plan 09
  owns transport-neutral investigation results; Plan 24 owns task identity,
  ready-commit, cross-worktree, and task-context semantics; Plan 21 binds
  supported surfaces; Plan 37 owns central advisory fanout and read-only
  GitHub stack/review snapshots; and Plan 11 renders those results without
  backend truth. None creates a second diagnostic store, provider contract,
  suggestion channel, task authority, or executor.
- Plan 09 implements and composes the one Doctor application use case and its
  legal-remediation handoffs; Plan 14 owns the historical regression and
  observable-behavior contract; Plan 11 renders only. Plan 20 alone owns
  configuration definitions, precedence, snapshots, behavior/provenance
  digests, activation, and audit. Plan 26 alone owns measurement descriptors,
  cohorts, labels, calibration/drift observations, and denominator-safe read
  models.
- Plan 24 owns task/work graph state, ready-node/decomposition/sizing/model-
  backend recommendation semantics, proposals, and auxiliary requests; Plan 06
  owns pure policy evaluation; Plan 26 owns task/model and retrieval/synthesis
  observations; Plan 32 alone owns synthesis attempts, workflow clocks,
  provider execution, scheduling, history, leases, attempts, effects, retries,
  cancellation, artifacts, and runtime receipts; and Plan 36 alone owns native
  Git preflight/apply/receipt mechanics.

## Authoritative PR sequence through PR12

| PR | Product delivery |
|---|---|
| PR5 (complete) | Sanitized observation vertical: one real provider from parse through sanitizer, daemon-owned persistence, replay, and restart. |
| PR6 (complete) | Provider coverage and event normalization: remaining hosts/sources, daemon host-admission spool for non-replayable events, identities, dedupe, partial input, backpressure, and canonical event relations. |
| PR7 (complete) | Memory, facts, and provenance: project/profile ownership, evidence, corrections, trust, curation, migration, deletion lineage, and generation-bound repository provenance anchors. |
| PR8 (complete) | Session/LCM temporal retrieval: occurrences, copies, summaries, supersession, current/as-of/evolution retrieval, stable context assembly, and explicit daemon-owned refresh. |
| PR9 (delivery active) | Code intelligence and lexical retrieval: deterministic extraction with typed edge authority and coverage, exact occurrence identity plus evidenced/abstaining lineage, generation-bound managed diagnostics/tests, a non-demotable exact/phrase/BM25 tier, typed quantifier inputs, V1 parity, and typed read-only Git status/diff/history/blame/hunk intelligence enriched by graph impact. Worktree-aware incremental indexing reuses content-addressed parse/chunk artifacts while retaining exact worktree and generation identity. |
| PR10 (delivery active) | Native semantic retrieval and ranking: local FastEmbed artifacts, immutable vector generations, exact flat-vector baseline/oracle, measured hybrid/reranking candidates, calibrated abstention, redundancy augmentation, and byte-stable lexical fallback. Semantic projection is asynchronous and batches only changed eligible chunks; ordinary search never waits for it. ANN, late interaction, and quantization remain optional measured candidates. |
| PR11 (integration active) | Policy, application, catalog, and configuration core: typed use cases, grants, routing, replay, operations, capabilities, analyzer policy/settings, one runtime configuration authority, daemon-serialized `stage_hunks`/`unstage_hunks`/`commit_index` transactions with `HunkRef` compare-and-swap and receipts, and the typed branch-aware feedback-cycle request/result and orchestration ([Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)) — first pillar of the PR11–PR13 read-only/advisory milestone (post-edit diagnostics and impact). |
| PR12 (integration active) | CLI, MCP, HTTP API, LSP gateway, and output convergence: one revisioned schema authority, dispatcher, binding taxonomy, semantic problem model, capability intersection, and executable lifecycle/stream/cancellation contract; stable errors/cursors, compact Markdown, canonical JSON, managed diagnostics, semantic surface parity, shared Git preview/apply bindings, callable canonical feedback diagnostics/impact reads with HTTP parity, and [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)'s PR12 slice — [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md)'s standard projections plus a negotiated, versioned TraceDecay LSP context extension for diagnostics, impact, affected tests, and test results, and the explicit diagnostics-call trigger/surface bound once through [Plan 21](21-cli-mcp-tool-surface-and-output-unification.md) — completing the post-edit diagnostics-and-impact pillar for LSP/MCP/CLI/HTTP surfaces. Dashboard binding starts in PR14. |

PR9 and later code consumers share one incremental-indexing rule: host
after-file-edit hooks are the primary change hint (with an off-by-default
opt-in filesystem watcher as a non-agent-driven fallback) and only wake bounded
work; native `gix` status/index/tree reconciliation, driven lazily by a
three-tier freshness ladder (per-query `.git` metadata fingerprint, configurable
bounded-staleness threshold, identity re-resolution backstop), defines the
changed path set; Tree-sitter `InputEdit` plus `changed_ranges`
narrows warm parsing; content and descriptor digests suppress no-ops; immutable
worktree generations keep identity exact; and only changed chunks and evidenced
dependency closures reach lexical or FastEmbed projection. Partial generations
never publish, and exact/lexical/graph search remains available while semantic
projection is indexing.

## PR13 — feedback in every supported host

**User outcome.** After an edit or stop event, a user receives TraceDecay
diagnostics and impact plus read-only CI failure localization, GitHub review
threads, and concurrent-agent proximity in the supported host surface they
already use.

**End-to-end production path.** Bounded hook or host events enter the daemon
and synchronously return only a receipt or already-ready guidance. Context
Scout, model work, feedback refresh, and GitHub/CI/proximity acquisition run
asynchronously; the branch-aware feedback cycle resolves generation-bound
evidence, and one semantic result is projected through Claude Code LSP,
Cursor desktop native diagnostics without duplicate analyzers, OpenCode's
custom LSP configuration without starting a second analyzer for the same
language, or the supported hook/MCP/CLI capability path for Cursor cloud,
Codex, Hermes, Kiro, Kimi Code, OpenCode, Cline-family, and other admitted
hosts. PR13 adds producer contributions to the PR12 feedback readers without
changing their transport.

**Implementation and deletion.**

- Ship asynchronous suggestions, host capability detection, install/upgrade/
  repair, and stock-host conformance as part of this callable path, including
  Kimi Code manifest/global `PostToolUse` and `Stop` hooks plus MCP and
  skills/commands, and OpenCode local JS/TS plugin events, custom LSP
  configuration, MCP, custom agents/skills, and commands (prompt templates).
- Keep GitHub review access read-only: never post, update, resolve, reply to,
  dismiss, or fabricate review content.
- Remove provider-local feedback logic and duplicate diagnostic/suggestion
  routes once each host delegates to the production cycle.

**Library-first implementation defaults.** Keep the shipped PR12 LSP gateway
and protocol structs; add no otherwise-unused `lsp-types` dependency.
`async-lsp` remains a later candidate only if a measured conversion deletes
gateway code and leaves one `lsp-types` version. GitHub/CI acquisition uses
existing `ureq`, shared narrow typed Serde DTOs, and one compile-time static
audited allowlisted GraphQL query; `gh api` is the manual troubleshooting
fallback, not a runtime
dependency. This replaces new provider clients, dynamic GraphQL parsing, and
source-local JSON models while retaining read-only methods, exact provider
identity, pagination, freshness, coverage, rate-limit, and typed failure
states; schema drift or missing required fields yields typed unavailable or
the manual fallback, never Octocrab, `backon`, or `graphql-parser`. Context
Scout reuses existing provider adapters before admitting `genai`; `genai` must
delete multiple transports without AWS-LC/Reqwest duplication and pass
cancellation, schema, and cost fixtures, otherwise the adapters and
deterministic Scout remain. Hermes configuration uses `yaml-edit`, supported
hosts use their official SDK/plugin APIs, and all hosts retain the existing
atomic-write/lifecycle kernel; an unproven host API or lossless edit blocks
mutation and reports typed unavailable instead of falling back to bespoke
text rewriting.

**Direct acceptance.** Exercise real edit/stop events in each supported host,
including Kimi Code `PostToolUse`/`Stop` and OpenCode `file.edited`,
`tool.execute.after`, `session.idle`/`session.status`, and LSP event paths;
refresh real checked-in CI/review provider fixtures; verify exact evidence and
proximity tiers; and prove bounded backpressure, restart, OpenCode
custom-LSP duplicate-analyzer avoidance, and truthful provider-unavailable
behavior,
including an explicit Cline-family route or evidence-backed typed unavailable
result. Dogfood the official install/upgrade/repair/uninstall flow for Kimi
Code and OpenCode; prove competing-extension, interrupted-lifecycle,
host-by-host rollback, and the direct feedback rollback switch. Normal
Linux/macOS/Windows CI covers supported-host default-feature compatibility.

**Not in this PR.** Dashboard investigation belongs to PR14; multi-root scope to
PR15; remote delivery to PR16; workflow composition to PR17.

## PR14 — an operable flagship product

**User outcome.** From one dashboard, a user can inspect the Brain, move from a
finding to exact evidence, understand Doctor status, change supported settings,
inspect truthful Observatory/Costs data, and execute a legal remediation.

**End-to-end production path.** The shared dashboard shell and Brain/Explorer/
Loom investigation surfaces request canonical application results, progressively
disclose anchored diagnostics, CI/review/proximity evidence and health, and send
authorized configuration or remediation commands back through the daemon.

**Implementation and deletion.**

- Ship exactly the original twelve dashboard workspaces — Brain, Explorer,
  Loom, Sessions, Agents, Code, Knowledge, Delivery, Automations, Observatory,
  Costs, and Settings — plus Work as the thirteenth PR14 workspace, with
  renderer-neutral semantics, a permissive default
  renderer, keyboard/accessibility parity, typed SLOs, and denominator-safe
  measurements with provenance, coverage, cohort, temporal delta, uncertainty,
  and calibration validity. Work renders the PR14 core Plan 24 graph and
  minimal Plan 32 runtime; advanced workflow capability remains PR17.
- Plan 09 remains the sole Doctor use-case implementation/composition
  authority, Plan 14 its historical regression/behavior contract, and Plan 26
  the measurement authority; the Plan 11 UI renders supplied results and never
  computes a second grade, readiness score, remediation policy, or backend
  truth.
- Optional GPU or commercial adapters may draw or accelerate only; they never
  own graph, query, storage, health, readiness, scheduling, ranking, or
  remediation semantics.
- Delete dashboard-local health, configuration, and action logic replaced by
  canonical application operations.

**Implementation architecture.** Finalized 2026-07-23 in
[Plan 11](11-dashboard-frontend.md) §"Finalized implementation architecture"
(fresh single-app rebuild on Rsbuild — decided, no ADR; React Router, TanStack
Query, bounded Zustand, Zod over one generated contracts module, Radix +
Tailwind v4 semantic tokens, TanStack Virtual, `d3-force` default graph
adapter, ECharts as the single charting library, SSE monotone reducer). The
legacy multi-bundle dashboard was removed the same day; Plan 11 is the single
authority for frontend structure, styling, and dependency decisions.

**Implementation checkpoint (2026-07-25; not acceptance).** The real
`app-dist` application is served at `/` and the legacy placeholder is isolated
at `/legacy`; formerly claimed unsupported Settings capabilities are
served by the existing backend and the UI now renders real capability and
authority-failure state; storage budget findings and unreadable storage roles
are preserved; graph failures render truthful partial/unverified state with
discriminated registry outcomes, scoped failures no longer masquerade as
`not mounted`, and Agents, Costs, Knowledge, and Sessions preserve unavailable
reads; Explorer owns its coordinator and source-local query binding, LCM
size/read-context support and direct accessibility coverage; Loom has explicit
time boundaries; and the Delivery, Explorer, Doctor,
storage telemetry, Loom, asset serving, and feedback-observation paths are
implemented.

**Verification correction (2026-07-27).** Direct backend and frontend coverage
now reaches Settings CAS, Delivery, Explorer routes, Doctor, storage telemetry,
Loom, asset serving, and all dashboard workspaces. The daemon-hosted dashboard
uses the production invocation executor; an unavailable control plane remains
typed unavailable, while a real daemon-hosted Settings mutation proves apply,
durable reread, and stale-revision rejection. Exact test names and counts are
run output, not roadmap status.

Acceptance remains open on the Plan 11 renderer parity/fallback behavior,
real-Chrome visual review, manual assistive-technology completion, and the
usability study. The Plan 11 performance and payload budgets and the
sustained-update rates were withdrawn by owner decision 2026-07-31; Plan 11
records the withdrawal.

**Direct acceptance.** Starting from a real PR13 finding, navigate to retained
evidence, diagnose an injected operational fault, apply an authorized
remediation or setting change, observe the resulting state, and verify
accessibility, cancellation, restart, denied-action, partial-data, and
unavailable-provider behavior. Run those direct tests and normal repository CI;
do not create a separate dashboard/Doctor acceptance gate.

**Not in this PR.** Multi-root operation belongs to PR15. Workflow definition
lifecycle, advanced placement, fan-out/synthesis/recovery, expertise and
calibration, automation execution controls, and host/LSP task handoff belong
to PR17.

## PR15 — authorized multi-root local work

**User outcome.** A user can query, inspect diagnostics, and act across an
authorized set of repositories and worktrees without losing scope, evidence,
coverage, or Git safety.

**End-to-end production path.** An explicit scope selection resolves stable
project/repository/worktree identities, freezes a scope digest and per-shard
snapshot/continuation vector, executes deterministic graph/query/LSP federation, returns
globally routable anchored evidence with typed coverage and rank fallback, and
fans changes out from the daemon. An explicit Git request follows
preflight/preview/apply/receipt through the same authorized scope.

**Implementation and deletion.**

- Include native worktree and local-stack inventory without treating paths as
  identity, plus clean, no-conflict, policy-approved fast-forward, two-parent
  merge, and exact cherry-pick operations.
- The optional private-preview GitHub Stacked PR reader reports
  `Unavailable | PrivatePreviewDisabled | Enabled | Degraded`; standard Git,
  another forge, and no-Git work remain fully supported.
- TraceDecay never invokes an automatic rebase, force-push, semantic-conflict
  resolution, or GitHub cascading stack rewrite.
- Delete CWD inference, path identity, client-side fanout, mutable pagination,
  and any Git operation that bypasses canonical preflight and receipts.

**Library-first implementation defaults.** Retain existing `gix`, `notify`,
and Tokio for Git object/ref intelligence, filesystem observation, and bounded
async work; use `petgraph` only for branch-stack DAG/SCC mechanics. This
replaces new Git parsers, watcher loops, async coordination, and bespoke
topological/SCC code while retaining TraceDecay's stable identities, frozen
scope, exact native snapshots, preflight/apply separation, compare-and-swap,
journaling, and receipts. If `gix` or the fixed native plumbing cannot prove a
state, keep it read-only or block apply; do not add `git2`, and do not let
`petgraph` become Git or authorization authority.

**Direct acceptance.** Query and diagnose across multiple authorized roots,
resume against frozen state, follow evidence to its owning root, reject scope
or watermark drift, then preview and perform each legal clean Git operation.
Verify unauthorized roots, conflicts, partial shards, unavailable private
preview, restart, and receipt recovery through direct multi-root and Git-safety
tests plus normal repository CI, without a separate acceptance gate.

**Not in this PR.** Remote authority belongs to PR16; task execution topology
belongs to PR17.

## PR16 — a remote shared Brain

**User outcome.** Enrolled machines can capture while offline, reconnect and
sync, query shared state, back up and restore it, and fail over without creating
multiple mutable authorities.

**End-to-end production path.** An enrolled node records sanitized offline
capture, authenticates to the current fenced authority on reconnect, and sends
duplicate-tolerant batches to the fenced shard authority. The sink admits each
effect once, publishes receipts and verified cache/replica state, serves remote
queries and node-local LSP overlays, and supports staged backup, restore, and
failover under a higher fence.

**Implementation and deletion.**

- Preserve deletion replay, Git correlation, analyzer policy, gap
  evidence, and recovery state across backup, restore, and authority transfer.
- Enforce the current fence at every durable mutation and publication sink;
  replicas and caches never turn admission into authority.
- Immutable capture or replicas may improve integrity and reads, but CRDT,
  wall-clock, multi-primary, or replicated-SQLite convergence never owns
  canonical mutation.
- Delete provisional remote writers, unfenced publication, and caches that
  cannot identify their generation and current authority.

**Library-first implementation defaults.** Build on the existing HTTP/SSE,
rustls, and daemon-owned rusqlite runtime paths. At a concrete delivery
integration, consider
`reqwest` plus `eventsource-stream`, `tokio-util`, `zstd`/`tar`,
`object_store`, or Hickory only when the dependency deletes identified stream,
cancellation, compression/archive, object-backend, or discovery mechanics and
passes compile-time and resource admission. TraceDecay's authentication,
revocation, fencing, single-writer admission, replay identity, coverage,
backup/restore verification, and failover semantics remain above those
libraries. If admission fails or the integration is not yet concrete, retain
the existing path or report the capability unavailable; do not add
HMAC/attestation layers or speculative transport abstractions.

**Direct acceptance.** Capture offline, reconnect with duplicates and gaps,
observe exactly-once admitted effects, query from another enrolled node, then
back up, restore, and fail over while stale epochs are rejected. Verify
partition/restart recovery, revoked enrollment, authenticated manifests,
deletion replay, and unavailable authority through direct remote durability
journeys and normal repository checks.

**Not in this PR.** Multi-primary mutation and automatic conflict convergence
are not product paths. Executable work orchestration belongs to PR17.

## PR17 — residual advanced workflows

**User outcome.** Building on PR14's canonical work graph, core projections,
and minimal real provider runtime, a user can define and activate advanced
workflows, execute bounded fan-out/review/synthesis/recovery, inspect advanced
placement and calibration, control execution from Automations, and hand tasks
off through supported hosts and LSP without creating a second authority.

**End-to-end production path.** A typed task request becomes daemon-owned,
deterministically replayable versioned task/ticket DAG state and `TaskId`-rooted
losslessly expandable evidence. Compact context drills through Plan 23
narrative retrieval, Plan 13 anchors, Plan 25 code generations, and the owning
Git, CI, diagnostic, review, artifact, and runtime stores; summaries never
replace exact evidence or widen authority. The product presents Kanban, DAG,
timeline, causal, and workload views plus calibrated task-shape, decomposition,
sizing, routing, handoff, and repair proposals. Only an explicitly admitted
step enters the daemon-owned workflow runtime, which negotiates a provider,
schedules and leases an attempt, records effects, artifacts, cancellation/
retry, review, outcome, and calibration, and returns a non-auto-applied replan.

**Implementation and deletion.**

- Preserve evidence/history relations, governed experience recall, selective
  escalation, isolated independent review, minimal repair, graph-native typed
  auxiliary-attempt requests, model-capability profiles, and explanations with
  raw values, provenance, coverage, uncertainty, and calibration validity.
- Canonical task retrieval rejects demonstrated expertise. Authorized expertise
  context may appear only in an ephemeral interactive view and never enters
  durable evidence, completion, routing, or product truth.
- `TaskId` and Work do not require Git, worktrees, branches, PRs, GitHub, or
  stacks. Execution placement, branch topology, review topology, and
  integration strategy remain independent: no-Git tasks, unbranched,
  independent, or locally stacked worktrees without PRs, and PR stacks without
  managed worktrees all remain valid.
- Claude-designated work uses native Claude Code CLI, never Hermes Anthropic.
  Codex app-server is preferred; an explicit policy/configuration-bounded Codex
  CLI fallback is reported rather than hidden.
- Workflow steps may consume PR13–PR16 advisory and Git operations, but do not
  reacquire first availability, native Git authority, or GitHub review-content
  writes. Runtime effect/audit receipts orchestrate admitted Git effects without
  replacing Plan 36 preflight/apply/receipt semantics.
- Delete task-local schedulers, provider execution paths, private evidence
  copies, automatic graph mutation, alternate workflow clocks or receipts,
  task-specific databases/projector runtimes, board query DSLs, and universal
  query ASTs.

**Library-first implementation defaults.** Use `petgraph` for task/workflow
DAG and SCC mechanics, `tokio-util` for cancellation, existing Tokio timers
and `DelayQueue` only for mechanical waiting, Serde plus
`schemars`/`jsonschema` for definition validation, rusqlite transactions for
atomic claims/effects/publication, `process-wrap` for admitted child-process
containment, and `d3-dag` for dashboard layout. These replace bespoke graph
algorithms, cancellation tokens, waiting queues, schema walkers, transaction
choreography, process-tree handling, and DAG layout while retaining Plan 24
task semantics, explicit admission, one runtime clock, cumulative budgets,
leases/fences, exact provider identity, effect receipts, and non-auto-applied
replans. Retry eligibility, attempts, caps, jitter, and receipts remain
TraceDecay-owned. Reject a library when it requires a second scheduler, state
machine, journal, or authority model; add no workflow platform, PTY layer,
`statig`, retry library, or outbox framework, and keep the existing atomic
outbox publication semantics.

**Direct acceptance.** Create and evolve work, drill from compact context to
exact evidence, review decomposition/sizing/model proposals, admit steps against
real supported provider adapters, inspect progress/effects/artifacts/outcomes,
exercise cancellation/retry and independent review, and accept or reject a
replan. Cover no-Git and Git-backed placements, provider unavailable/fallback,
lease loss, restart/replay, and exact project scope through direct task/runtime
journeys and normal repository checks.

**Not in this PR.** Public SDK commitments belong to PR18. Suggestions never
silently choose a model, mutate the graph, or execute an unadmitted step.

## PR18 — supported external development

**User outcome.** Rust and TypeScript users can perform every
supported public TraceDecay operation—not only PR17 graph,
task-intelligence, and workflow additions—through first-party SDKs with the
same behavior and lifecycle as built-in surfaces.

**End-to-end production path.** Revisioned published names, schemas, and
OpenAPI bind each SDK to the production API; clients exercise representative
operations from every supported public family plus the complete PR17 work
loop, handle paging/streams/cancellation/problems/receipts and resume after
disconnect through the same application/runtime behavior as built-in
surfaces. Negotiated Plan 35 actions may consume either PR18 single-use public
handoff token: feedback/diagnostic cues open the owning investigation
operation, while ready-commit/cross-worktree/task cues open the owning Plan 24
task operation. Both delegate through their owning application operation,
reauthorize exact scope, and never carry or apply an edit.

**Implementation and deletion.**

- Freeze names and schemas only after the operations are accepted, and test
  oldest-supported and current clients for structural, semantic, and lifecycle
  compatibility.
- Keep authorization, scoring, policy, query, retry, scheduling, and effect
  semantics server-side. Delete handwritten or generated client behavior that
  competes with the production kernel.

**Library-first implementation defaults.** Derive accepted wire contracts from
Serde plus `schemars`; admit Aide only after typed DTOs exist and only when it
removes route/OpenAPI glue without creating parallel models. Generate
TypeScript wire models, then keep handwritten lifecycle façades
over Rust `reqwest` and browser/Node `fetch`; use
`oasdiff` and ecosystem semver tooling for compatibility checks. This replaces
hand-copied wire types, route schema glue, SSE framing, and bespoke
compatibility comparison while retaining server-owned authorization, retry
directives, paging, cancellation, reconnect/resume, receipts, and idiomatic
client lifecycle. If generation loses required union/error/stream semantics,
keep or repair the typed façade and reject the generator/Aide path rather than
weakening conformance.

**Direct acceptance.** Run behavioral/lifecycle conformance for every
supported public operation through both published SDKs against local and
PR16-enrolled remote authority, including representative complete journeys for
each capability family, the create/evidence/admit/monitor/cancel/resume loop,
version negotiation, paging, streaming interruption, typed failure and retry
directive, unavailable capability, receipts, and cross-version compatibility.
Produce and consume both the investigation and task handoff token through
Rust and TypeScript; prove exact destination, scope, authorization,
single-use expiry, policy-safe non-enumeration, wrong-scope/expired/
unauthorized behavior, and local/remote semantic parity. LSP only opens the
owning surface; it retrieves no task body and mutates no work. Compilation,
generated declaration coverage, or schema equality alone is not acceptance.

**Not in this PR.** Data cutover and V1 deletion belong to PR19.

## PR19 — V2 becomes the only product path

**User outcome.** Existing installations migrate predecessor APIs and persisted
families proven in released/live use plus potentially deployed or persisted
branch-era families not eliminated by an authorized census, cut over to the V2
product with bounded recovery, and finish with one supported implementation.

**End-to-end production path.** A destination-committed resumable backfill
converts released data, isolated read-only shadow comparison verifies product
behavior, an explicit bounded cutover makes V2 default, and recovery restores
the V1 archive forward into verified V2 under a new fence. Superseded V1 and
migration-only paths are deleted only after the recovery window and the
applicable authorized installed-client/host/registered-store census.

**Implementation and deletion.**

- Record explicit compatibility dispositions for every published API, every
  callable name or protocol revision potentially retained by a dogfood
  client/host, and every stored datum potentially present in a released or live
  format that crosses the cutover. Pure source-only/internal API shapes change
  in place; potentially installed names and branch-written stores,
  spools, files, and projections remain in the inventory until the applicable
  authorized installed-host/live-profile census proves absence.
- Do not retain reverse cutover, long-lived dual write, lazy read migration,
  production shadow reads, or V1 as renewed authority.
- Remove superseded V1 implementations, adapters, flags, branches, and
  migration-only machinery only when their recovery boundary closes and the
  applicable authorized census proves no installed consumer or registered store
  depends on them.

**Library-first implementation defaults.** Use the existing SQLite
`VACUUM INTO`/backup path, migration and fault-injection seams, and proptest
coverage. They replace a new backup copier, migration orchestration framework,
and separate crash/property harness while retaining maintenance fencing,
isolated staged import, bounded transactions, durable checkpoints, semantic
verification, atomic cutover, and forward-only recovery. If a source or
platform cannot produce and verify the required snapshot, block cutover and
use the existing explicit backup fallback; do not add a migration framework or
claim success from schema-only evidence.

**Direct acceptance.** Migrate representative released data, interrupt and
resume every phase, compare direct product journeys, cut over atomically,
restore from the archive into a new V2 fence, and prove deletion,
compatibility, and rollback-window behavior before deleting the old path.

**Not in this PR.** V1 archives are bounded recovery input, not a second product
or permanent read path.

## PR20 — measured product speed

**User outcome.** The production journeys delivered by PR13–PR19 are
demonstrably faster or cheaper without changing their results, safety, or
recovery behavior.

**End-to-end production path.** Production instrumentation identifies a
bottleneck in database access, synchronization, projection, indexing,
cache/generation handling, host feedback, dashboard investigation, multi-root
query/Git, remote sync/recovery, task-intelligence evidence/calibration/
proposal, workflow execution, SDK lifecycle, migration, or repository-controlled
developer builds. A focused implementation changes that path, recomputes the
same observable result, and is retained only when repeated comparisons show a
practical gain.

**Implementation and deletion.**

- Compare the same real Linux developer workload before and after a candidate,
  report raw samples and practical deltas, and keep crash/restart correctness
  in direct product tests and normal cross-platform CI.
- Preserve exact/lexical fallbacks, project/user isolation, receipts,
  determinism, coverage, and recomputation equivalence.
- Developer-build improvements may change portable manifests, profiles,
  features, build settings, and build scripts only when the same workload
  improves and stock-Cargo contributor, CI, release, and publication behavior
  remains valid.
- Delete ineffective candidates, obsolete slow paths, and one-off measurement
  code that is not production instrumentation or a reproducible regression.

**Library-first implementation defaults.** Reuse existing `tracing`,
`sysinfo`, and Criterion diagnostics, with `psutil` only in the Python soak
harness. This replaces custom telemetry collectors, process sampling, and
microbenchmark plumbing while retaining real-journey oracles, practical
before/after comparisons, resource observations, and semantic/recovery
equivalence. Missing Linux measurement coverage yields a truthful pending or
insufficient result; it does not justify a benchmark service, performance
protocol, or new measurement authority.

**Direct acceptance.** Re-run representative shipped journeys before and after
each retained change on Linux, reproduce the gain and equivalent result, and
pass direct migration, recovery, deletion, and cross-platform default-feature
CI tests. No universal score, public benchmark rank, or paper threshold
substitutes for product evidence.

**Not in this PR.** Unmeasured speculative optimizations and placeholder
benchmarks do not ship.

## Compact owner reference

- PR5–PR7: [Plans 01](01-domain-crate.md), [02](02-store-crate.md),
  [03](03-capture-crate.md), [04](04-projectors-crate.md), and
  [18](18-secret-detection-redaction-and-private-data-safety.md).
- PR8–PR10: [Plans 05](05-query-crate.md),
  [15](15-search-quality-evaluation-and-retrieval-research.md),
  [23](23-session-lcm-temporal-retrieval-and-evaluation.md),
  [25](25-code-intelligence-indexing-crate.md), and
  [31](31-native-fastembed-semantic-code-search.md).
- PR11–PR12: [Plans 06](06-policy-crate.md), [08](08-tool-catalog-crate.md),
  [09](09-application-crate.md), [10](10-api-crate.md),
  [17](17-official-public-api-and-sdks.md),
  [20](20-configuration-control-plane.md),
  [21](21-cli-mcp-tool-surface-and-output-unification.md),
  [34](34-workspace-refactoring-and-api-migration.md),
  [35](35-daemon-lsp-gateway-and-universal-diagnostics.md),
  [36](36-git-aware-change-context-and-index-transactions.md), and
  [37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md).
- PR13: [Plans 07](07-hooks-crate.md), [22](22-incremental-context-scout-and-suggestion-envelopes.md),
  [27](27-cross-host-agent-plugin-bundles.md), [35](35-daemon-lsp-gateway-and-universal-diagnostics.md),
  and [37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md).
- PR14: [Plans 11](11-dashboard-frontend.md),
  [11c](11c-work-workspace-design.md),
  [14](14-historical-failure-regression-matrix.md),
  [24](24-canonical-task-plan-graph-and-multi-agent-executor.md),
  [26](26-observability-accounting-and-usage.md),
  [32](32-dynamic-workflow-runtime-and-sdk.md), and
  [37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md).
- PR15: [Plans 05](05-query-crate.md), [16](16-cross-project-repository-worktree-scope.md),
  [35](35-daemon-lsp-gateway-and-universal-diagnostics.md),
  [36](36-git-aware-change-context-and-index-transactions.md), and
  [37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md).
- PR16: [Plans 28](28-remote-multi-machine-shared-brain.md),
  [35](35-daemon-lsp-gateway-and-universal-diagnostics.md), and
  [37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md).
- PR17 residual capability: [Plans 01](01-domain-crate.md), [02](02-store-crate.md),
  [03](03-capture-crate.md), [04](04-projectors-crate.md),
  [05](05-query-crate.md), [06](06-policy-crate.md),
  [09](09-application-crate.md),
  [11](11-dashboard-frontend.md),
  [13](13-research-provenance-and-context-anchors.md),
  [14](14-historical-failure-regression-matrix.md),
  [16](16-cross-project-repository-worktree-scope.md),
  [20](20-configuration-control-plane.md),
  [21](21-cli-mcp-tool-surface-and-output-unification.md),
  [22](22-incremental-context-scout-and-suggestion-envelopes.md),
  [23](23-session-lcm-temporal-retrieval-and-evaluation.md),
  [24](24-canonical-task-plan-graph-and-multi-agent-executor.md),
  [26](26-observability-accounting-and-usage.md),
  [27](27-cross-host-agent-plugin-bundles.md),
  [32](32-dynamic-workflow-runtime-and-sdk.md),
  [35](35-daemon-lsp-gateway-and-universal-diagnostics.md),
  [36](36-git-aware-change-context-and-index-transactions.md), and
  [37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md).
- PR18: [Plans 08](08-tool-catalog-crate.md),
  [12](12-root-compatibility-migration.md),
  [13](13-research-provenance-and-context-anchors.md),
  [17](17-official-public-api-and-sdks.md), and
  [19](19-system-defragmentation-convergence-and-extensibility.md).
- PR19: [Plans 08](08-tool-catalog-crate.md),
  [12](12-root-compatibility-migration.md),
  [13](13-research-provenance-and-context-anchors.md),
  [17](17-official-public-api-and-sdks.md),
  [19](19-system-defragmentation-convergence-and-extensibility.md),
  [34](34-workspace-refactoring-and-api-migration.md), and every component
  migration section whose released data or API crosses the cutover.
- PR20: [Plan 33](33-end-to-end-performance-optimization.md) and
  [Plan 38](38-storage-retention-size-and-efficiency.md)'s compaction and
  size-telemetry budgets.

Storage retention, size, and efficiency
([Plan 38](38-storage-retention-size-and-efficiency.md)) threads through the
remaining slices rather than owning one PR: the Doctor storage finding
family lands with PR14 (plan 09 over plan 26 read models); automatic
branch-DB lifecycle and registry orphan detection/collection land with the
storage-runtime S11 window; session retention with raw/projected dedup
extends the staged LCM GC cards. Measured driver: one dogfood profile
reached 256 GB and was reduced to ~75 GB purely by removing data the
product should never have retained. All plan 38 sections (§1–§7) have now
landed on this branch (2026-07-23) — branch lifecycle, registry orphan
detection/collection, session retention with raw/projected dedup and
disposition-scoped evidence release, one-content-copy, the debris contract,
compaction policy types, and telemetry read models with typed Doctor storage
findings. Daemon-owned GC/retention/compaction cadence, per-transaction
retention reauthorization, exact registry relink/retirement, durable incident
debris quarantine/collection, real stale-branch and retention-backlog Doctor
sources, and the rusqlite reserved-health size/table-growth primitive plus
scoped daemon application adapter landed on 2026-07-23. Store soft budgets are
owner-configured and inert by default.

The remaining delivery proceeds by complete product journeys under this index.
Branch, pull-request, merge, worktree, and SHA choreography is not roadmap
authority.
