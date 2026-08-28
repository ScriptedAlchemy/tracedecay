---
design_status: current
---

# Loom 02 — Temporal replay

- **Asset:** `02-temporal-replay.png`
- **Lifecycle:** `current`
- **Boundary:** `CONCEPT / SYNTHETIC DATA`

## User job

Replay how work unfolded rather than read a flattened event list. The playback cursor reveals only events at or before the selected timestamp; later events remain visibly unrevealed instead of dimly implying knowledge the reviewer has not reached.

## Production authorities

- Sessions/LCM owns the loaded replay window, timestamp ordering, pagination, and exact transcripts; admitted hook/event records own the revealable execution stream.
- Agents owns branch parentage, Work owns task transitions, local Git/Code owns commit/diff/test evidence, and Delivery owns PR/outcome pivots.
- The deterministic temporal projection applies the replay cursor to stable layout coordinates without mutating source evidence. Provider state stays read-only, and private reasoning is unavailable unless represented by an allowed persisted artifact.

## Entry and visible state

Replay begins from the loaded-tail state or a selected range. The transport exposes play/pause, previous/next meaningful episode, speed, timestamp, range, and `RETURN TO LOADED TAIL`. Branches emerge when their evidenced spawn time is reached, and handoff/rejoin edges appear only when their event becomes visible.

## Interaction model

- Scrub, arrow-key seek, episode-step, or play continuously while preserving horizontal pan and semantic zoom.
- Follow-loaded-tail and replay are mutually legible modes; replay never silently snaps to the endpoint.
- Selecting an event pauses playback and opens its evidence without revealing future nodes.
- Filters and collapsed branches show counts for both revealed and withheld events without exposing future content.

## Evidence and honesty

The cursor is applied to the loaded page, not a fabricated complete history. Exact source timestamps remain distinct from inferred ordering. Ambiguous ordering, stale joins, missing pages, and unavailable private reasoning stay labeled by the canonical evidence ladder.

## Scale and accessibility acceptance

Playback is keyboard-operable and screen-reader announced without announcing every animation frame. Reduced motion uses discrete cursor steps. Exact chronological table/transcript/branch-tree fallbacks honor the same reveal boundary. Dense-real-data histories aggregate between meaningful episodes, and 200% zoom preserves transport, timestamp, and selected evidence. `CONCEPT / SYNTHETIC DATA` remains visible.
