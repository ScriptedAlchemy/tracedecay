# TraceDecay V2 cross-cutting regression contract

## Status and role

This is the active historical regression and observable-behavior contract for
PR5–PR20. It preserves failures learned from V1 and pre-V2 development while product
plans replace and optimize implementation. It is not a numbered failure
ledger, exact fixture inventory, compatibility catalog, Doctor kernel, plan
parser, generated gate, or requirement that PR descriptions reference this
file.

Names of retired fixtures, matrices, packets, files, and intermediate
milestones are historical evidence only and are not features to recreate.
Only actually independently released public protocols retain compatibility.
Persisted state accepts only its exact final shape; every other shape returns
typed `ResetRequired` for explicit reset or recreation, with no storage reader,
migration, backfill, dual write, or census path. Otherwise retention is judged
by the observable behavior, platform coverage, and regression classes in this
plan.

## Outcome

No slice is complete merely because its happy path passes. Its direct product
journey must prevent, expose, and recover truthfully from the corruption,
routing, scope, lifecycle, compatibility, and authority failures
relevant to that slice.

**Regression-status correction (2026-07-27).** Direct tests now cover
cooperative cancellation of daemon startup transcript/provider ingest and
code-index reconcile work; terminal-versus-retryable startup-health
classification; active-project convergence scoping; prior-complete generation
serving during refresh; and bundled-SQLite FTS blob self-heal without treating
whole-database corruption as repairable. Those observable regressions are
delivered and their old missing-test language must not be recreated.

Operational acceptance remains open. Doctor reports
`authority_audit_unavailable` and a Cursor Core component-ownership conflict,
semantic search is disabled by an invalid configuration snapshot, incremental
index cadence is suspect, and the full suite has not completed. Plan 09 owns
Doctor composition, Plan 27 owns host component lifecycle, Plans 20/31 own
semantic configuration/activation, Plan 25 owns index freshness, and the active
PR12/PR13 slice owns aggregate test stabilization.

Concrete fixture names, record counts, files, and harness layout remain test
implementation details owned beside the callable DTOs and product tests. Tests
may consolidate cases when they retain every observable regression class,
failure injection, restart/recovery behavior, and negative authority check
below.

## Ownership

- Plan 09 implements and composes the single Doctor application use case and
  legal-remediation handoffs.
- A proposed synthetic Doctor remediation dispatcher was rejected because no
  operation owner, authorization, preview/confirmation, compare-and-swap,
  effect boundary, receipt, or rollback/recovery contract had been supplied.
  That refusal is the accepted decision, not missing work. Plan 09 composes
  only owner-supplied legal operations; absent authority remains typed
  unavailable with the owning plan.
- This plan owns Doctor's historical regression classes and observable
  behavior, not runtime health evaluation or remediation execution.
- Plan 11 renders canonical Doctor findings, evidence, coverage, and supplied
  legal actions without computing health.
- Plan 20 owns configuration truth; Plan 26 owns measurement definitions and
  denominator-safe read models; Plans 27 and 32 supply host/runtime evidence
  and their own confirmed remediation operations.
- Each product owner retains its state, policy, effects, receipts, and direct
  tests. Historical evidence is a reason for a test, not architecture to copy.

## Delivery-first regression journeys

### PR14 finding to verified remediation

A direct product test begins with a canonical finding from Brain, Explorer,
Loom, Code, or Observatory; preserves exact scope, provider state, provenance,
coverage, freshness, omissions, and evidence identity; calls Plan 09's Doctor
use case; previews and confirms only an owner-supplied remediation; resumes its
durable receipt across reload and daemon restart; independently observes the
result; and reconciles effective Settings plus truthful Observatory/Costs
state.

Journey branches cover:

- loading, ready, complete-zero, stale, partial, denied, unauthorized,
  redacted, locked, conflicting, offline, unknown, cancelled, timed-out,
  error, and unsupported-schema UI states;
- semantic providers that are unsupported, absent, indexing, stale,
  cancelled, timed-out, failed, or partial, none of which may become
  complete-zero; only supported, completed, complete-coverage zero findings
  may render complete-zero;
- complete, partial, unknown-denominator, mixed-score-kind, uncalibrated,
  omitted, redacted, locked, cross-authority-disagreement, and unsupported
  evidence, including authorization-checked expansion and stale-generation
  suppression;
- exact deep-link scope/entity/version/time/selection/anchor preservation with
  no CWD, active-checkout, current-version, title, card-index, or renderer
  fallback;
- current, outdated, resolved, edited, and deleted GitHub item lifecycle kept
  separate from complete, partial, unavailable, denied, rate-limited, stale,
  and failed ingress outcome; outbound policy denial/effect suppression never
  becomes ingress state and makes no GitHub write;
- CI provenance with stale/partial/unavailable source state and no raw-log or
  execution claim; proximity emitted/suppressed/expired/risk-class state with
  Plan 20 threshold provenance and no lock or schedule;
- source progress, cancellation, stale-event suppression, reconnect
  redelivery, revision-gap refetch, bounded SSE churn, partial denominators,
  and no false zero;
- stable cross-view selection, legal action, scope, temporal, graph/entity,
  evidence-anchor, and coverage semantics across the default renderer,
  semantic table, Brain, Explorer, Loom, Code, and other shipped workspaces;
- default permissive rendering, optional-renderer parity, WebGL/init/context
  loss fallback, keyboard-only completion, focus restoration, reduced motion,
  contrast, zoom/reflow, accessible table alternatives, screen-reader critical
  journeys, and Plan 11 usability/performance budgets; and
- unavailable providers, executable/protocol/configuration drift, invalid
  fallback, environment/sandbox/capability mismatch, restart/reconnect/resume
  failure, stuck/unknown attempt, incomplete telemetry, source disagreement,
  denied action, failed remediation, and truthful no-change or reconciliation.

Dispatch, preview, confirmation, or a successful HTTP response is not verified
recovery. The journey re-observes the owning state and keeps native Git,
configuration, runtime, tests, CI, policy, and measurement authority separate.

### PR17 work to independently reviewed outcome

A direct product test begins with one canonical Plan 24 Work item, preserves
TaskId and graph version across Kanban, DAG, timeline, causal, workload,
executor/model, repository/delivery, evidence, and history projections,
executes through Plan 32 and a supported real provider, observes leases,
attempts, ordered progress, artifacts, cancellation/recovery, topology,
Git/test/CI receipts, and Plan 26 measurements, distinguishes runtime
completion from task acceptance, and explicitly dispositions a versioned
replan.

Journey branches cover:

- no-Git, unbranched, independent-branch, local-stack, and PR-stack work;
  complete worktree lifecycle; linear and branched dependency topology;
  required/produced commits; merge order; head/base/merge-base/generation drift
  and retarget;
- disjoint and overlapping concurrent edits; mechanical, semantic, combined,
  unknown, false-positive, and false-negative conflict evidence; clean
  preflight and exact approved native integration without auto-resolution;
- dependency, needs-input, and capability blockers; recurrence and human
  escalation; graph-version/CAS failure; proposal pending/accepted/rejected/
  superseded/stale; deterministic fallback and abstention;
- requested versus actual provider/backend/executable/protocol/model/reasoning;
  unsupported/absent/stale/version-drift capability; malformed, out-of-order,
  truncated, or missing-heartbeat streams; capacity deferral; reconnect,
  resume, cancellation, kill escalation, restart, unknown effects, and
  completed-but-unaccepted outcomes;
- role-isolated independent review, minority evidence, self-grading and
  recursive-dispatch rejection, harmful recall quarantine, sparse/private/
  shifted cohorts, censored/unknown outcomes, calibration drift, route
  propensity, and no locally invented labels;
- process/daemon failure before and after each effect boundary, idempotent
  replay, duplicate-effect prevention, stale lease/authority rejection,
  committed/partial/reconciling/effect-unknown truth, and no backward ref move;
- Linux, Windows, and supported macOS path/process identity; moved and
  symlinked repositories; drive/UNC/case/long-path/cross-volume behavior;
  retention and deletion; hook/LSP/CLI/MCP/HTTP/dashboard fanout where owned;
  lossless TaskId drilldown; and no credentials, prompts, or private source in
  logs and fixtures.

Claude-designated execution uses native Claude Code, not Hermes Anthropic.
Codex app-server and policy-eligible CLI fallback remain distinct. Provider
terminal text, dashboard state, or a successful check cannot synthesize graph
acceptance, effect success, or an independent outcome.

## Regression ownership by delivery slice

- **PR5:** partial, malformed, duplicated, truncated, reset, or replaced
  provider input never advances beyond a complete sanitized frame; restart
  resumes without gaps.
- **PR6:** provider-native identity/order, projection replay, partial input,
  relation preservation, and backpressure never duplicate, skip, corrupt, or
  overclaim observations.
- **PR7:** project/profile ownership, facts, memory, anchors, copied prompts,
  correction, redaction, deletion, and provenance never cross authority or
  lose safe lineage.
- **PR8:** temporal/LCM copies, summaries, supersession, cursors, stale shards,
  current/as-of/evolution reads, and no-result states remain truthful and never
  repair storage during a read.
- **PR9:** deterministic code generations, typed edge/coverage authority,
  protected exact identifiers/phrases, parse/lineage abstention, affected-test
  semantics, and dirty/stale diagnostic suppression survive restart and
  partial language support.
- **PR10:** semantic search never substitutes models, crosses project scope,
  recomputes unchanged documents, demotes protected exact results, hides tail
  failure, or changes lexical fallback bytes/order after model failure.
- **PR11:** policy, application, configuration, catalog, analyzer, edit/Git
  transactions, and branch-aware feedback remain authorized, deterministic,
  idempotent, receipt-backed, and free of surface-local business
  logic or guessed clean outcomes.
- **PR12:** CLI, MCP, HTTP, output, catalog, and LSP agree on lifecycle,
  framing, negotiated capabilities/versions, cancellation, stable problems and
  retry directives, schemas, paging, rendering, diagnostics/impact readers,
  the TraceDecay context extension, and explicit unavailable state. Rename,
  arbitrary forwarding, and code actions cannot self-apply.
- **PR13:** synchronous hooks stay bounded and return only receipt/already-ready
  guidance; model/GitHub/feedback work is asynchronous. Saved-content feedback,
  CI localization, read-only GitHub lifecycle, proximity, host capability,
  Kimi Code manifest/global `PostToolUse`/`Stop`, OpenCode local-plugin edit/
  tool/session/LSP events, Cline-family route/unavailable evidence,
  cross-platform official lifecycle validation, competing-extension/interruption/
  host rollback, and the feedback rollback switch retain capture redaction, no-write,
  restart, truthful coverage, and all previously supported host features.
- **PR14:** the direct Doctor/dashboard journey above retains canonical state,
  evidence, accessibility, renderer, observability, configuration, and legal
  remediation behavior without a second kernel.
- **PR15:** explicit multi-root/worktree/ref/collection/stack scope never falls
  back to ambient identity; per-root query/LSP/feedback coverage remains
  visible; stack fanout obeys debounce, preflight concurrency, batch,
  overflow, watermark, restart, and circuit-breaker contracts; Git apply is
  exact, approved, mechanical, and receipt-backed.
- **PR16:** remote fencing, offline replay, cache verification, backup,
  restore, failover, retention, and deletion never admit two writers or move
  unsaved overlays/analyzers/proximity into durable or remote state.
- **PR17:** canonical work identity, readiness, proposals, task intelligence,
  separate ephemeral expertise, provider/runtime authority, execution
  topology, review, outcomes, handoff, replanning, and Git integration retain
  explicit versions, consent, lifecycle, receipts, and no auto-apply.
- **PR18:** every supported public operation has Rust and TypeScript
  behavioral/lifecycle conformance, including paging, streams, cancellation,
  retry directives, reconnect/resume, cross-version behavior, and the
  diagnostic handoff-token journey.
- **PR19:** exact-final persisted-state admission retains one writer and the
  canonical route; every other shape returns `ResetRequired` for explicit
  reset/recreation, and superseded storage paths are deleted.
- **PR20:** accepted production comparisons cannot weaken semantics,
  authority, project isolation, ordering, coverage, durability, crash/restart
  correctness, or hide tail/resource regressions behind averages.

## Cross-cutting direct acceptance

- Each owning suite injects relevant corruption, disk-full, process death,
  concurrent writer, partial shard/input, wrong scope, stale identity,
  provider ambiguity, authorization loss, unsupported platform, prohibited
  log/fixture content, cancellation, and restart/recovery failures through the real product
  path, not only validation before work starts.
- LSP/gateway tests cover malformed/interleaved frames, notification/response
  confusion, stale generations, conflicting overlays, cancellation races,
  analyzer restart exhaustion and disagreement, competing extensions,
  graph-only/analyzer-only coverage, OpenCode custom-LSP registration without
  duplicate analyzers, negotiated context-extension envelopes and expansion
  handles, version-monotone clear/republish, and no edit authority.
- PR13 host conformance and lifecycle tests exercise Kimi Code and OpenCode
  alongside every previously supported host on each supported platform,
  including install/update/repair/uninstall, real edit and stop feedback,
  competing registration, interruption, ownership-preserving rollback, and
  the direct feedback rollback switch.
- Plan 37 tests keep one-shot termination, GitHub item lifecycle, GitHub ingress
  outcome, semantic-provider state, CI provenance, proximity threshold,
  expansion, multi-root fanout, and remote fencing as separate typed
  dimensions. No automatic continuation, GitHub write, CI rerun, task
  mutation, Git mutation, schedule, or clean-by-silence is representable.
- Plan 09 Doctor tests compose source disagreements into one finding family,
  preserve coverage and owner-specific actions, execute remediation only
  through its owner, resume the receipt, and independently verify the result.
  Plan 11 rendering and this plan contain no health formula.
- PR14 and PR17 product journeys execute the observable classes listed above
  with nonzero cases/samples and report failures by product behavior. Exact
  fixture IDs, every-record execution, checked-in file inventories, generated
  matrices, PR-description references, and placeholder benchmark packets are
  not acceptance artifacts.
- Visual review, screenshots, schema equality, generated declarations,
  compilation-only parity, retries of flaky tests, or a renderer benchmark
  alone cannot close a semantic, authority, provenance, coverage, redaction,
  accessibility, lifecycle, effect, or legal-action regression.
