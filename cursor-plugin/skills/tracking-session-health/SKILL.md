---
name: tracking-session-health
description: Bracket a work session with a code-health baseline and a final per-dimension delta to prove whether changes improved or degraded the codebase. Use for "did my refactor make things better", "track quality while I work", "show a before/after health delta", or wrapping a cleanup/refactor session with evidence.
---

# Tracking session health

## Workflow

1. **Before the first edit → `tokensave_session_start`** (no args): snapshots current health metrics as the baseline (`.tokensave/session_baseline.json`).
2. **Work normally** — edits via `tokensave:atomic-code-edits` or regular tools; tokensave re-indexes as files change.
3. **After the work → `tokensave_session_end`**: re-scans and returns the per-dimension diff (acyclicity, depth, equality, redundancy, modularity) — what improved, what degraded — and clears the baseline.
4. **Interpret the delta:** a dropped dimension names the follow-up — e.g. redundancy fell → `tokensave_redundancy` to find what got duplicated; acyclicity fell → `tokensave_circular`. The full dimension→drill-down table lives in `tokensave:code-health-report`.

## Guardrails

- `tokensave_session_start` / `tokensave_session_end` write/remove `.tokensave/session_baseline.json` — a second `session_start` silently overwrites the baseline, so bracket one session at a time; respect Cursor approval/run-mode.
- `session_end` without a prior `session_start` has no baseline to compare against — start one first.
- Bracket only work where a before/after delta is wanted (refactors, cleanups, health-focused sessions) — don't bracket trivial edits.

## Output

- The per-dimension health delta with a one-line interpretation (improved / degraded / unchanged) and any recommended follow-up.
- If any result includes a `tokensave_metrics:` line, report the savings to the user.
