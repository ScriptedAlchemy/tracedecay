# TraceDecay V2 Branch-Aware Feedback Cycle, Read-Only PR Review Ingestion, and Agent Proximity

## Status / role

Planned across PR9, PR11–PR17. PR9 adds no new authority here: it ships
Plan 36's repository/commit snapshot identity (base/head SHA, merge base,
HEAD/ref state) and Plan 36/Plan 05's read-only diff and hunk intelligence
that GitHub-comment remap and CI-failure localization later consume; Plan 32's
workflow/effect/audit/receipt kernel is not required for any read-only/advisory
capability this plan defines. PR11 ships the concrete typed feedback-cycle
request/result, orchestration, and one-shot termination taxonomy in
[Plan 09](09-application-crate.md) — the first pillar (branch-aware post-edit
diagnostics and impact) begins shipping. PR12 completes that pillar for
LSP/MCP/CLI by wiring [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md)
gateway triggers and the explicit diagnostics-call trigger bound once by
[Plan 21](21-cli-mcp-tool-surface-and-output-unification.md). **PR13 is the
first coherent milestone**: hook/host delivery-adapter parity through
[Plan 27](27-cross-host-agent-plugin-bundles.md) plus first availability of
CI-failure localization, read-only ingestion/surfacing of existing GitHub
bot/maintainer PR review comments and threads, and tiered concurrent-agent
proximity — all four read-only/advisory pillars simultaneously available by
the end of PR13. PR14 adds dashboard/Doctor consumption of that same shipped
state (not first availability). PR15 extends scope to multi-root/cross-project.
PR16 defines node-local overlay/proximity computation and remote-authority
fencing for durable delivery. PR17 may compose these already-shipped
operations as typed workflow steps through
[Plan 32](32-dynamic-workflow-runtime-and-sdk.md); PR17 is not first
availability of any capability defined here, introduces no external effect,
and performs no GitHub write of any kind.

This plan is the architectural center for every closed-loop, branch-aware
semantic-feedback decision in the V2 plan set — trigger sources, the one-shot
cycle boundary, delivery-adapter parity across five surfaces, read-only GitHub
PR review ingestion, CI-failure localization, and concurrent-agent/thread
proximity — so other plans link back to it instead of restating this
architecture. It creates no second authority and no second finding store:
[Plan 09](09-application-crate.md) owns the one typed, transport-neutral
feedback-cycle request/result and orchestration that every consumer of this
architecture renders or projects; [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md)
remains the semantic-evidence provider and editor/host protocol
adapter — LSP is a projection target and evidence source, never the universal
transport for hooks, GitHub, CI, or proximity signals; [Plan 05](05-query-crate.md)
owns graph/query impact, affected-test evidence, cursors/watermarks, and
revision-range diff/hunk query primitives; [Plan 36](36-git-aware-change-context-and-index-transactions.md)
owns Git/branch/worktree/HEAD/commit-snapshot identity and read-only Git
intelligence; [Plan 13](13-research-provenance-and-context-anchors.md) owns
the durable `RetrievalAnchorId` every finding in this cycle anchors to;
[Plan 21](21-cli-mcp-tool-surface-and-output-unification.md) owns canonical
JSON, compact Markdown, CLI/MCP/HTTP/LSP bindings, and reversible
truncation/typed budget errors; [Plan 23](23-session-lcm-temporal-retrieval-and-evaluation.md)
owns session/LCM narrative retrieval and summary-DAG drilldown only — never
evidence authority; [Plan 22](22-incremental-context-scout-and-suggestion-envelopes.md)
owns the one suggestion/policy channel that renders this cycle's inert
suggested next actions; [Plan 27](27-cross-host-agent-plugin-bundles.md) owns
host hook/native delivery mechanics and the read-only GitHub ingestion
adapter's transport; [Plan 26](26-observability-accounting-and-usage.md) owns
telemetry; [Plan 16](16-cross-project-repository-worktree-scope.md) owns scope
resolution; [Plan 28](28-remote-multi-machine-shared-brain.md) owns remote
authority and node-local overlay fencing;
[Plan 32](32-dynamic-workflow-runtime-and-sdk.md) owns the optional PR17
workflow-step composition of this plan's already-shipped operations and is
not a prerequisite for any PR9–PR16 capability defined here;
[Plan 34](34-workspace-refactoring-and-api-migration.md) remains the only
apply path for accepted refactor candidates; and
[Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md) remains a
permanent tombstone for any universal executor, orchestrator, or scheduler
this plan must not resurrect.

## Outcome

Every trigger — a saved-file hook, an IDE/LSP save lifecycle event, an
explicit TraceDecay diagnostics call, an agent stop/pre-stop gate, a request to
surface a PR's existing review threads, or a request to localize a CI failure
— reaches the same one shared advisory typed feedback-cycle/finding fabric:
[Plan 09](09-application-crate.md)'s one typed cycle result. Every consumer —
hook/MCP agent context, LSP IDE Problems, dashboard, and CLI — renders or
projects that single result; none forks a private evidence shape or a second
finding store. LSP is semantic evidence and an editor-projection target only,
never the universal transport. TraceDecay never posts, updates, resolves,
dismisses, or replies to a GitHub PR comment or thread; it surfaces ingested,
remapped, read-only GitHub evidence, localizes CI failures, and reports
advisory concurrent-agent proximity — nothing in this architecture writes to
GitHub, applies a fix, or continues an agent automatically.

## Owns

- The closed-loop feedback-cycle architecture: the canonical trigger-source
  list, the one-shot edit → evaluate → deliver flow (not a repeating
  edit-fix loop), the exhaustive termination taxonomy, dedupe/idempotency
  rules across hook/MCP/LSP/dashboard/CLI delivery, and the safety boundary
  between session-only overlay feedback and durable saved-content feedback.
  [Plan 09](09-application-crate.md) implements the concrete typed
  request/result and orchestration against this architecture; this plan does
  not duplicate that type, and it does not define a GitHub-specific,
  CI-specific, or proximity-specific result type — each is a typed section of
  the one Plan 09 result.
- The compact feedback capsule shape: a reference-only artifact carrying
  canonical diagnostic/evidence/finding IDs plus a
  [Plan 13](13-research-provenance-and-context-anchors.md) `RetrievalAnchorId`
  rather than copied source or a second durable finding model.
- The read-only GitHub PR review ingestion architecture: the typed ingest
  contract (repository/PR/thread/comment identity, author class, diff/symbol
  remap, item/thread lifecycle state, and ingress provider outcome — §4), the
  staleness rule for remapped findings, and the absolute non-write boundary.
  GitHub REST/GraphQL is read-only ingress; TraceDecay is never a GitHub write
  client anywhere in this architecture.
- The CI-failure localization typed input contract (§5) and the mapping from a
  reported failure to symbol/branch-generation/caller evidence and rerun
  hints, without claiming CI authority.
- Concurrent-agent/thread proximity: the tiered advisory presence/proximity
  contract, warning classes, thresholds, freshness/expiry, and privacy
  controls, built from existing agent/session/worktree/branch observations and
  graph neighborhoods rather than a new coordination or scheduling authority.
- The lossless-evidence/bounded-projection contract for this cycle's findings:
  which fields are canonical-anchored, which are safe bounded snippets, and
  how truncation, expansion, and expiry compose across every delivery surface
  without becoming a second anchor or paging authority.
- Cross-plan PR staging for this cycle, including the explicit first-milestone
  section (§7) proving all four pillars are simultaneously available by PR13.

## Does not own

- A second diagnostic store, code graph, query engine, semantic-evidence
  provider contract, suggestion/envelope channel, workflow/scheduling engine,
  durable evidence anchor, cursor/paging mechanism, canonical rendering
  format, or host integration manifest. Every one of those remains owned by
  Plans 09/35 (evidence/providers), 05 (graph/query/cursors), 13 (anchors), 21
  (bindings/rendering/truncation), 23 (session narrative), 22 (suggestions),
  32 (workflows), and 27 (host manifest) respectively.
- Any GitHub write path. Posting, updating, resolving, dismissing, or
  replying to a PR review comment or thread is out of scope unconditionally —
  not gated behind a policy grant, receipt, or supersession-as-write path.
  GitHub REST/GraphQL is read-only ingress in this architecture; no component
  this plan describes, stages, or composes may become a GitHub write client.
- CI execution or result authority. CI runs and their pass/fail outcome
  belong to the CI system; this plan only localizes a reported failure to
  symbols, branch generation, callers, and rerun hints.
- LSP JSON-RPC framing, upstream analyzer supervision, or diagnostic
  merge/publication mechanics —
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) owns those;
  this plan only defines the `source`/`data`/`relatedInformation` projection
  contract (§6) that Plan 35 implements.
- Git object/index/ref state, `HunkRef`, `RepositorySnapshot`, or revision-range
  diff mechanics — [Plan 36](36-git-aware-change-context-and-index-transactions.md)
  and [Plan 05](05-query-crate.md) own those; this plan only binds GitHub
  comments and CI failures to that identity for remap and localization.
- Editing authority. No trigger, capsule, or delivery adapter defined here
  may apply a `WorkspaceEdit`, codeAction, or any file mutation.
  [Plan 34](34-workspace-refactoring-and-api-migration.md) remains the only
  apply path for accepted rename/refactor candidates surfaced as an inert
  suggested next action; general `codeAction` remains deferred per Plan 35.
- Scheduling, leasing, or executing agents, and any autonomous agent
  continuation or follow-up. No trigger, cycle result, or proximity warning
  creates a file lock, task assignment, agent step, or `followup_message`.
  [Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md)'s
  tombstone stands; the optional [PR17 workflow composition](32-dynamic-workflow-runtime-and-sdk.md)
  remains workflow-authority effect safety, not generic editor or agent
  ownership, and performs no GitHub write.
- Host packaging, install/repair/uninstall mechanics, or the canonical
  host-integration catalog — [Plan 27](27-cross-host-agent-plugin-bundles.md)
  owns those; this plan only defines which delivery adapter a host receives
  and the read-only shape of the GitHub ingestion adapter it transports.
- Durable evidence-anchor issuance, cursor/watermark paging, canonical
  JSON/Markdown rendering, or transport-level response-handle truncation —
  Plans 13, 05, and 21 own those; this plan only states how its findings must
  use them (§8) and forbids substituting a transport handle for durable
  identity.
- Session/LCM narrative summarization or its summary-DAG — Plan 23 owns
  those; this plan only forbids narrative from replacing canonical evidence
  (§8).

## Required architecture

### 1. One shared advisory typed feedback-cycle/finding fabric

- [Plan 09](09-application-crate.md) owns the one typed, transport-neutral
  feedback-cycle request/result and orchestration. Every consumer of this
  architecture — hook/MCP agent context, the LSP editor projection, the
  dashboard, and the CLI diagnostics call — renders or projects that single
  result. No consumer forks a private evidence shape, and no pillar (post-edit
  diagnostics, CI localization, GitHub ingestion, proximity) defines a second
  result type: each is a typed section of the one Plan 09 result.
- LSP is a sensor and presentation adapter, exactly as
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) defines it. It
  contributes active-document semantic evidence and publishes
  Problems/related-location results for the diagnostics-and-impact pillar and
  the read-only projection of GitHub/CI/proximity findings (§6). It never
  becomes the transport that originates hook, GitHub, CI, or proximity
  signals — those reach the cycle through their own native transports over
  the same daemon/application contracts.
- Trigger sources are exhaustive and typed: saved-file/post-edit hook,
  IDE/LSP document-save lifecycle, an explicit TraceDecay diagnostics
  MCP/CLI/API call, an agent stop/pre-stop gate, a request to surface a PR's
  existing review threads, and a request to localize a CI failure. Every
  trigger maps to the same one-shot cycle; none defines a private evidence
  shape.
- The cycle composes: [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md)
  semantic-provider results for the active document/session; [Plan 05](05-query-crate.md)
  graph/query impact, affected-test, and revision-range diff/hunk evidence;
  Git/branch/worktree/commit-snapshot identity from
  [Plan 36](36-git-aware-change-context-and-index-transactions.md); read-only
  GitHub-ingested review-thread findings remapped to the current branch (§4);
  CI-failure localization findings (§5); concurrent-agent proximity warnings
  (§3); [Plan 22](22-incremental-context-scout-and-suggestion-envelopes.md)'s
  inert suggested-next-action policy; diagnostics provenance (producer,
  revision, freshness, coverage); and host delivery selection per
  [Plan 27](27-cross-host-agent-plugin-bundles.md).
- The feedback capsule references canonical diagnostic/evidence/finding IDs
  and a [Plan 13](13-research-provenance-and-context-anchors.md)
  `RetrievalAnchorId`. It never copies source text or a GitHub comment body
  into a new durable representation and never invents a second finding model
  alongside Plan 09's/Plan 35's diagnostic identity.
- Cycle-request inputs bind: project/repository/worktree/branch/ref/HEAD SHA;
  clean source-generation identity or an explicitly tagged
  ephemeral-overlay identity; file digest and document version;
  agent/session/turn identity; changed files/ranges/symbols; the exact
  trigger; policy/config digests; deadline/cancellation; and
  token/latency/cost budgets for this one-shot evaluation.
- Cycle results distinguish: new versus pre-existing diagnostics; the
  complete canonical provider-state set from Plan 35/09 — supported+
  completed+complete-coverage zero-findings versus unsupported, absent,
  indexing, stale, cancelled, timed-out, failed, and partial — with none
  collapsing to a clean empty result; affected callers/files/tests from
  Plan 05; semantic risks; read-only GitHub-ingested findings with orthogonal
  item/thread lifecycle and ingress provider outcome (§4); CI-failure
  localization findings; concurrent-agent proximity (§3); an inert suggested
  next action; and an exact termination reason (§2).

### 2. One-shot evaluation and closed-loop safety

- Canonical flow: trigger → evaluate (diagnose, classify baseline versus
  regression, enrich with semantic/impact/test/GitHub/CI/proximity evidence)
  → bounded delivery. There is no repeating edit-fix loop: this plan replaces
  any "edit → ... → agent fixes → repeat" semantics with one deliberate
  evaluation per trigger. A user or host may explicitly invoke a later cycle
  after a subsequent edit; that is a new, independently triggered one-shot
  cycle, never a continuation of the prior one.
- No automatic agent continuation or follow-up of any kind. No cycle result
  fires a host `followup_message`, schedules another turn, or otherwise
  causes an agent to act without an explicit new trigger. Suggested next
  actions from [Plan 22](22-incremental-context-scout-and-suggestion-envelopes.md)
  are inert: text and evidence references only, never auto-executed and never
  auto-applied. Silence remains a normal successful outcome exactly as Plan
  22 already requires for suggestions.
- Exhaustive termination taxonomy: clean, blocked, incomplete coverage,
  stale/replan required, budget exceeded (deadline, token, latency, or cost),
  cancellation, user stop, and daemon unavailable. There is no max-iterations
  state because there is no loop to bound; the one-shot budget fields above
  are retained and every cycle result names exactly one termination reason —
  none is inferred from adapter-side silence.
- Dedupe/idempotency keys bind trigger, address
  (project/branch/file/range/symbol), diagnostic/evidence/finding identity,
  and delivery channel so that hook, MCP, LSP, dashboard, and CLI delivery of
  the same evidence never duplicate. Because no GitHub write exists, dedupe
  never has to justify or bound outbound comment volume — it exists purely to
  keep the five read surfaces (§6) consistent for one trigger.
- No raw LSP `WorkspaceEdit` or `codeAction` apply anywhere in this cycle.
  General `codeAction` remains deferred per
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
  [Plan 34](34-workspace-refactoring-and-api-migration.md)'s
  `EditTransaction` remains the only apply path for supported
  rename/refactor candidates surfaced as an inert suggested next action.
- No scheduling, leasing, or locking of any kind rides on this cycle or its
  proximity warnings. [Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md)'s
  tombstone stands unconditionally.
- Unsaved overlays may produce immediate session-only feedback for the
  authorized client that owns the overlay. That feedback is never durable:
  it cannot enter a capsule, envelope, checkpoint, receipt, feedback-history
  record, observation, fact, memory entry, telemetry payload, spool, cache,
  replica, export, LCM node, or any GitHub-bound evidence. Durable feedback
  requires exact saved-content/clean-generation identity, matching the
  positive/negative fixture contract already required by
  [Plan 22](22-incremental-context-scout-and-suggestion-envelopes.md) and
  [Plan 14](14-historical-failure-regression-matrix.md).

### 3. Concurrent-agent/thread proximity (advisory, tiered, TraceDecay-native)

- The daemon tracks advisory presence/proximity using existing
  agent/session/worktree/branch observations plus the changed
  file/range/symbol and graph package/crate/dependency neighborhoods already
  owned by [Plan 05](05-query-crate.md) and
  [Plan 25](25-code-intelligence-indexing-crate.md). This plan adds no
  second observation, session, or graph model.
- **Immediate tier:** exact same file/range/symbol qualifying conflicts emit
  immediately, without waiting on a risk-threshold evaluation.
- **Threshold tier:** same package/crate, shared callers/dependencies/tests,
  incompatible branch/worktree state, and overlapping planned workspace
  changes emit only when a typed, configurable proximity risk threshold is
  met. Below that
  threshold, the daemon stays silent — silence is a normal, expected outcome,
  not a missing feature.
- Risk-threshold inputs are typed and explicit: overlap/blast-radius size,
  relation strength (direct call/dependency versus transitive), branch/worktree
  incompatibility class, and freshness decay of the underlying observation.
  Configuration may raise or lower the threshold; it never removes the
  distinction between the two tiers.
- Every warning carries `observed_at` and `expires_at`, participates in
  suppression/dedupe by address and warning class so repeated observation of
  the same conflict does not re-emit, and respects existing privacy scoping
  across sessions/agents — a warning never reveals another session's content,
  only the fact and coarse shape of an overlapping change.
- `textDocument/documentSymbol` may resolve ranges/symbols for a proximity
  warning; LSP never transports leases, presence, or the warning payload
  itself as a side channel outside the one typed cycle result (§1, §6).
- This plan preserves
  [Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md)'s
  tombstone exactly: no resurrected universal executor/orchestrator, no
  autonomous file locks, and no agent scheduling. Every proximity warning is
  advisory only.
- Delivery is through the same five surfaces as every other pillar (§6);
  optional IDE `relatedInformation` presentation ships only after that
  presentation method passes its own separate conformance gate per §9.

### 4. Read-only GitHub PR review ingestion (never an LSP transport, never a write path)

- The GitHub REST/GraphQL API is read-only ingress in this architecture.
  TraceDecay ingests **existing** review comments, threads, and replies from
  bots and maintainers; it never posts, updates, resolves, dismisses, or
  replies to a comment or thread. There is no policy grant, receipt, delivery
  confirmation, spam-avoidance dedupe for outbound comments, posted- or
  supersession-as-write state, autonomous posting mode, or
  [Plan 32](32-dynamic-workflow-runtime-and-sdk.md) prerequisite anywhere in
  this ingestion path — none of that exists because there is no write path to
  gate.
- The typed ingest contract binds, per ingested item:
  - repository, provider, and PR identity; base and head SHA; and merge base;
  - review, thread, comment, and reply IDs;
  - author identity and author class (bot, maintainer, or other observed
    role) reported as-observed, never upgraded into an invented trust level;
  - a body digest plus a sanitized retained-payload
    [Plan 13](13-research-provenance-and-context-anchors.md) anchor — never
    the raw body copied into a second durable representation;
  - review state (e.g. approved, changes-requested, commented);
  - exactly one **item/thread lifecycle** value from the exhaustive typed
    set in the next bullet — never conflated with ingress provider outcome;
  - exactly one **ingress provider outcome** value from the exhaustive typed
    set in the bullet after that — never conflated with item/thread lifecycle;
  - path, side, original line, current line, and commit for both the
    original and (when remapped) current position;
  - the comment/thread URL when authorized and safe to retain; and
  - API cursor, ETag, `fetched_at`, rate-limit metadata, permission state,
    and ingestion coverage.
- Diff and symbol remap to the current branch uses
  [Plan 36](36-git-aware-change-context-and-index-transactions.md)'s
  repository/commit-snapshot identity and
  [Plan 05](05-query-crate.md)'s revision-range diff/hunk evidence — both
  available by PR9. Remap never rewrites source thread history: the original
  ingested review thread remains exactly as observed, and remap produces a
  derived, anchored projection alongside it.
- **Item/thread lifecycle** is exhaustive, typed, and orthogonal to ingress
  provider outcome. Each ingested item carries exactly one lifecycle value
  describing the review comment, thread, or reply as observed or remapped:
  - `current` — active on the current branch with a provable exact
    content-and-anchor remap match;
  - `outdated` — GitHub-marked outdated or remap cannot prove an exact current
    binding (path or line similarity alone never upgrades to `current`);
  - `resolved` — the thread or review item is resolved on GitHub;
  - `edited` — body or metadata edited since the retained anchor was issued;
  - `deleted` — the comment, reply, or thread is deleted on GitHub.
  No lifecycle value represents a TraceDecay outbound action, a posted comment,
  or an ingress fetch result.
- **Ingress provider outcome** is exhaustive, typed, and orthogonal to
  item/thread lifecycle. Each fetch, refresh, or expansion attempt names
  exactly one outcome aligned with [Plan 09](09-application-crate.md) and
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) canonical
  provider semantics:
  - `complete` — supported fetch successfully completed with complete
    coverage;
  - `partial` — some items or pages returned but coverage is incomplete;
  - `unavailable` — provider, endpoint, or daemon unavailable;
  - `denied` — authorization or permission denied for the requested scope;
  - `rate_limited` — GitHub rate limit or quota prevented complete fetch;
  - `stale` — cached ETag, cursor, or head-SHA drift makes the retained
    snapshot stale relative to the current repository state;
  - `failed` — fetch failed for a reason not covered above.
  Lifecycle and provider outcome are never collapsed: for example, a
  `complete` fetch may return items in any lifecycle state, and an item in
  `current` lifecycle may be surfaced under a `partial`, `stale`, or
  `unavailable` ingress outcome when refresh or expansion fails.
- TraceDecay never surfaces a finding from a dirty overlay, a stale head SHA,
  an unmappable non-diff line, incomplete coverage presented as clean, or
  unauthorized/private evidence — because there is no comment path, these
  conditions instead produce the exact typed ingress provider outcome
  (`partial`, `unavailable`, `denied`, or `failed`) and never fabricate a
  `complete`/`current` pair.
- Semantic surfacing expands an ingested finding with callers, implementations,
  affected tests, branch diagnostics, and CI evidence (§5) rather than
  rendering a bare copied comment body.

### 5. CI-failure localization (advisory, CI remains authority)

- CI execution and its pass/fail outcome remain the CI system's authority.
  TraceDecay only localizes a reported failure to a symbol, branch
  generation, callers, and targeted rerun hints; it never claims to have run,
  verified, or influenced CI.
- The typed CI input contract binds: CI provider and repository identity;
  workflow, job, check-suite, check-run, run, and attempt IDs; head SHA and
  ref; an artifact/log URI or a retained
  [Plan 13](13-research-provenance-and-context-anchors.md) retrieval anchor
  for the log; an excerpt digest; parser identity and version; event time;
  failure kind, file, line, and test; a confidence value; coverage; explicit
  stale, partial, unavailable, and denied states; and permissions, rate
  limits, and retention for the underlying log/artifact.
- Rerun hints and any other suggested next action from this pillar are inert
  per §2: TraceDecay never triggers a rerun, never re-executes CI, and never
  schedules a retry.

### 6. Delivery across five surfaces over one result

- Every pillar (post-edit diagnostics+impact, CI localization, GitHub
  ingestion, proximity) delivers through the same five surfaces over the same
  Plan 09 cycle result; no pillar gets a private sixth surface:
  1. **Hook** — post-edit/stop-gate hook delivery through
     [Plan 27](27-cross-host-agent-plugin-bundles.md) host adapters.
  2. **MCP** — the explicit TraceDecay diagnostics/ingestion/localization/
     proximity operation bound once by
     [Plan 21](21-cli-mcp-tool-surface-and-output-unification.md).
  3. **LSP** — IDE Problems/annotations attached to the current file/function
     through [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md)'s
     projection contract (below).
  4. **Dashboard** — through [Plan 11](11-dashboard-frontend.md) at PR14,
     consuming state already shipped at PR13.
  5. **CLI** — the same bound operation as MCP, rendered per
     [Plan 21](21-cli-mcp-tool-surface-and-output-unification.md).
- No surface requires the IDE to be open. Hook, MCP, dashboard, and CLI
  delivery all function with no LSP session connected; LSP delivery is an
  additional projection when a session exists, never the only path.
- [Plan 21](21-cli-mcp-tool-surface-and-output-unification.md) owns the
  CLI/MCP/HTTP/LSP binding taxonomy and rendering; [Plan 27](27-cross-host-agent-plugin-bundles.md)
  owns host hook/native transport mechanics; this plan only defines which
  cycle-result sections each surface must expose and the LSP projection
  contract those owners implement.
- **LSP projection contract** (implemented by
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md); must match
  Plan 35's Plan 37 feedback-finding projection exactly):
  - `Diagnostic.range` binds to the current function/symbol after remap, not
    the original PR/CI coordinate.
  - `Diagnostic.source` is `github-review`, `tracedecay-ci`, or
    `tracedecay-proximity` for these three pillars, distinct from Plan 35's
    existing diagnostic sources for the post-edit pillar.
  - `Diagnostic.code` is a stable finding-code identifier.
  - `Diagnostic.codeDescription.href` is the original GitHub or CI URL only
    when authorized and safe to expose; otherwise the field is omitted
    entirely — never a TraceDecay-internal link and never an
    unauthorized/private URL.
  - `Diagnostic.data` carries only the stable finding ID, the
    [Plan 13](13-research-provenance-and-context-anchors.md)
    `RetrievalAnchorId`, item/thread lifecycle state, ingress provider
    outcome, and coverage — never a large payload, thread body, reply text,
    or log excerpt. Full GitHub thread/reply text, CI logs, and proximity
    evidence expand only through authorized Plan 21 `feedback_get` /
    `feedback_expand` and Plan 13 anchor resolution (§8), not embedded in
    the diagnostic.
  - `Diagnostic.relatedInformation` carries only valid LSP Locations (`uri` +
    `range`) plus bounded messages for co-located related sites (another
    comment anchor-mapped to a workspace location, a related CI failure site,
    or a proximity-conflicting range). It never carries pointer-only reply
    records, anchor IDs without a resolvable Location, or copied thread/reply
    bodies. GitHub replies and full thread context are retrieved through
    authorized anchor / `feedback_expand` operations, not through
    `relatedInformation`.
  - Severity is conservative: it mirrors the source's own classification
    (a CI failure keeps its reported severity; a GitHub review comment or a
    proximity note never exceeds an advisory severity) — this projection
    never fabricates a severity the underlying evidence does not support.
  - Clearing and removal are deterministic on thread resolution, comment
    deletion, head-SHA or content/generation change that invalidates the
    remap, or supersession, following the same "clear or republish exactly
    once" rule
    [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) already
    requires for other diagnostic sources.

### 7. First coherent milestone: PR11–PR13

- The first coherent milestone for this architecture spans **PR11–PR13** and
  is complete only when all four read-only/advisory pillars are simultaneously
  available:
  (a) branch-aware post-edit diagnostics and impact (Plan 09's typed result
      shipped at PR11, LSP/MCP/CLI trigger binding completed at PR12);
  (b) CI-failure localization (§5), first available at PR13;
  (c) read-only ingestion and display of **existing** GitHub bot/maintainer
      review comments and threads (§4), first available at PR13; and
  (d) tiered concurrent-agent proximity (§3), first available at PR13.
- PR14 adds dashboard/Doctor consumption of that already-shipped state
  through [Plan 11](11-dashboard-frontend.md) and
  [Plan 26](26-observability-accounting-and-usage.md); PR14 is not first
  availability of any pillar.
- PR15 extends every pillar's scope to multi-root/cross-project targets
  through [Plan 16](16-cross-project-repository-worktree-scope.md); no
  pillar's first availability depends on PR15.
- PR16 defines node-local overlay/proximity computation and remote-authority
  fencing for durable delivery through
  [Plan 28](28-remote-multi-machine-shared-brain.md); no pillar's first
  availability depends on PR16.
- PR17 may compose these already-shipped, read-only advisory operations as
  typed workflow steps through
  [Plan 32](32-dynamic-workflow-runtime-and-sdk.md)'s shared
  scheduler/history/lease/effect/artifact kernel. PR17 introduces no first
  availability of any capability defined here, no new external effect, and
  no GitHub write — workflow composition in that kernel remains workflow
  authority only, never a new GitHub write path.
- Acceptance for this milestone requires one integration fixture (§9) that
  exercises a single branch/PR scenario and proves all four pillars produce
  consistent, simultaneously available evidence by the end of PR13, rendered
  identically across all five delivery surfaces where each is applicable.

### 8. Lossless canonical evidence with bounded projections

- Canonical evidence for every finding in this cycle is lossless and
  anchored; every surface receives a bounded projection of it, never a
  second copy of record.
  [Plan 13](13-research-provenance-and-context-anchors.md) owns the durable
  `RetrievalAnchorId` that every GitHub thread/comment, CI log excerpt,
  semantic-evidence capsule, related finding, and proximity observation
  anchors to. [Plan 05](05-query-crate.md) owns stable cursor/watermark
  paging over collections of findings. [Plan 21](21-cli-mcp-tool-surface-and-output-unification.md)
  owns canonical JSON, compact Markdown, and reversible truncation with a
  typed budget error. [Plan 23](23-session-lcm-temporal-retrieval-and-evaluation.md)
  owns session/LCM narrative retrieval and summary-DAG/external-payload
  drilldown only.
- Every cycle result and every finding within it carries a stable finding ID
  plus its `RetrievalAnchorId` as durable identity. A 24-hour
  transport-level response handle (the kind [Plan 21](21-cli-mcp-tool-surface-and-output-unification.md)'s
  oversized-result truncation may emit) is never treated as durable finding
  identity anywhere in this cycle — it is a reversible transport convenience
  over the same canonical anchor, and it expires; the anchor does not.
  Presenters may additionally emit a response handle and its expiry for an
  oversized serialized result without that handle ever substituting for the
  finding ID/anchor pair.
- Large GitHub threads/replies, CI logs, semantic context, related findings,
  and proximity evidence expose a safe bounded snippet plus: the exact
  retained-source anchor or payload reference; a stable cursor; total,
  returned, and omitted counts; the byte/char/token budget applied; coverage;
  a truncation flag and reason when truncated; a next cursor/offset/handle;
  and expiry/watermark information. No surface silently truncates without
  reporting that it did.
- LCM/session narrative summaries may help retrieve session-linked narrative
  about a finding, but never replace canonical GitHub thread, CI, diagnostic,
  or branch/commit evidence as the authoritative record. Every
  narrative summary that touches this cycle's evidence retains exact source
  lineage and exact expansion back to the canonical anchor, exactly as
  [Plan 23](23-session-lcm-temporal-retrieval-and-evaluation.md)'s summary-DAG
  already requires.
- Dirty overlays and private/unsaved source can never enter a durable
  finding, a payload, a response-handle body, an LCM node, an export, a
  cache, a replica, or remote transport for this cycle, matching the same
  overlay-durability boundary [Plan 22](22-incremental-context-scout-and-suggestion-envelopes.md)
  and §2 already require.
- Authorization is rechecked on every anchor, payload, or handle expansion;
  possessing an ID never grants access on its own, exactly as
  [Plan 13](13-research-provenance-and-context-anchors.md) already requires.
  Retention, redaction, deletion, expired, missing, and corrupt states are
  typed and distinct, with a safe tombstone where Plan 13's retention policy
  allows one — never a silent empty result standing in for any of those
  states.

### 9. Lint/CI/hints and other opportunities

- Compiler/linter diagnostics remain provider evidence with producer
  codes/severity/provenance and the baseline classification defined in §2;
  this plan adds no second producer taxonomy.
- Context Scout and hints reuse the same bounded capsule and the one
  suggestion channel owned by
  [Plan 22](22-incremental-context-scout-and-suggestion-envelopes.md); this
  plan creates no second hint stream.
- Future P2 presentation methods — CodeLens for impact/tests/rename preview,
  inlay hints for provenance/freshness, and cross-repo moniker/federated
  definition — are explicitly gated and deferred rather than silently in
  scope. Shipping one requires its own conformance gate exactly as
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) already
  requires for vendor-specific methods.

## PR staging

PR6 gains no implementation scope from this plan; it may only preserve
event/host/branch identity that this cycle needs later, matching the existing
PR6 boundary.

| PR | This plan's contribution |
|---|---|
| PR9 | No new authority here. [Plan 36](36-git-aware-change-context-and-index-transactions.md) ships repository/commit-snapshot identity (base/head SHA, merge base, HEAD/ref) and read-only diff/hunk intelligence, and [Plan 05](05-query-crate.md) ships the composed revision-range diff/hunk query primitives, that this cycle's GitHub remap and CI localization later consume. Plan 32's workflow kernel is not required here or anywhere else in this milestone. |
| PR11 | [Plan 09](09-application-crate.md) ships the concrete typed feedback-cycle request/result, orchestration, and the one-shot termination taxonomy (§2). First pillar (post-edit diagnostics+impact) begins shipping. |
| PR12 | [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) gateway triggers and the explicit MCP/CLI/API diagnostics-call trigger bound once by [Plan 21](21-cli-mcp-tool-surface-and-output-unification.md) (§6). Completes the post-edit diagnostics-and-impact pillar for LSP/MCP/CLI. |
| PR13 | **First coherent milestone (§7).** Hook and agent stop/pre-stop-gate triggers, host delivery-adapter parity through [Plan 27](27-cross-host-agent-plugin-bundles.md); first availability of CI-failure localization (§5), read-only GitHub review-comment/thread ingestion and surfacing (§4), and tiered concurrent-agent proximity (§3). All four pillars are simultaneously available across hook/MCP/CLI/LSP surfaces. No GitHub write exists at PR13 or at any later PR. |
| PR14 | Dashboard/Doctor/observability consumption of the same typed cycle, GitHub-ingested, CI-localization, and proximity state already shipped at PR13, through [Plan 11](11-dashboard-frontend.md) and [Plan 26](26-observability-accounting-and-usage.md). Not first availability. |
| PR15 | Multi-root/cross-project cycle, GitHub-remap, CI-localization, and proximity scope through [Plan 16](16-cross-project-repository-worktree-scope.md); no pillar's first availability depends on PR15. |
| PR16 | Node-local overlay and remote-authority rules through [Plan 28](28-remote-multi-machine-shared-brain.md): unsaved overlays and proximity computation stay node-local; durable cycle state, GitHub-ingested evidence, and CI-localization evidence are fenced through shard authority. No pillar's first availability depends on PR16. |
| PR17 | Optional composition of this plan's already-shipped read-only advisory operations (feedback-cycle findings, GitHub-ingested review-thread surfacing, CI localization, proximity) as typed workflow steps through [Plan 32](32-dynamic-workflow-runtime-and-sdk.md)'s shared scheduler/history/lease/effect/artifact kernel. Not first availability of any capability; no GitHub write; no new authority. |

## Acceptance

- Real one-shot evaluation fixtures for an LSP-capable host, a hook-only
  host, an explicit MCP diagnostics call, and the CLI/dashboard all produce
  semantically equivalent typed feedback where capabilities overlap, with no
  IDE required to be open for hook/MCP/dashboard/CLI delivery.
- New-versus-pre-existing baseline classification, zero-findings versus
  unavailable/partial coverage, and the save/overlay durability boundary are
  covered exactly as
  [Plan 14](14-historical-failure-regression-matrix.md) requires for Plan
  09/35 states.
- Branch/worktree/head changes, duplicate triggers, cancellation, budget
  exhaustion, analyzer restart, and stale-generation fixtures each produce
  their exact typed termination reason (§2) — clean, blocked, incomplete
  coverage, stale/replan required, budget exceeded, cancellation, user stop,
  or daemon unavailable — rather than a guessed clean result or a
  max-iterations state, which does not exist.
- A fixture proves no cycle result ever fires a host `followup_message` or
  otherwise causes an agent to act without an explicit new trigger; a
  suggested next action remains inert text/evidence across every delivery
  surface.
- **First-milestone integration fixture (§7):** one branch/PR scenario proves
  post-edit diagnostics+impact, CI-failure localization, GitHub review-thread
  ingestion/surfacing, and concurrent-agent proximity are all available and
  mutually consistent by the end of PR13, and that the same evidence renders
  correctly on hook, MCP, LSP, dashboard (once PR14 ships), and CLI surfaces.
- **GitHub ingestion fixtures** cover the two orthogonal typed dimensions
  from §4:
  - **Item/thread lifecycle:** `current`, `outdated`, `resolved`, `edited`,
    and `deleted` — including GitHub-native resolved/edited/deleted states and
    exact-match versus symbol-remapped-but-`outdated` binding after a head-SHA
    change.
  - **Ingress provider outcome:** `complete`, `partial`, `unavailable`,
    `denied`, `rate_limited`, `stale`, and `failed` — including rate-limit,
    auth-failure, ETag-reuse, head-SHA drift, and daemon-restart recovery.
  - A **lifecycle × provider-outcome matrix** exercises every lifecycle
    value under each relevant provider outcome (for example, `current` under
    `complete`, `outdated` under `stale`, `deleted` under `complete`, and
    `resolved` under `denied`) and proves the dimensions are never collapsed
    into one field or inferred from silence.
  - Bot-versus-maintainer author-class reporting without invented trust;
    remap never mutates the original ingested thread record; and every
    attempted GitHub write operation is rejected before any outbound GitHub
    call, producing a typed `denied` ingress outcome rather than any partial
    write.
- **CI-localization fixtures** map a structured failure to symbol/branch
  generation/callers and a targeted rerun hint using the typed input contract
  (§5), including stale, partial, unavailable, and denied log/artifact states
  without ever exposing raw log content outside its retained anchor, and prove
  the cycle never claims to have executed, retried, or influenced CI.
- **Proximity fixtures** cover the immediate tier (exact same
  file/range/symbol) and the threshold tier (same package/crate, shared
  callers/dependencies/tests, incompatible branch/worktree state, overlapping
  planned workspace changes) both above and below the configured risk
  threshold,
  proving below-threshold silence is a normal outcome. Fixtures also prove
  advisory-only semantics, `observed_at`/`expires_at` freshness, suppression/
  dedupe, and privacy scoping across sessions/agents without creating a lock
  or schedule.
- **Lossless evidence fixtures** prove: a finding's ID + `RetrievalAnchorId`
  survive envelope, checkpoint, delivery, telemetry, and every durable
  spool/cache/replica/export representation unchanged; a 24-hour response
  handle is never substituted for that identity and its expiry is independent
  of the anchor's lifecycle; authorization is rechecked on every anchor/
  payload/handle expansion even when the caller already holds the ID; GitHub
  thread/reply bodies expand only through authorized anchor /
  `feedback_expand` and return the exact retained-source payload with coverage
  metadata; and expired, missing, redacted, and corrupt evidence return safe
  typed tombstones rather than a silent empty result. Restart-stability
  fixtures prove cursors and anchors remain valid and resumable across a
  daemon restart.
- **Overlay/privacy canary fixtures** prove unsaved, dirty, or private source
  never reaches a durable finding, payload, LCM node, cache, replica, export,
  or any GitHub-bound evidence, matching the positive/negative contract
  already required by
  [Plan 22](22-incremental-context-scout-and-suggestion-envelopes.md).
- **LSP projection fixtures** prove the Plan 35/§6 contract exactly:
  `Diagnostic.range` binds to the current function/symbol after remap;
  `source` is exactly `github-review`, `tracedecay-ci`, or
  `tracedecay-proximity` for these three pillars; `data` carries stable
  finding ID, `RetrievalAnchorId`, item/thread lifecycle, ingress provider
  outcome, and coverage — never a large payload; `codeDescription.href` is
  present only when authorized and safe and is omitted otherwise;
  `relatedInformation` contains only valid LSP Locations plus bounded
  messages and never pointer-only reply records or copied bodies; GitHub
  replies and full thread text expand only through authorized anchor /
  `feedback_expand`; severity never exceeds what the source evidence supports;
  and resolution, deletion, head-SHA/content/generation change that
  invalidates a remap, or supersession clears or republishes the diagnostic
  exactly once.
- [Plan 14](14-historical-failure-regression-matrix.md) names this plan's
  PR11–PR17 rows before any owning PR is considered complete.

## Rejected designs

- **LSP as the universal feedback transport:** rejected because hooks,
  GitHub ingestion, CI localization, and proximity have native transports
  that do not implement or need the LSP document lifecycle; forcing them
  through LSP would recreate the "blind JSON-RPC proxy" Plan 35 already
  rejects.
- **A second durable finding/evidence model for the feedback capsule:**
  rejected because Plan 09/35/13 already define canonical diagnostic/provider/
  anchor identity; the capsule references it instead of copying it.
- **Any GitHub write, gated or autonomous:** rejected outright. Posting,
  updating, resolving, dismissing, or replying to a PR comment or thread has
  no policy grant, receipt, or supersession-as-write path anywhere in this
  plan; GitHub REST/GraphQL is read-only ingress, full stop.
- **Automatic agent continuation, follow-up, or a repeating edit-fix loop:**
  rejected; this cycle is one-shot per trigger (§2), suggested next actions
  are inert, and no `followup_message` or scheduling exists.
- **A generic multi-agent scheduler or file-lock service riding on
  proximity data:** rejected;
  [Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md)'s
  tombstone stands, and proximity stays advisory at every tier.
- **A GitHub-specific suggestion channel parallel to Scout:** rejected; PR
  review reuses the one capsule and suggestion channel owned by
  [Plan 22](22-incremental-context-scout-and-suggestion-envelopes.md).
- **Irreversible truncation of large evidence:** rejected; every oversized
  result uses Plan 21's reversible truncation and typed budget error, never a
  silently shortened body.
- **LCM/session narrative as evidence authority:** rejected; Plan 23's
  narrative and summary-DAG retrieval may help find a finding but can never
  replace or outrank its canonical GitHub/CI/diagnostic/branch/commit
  evidence.
- **Treating [Plan 32](32-dynamic-workflow-runtime-and-sdk.md) as a
  prerequisite for the read-only/advisory first milestone:** rejected; Plan
  32 only optionally composes this plan's already-shipped operations as PR17
  workflow steps and owns no PR9–PR16 capability defined here.
