# Code topography — concept mockups

Four screenshot-ready HTML sheets explore a "code topography" direction: the
dashboard concept draws the codebase as **measured terrain and flow** rather than as
boxes and arrows, and callers/callees drawn as **structure** rather than as a route
between two points.

These are **concepts, not production code.** Nothing here imports from
`dashboard/src/**`, nothing here is wired to an endpoint, and no file under
`dashboard/` was modified. Every page is a static HTML file that can be opened
straight off disk.

| Sheet | File | What it is |
| --- | --- | --- |
| 01 CORTEX | [`cortex.html`](cortex.html) | The repository as continuous relief: modules as landforms with elevation (dependency depth), area (symbol mass), contour density (internal connectivity) and churn heat, with bundled call-flow channels and an instrument-plate map legend. |
| 02 TRACE | [`trace.html`](trace.html) | **Hero sheet.** One symbol as a watershed — caller tributaries converging, callee delta fanning, over a dimmed cortex underlay, with impl/trait membranes the flow enters, moves inside, and leaves. |
| 03 CORE SAMPLE | [`core-sample.html`](core-sample.html) | Six files as vertical strat columns against one shared line datum: symbols banded at true line span, complexity rails, internal call arcs in the gutter, external arcs across it. |
| 04 LENS | [`lens.html`](lens.html) | An optional fourth form, offered with an argument: the horizontal axis *is* the aggregation ratio, so repository / module / symbol scales sit on one sheet and zoom becomes a position instead of a transition. |

Design notes live **on each page** (the three-column plate at the bottom: what
the form is, which real data backs each channel, and one open question).
[`NOTES.md`](NOTES.md) collects the cross-sheet rules and the wire-truth
constraints they were designed against.

## Screenshots

`shots/` holds each sheet at 1440 wide, in both themes, at 2× device scale:

```
shots/cortex-dark.png        shots/cortex-light.png
shots/trace-dark.png         shots/trace-light.png
shots/core-sample-dark.png   shots/core-sample-light.png
shots/lens-dark.png          shots/lens-light.png
```

## Re-shooting

```sh
node shoot.mjs
```

Playwright is **not** a dependency of this folder — it is resolved from the
dashboard's `node_modules`, the only place in the repo that has it. The location
comes from an environment variable so no machine-local path is committed:

- `TD_DASHBOARD_DIR` — directory containing the dashboard's `node_modules`.
  **Default: `../../dashboard`, resolved relative to `shoot.mjs`** (i.e. the
  repo's own `dashboard/`).

Override it when the default has no install — for example in a **git worktree**,
where `dashboard/node_modules` does not exist and the main checkout's does:

```sh
TD_DASHBOARD_DIR=/path/to/main-checkout/dashboard node shoot.mjs
```

The script launches headless Chromium, sets `data-theme` on the document, and
writes full-page PNGs. It starts no daemon and no dev server. All page geometry is
seeded and deterministic (no animation, no randomness), so re-running produces
byte-comparable output for the same source.

## Opening a sheet by hand

Open the `.html` file directly — `file://` works, no server needed. Append
`?theme=light` to load the light theme. The pages are classic scripts rather
than ES modules for exactly this reason: module scripts are blocked by CORS on
`file://`.

## Layout

```
topography.css   the token block (transcribed from dashboard/src/theme/tokens.css)
                 plus the shared chassis: spine, readbar, panels, plates, hatch
topography.js    kindColor.ts arithmetic verbatim, theme-reactive kind paint,
                 and the relief primitives (seeded blobs, bundled splines,
                 tapered ribbons) all four sheets draw with
cortex.html      sheet 01 + its fixture and renderer
trace.html       sheet 02 + its fixture and renderer
core-sample.html sheet 03 + its fixture and renderer
lens.html        sheet 04 + its fixture and renderer
shoot.mjs        screenshot harness
shots/           1440-wide PNGs, dark + light
NOTES.md         cross-sheet design rules and wire-truth constraints
```

## Data

All figures are **fake but honestly shaped**: they use real crate and file paths
from this repository (`crates/tracedecay-application/src/retrieval/service.rs`
and friends) and only quantities the graph actually holds or already computes —
node `kind` / `file_path` / `start_line` / `end_line` / `degree`, edge kinds
`calls` / `uses` / `imports` / `contains`, per-edge call-site counts, file-level
dependency depth, DSM directory clusters, per-symbol cyclomatic complexity, and
git churn. Where a sheet shows something the wire cannot support at the stated
granularity, the sheet says so on itself (see the weather strip on sheet 01).
