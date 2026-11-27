# Design notes, code topography

Each sheet has its own notes. These notes describe the shared design rules,
available data, and limitations.

## The direction

The brief asked to show callers and callees within their surrounding types, the
structure of many files and functions, and "the topography of the code base".
It ruled out boxes-and-arrows UML and diagrams limited to a route between two
points.

The sheets draw modules as terrain with an elevation, area, outline, and contour
density. Calls flow downhill and their width shows call-site count. They can
converge, fork, and enter or leave types. The continuous shapes let readers see
groups of calls in context without tracing each one.

Sheets 01 and 04 use height for dependency depth. Sheet 02 uses hop distance
instead and labels it in its legend.

## Shared design rules

Each mark encodes a named measurement. Region roughness uses the region's
connectivity. Node width uses the symbol's `degree`, and channel width uses
call-site count. Band height is `end_line − start_line`. The rail shows cyclomatic
complexity, and the tint shows churn. The sheets label derived values. For
example, sheet 04 divides the number of symbols by the number of drawn bodies
to get its x-axis values.

Each caption names what produces each visual channel and whether the graph
stores that quantity. Legends give units and intervals. Sheet 01 prints its
aggregation ratio and its contour interval of 0.50 internal edges per symbol,
with an index contour every fifth line. Sheet 03 gives its vertical scale of
1 px per 1.35 source lines and marks the shared line datum. Sheet 04 labels the
aggregation ratio on the ruler itself.

Each sheet shows one example of missing data:

- 01 `hooks/src/vendor/` is on the map at its true position and true area with a
  dashed shoreline and an empty interior: *no relief, 0 of 11 files parsed*.
- 02 a downstream channel ends in a dashed mouth at `dyn ContextSource`, full
  width, then it stops: *channel ends, target not in graph*.
- 03 core 6 is banded to line 331 and the remaining 281 lines are drawn at full
  length and hatched, with the reason and the consequence: *outgoing edges unknown,
  not zero*. So no arc leaves it.
- 04 the axis is drawn to its floor and then hatched: *statement scale not held*.
  The absence of finer resolution is itself a reading.

**Dark and light through CSS variables.** `topography.css` transcribes the token
block from `dashboard/src/theme/tokens.css`, including the `[data-theme='light']`
mappings. Kind hues are not baked: `topography.js` registers one custom property
per kind with its dark value and a `:root[data-theme='light']` override with its
light value, so a theme flip re-tints every band with no observer and no second copy
of the arithmetic. The same pattern `kindColorVars` uses in the app.

## Reuse of the existing language

- **Kind hue** is `kindColor.ts` transcribed verbatim. The same hash, the same
  chroma step, the same `186 + hash % 148` arc, the same two lightness levels. A
  `struct` is the same hue on these sheets as on the Code workspace's connectivity
  spine. Sheet 01 extends the arc to DSM cluster ids so a cluster keeps one hue
  across all four sheets; sheet 02 extends it to the two flow directions.
- **The instrument chassis** is the one the console already has: engraved legends
  with a hairline fill rule, monospaced tabular values, square corners, corner
  brackets, `well / face / raised` as three planes, the graticule behind a field,
  and the graph field's two-source lighting on every canvas.
- **The measured-field philosophy** comes from `brain/field.ts`: axes are
  measurements, an offset inside a cell costs nothing while an offset across one
  would be a lie, and an empty column is a reading. Sheet 01 spreads regions inside
  a (cluster, depth) cell and never across one, for exactly that reason.
- **Magnitude through area, not radius** comes from `CodePage.tsx`'s
  `markDiameter`: symbol counts go through a square root before they become a width.

## Graph data the designs use

- Nodes carry `{id, name, kind, file_path, start_line, end_line, degree}`; edges
  carry kinds `calls` / `uses` / `imports` / `contains`, with per-edge call-site
  counts. Every channel on every sheet reduces to one of these, or to one of the
  derived metrics below.
- File-level dependency depth (Tarjan SCC condensation → longest path) and DSM
  directory clustering exist. They are sheet 01's two axes.
- Per-symbol cyclomatic complexity exists. It is sheet 03's rail.
- Churn / hotspots (git) exist. They are sheet 01's tint.
- Live SSE reports activity at the project level. Sheet 01 labels that scope in
  its weather strip and identifies per-region activity as future work. No other
  sheet shows live data.
- The subgraph endpoint serves roughly 80 to 250 nodes. Sheet 01 labels its
  aggregation of 1 206 symbols into 19 regions and prints the ratio. Sheets 02
  and 03 show individual nodes and print their counts. Sheet 04 labels where the
  display switches from aggregates to individual symbols.

## Known weaknesses

Each sheet prints its own open question. The designs also have these limits:

1. **Correlated channels.** Area and contour density on sheet 01 both grow with
   module size, so a reader can double-count. Coupling ratio (internal ÷ external
   edges) would be less correlated and more actionable, and would cost the "how
   crowded is it here" reading that makes the map legible.
2. **Two containments.** Modules and types are different nestings. Sheet 02 draws
   both, as relief and as membranes, and they already overlap awkwardly at three
   hops; a type whose methods span two files cannot be one convex membrane.
3. **Scale.** Sheet 02 is legible at depth 3, and its tributary count roughly
   cubes by depth 5. Sheet 03 fits six cores, but the brief asks for "many files".
   Each sheet leaves open what to show when the full view no longer fits.
