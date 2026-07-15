# TraceDecay V2 Incremental Context Scout

**Delivery:** PR 13

**Status:** planned product work
**Depends on:** capture/projectors, [05 query](05-query-crate.md), [06 policy](06-policy-crate.md), [07 hooks](07-hooks-crate.md), [09 application](09-application-crate.md), [18 privacy](18-secret-detection-redaction-and-private-data-safety.md), [20 configuration](20-configuration-control-plane.md), and [23 session/LCM retrieval](23-session-lcm-temporal-retrieval-and-evaluation.md).

## Outcome

Context Scout asynchronously prepares one evidence-backed suggestion when new context would materially improve an active agent's next action. It is optional, bounded, advisory, and silent by default.

PR 13 ships both complete execution paths:

- deterministic retrieval and ranking with no model dependency;
- configured model assistance through the owned model gateway, with explicit capability, privacy, cost, and fallback policy.

Neither path is deferred. Both are implemented and tested before PR 13 is complete.

## Runtime boundary

- Hooks send a small sanitized event or wake signal to `tracedecayd` and return within the host budget.
- Hooks never run a model, search, graph traversal, remote request, or unbounded read.
- The daemon owns event admission, coalescing, retrieval, model calls, policy, persistence, retries, and cancellation.
- At an eligible host boundary, a hook may perform one bounded daemon lookup for a ready envelope, revalidate it, render it, and record delivery.
- Capture succeeds independently of Scout. Scout failure never blocks session ingestion or normal hook behavior.
- There is no task graph, plan executor, workflow runner, recursive MCP client, or second scheduler.

## Address and evidence

A deliverable envelope identifies the exact profile, provider, session, thread, Turn, agent, and logical message. Ambiguous identity suppresses delivery.

Every envelope contains:

- compact prompt-eligible text;
- durable retrieval anchors and safe provenance;
- the frozen input watermark and scope resolution;
- policy and configuration versions;
- reason, expiry, dedupe key, and delivery state;
- coverage, redaction, and omission information.

Model prose without authorized evidence does not become a suggestion. Historical content is quoted evidence, never active instruction.

## Processing

1. The daemon consumes sanitized canonical events from a durable checkpoint.
2. It coalesces bursts by exact address and cancels superseded work.
3. It builds a bounded retrieval request against the side-effect-free query and LCM APIs.
4. Deterministic policy produces candidates from retrieved evidence.
5. When configured and eligible, the model gateway may propose or refine a structured candidate using only approved bounded reads.
6. Policy checks relevance, novelty, authority, privacy, timing, dedupe, token, latency, and cost budgets.
7. At most one ready envelope is committed for the address and eligibility window.
8. Delivery claim, revalidation, delivery receipt, and outcome feedback are atomic or idempotent.

## Silence and dedupe

Suppress vague capability advertising, restated prompts, repeated categories, uncited claims, stale evidence, already observed information, unrelated sibling activity, and suggestions that arrive after their useful boundary.

Dedupe uses logical-message identity, evidence anchors, category, address, and recent delivery state. Identical text in different Turns or agents is not automatically the same event.

## Model path

The model is selected only from typed configuration and discovered capabilities. No provider, executable, or model name is a source-code default. The application authorizes tools and executes them; the model cannot widen scope, mutate state, run shell commands, call arbitrary MCP tools, access credentials, or choose delivery policy.

Tests cover configured success, disabled mode, unavailable capability, timeout, disconnect, cancellation, malformed structured output, privacy denial, budget exhaustion, explicit fallback, and requested-versus-actual route receipts.

## Product surface

Expose concise status, recent runs, pending/delivered/suppressed envelopes, explanation, feedback, pause/resume, cancellation, budget health, and Doctor diagnostics through existing typed application surfaces. No approval queue, item apply/reject flow, Orchestration Lab, task board, or separate evaluation product is created.

Doctor checks checkpoints, stuck runs, expired claims, duplicate delivery, queue pressure, model-gateway health, privacy quarantine, and hook/daemon handshake compatibility. Repairs remain daemon-owned and idempotent.

## Direct verification

- deterministic replay produces the same candidate and suppression reason;
- configured model replay remains schema-bound and evidence-bound;
- hooks perform no model/query/network work and remain within their latency budget;
- restart, duplicate event, lease takeover, cancellation, and partial write do not duplicate delivery;
- wrong session, Turn, agent, project, or privacy domain always suppresses;
- silence, dedupe, expiry, token, latency, and cost limits are enforced;
- feedback and outcomes attach to the exact delivered envelope without treating adjacency as adoption;
- disabled or unavailable Scout leaves capture and ordinary hints healthy.

## PR 13 deliverables

- daemon event consumer and bounded queue;
- deterministic candidate path;
- configured model-assisted path;
- durable envelope, checkpoint, delivery, feedback, and health state;
- hook ready-envelope handshake;
- status, feedback, controls, and Doctor views;
- fault, privacy, latency, deterministic, and model-path tests.

## Done

- Both deterministic and configured model paths are production-complete.
- Hooks only signal the daemon and read a ready envelope.
- Suggestions are exact-addressed, evidence-backed, compact, bounded, and duplicate-safe.
- Silence is a normal successful outcome.
- No task graph, plan tracker, executor, lab, or model-specific default exists.
