---
design_status: current
---

# Loom 01 — Follow loaded tail

- **Asset:** `01-follow-loaded-tail.png`
- **Lifecycle:** `current`
- **Boundary:** `CONCEPT / SYNTHETIC DATA`

## User job

See execution progressing through time without confusing the end of a loaded LCM page with a complete global live stream. The main field shows hooks, messages, commands, file activity, tests, tasks, spawns, handoffs, commits, and other authenticated event classes on a left-to-right axis.

## Production authorities

- Sessions/LCM owns loaded-page bounds, pagination, session identity, timestamps, and exact persisted transcript content; the admitted hook/event stream owns visible execution events.
- Agents owns parent/subagent identity, Work owns task identity and transitions, local Git/Code owns files, diffs, commits, symbols, and test evidence, and Delivery owns PR/outcome cross-links.
- A deterministic temporal layout and scene projection map authenticated identities to stable coordinates. Provider state is read-only, and unavailable private reasoning remains unavailable.

## Entry and visible state

Opening `/loom` with a loaded page positions the cursor at that page's end and enables `FOLLOW LOADED TAIL`. `NOW` means **the end of the currently loaded page only**. Coverage and pagination controls state what hosts, sessions, and time bounds are loaded. New events appear only after they are admitted into the loaded page.

Panning, selecting an older node, or opening evidence pauses follow behavior without moving the loaded-page endpoint. `RETURN TO LOADED TAIL` restores the endpoint and resumes following subsequent loaded events.

## Interaction model

- Horizontal pan, zoom, minimap, time ruler, and keyboard seeking retain a stable cursor.
- Event-type, agent, task, file, and evidence filters change visibility, not source truth.
- Nodes open exact evidence; branch headers collapse or expand their causal neighborhood.
- Cross-page links open Sessions, Agents, Work, Code, or Delivery with the time and event selection preserved.

## Evidence and honesty

Nodes and connections use the canonical `exact`, `explicit`, `inferred`, `ambiguous`, `stale`, and `unavailable` treatments. Gaps at either page boundary remain visible. Loom never invents off-page events, private reasoning, a provider live tail, or a relationship merely because two events are nearby.

## Scale and accessibility acceptance

Keyboard follow/pause/return controls, reduced motion, exact event-table/transcript/branch-tree fallbacks, dense-real-data pagination, and visible focus are mandatory. At 200% zoom, the time field and controls reflow without hiding coverage bounds. `CONCEPT / SYNTHETIC DATA` remains visible in the shell.
