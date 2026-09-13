/**
 * Token sampling for the TRACE canvas.
 *
 * Canvas2D cannot read CSS custom properties, so the resolved strings have to
 * be handed to the renderer. Sampling happens here — once at mount and once per
 * theme flip — rather than inside the draw loop, which keeps `tokens.css` the
 * single source of the instrument's colour at 60 Hz.
 */
import type { TracePalette } from './types.ts';

/** Fallbacks are only reached if a token is missing; they mirror the dark set. */
const FALLBACK: Record<string, string> = {
  '--raw-surface-0': '#141619',
  '--raw-surface-1': '#1c2029',
  '--raw-text-primary': '#eef0f4',
  '--raw-text-muted': '#9aa1b0',
  '--raw-edge-subtle': '#333a46',
  '--raw-edge-strong': '#4c5464',
  '--raw-grid': '#2a2f38',
  '--raw-accent': '#5fd0e0',
  '--raw-state-partial': '#e0b45f',
  '--raw-state-unknown': '#8d919b',
};

export function resolveTracePalette(element: HTMLElement): TracePalette {
  const style = getComputedStyle(element);
  const token = (name: string): string =>
    style.getPropertyValue(name).trim() || FALLBACK[name] || '#888888';

  // Which medium the field is suspended in, measured rather than assumed, so a
  // future theme that is neither of the two shipped ones still resolves.
  const light = document.documentElement.dataset['theme'] === 'light';
  return {
    surface0: token('--raw-surface-0'),
    surface1: token('--raw-surface-1'),
    textPrimary: token('--raw-text-primary'),
    textMuted: token('--raw-text-muted'),
    edgeSubtle: token('--raw-edge-subtle'),
    edgeStrong: token('--raw-edge-strong'),
    grid: token('--raw-grid'),
    accent: token('--raw-accent'),
    // Upstream and downstream are the accent and the muted ink rather than two
    // invented hues: kind colour already owns the hue channel on this field,
    // and a third palette competing with it made the ribbons unreadable.
    upstream: token('--raw-accent'),
    downstream: token('--raw-edge-strong'),
    statePartial: token('--raw-state-partial'),
    stateUnknown: token('--raw-state-unknown'),
    membraneFill: token('--raw-surface-1'),
    light,
  };
}
