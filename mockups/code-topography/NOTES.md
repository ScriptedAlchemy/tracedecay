# Design notes — code topography

Per-sheet notes are printed on each sheet. This file holds what is common to all
four: the rules they obey, the data they are allowed to claim, and where they are
knowingly weak.

## The direction

The ask was for callers/callees traced **over the surrounding types**, a way to see
the structure of many files and many functions in a file, and "the topography of the
code base" — explicitly not boxes-and-arrows UML and not a route-between-two-points
diagram.

Everything here follows from one decision: **treat structure as terrain and traffic
as flow.** A module is not a container with a border, it is a landform with an
elevation, an area, a shoreline and a contour density. A call is not an arrow, it is
water with a measured volume that runs downhill, converges, forks, crosses
boundaries, and enters and leaves types. Both are continuous, which is what lets a
reader take in a whole basin at a glance instead of tracing a path.

The sheets share a vertical semantic on purpose: **height is dependency depth**.
Sheet 01 establishes it, sheet 04 carries it through unchanged, sheet 02 uses hop
distance instead and says so in its own legend rather than borrowing the authority
of the elevation axis.

## House rules, and how each sheet keeps them

**Every position, size, elevation and width encodes a stated measurement.** There
is no decorative mark on any of the four sheets. Blob roughness is fed the region's
own connectivity, node width is the symbol's `degree`, channel width is the call-site
count, band height is `end_line − start_line`, the rail is cyclomatic complexity,
the tint is churn. Where a value is derived rather than stored (sheet 04's x axis is
symbols ÷ drawn bodies), the sheet says "derived".

**Captions say exactly what encodes what.** Every sheet carries a `channel → data`
list naming the channel, the underlying quantity, and whether the graph really holds
it. Legends state units and intervals like a real instrument plate: sheet 01 prints
its contour interval (0.50 internal edges per symbol, index contour every fifth
line) and its aggregation ratio; sheet 03 prints its vertical scale (1 px = 1.35
source lines) and its datum; sheet 04 prints the aggregation ratio on the ruler
itself.

**Absence is drawn, not cropped — one deliberate beat per sheet.**

- 01 `hooks/src/vendor/` is on the map at its true position and true area with a
  dashed shoreline and an empty interior: *no relief, 0 of 11 files parsed*.
- 02 a downstream channel ends in a dashed mouth at `dyn ContextSource` — full
  width, then it stops: *channel ends, target not in graph*.
- 03 core 6 is banded to line 331 and the remaining 281 lines are drawn at full
  length and hatched, with the reason and the consequence: *outgoing edges unknown,
  not zero* — so no arc leaves it.
- 04 the axis is drawn to its floor and then hatched: *statement scale not held*.
  The absence of finer resolution is itself a reading.

**Dark and light through CSS variables.** `topography.css` transcribes the token
block from `dashboard/src/theme/tokens.css`, including the `[data-theme='light']`
mappings. Kind hues are not baked: `topography.js` registers one custom property
per kind with its dark value and a `:root[data-theme='light']` override with its
light value, so a theme flip re-tints every band with no observer and no second copy
of the arithmetic — the same pattern `kindColorVars` uses in the app.

## Reuse of the existing language

- **Kind hue** is `kindColor.ts` transcribed verbatim — the same hash, the same
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

## Wire truth these were designed against

- Nodes carry `{id, name, kind, file_path, start_line, end_line, degree}`; edges
  carry kinds `calls` / `uses` / `imports` / `contains`, with per-edge call-site
  counts. Every channel on every sheet reduces to one of these, or to one of the
  derived metrics below.
- File-level dependency depth (Tarjan SCC condensation → longest path) and DSM
  directory clustering exist. They are sheet 01's two axes.
- Per-symbol cyclomatic complexity exists. It is sheet 03's rail.
- Churn / hotspots (git) exist. They are sheet 01's tint.
- **Live SSE activity is project-granular today.** Sheet 01's weather strip is
  therefore captioned as project-level, in the strip itself, with per-region weather
  named as a future state. No other sheet claims liveness.
- **The subgraph endpoint serves roughly 80–250 nodes.** Sheet 01 declares itself an
  aggregate (19 regions standing for 1 206 symbols) and prints the ratio. Sheets 02
  and 03 declare themselves direct renders and print their node counts to show they
  are inside the band. Sheet 04 turns the cap into a labelled position on its ruler
  — the point where bodies stop being masses and start being symbols.

## Known weaknesses

Each sheet prints its own open question. The three that cut across all four:

1. **Correlated channels.** Area and contour density on sheet 01 both grow with
   module size, so a reader can double-count. Coupling ratio (internal ÷ external
   edges) would be less correlated and more actionable, and would cost the "how
   crowded is it here" reading that makes the map legible.
2. **Two containments.** Modules and types are different nestings. Sheet 02 draws
   both, as relief and as membranes, and they already overlap awkwardly at three
   hops; a type whose methods span two files cannot be one convex membrane.
3. **Scale.** Sheet 02 is legible at depth 3 and its tributary count roughly cubes
   by depth 5; sheet 03 fits six cores and the ask said "many files". Each sheet
   names its degradation strategy as an open question rather than pretending it
   scales.
