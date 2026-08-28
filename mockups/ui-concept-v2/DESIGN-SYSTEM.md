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

### Loaded-page playback language

Playback is bounded by the events currently loaded into the client. Use
`FOLLOW LOADED TAIL` for the mode that keeps the playback cursor on the newest
event in that loaded page, and `RETURN TO LOADED TAIL` for the action that
restores that position after paused inspection. `NOW` means the newest event
in the loaded page. It is not a promise of a live connection, continuous
streaming, complete history, or automatic arrival of newer events. Feed and
pagination state must remain visible beside playback state.

### Accessibility acceptance floor

- Every pointer operation has a keyboard path with visible focus, logical
  order, and no hover-only information. Dense visualizations provide a
  keyboard-navigable exact list, tree, table, transcript, or diff fallback.
- Color is never the sole carrier of identity, grade, status, selection,
  activity, branch, or causality. Pair it with text, shape, line style,
  pattern, or position.
- Reduced motion preserves chronology, causality, selection, activity, and
  evidence grade through static states; it never removes information.
- At 200% browser zoom, primary navigation, labels, controls, evidence text,
  inspectors, diffs, and feedback remain readable and operable. Regions
  reflow, resize, collapse, or enter a dedicated focus mode rather than clip
  essential content or force the canvas to scale text illegibly.

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

## Evidence-grade ladder

Every causal link, attribution, identity, count, finding, and status shown in
a concept or shipping surface uses exactly one of these evidence grades. Grade
is an ordered description of the support available for the displayed claim;
it is not a confidence percentage.

| Grade | Meaning | Required treatment |
|---|---|---|
| `EXACT` | A direct source fact or stable identity read from its owning authority. | Name and link the exact source record; use a solid relation. |
| `EXPLICIT` | A persisted user or agent claim, rationale, assumption, or decision artifact. | Attribute the speaker/artifact and time; present it as a claim or decision, not automatically as repository truth. |
| `INFERRED` | A relation derived by correlation rather than stated by a source authority. | Name the correlation basis and use an inferred/dashed relation without decimal confidence theatre. |
| `AMBIGUOUS` | Multiple plausible source records, identities, or causal candidates remain. | Preserve the candidates and the unresolved choice; never select one silently. |
| `STALE` | A source exists, but its declared freshness window has elapsed. | Show the source timestamp and freshness boundary; do not silently substitute cached state for current state. |
| `UNAVAILABLE` | Evidence is missing, denied, private, not ingested, or otherwise inaccessible. | Name the reason when known and leave an honest gap; never reconstruct private reasoning or fabricate a source. |

`EXACT` and `EXPLICIT` distinguish direct authoritative facts from persisted
claims. A commit identity can be `EXACT` while rationale written in its commit
message is `EXPLICIT`. `INFERRED` never becomes `EXACT` through visual polish.
`AMBIGUOUS`, `STALE`, and `UNAVAILABLE` remain visible states until new source
evidence changes the grade.

Source classes are orthogonal metadata, not competing grades. Labels such as
`RETAINED`, `OBSERVED`, `PR BODY`, `COMMIT`, `TRANSCRIPT`, and `CHECK RESULT`
say where or how evidence was persisted or observed. They must appear beside a
grade when useful, but they never replace one. For example, a repository-read
commit SHA may be `COMMIT / EXACT`; rationale in the same message is
`COMMIT / EXPLICIT`; a linkage from a transcript episode to that commit may be
`TRANSCRIPT + COMMIT / INFERRED`; a missing provider transcript is
`TRANSCRIPT / UNAVAILABLE`.

All grades remain legible without color, under reduced motion, from the
keyboard-accessible fallback, and at 200% browser zoom.

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
