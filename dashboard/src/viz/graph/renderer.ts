import type Graph from 'graphology';
import Sigma from 'sigma';
import {
  cssColorToRgb,
  lerpRgb,
  lerpRgbTuple,
  restingNodeTint,
  type ActivationField,
} from './activation.ts';
import { kindColor } from './kindColor.ts';
import { isManaged } from './managed.ts';
import { palette, rgb, rgba, type ThemeBox } from './palette.ts';
import type { FieldFrame } from './layout.ts';

/**
 * Sigma instance lifecycle: construct the renderer over a prepared graph, wire
 * its pointer events, absorb a resize, re-sample the theme, and kill it.
 *
 * Everything here is about drawing. What is drawn was already decided by the
 * layout pass; what MOVES is decided by the activation overlay, which drives
 * this renderer's repaints. The reducers below are the seam between the two:
 * they read the live heat and hover state on every frame the overlay paints.
 */

/**
 * How isolated the hovered neighbourhood currently is. `t` is eased rather
 * than switched so focus propagates outward instead of blinking; the overlay's
 * loop advances it toward `target`.
 */
export interface FocusState {
  node: string | null;
  t: number;
  target: number;
}

export function createFocusState(): FocusState {
  return { node: null, t: 0, target: 0 };
}

export interface FieldRendererOptions {
  graph: Graph;
  container: HTMLElement;
  theme: ThemeBox;
  focus: FocusState;
  field: ActivationField;
  neighborsOf: ReadonlyMap<string, string[]>;
  nodeCount: number;
  denseField: boolean;
  roominess: number;
  frame: FieldFrame;
  /** Read per call, never captured: selection changes without a rebuild. */
  selectedId: () => string | null | undefined;
  onNodeClick: (node: string) => void;
  onStageClick: () => void;
  /** The hover state just changed; the overlay has an easing to run. */
  onFocusChange: () => void;
}

export interface FieldRenderer {
  refresh(): void;
  resize(): void;
  /** Re-sample the theme tokens and re-derive anything baked from them. */
  retheme(): void;
  kill(): void;
}

export function createFieldRenderer({
  graph,
  container,
  theme,
  focus,
  field,
  neighborsOf,
  nodeCount,
  denseField,
  roominess,
  frame,
  selectedId,
  onNodeClick,
  onStageClick,
  onFocusChange,
}: FieldRendererOptions): FieldRenderer {
  const roomyDenseField = denseField && roominess >= 0.8;

  const sigma = new Sigma(graph, container, {
    renderLabels: true,
    labelRenderedSizeThreshold: roomyDenseField
      ? 0
      : denseField
        ? 6.5
        : nodeCount <= 60
          ? 4.5
          : 8,
    labelDensity: 1,
    labelGridCellSize: roomyDenseField ? 90 : 100,
    labelFont: theme.colors.labelFont,
    labelSize: roomyDenseField ? 12 : 11,
    labelColor: { color: rgb(theme.colors.label) },
    defaultEdgeColor: rgba(theme.colors.edge, 0.9),
    // Every reducer below hands back a `zIndex` (bloom=0, halo=1, body=2,
    // selected/hot=3, travelling pulse=4) on the assumption that draw order
    // honours it. Sigma does not, unless told to: this setting is off by
    // default, so without it every node paints in graph insertion order
    // regardless of its zIndex attribute. The glow companions are inserted
    // AFTER the real body (`syncGlow` runs once the body already exists),
    // so they were silently painting OVER it -- their own faint colour,
    // stacked as halo then bloom on top of an opaque body, was what read as
    // a plain white disc on the light theme's near-white field (worst on
    // the largest, highest-degree bodies, which carry the largest glow).
    // Enabling real z-ordering restores what the reducers already intended:
    // bloom, then halo, then the body on top, then anything hot.
    zIndex: true,
    nodeReducer: (node, data) => {
      if (isManaged(node)) return data;
      const colors = theme.colors;
      const hovered = focus.node;
      const isSelected = node === selectedId();
      const isHovered = node === hovered;
      const isNeighbor =
        hovered != null && (neighborsOf.get(node)?.includes(hovered) === true || isHovered);
      const dim = hovered != null && !isNeighbor ? focus.t : 0;
      const heat = field.heatOf(node);
      const vitality = (data['vitality'] as number | undefined) ?? 0.6;
      // Two axes, both real. Vitality is the slow one: a node's resting
      // luminance is how alive the caller measured it to be, so a dormant
      // corner of the graph literally recedes into the substrate while a
      // live one holds its hue. Heat is the fast one: a strike blooms the
      // node toward the accent and swells it, then decays on the field's
      // exponential half-life. Hover isolation is a third, transient mix
      // toward the neutral dim token so an isolated neighbourhood is
      // unambiguous.
      const [kr, kg, kb] = (data['kindRgb'] as [number, number, number] | undefined) ?? [
        149, 152, 157,
      ];
      let tint = restingNodeTint(colors.substrate, [kr, kg, kb], vitality, colors.light);
      if (dim > 0) tint = lerpRgbTuple(tint, colors.dim, dim);
      const color =
        isSelected || isHovered
          ? rgb(colors.hot)
          : heat > 0
            ? lerpRgb(tint, colors.hot, Math.min(1, heat))
            : rgb(tint);
      return {
        ...data,
        color,
        size: (data['size'] as number) * (1 + 0.5 * heat),
        zIndex: isSelected || isHovered || heat > 0.4 ? 3 : 2,
        label:
          isSelected || isHovered || heat > 0.5 || data['isHub'] || nodeCount <= 60
            ? data['label']
            : '',
      };
    },
    edgeReducer: (edge, data) => {
      const colors = theme.colors;
      const hovered = focus.node;
      const from = (data['srcReal'] as string | undefined) ?? '';
      const to = (data['dstReal'] as string | undefined) ?? '';
      const dim = hovered != null && from !== hovered && to !== hovered ? focus.t : 0;
      // A relation conducts only when both of its ends are warm — that is
      // what makes it a synapse rather than a wire. Vitality sets how
      // present the tissue is at rest.
      const edgeHeat = Math.min(field.heatOf(from), field.heatOf(to));
      const restVitality =
        (((graph.hasNode(from) ? (graph.getNodeAttribute(from, 'vitality') as number) : 0.6) ??
          0.6) +
          ((graph.hasNode(to) ? (graph.getNodeAttribute(to, 'vitality') as number) : 0.6) ??
            0.6)) /
        2;
      const alpha = (0.36 + 0.5 * restVitality) * (1 - 0.92 * dim);
      const color =
        edgeHeat > 0.05
          ? rgba(
              lerpRgbTuple(colors.edge, colors.hot, Math.min(1, edgeHeat)),
              Math.min(1, alpha + 0.4 * edgeHeat),
            )
          : rgba(colors.edge, alpha);
      return { ...data, color, size: edgeHeat > 0.05 ? 1 + 2 * edgeHeat : data['size'] };
    },
  });

  sigma.setCustomBBox({ x: frame.x, y: frame.y });

  sigma.on('enterNode', ({ node }) => {
    if (isManaged(node)) return;
    focus.node = node;
    focus.target = 1;
    onFocusChange();
  });
  sigma.on('leaveNode', () => {
    focus.target = 0;
    onFocusChange();
  });
  sigma.on('clickNode', ({ node }) => {
    if (isManaged(node)) return;
    onNodeClick(node);
  });
  sigma.on('clickStage', () => onStageClick());

  return {
    refresh: () => {
      sigma.refresh();
    },
    resize: () => {
      sigma.resize();
    },
    retheme: () => {
      const wasLight = theme.colors.light;
      theme.colors = palette(container);
      sigma.setSetting('defaultEdgeColor', rgba(theme.colors.edge, 0.9));
      sigma.setSetting('labelColor', { color: rgb(theme.colors.label) });
      // kindRgb is baked once at construction, so a theme flip used to leave
      // every node wearing the other theme's lightness — the hues only looked
      // right on whichever theme happened to be active at mount. Re-derive
      // them when, and only when, the medium actually changed sides.
      if (theme.colors.light !== wasLight) {
        for (const node of graph.nodes()) {
          if (isManaged(node)) continue;
          const kind = graph.getNodeAttribute(node, 'kind') as string | undefined;
          if (kind == null) continue;
          graph.setNodeAttribute(
            node,
            'kindRgb',
            cssColorToRgb(kindColor(kind, theme.colors.light)),
          );
        }
      }
    },
    kill: () => {
      sigma.kill();
    },
  };
}

/** Whether this browser can give Sigma a WebGL context at all. Probed once per
 * canvas mount against a throwaway element: a blocklisted or disabled GPU
 * stack returns null here rather than throwing inside the renderer. */
export function hasWebGl(): boolean {
  if (typeof document === 'undefined') return false;
  try {
    const probe = document.createElement('canvas');
    const context =
      probe.getContext('webgl2') ??
      probe.getContext('webgl') ??
      probe.getContext('experimental-webgl');
    return context !== null;
  } catch {
    return false;
  }
}
