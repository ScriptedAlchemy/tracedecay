# Codex provider-normalization golden inputs

Inputs are real Codex rollout JSONL record shapes already exercised by
`tests/transcript_ingest_suite/codex.rs` and
`crates/tracedecay-sessions/src/runtime/codex.rs`
(`session_meta`, `event_msg`/`agent_message`, `response_item`/`function_call`,
plus lifecycle shapes: nested `thread_goal_updated`, `update_plan`, and exact
`task_started`/`task_complete`/`turn_aborted`).

`thread_goal_updates.input.json` is a redacted four-record production
sequence: active, a token/time-only active tick, an objective transition, then
paused. Provider keys, nesting, statuses, and counter transitions are
preserved; session/objective payload values are replaced with stable
fixture-safe values.

Each input has a checked-in `*.expected_envelope.json`. Tests derive the stable
record id with `codex_native_record_id`, invoke `normalize_codex_observation`,
and compare the full serialized envelope after substituting that parser-derived
id. Do not replace this path with hand-built `DurableObservationV1` lookalikes.

Lifecycle normalization maps verified natives onto
`CanonicalObservationFactV1::WorkflowLifecycle` (`goal` / `plan` / `task`) in
unit and production-path tests; see `goal_event_tests` and
`codex_workflow_lifecycle_*` in `transcript_ingest_suite/codex.rs`.

Canonical projection retains every raw `thread_goal_updated` observation, but
collapses consecutive identical `(thread, objective, status)` goal ticks when
projecting current goal state (token/time-only drift does not open a new
projected row; status/objective transitions do).

## Protocol gaps (intentional)

- **UnknownVersion:** Codex rollout JSONL records are typed (`type` /
  `payload.type`) but have no checked-in versioned transcript schema with an
  unsupported-version evidence path. Do not invent
  `ObservationCoverageReason::UnknownVersion` emission or synthetic fixtures.
- **IdentityCollision via content rewrite:** native record ids are
  content-addressed (`codex_native_record_id`); changing payload changes id, so
  production IdentityCollision is unreachable without forging identity outside
  the parser. Production tests cover ExactDuplicate / no-overwrite redelivery.
