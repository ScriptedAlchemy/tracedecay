# Call-chain transit map — design note

`call-chain-transit.html` · shots: `shots/call-chain-transit-{dark,light}.png`

A call chain drawn as a route across horizontal strata, where the strata are
ordered by a *measured* quantity. The point is that layering violations become a
**silhouette** rather than a finding you have to read: route A is a staircase
with no risers, route B has one riser, and you can tell them apart from across
the room without reading a single symbol name.

## What each channel encodes

| Channel | Encodes |
| --- | --- |
| Band (vertical position) | The crate that owns the station. |
| Band **order** | Measured dependency depth — the longest path from that crate to a leaf crate in the workspace's Cargo dependency graph. `tracedecay-domain` = 0, `tracedecay-api` = 5. |
| Gutter numeral | That depth, printed. It is the axis tick; the crate name annotates it. |
| Station x-position | Position in the chain, 1 → 8, evenly spaced. Encodes **nothing** about time, cost or call frequency. |
| Station hue | Symbol kind, `kindColor` arc. |
| Station numeral | Chain order. |
| Line | The chain. Level runs plus a 45° diagonal per hop — except where the depth crossed exceeds the horizontal room a hop gets, where the diagonal is simply steeper. |
| Line colour | Accent for a hop that descends or stays level; error hue plus a halo and a double stroke for a hop that climbs. |
| Foot ruler numeral | That hop's depth delta, negative downward. |

Both panels share one band geometry, one scale and one station spacing, so the
two silhouettes are directly comparable — that is the whole argument for
stacking them rather than putting them side by side.

## Where absence is shown rather than cropped

`tracedecay-hooks` and `tracedecay-tool-catalog` sit at depths 4 and 3 and
neither chain enters them. They keep their full band height, their gutter label
and their depth numeral, hatched and captioned `NO STATION ON THIS ROUTE`. A
strata chart that quietly drops the layers a chain skips is structurally
incapable of showing you a layer being skipped — which is one of the two things
this chart is for. Route B additionally leaves `tracedecay-domain` empty, and
that empty band is the visible fact that the correction path never reaches the
domain model.

## What data would back it, honestly

Invented: every symbol, every hop, both routes, and all depth values.

- **Dependency depth per crate.** The one number the entire chart's ordering
  rests on. A workspace Cargo graph makes this computable and it is the thing I
  would verify first, because if the ordering is editorial the chart is a UML
  diagram with better typography and the caption is a lie.
- **The chain itself.** Call chains between two symbols are a graph query; an
  8-station chain is plausible. What is invented is that these *particular*
  eight symbols form a chain.
- **Crate ownership per symbol.** Should be free from file paths.
- The eight crates are real names from this workspace; the depths assigned to
  them are guesses that happen to be consistent with the obvious layering.

## One open question

**Which chain does a user get, and who picks it?** The map is compelling for one
chosen chain and says nothing about how that chain was chosen. Between two
symbols there are usually many paths; the shortest one is the least likely to
contain the violation, and "all of them" is a hairball. The honest options are
(a) pick the chain that maximises depth climbs and label it as such — a
*worst-path* view, useful and clearly captioned; (b) let the user drive it from
a selected symbol; or (c) drop the two-symbol framing and draw every chain that
contains a climb, which is a different chart. I do not think the surface is
shippable until that selection rule is stated on it.

## Known mockup shortcuts

- Exactly 8 stations, hand-sized to the width. Longer chains need either
  scrolling or a rank cut-off, and the label spacing does not survive either.
- Station labels are `module` + short name; the crate comes from the band. A
  chain crossing two modules with the same leaf name would read ambiguously.
- A climbing hop is not necessarily a bug — a legitimate callback climbs. The
  page says so; a real surface would need to let the user mark a climb as
  intended, which implies persistent state this mockup does not model.
