---
design_status: current
---

# Loom 08 — Work proximity

- **Image:** [Generated concept](08-work-proximity.png)
- **Interactive concept:** [Open the existing concept application](../../app/README.md)
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
- Affected strands fade from their base color into amber for shared-code
  candidates or coral for observed overlapping edits, then back after the local
  interval. The minimap repeats the same colored strand segments. Outlines appear
  only on hover or keyboard focus, as a secondary selection affordance. Shared symbols
  alone do not establish duplicate implementation or an impending conflict.
  The fade describes the observed activity interval, not predicted risk or proof
  of resolution; source labels and the encounter navigator retain that distinction.
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

The implementation belongs to the imported application's existing Loom components
under `app/src/loom/`. It reuses the retained event/session identities, evidence
workspaces and source modes. The isolated HTML study is superseded, not a second
application entry point. Existing execution, replay and selected-event states stay
available alongside proximity. Ordinary curve crossings carry no relationship
claim; only labeled encounter intervals carry proximity evidence.

Run the focused journey from `mockups/ui-concept-v2/app` with the app server running:

```sh
BASE_URL=http://127.0.0.1:5195 node qa/loom-proximity.mjs
```
