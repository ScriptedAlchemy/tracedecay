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

Deterministic action facts already describe useful tool behavior in the
session window: searches, reads, edits, subagent activity, and fact-backed
project or review activity. These facts can drive suggestions without
classifying prompt text or inferring unstated user goals.

## Design

One daemon-owned path, one channel (Context Scout), deterministic action
facts for session-evidence intent candidates.

```
HOST HOOK (content-free boundary; ack budget 25 ms, dispatch.rs HOOK_ADMISSION_ACK_BUDGET_MICROS)
  decode_native_hook_event -> DecodedNativeHookEventV1 (no payload field)
  prepare_bound_hook -> HookV2AdmitRequestV1 { envelope, native_session_id, native_lifecycle }
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
  4. action facts (deterministic, from tool calls in the window; tracedecay-policy/src/intent_action_facts.rs)
  5. deterministic policy (tracedecay-policy/src/intent_decision.rs) -> <= 1 candidate
     (evidence = Session, category Retrieval) -> delivery window (IntentBoundary -> Immediate)
  6. owner.prepare_configured -> select_deterministic_context_scout -> store.enqueue
  7. mount claim authority on the session-scoped intent address
NEXT BOUNDARY (or the bounded lookup inside the same prompt hook)
  claim -> HookReadyGuidanceV1 -> render_host_delivery -> host surface -> delivery receipt
  -> analytics `hint_emitted` -> existing settlement writes `hint_outcome` acted/ignored
```

### Privacy boundary

Hooks send only content-free boundaries. The daemon reads prior turns from
its own ingested transcript store and derives action facts in memory.
Durable session evidence is metadata only: protected identities, window and
claim digests, fact summaries, authority, and redaction state. Neither raw
transcript text nor prompt content enters `HookEventEnvelopeV2`, durable
candidates, delivery receipts, or analytics metadata.

### Timing

Fact derivation never runs inside the admission ack budget. Delivery is
`Immediate`: the pass runs after admission, so the first later boundary of
any kind claims it (a delivery inside `min_delivery_gap_secs` is withheld
as `suppressed_budget`). For prompt hooks with a configured
`prompt_lookup_deadline_ms`, phase 2 adds one bounded second round-trip
inside the same hook (plan 22 "the host performs one bounded lookup"). A
pass completed before that deadline can deliver in the same prompt;
otherwise delivery remains available to the next boundary.

### Bounded work and cancellation

The pass remains asynchronous and coalesced per session. Freshening and
window reads are bounded, and the pass observes orchestrator cancellation
through preparation and enqueue. Disabled configuration, cancellation,
stale sessions, and lease loss remain distinct typed outcomes. Cancelled or
superseded work does not enqueue a candidate or emit delivery and outcome
receipts; only committed durable state is visible after restart.

### Deterministic action facts

Fact-backed intent kinds are search, file_read, file_lookup, broad_read,
tool_descriptor_read, semantic_search, explore_subagent,
subagent_start_context, edit_redundancy, memory_store, review_changes,
build_diagnostics, and project_context. No other intent kind produces a
session-evidence candidate. Catalog text (message, context, expected tools,
skill) lives in `tracedecay-policy/src/intent_catalog.rs`;
`evals/agent_adoption/grade.py` and hint settlement depend on those
strings.

Each candidate comes directly from observed action facts and deterministic
policy. There are no inferred probabilities or categorical judgments.
`dedupe_key = sha256("tracedecay.context-scout.intent-candidate.v1", kind,
protected_session_id)` permits at most one suggestion per kind per session.
The existing `select_deterministic_context_scout` picks the single highest
candidate under the same deterministic ordering used by other Scout
candidates.

### Budget, dedupe, quiet mode

Scout's entirely: quiet/paused -> Suppressed; `delivered_dedupe_keys`;
`min_delivery_gap_secs` for inter-suggestion spacing; one pending entry per
session (`PendingSibling` suppression). `hint_delivery.rs`,
`ToolHintDedupe`, `tool_hints_seen.json` and the escalation re-hint are
retired.

### Feedback

Delivery receipts for Session-evidence entries write an analytics
`hint_emitted` row (category = intent key, metadata = route, window digest,
and facts), so the existing settlement (`hooks/hint_outcomes.rs`,
`ingest.rs`) writes `hint_outcome` acted/ignored and the dashboard keeps
working. Tests exercise deterministic windows and facts, including empty,
denied, stale, cancelled, and malformed-session outcomes; they do not
manufacture confidence values or timestamps.

## Contracts

- `ContextScoutIntentKindV1` contains the deterministic fact-backed keys.
- `ContextScoutCandidateV1.evidence: ContextScoutCandidateEvidenceV1 {
  Code(..), Session(ContextScoutSessionEvidenceV1) }`; old durable rows
  decode into `ContextScoutDurableStartupOutcomeV1::Reset`.
- `ContextScoutSessionEvidenceV1` (scope, provider, session, protected
  session id, window summary, authority, redaction = metadata only, intent
  receipt, claim digest).
- Deterministic intent variants: `ContextScoutSuppressionV1::PendingSibling`,
  `ContextScoutTriggerV1::IntentBoundary`;
  `ContextScoutEvidenceProjectionV1.session_window: Option<..>`.
- `HookV2GuidanceLookup { envelope, deadline_micros }` for the bounded
  prompt-hook lookup.
- Domain: `ContextScoutSettingsV1.intent: Option<ContextScoutIntentSettingsV1>`
  controls deterministic intent behavior and its bounded session window.
- `content_generation` stays required. Independent authoring backends and
  their lifecycle, privacy, cancellation, usage, and schema receipts are
  unchanged; they do not participate in action-fact derivation or intent
  candidate selection.
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
  drop hook-local intent classification. Done (`31372ed4`).
- **P1** Daemon intent pass behind configuration, no host changes:
  contracts, settings, policy crate move, deterministic action-fact reader
  and policy, session-window reader, `IntentBoundary` orchestration,
  `run_production_intent_cycle`, owner `PendingSibling` and window arm,
  delivery-time analytics, journey tests.
- **P2** Hosts and the prompt hot path: `HookV2GuidanceLookup`, decoder arms
  and `stock_event_support` flips, Claude `UserPromptSubmit` registration,
  Codex and Cursor routed through dispatch, Hermes lookup.
- **P3** Delete the legacy path: `tool_hints.rs` and its evals,
  `hint_delivery.rs`, `deduped_project_hint_with_id`, hook-side hint
  analytics emission, the Hermes regex nudge; update
  `.claude/skills/evaluating-hook-hints` and `evals/agent_adoption`.

## Decisions taken

1. Wire contracts expose only deterministic fact-backed intent. Removed
   durable representations reset through the typed startup outcome;
   `content_generation` stays required and independent authoring behavior is
   unchanged.
2. Hooks remain content-free; intent facts come only from the daemon's
   ingested session window.
3. Only observed action facts produce intent candidates. Prompt-text
   predicates are removed with the legacy hint path rather than retained as
   another intent source.
4. The escalation re-hint is dropped.
5. Hermes's in-process nudge is replaced by a daemon lookup.

## Unverified at design time

Historical-ingestor cadence per provider; Kimi, Droid and Hermes acceptance
of hook stdout / `additional_context`; the LCM provider label for Cursor
versus `hook_key()`.
