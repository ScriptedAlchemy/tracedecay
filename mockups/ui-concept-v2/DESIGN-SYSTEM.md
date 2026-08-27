# TraceDecay V2 concept design system

This document is the shared visual authority for every plate in this lookbook.
`NAVIGATION.md` owns route order and persistent shell behavior;
`INTERACTION-STATES.md` owns per-workspace coverage. Plates are concept art
built with synthetic data. They are not runtime evidence and must never imply
that an unavailable product path ships.

## Design thesis

TraceDecay is a code-intelligence instrument, not a generic analytics console.
The outer shell is a restrained graphite chassis. Each workspace opens an
optically deep data aperture inside it. Cyan identifies signal and focus;
amber identifies measured activity or attention only when a legend names that
dimension. Luminous material is welcome when it represents real geometry,
selection, time, or activity. Decorative firing is not.

## Brand

- Wordmark: `TRACEDECAY`, expanded grotesk, ice white, generous tracking.
- Glyph: one origin point followed by three progressively smaller and dimmer
  trace-tail points. It is an identity mark, not a health indicator.
- The logo is identity only. Brain remains available through channel 01; do not
  add a logo action until the shipping shell exposes one.
- Logo hover/focus does not fire, breathe, pulse, or report connectivity,
  health, or work.
- Workspace icons are 14-16px monoline glyphs. An anatomical Brain icon may
  identify channel 01; it is never the product logo or the hero visualization.

## Night-glass tokens

```css
--night-void:      oklch(0.045 0.012 272);
--night-substrate: oklch(0.105 0.018 268);
--night-well:      oklch(0.118 0.004 262);
--night-face:      oklch(0.145 0.003 260);
--night-raised:    oklch(0.228 0.004 260);

--ink-primary:     oklch(0.945 0.002 260);
--ink-secondary:   oklch(0.790 0.003 260);
--ink-muted:       oklch(0.685 0.004 260);
--edge-subtle:     oklch(0.315 0.004 260);
--edge-strong:     oklch(0.460 0.006 260);

--signal-cyan:     oklch(0.750 0.150 200);
--signal-cyan-hot: oklch(0.820 0.160 205);
--activity-amber:  oklch(0.780 0.140 85);
--state-ready:     oklch(0.720 0.140 155);
--state-danger:    oklch(0.620 0.180 25);
--state-violet:    oklch(0.680 0.100 300);

--radius-panel: 2px;
--radius-chip: 1px;
--grid-cell: 32px;
--target-min: 44px;
```

Use one inset highlight and one soft shadow. Heavy bezels belong only on the
outer frame and hero aperture. Avoid floating rounded cards, pill farms,
purple gradients, uniformly glowing borders, and ambient particle theatre.

## Typography and grid

- Display and engraved legends: Archivo Variable, expanded to 112%.
- Body: IBM Plex Sans Variable.
- Values, timestamps, paths, code, and identifiers: IBM Plex Mono with
  tabular figures.
- Base text: 14px. Legends: 10px uppercase with 0.16em tracking. Workspace
  titles: 12px uppercase. Explanations remain sentence case.
- Use an 8px rhythm, 32px minor graticule, and 128px major divisions.
- The normalized shell has a 192px expanded or 48px compact navigation rail,
  a 52px scope/workspace register, one main aperture, an optional workspace-
  owned inspector, and a 32px bottom status strip. Use those region names and
  dimensions exactly as `NAVIGATION.md` does.
- Compact controls may look smaller, but their hit area remains at least
  44x44px.

## Interaction language

- Hover inspects. It raises the face slightly, adds one restrained focus halo,
  dims unrelated material, and may reveal an inspector when that production
  path exists. It never changes scope, fires activity, or changes a measured
  count.
- Keyboard focus is a stable 2px cyan outline with an offset. Hover never
  substitutes for focus.
- Click selects or scopes only when the production control does. Persistent
  selection uses a cyan gutter or position bar and remains identifiable
  without glow.
- Zoom is pointer-centered inside the visualization only. Navigation, labels,
  and inspectors do not scale with the canvas. Concepts use `-`, `100%`, `+`,
  and `Fit` only where the product really exposes zoom.
- Real admitted activity blooms the exact touched identity and may travel one
  evidenced hop. Heat decays with a 4.2s half-life. Hover, focus, selection,
  loading, and connectivity are never activity.
- Reduced motion removes travel, breathing, entrance staging, scanlines, and
  zoom interpolation. Static heat and labels preserve the meaning.

## Typed-state language

Color supplements text and shape; it never carries state alone.

| Family | Treatment | Examples |
|---|---|---|
| ready / served complete | green, solid | `ready`, `complete-zero` |
| loading | neutral ice | `loading` |
| degraded | amber, hatched | `partial`, `stale`, `timed-out`, `conflicting` |
| restricted | violet, crosshatched | `locked`, `redacted`, `unsupported-schema` |
| refusal | red, solid | `denied`, `unauthorized`, `error` |
| disconnected | gray, dashed | `offline`, `unknown`, `cancelled`, `unavailable` |

Always distinguish served empty, `exists:false`, absent/not activated,
transport failure, unavailable authority, truncated, paginated, and stale.
Blank panels and green zeroes are not substitutes.

Cyan is reserved for identity, focus, selection, navigation position, and
measured signal. Green confirms a served ready/complete state. Amber denotes
measured activity or degraded attention only when adjacent text and pattern
name which meaning applies. Violet is restricted, red is refused/failed, and
gray dashed is disconnected or unknown. The same meanings bind the rail,
aperture, inspector, and bottom strip.

## Prompt floor

Every final image prompt begins from this visual floor:

> TraceDecay V2 desktop code-intelligence instrument, cinematic night-glass
> interface, near-black optical data field cut into a machined graphite
> chassis, hairline cyan-gray frames, ice-white engraved labels, sparse cyan
> signal and restrained amber measured activity, fixed fourteen-channel left
> navigation rail, explicit project scope, separate transport/feed/authority
> states, one luminous hero visualization modeled on named production
> authorities using synthetic data only, deep vignette, subtle sensor grain,
> 16:10 composition. Visibly stamp every plate CONCEPT / SYNTHETIC.

Negative floor: no flat generic dashboard, duplicate navigation, rounded SaaS
cards, decorative neural particles, fake live badges, invented health, unlabeled
graphs, Marvel marks, anatomical product logo, or motion unrelated to measured
state. No production, customer, operator, repository, session, or event data.
