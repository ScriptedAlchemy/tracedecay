# Normalized navigation

Every final plate uses the same normalized shell named in `DESIGN-SYSTEM.md`.
A workspace may replace the main aperture and its own inspector; it may not
redraw the product navigation.

## Canonical rail

| Channel | Task family | Route | Views (visible tab strip) |
|---:|---|---|---|
| 01 | Code | `/code` | Code `/code`, Delivery `/code/delivery` |
| 02 | Memory | `/memory` | Brain `/memory`, Explorer `/memory/explorer`, Knowledge `/memory/knowledge`, Sessions `/memory/sessions`, Loom `/memory/loom`, Agents `/memory/agents` |
| 03 | Work | `/work` | Work `/work` (board and DAG), Workflows `/work/workflows` |
| 04 | Operations | `/operations` | Observatory `/operations`, Automations `/operations/automations`, Costs `/operations/costs` |
| 05 | Settings | `/settings` | Settings `/settings` |

The fourteen workspaces this rail used to number were grouped into these five
families by issue #4220; `docs/dashboard-task-map.md` records the task each
serves and where every retired path (`/brain`, `/sessions`, `/loom`, ...) now
lands. A family page draws a tab strip of real links over its views; the first
view answers at the family path.

Desktop uses one left rail: 192px expanded or 48px compact. Do not duplicate it
as a top product bar. Settings remains its own channel. Doctor belongs inside
Observatory, the first view of Operations, which carries the Doctor dot;
Command Palette is an overlay, not a channel.

## Persistent regions

1. **Brand block:** trace-tail glyph and `TRACEDECAY` wordmark at the top of the
   rail. It is identity only and never visualizes activity, health,
   connectivity, or work. Brain remains available as the first view of Memory.
2. **Navigation rail:** the five numbered task families in the fixed order
   above; 192px expanded or 48px compact.
3. **Scope/workspace register:** a 52px register containing `Project: all` or
   the reconciled project label and canonical ID, the active channel/title,
   view-local controls, and real query/cancel state when applicable.
4. **Main aperture:** the workspace visualization or primary read model.
5. **Inspector:** workspace-owned and shown only by a supported selection or
   drill-down path.
6. **Bottom status strip:** a 32px strip separating Link, Feed, Query,
   Registry/Authority, and recovery states. `Link live` never implies synced
   data, accepted activity, or health.

## Behavior

- Selected route: 3px cyan gutter, raised face, cyan channel number, white label.
- Hover: inspect with a raised face and primary text without changing
  selection, scope, measured values, or activity.
- Keyboard: native Tab order and Enter activation. `Cmd/Ctrl+K` opens the
  command palette. Do not invent numeric or arrow-key global shortcuts.
- Route changes preserve global project scope unless an explicit control
  changes it. View-local query persistence follows the shipping workspace; a
  concept may not promise persistence without sidecar evidence.
- `/` redirects to Brain at `/memory`. The Memory rail entry navigates to
  `/memory` while retaining global project scope; the logo is not an
  additional control. Retired paths redirect with their query string.
- Clicking the scope indicator clears an explicit project and returns to
  `Project: all` only when that control is visibly offered.
- Project rows in the command palette may set global project scope; view rows
  (every view of every family, searchable by family name) only navigate.

## Production evidence

- Family names/order, their views, and `/`'s Brain surface:
  `dashboard/src/app/channels.ts`, `dashboard/src/app/routes.tsx`,
  `dashboard/src/app/channels.test.ts`, and
  `dashboard/src/app/routes.dom.test.tsx` (every retired path redirects).
- The family tab strip: `dashboard/src/app/shell/FamilyLayout.tsx`.
- Rail navigation and selected-route treatment:
  `dashboard/src/app/shell/NavRail.tsx`.
- Shell composition and the command-palette hotkey:
  `dashboard/src/app/shell/Shell.tsx`.
- Explicit scope clear and URL persistence:
  `dashboard/src/app/shell/ScopeBar.tsx`,
  `dashboard/src/data/scope/UrlSync.tsx`, and
  `dashboard/src/data/scope/UrlSync.dom.test.tsx`.
- Command-palette workspace navigation and registered-project scoping:
  `dashboard/src/app/shell/CommandPalette.tsx` and
  `dashboard/src/app/shell/CommandPalette.dom.test.tsx`.

## Truth constraints

Do not invent Home, Dashboard, Health, Doctor, Approvals, Tools, Integrations,
Policies, Status, Audit, Labs, Stores, Budget, or Orphans as workspaces.
Never collapse independent authorities into a generic nominal-health banner;
show their separate labeled states.
Unresolved scope, absent registry entries, read-only projects, and unavailable
authorities remain visible.
