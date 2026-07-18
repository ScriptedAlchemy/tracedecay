# V2 policy crate

## Status / Role

- Status: pending for PR11.
- PR5 and PR7–PR10 provide typed, immutable query candidates and evidence.
- PR11 implements tracedecay-policy and application authorization/orchestration
  together so no policy result is left without a production consumer.
- PR17 adds pure Plan 24 task-shape, decomposition, assignment/model-route, and
  live-recalibration evaluators over Plan 26 evidence.
- tracedecay-policy is a pure Rust decision library. It evaluates facts; it does not perform effects.

## Outcome

Hints, retrieval choices, routing, correlation, diagnostics, curation,
admission recommendations, and memory decisions use deterministic, explainable
evaluators. Plan 09 authorizes and orchestrates typed operations; each owning
runtime performs its effects.

## Owns

- Versioned evaluator IDs, typed input snapshots, typed decisions, reason codes, score components, and canonical policy decision/revision/digest semantics consumed by [09](09-application-crate.md) provider-result identity.
- Ordinary pure Rust evaluators for hint eligibility and delivery, retrieval
  selection, tool/Git routing, correlation, diagnostics/curation, admission
  recommendations, and memory proposals.
- Pure analyzer eligibility and routing decisions for the daemon-hosted LSP
  gateway in [35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- Pure Plan 24 task sizing/decomposition and executor/provider/model/effort
  recommendations, including bounded exploration and deterministic fallback.
- Replay comparison over immutable recorded inputs and outputs.
- One delivery arbiter that resolves eligible guidance by priority, relevance, repetition, cooldown, token budget, and host capability.
- Deterministic conflict handling when several rules propose incompatible effects.

## Does not own

- A custom bytecode VM, rule compiler, DSL, dynamic workflow runtime, or generated bundle language.
- Queries, ranking execution, database access, files, clocks, randomness, network calls, host probes, model calls, queues, locks, or process execution.
- Saving facts, sending hints, mutating config, scheduling runs, editing task plans, or applying any ProposedEffect.
- Starting or supervising analyzers, handling LSP JSON-RPC, or fabricating an
  analyzer or code-intelligence fallback.
- Task/work graph identity or mutation, board state, readiness authority,
  leases, attempts, packets, runtime fairness enforcement, or executor
  lifecycle. Policy may propose typed task decomposition and route decisions;
  Plan 24 defines graph semantics, application revalidates, and Plan 32
  executes.
- UI, API, CLI, MCP, hook rendering, experiment persistence, or generated inventories.

## Required behavior

- **PR11 — runtime:** define small evaluator traits/functions over immutable typed inputs. Each evaluator returns a decision, reasons, evidence references, version, config digest, and optional ProposedEffect.
- **PR11 — determinism:** identical canonical input and evaluator/config versions produce identical output. Time and host state arrive as explicit input fields.
- **PR11 — replay:** ExactDeterministic reruns the same implemented evaluator against complete recorded inputs; RecordedResult displays the recorded decision; CurrentBestEffort runs the current evaluator and names every substitution.
- **PR11 — no VM:** implement all required product rules as reviewed Rust. A custom VM is not part of V2 unless PR11 contains a directly proven requirement, full implementation, direct tests, and a simpler-Rust comparison.
- **PR11 — hinting:** evaluate candidate eligibility, sensitivity, scope, relevance, repetition, cooldown, prior outcome, and token cost. The delivery arbiter emits at most the host- and budget-allowed set.
- **PR11 — outcomes:** distinguish shown, suppressed, ignored, acted on, contradicted, expired, and unknown. Missing feedback is unknown, never success or failure.
- **PR11 — retrieval:** select declared query/ranking profiles and candidate limits without opening stores or reranking results itself.
- **PR11 — routing:** choose among cataloged capabilities using explicit availability, freshness, scope, effect, and truth-source metadata. Never invent a fallback capability.
- **PR11 — Git effects:** policy may classify a proposed Git index mutation by
  scope, authority, freshness, conflict risk, and effect class, but it never
  opens the index or executes Git. Application owns the typed
  `GitIndexTransaction`, revalidates the immutable preview digest/CAS guards,
  and returns an idempotent receipt or a typed stale/conflict/denied result.
  Policy cannot propose or authorize a generic Git command, merge, rebase,
  cherry-pick, branch/tag/ref mutation, or history rewrite.
- **PR11 — analyzer routing:** decide only among cataloged analyzers and typed
  code/diagnostic capabilities from explicit availability, privacy, scope,
  configuration, and resource evidence. Publish canonical policy
  decision/revision/digest tuples for Plan 09 provider-result identity and
  Plan 35 runtime snapshot composition. Application revalidates authorization,
  freshness, limits, and effect preconditions before admission or execution.
  Plan 35 consumes these decisions while composing runtime snapshots; it does
  not duplicate policy fields or digest semantics.
- **PR11 — correlation:** reconcile local code/session evidence with live Git delivery evidence while preserving separate watermarks and disagreements.
- **PR11 — diagnostics/curation:** propose bounded remediation or fact changes with evidence and confidence; application revalidates authority and preconditions before applying.
- **PR11 — admission policy:** recommend eligibility from explicit
  configuration, activity, prior-run evidence, budget, and declared runtime
  state. Plan 09 revalidates and authorizes the typed operation. Plan 32 alone
  owns workflow/task runtime clocks, queues, fairness enforcement, leases,
  attempts, retries, cancellation, effects, and execution.
- **PR11 — memory:** propose retain, supersede, contradict, merge, forget, or no-op against explicit fact/version evidence. Equal text across scopes does not imply identity.
- **PR11 — application:** implement every `ProposedEffect` authorization and
  orchestration handler in the same PR, with idempotency, stale-input
  rejection, persistence receipts, and explicit failure. The owning subsystem
  performs the effect; Plan 32 exclusively performs workflow/task runtime
  clock, lease, attempt, retry, cancellation, and effect transitions.
- **PR11 — experiments:** expose pure evaluator adapters to the application experiment service; no evaluator writes experiment state.
- **PR13 — hooks:** hooks receive only application-approved guidance. They never invoke policy directly against partial host state.
- **PR17 — task/model routing:** grade task shape and decomposition fit, then
  recommend task sizing, executor/provider/model/effort, reviewer, and
  fallback from eligible versioned Plan 26 evidence. Inputs carry cohort,
  coverage, horizon, graph/scope, policy/config/catalog/privacy revisions,
  human overrides, and independent outcome evidence. Sparse, shifted, denied,
  or incomplete evidence selects the deterministic baseline. Exploration has
  explicit allowlists, sample/coverage floors, budget and privacy ceilings,
  maximum share, rollback thresholds, and circuit breakers. Workers cannot
  choose their grade, denominator, or policy; self-reported completion is
  distinct from tests, review, accepted outcomes, rework, and escaped defects.
  Results are an explained `Recommendation`, `FallbackRecommendation`, or
  typed `Abstention`; they include ranked eligible routes, exclusions,
  confidence/coverage, evidence horizon, calibrated ranges, and legal next
  actions. Exact model/version, tool/context capability, host adapter,
  decomposition role, risk band, censoring, override/selection exposure, and
  estimator revision are comparison dimensions. A decision never rewrites
  admitted work or changes policy code/config.
- **PR17 — estimator governance:** use reviewed versioned formulas, cohort
  filters, priors, thresholds, intervals, and change-point rules over immutable
  inputs. Cold start, sparse/private cohorts, model/version drift,
  nonstationarity, censored outcomes, or incomparable evidence widen
  uncertainty, coarsen only to an eligible declared parent cohort, select the
  baseline, or abstain. There is no opaque online weight mutation, hidden
  self-training, self-authored reward, or single-proxy optimization.

## Acceptance

- Direct unit tests freeze canonical inputs and assert byte-stable decisions, reasons, evidence, versions, and config digests.
- Replay tests cover exact, recorded, and current-best-effort behavior plus missing inputs, version drift, and named substitutions.
- Hint tests cover repetition, cooldown, token budget, sensitivity, host limits, competing candidates, and outcome attribution.
- Retrieval/routing tests cover unavailable capabilities, stale truth, scope mismatch, no silent fallback, and unchanged query ordering.
- Git-routing tests cover preview/apply separation, effect classification,
  stale preview rejection, index conflicts, denied authority, and the absence
  of generic or history-mutating Git effects.
- Correlation tests preserve local/live disagreement and both watermarks.
- Diagnostics, admission-policy, and memory tests prove evaluators cannot
  mutate, application handlers revalidate stale decisions, and no policy
  evaluator advances a clock, queue, lease, attempt, retry, cancellation, or
  effect.
- Concurrent evaluation tests use immutable snapshots and remain deterministic while application state changes.
- Architecture tests reject storage, transport, hook, model, process, task-executor, compiler, and generated-inventory dependencies.
- PR17 fixtures replay task/model decisions byte-for-byte, exercise
  first-pass completion, correctness, tests/review, rework, latency,
  tokens/cost, autonomy, overrides, cancellation and unknown outcomes, and
  prove privacy suppression, anti-gaming, bounded exploration, and
  deterministic fallback.
- PR17 estimator fixtures cover cold start, sparse and private cohorts, exact
  model-version boundaries, shifted horizons, censored failures,
  selection/override bias, task inflation, self-grading, non-causal
  correlation, confidence/calibration error, and explicit abstention without
  mutating policy or graph/runtime state.
