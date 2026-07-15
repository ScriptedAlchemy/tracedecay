# TraceDecay V2 Incremental Context Scout

**Delivery:** PR 13

**Status:** planned product work
**Depends on:** capture/projectors, [05 query](05-query-crate.md), [06 policy](06-policy-crate.md), [07 hooks](07-hooks-crate.md), [09 application](09-application-crate.md), [12 CLI/MCP/HTTP/LSP gateway](21-cli-mcp-tool-surface-and-output-unification.md), [18 privacy](18-secret-detection-redaction-and-private-data-safety.md), [20 configuration](20-configuration-control-plane.md), [23 session/LCM retrieval](23-session-lcm-temporal-retrieval-and-evaluation.md), and [35 daemon LSP gateway and semantic-evidence provider](35-daemon-lsp-gateway-and-universal-diagnostics.md).

**Staging:** PR 13 consumes the PR 11 semantic-evidence provider contract and PR 12 gateway surfaces delivered by [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) and [Plan 21](21-cli-mcp-tool-surface-and-output-unification.md); it does not redefine analyzer authorization, duplicate-analyzer policy, or gateway lifecycle behavior.

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

## Semantic evidence and privacy

Only clean-generation or saved-content semantic evidence is eligible for a committed Scout suggestion envelope.

Ephemeral-overlay results — hover, signature, diagnostic, reference, or implementation content derived from unsaved or dirty editor state — may be used only for non-durable immediate session context when explicitly authorized. They are ineligible for envelopes, checkpoints, delivery or feedback records, observations, facts, memory, telemetry payloads, spools, caches, replicas, or exports. When durable delivery was requested for such evidence, Scout must emit a typed suppressed or unavailable state rather than persisting or forwarding the overlay content.

No unsaved source-derived hover, signature, diagnostic, or reference content may be persisted.

## Processing

1. The daemon consumes sanitized canonical events from a durable checkpoint.
2. It coalesces bursts by exact address and cancels superseded work.
3. It builds a bounded retrieval request against the side-effect-free query and LCM APIs. It may additionally request bounded on-demand semantic capsules from [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md)'s semantic-evidence provider around changed or selected symbols — hover/signature, the exact target, nearby diagnostics, and implementations/references as budget permits — but only clean-generation or saved-content capsules may enter envelope commit. Overlay capsules may inform immediate non-durable session context when authorized and must otherwise suppress with typed state. Capsule requests dedupe against already-retrieved evidence, stay within the same token/latency budgets as other retrieval, retain provider provenance, and never flood the prompt or create a second suggestion channel alongside the deterministic and model-assisted candidate paths below.
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

Expose concise status, recent runs, pending/delivered/suppressed envelopes, explanation, feedback, pause/resume, cancellation, and budget health through existing typed application surfaces. No approval queue, item apply/reject flow, Orchestration Lab, task board, or separate evaluation product is created.

PR 13 emits typed Scout and host finding and conformance state only. The unqualified Doctor kernel, UI, dashboard views, and remediation are owned by PR 14 in [Plan 11](11-dashboard-frontend.md); PR 13 does not own Doctor presentation or repair orchestration.

## Direct verification

PR 13 direct tests must satisfy every requirement below. These are acceptance gates, not advisory guidance.

- deterministic replay produces the same candidate and suppression reason;
- configured model replay remains schema-bound and evidence-bound;
- hooks perform no model/query/network work and remain within their latency budget;
- restart, duplicate event, lease takeover, cancellation, and partial write do not duplicate delivery;
- wrong session, Turn, agent, project, or privacy domain always suppresses;
- silence, dedupe, expiry, token, latency, and cost limits are enforced;
- feedback and outcomes attach to the exact delivered envelope without treating adjacency as adoption;
- disabled or unavailable Scout leaves capture and ordinary hints healthy;
- a **positive** saved-content/clean-generation semantic-evidence fixture proves committed evidence remains bound to the exact saved-content/clean-generation identity through envelope, checkpoint, delivery receipt, feedback state, telemetry metadata, and every durable spool, cache, replica, and export representation; no sink may drop, substitute, or relabel that identity;
- a **negative** unsaved-secret dirty-overlay fixture proves no durable envelope, checkpoint, receipt, feedback record, observation, fact, memory entry, telemetry payload, spool, cache, replica, or export contains overlay-derived hover, signature, diagnostic, reference, or implementation source/evidence; durable delivery requests for such evidence return typed suppressed or unavailable state.

## PR 13 deliverables

- daemon event consumer and bounded queue;
- deterministic candidate path;
- configured model-assisted path;
- durable envelope, checkpoint, delivery, feedback, and health state;
- hook ready-envelope handshake;
- status, feedback, controls, and typed Scout/host finding and conformance state emission;
- fault, privacy, latency, deterministic, and model-path tests;
- required positive and negative semantic-evidence acceptance fixtures.

## Done

- Both deterministic and configured model paths are production-complete.
- Hooks only signal the daemon and read a ready envelope.
- Suggestions are exact-addressed, evidence-backed, compact, bounded, and duplicate-safe.
- Silence is a normal successful outcome.
- No task graph, plan tracker, executor, lab, or model-specific default exists.
