# Normalized navigation

Every final plate uses the same normalized shell named in `DESIGN-SYSTEM.md`.
A workspace may replace the main aperture and its own inspector; it may not
redraw the product navigation.

## Canonical rail

| Channel | Workspace | Route |
|---:|---|---|
| 01 | Brain | `/brain` |
| 02 | Explorer | `/explorer` |
| 03 | Loom | `/loom` |
| 04 | Sessions | `/sessions` |
| 05 | Agents | `/agents` |
| 06 | Code | `/code` |
| 07 | Knowledge | `/knowledge` |
| 08 | Delivery | `/delivery` |
| 09 | Automations | `/automations` |
| 10 | Observatory | `/observatory` |
| 11 | Costs | `/costs` |
| 12 | Settings | `/settings` |
| 13 | Work | `/work` |
| 14 | Workflows | `/workflows` |

Desktop uses one left rail: 192px expanded or 48px compact. Do not duplicate it
as a top product bar. Settings remains channel 12. Doctor belongs inside
Observatory; Command Palette is an overlay, not channel 15.

## Persistent regions

1. **Brand block:** trace-tail glyph and `TRACEDECAY` wordmark at the top of the
   rail. Activation returns to Brain with project scope preserved. It never
   visualizes activity, health, connectivity, or work.
2. **Navigation rail:** all fourteen numbered workspaces in the fixed order
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
- Route changes preserve project scope and workspace query state unless an
  explicit control changes them.
- `/` resolves to Brain. Activating the logo returns to Brain and retains
  scope; the logo has no status or activity behavior.
- Clicking the scope indicator clears an explicit project and returns to
  `Project: all` only when that control is visibly offered.
- Project rows in the command palette may set global project scope; workspace
  rows only navigate.

## Truth constraints

Do not invent Home, Dashboard, Health, Doctor, Approvals, Tools, Integrations,
Policies, Status, Audit, Labs, Stores, Budget, or Orphans as workspaces.
Never collapse independent authorities into a generic nominal-health banner;
show their separate labeled states.
Unresolved scope, absent registry entries, read-only projects, and unavailable
authorities remain visible.
