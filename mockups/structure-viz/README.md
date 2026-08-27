# Structure-viz mockups

Screenshot-ready **concept mockups** — not production code, not wired to
anything, and deliberately outside `dashboard/src/`. Three surfaces exploring
one question: how do symbols connect to threads, sessions and memory, and how do
we draw call chains and architecture without falling back on bland UML?

| Concept | Page | Note |
| --- | --- | --- |
| 1 · Symbol anatomy | [`symbol-anatomy.html`](symbol-anatomy.html) | [note](notes/symbol-anatomy.md) |
| 2 · Call-chain transit map | [`call-chain-transit.html`](call-chain-transit.html) | [note](notes/call-chain-transit.md) |
| 3 · Disagreement field | [`disagreement-field.html`](disagreement-field.html) | [note](notes/disagreement-field.md) |

Shots in [`shots/`](shots/), dark and light, 1440 wide at 2× — six files.

## Ground rules these follow

- **Every position, size and brightness encodes a stated measurement**, and each
  figure's caption says exactly what encodes what — including the channels that
  encode *nothing* (a connector's length, a hand-laid node's position).
- **Absence is shown, not cropped.** Each page carries at least one deliberate
  beat where missing data keeps its slot and is labelled `unrecorded`,
  `absent`, `uncovered`, `no station`, or drawn hollow. Unmeasured is never
  collapsed into zero.
- **All data is invented.** Each note names precisely which fields a real
  backend would have to serve and which ones I am least confident exist today.

## Running them

Open any page from `file://`. Theme comes from a query parameter:

    …/symbol-anatomy.html?theme=light     # default is dark

Chrome tokens are copied verbatim from `dashboard/src/theme/tokens.css` and the
`@theme` block of `dashboard/src/theme/tailwind.css`; `kindColor()` is a
line-for-line port of `dashboard/src/viz/graph/kindColor.ts` so a kind lands on
the same hue here as in the product. Those copies exist only because a
standalone `file://` page cannot import the product modules — **if the palette
or the token values move, these mockups are stale and should be regenerated
rather than patched.**

## Re-shooting

    node mockups/structure-viz/shoot.mjs                  # all six
    node mockups/structure-viz/shoot.mjs disagreement     # one page, both themes

`shoot.mjs` drives `file://` directly — no dev server, no daemon. It resolves
`@playwright/test` from whichever `dashboard/` directory actually has
`node_modules` (a git worktree has none of its own); override with
`TD_DASHBOARD_DIR`.
