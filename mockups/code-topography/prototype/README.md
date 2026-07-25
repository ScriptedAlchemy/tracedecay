# TRACE — round two, the live physics prototype

Round one (`../trace.html`) settled the **spatial** language of the TRACE
surface. It could not answer the question the sensory contract actually asks:
does the structure *feel* like what it is. This folder is that answer — the same
26-node subgraph, the same visual grammar, with real spring physics under it, a
flight recorder on it, and a gate that can fail.

Requirements this implements live in
`docs/plans/tracedecay-v2/11b-structure-visualization.md`, sections *Sensory
contract*, *Rendering strategy*, and *Topography round one — coordinator
verdict*.

> This is still a **prototype**, not `dashboard/src`. But `sim.js` is written to
> production standard because it is the module that gets lifted: pure, seeded,
> DOM-free, deterministic, and covered by `sim.test.mjs`. The renderer and the
> page are throwaway scaffolding around it.

## Files

| File | What it is |
| --- | --- |
| `sim.js` | **Pure** simulation. Hand-rolled position-Verlet integrator, per-node velocity damping, anchor springs + channel springs. No DOM, no canvas, no clock, no `Math.random` at runtime. Exposes `step(dt)`, `applyDrag`, `release`, `settle`, and readback of positions / velocities / stretches / energy. |
| `dataset.js` | The subgraph, carried over unchanged from the static sheet. Also `buildSimSpec` (measurement → simulation vocabulary) and `hopRings` (the distance the sheet's rows actually encode). |
| `render.js` | Canvas2D only. Draws channels, membranes, ports, relief underlay, node sills, focus basin, labels, hover bloom. Computes no physics and reads no CSS — it is handed positions and a resolved palette. |
| `recorder.js` | Flight recorder: per-frame `{t, frameMs, workMs, positions}` plus timeline marks, exported as JSON. Also `displacements` and `settleFrame`, the shared definitions of "followed" and "settled". |
| `trace-live.html` | Composes the three. Hover / drag / release, reduced-motion mode, theme flip, live gauges, `window.__traceRecorder`, `window.__traceHarness`. |
| `sim.test.mjs` | 14 numeric tests, `node --test`, zero dependencies. |
| `qa-drive.mjs` | Playwright driver: real pointer gestures, recorded, asserted, screenshotted. |
| `shots/` | Keyframes from the last QA run. |

## How to run

### The tests

```sh
node --test mockups/code-topography/prototype/
```

No install. `node:test` and `node:assert/strict` only.

### The page

ES modules are blocked over `file://`, so the page needs an origin. The QA
script doubles as the static server:

```sh
node mockups/code-topography/prototype/qa-drive.mjs --serve
# → serving http://127.0.0.1:<port>/trace-live.html
```

Query parameters: `?theme=light`, `?seed=<int>`, `?realtime=1` (wall-clock
accumulator instead of one fixed step per frame — for judging feel on a machine
that is not holding 60 Hz; determinism is knowingly given up there and nowhere
else).

Interactions: **hover** a node to feel its weight, **drag** it to deform its
neighbourhood, **release** and watch it settle. The `Reduced motion` button (and
`prefers-reduced-motion`) switches to the one-shot settle.

### The QA gate

Playwright is not a dependency of this folder — it is resolved out of the
dashboard's `node_modules`, the only place in the repo that has it. The location
comes from an environment variable, so no machine-local path is committed:

```sh
# default: ../../../dashboard relative to this script
node mockups/code-topography/prototype/qa-drive.mjs

# or point it somewhere else
TD_DASHBOARD_DIR=/path/to/dashboard node mockups/code-topography/prototype/qa-drive.mjs
```

It opens the page, runs a scripted drag on the heaviest hub and on a leaf,
asserts on the recording, writes nine keyframes to `shots/`, and exits non-zero
if any check fails.

## Tuning parameters

Every knob, its default, and why that default. These are the numbers the owner's
feel feedback moves.

| Parameter | Default | Meaning | Why this value |
| --- | --- | --- | --- |
| `anchorBase` | `90` | Anchor stiffness at unit mass, force/px. | Sets the absolute speed of the field. At 40 the 63-degree hub took ~2.5 s to settle, which reads as broken rather than heavy; 90 puts the whole mass range in 0.8–1.75 s. |
| `anchorMassExponent` | `0.5` | Anchor stiffness scales as `mass^this`. | The one deliberate compression on this page. At `0` every node keeps its own natural frequency and the hub settles ~4.6x slower than a leaf — true to the measurement, unusable as an interface. At `1` mass cancels out entirely and the weight channel dies. `0.5` gives settle time ∝ `mass^0.25`: a **2.19x** spread across this subgraph, monotone, felt, never stalling. |
| `edgeStiffnessScale` | `6` | Channel stiffness = `this × call sites`. | Puts the strongest channel (58 call sites → k 348) above typical anchor stiffness so coupled code moves as flesh, and the weakest (1 call site → k 6) far below it so loose code trails. Measured follow ratios 20.6 % vs 6.5 %. |
| `dampingRatio` | `0.72` | Damping ratio against each node's own anchor spring. | Underdamped, so there is one visible overshoot — flesh, not jelly. Measured swing sequence, mass-independent by construction: **100 % → 3.97–4.10 % → 0.16 % → 0.006 %**. At `1.0` the return is inert; below ~0.5 it rings. |
| `substep` | `1/240` | Integrator substep, seconds. | Four substeps per 60 Hz frame. Keeps the stiffest channel far from the stability limit and keeps post-release energy decay monotone to within 1e-6 relative. |
| `restSpeed` | `0.6` | px/s below which the gauge calls the field settled. | A speed the eye cannot see at this scale. The QA gate demands a much tighter `0.02` before taking a "before" reading, because a drifting field would credit the drag with the startup settle's motion. |
| `minMass` | `3` | Degree floor. | The unresolved node has degree 0. Zero mass is infinite acceleration; a floor keeps absence a body with inertia instead of a numerical singularity. |
| `jitter` | `6` | px of seeded startup displacement. | Enough that the field visibly breathes into place (75 frames) and the physics announces itself; small enough that no label collides on the way. Applied once, from the seed, never at runtime. |
| `bloomAttack` | `9.5` | Hover bloom approach rate at unit mass, 1/s, growing. | Leaf reaches half bloom in **0.13 s** (a flick), hub in **0.42 s** (arrives late and keeps arriving). |
| `bloomRelease` | `5.0` | …decaying. | Slower than attack, so a bloom lingers rather than snapping off. |
| `bloomMassExponent` | `0.42` | Bloom rate divided by `mass^this`. | Tuned so the latency spread is ~3.2x — clearly wider than the settle-time spread, because hover is where a reader first meets a node's weight. |

Renderer constants, same idea:

| Constant | Default | Why |
| --- | --- | --- |
| `TENSION_FLOOR_PX` | `5` | Below this a channel is at rest; drawing a rail for 1 px of stretch produced label noise across the whole field. |
| `TENSION_SATURATION_PX` | `60` | The reduced-motion tension rail saturates here. Drawn at true scale a 172 px stretch is a 23 px slab that swamps the picture. |
| gate `dragDistance` | `200` px | Long enough to deform, short enough to keep the target inside the field for both gestures. |

## Measured baseline — QA run of 2026-07-25

Chromium headless, 1440x1500 viewport, `deviceScaleFactor: 1`, dark theme,
`anchorMassExponent 0.5`, all defaults above. 18/18 checks passed.

### Gestures

| | hub | leaf |
| --- | --- | --- |
| node | `focus` (`resolve_context`) | `ctest` (`contributes_catalog`) |
| mass (degree) | 63 | 4 |
| channels | 7 | 1 |
| channel under test | `hctx`, stiffness 204 (34 call sites) | `profile`, stiffness 18 (3 call sites) |
| gesture | 200 px along that channel's axis | 200 px along that channel's axis |
| dragged travel | 200.0 px | 203.3 px |
| **follow ratio** | **20.6 %** | **6.5 %** |
| next-largest mover | `hctx`, 41.2 px | `profile`, 13.2 px |
| worst node ≥3 channels away | `dispatch`, 4.08 px = **2.0 %** | `mount`, 5.84 px = **2.9 %** |
| **settle frames after release** | **166** (2.77 s) | **97** (1.62 s) |
| **step+paint p95** | **2.50 ms** | 2.60 ms |
| step+paint p50 / max | 2.30 / 7.00 ms | 2.30 / 5.30 ms |
| frame interval p50 / p95 | 16.70 / 17.80 ms | 16.70 / 19.10 ms |
| frames captured / dropped | 385 / 0 | 291 / 0 |

The frame **interval** maxima in a take are ~300 ms. Those are the QA driver's
own screenshot pauses, which freeze `requestAnimationFrame`; they are why the
budget is asserted against `workMs` (the page's step+paint cost) and the interval
is only used to prove no frames were dropped. A page holding 60 Hz has an
interval of 16.7 ms *by definition*, so asserting "frame time p95 < 16.7 ms"
against the interval can never pass — the first run of this gate failed exactly
that way before the two clocks were separated.

### Weight channel, isolated

Frames for a body pulled 200 px to return inside 2 % of home (envelope
definition — see the note in `sim.test.mjs` about why a speed threshold is not
monotone here):

| degree | 3 | 4 | 9 | 12 | 20 | 31 | 41 | 46 | 63 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| settle frames | 48 | 52 | 64 | 69 | 78 | 87 | 94 | 97 | 105 |
| settle seconds | 0.80 | 0.87 | 1.07 | 1.15 | 1.30 | 1.45 | 1.57 | 1.62 | 1.75 |
| half-bloom seconds | 0.117 | 0.133 | 0.200 | 0.217 | 0.267 | 0.317 | 0.350 | 0.367 | 0.417 |

Spread across the real subgraph: **2.19x** settle, **3.2x** hover latency.
Startup settle from seeded jitter: **75 frames** (1.25 s).

## Keyframes in `shots/`

| Shot | What it shows |
| --- | --- |
| `hub-1-rest.png` | The field at equilibrium. Identical to the static sheet, because every channel's rest length is its anchor-to-anchor distance. |
| `hub-2-mid-drag.png` | Hub at 50 % of the gesture. The `RetrievalService` membrane stretches with its members; `hctx` has been pulled in; loaded channels brighten. |
| `hub-3-release-plus-100ms.png` | 100 ms after release — the return, mid-flight. |
| `hub-4-settled.png` | Back home. |
| `leaf-1..4` | Same four beats on the 4-degree leaf. The contrast is the point: it flicks, and its loose partner barely notices. |
| `settled-light.png` | Light theme, settled. The renderer answers a theme flip from the token block, not from a second palette. |
| `reduced-motion-settled.png` | Reduced motion at rest — no rails, because nothing is under tension. |
| `reduced-motion-held.png` | Reduced motion with a node held. Tension is amber core rails with the stretch printed in px. |

## What is deliberately NOT here

- **Warmth** (churn recency) and **grain** (cyclomatic complexity). Both are real
  channels in the sensory contract, but they need a time axis and a contour pass.
  Mixing them in now would make it impossible to tell which mapping a reviewer is
  reacting to.
- **The full cortex relief.** The underlay is simplified to two contours per
  region. Shorelines still move with their members, so a crossing is still a
  cross-module call — that was the part that had to be proven live.
- **Semantic zoom** between CORTEX / TRACE / CORE SAMPLE. That is the LENS
  navigation model and a separate question.

## Open questions for round three

1. `anchorMassExponent` is the only place a measurement is compressed for
   usability. Is `0.5` the right trade, or should the hub be allowed to feel as
   slow as it measurably is (`0` → 4.6x spread, ~2.5 s hub settle)?
2. Release currently carries **no fling**: a pinned node has zero velocity, so
   letting go drops the node rather than throwing it. Momentum on release would
   feel better but encodes nothing measured. Feel channel or dishonest garnish?
3. The reduced-motion tension rail saturates at 60 px. A saturating scale means
   two very different stretches can draw the same thickness. Print the figure
   always (current behaviour) or switch the rail to a log scale?
4. Hover bloom is currently exclusive — one node at a time. Should a hover bloom
   the neighbourhood in proportion to channel stiffness, so weight and coupling
   are felt in the same gesture?
