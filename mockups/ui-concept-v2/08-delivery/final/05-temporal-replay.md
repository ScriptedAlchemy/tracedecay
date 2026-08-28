---
design_status: current
evidence_class: concept_synthetic
---

# Temporal replay

## User job

Replay the PR's creation at a chosen timestamp and understand what became known, changed, or verified next.

## Product behavior

- Scrubbing reveals only events admitted up to the selected time.
- Play, pause, step, speed, Follow loaded tail, and Return to latest controls are keyboard-addressable. Loaded-page playback must not claim a live stream.
- Branches appear at spawn time, carry their own events, and merge only when a recorded handoff or result exists.
- Selecting a replay event opens its exact transcript, task, command, diff, check, or review evidence.

## Truth boundary

Replay is ordered from persisted event time and source authority. Clock skew, missing segments, and inferred ordering are visible; future events are not shown as already known.

## Access gates

- Keyboard controls cover play, pause, previous or next event, range seek, speed, Follow loaded tail, Return to latest, and selected-evidence navigation.
- Reduced motion disables animated travel and interpolated reveals; stepping and the static playback cursor preserve temporal state.
- At 200% zoom, playback controls and selected evidence reflow without shrinking the timeline labels or hiding the loaded-page boundary.
- Exact transcript, event-table, task, command, diff, check, and review fallbacks expose the loaded replay page without relying on animation.

## Production authorities

- The loaded Sessions/LCM page and its persisted event timestamps bound replay; Follow loaded tail never reads beyond that loaded authority.
- Agent spawn and handoff records, Work task transitions, local Git events, code edits, checks, reviews, and release evidence enter replay only through stable source identities.
- The temporal projection marks exact events, explicit persisted claims, inferred order, ambiguous attribution, and unavailable gaps separately. It never synthesizes private reasoning.
