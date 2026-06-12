---
name: tracking-session-health
description: Use when bracketing refactor or cleanup work with a code-health baseline and final per-dimension delta to prove whether quality improved or degraded.
---

# Tracking session health

## Workflow

1. **Before the first edit → `tracedecay_session_start`** (no args): snapshots current health metrics as the baseline (`.tracedecay/session_baseline.json`).
2. **Work normally** — edits via `tracedecay:atomic-code-edits` or regular tools; TraceDecay re-indexes as files change.
3. **After the work → `tracedecay_session_end`**: re-scans and returns the per-dimension diff (acyclicity, depth, equality, redundancy, modularity) — what improved, what degraded — and clears the baseline.
4. **Interpret the delta:** a dropped dimension names the follow-up — e.g. redundancy fell → `tracedecay_redundancy` to find what got duplicated; acyclicity fell → `tracedecay_circular`. The full dimension→drill-down table lives in `tracedecay:code-health-report`.

## Guardrails

- `tracedecay_session_start` / `tracedecay_session_end` write/remove `.tracedecay/session_baseline.json` — a second `session_start` silently overwrites the baseline, so bracket one session at a time; respect Cursor approval/run-mode.
- `session_end` without a prior `session_start` has no baseline to compare against — start one first.
- Bracket only work where a before/after delta is wanted (refactors, cleanups, health-focused sessions) — don't bracket trivial edits.

## Output

- The per-dimension health delta with a one-line interpretation (improved / degraded / unchanged) and any recommended follow-up.
- If any result includes a `tracedecay_metrics:` line, report the savings to the user.
