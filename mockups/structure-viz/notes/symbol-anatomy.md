# Symbol anatomy — design note

`symbol-anatomy.html` · shots: `shots/symbol-anatomy-{dark,light}.png`

A drill-in for one symbol that puts the code neighbourhood and the *work*
neighbourhood on the same surface. The premise is that "who calls this" and
"who has been arguing about this in a session for three weeks" are the same
question asked twice, and a tabbed detail panel is what stops anyone from ever
noticing that.

## What each channel encodes

| Channel | Encodes |
| --- | --- |
| Bar length, left and right columns | Call sites between that neighbour and this symbol, 0 → 6. **One scale for both columns**, so a caller and a callee bar are directly comparable. |
| Bar hue | Neighbour's symbol kind, via the product's `kindColor` arc (ported verbatim). |
| Vertical order within a column | Call-site count, descending. Not alphabetical, not file order. |
| The curve | The call edge itself. Its length, curvature and crossing pattern encode **nothing** — it is linkage, and the caption says so. |
| Body plate cap hue | This symbol's own kind, same arc. |
| Session tick x-position | When that session last touched this symbol, linear over 63 real days. |
| Session tick height | Messages in that session, 0 → 214. |
| Session tick hue | Host (claude-code / codex / cursor / hermes). |
| Fact rail length | Trust, 0 → 1, anchored at zero. |
| Test dot | Run state — filled green/amber for green/stale, dashed outline for uncovered. |
| Pulsing dot | The only animated mark on the surface: an open session has this symbol in its working set. |

## Where absence is shown rather than cropped

Three deliberate beats, because this is the part a real implementation will be
tempted to shortcut:

1. The 2026-06-02 session has **no recorded message count**. Its tick is drawn
   hollow, dashed, at zero height, on its true x-position; the table row reads
   `unrecorded`, not `0`. The session stays on the axis because it did edit the
   symbol — dropping it would silently shrink the "6 sessions" headline.
2. The body plate prints `doc comment: absent` in the partial hue rather than
   omitting the row. The field exists and is empty; those are different.
3. The uncovered policy-denied branch gets a row in the covering-tests table
   with a dashed empty dot. A test list that only lists tests can never show you
   the hole.

## What data would back it, honestly

Invented: **all of it**. Specifically what does *not* exist today:

- **Call-site counts per neighbour.** The graph has caller/callee edges; a
  per-edge *multiplicity* (how many times B calls A) is the load-bearing number
  for the bar length and I did not verify it is stored. If it isn't, the bar
  should fall back to a uniform mark and the caption must change — an invented
  magnitude is exactly the failure mode this console exists to avoid.
- **Symbol ↔ session attribution.** Sessions are captured and symbols are
  indexed; the join ("this session edited this symbol") is the whole premise of
  the lower half. Message count per session and host are plausible, `last
  touched this symbol` is the field I'd expect to be missing.
- **Facts citing a symbol.** Facts carry trust and feedback; a symbol-level
  citation edge is the piece I'd check first.
- **Covering tests + per-branch coverage.** Test mapping exists in some form;
  "9 of 12 branches" and the named uncovered branch imply branch-level coverage
  data, which is a much stronger claim than a test-to-symbol map.
- Cyclomatic complexity, LOC, dependency depth: these are the fields most likely
  to already exist.

## One open question

**Does the strike indicator survive being right?** It pulses when an open
session has the symbol in its working set — which, on the symbol you are
currently editing, is *always*. The one animated mark on the surface may end up
lit on essentially every drill-in a developer actually opens, at which point it
has told them nothing and cost them the only motion budget the console has. The
honest alternatives are to scope it to *other* sessions (someone else is in
here) or to drop the animation and keep only the `struck 4 min ago` readout.

## Known mockup shortcuts

- Fixed 1440 layout; no responsive behaviour, no virtualization, no empty/error
  states, no keyboard equivalents for the SVG marks (the product would need the
  list-beside-canvas pattern `CodePage` already uses).
- Neighbour columns are capped at what fits; a symbol with 200 callers would
  need a rank cut-off and an honest "top N of M" legend, which this does not
  demonstrate.
