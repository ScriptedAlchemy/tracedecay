# TraceDecay V2 canonical task/work graph and Kanban plan

**Status:** required product work. The approved 2026-07-28 delivery decision
assigns the canonical graph, core Work projections, proposal review, separate
admission/accept-reject-replan operations, and minimal real-provider
[Plan 32](32-dynamic-workflow-runtime-and-sdk.md) runtime to PR14. PR17 extends
those authorities with advanced workflow behavior. PR18 stabilizes public SDK
names for accepted operations; it does not add missing semantics.

Earlier task-schema names, operation registries, fixture catalogs, packet
gates, and milestone/file inventories are historical evidence, not
prerequisites or features that PR17 must recreate. Only actually independently
released public operations may retain protocol compatibility. Persisted
task/work records use the fresh-store rule; all other retention is judged by
the direct Work journey, lifecycle,
platform, and regression behavior below.

No Plan 24 Work contract is established on `origin/master` or in a published
package/release. Pure source-only/internal request helpers take their final
shape in place, as do wire-visible V2 Work request revisions and branch-local
V2 work-item, graph, read, lease, evidence, journal, checkpoint, and receipt
records. Only their exact final persisted shape is accepted; any other
database, store, spool, file, or projection returns typed `ResetRequired` and
requires explicit reset or recreation. No storage reader, migration, backfill,
dual write, or census path exists. Immutable work-item and graph versions below
remain product history/CAS identities, not by themselves public protocol
release evidence.

## Decision

TraceDecay owns one host-neutral, typed task/work graph for user and agent work.
Tasks and tickets are presentation vocabulary for canonical work items. Kanban,
DAG, timeline, causal, critical-path, workload, executor/model,
repository/delivery, evidence, and history views are projections over the same
versioned graph, not separate stores or authorities.

PR14 owns and delivers this graph, Kanban, DAG, timeline, causal, workload,
basic topology read, canonical TaskId selection, `task_activity` SSE, and deep
links. PR17 consumes the same graph/runtime authorities for advanced topology,
placement, workflow definitions, automation execution, expertise/calibration,
fan-out/synthesis/recovery, and host/LSP handoff. Session-derived tasks, an
independent Kanban database, or dashboard-owned task authority remain rejected.

Roadmap Markdown, `NEXT.md`, PR sequences, contributor checklists, and
completion ledgers are documentation and Git evidence only. PR17 never parses,
imports, schedules, or executes them. Product work enters through explicit
application commands or an authorized product-data import.

## PR14 core and PR17 residual outcome

An authorized user can create work, inspect it through every retained Work
view, retrieve exact evidence, review an explained decomposition/sizing/route
proposal, explicitly admit a real provider step, inspect progress and outcome,
and review a live replan that remains unapplied until separately accepted.

The journey supports no-Git work, in-place or isolated execution, independent
or stacked branches, independent or pull-request review, safe explicit
integration, multi-agent fan-out, independent review, optional synthesis,
handoff, the separately authorized ephemeral expertise view, and outcome
calibration without changing TaskId or creating a second runtime.

## Canonical graph and history

The profile activity owner shard stores immutable graph events and current
transactional heads. Project shards retain canonical code, Git, delivery, and
session entities plus content-safe relation locators. The owning daemon remains
the only mutable authority.

The graph retains:

- initiatives, work plans and immutable versions, work items and immutable
  versions, milestones, blockers, typed gating dependencies, acceptance
  criteria, decisions, comments, handoffs, supersession, and history;
- separate non-gating evidence, similarity, planned-parallel, production,
  review, and causal-candidate relations;
- projects, repositories, checkouts, worktrees, branches, refs, commits,
  snapshots, pull requests, reviews, checks, releases, and deployments;
- Threads, Sessions, Turns, agents, subagents, assignments, advisory claims,
  runtime runs, nodes, leases, attempts, controls, effects, retries, and
  receipts;
- tools, files, symbols, diagnostics, tests, context packets, facts, memories,
  hints, skills, retrieval anchors, checkpoints, artifacts, outcomes, residual
  risk, token, latency, and cost evidence; and
- valid and observation time, source watermarks, entity versions, policy,
  configuration, catalog, provider, and estimator revisions.

Task identity is never a card row, title, session, branch, prompt, workflow run,
provider profile, path, or external issue number. External identifiers are
aliases or evidenced relations. One work item may span many sessions,
attempts, worktrees, commits, and PRs, and each may relate to several work
items without copying identity.

Every mutation records actor, typed scope, expected graph/item versions,
idempotency identity, causation, evidence, source watermark, and pinned policy,
configuration, and catalog revisions. Same-key/same-input replay
returns the original receipt; changed input conflicts. Current projections are
rebuildable from immutable history.

## Readiness, acceptance, and execution authority

Gating dependency edges form a DAG. Informational and evidence relations may
cycle but never unlock work or enter critical-path calculations. Readiness is
derived from the active plan version, dependencies, acceptance prerequisites,
schedules, budgets, policy, exact evidence, and compatible Plan 32 state. It is
not a mutable Kanban column.

Assignment is desired ownership and route. A work claim is advisory
proximity/intent evidence. Runtime authority exists only in a current fenced
Plan 32 lease and attempt. Late or stale-authority receipts remain evidence but
cannot advance graph state.

Plan 24 owns TaskId/WorkItemId, graph versions, dependencies, acceptance,
readiness, saved views, task-to-evidence relations, decomposition, sizing,
task-domain route recommendation, proposals, legal graph transitions, and the
accepted-attempt set. Plan 32 alone owns runtime clocks, queues, capacity,
leases, attempts, provider execution, progress, effects, retries, recovery,
cancellation, artifacts, and runtime receipts.

Runtime `Completed`, a commit, process exit, card move, provider summary,
artifact, test result, or elapsed time is evidence, never automatic task
acceptance. Plan 24 applies acceptance in a separate version-checked command.
Plan 24 never dispatches; Plan 32 never invents task identity, readiness,
dependency, decomposition, route policy, or completion.

## TaskId-rooted evidence retrieval

Every opaque TaskId is a stable authorized selection root for bounded current,
as-of, evolution, and forensic retrieval. The result can traverse task
versions, dependencies, attempts, reviews, outcomes, sessions, messages,
agents, tool calls, artifacts, receipts, handoffs, sibling work, code, Git, CI,
diagnostics, impact, affected tests, and delivery evidence.

Plan 24 selects typed relations and delegates hydration to the owning
application/query authorities. Plan 23 supplies session narrative; Plan 13
anchors provide exact source expansion. Summaries, compact context, and
response handles accelerate retrieval but never replace exact spans, packets,
anchors, or owning-store evidence.

Every selection, page, hydration, continuation, and expansion reauthorizes.
Possession of a TaskId, cursor, packet, anchor, attempt, or receipt grants
nothing. Coverage, omissions, freshness, disagreement, unknowns, redaction,
and source authority remain explicit. Hidden or denied relations do not leak
identity, count, timing, or existence.

The planner receives a bounded authorized evidence manifest and cannot query
stores, discover ambient capabilities, recurse through generic dispatch, or
replace exact evidence with a generated narrative.

## Decomposition, sizing, routing, and task intelligence

PR17 retains the complete task-intelligence capability:

- task-shape assessment covers complexity, ambiguity, blast radius,
  context/tool burden, coupling, concurrency, integration overhead,
  sensitive-data/network-boundary risk, and unknown feature coverage;
- sizing preserves ordinal or heuristic estimates and calibrated ranges with
  estimator, cohort, horizon, support, error, drift, and coverage;
- decomposition proposals preserve parent/child identity, gating versus
  informational edges, independent work, unsafe overlap, serial and no-split
  alternatives, fan-out and synthesis shape, integration cost, collapse
  conditions, and independent-review requirements;
- route recommendations preserve eligible executor/provider/backend/model/
  effort choices, exact exclusions, capability and content-location fit, expected
  ranges, requested and actual route, human override, exploration propensity,
  deterministic fallback, and abstention; and
- live proposals preserve split, merge, resize, reroute, re-review, restart,
  local repair, minimal repair, and targeted escalation options.

Every numeric assessment declares its score kind. Incomparable kinds, scales,
calibration revisions, model versions, or cohorts are not averaged or silently
ordered. Cold start, sparse/private evidence, drift, censoring, selection bias,
override bias, hidden route substitution, self-grading, task inflation, or
non-causal correlation narrows the claim, chooses the declared baseline, or
abstains.

Workers cannot grade themselves, accept their own proposal, select their
denominator/cohort, or turn provider output into graph state. Independent
review remains role-isolated; synthesis preserves failures, disagreements,
unknowns, and minority evidence.

## Governed experience, expertise, and handoff

Task-scoped experience may retrieve prior decomposition, route, failure,
review, and outcome evidence only through authorized anchored relations with
cohort, age, scope, and applicability. Harmful, stale, shifted, private, or
contradicted experience is quarantined, retired, or excluded. Recall never
becomes authority or a hidden model default.

Demonstrated expertise is a separate Plan 24-owned, default-off, authorized
ephemeral operation and task-context view. It may support “who knows” context,
reviewer discovery, and handoff explanation only after purpose-specific
consent and authorization. It is excluded from canonical task evidence,
retrieval candidates, fusion/ranking, readiness, assignment, reviewer
selection, acceptance, completion, calibration, and durable Work history.
Plan 37 may supply authorized anchored evidence input to this operation but
does not own the view, actor projection, consent, retention, or purge
semantics. The operation never ranks people, grants scope, overrides
independence, or substitutes acknowledgement for verified outcome.

Each invocation rechecks project/repository/signal/purpose consent,
authorization, source lifecycle, retention, and expiry; returns bounded
explanation and coverage without a composite person score; and keeps no
cross-request cache beyond its declared ephemeral lifetime. Revocation,
deletion, expiry, or authorization loss invalidates active handles immediately
and purges ephemeral expertise inputs, projections, and caches within the bounded
retention contract, leaving only a non-reversible deletion tombstone.
Handoffs record exact work/evidence frontier, unknowns, blockers, legal
actions, and lineage so rediscovery and reliance can be measured.
Checkpoint evidence preserves typed identity, order, grounding, and source
anchors, but cannot renew a lease, establish task acceptance, or mutate graph
or runtime state. Applicable skill, hint, provider, model, and capability
discovery remains provenance- and availability-bearing Work context rather
than ambient host behavior.

## Optional topology, placement, review, and integration

Execution placement, branch topology, review topology, and integration
strategy are independent versioned relations attached to a work-item version.
Changing any of them preserves TaskId.

Supported retained choices include:

- no managed placement, explicitly acknowledged clean in-place execution,
  linked worktree, or isolated local clone;
- no branch, unbranched work, independent branches, or a validated local
  branch stack;
- no review, independent review, standard pull requests, or capability-backed
  GitHub stacked pull requests; and
- no integration, externally observed/manual integration, exact
  fast-forward, conflict-free two-parent merge, or exact ordered cherry-pick
  when Plan 36 and policy permit it.

No-Git tasks remain first-class. No placement or topology is inferred from CWD,
path, current branch, task text, provider workspace, PR base, or host profile.

The task DAG and branch-stack DAG remain distinct. Branch ancestry does not
create task dependency or readiness, and task dependency does not move or
order refs. Cross-graph meaning requires an explicit versioned relation with
provenance and required/produced commit evidence.

Cross-merge and minimal-repair proposals are advisory until explicitly
accepted. Plan 32 may lower an accepted integration only through Plan 36 typed
preflight/apply operations, fenced leases, clean exact generations, required
tests/review, expected-ref CAS, effect permits, and durable receipts.

## Work experience and projections

Kanban, DAG, timeline, causal, critical-path, workload/capacity,
executor/model, repository/delivery, evidence, and history views select the
same authorized graph version and selection. Each preserves TaskId, versions,
scope, time, coverage, source watermarks, blockers, readiness derivation,
runtime references, and legal actions.

Familiar triage, todo, scheduled, ready, running, blocked, review, done, and
archived lanes are derived views over immutable history. Dragging a card or
setting a surface status cannot create readiness, acceptance, runtime state, or
completion.

Workload and capacity views include active and deferred work, provider and
capability constraints, requested versus actual concurrency, shared-authority
serialization, queue/defer reasons, deadlines, tokens, cost, and outcome
coverage. Executor/model views show recommendation evidence and actual route
without exposing private prompts or credentials.

## End-to-end PR14 core production path

1. **Create and view work.** The user creates an initiative/work plan/item with
   dependency and acceptance semantics, then inspects it in Kanban, DAG,
   timeline, causal, workload, executor/model, repository, and history views.
2. **Retrieve exact evidence.** TaskId-rooted context returns bounded evidence,
   coverage, unknowns, and anchors; the user expands at least one exact source.
3. **Receive an explained proposal.** Task-shape, sizing, decomposition,
   topology, independent-review, and route options are evaluated together,
   including serial/no-auxiliary fallbacks and explicit uncertainty.
4. **Review graph change.** The user accepts, rejects, or supersedes the
   proposal with graph/evidence/version CAS. Proposal generation itself changes
   nothing.
5. **Explicitly admit execution.** A separate command admits an accepted step
   through Plan 32 with exact scope, readiness, context, provider/model,
   sandbox, approval, grants, budgets, deadline, cancellation, configuration,
   and idempotency.
6. **Inspect progress and outcome.** Work projections join the Plan 32 run,
   lease, attempt, progress, artifacts, review, tests, requested/actual route,
   cost, cancellation/recovery, and terminal receipts by exact references.
7. **Review live replanning.** New evidence may generate split/merge/resize/
   reroute/re-review/minimal-repair/restart/escalation proposals. None changes
   graph, topology, runtime, or Git state until separately accepted and, where
   needed, lowered through a distinct Plan 32 control or re-admission command.

All graph contracts, owner-shard persistence, projections, policy/configuration
inputs, application operations, surfaces, provider/runtime mapping,
observations, and host execution behavior land as implementation slices of
this journey.

## Implementation slices

1. In PR14, deliver create/change/history plus core Work projections on one
   immutable owner-shard graph.
2. Deliver TaskId-rooted bounded evidence retrieval and exact expansion through
   existing evidence authorities.
3. Deliver explained shape/sizing/decomposition/topology/route proposal and
   explicit review.
4. Deliver one accepted-attempt mapping to a supported real provider through
   Plan 32, with progress/outcome projection and safe control.
5. In PR17, deliver outcome/calibration updates, governed recall/handoff, and
   non-auto-applied live replanning through the same surfaces.
6. In PR17, deliver optional advanced placement/stack/review/integration
   behavior through the same work identity and explicit effect flow.

Each slice includes the minimum domain, store, application, surface, and direct
tests it uses. No standalone schema, registry, port, exact type/file inventory,
fixture framework, or contract-only phase counts as delivery.

## Replacement and deletion

- Remove duplicate task stores, card-status authority, readiness calculators,
  model defaults, proposal engines, runtime schedulers, and view-specific
  filters after the canonical path works.
- Remove scaffold-only PR14/PR17 milestones, operation registries, generated
  inventories, declaration parity checks, giant fixture catalogs, repeated
  ownership prose, and compatibility paths that own logic.
- Preserve every retained operation and semantic state through the canonical
  application bindings. PR18 may freeze names and SDK compatibility, but PR17
  cannot leave an SDK-facing operation semantically incomplete.

## Safety constraints

- Every graph relation, evidence expansion, model context, artifact, event,
  view, metric, and handoff remains bound to its exact authorized project/user
  scope; logs and fixtures contain no prompts, private source, provider
  payloads, or credentials.
- Graph and projection writes use expected-version CAS, idempotency, immutable
  events, deterministic rebuild, and crash-safe owner-shard transactions.
- No provider receives global board/store access, unrelated sibling context,
  ambient daemon credentials, raw secrets, or task/runtime control authority.
- No provider output recursively dispatches, creates work, accepts a proposal,
  changes readiness, mints a lease, or marks completion.
- Proposal generation and recalibration never auto-apply.
- Scope, readiness, acceptance, effect, cancellation, and recovery uncertainty
  fail closed and remain truthful as partial, stale, denied, or unknown.
- GitHub review ingestion remains read-only. No task/workflow action posts,
  updates, resolves, dismisses, or replies to review comments.
- Git effects are explicit, previewed, version/CAS checked, fenced, and
  receipt-backed. No stash/clean/reset, rebase, squash, amend, revert, branch
  deletion, backward ref move, force push, semantic conflict resolution, or
  arbitrary Git fallback is permitted.

## Direct acceptance

The PR14 aggregate production journey must execute the core seven-step path
through
CLI, MCP, HTTP, and dashboard application bindings and run one supported real
provider adapter. It must demonstrate every retained Work projection against
the same graph selection, exact evidence expansion, explained proposal and
fallback, explicit acceptance and admission, bounded resumable progress,
independent review/outcome evidence, and an unapplied replan.

Focused behavior covers DAG cycles and supersession, current/as-of/evolution/
forensic history, projection equivalence, no-Git and every supported topology,
task/stack DAG separation, exact required/produced commits, stale graph/
evidence/route/config/provider/lease state, authorization narrowing,
partial and unknown evidence, idempotent replay/conflict, capacity deferral,
cancellation/restart/effect recovery, independent-review isolation, harmful
recall quarantine, deterministic fallback, calibration drift, no recursive
dispatch, checkpoint non-authority, skill/hint/capability discovery
provenance, adversarial hacker/fixer/legitimate-solver evaluator hardening,
role isolation, minority-review preservation, and no false completion.

Expertise-focused tests prove the separate operation is default-off and
purpose-authorized; consent grant enables only the scoped ephemeral view;
revocation, source deletion, expiry, authorization loss, and purge invalidate
handles and remove ephemeral expertise state within the bound; Plan 37 input
remains anchored and authorization-checked; and no expertise signal changes
canonical retrieval/evidence, rank/order, readiness, assignment, reviewer
selection, acceptance, completion, calibration, or durable task history.

Direct native Git cases cover clean preflight, conflict/test failure before
target movement, exact ref CAS, safe fast-forward/merge/cherry-pick where
authorized, partial or unknown external effects, and recovery without replaying
ambiguity or moving a ref backward.

The direct journey tests plus normal aggregate repository checks prove one
graph authority and one Plan 32 runtime authority, no provider-local defaults,
no hidden model choice, no auto-apply, no duplicate effect, and no parser or
executor for these roadmap files. Compact focused fixtures may cover the named
behaviors; PR17 does not require a giant declarative corpus or a separate
acceptance gate per declaration.

## Not in PR17

- PR18 publishes and freezes Rust and TypeScript SDK names/schemas for
  these already callable operations and preserves their lifecycle semantics.
- PR20 optimizes measured graph, evidence, proposal, projection, and runtime
  latency after this production loop emits real evidence.
- A second task database, generic workflow language, hidden online learner,
  autonomous graph/Git mutation, or developer-roadmap executor is not part of
  V2.
