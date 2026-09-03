# TraceDecay V2 Incremental Context Scout

Earlier Scout envelope names, conformance packets, fixture matrices, timing
baselines, and file layouts are historical evidence, not prerequisites.
Independently released public suggestion envelopes retain their negotiated wire
compatibility. Internal and unreleased shapes change in place, and persisted
TraceDecay state uses only the exact final shape; old store bytes return
`ResetRequired` rather than entering a migration or dual-write path. Acceptance
follows the deterministic and model-assisted behavior, delivery controls,
overlay safety, and regressions below.

## User outcome

After a meaningful saved edit or host boundary, Context Scout can deliver one
compact, evidence-backed suggestion when new context would materially improve
the agent's next action. Scout is optional, advisory, bounded, and silent by
default. The product ships both the deterministic path and configured model-assisted
path as working product behavior.

## End-to-end production path

1. A real host edit or stop event wakes `tracedecayd`; the hook performs no
   retrieval or model work.
2. The daemon coalesces events by exact project, session, thread, turn, agent,
   message, worktree, and saved-content identity and cancels superseded work.
   The same event may wake Plan 25 indexing, but Scout neither joins that work
   nor starts its own parser/indexer.
3. It retrieves a bounded evidence set from canonical query, LCM, diagnostics,
   and semantic-provider operations using the latest complete compatible
   generation already ready for that worktree. `indexing` is a source-
   availability/coverage state, not a reason to wait. Exact/lexical/graph and
   non-code evidence remain usable while semantic results are omitted. Plan
   37's post-edit, read-only GitHub, CI-localization, and proximity findings
   enter through the same evidence path.
4. Deterministic policy always remains available. When explicitly configured
   and authorized, the owned model gateway may propose or refine a structured
   candidate using only the approved evidence and current budgets.
5. Policy checks relevance, novelty, exact project scope, freshness, timing,
   dedupe, latency, token, and cost limits. Silence or a typed suppression is a
   successful outcome.
6. The daemon commits at most one ready suggestion for the exact address and
   useful boundary. The host performs one bounded lookup, revalidates it, and
   renders it through its supported surface.
7. Delivery and explicit feedback are recorded idempotently against the exact
   suggestion. Display, timing, later edits, or session success never imply
   adoption or correctness.

A deliverable suggestion remains bound to the exact profile, provider,
session, thread, turn, agent, and logical message. Ambiguous identity
suppresses delivery. Its durable record retains compact prompt-eligible text,
canonical evidence anchors and provenance, frozen input watermark and scope,
policy/configuration revisions, reason, expiry, dedupe identity, coverage,
redaction/omission state, and delivery state. It copies no unanchored evidence
and treats historical narrative as quoted evidence, never active instruction.

## Implementation slices

### Deterministic Scout

- Produce suggestions directly from current authorized evidence with stable
  ranking, suppression, expiry, and dedupe behavior.
- Consume daemon checkpoints, coalesce bursts by exact address, cancel
  superseded work, and commit at most one ready suggestion for an eligibility
  window. Claim, revalidation, display receipt, and feedback update remain
  atomic or idempotent across restart.
- A saved edit may make newer code evidence pending without invalidating the
  prior complete compatible generation. Scout records the selected generation
  and freshness, never waits for parsing or FastEmbed, and suppresses only the
  stale-sensitive suggestion when no compatible ready evidence exists.
- Permit bounded semantic capsules for hover, signature, exact target,
  diagnostics, implementations, and references where Plan 35 can answer with
  exact generation, provenance, and coverage. Capsules dedupe against other
  retrieved evidence and share the same token/latency budget.
- Suppress restated prompts, vague capability advertising, stale or uncited
  claims, already observed information, unrelated sibling activity, and
  suggestions that miss their useful boundary.
- Return concise status, explanation, recent delivery state, pause/resume,
  cancellation, feedback, and budget state through existing application
  surfaces.

### Configured model assistance

- Prefer the existing provider adapters and their typed capability,
  cancellation, usage, cost, and schema receipts. This avoids another model
  transport while retaining exact configured route, provider availability,
  deterministic fallback, scope, evidence, and budget semantics.
- `genai` is only an admitted replacement candidate when one integration
  deletes multiple provider transports, introduces neither an AWS-LC nor a
  duplicate Reqwest stack, and passes the existing cancellation, structured
  schema, cost, timeout, malformed-output, and fallback fixtures. Otherwise
  keep the existing adapters; provider absence remains typed unavailable and
  deterministic Scout remains usable.
- Select a model only from typed configuration and discovered capabilities;
  no provider, executable, or model is a source-code default.
- Constrain model output to the same structured candidate and evidence
  references as deterministic Scout. The model cannot widen scope, choose
  delivery timing, mutate state, run commands, call arbitrary tools, or access
  credentials.
- On disabled capability, denial, timeout, malformed output, cancellation, or
  budget exhaustion, return an explicit outcome and use deterministic fallback
  only where policy permits it.
- Preserve requested versus actual route/capability receipts, configured
  success, disabled mode, unavailable provider, disconnect, scope denial,
  and cost exhaustion as distinct outcomes.

### Delivery and feedback

- Use one suggestion channel for Scout and Plan 37 suggested next actions.
  There is no GitHub-specific, CI-specific, LSP-specific, or host-specific
  parallel hint stream.
- Keep suggestions inert. Delivery never edits source, triggers CI, writes to
  GitHub, creates work, admits a workflow step, or continues an agent.
- Select `immediate`, `next_boundary`, `idle_window`, `on_request`, or
  `suppressed` from native host phase/boundary, quiet mode, recent delivery,
  unresolved interaction, urgency, expiry, and content-free activity. A model
  never chooses receptivity. Missing evidence suppresses unsolicited delivery
  but does not erase an explicit request.
- Distinguish attempted, delayed, displayed, expanded, explicitly accepted,
  explicitly rejected, dismissed, corrected, expired unseen, and unknown
  outcomes. Preserve explicit user deferral and override.
- Preserve the callable status, recent-run, pending/delivered/suppressed,
  explanation, feedback, pause/resume, cancellation, capability, and budget
  surfaces. No approval/apply queue or separate evaluation product is added.
- Emit typed Scout/host diagnostic and conformance evidence plus remediation
  references for Doctor. Scout owns the working advisory operations and
  evidence; the application layer owns canonical diagnosis and remediation
  orchestration.

## Overlay handling

Durable suggestions require exact saved-content or clean-generation identity.
Unsaved or dirty-overlay results may inform only the owning client's immediate
session and never enter durable state, telemetry, fixtures, exports, or remote
requests. A request for durable delivery is suppressed until the content is
saved.

## Replacement and deletion

- Remove future task/workflow fields and approval/apply states from the Scout
  envelope. A later task operation may reference a delivered suggestion by
  canonical evidence ID; Scout does not reserve fields for it.
- Remove exact schema and file inventories, standalone conformance-input
  milestones, placeholder timing baselines, and large fixture matrices.
- Remove duplicate hint channels and any hook-local retrieval, model, ranking,
  or feedback logic.
- This pruning removes no deterministic or model-assisted path, semantic
  evidence source, timing mode, feedback outcome, control/status surface,
  restart behavior, compatibility behavior, or overlay behavior.

## Direct acceptance

- A real saved edit and stop boundary can produce one deterministic,
  evidence-backed suggestion and render it on each supported host surface.
- A configured model-assisted run remains evidence- and schema-bound; disabled,
  unavailable, denied, timed-out, malformed, cancelled, and over-budget runs
  remain distinguishable and do not damage deterministic Scout.
- Restart, duplicate event, lease takeover, cancellation, and partial write do
  not duplicate delivery or attach feedback to the wrong address.
- Wrong project, session, turn, agent, message, or content generation
  domain suppresses delivery.
- Silence, dedupe, expiry, quiet mode, timing, token, latency, and cost limits
  are enforced without fixed planning-only benchmark packets.
- Context-order coverage varies decisive-evidence position and distractor
  volume and preserves the eligible/delayed/suppressed denominator. It proves
  immediate conflicts versus boundary-delayed nonurgent guidance without
  replacing production behavior with paper timing constants.
- Saved-content coverage proves identity survives every durable sink, and
  dirty-overlay coverage proves overlay-derived evidence reaches none.
- Scout failure leaves capture, ordinary host feedback, and daemon health
  intact. No result causes an edit, GitHub write, CI rerun, task mutation,
  workflow admission, or automatic agent continuation.

## Explicit non-goals

- Task graphs, approval queues, orchestration labs, workflow execution,
  recursive tool dispatch, personalized online models, and host-local ranking
  are not Scout features.
- Dashboard/Doctor rendering may consume Scout state, while Scout already
  ships the callable status, controls, delivery, and feedback operations.
