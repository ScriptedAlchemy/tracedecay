# Disagreement field, design note

`disagreement-field.html` · shots: `shots/disagreement-field-{dark,light}.png`

The chart overlays the call graph and the co-change graph for the same 25
symbols. A co-change edge joins symbols edited in the same session. The chart
emphasizes pairs that share several editing sessions but have no call edge in
either direction.

## What each channel encodes

| Channel | Encodes |
| --- | --- |
| Node position | Module cluster membership only. Hand-laid; **position carries no magnitude**, and the caption says so outright. |
| Node diameter | Sessions that edited this symbol, 1 → 9. This is the denominator every co-change claim rests on. A symbol touched once cannot co-change with anything, and the reader can see that. |
| Node hue | Symbol kind, `kindColor` arc. |
| Edge: thin solid, edge-strong | **Coupled and called**. A call edge whose ends also co-change. Agreement, and therefore drawn quietest. |
| Edge: fine dotted, edge-subtle | **Called, never co-touched**. A real call edge no captured session has crossed. |
| Edge: conflicting hue, halo + weighted stroke | **Co-changed, not linked.** The finding. Stroke width = shared sessions. |
| Draw order | The chart draws call-only edges first, then coupled edges, then unlinked co-change edges so they stay visible. |

The renderer computes the three legend counts and four header counts from the
same edge list it draws. The chart shows where unlinked pairs cluster. The
table below it names all seven pairs and sorts them by shared sessions.

## Missing session attribution

Three of the 25 symbols (`storage::open_reader`, `kind_histogram`,
`snapshot::validate`) have no session attribution. The chart draws them as open
dashed circles at the low end of the diameter scale and keeps their call edges.
The caption distinguishes unavailable session attribution from a measured
co-change count of zero.

## Data requirements

All 25 symbols, 39 edges, and session counts are synthetic.

- Verify that each session can supply the symbols it edited before deriving
  co-change edges. Sessions from before capture began can produce a false
  "never co-touched" result.
- Count sessions per symbol to size each node. Track missing attribution
  separately so the chart can mark those nodes.
- Call edges are the most likely data to be available already.
- Symbol names and module membership are shaped like this repo's real crates;
  the specific symbols and their pairings are made up.

A single shared session may not indicate coupling. The mockup shows pairs with
as few as two shared sessions but does not state a cutoff. The production view
must state its threshold.

## One open question

Should any graph relationship count as a link, or only a call edge? A trait
impl, serialized schema, config key, generated binding, or test fixture may
couple two symbols without a call edge. If those cases are common, the view
would highlight many pairs whose relationship is already recorded. Classify a
sample of real co-change pairs by hand before shipping the third edge state at
this volume.

## Known mockup shortcuts

- The mockup positions 25 nodes and their labels by hand. Production data needs
  an automatic layout and label placement that do not imply a measured position.
- The chart omits call direction because this comparison only asks whether a
  call edge exists.
- The mockup has no hover or threshold filter. It also cannot open the sessions
  for a pair, which a reader would need to verify the finding.
