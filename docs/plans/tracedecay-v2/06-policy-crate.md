# V2 policy crate

## Status / Role

- Status: pending for PR11.
- PR5 and PR7–PR10 provide typed, immutable query candidates and evidence.
- PR11 implements tracedecay-policy and application effect handling together so no policy result is left without a production consumer.
- tracedecay-policy is a pure Rust decision library. It evaluates facts; it does not perform effects.

## Outcome

Hints, retrieval choices, routing, correlation, diagnostics, curation, scheduling, and memory decisions use deterministic, explainable evaluators with one application-owned path for validation and effects.

## Owns

- Versioned evaluator IDs, typed input snapshots, typed decisions, reason codes, score components, and canonical policy decision/revision/digest semantics consumed by [09](09-application-crate.md) provider-result identity.
- Ordinary pure Rust evaluators for hint eligibility and delivery, retrieval selection, tool/Git routing, correlation, diagnostics/curation, scheduler decisions, and memory proposals.
- Pure analyzer eligibility and routing decisions for the daemon-hosted LSP
  gateway in [35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- Replay comparison over immutable recorded inputs and outputs.
- One delivery arbiter that resolves eligible guidance by priority, relevance, repetition, cooldown, token budget, and host capability.
- Deterministic conflict handling when several rules propose incompatible effects.

## Does not own

- A custom bytecode VM, rule compiler, DSL, dynamic workflow runtime, or generated bundle language.
- Queries, ranking execution, database access, files, clocks, randomness, network calls, host probes, model calls, queues, locks, or process execution.
- Saving facts, sending hints, mutating config, scheduling runs, editing task plans, or applying any ProposedEffect.
- Starting or supervising analyzers, handling LSP JSON-RPC, or fabricating an
  analyzer or code-intelligence fallback.
- Task decomposition, board state, work-item readiness, leases, attempts, fairness, packets, or executor lifecycle.
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
- **PR11 — scheduler:** decide eligibility from explicit config, activity, lock, retry, budget, and prior-run state; application owns clocks, leases, and execution.
- **PR11 — memory:** propose retain, supersede, contradict, merge, forget, or no-op against explicit fact/version evidence. Equal text across scopes does not imply identity.
- **PR11 — application:** implement every ProposedEffect handler in the same PR, with authorization, idempotency, stale-input rejection, persistence receipts, and explicit failure.
- **PR11 — experiments:** expose pure evaluator adapters to the application experiment service; no evaluator writes experiment state.
- **PR13 — hooks:** hooks receive only application-approved guidance. They never invoke policy directly against partial host state.

## Acceptance

- Direct unit tests freeze canonical inputs and assert byte-stable decisions, reasons, evidence, versions, and config digests.
- Replay tests cover exact, recorded, and current-best-effort behavior plus missing inputs, version drift, and named substitutions.
- Hint tests cover repetition, cooldown, token budget, sensitivity, host limits, competing candidates, and outcome attribution.
- Retrieval/routing tests cover unavailable capabilities, stale truth, scope mismatch, no silent fallback, and unchanged query ordering.
- Git-routing tests cover preview/apply separation, effect classification,
  stale preview rejection, index conflicts, denied authority, and the absence
  of generic or history-mutating Git effects.
- Correlation tests preserve local/live disagreement and both watermarks.
- Diagnostics, scheduler, and memory tests prove evaluators cannot mutate and application handlers revalidate stale decisions.
- Concurrent evaluation tests use immutable snapshots and remain deterministic while application state changes.
- Architecture tests reject storage, transport, hook, model, process, task-executor, compiler, and generated-inventory dependencies.
