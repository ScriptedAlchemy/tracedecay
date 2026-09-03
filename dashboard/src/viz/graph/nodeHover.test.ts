import { describe, expect, it } from 'vitest';
import { createNodeHoverDrawer } from './nodeHover.ts';
import { rgb, rgba, type GraphPalette, type ThemeBox } from './palette.ts';
import type { Settings } from 'sigma/settings';

/**
 * The hover pass, drawn against both mediums.
 *
 * The regression this guards: Sigma's default hover drawer paints an opaque
 * white shadowed disc and a white label backdrop, which read as the hovered
 * body "going white" and growing a blob on the dark field. Every paint this
 * drawer makes must come from the theme box it was handed — and from the box's
 * CURRENT colors, so a theme flip re-lights hovers without a rebuild.
 */

const DARK: GraphPalette = {
  hot: [93, 231, 255],
  edge: [55, 83, 114],
  label: [196, 212, 232],
  labelFont: 'ui-monospace, monospace',
  substrate: [7, 11, 22],
  dim: [38, 55, 76],
  light: false,
};

const LIGHT: GraphPalette = {
  hot: [11, 116, 145],
  edge: [148, 166, 188],
  label: [32, 42, 56],
  labelFont: 'ui-monospace, monospace',
  substrate: [244, 246, 250],
  dim: [210, 218, 228],
  light: true,
};

interface PaintOp {
  op: 'stroke' | 'fillRect' | 'fillText';
  style: string;
}

/** A recording 2d context: every stroke and fill is captured with the style it
 * was painted in, which is the whole question this file asks. */
function recordingContext(): { context: CanvasRenderingContext2D; paints: PaintOp[] } {
  const paints: PaintOp[] = [];
  const context = {
    fillStyle: '',
    strokeStyle: '',
    lineWidth: 0,
    font: '',
    beginPath: () => undefined,
    arc: () => undefined,
    stroke() {
      paints.push({ op: 'stroke', style: String(this.strokeStyle) });
    },
    measureText: (text: string) => ({ width: text.length * 6 }),
    fillRect() {
      paints.push({ op: 'fillRect', style: String(this.fillStyle) });
    },
    fillText() {
      paints.push({ op: 'fillText', style: String(this.fillStyle) });
    },
  };
  return { context: context as unknown as CanvasRenderingContext2D, paints };
}

const SETTINGS = {
  labelSize: 11,
  labelWeight: 'normal',
  labelFont: 'ui-monospace, monospace',
} as Settings;

const NODE = { x: 10, y: 20, size: 6, label: 'alpha::beta', color: 'rgb(93, 231, 255)' };

/** Styles that would reproduce the white-blob regression. */
const WHITES = [/rgb\(255,\s*255,\s*255\)/, /#fff/i, /\bwhite\b/];

describe('createNodeHoverDrawer', () => {
  it('paints only theme colors on the dark field: accent ring, substrate backdrop, label ink', () => {
    const theme: ThemeBox = { colors: DARK };
    const { context, paints } = recordingContext();
    createNodeHoverDrawer(theme)(context, NODE, SETTINGS);

    expect(paints).toEqual([
      { op: 'stroke', style: rgba(DARK.hot, 0.9) },
      { op: 'fillRect', style: rgba(DARK.substrate, 0.85) },
      { op: 'fillText', style: rgb(DARK.label) },
    ]);
    for (const paint of paints) {
      for (const white of WHITES) expect(paint.style).not.toMatch(white);
    }
  });

  it('flips with the theme box without a rebuild: the same drawer re-lit from the light palette', () => {
    const theme: ThemeBox = { colors: DARK };
    const drawer = createNodeHoverDrawer(theme);
    theme.colors = LIGHT;

    const { context, paints } = recordingContext();
    drawer(context, NODE, SETTINGS);

    expect(paints).toEqual([
      { op: 'stroke', style: rgba(LIGHT.hot, 0.9) },
      { op: 'fillRect', style: rgba(LIGHT.substrate, 0.85) },
      { op: 'fillText', style: rgb(LIGHT.label) },
    ]);
  });

  it('draws only the ring for a body whose label the reducer withheld', () => {
    const { context, paints } = recordingContext();
    createNodeHoverDrawer({ colors: DARK })(context, { ...NODE, label: '' }, SETTINGS);

    expect(paints).toEqual([{ op: 'stroke', style: rgba(DARK.hot, 0.9) }]);
  });
});
