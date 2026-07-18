# TraceDecay V2 Canonical Task/Work Graph and Kanban Plan

**Status:** required product work. PR17 delivers it with
[Plan 32](32-dynamic-workflow-runtime-and-sdk.md); PR18 stabilizes its public
SDK bindings.

## Decision

TraceDecay owns one first-class, typed task/work graph for user and agent work.
Tasks and tickets are presentation vocabulary for canonical work items. Kanban
is one saved projection over the graph, alongside DAG, timeline, causal,
critical-path, workload, executor, repository, and history projections.

This plan does **not** revive the removed V2 rewrite machinery. The Markdown
files in this directory, `NEXT.md`, contributor checklists, completion ledgers,
PR sequences, and developer-roadmap state are documentation and Git evidence
only. TraceDecay never parses, imports, schedules, executes, or infers product
work from them. Product work enters through explicit typed application
commands or authorized imports whose source is product data.

## Outcome

An authorized user can create or observe an initiative that spans projects and
repositories, decompose it into a versioned dependency DAG, relate every work
item to the evidence and execution that produced its state, and inspect the
same canonical selection through several useful views. Agents receive only
their authorized task slice and evidence; no host, board, path, provider, or
dashboard route becomes a second task authority.

The graph connects:

- initiatives, product work plans, immutable plan versions, work items,
  milestones, blockers, typed dependencies, acceptance criteria, decisions,
  handoffs, and supersession;
- projects, repositories, checkouts, worktrees, branches, refs, commits,
  snapshots, pull requests, reviews, checks, releases, and deployment evidence;
- Threads, Sessions, Turns, agents, subagents, executor registrations,
  advisory work claims, runtime leases, attempts, cancellations, and retries;
- tool calls and receipts, files, symbols, diagnostics, tests, context packets,
  facts, memories, hints, skills, retrieval anchors, artifacts, outcomes,
  residual risk, tokens, latency, and cost; and
- valid time, observation time, source watermarks, entity versions, policy and
  configuration revisions, and complete immutable history.

Task identity is never a card row, title, session, branch, provider prompt,
workflow run, or external issue number. External IDs are aliases or evidenced
relations. One work item may span many sessions, attempts, worktrees, commits,
and PRs; each of those may relate to several work items without creating
copies.

## Canonical graph semantics

The profile activity owner shard stores immutable graph events and current
transactional heads. Project shards retain canonical code, Git, delivery, and
session entities and content-safe relation locators. The owning daemon remains
the only mutable store authority.

The minimum typed model includes:

- `InitiativeId`, `WorkPlanId`, immutable `WorkPlanVersionId`,
  `WorkItemId`, and immutable `WorkItemVersionId`;
- Plan 24-owned typed selection, neighborhood, history, projection, and lens
  requests with work-domain fields and legal pivots;
- typed gating dependency edges and separate non-gating evidence, similarity,
  planned-parallel, handoff, production, review, and causal-candidate edges;
- assignment and route recommendations, advisory `WorkClaim` evidence, and
  references to Plan 32 runtime step, lease, attempt, effect, artifact, and
  receipt identities;
- immutable task-shape snapshots, decomposition and resize proposals, and
  references to Plan 06 routing decisions plus Plan 26 model-capability
  profile, independent-review, calibration, and outcome revisions;
- typed auxiliary-attempt requests that bind one accepted work-item version,
  ready-node proof, parent lineage, exact scope/context, recommended provider
  backend/model/effort, grants, budgets, and fallback constraints without
  acquiring a runtime lease;
- acceptance, review, decision, exception, artifact, outcome, and cost records;
  and
- typed relations to every project/session/code/Git/delivery entity listed
  above, with provenance, confidence, temporal validity, and retrieval anchors.

Gating edges form a DAG. Informational and evidence relations may contain
cycles but never unlock work or enter critical-path calculations. Readiness is
derived from the active work-plan version, dependency and acceptance evidence,
schedules, budgets, policy, and current Plan 32 runtime state. It is not a
mutable Kanban column. Editing graph state creates immutable versions and
events; in-flight work stays pinned until an explicit revalidation,
cancellation, or supersession decision.

Assignment expresses desired ownership and route. A work claim is advisory
proximity/intent evidence. Runtime authority exists only in the current fenced
Plan 32 lease and attempt. Late receipts from an old authority epoch are
retained as stale-attempt evidence and cannot advance graph state.

Every accepted mutation records actor, scope, expected versions, idempotency
identity, policy/config/catalog/privacy revisions, causation, evidence, and
source watermark. Current projections are rebuildable from immutable history.
Missing, stale, partial, denied, or ambiguous evidence remains explicit and
cannot render as no work or successful completion.

## Plan 24 and Plan 32 authority boundary

Plan 24 owns task/work identity, graph versions, dependency and acceptance
semantics, derived readiness, task-to-evidence relations, saved projection
semantics, and legal graph transitions.

[Plan 32](32-dynamic-workflow-runtime-and-sdk.md) is the sole runtime for
executing authorized task steps. Activation of executable work creates or
references a typed Plan 32 workflow run/node pinned to the exact work-item
version, readiness digest, scope, route, budgets, grants, and acceptance
contract. Plan 32 alone schedules runnable steps, issues and fences leases,
advances runtime clocks/timers, records attempts, applies or reconciles
effects, retries, pauses, cancels, and publishes runtime receipts and artifacts.

Plan 24 projects those runtime identities and events into task history and
uses their validated receipts as graph evidence. It does not define another
scheduler, runtime clock/timer, queue authority, lease table, retry loop,
effect journal, artifact store, worker protocol, or cancellation engine. Plan
32 does not redefine task identity, dependencies, readiness, board columns, or
completion semantics. A workflow node is not automatically a work item; the
relation is explicit and versioned.

Plan 24's task-graph dispatcher derives ready-node candidates and owns
decomposition, calibrated sizing, task-domain model/backend recommendation,
and typed auxiliary-attempt request semantics. Plan 06 remains the pure
evaluator used to produce policy decisions over immutable inputs. Emitting an
auxiliary-attempt request is advisory: Plan 24 does not reserve capacity,
acquire a lease, start a process, dispatch bytes, supervise a provider, or
create an attempt. Plan 32 alone revalidates the request, acquires the fenced
lease, creates the attempt, and executes it through a typed provider adapter.

## Model-routing review and live recalibration

Each executable work-item version declares a typed task-shape profile:
objective class, decomposition role, scope and blast radius, languages and
capabilities, ambiguity, dependency depth, review risk, expected evidence,
latency/token/cost budgets, privacy/egress limits, and deterministic fallback.
The assignment decision records candidate routes, exclusions, selected
provider/model/reasoning effort, explanation, evidence horizon, policy
revision, and whether a human overrode it. The Plan 32 attempt receipt records
the requested and actual route without silently substituting either.

[Plan 26](26-observability-accounting-and-usage.md) owns privacy-safe
observations and denominator-safe metrics for:

- decomposition quality and task-shape fit;
- first-pass scope completion and accepted correctness;
- tests executed, review findings, escaped defects, and outcome confidence;
- rework count, successor/remediation work, retries, and failure causes;
- queue and execution latency, tokens, measured/estimated cost, and resource use;
- autonomy, human interventions and overrides, cancellation, and unknown
  outcomes; and
- model/provider/effort availability, coverage, policy version, and cohort
  comparability.

Typed policy consumes only eligible, versioned Plan 26 evidence to recommend
future task sizing, decomposition, model, provider, effort, reviewer, and
fallback routes. Recalibration is live for later admissions but never rewrites
an admitted attempt or silently self-modifies policy. Every recommendation is
explainable, reproducible from a pinned evidence horizon, reversible by a
human, and auditable against the deterministic baseline.

Exploration is bounded by explicit cohort eligibility, minimum sample and
coverage, route allowlists, budgets, privacy/egress constraints, maximum
exploration share, rollback thresholds, and circuit breakers. Sparse or
shifted data falls back deterministically. Anti-gaming controls separate
self-reported completion from independent tests/review/outcomes, prevent
workers from selecting their own score or denominator, retain negative and
cancelled outcomes, suppress small/private cohorts, and reject optimization
for superficial throughput at the expense of correctness, safety, or task
inflation. Raw prompts, source, review bodies, private session content, and
hidden reasoning never enter routing metrics.

## Adaptive task-intelligence contract

Task intelligence is an advisory, graph-native decision loop over immutable
evidence. It estimates work, proposes graph changes and routes, and explains
why. It never mutates a work plan, selects a model silently, admits an attempt,
or changes an evaluator/configuration revision. Only a human-authorized
application command may accept, reject, or supersede a proposal; deterministic
expiry may close one without mutation.

### Task shape and calibrated size

Every assessment binds an exact `WorkItemVersionId`, graph neighborhood,
resolved scope, code/Git generation, evidence watermark, and
policy/config/catalog/privacy revisions. Its typed feature set includes:

- objective and work class, decomposition role, deliverable and acceptance
  shape, novelty, and similarity to anchored prior work;
- algorithmic, semantic, integration, protocol, migration, verification, and
  operational complexity as separate dimensions rather than one opaque score;
- ambiguity from unresolved decisions, conflicting evidence, missing
  acceptance criteria, unknown dependencies, and requirement volatility;
- blast radius across projects, repositories, worktrees, branches, public APIs,
  stores/migrations, callers/dependents, tests, delivery paths, and users;
- context burden: required anchors, relevant symbols/files/history, dependency
  breadth, expected context refresh, and information still unavailable;
- tool and environment requirements, capability availability, setup cost,
  effect class, expected receipts, and deterministic no-tool fallback;
- concurrency shape: independently claimable regions, shared authorities,
  ordering constraints, overlap/conflict risk, integration gates, and useful
  parallelism bounds;
- security, privacy, egress, credential, supply-chain, data-loss, and
  irreversible-effect risk; and
- expected effort, elapsed time, latency, token, cost, review, and rework
  ranges.

Each feature records value or interval, unit/scale, provenance class
(`declared`, `derived`, or `observed`), evidence anchors, coverage, confidence,
and unknown reasons. Calibrated size is a distribution or ordinal band with
prediction interval, not a synthetic story-point precision claim. Estimates
for execution effort, wall time, tokens, cost, and review load remain separate
because concurrency, queueing, and model route affect them differently.

### Decomposition and live revision proposals

A decomposition proposal contains the pinned parent version, proposed child
versions, typed parent/child and gating/non-gating edges, child acceptance
contracts, scope and context boundaries, suggested integration/review gates,
parallelism constraints, estimated ranges, and evidence for every cut. It must
also explain why the proposed boundary is safer or more efficient than leaving
the parent intact. Shared-state or cross-cutting work is not falsely labeled
parallel merely because several agents are available.

Committed graph/runtime evidence may produce a new split, merge, resize,
reorder, re-scope, re-review, or re-route proposal when, for example:

- code-symbol impact, dependency, scope, or acceptance evidence expands or
  contracts;
- required context or tool capability becomes unavailable, stale, or larger
  than budgeted;
- an attempt stalls, exhausts budget, returns partial work, or exposes an
  effect/cancellation uncertainty;
- tests, diagnostics, independent review, CI, or delivery evidence changes
  risk or expected rework; or
- sibling work overlaps, invalidates an assumption, reveals a reusable result,
  or makes a planned integration gate unsafe.

The proposal cites the old estimate, new evidence, changed dimensions,
predicted consequence, confidence, coverage, and legal choices. It cannot
pause, cancel, split, merge, resize, or re-route admitted work. Plan 09
revalidates it; only an explicit human-authorized command chooses a graph
version and, where runtime work exists, a separate explicit Plan 32
pause/cancel/continue/re-admit action.

### Model capability profiles and routing

A Plan 26-owned model-capability profile is a versioned, temporally valid
evidence view keyed by provider, exact model/version, reasoning-effort class,
host/adapter, tool/capability set, privacy/egress class, and relevant
configuration/catalog revisions. Plan 24 relates the pinned profile revision
to work, assignment, recommendation, and outcome history; it does not
recalculate profile metrics. The profile separates:

- declared availability, limits, modalities, tools, context, effects, and
  pricing from observed outcomes;
- performance by eligible task-shape cohort, decomposition role, scope/risk
  band, and evidence coverage;
- first-pass scope completion, accepted correctness, test evidence,
  independent review severity, escaped defects, rework, latency, tokens,
  measured/estimated cost, autonomy, and intervention;
- route unavailability, hidden or explicit provider fallback, timeout,
  cancellation, policy denial, and outcome still unknown; and
- sample size, censoring, selection/override/exploration exposure, horizon,
  freshness, calibration error, and confidence interval.

Profiles learn across authorized sessions and attempts by joining canonical
task, route, attempt, review, remediation, and outcome identities—not by
copying prompts or summaries. Every aggregate drills through safe Plan 13
retrieval anchors to the exact eligible evidence and preserves project,
repository/worktree/branch, code-symbol impact, valid/observation time, and
retention/privacy scope. Cross-session evidence outside the requester's scope
may contribute only through an authorized privacy-safe cohort; it never reveals
another session's content or becomes an unscoped context packet.

A routing recommendation ranks only policy-eligible routes and may recommend a
primary executor, reasoning effort, independent reviewer, tool/context packet,
budgets, and deterministic fallback. It records exclusions and trade-offs
instead of collapsing quality, safety, latency, and cost into one reward.
Requested and actual routes remain distinct through Plan 32 receipts. The
attempt worker cannot grade itself, select its comparison cohort, or act as its
sole independent reviewer.

### Proposal and evaluation states

All task-intelligence artifacts are immutable revisions. Proposal lifecycle is:

```text
Proposed -> UnderReview -> Accepted | Rejected | Superseded | Expired
```

`Accepted` records the explicit command/actor and resulting graph version or
runtime-control reference; it is not itself a graph mutation. An evaluator may
instead terminate without a proposal as `Abstained`, with one typed reason:
insufficient eligible evidence, incomplete coverage, ambiguity above policy,
no eligible route, privacy/authorization denial, stale/invalidated inputs,
budget/cancellation, evaluator unavailable, or model/version drift. A
deterministic baseline result is `FallbackRecommended`, not a disguised
high-confidence recommendation.

Plan 24's outcome-dependent graph transitions consume the exact versioned
Plan 26 label schema; Plan 24 does not define another outcome vocabulary.
Using Plan 26-owned labels, outcome state is independent of attempt state:

```text
Pending -> ObservedPartial -> Reviewable -> Accepted | Rejected
Pending | ObservedPartial | Reviewable -> Censored | Unknown
```

Cancellation, timeout, lost authority, supersession, or an unfinished
observation horizon can censor an outcome without turning it into failure or
success. Late evidence appends a new outcome revision. First-pass means the
first admitted attempt against the pinned work-item/acceptance version before
remediation; changing scope or acceptance creates a new comparison identity
rather than laundering rework into a first pass.

Plan 26 independent-review records and labels carry dimension grades, findings
by severity, evidence coverage, reviewer route and relationship to the attempt,
tests/reproduction, accept/reject/partial judgment, residual risk, and
conflicts. Plan 24 consumes their identity/revision when evaluating acceptance
evidence and legal transitions. A Plan 26-labeled non-independent or missing
review cannot satisfy a Plan 24 independent-review gate.

### Evidence flow and operations

The product flow is:

```text
authorized product work + anchored graph/code/Git/session evidence
  -> Plan 24 task-shape snapshot and decomposition/resize candidates
  -> Plan 06 typed recommendation under Plan 20 configuration
  -> Plan 09 authorization, revalidation, and proposal review
  -> explicit accepted Plan 24 graph version
  -> Plan 32 admission, lease, attempt, effect, artifact, and receipt history
  -> Plan 26 observations, outcome/review labels, cohorts, and calibration
  -> later Plan 06 recommendations from a pinned evidence horizon
```

PR17 must provide typed product operation concepts for:

- task-shape assessment and explanation;
- decomposition proposal creation, comparison, review, acceptance, rejection,
  expiration, and supersession;
- routing recommendation and deterministic fallback explanation;
- live split/merge/resize/re-route proposal and review;
- independent review-grade recording and conflict disclosure;
- outcome recording and later evidence attachment; and
- calibration reports by estimate dimension, task-shape cohort, route, and
  horizon.

These are semantic operation families, not frozen PR18 public method, command,
or MCP-tool names. Plan 09 owns the transport-neutral use cases, Plan 08 the
capability definitions, Plan 21 the compact CLI/MCP bindings, and Plan 17 the
later stabilized public API/SDK names.

### Auxiliary-agent request semantics

An accepted decomposition or route proposal may yield one immutable typed
auxiliary-attempt request for a ready node. This is the graph-native successor
to a host-local "spawn an agent" command. It contains:

- the initiative, work-plan/version, parent/child work-item/version,
  ready-node digest, acceptance contract, proposal/decision references, and
  idempotency identity;
- exact project, repository, checkout/worktree generation, branch/ref/commit,
  code generation, parent runtime attempt, parent Session/Turn, and requesting
  actor identities;
- a bounded retrieval-context manifest of Plan 13 anchors, permitted sibling
  summaries, relevant code/Git/test evidence, exclusions, freshness, coverage,
  and context/token ceilings—never an unbounded board or copied transcript;
- the recommended provider backend, exact provider/model/version and reasoning
  effort constraints, required tool/protocol capabilities, deterministic
  fallback set, and why every alternative was included or excluded;
- sandbox, approval, network/egress, filesystem, tool/effect, environment,
  secret-reference, deadline, cancellation, output, artifact, token, and cost
  constraints; and
- the expected structured event, progress, artifact, terminal receipt, and
  independent-review contracts.

The request carries typed argument and standard-input fields for a Plan 32
provider adapter; it never carries a shell command string, shell fragment,
ambient executable lookup, or provider-owned task text as authority. A
provider may receive only an argument vector and bounded stdin/protocol
envelope assembled from authorized fields. Environment values are an explicit
allowlist; credentials remain opaque references resolved only at the Plan 32
execution boundary and never enter the graph request.

Claude-designated auxiliary work recommends the native Claude Code CLI
backend. It never routes through Hermes Anthropic or treats Hermes as a Claude
provider. Codex-designated work recommends the structured Codex app-server
backend when its negotiated protocol/version/capabilities satisfy the request.
A separately cataloged Codex CLI backend is an explicit fallback only when the
pinned Plan 06 decision and Plan 20 configuration allow it; fallback is never
hidden, and requested and actual backends remain separate evidence.

The request grants no task-dispatch, graph mutation, runtime-control, lease,
or provider-selection capability to the auxiliary agent. An auxiliary attempt
cannot recursively create or admit another auxiliary attempt, accept a
proposal, change readiness, resize or re-route itself, or mark work complete.
Provider-native child/subagent activity is disabled for canonical dispatch or
retained only as non-authoritative observation when unavoidable. The attempt
returns structured evidence, artifacts, progress, and a terminal outcome to
Plan 32; Plan 24 may use validated receipts as evidence for a later
human-authorized graph transition or proposal.

Request evaluation distinguishes `Unsupported`, `Absent`, `Stale`,
`Cancelled`, `TimedOut`, `Failed`, and `Partial` from an admitted or completed
attempt. These states cannot become a different backend, successful
completion, or clean empty result. Plan 32 owns runtime forms of the same
outcomes after admission.

### Learning, drift, and anti-gaming

There is no opaque online weight mutation, hidden model fine-tuning, or
self-authored reward function. PR17 uses reviewed, versioned, replayable
estimators over explicit features and labels. Formulas, cohort filters,
priors, thresholds, confidence/coverage rules, evidence horizons, and
calibration error are inspectable and configuration/policy bounded.

- **Cold start:** use declared capability constraints and conservative reviewed
  priors, mark observed coverage absent, and select the deterministic fallback.
  Do not present capability marketing or another model family as measured
  correctness.
- **Sparse data:** widen intervals, coarsen only to a documented eligible
  parent cohort, suppress private/small cells, and abstain when the parent
  cohort is not comparable.
- **Model/version drift:** create a new capability-profile revision and cohort.
  Old versions remain historical evidence; they do not silently score a new
  version. Explicit bridge evidence may inform a prior but never erase the
  version change.
- **Nonstationarity:** retain valid and observation time, use declared rolling
  or fixed horizons and change-point evidence, expose stale/shifted cohorts,
  and compare current versus historical calibration before recommending.
- **Censored failures:** retain cancellation, timeout, unavailable route,
  partial review, and unknown terminal evidence in the eligible population;
  publish censoring/coverage bounds and refuse a success ranking when missing
  outcomes could reverse it.
- **Causal claims:** attempt, production, review, remediation, and outcome
  edges record what evidence caused or assessed what. Observational route
  correlations remain labeled non-causal unless an eligible, policy-approved
  experiment or defensible comparison establishes otherwise.

Anti-gaming freezes work/acceptance identity before first-pass evaluation,
normalizes child outcomes back to their parent/initiative, charges
decomposition and integration overhead, preserves rejected/cancelled/unknown
attempts, and prevents task inflation from improving throughput denominators.
No policy optimizes a single proxy such as cards closed, tests run, review
count, tokens, latency, or cost. Security/privacy violations, hidden route
substitution, severe escaped defects, and duplicate effects are hard adverse
outcomes rather than tradeable score components.

## Projections and Work experience

Saved views store an authorized Plan 24 typed selection/projection request,
scope, lens, grouping, and layout—not copied task rows, independent status, a
board filter DSL, or a universal cross-domain query AST. Plan 05 may execute
the request through shared scope, budget, cancellation, cursor, watermark,
merge, coverage, and explanation primitives, but it cannot redefine the
selected work entities, edges, readiness, lens, or legal pivots. Required
projections are:

- **Kanban:** derived readiness/resolution lanes with blockers and legal next
  actions. Dragging a card invokes an explicit authorized graph or Plan 32
  command; it never writes a status string or bypasses dependencies.
- **DAG/critical path:** gating edges, fan-in/fan-out, subplans, cycle
  diagnostics, estimates, observed ranges, slack, and unknown segments.
- **Timeline/history:** graph versions, sessions/Turns, assignments, claims,
  leases, attempts, tools, artifacts, commits/PRs/checks, reviews, costs,
  cancellations, retries, and outcomes on valid and observation time.
- **Causal:** evidence-backed production, decision, failure, and impact edges;
  temporal proximity is visually distinct from proven causation.
- **Workload/executor/model:** queue, running, blocked, review, age, capacity,
  overlap, cost, route quality, failure/rework, and coverage by authorized
  initiative/project/agent/executor/provider/model/effort.
- **Repository/delivery:** exact repository/worktree generation, branch/ref,
  commit, PR/check/review/release, freshness, ownership, and retention state.

One item may appear in several projections without copies. Selection, scope,
time, and evidence anchors survive lens changes. Large views use bounded
server-side neighborhoods, aggregation, cursors, and accessible list/table
fallbacks; the browser never loads or evaluates the global graph.

The dashboard adds a first-class **Work** workspace and cross-links it with
Brain, Explorer, Loom, Sessions, Agents, Code, Delivery, Automations,
Observatory, Costs, and Settings. The UI renders typed application views and
legal actions only. It owns no readiness, routing, lease, scoring, policy, or
effect logic.

## Hermes prior art and TraceDecay improvements

Hermes Agent Kanban is useful prior art, not runtime authority or a
specification to copy. The recovered dossier identified durable tasks,
dependency graphs, atomic claims, worker context, retries, worktree/model
controls, compact worker tools, runs, and a rich board as valuable interaction
and regression evidence. It also identified ambient/current-board selection,
board-local databases and IDs, overloaded status/assignee fields, host-local
PID authority, weak distributed fencing, free-form metadata, and dashboard
business logic as unsuitable for TraceDecay.

Any direct or behavioral port must pin an official source revision and bounded
source/test span, record license and disposition, and prove the TraceDecay
regression it serves. Independent TraceDecay-native work does not wait for a
whole-Hermes port inventory.

The reviewed prior-art baseline is
`NousResearch/hermes-agent@c48d53413aa2c09f6d5703082361c2754f1d5350`.
The following source-backed outcomes are adopted without copying their storage
or API shape:

- `hermes_cli/kanban_db.py` has mutable `tasks` plus append-only
  `task_events` and historical `task_runs`; TraceDecay keeps the useful durable
  event/run history but makes immutable temporal task events and canonical
  graph projection authoritative. Familiar Hermes lanes (`triage`, `todo`,
  `scheduled`, `ready`, `running`, `blocked`, `review`, `done`, `archived`) are
  UX evidence for derived Kanban lanes, never stored lifecycle truth.
- Hermes rechecks parent completion during its atomic ready-to-running claim.
  TraceDecay directly fixtures the stronger invariant: Plan 32 must revalidate
  the exact Plan 24 ready-node and dependency digest immediately before
  acquiring a fenced lease; a stale ready projection cannot dispatch.
- `hermes kanban` and the web dashboard prove demand for concise list/detail,
  dependency, assignment, block/review, run/event tail, diagnostics,
  dispatch-status, statistics, comment/attachment, and recovery affordances.
  Plans 21 and 11 expose equivalent product outcomes over one graph rather than
  cloning the command names or board database.
- `KANBAN_GUIDANCE` plus `agent/kanban_stop.py` prove that workers need visible
  task instructions, skills, heartbeat, artifact, block, and terminal-protocol
  guidance. TraceDecay makes applicable skill/hint/capability IDs discoverable
  in the bounded request and surfaces protocol help, but only a typed Plan 32
  terminal receipt can close an attempt. Plain-text exit or a bounded reminder
  never marks work done or writes a follow-up task.
- `tests/hermes_cli/test_kanban_swarm.py` demonstrates a useful
  root-to-parallel-workers-to-verifier-to-synthesizer shape. TraceDecay uses it
  as a decomposition fixture with stable graph identities, independent review,
  and gated synthesis; no fixed swarm template or direct LLM-written child
  cards become authority.
- `tests/hermes_cli/test_kanban_block_kinds.py` demonstrates dependency,
  needs-input, and capability blockers plus recurrence memory. TraceDecay
  preserves typed blocker causes and repeat-block evidence so policy can
  propose human review/triage; unblock never erases history and no cron loop
  silently re-dispatches the same blocked work.
- `tests/tools/test_async_delegation.py` demonstrates durable undelivered
  completion, exclusive delivery acknowledgement, one-time restart replay, and
  abandoned-running state becoming unknown. TraceDecay requires the stronger
  Plan 32 fenced receipt/recovery form and never treats process-local in-flight
  state as recoverable authority.
- `tests/tools/test_kanban_redaction.py` proves comments, completion summaries/
  metadata, and block reasons are secret-bearing durable sinks. Equivalent
  TraceDecay event, receipt, artifact metadata, blocker, review, and hint sinks
  receive secret-canary fixtures before persistence.

Rejected mechanisms remain explicit: per-board mutable-card authority,
profile-string routing, PID/TTL claims, direct auxiliary-LLM decomposition that
writes children, free-form comments as the blackboard/outcome protocol,
process-local child authority, same-workflow self-grading, and recursive
agent-driven dispatch.

TraceDecay improves on that prior art through:

- graph-native links to code, sessions, tools, Git/delivery, evidence, and
  outcomes instead of a board-centered database;
- immutable temporal history and as-of/evolution queries rather than current
  row state alone;
- canonical repository, checkout, worktree generation, branch/ref, commit, and
  snapshot identity instead of ambient paths;
- fenced Plan 32 runtime authority, explicit recovery, cancellation, and
  unknown-effect handling across hosts;
- evidence-driven model-performance review and auditable recalibration; and
- several synchronized projections rather than treating Kanban as the product.

Hermes may be an observed source or a Plan 32 executor adapter. It never owns
canonical tasks, scheduling, leases, policy, or storage.

## Cross-plan ownership

- Plans 01–04 own shared IDs/types, owner-shard persistence, and generic
  projector infrastructure used here. Plan 24 owns work projection semantics
  and its typed domain requests.
- Plan 05 owns only shared query-execution primitives reused by Plan 24:
  scope/budget/cancellation, cursors, watermarks, deterministic merge,
  coverage, and explanations. It owns no task/board request schema, graph
  semantics, lens, or universal query AST.
- Plan 24 owns task-domain ready-node, decomposition, sizing, and model/backend
  recommendation semantics and their graph artifacts. Plan 06 owns the pure
  evaluator/policy-decision mechanics over immutable inputs, never runtime
  scheduling authority.
- Plan 08 owns auxiliary-provider capability descriptors. Plan 20 alone owns
  provider executable/configuration definitions, resolution, versions, and
  fallback policy. Plan 27 owns native executable discovery against that
  snapshot, host packaging/install/repair, probes, and conformance. Neither
  Plan 08 nor Plan 27 invokes or supervises an attempt.
- Plan 09 owns typed task/work application commands, authorization,
  idempotency, graph transactions, context assembly, and the Plan 32 bridge.
- Plans 10, 21, and 17 own HTTP/SSE, CLI/MCP presentation, and official
  Rust/TypeScript/Python API/SDK bindings over those application contracts.
- Plan 11 owns the Work UI and every visual projection.
- Plans 13, 16, 18, 22, 23, 28, 35, 36, and 37 retain their existing
  provenance, scope, privacy, advisory delivery, temporal retrieval, host,
  remote, diagnostics, Git, and feedback-cycle authority.
- Plan 26 owns observations, accounting, evaluation cohorts, coverage,
  model-capability profile/calibration read models, the canonical
  independent-review/task-outcome label vocabulary and measurement schema, and
  model-routing metrics. Plan 24 consumes pinned labels for graph transitions;
  Plan 26 never executes or changes policy.
- Plan 32 owns the one workflow runtime clock, scheduler, history, lease,
  attempt, effect, and artifact authority.
- Plan 14 owns the direct cross-cutting regression classes and Plan 33 owns
  end-to-end performance gates.

## Safety and privacy invariants

- TraceDecay never autonomously creates, stashes, cleans, resets, rebases,
  merges, deletes, pushes, force-pushes, or rewrites Git state. Existing typed
  Git preview/apply operations remain explicit user-authorized effects.
- GitHub review data is read-only ingress. No task, workflow, board action,
  route policy, or model may post, update, resolve, dismiss, or reply to a
  GitHub comment.
- Workers receive bounded sanitized context and attempt-scoped capability
  grants, never global-board dumps, store access, broad credentials, hidden
  reasoning, or unrelated sibling content.
- Scope, privacy, authority, acceptance, effect reconciliation, and
  cancellation uncertainty fail closed. A process exit, card move, commit,
  PR, model self-report, or elapsed time alone never proves completion.
- Plan 22 proximity remains advisory. Only an explicit authorized graph command
  and Plan 32 admission may create executable work.
- Retention, redaction, deletion, backup, restore, remote fencing, and
  authorization follow the existing daemon/store authorities; this feature
  creates no alternate database or host-local durable state.

## Delivery and acceptance

PR17 delivers one coherent **advisory task-intelligence loop** with Plan 32,
not a scoring-only backend or UI-only prototype:

1. create explicit product work and an immutable task-shape assessment;
2. propose and review a parent/child decomposition with calibrated ranges;
3. recommend an eligible executor/model/effort and independent reviewer with
   explanation, confidence/coverage, abstention, and deterministic fallback;
4. explicitly accept a graph version, emit one typed auxiliary-attempt request,
   and admit one mapped Plan 32 task step through a negotiated provider adapter;
5. record requested/actual route, attempt/runtime evidence, independent review,
   outcome, rework, latency, tokens/cost, and autonomy through Plan 26; and
6. replay a calibration report and generate—but never auto-apply—a justified
   split/merge/resize/re-route proposal after evidence changes.

The slice includes domain/store contracts, graph projections/query, typed
application commands, runtime mapping, pure policy inputs/results, Plan 26
observations/read models, CLI/MCP/HTTP bindings, dashboard Work views, and host
execution adapters. It ships representative deterministic estimators and
fixtures for bounded work classes; unsupported task shapes abstain rather than
pretending universal intelligence.

PR18 freezes public API names/schemas and ships Rust/TypeScript/Python SDK
parity for the accepted PR17 semantics. It may improve ergonomics but cannot
redefine task shape, proposal states, routing evidence, or runtime authority.
PR20 optimizes graph projection, evidence aggregation, recommendation,
calibration, and live-proposal latency after representative PR17 baselines; it
does not defer obvious PR17 bounds, cancellation, or fallback behavior.

Acceptance requires direct tests proving:

- versioned DAG creation/change, cycle rejection, readiness, supersession,
  as-of history, and deterministic projector rebuild;
- exact project/repository/worktree/branch/snapshot scope and many-to-many
  relations across sessions, agents, tools, code, commits, PRs, and checks;
- one Plan 32 runtime authority, fenced stale attempts, idempotent receipts,
  cancellation/recovery, no duplicate effects, and no second runtime clock,
  task scheduler, lease, attempt, or effect authority;
- Kanban/DAG/timeline/causal/workload/repository views select the same canonical
  entities and preserve scope, time, coverage, and legal actions;
- model-routing grades and recommendations are reproducible, coverage-aware,
  privacy-safe, resistant to self-report gaming, bounded in exploration, human
  overridable, and deterministically fall back;
- task-shape fixtures cover complexity, ambiguity, blast radius, context/tool
  burden, concurrency, security/privacy risk, calibrated size intervals, and
  unknown feature coverage without a universal opaque score;
- decomposition fixtures cover parent/child identity, gating versus
  informational edges, independent work versus unsafe overlap, integration
  overhead, cycle rejection, explicit review, and no mutation before
  acceptance;
- auxiliary-request fixtures cover exact graph/scope/parent lineage, bounded
  retrieval anchors, typed argv/stdin, model/backend reasoning, sandbox/grants,
  opaque secret references, deadlines, cancellation, output/artifact
  contracts, deterministic provider selection, and the absence of shell
  strings or recursive dispatch authority;
- pinned-Hermes translation fixtures cover derived familiar Kanban lanes over
  immutable history, ready-node revalidation at fenced admission, typed
  dependency/needs-input/capability blockers with recurrence history,
  reviewed parallel-worker/independent-review/gated-synthesis decomposition,
  durable terminal evidence and one-time delivery, discoverable skills/hints,
  and secret-safe comments/blockers/artifact metadata without copying Hermes
  card fields, CLI names, or profile/PID authority;
- live evidence fixtures generate justified split/merge/resize/re-route
  proposals without changing graph or runtime state, and stale proposal
  acceptance fails closed;
- cold-start, sparse/private cohort, exact model-version drift,
  nonstationarity, censored failure, selection/override bias, hidden route
  substitution, task inflation, self-grading, and non-causal-correlation
  fixtures produce transparent fallback, abstention, suppression, or bounded
  claims;
- outcome/calibration rebuilds preserve first-pass identity, independent-review
  status, parent-normalized rework, unknown/censored denominators, confidence
  intervals, Plan 26 label/schema revision, and
  estimator/policy/config/evidence revisions; fixtures prove Plan 24 accepts no
  locally invented/coerced outcome label and Plan 32 completion alone cannot
  satisfy a graph acceptance transition;
- CLI/MCP/HTTP/dashboard semantic parity in PR17 and Rust/TypeScript/Python SDK
  parity in PR18;
- restart, concurrency, partial coverage, stale evidence, denied scope,
  secret canaries, and remote authority loss remain truthful and recoverable;
  and
- no source, test, tool, or runtime path parses or executes these V2 roadmap
  Markdown files, completion state, PR sequence, or developer plan.
