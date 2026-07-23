# Dashboard design: shell, layout, and visual system

Design authority for the PR14 fresh-start dashboard. Owned by the design
owner; foundation and workspace lanes implement against this document and do
not make structural, styling, or dependency decisions. Binding contracts
(journeys, typed envelopes, state taxonomy, accessibility and performance
gates) live in [Plan 11](11-dashboard-frontend.md); this file decides how
they look, sit, and move. Decided 2026-07-23.

## Design principles

1. **Truth before polish.** The plan's sixteen domain states, coverage
   denominators, and evidence quality are the product. Layout and styling
   exist to make truthful states legible at a glance — never to smooth them
   over. An `unknown` is designed, not blank.
2. **Calm density.** Dense data with generous rhythm: one type scale, one
   spacing scale, restrained borders, whitespace doing the separation work.
   No more than one competing accent per region.
3. **Progressive disclosure along the journeys.** Overview → finding/entity →
   investigation → evidence/action is the plan's spine; each step reveals the
   next level in place (inspector, not page-jump) and deep links capture the
   exact position.
4. **Keyboard is a first-class pointer.** Every flow works keyboard-only per
   the plan's active-descendant rules; the command palette is the fastest
   path to anything.
5. **Severity is not quality.** Two independent visual axes everywhere:
   severity/consequence (how bad) and evidence quality (how sure). They never
   blend into one color.

## Shell layout

One responsive shell, four fixed regions plus content:

- **Left navigation rail.** The twelve workspaces (Brain, Explorer, Loom,
  Sessions, Agents, Code, Knowledge, Delivery, Automations, Observatory,
  Costs, Settings; Work joins in PR17), icon+label, collapsible to icons at
  narrow widths, bottom-anchored Settings. The rail is navigation only — no
  status, no badges except a single Doctor attention dot driven by typed
  findings.
- **Top scope bar.** The active scope (profile/project/repository/worktree/
  branch/time window) rendered as removable chips, always visible per the
  plan's scope rule; cross-scope transitions are explicit chip edits. Right
  side: freshness indicator (SSE liveness + last watermark), theme toggle,
  command palette affordance.
- **Command palette** (Cmd/Ctrl-K, Radix Dialog): scope-aware search across
  workspaces, entities, saved deep links, and *legal actions only* — an
  action appears solely when its typed action reference exists for the
  current selection. Palette results carry the same truth metadata as lists
  (state chip, scope).
- **Right inspector panel.** The universal drill-down surface: finding,
  entity, TaskId, operation, citation. Resizable, keyboard-reachable
  (Enter opens, Escape closes and restores focus), URL-addressed (the deep
  link encodes inspector identity), stackable one level (breadcrumb, no
  infinite drawers). Evidence expansion happens here, never in tooltips.
- **Status strip** (bottom, one line): daemon connection state, active
  query/run progress with cancel, background operation receipts. Live
  regions coalesce announcements to ≤1/s per the plan.

## Layout archetypes

Every workspace composes from four archetypes; no bespoke layouts:

1. **Overview grid** (Brain, Costs, Observatory landing): responsive card
   grid, each card one read model with its truth strip; cards link into
   archetype 2/3. No card renders a computed grade.
2. **Explorer split** (Explorer, Sessions, Knowledge, Agents, Delivery,
   Automations): left query/filter column (collapsible), center
   virtualized result table/list (36px data rows, 44px touch targets,
   sticky header, left-aligned text / right-aligned tabular numbers /
   state chips leading), right inspector. The planner-progress panel
   renders per-source progress rows here.
3. **Canvas + table** (Loom, Code graph, Brain map, PR17 topology): the
   renderer-neutral canvas above, the synchronized accessible table below
   (toggleable to table-only), shared selection, playback controls docked
   to the canvas, cluster/aggregation counts always visible.
4. **Config surface** (Settings, per-workspace preferences): sectioned
   forms, effective-vs-desired layered values side by side, typed patch
   preview → validate → CAS confirm as distinct steps.

## Visual system

Token architecture (Tailwind v4, two-stage so runtime theming works):

- Raw channel values live on `:root` and `[data-theme="light"]` /
  `[data-theme="dark"]` / `[data-contrast="more"]` scopes; a **non-inline**
  `@theme` maps semantic tokens onto those variables (values must not be
  baked at build time). Forced-colors mode defers to system colors
  (`forced-color-adjust: auto` by default; opt-outs only where a state chip
  would lose meaning, with a system-color replacement).
- Semantic token families: `surface-{0..3}` (elevation by background, not
  shadow), `text-{primary,secondary,muted}`, `edge-{subtle,strong}`,
  `accent` (single hue), and the truth families below. Role names only;
  no raw palette names in components.
- **Domain-state tokens**: one token per Plan 11 state
  (`state-loading`, `state-ready`, `state-partial`, `state-stale`,
  `state-locked`, `state-denied`, `state-unauthorized`, `state-redacted`,
  `state-conflicting`, `state-offline`, `state-unknown`,
  `state-cancelled`, `state-timed-out`, `state-error`,
  `state-unsupported-schema`, `state-complete-zero`). Every state chip is
  token + icon + label — never color alone.
- **Severity axis**: five ordered steps (`sev-info` → `sev-critical`),
  expressed as border/fill weight on a single hue ramp.
- **Evidence-quality axis**: four steps (`ev-measured`, `ev-associated`,
  `ev-predicted-calibrated`, `ev-unknown`), expressed as fill pattern/
  solidity (solid, hatched, dotted outline, dashed outline) so it survives
  monochrome and forced colors and can never be confused with severity.
- Dark is the default theme; light and contrast-more are complete first-class
  mappings, not overrides of a few tokens.

Typography and rhythm:

- One stack: `Inter var` (bundled, offline) for UI, `ui-monospace` stack for
  code/identifiers, `font-variant-numeric: tabular-nums` on all data cells.
- Type scale: 12/13/14/16/20/24 px; body-data is 13, UI chrome 14. Spacing
  on a 4px grid; data-row height 36px; panel padding 16px; card gap 12px.
- Radius 6px standard, 4px chips; borders 1px `edge-subtle`; elevation by
  surface step, shadows reserved for overlays only.

Motion:

- Durations 120ms (state), 180ms (panel), easing standard-decelerate;
  nothing animates position on data update (values change in place with a
  brief background pulse using `state-*` at 8% alpha). Reduced-motion
  replaces all of it with instant changes and starts playback paused per the
  plan.

## Component conventions

- **EvidenceTruthStrip**: one-line strip on every compact result — authority
  icon, coverage fraction (never a bare percent without denominator),
  freshness age, citation count, omission count, score kind label. Always
  visible; the inspector expands each element.
- Tables: TanStack Virtual above 200 rows per the plan's mounts budget;
  selection is row-level with checkbox column appearing on first selection;
  column set customization persisted in the presentation allowlist only.
- Empty states: `complete_zero_findings` gets a designed confirmation
  (what was covered, when); every other "nothing here" renders its true
  domain state. No illustrations standing in for unknown.
- Forms never submit on Enter from a text field inside multi-field CAS
  surfaces; destructive/mutating actions always show the typed preview
  first.
- Charts (ECharts, lazy): one categorical palette derived from the token
  ramp, direct labeling over legends where ≤4 series, axis units mandatory,
  uncertainty bands rendered when the read model provides intervals.

## Implementation notes for foundation lanes

- `dashboard/src/theme/tokens.css` holds the raw scopes + `@theme` mapping;
  `dashboard/src/ui/` holds the variant-layer primitives (CVA over Radix)
  that consume only semantic tokens. Workspace lanes consume `ui/` and
  archetype shells; they do not write raw Tailwind color/spacing utilities
  outside the token set (lint-enforced via a class allowlist check in CI).
- The four archetypes ship as shell components (`ui/archetypes/`) with the
  regions as slots; workspaces fill slots and own only their read-model
  wiring and workspace-specific panels.
