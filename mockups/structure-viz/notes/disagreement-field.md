# Disagreement field — design note

`disagreement-field.html` · shots: `shots/disagreement-field-{dark,light}.png`

Two graphs over the same 25 symbols — the call graph, and the co-change graph
built from sessions that edited two symbols within one session — overlaid so
that the interesting cell is the one where they **disagree**. The loud state is
the pair that gets edited together over and over with no call edge in either
direction: coupling that is real, repeated, and entirely absent from the code.

## What each channel encodes

| Channel | Encodes |
| --- | --- |
| Node position | Module cluster membership only. Hand-laid; **position carries no magnitude**, and the caption says so outright. |
| Node diameter | Sessions that edited this symbol, 1 → 9. This is the denominator every co-change claim rests on — a symbol touched once cannot co-change with anything, and the reader can see that. |
| Node hue | Symbol kind, `kindColor` arc. |
| Edge: thin solid, edge-strong | **Coupled and called** — a call edge whose ends also co-change. Agreement, and therefore drawn quietest. |
| Edge: fine dotted, edge-subtle | **Called, never co-touched** — a real call edge no captured session has crossed. |
| Edge: conflicting hue, halo + weighted stroke | **Co-changed, not linked.** The finding. Stroke width = shared sessions. |
| Draw order | call-only → coupled → unlinked, so the finding lands on top. A finding that renders under the expected case is not a finding. |

The three counts in the key and the four in the masthead are **computed from the
edge list at render time**, not typed into the markup, so the caption and the
picture cannot drift apart. The seven unlinked pairs are also printed in full
below the field, sorted by shared sessions — the field shows that the state
exists and where it clusters; only the table says *which*.

## Where absence is shown rather than cropped

Three of the 25 symbols (`storage::open_reader`, `kind_histogram`,
`snapshot::validate`) have **no session attribution at all**. They are drawn as
open dashed circles at the low end of the diameter scale, and they still carry
their call edges. Their absence from the co-change layer is **unmeasured, not
zero** — and collapsing that distinction is the easiest lie this chart could
tell, because a symbol nobody has captured a session for looks exactly like a
symbol nobody edits together with anything. The caption names it.

## What data would back it, honestly

Invented: all 25 symbols, all 39 edges, every session count.

- **Co-change edges.** The load-bearing new data: for each session, the set of
  symbols it edited, pairwise. Everything loud on this page comes from that
  join, and it is the piece I would verify exists before drawing any of this.
  Its quality also depends entirely on capture coverage — sessions from before
  capture was installed produce false "never co-touched".
- **Sessions-per-symbol**, for the diameter and for the honesty about the three
  unattributed nodes.
- **Call edges** — the one layer I would expect to be solid.
- Symbol names and module membership are shaped like this repo's real crates;
  the specific symbols and their pairings are made up.

A real version also needs a **threshold**, which this mockup dodges: two symbols
sharing one session is noise. The page shows pairs down to 2 shared sessions and
does not state a cut-off — a shipping version must.

## One open question

**Is "no call edge" the right definition of unlinked, or just the cheap one?**
Two symbols can be genuinely coupled through a trait impl, a serialized schema,
a config key, a generated binding, or a test fixture — none of which is a call
edge, all of which would make a pair land in the loud bucket while being
perfectly well recorded in the code. If that is common, the loud state is mostly
false positives and the chart trains people to ignore it. The question is
whether "linked" should mean *any* graph relationship rather than a call edge,
and I cannot answer it from the mockup: it needs a sample of real co-change
pairs, hand-classified, before the third edge state is worth shipping at this
volume.

## Known mockup shortcuts

- 25 nodes hand-placed, labels hand-positioned. Neither survives real data; a
  real field needs a layout and a label solver, and the "position means nothing"
  caption is only honest as long as the layout stays deliberate.
- Undirected edges. Call direction is dropped, which is fine for the
  disagreement question and would be wrong for anything else.
- No interaction: no hover, no filtering by threshold, no drill-through from a
  pair to the sessions that produced it — which is the obvious next click and
  the thing that would make the finding actionable.
