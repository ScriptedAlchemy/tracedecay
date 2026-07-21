# V2 policy crate

## Status / role

`tracedecay-policy` remains the pure Rust decision library delivered with the
application authorization core. It evaluates immutable facts and returns
versioned, explainable decisions; it never performs storage, provider, runtime,
Git, configuration, or delivery effects.

PR17 extends that existing kernel only where the executable work loop needs a
decision: task-shape and decomposition assessment, provider/model/effort
recommendation, deterministic fallback, and evidence-driven replanning.

## Existing policy capabilities retained

The rewrite retains pure evaluators for hint eligibility/delivery, retrieval
selection, capability and Git routing, analyzer routing, local/live
correlation, diagnostics and curation, admission recommendation, memory
proposal, experiment replay, conflict arbitration, and external-source/sink
authorization. PR17 adds task-shape, calibrated sizing, decomposition,
topology, provider/model/effort, independent-review, exploration, fallback,
and live-replan decisions to those callable production paths; it removes no
evaluator or decision state.

Exact, recorded, and current-best-effort replay remain distinct. Current-best-
effort names every substitution; recorded display does not pretend to rerun,
and exact replay fails when a required immutable input is unavailable.

## PR17 user outcome

When a user creates work, TraceDecay can explain why a proposed decomposition
and provider route fit the available evidence, identify exclusions and
uncertainty, and later explain why new runtime evidence justifies keeping or
replanning the work. The evaluator never applies the proposal, admits a
provider, or changes the graph.

## End-to-end production path

1. The application assembles an authorized immutable snapshot containing the
   selected work version, exact evidence references and coverage, eligible
   provider capabilities, configuration, privacy limits, budgets, prior
   outcomes, and any human override.
2. Policy returns an explained recommendation, deterministic fallback, or
   abstention. The result records the evaluator and input revisions, ranked
   eligible routes, exclusions, reason codes, evidence horizon, coverage, and
   uncertainty.
3. The application presents the proposal for explicit review. Accepting it is
   a separate version-checked graph command; admitting a provider step is a
   separate Plan 32 command.
4. After the admitted step records progress and a terminal outcome, the same
   evaluator can assess the new immutable evidence and emit a replan proposal.
   That proposal is read-only until an authorized user explicitly accepts or
   rejects it.

This path uses the existing policy decision/revision/digest identity consumed
by application results and runtime admission. PR17 adds no parallel policy
registry, scoring service, or configuration source.

## Required behavior

- Identical canonical inputs and evaluator/configuration revisions produce the
  same decision. Clocks, availability, randomness, and host state arrive only
  as explicit inputs.
- Every decision has exactly one disposition: `allow`, `deny`, `abstain`,
  `not_applicable`, or `indeterminate`. Natural-language explanation renders
  the recorded trace and adds no authority.
- Recommendations keep correctness, safety, privacy, latency, cost, autonomy,
  and evidence quality as separate dimensions. An ordinal or heuristic score
  never renders as a probability. Calibrated values name their cohort,
  horizon, support, error, and drift validity.
- Sparse, private, shifted, stale, censored, or incomparable evidence widens
  uncertainty, selects the declared deterministic baseline, or abstains. It
  never triggers an adapter-local fallback or hidden model choice.
- Exploration, when enabled, is bounded by explicit allowlists, coverage and
  sample floors, privacy and budget ceilings, maximum share, rollback
  thresholds, and circuit breakers. The selected propensity and reason are
  recorded.
- Workers cannot choose their grade, denominator, comparison cohort, route
  policy, or acceptance result. Self-reported completion remains distinct from
  tests, independent review, accepted outcomes, rework, and escaped defects.
- There is no opaque online weight mutation, self-authored reward, autonomous
  contextual bandit, or provider-authored policy/configuration change.
- A recommendation cannot create task identity, mutate a work graph, mark work
  ready or complete, reserve capacity, issue a lease, start a provider, retry,
  cancel, reconcile an effect, or apply a Git operation.

## Authorization and effect safety

The existing authorization kernel remains authoritative. Effective authority
is the narrow intersection of the caller grant, source grant, resolved typed
owner scope, sink policy, requested operation, and mandatory privacy
constraints. Every hydration, continuation, publication, model-context
delivery, host delivery, export, telemetry write, and effect requires a fresh
application sink recheck. Missing, stale, revoked, ambiguous, or widened
authority fails closed without disclosing hidden identity, counts, timing, or
existence.

Source definition, owner binding, mutable policy metadata, source/requester
grants, and resolved typed owner scope remain separate inputs. Definitions do
not carry owner or sink authority, policy metadata cannot become identity, and
ProjectId and projectless UserProfileId never match through CWD, path, label,
collection, provider account, branch, or native object ID. Content state and
access state remain orthogonal: exclusion, denial, partial authorization,
temporary unavailability, or stale proof never masquerades as authoritative
deletion.

Every provider fetch, source continuation, canonical admission, shard
selection, statistics read, graph expansion, hydration, projection, model/host
delivery, export, and telemetry sink requires a fresh admission proof pinned
to current grant, binding, owner, policy, privacy, configuration, and sink
revisions. Narrowing is monotonic; local-only privacy is non-waivable; derived
evidence inherits the most restrictive contributing constraint.

Policy may classify a proposed Git effect, but it cannot produce or authorize a
generic Git command. Application and the native Git owner must revalidate an
immutable preview and CAS guards before any explicitly requested effect. Merge,
rebase, force update, history rewrite, branch deletion, and semantic conflict
resolution are never implicit fallbacks.

## Implementation slices

1. Add the smallest evaluator inputs and decision output needed by work
   creation and evidence-backed proposal review, and call them from the
   production application path in the same slice.
2. Use the decision directly during explicit provider admission, including the
   selected route, all exclusions, the declared fallback, and the pinned
   configuration/privacy/budget revisions.
3. Feed committed attempt, review, and outcome evidence back through the same
   evaluator to produce a non-auto-applied replan with legal next actions.
4. Exercise all retained hint, retrieval, analyzer, correlation,
   diagnostics/curation, admission, memory, authorization, task-intelligence,
   topology, routing, exploration, fallback, and replay decisions through their
   real application consumers; no capability is deferred to a policy-only
   phase.

No slice lands a standalone schema, trait, registry, fixture framework, or
policy phase without its production caller.

## Replacement and deletion

- Remove any PR17 route, score, fallback, or replan decision duplicated in a
  surface, provider adapter, dashboard, graph projector, or runtime handler.
- Remove policy-only PR17 milestones and declaration-parity gates.
- Do not retain a shadow evaluator or hidden provider default for compatibility.

## Direct acceptance

The PR17 journey must prove that a user can create versioned work, retrieve
authorized exact evidence, receive an explained recommendation, explicitly
accept the proposal, admit one supported real provider step, inspect its
recorded route and outcome, and receive a justified replan that changes
nothing until separately accepted.

Focused failures cover stale evidence or graph versions, revoked or narrowed
authority, privacy suppression, missing provider capability, invalid
calibration, deterministic fallback, human override, cancellation, unknown
outcome, self-grading attempts, and idempotent replay. The aggregate gate also
proves that policy performs no I/O or runtime/graph/Git effect and that no
provider-local default or hidden model selection exists.

## Not in PR17

- Public SDK name/schema stabilization belongs to PR18.
- Performance tuning belongs to PR20 after the production loop emits real
  stage, coverage, budget, and outcome evidence.
- A custom policy VM, workflow DSL, online-learning service, or autonomous
  policy/configuration mutation is not part of V2.
