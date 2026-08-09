# V2 policy crate

## Status / role

`tracedecay-policy` remains the pure Rust decision library delivered with the
application authorization core. It evaluates immutable facts and returns
versioned, explainable decisions; it never performs storage, provider, runtime,
Git, configuration, or delivery effects.

The canonical mechanism is the callable evaluator API consumed directly by
`tracedecay-application`, together with the decision revision and explanation
carried in the application result. Historical type names, module layouts, and
contract fixtures are implementation evidence, not an inventory to recreate.
An evaluator required by a production journey but not callable from that
journey is a gap; a renamed or deleted scaffold is not.

The executable-work loop extends that existing kernel only where it needs a
decision: task-shape and decomposition assessment, provider/model/effort
recommendation, deterministic fallback, and evidence-driven replanning.

## Retained callable policy behavior

The rewrite retains pure evaluators for hint eligibility/delivery, retrieval
selection, capability and Git routing, analyzer routing, local/live
correlation, diagnostics and curation, admission recommendation, memory
proposal, experiment replay, conflict arbitration, and external-source/sink
authorization. The work loop adds task-shape, calibrated sizing, decomposition,
topology, provider/model/effort, independent-review, exploration, fallback,
and live-replan decisions to those callable production paths; it removes no
evaluator or decision state.

Exact, recorded, and current-best-effort replay remain distinct. Current-best-
effort names every substitution; recorded display does not pretend to rerun,
and exact replay fails when a required immutable input is unavailable.

## User outcome

When a user creates work, TraceDecay can explain why a proposed decomposition
and provider route fit the available evidence, identify exclusions and
uncertainty, and later explain why new runtime evidence justifies keeping or
replanning the work. The evaluator never applies the proposal, admits a
provider, or changes the graph.

## End-to-end production path

1. The application assembles an authorized immutable snapshot containing the
   selected work version, exact evidence references and coverage, eligible
   provider capabilities, configuration, content-location limits, budgets, prior
   outcomes, any human override, and separate watermarks for local
   code/session evidence and live Git evidence.
2. Policy returns an explained recommendation, deterministic fallback, or
   abstention. The result records the evaluator and input revisions, ranked
   eligible routes, exclusions, reason codes, evidence horizon, coverage, and
   uncertainty, preserves both watermarks, and reports whether the two
   frontiers agree, disagree, or cannot be compared completely.
3. The application presents the proposal for explicit review. Accepting it is
   a separate version-checked graph command; admitting a provider step is a
   separate Plan 32 command.
4. After the admitted step records progress and a terminal outcome, the same
   evaluator can assess the new immutable evidence and emit a replan proposal.
   That proposal is read-only until an authorized user explicitly accepts or
   rejects it.

This path uses the existing policy decision/revision/digest identity consumed
by application results and runtime admission. This adds no parallel policy
registry, scoring service, or configuration source.

## Required behavior

- Identical canonical inputs and evaluator/configuration revisions produce the
  same decision. Clocks, availability, randomness, and host state arrive only
  as explicit inputs.
- Local code/session evidence and live Git evidence retain independent
  watermarks, freshness, and coverage through evaluation and explanation.
  Agreement is recorded without collapsing identity; disagreement is
  preserved and reported. The evaluator never advances, substitutes, or
  silently merges either frontier from the other, and stale or partial state
  on either side remains explicit.
- Every decision has exactly one disposition: `allow`, `deny`, `abstain`,
  `not_applicable`, or `indeterminate`. Natural-language explanation renders
  the recorded trace and adds no authority.
- Recommendations keep correctness, sensitive-data handling, latency, cost, autonomy,
  and evidence quality as separate dimensions. An ordinal or heuristic score
  never renders as a probability. Calibrated values name their cohort,
  horizon, support, error, and drift validity.
- Sparse, private, shifted, stale, censored, or incomparable evidence widens
  uncertainty, selects the declared deterministic baseline, or abstains. It
  never triggers an adapter-local fallback or hidden model choice.
- Exploration, when enabled, is bounded by explicit allowlists, coverage and
  sample floors, content-location and budget ceilings, maximum share, rollback
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
uses the exact resolved `ProjectId` or projectless `UserProfileId`; CWD, paths,
labels, collections, provider accounts, branches, and native object IDs never
substitute for identity or widen scope. Missing, revoked, ambiguous, or widened
authority fails without exposing hidden resources.

Network/provider calls require the applicable grant before the call, and
hydration or continuation checks the same exact owner scope before returning
content. Policy may classify a proposed Git effect, but it cannot produce or
authorize a generic Git command. Application and the native Git owner require
the operation's confirmed immutable preview and CAS guards before mutation;
merge, rebase, force update, history rewrite, branch deletion, and semantic
conflict resolution are never implicit fallbacks.

## Implementation slices

1. Add the smallest evaluator inputs and decision output needed by work
   creation and evidence-backed proposal review, and call them from the
   production application path in the same slice.
2. Use the decision directly during explicit provider admission, including the
   selected route, all exclusions, the declared fallback, and the pinned
   configuration/budget revisions.
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

- Remove any route, score, fallback, or replan decision duplicated in a
  surface, provider adapter, dashboard, graph projector, or runtime handler.
- Remove policy-only milestones and declaration-parity gates.
- Do not retain a shadow evaluator or hidden provider default for compatibility.

## Direct acceptance

The direct journey must prove that a user can create versioned work, retrieve
authorized exact evidence, receive an explained recommendation, explicitly
accept the proposal, admit one supported real provider step, inspect its
recorded route and outcome, and receive a justified replan that changes
nothing until separately accepted.

Focused failures cover stale evidence or graph versions, revoked or narrowed
authority, scope denial, missing provider capability, invalid
calibration, deterministic fallback, human override, cancellation, unknown
outcome, self-grading attempts, and idempotent replay. Those direct tests plus
ordinary aggregate repository checks also prove that policy performs no I/O or
runtime/graph/Git effect and that no provider-local default or hidden model
selection exists; no separate acceptance gate is created.

A direct local/live-correlation regression supplies distinct local
code/session and live Git watermarks and proves both are returned unchanged
when evidence agrees, disagreement is preserved and explained without
frontier substitution, and stale or partial state on either source remains
independently visible rather than becoming a merged current result.

## Excluded mechanisms

- A custom policy VM, workflow DSL, online-learning service, or autonomous
  policy/configuration mutation is not part of V2.
