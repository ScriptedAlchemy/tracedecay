# Code topography, concept mockups

Four HTML sheets draw code structure as terrain and calls as flows across that
terrain. The caller and callee views show the surrounding types and modules
instead of tracing a route between two points.

These are concepts, not production code. They import nothing from
`dashboard/src/**` and call no endpoints. No files under `dashboard/` changed
for these mockups. Open any page as a static HTML file.

| Sheet | File | What it is |
| --- | --- | --- |
| 01 CORTEX | [`cortex.html`](cortex.html) | The repository as continuous relief: modules as landforms with elevation (dependency depth), area (symbol mass), contour density (internal connectivity) and churn heat, with bundled call-flow channels and an instrument-plate map legend. |
| 02 TRACE | [`trace.html`](trace.html) | The main sheet shows one symbol against a dimmed cortex. Caller flows converge and callee flows spread out. The flows enter, cross, and leave impl and trait outlines. |
| 03 CORE SAMPLE | [`core-sample.html`](core-sample.html) | Six files as vertical strat columns against one shared line datum: symbols banded at true line span, complexity rails, internal call arcs in the gutter, external arcs across it. |
| 04 LENS | [`lens.html`](lens.html) | The optional fourth sheet uses aggregation ratio as its horizontal axis. Position on that axis represents zoom across repository, module, and symbol scales. |

The notes at the bottom of each page explain the design, name the data each
visual channel would use, and pose an open question. [`NOTES.md`](NOTES.md)
lists the shared rules and the limits of the data the graph returns.

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

Playwright is **not** a dependency of this folder. It is resolved from the
dashboard's `node_modules`, the only place in the repo that has it. The location
comes from an environment variable so no machine-local path is committed:

- `TD_DASHBOARD_DIR`, directory containing the dashboard's `node_modules`.
  **Default: `../../dashboard`, resolved relative to `shoot.mjs`** (i.e. the
  repo's own `dashboard/`).

Override it if the default has no install, for example when only the main
checkout has `dashboard/node_modules`:

```sh
TD_DASHBOARD_DIR=/path/to/main-checkout/dashboard node shoot.mjs
```

The script launches headless Chromium, sets `data-theme` on the document, and
writes full-page PNGs. It starts no daemon and no dev server. All page geometry is
seeded and deterministic (no animation, no randomness), so re-running produces
byte-comparable output for the same source.

## Opening a sheet by hand

Open the `.html` file directly over `file://`; no server is needed. Append
`?theme=light` to load the light theme. The pages use classic scripts because
CORS blocks ES modules on `file://`.

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

The figures use synthetic values and real crate and file paths, such as
`crates/tracedecay-contracts/src/retrieval/service.rs`. They use quantities the
graph stores or computes:
node `kind` / `file_path` / `start_line` / `end_line` / `degree`, edge kinds
`calls` / `uses` / `imports` / `contains`, per-edge call-site counts, file-level
dependency depth, DSM directory clusters, per-symbol cyclomatic complexity, and
git churn. A sheet labels any value the API cannot supply at the shown
granularity, such as the weather strip on sheet 01.
