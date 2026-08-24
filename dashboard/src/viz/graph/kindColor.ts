import type { CSSProperties } from 'react';

/**
 * Deep-space plasma palette: a node's kind picks a hue on the cyan → violet
 * arc at fixed lightness. One rule instead of a hardcoded map, so every graph
 * in the app harmonizes — repositories and checkouts in Brain, symbol kinds in
 * Code — and an unseen kind still lands somewhere deliberate rather than
 * defaulting to grey. The arc is bounded so colour never wanders into muddy
 * yellows that read as "warning" against the dark field; chroma varies a
 * little across the arc so neighbouring hues stay tellable apart.
 *
 * This lives outside `GraphCanvas` because the canvas is no longer the only
 * consumer: the Code workspace's connectivity spine tints its marks by the
 * same rule, which is what makes the spine and the field above it read as one
 * instrument rather than two views that happen to share a dataset. Two copies
 * of this arithmetic would silently drift; one copy cannot.
 */

/** Stable per-kind hash — the only input the palette has. */
function hashKind(kind: string): number {
  let hash = 0;
  for (let index = 0; index < kind.length; index += 1) {
    hash = (hash * 31 + kind.charCodeAt(index)) >>> 0;
  }
  return hash;
}

/**
 * @param light whether the kind is being drawn against a light medium.
 *
 * A body is lit against its medium, so which side of the substrate it sits on
 * has to flip with the theme. Pinned at L 0.78 the kind hues were tuned for a
 * dark field; on the light field they landed ABOVE the background and forty
 * overlapping translucent discs accumulated into a white cloud with no
 * structure in it at all. On paper a node is saturated ink: darker than its
 * medium, with a little more chroma to hold its hue at the lower lightness.
 * Chroma is what survives overlap. At the old 0.112 the dark hues were pastels
 * sitting near the top of the lightness range, so a dense cluster of them
 * accumulated into an undifferentiated pale mass — the graph lost its colour
 * exactly where it had the most structure to show. Saturated bodies a little
 * further down the range stay tellable apart when they pile up.
 */
export function kindColor(kind: string, light: boolean): string {
  const hash = hashKind(kind);
  const chroma = (light ? 0.135 : 0.152) + ((hash >>> 9) % 6) * 0.012;
  const lightness = light ? 0.55 : 0.72;
  return `oklch(${lightness} ${chroma.toFixed(3)} ${186 + (hash % 148)})`;
}

/**
 * Both sides of the same hue, for DOM marks. Sigma has to be handed one
 * resolved colour because a canvas cannot read CSS variables, but an HTML mark
 * can simply carry both and let the stylesheet pick — which is how the rest of
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
