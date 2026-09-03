import Graph from 'graphology';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ActivationField, lerpRgbTuple, restingNodeTint, luma } from './activation.ts';
import { createFieldRenderer, createFocusState, type FocusState } from './renderer.ts';
import { rgb, rgba, type GraphPalette, type ThemeBox } from './palette.ts';
import type { Settings } from 'sigma/settings';

/**
 * The renderer's interaction states, asserted as the colors they actually
 * paint — on both mediums.
 *
 * Every state on the canvas is a response to something real: rest is measured
 * vitality, hover recolours the body to the hot accent and dims everything
 * outside the neighbourhood, a strike lerps toward the accent and swells, and
 * selection holds the accent. The regressions this file exists for are the
 * theme-blind ones — a body or hover pass that paints white (or any color not
 * derived from the theme box) reads as data on one medium and vanishes on the
 * other.
 */

type NodeAttributes = Record<string, unknown>;
type NodeReducer = (node: string, data: NodeAttributes) => NodeAttributes;
type HoverDrawer = (
  context: CanvasRenderingContext2D,
  data: { x: number; y: number; size: number; label: string; color: string },
  settings: Settings,
) => void;

const captured = vi.hoisted(() => ({
  settings: undefined as
    | { nodeReducer?: NodeReducer; defaultDrawNodeHover?: HoverDrawer }
    | undefined,
  handlers: new Map<string, (payload: never) => void>(),
}));

vi.mock('sigma', () => ({
  default: class MockSigma {
    constructor(_graph: Graph, _container: HTMLElement, settings: Record<string, unknown>) {
      captured.settings = settings;
    }
    setCustomBBox() {}
    getCanvases(): Record<string, HTMLCanvasElement> {
      return {};
    }
    on(event: string, handler: (payload: never) => void) {
      captured.handlers.set(event, handler);
    }
    refresh() {}
    resize() {
      return this;
    }
    setSetting() {}
    kill() {}
  },
}));

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

const KIND_RGB: [number, number, number] = [120, 180, 240];

interface Harness {
  reducer: NodeReducer;
  hoverDrawer: HoverDrawer;
  focus: FocusState;
  field: ActivationField;
  enterNode: (node: string) => void;
  leaveNode: () => void;
  attrs: (node: string) => NodeAttributes;
  setSelected: (node: string | null) => void;
}

function buildField(colors: GraphPalette): Harness {
  const graph = new Graph({ multi: true, type: 'mixed' });
  for (const node of ['a', 'b', 'c']) {
    graph.addNode(node, {
      x: 0,
      y: 0,
      size: 5,
      label: `symbol ${node}`,
      vitality: 0.6,
      kindRgb: KIND_RGB,
    });
  }
  graph.addEdgeWithKey('a->b', 'a', 'b', { srcReal: 'a', dstReal: 'b', size: 1 });
  const focus = createFocusState();
  const field = new ActivationField();
  let selected: string | null = null;
  const theme: ThemeBox = { colors };
  createFieldRenderer({
    graph,
    container: {} as HTMLElement,
    theme,
    focus,
    field,
    neighborsOf: new Map([
      ['a', ['b']],
      ['b', ['a']],
      ['c', []],
    ]),
    nodeCount: 3,
    denseField: false,
    roominess: 1,
    frame: { x: [0, 1], y: [0, 1] },
    selectedId: () => selected,
    onNodeClick: () => undefined,
    onStageClick: () => undefined,
    onFocusChange: () => undefined,
  });
  const settings = captured.settings;
  if (!settings?.nodeReducer || !settings.defaultDrawNodeHover) {
    throw new Error('renderer registered no reducer or hover drawer');
  }
  return {
    reducer: settings.nodeReducer,
    hoverDrawer: settings.defaultDrawNodeHover,
    focus,
    field,
    enterNode: (node) =>
      captured.handlers.get('enterNode')?.({ node } as never),
    leaveNode: () => captured.handlers.get('leaveNode')?.(undefined as never),
    attrs: (node) => graph.getNodeAttributes(node),
    setSelected: (node) => {
      selected = node;
    },
  };
}

function paintedColor(harness: Harness, node: string): string {
  return String(harness.reducer(node, harness.attrs(node))['color']);
}

beforeEach(() => {
  captured.settings = undefined;
  captured.handlers.clear();
});

describe('field renderer interaction states', () => {
  it('rests every body in its vitality tint, lit against the correct side of each medium', () => {
    for (const colors of [DARK, LIGHT]) {
      const harness = buildField(colors);
      const painted = paintedColor(harness, 'a');
      expect(painted).toBe(rgb(restingNodeTint(colors.substrate, KIND_RGB, 0.6, colors.light)));
      // The ember rule: a resting body never dims past its own background —
      // lighter than a dark substrate, darker than a light one.
      const match = /rgb\((\d+), (\d+), (\d+)\)/.exec(painted);
      const paintedLuma = luma([
        Number(match?.[1]),
        Number(match?.[2]),
        Number(match?.[3]),
      ]);
      if (colors.light) expect(paintedLuma).toBeLessThan(luma(colors.substrate));
      else expect(paintedLuma).toBeGreaterThan(luma(colors.substrate));
    }
  });

  it('recolours the hovered body to the hot accent and dims only outside its neighbourhood', () => {
    for (const colors of [DARK, LIGHT]) {
      const harness = buildField(colors);
      harness.enterNode('a');
      expect(harness.focus.target).toBe(1);
      // The overlay owns the easing; the reducer reads wherever it has got to.
      harness.focus.t = 1;

      expect(paintedColor(harness, 'a')).toBe(rgb(colors.hot));
      // The neighbour keeps its resting tint; the stranger dims to the token.
      const resting = restingNodeTint(colors.substrate, KIND_RGB, 0.6, colors.light);
      expect(paintedColor(harness, 'b')).toBe(rgb(resting));
      expect(paintedColor(harness, 'c')).toBe(
        rgb(lerpRgbTuple(resting, colors.dim, 1)),
      );

      harness.leaveNode();
      expect(harness.focus.target).toBe(0);
    }
  });

  it('half-eased isolation dims the stranger exactly as far as the easing has got', () => {
    const harness = buildField(DARK);
    harness.enterNode('a');
    harness.focus.t = 0.5;
    const resting = restingNodeTint(DARK.substrate, KIND_RGB, 0.6, DARK.light);
    expect(paintedColor(harness, 'c')).toBe(rgb(lerpRgbTuple(resting, DARK.dim, 0.5)));
  });

  it('lerps a struck body toward the accent and swells it with its heat', () => {
    const harness = buildField(DARK);
    harness.field.strike(['b'], 0.8);
    const resting = restingNodeTint(DARK.substrate, KIND_RGB, 0.6, DARK.light);
    const reduced = harness.reducer('b', harness.attrs('b'));
    expect(reduced['color']).toBe(rgb(lerpRgbTuple(resting, DARK.hot, 0.8)));
    expect(reduced['size']).toBeCloseTo(5 * (1 + 0.5 * 0.8));
  });

  it('holds the selected body at the accent above the field', () => {
    const harness = buildField(DARK);
    harness.setSelected('c');
    const reduced = harness.reducer('c', harness.attrs('c'));
    expect(reduced['color']).toBe(rgb(DARK.hot));
    expect(reduced['zIndex']).toBe(3);
  });

  it('wires the theme hover drawer in place of sigma default white disc', () => {
    const harness = buildField(DARK);
    const strokes: string[] = [];
    const context = {
      strokeStyle: '',
      fillStyle: '',
      lineWidth: 0,
      font: '',
      beginPath: () => undefined,
      arc: () => undefined,
      stroke() {
        strokes.push(String(this.strokeStyle));
      },
      measureText: () => ({ width: 30 }),
      fillRect: () => undefined,
      fillText: () => undefined,
    };
    harness.hoverDrawer(
      context as unknown as CanvasRenderingContext2D,
      { x: 0, y: 0, size: 5, label: 'symbol a', color: rgb(DARK.hot) },
      { labelSize: 11, labelWeight: 'normal', labelFont: 'monospace' } as Settings,
    );
    expect(strokes).toEqual([rgba(DARK.hot, 0.9)]);
  });
});
