---
name: managing-session-context
description: 'Use when driving the LCM compression lifecycle for a host — preflight, compression, session-boundary reporting, or diagnosing/repairing the LCM store. For past-session recall see recalling-session-context.'
---

# Managing session context

This skill owns the **LCM compression and maintenance lifecycle** — the write
and health side of the session store. It is the counterpart to
`tracedecay:recalling-session-context`, which owns retrieval (grep, replay,
summary-DAG expansion). These lifecycle tools are **host-agent integration
tools**: invoke them when the host is managing its own context window or when
the user explicitly asks to compress, repair, or inspect the LCM store — not
casually during recall.

## Lifecycle tools

All take `--provider` and (except doctor/status) `--session-id`. All default to
`storage_scope: "project_local"`; pass `hermes_profile` with an absolute
`hermes_home` only when the user targets a Hermes profile store.

1. **Preflight → `tracedecay_lcm_preflight`** (`provider`, `session-id`, plus
   token knobs like `current-tokens`, `threshold-tokens`, `context-length`,
   `reserve-tokens-floor`, `max-assembly-tokens`, `fresh-tail-count`): decide
   *whether* compression should run before doing it. Read-only planning call.
2. **Compress → `tracedecay_lcm_compress`** (same core args plus
   `focus-topic`, `summarizer`, `expected-current-frontier-store-id` as an
   optimistic guard): advance the compression lifecycle. **Mutates** the store.
   Use `expected-current-frontier-store-id` to no-op safely if the frontier
   moved under you.
3. **Session boundary → `tracedecay_lcm_session_boundary`** (`provider`,
   `session-id`, `old-session-id`, `bound-session-id`, `boundary-reason`):
   report that the host crossed a compression boundary. A mismatch between the
   bound and old session skips carry-over and starts a short cooldown.
4. **Doctor → `tracedecay_lcm_doctor`** (`provider`, `mode`:
   `diagnose`|`repair`|`retention`|`clean`|`gc`, `apply`, optional
   `session-id`): bounded diagnostics and safe repairs. `diagnose`/`retention`
   are read-only; `repair`/`clean`/`gc` **mutate only with `apply: true`** and
   are further gated by safety flags/env for clean and gc.
5. **Status → `tracedecay_lcm_status`** (optional `provider`, `session-id`,
   `deep`): schema/message/summary/payload counts, token estimates, summary
   depth distribution + compression ratio, payload byte totals, and GC status.
   Read-only; `deep: true` adds an on-disk integrity sweep.

## Typical flow

Preflight → (if it requests compression) compress → status to confirm the ratio
moved. On a real host session change, call session_boundary. If counts look
wrong (missing sessions, stale FTS, orphaned payloads) run doctor
`mode: "diagnose"` first, review, then repair/clean/gc with `apply: true` only
on explicit user intent.

## Guardrails

- `preflight`, `status`, and doctor `diagnose`/`retention` are read-only.
  `compress`, `session_boundary`, and doctor `repair`/`clean`/`gc` + `apply`
  **mutate** durable session state — run them only with clear lifecycle or user
  intent, never speculatively.
- `provider` is required and `all` is rejected for these lifecycle tools; target
  one provider at a time.
- Do not let subagents drive compression, boundaries, or repair; those are
  parent-agent/host responsibilities.
- Keep token knobs conservative; over-aggressive compression loses replay
  fidelity that `tracedecay:recalling-session-context` depends on.

## Handoff

- Retrieving past-session content (grep, replay, summary-DAG expansion) → `tracedecay:recalling-session-context`.
- CLI fallback when MCP transport fails → `tracedecay:using-the-cli`.

## Output

- The lifecycle action taken (preflight decision, compression result, boundary
  outcome, or store counts), whether it was read-only or mutating, and the
  resulting compression ratio / health signals.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
