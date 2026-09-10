---
design_status: current
---

# Loom 08 — Work proximity

- **Image:** [Generated concept](08-work-proximity.png)
- **Interactive concept:** [Open prototype](08-work-proximity.html)
- **Boundary:** CONCEPT / SYNTHETIC DATA. Neither artifact establishes production availability.

## User job

Find where concurrent agents approach the same code, understand whether they
share a worktree, and inspect the evidence without losing the wider execution.
Retain the normal Loom execution lens. Proximity adds an overview and encounter
drilldown; it does not replace provenance with decorative curves.

## Interaction and evidence contract

- Stable time runs left to right. Threads represent identifiable agents or
  session attempts; bundles represent named workstream participations.
- Bend affected threads toward each other only within an evidenced activity
  interval. Screen distance is an explanatory layout, not a measured risk score.
- Observed overlapping edits use a localized coral marker. Separate-worktree
  shared-code candidates use amber with a distinct dashed outline. Shared symbols
  alone do not establish duplicate implementation or an impending conflict.
- Proximity never creates spawn, handoff, rejoin, or causal edges. Those require
  their own evidence. Unaffected threads retain their identity colors.
- Select a marker or navigator row to open the encounter's source identities,
  paths/ranges, access types, time interval, worktrees, and exact revisions.
  Return restores the overview window and selection. Exact source and session
  pivots preserve that context when the target evidence is available.
- Pan, zoom, minimap selection, and replay operate on the same time coordinates.
  Replay withholds future observations and event details rather than dimming them.
  Following refers only to the loaded page's tail.
- Missing coverage is unknown, not evidence that agents are safely separated.
  Expiration makes an observation stale; it does not resolve the condition.

## Production authorities

Sessions/Agents supply host and attempt identity; admitted edit/tool observations
supply times and source events; Code supplies paths, symbols and ranges; Git
supplies worktree, head and common-base identity. Confirmed content conflicts
require actual Git/content evidence. Semantic duplication needs a named comparison
basis and coverage; neither temporal proximity nor exact file equality suffices.

## Image interpretation

The generated image communicates composition, local warning color and the
overview-to-evidence transition. Its simplified sidebar, incidental slogan,
floating glyphs, and arbitrary curves do not override the shared design system.
Use the fourteen canonical navigation items. Event glyphs belong on their actual
threads. The minimap viewport must correspond exactly to the visible window.
Do not reproduce unsupported confidence numbers or treat a shared symbol as
proven duplication. The interactive concept corrects these rendering limitations.

## Acceptance

Exercise encounter selection and return, pan/zoom/minimap alignment, and replay
before/within/after an observation. Preserve keyboard selection, visible focus,
reduced motion, a readable exact encounter list, and reflow at 200% zoom.
Synthetic examples remain visibly labeled; recorded mode must disclose unavailable
evidence instead of substituting this demonstration.

The dependency-free HTML is a bounded interaction prototype: 32 illustrative
agents, two encounters, temporal-window controls and local evidence detail.
Smooth authored layout knots give each strand a recognizable path; ordinary
curve crossings carry no relationship claim. Only labeled encounter intervals
carry proximity evidence. Event glyphs and minimap strands share the same path
coordinates, including when replay revokes an encounter's local bend.
Semantic aggregation, backend pivots and live observation ingestion remain
production work; the execution-plate control opens the existing design reference.
Run its focused browser journey with the dashboard's existing Playwright install:

```sh
node mockups/ui-concept-v2/03-loom/final/08-work-proximity.check.mjs
```

The check exercises pointer and keyboard selection, backward replay revocation
of detail/markers/bends, minimap alignment and narrow-viewport reflow.
