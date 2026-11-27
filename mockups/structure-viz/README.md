# Structure-viz mockups

These three concepts are outside `dashboard/src/` and use no production
endpoints. They explore how to show links between symbols, threads, sessions,
and memory, and how to draw call chains and architecture without using UML.

| Concept | Page | Note |
| --- | --- | --- |
| 1 · Symbol anatomy | [`symbol-anatomy.html`](symbol-anatomy.html) | [note](notes/symbol-anatomy.md) |
| 2 · Call-chain transit map | [`call-chain-transit.html`](call-chain-transit.html) | [note](notes/call-chain-transit.md) |
| 3 · Disagreement field | [`disagreement-field.html`](disagreement-field.html) | [note](notes/disagreement-field.md) |

[`shots/`](shots/) contains six screenshots, one per page and theme, at 1440 px
wide and 2× device scale.

## Design rules

- Each caption explains what position, size, and brightness measure. It also
  names marks that carry no measurement, such as a connector's length or a
  manually placed node's position.
- Each page keeps at least one slot for missing data and uses a label such as
  `unrecorded`, `absent`, `uncovered`, or `no station`, or draws a hollow mark.
  Missing measurements never appear as zero.
- All data is synthetic. Each note lists the fields a backend would need to
  supply and identifies the fields whose availability is uncertain.

## Open a page

Open any page from `file://`. Theme comes from a query parameter:

    …/symbol-anatomy.html?theme=light     # default is dark

The mockups copy the UI frame tokens from `dashboard/src/theme/tokens.css` and
the `@theme` block of `dashboard/src/theme/tailwind.css`. They also copy
`kindColor()` from `dashboard/src/viz/graph/kindColor.ts` so each kind uses the
same hue as in the product. The copies are necessary because a standalone
`file://` page cannot import the product modules. If the palette or the token
values change, regenerate the mockups rather than patching the copies.

## Re-shooting

    node mockups/structure-viz/shoot.mjs                  # all six
    node mockups/structure-viz/shoot.mjs disagreement     # one page, both themes

`shoot.mjs` opens `file://` pages without a dev server or daemon. It loads
`@playwright/test` from the dashboard's `node_modules`. In a git worktree without
its own install, use `TD_DASHBOARD_DIR` to point to an installed dashboard.
