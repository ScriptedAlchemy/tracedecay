---
design_status: current
evidence_class: concept_synthetic
---

# Temporal replay

## User job

Replay the PR's creation at a chosen timestamp and understand what became known, changed, or verified next.

## Product behavior

- Scrubbing reveals only events admitted up to the selected time.
- Play, pause, step, speed, and Return to Live controls are keyboard-addressable.
- Branches appear at spawn time, carry their own events, and merge only when a recorded handoff or result exists.
- Selecting a replay event opens its exact transcript, task, command, diff, check, or review evidence.

## Truth boundary

Replay is ordered from persisted event time and source authority. Clock skew, missing segments, and inferred ordering are visible; future events are not shown as already known.
