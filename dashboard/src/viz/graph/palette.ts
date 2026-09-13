import { cssColorToRgb } from './activation.ts';

/** The resolved theme tokens the canvas renderers need, sampled from the DOM
 * and handed to everything that draws. Nothing in here decides anything about
 * the data; it is the medium the field is lit against. */
export interface GraphPalette {
  hot: [number, number, number];
  edge: [number, number, number];
  label: [number, number, number];
  labelFont: string;
  /** What a node fades INTO as its signal decays: the substrate itself. */
  substrate: [number, number, number];
  dim: [number, number, number];
  light: boolean;
}

/** A palette that outlives any single sample, so the reducers and the glow
 * pass can be handed one object at construction and still see the tokens a
 * later theme flip re-sampled. */
export interface ThemeBox {
  colors: GraphPalette;
}

/** Samples the resolved theme tokens Sigma needs; canvas renderers cannot
 * consume CSS variables directly, so we re-sample on every theme flip. */
export function palette(element: HTMLElement): GraphPalette {
  const style = getComputedStyle(element);
  const token = (name: string, fallback: string): [number, number, number] => {
    // Sigma's WebGL programs parse colors themselves and accept only
    // hex / rgb() / named forms. Our oklch tokens resolve to `lab(...)`,
    // which they cannot parse — every node and edge silently became black.
    // Normalize through the canvas parser (which does understand lab/oklch)
    // so the renderer always receives a form it can read.
    return cssColorToRgb(style.getPropertyValue(name).trim() || fallback);
  };
  // The graph is a dark analytical instrument in both shell themes. Reading
  // graph-specific tokens instead of shell surfaces keeps its labels, edges
  // and glow coherent when a light shell surrounds the field.
  const substrate = token('--raw-graph-substrate', '#070b16');
  const light = (substrate[0] * 299 + substrate[1] * 587 + substrate[2] * 114) / 1000 > 128;
  return {
    hot: token('--raw-graph-accent', '#5de7ff'),
    edge: token('--raw-graph-edge', '#375372'),
    label: token('--raw-graph-text', '#c4d4e8'),
    /* A node label is a code symbol, so it belongs to the same mono face as
     * every other symbol, path and measured value in the app — read from the
     * token rather than named here, because a canvas that hard-codes its own
     * family is a canvas that quietly stops matching the design system. Kept as
     * a raw string: unlike the colors above it is not a color to normalize. */
    labelFont:
      style.getPropertyValue('--font-mono').trim() || 'ui-monospace, monospace',
    substrate,
    dim: token('--raw-graph-dim', '#26374c'),
    light,
  };
}

export function rgb([r, g, b]: [number, number, number]): string {
  return `rgb(${r}, ${g}, ${b})`;
}

export function rgba([r, g, b]: [number, number, number], alpha: number): string {
  return `rgba(${r}, ${g}, ${b}, ${Math.max(0, Math.min(1, alpha)).toFixed(3)})`;
}
