import type { CSSProperties } from 'react';

/**
 * Kind palette: a fixed ordinal ramp of eight slots inside the cool band the
 * design system leaves free for identity (teal h 168 through blue h 248),
 * separated by lightness as much as by hue. Every graph in the app draws from
 * it: repositories and checkouts in Brain, symbol kinds in Code, providers and
 * runs elsewhere. The band is closed on purpose. Amber means measured activity,
 * violet means restricted, red means refused and a saturated h 155 green means
 * ready, so a kind that landed on any of them would read as a state it is not.
 * Chroma stays under the signal cyan's 0.15 so a kind never passes for focus.
 *
 * The common symbol kinds hold named slots, so the kinds a field shows most are
 * the ones kept furthest apart; any other name hashes onto the same ramp, so an
 * unseen kind is still stable across reloads and never falls back to grey.
 *
 * This lives outside `GraphCanvas` because the canvas is not the only
 * consumer: the Code workspace's connectivity spine tints its marks by the
 * same rule, which is what makes the spine and the field above it read as one
 * instrument rather than two views that happen to share a dataset.
 */

/**
 * Each slot is one hue drawn against both media. A body is lit against its
 * medium, so which side of the substrate it sits on flips with the theme: on
 * the dark field a slot is light and moderately saturated; on paper it is ink,
 * darker than the medium with a little more chroma to hold its hue at the lower
 * lightness. The two ramps stay in step: the slot that stands furthest from
 * the dark field (ice, L 0.90) is also the darkest ink on paper (L 0.41), so
 * relative prominence survives a theme flip. Chroma is what survives overlap:
 * forty translucent pastel discs accumulate into an undifferentiated pale mass,
 * saturated bodies further down the range stay tellable apart when they pile up.
 */
interface KindSlot {
  dark: string;
  light: string;
}

const CYAN_SLOT: KindSlot = { dark: 'oklch(0.8 0.12 202)', light: 'oklch(0.49 0.135 205)' };

const KIND_RAMP: readonly KindSlot[] = [
  CYAN_SLOT,
  { dark: 'oklch(0.68 0.13 248)', light: 'oklch(0.58 0.145 250)' }, // blue
  { dark: 'oklch(0.78 0.11 185)', light: 'oklch(0.5 0.125 186)' }, // teal
  { dark: 'oklch(0.9 0.045 222)', light: 'oklch(0.41 0.06 225)' }, // ice
  { dark: 'oklch(0.7 0.12 225)', light: 'oklch(0.57 0.135 228)' }, // sky
  { dark: 'oklch(0.62 0.1 186)', light: 'oklch(0.63 0.115 188)' }, // deep teal
  { dark: 'oklch(0.86 0.075 168)', light: 'oklch(0.44 0.09 170)' }, // seafoam
  { dark: 'oklch(0.82 0.08 244)', light: 'oklch(0.47 0.095 246)' }, // slate blue
];

const NAMED_SLOTS: Readonly<Record<string, number>> = {
  function: 0,
  method: 1,
  struct: 2,
  class: 2,
  trait: 3,
  interface: 3,
  module: 4,
  file: 4,
  enum: 5,
  type: 5,
  field: 6,
  constant: 6,
  variable: 6,
  impl: 7,
};

/** Stable per-name hash for kinds without a named slot. */
function hashKind(kind: string): number {
  let hash = 0;
  for (let index = 0; index < kind.length; index += 1) {
    hash = (hash * 31 + kind.charCodeAt(index)) >>> 0;
  }
  return hash;
}

function kindSlot(kind: string): KindSlot {
  const slot = NAMED_SLOTS[kind.toLowerCase()] ?? hashKind(kind) % KIND_RAMP.length;
  return KIND_RAMP[slot] ?? CYAN_SLOT;
}

/** @param light whether the kind is being drawn against a light medium. */
export function kindColor(kind: string, light: boolean): string {
  const slot = kindSlot(kind);
  return light ? slot.light : slot.dark;
}

/**
 * Both sides of the same hue, for DOM marks. Sigma has to be handed one
 * resolved colour because a canvas cannot read CSS variables, but an HTML mark
 * can simply carry both and let the stylesheet pick, which is how the rest of
 * this console answers a theme flip (see the `[data-theme=light]` variants in
 * the shell). Returned as custom-property values so the caller styles with
 * `bg-[var(--kind-dark)]` and a `[[data-theme=light]_&]` variant, with no
 * observer, no re-render and no theme state to keep in sync.
 */
export function kindColorVars(kind: string): CSSProperties {
  // React types `style` as CSSProperties, which has no index signature for
  // custom properties; the cast is the standard way to hand it one and is
  // confined to this single line rather than repeated at every call site.
  return {
    '--kind-dark': kindColor(kind, false),
    '--kind-light': kindColor(kind, true),
  } as CSSProperties;
}
