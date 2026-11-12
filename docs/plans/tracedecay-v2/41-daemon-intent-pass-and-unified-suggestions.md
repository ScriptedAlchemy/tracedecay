# 41. Daemon intent pass and unified suggestions

Status: design accepted, implementation in progress (phases below).
Governing plan: [22](22-incremental-context-scout-and-suggestion-envelopes.md).

## Problem

Two hint systems exist. A hook-local keyword classifier
(`crates/tracedecay-agent-hosts/src/hooks/tool_hints.rs`,
`tool_hints/classifiers.rs`) still serves the Codex prompt hook and the
Cursor `postToolUse` hook, with its own budget and dedupe
(`tracedecay-policy/src/hint_delivery.rs`, `tool_hints_seen.json`). Every
other hook event on every host goes through daemon V2 admission, which
delivers only Context Scout suggestions built from feedback findings
(diagnostics, GitHub reviews, CI localization, proximity). Claude Code
receives no tool hints at all. Plan 22 requires one suggestion channel and
the removal of "any hook-local retrieval, model, ranking, or feedback
logic".

Keyword rules also cannot see intent. On a held-out set of 30 natural
requests for earlier context, the session-recall phrase list caught 3.
Intent is often spread over several turns ("ok, do the same for the other
adapters"; "from now on always use pnpm"), which a single-prompt rule
cannot read.

## Design

One daemon-owned path, one channel (Context Scout), two deterministic
signal sources: observed tool actions and prompt inferences.

```
HOST HOOK (content-free boundary; ack budget 25 ms, dispatch.rs HOOK_ADMISSION_ACK_BUDGET_MICROS)
  decode_native_hook_event -> DecodedNativeHookEventV1 (no payload field)
  prepare_bound_hook -> HookV2AdmitRequestV1 { envelope, native_session_id, native_lifecycle,
                                              prompt_excerpt? (phase 2, PromptBoundary only, never spooled) }
DAEMON ADMISSION (mcp/handlers/hook_runtime/admission.rs)
  stage ledger -> lifecycle lookup -> claim ready guidance (owner.claim_ready_guidance_exact / claim_ready_guidance)
  admit_registered_hook_orchestration -> NEW trigger IntentBoundary for
      PromptBoundary | ToolLifecycle{Completed|Failed} | SessionBoundary{TurnComplete|End}
INTENT PASS (async, coalesced per session by BoundedHookOrchestratorV1)
  1. pin configuration; intent disabled => Completed{Disabled}
  2. resolve native session id
  3. bounded freshen (re-read every 50 ms up to freshen_budget_ms until the
     boundary's turn is ingested; the pass never drives ingest) + read window
     (agents/context_scout/session_window.rs over lcm_raw_messages)
  4. action facts from assistant tool calls in the newest turn, and prompt
     inferences from its user prompt (tracedecay-policy/src/intent_action_facts.rs)
  5. deterministic policy (tracedecay-policy/src/intent_decision.rs) -> <= 1 candidate
     (evidence = Session, category Retrieval) -> delivery window (IntentBoundary -> Immediate)
  6. owner.prepare_configured -> select_deterministic_context_scout -> store.enqueue
  7. mount claim authority on the session-scoped intent address
NEXT BOUNDARY (or the phase-2 bounded lookup inside the same prompt hook)
  claim -> HookReadyGuidanceV1 -> render_host_delivery -> host surface -> delivery receipt
  -> analytics `hint_emitted` -> existing settlement writes `hint_outcome` acted/ignored
```

### Privacy boundary

Hooks keep sending content-free boundaries. The daemon reads the prior
turns from its own ingested transcript store. The one relaxation is a
bounded transient `prompt_excerpt` (<= 2 KiB) on prompt boundaries, because
for Claude Code, Codex and Cursor the just-submitted prompt is not yet in
the store when the prompt hook fires. The excerpt is classified in memory,
never spooled, replayed or persisted; durable evidence stores only a digest
of it. Nothing content-bearing enters `HookEventEnvelopeV2`.

### Timing

Classification never runs inside the admission ack budget. Delivery is
`Immediate`: the pass runs after admission, so the first later boundary of
any kind claims it (a delivery inside `min_delivery_gap_secs` is withheld
as `suppressed_budget`). Phase 2 adds, for prompt hooks with a configured
`prompt_lookup_deadline_ms`, one bounded second round-trip inside the same
hook (plan 22 "the host performs one bounded lookup"). A pass that finishes
inside it delivers in the same prompt; a slower one degrades to the next
boundary.

### No decision model

The intent pass asks no model. An earlier revision asked a pluggable
decision-model engine (`/v1/systemone`) one yes/no question per intent; it
was removed in favor of the deterministic signals alone, together with its
client, settings (engine URL, model, question set, thresholds), receipts,
and evals. The window size, action-fact score, TTL, delivery gap, and
prompt lookup deadline remain operator settings in
`ContextScoutSettingsV1.intent`. Settings or durable Scout state still
carrying the removed engine fields are refused, not migrated.

### Intent taxonomy

Prompt inferences: session_recall, memory_store, call_graph, impact,
review_changes, build_diagnostics, atomic_edit (refactor),
type_orientation, project_context, unexpected_changes. Observed action
facts: search, file_read, file_lookup, broad_read, tool_descriptor_read,
semantic_search, explore_subagent, subagent_start_context,
edit_redundancy; memory_store, review_changes, build_diagnostics and
project_context are also fact-backed. Catalog text
(message, context, expected tools, skill) moves verbatim from
`CATEGORY_SPECS` into `tracedecay-policy/src/intent_catalog.rs`;
`evals/agent_adoption/grade.py` and hint settlement depend on those strings.

Selection: undelivered observed facts first, then undelivered prompt
inferences, each in classification priority order; an intent that is both
is recorded as an observed fact. Every candidate scores
`action_fact_score_permille`, and the receipt names its source
(`action_fact` or `prompt_inference`). `dedupe_key =
sha256("tracedecay.context-scout.intent-candidate.v1", kind,
protected_session_id)`: at most one suggestion per kind per session. The
existing `select_deterministic_context_scout` picks the single highest.

### Budget, dedupe, quiet mode

Scout's entirely: quiet/paused -> Suppressed; `delivered_dedupe_keys`;
`min_delivery_gap_secs` for inter-suggestion spacing; one pending entry per
session (`PendingSibling` suppression). `hint_delivery.rs`,
`ToolHintDedupe`, `tool_hints_seen.json` and the escalation re-hint are
retired.

### Feedback

Delivery receipts for Session-evidence entries write an analytics
`hint_emitted` row (category = intent key, metadata = route, decision
source, facts, prompt inferences, window digest), so the existing
settlement (`hooks/hint_outcomes.rs`, `ingest.rs`) writes `hint_outcome`
acted/ignored and the dashboard keeps working.

## Contracts

- `ContextScoutIntentKindV1` (keys = today's hint category keys).
- `ContextScoutCandidateV1.evidence: ContextScoutCandidateEvidenceV1 {
  Code(..), Session(ContextScoutSessionEvidenceV1) }`; old durable rows
  decode into `ContextScoutDurableStartupOutcomeV1::Reset`.
- `ContextScoutSessionEvidenceV1` (scope, provider, session, protected
  session id, window summary, prompt-excerpt digest, authority, redaction
  = metadata only, intent receipt, claim digest).
- `ContextScoutIntentReceiptV1 { facts, prompt_inferences, selected,
  source }`.
- Variants: `ContextScoutRouteV1::SessionIntent`,
  `ContextScoutSuppressionV1::PendingSibling`,
  `ContextScoutTriggerV1::IntentBoundary`;
  `ContextScoutEvidenceProjectionV1.session_window: Option<..>`
  (`content_generation` stays required).
- Phase 2: `HookV2AdmitRequestV1.prompt_excerpt: Option<String>` and
  `HookV2GuidanceLookup { envelope, deadline_micros }`.
- Domain: `ContextScoutSettingsV1.intent: Option<ContextScoutIntentSettingsV1>`.
- Regenerate dashboard contracts, the TypeScript SDK and
  `crates/tracedecay-sdk/src/operations.rs` after any change.

## Hosts

| Host | Boundaries admitted today | Suggestion surface | Gap closed in phase 2 |
|---|---|---|---|
| Claude Code | SessionStart, PostToolUse (edit tools), Stop | `hookSpecificOutput.additionalContext` | register `UserPromptSubmit` |
| Codex | SessionStart, PostToolUse, Stop | `additionalContext` | route `UserPromptSubmit` through dispatch; drop local hint |
| Cursor | sessionStart, afterFileEdit, stop | `additional_context` on sessionStart / postToolUse | decode `postToolUse` -> ToolLifecycle; `beforeSubmitPrompt` cannot inject |
| Kiro | `userPromptSubmit` | stdout | bounded lookup |
| Kimi, OpenCode, Pi, Droid | tool / session boundaries | stdout, OpenCode `session.hook("context")` | next-boundary delivery only |
| Hermes | tool and session boundaries | `additional_context` (plugin consumption unverified) | replace the in-process regex nudge with a daemon lookup |

## Phases

- **P0** Rework PR #4089: keep the phrase removals and regression cases;
  drop the hook-local engine client. Done (`31372ed4`).
- **P1** Daemon intent pass behind configuration, no host changes:
  contracts, settings, policy crate move, session-window reader, `IntentBoundary` orchestration,
  `run_production_intent_cycle`, owner `PendingSibling` and window arm,
  delivery-time analytics, journey tests.
- **P2** Hosts and the prompt hot path: `prompt_excerpt`,
  `HookV2GuidanceLookup`, decoder arms and `stock_event_support` flips,
  Claude `UserPromptSubmit` registration, Codex and Cursor routed through
  dispatch, Hermes lookup.
- **P3** Delete the legacy path: `tool_hints.rs` and its evals,
  `hint_delivery.rs`, `deduped_project_hint_with_id`, hook-side hint
  analytics emission, the Hermes regex nudge; update
  `.claude/skills/evaluating-hook-hints` and `evals/agent_adoption`.

## Decisions taken

1. Public wire types change additively only; `content_generation` stays
   required.
2. Prompt hooks send a transient, bounded excerpt (classified in memory,
   never persisted).
3. Keyword prompt predicates are the prompt inference source, in the
   policy crate; no decision model judges intent.
4. The escalation re-hint is dropped.
5. Hermes's in-process nudge is replaced by a daemon lookup.

## Unverified at design time

Historical-ingestor cadence per provider; Kimi, Droid and Hermes
acceptance of hook stdout / `additional_context`; the LCM provider label
for Cursor versus `hook_key()`.
