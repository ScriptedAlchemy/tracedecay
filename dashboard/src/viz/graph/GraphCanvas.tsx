import { useEffect, useRef, useState, type ReactNode } from 'react';
import Graph from 'graphology';
import forceAtlas2 from 'graphology-layout-forceatlas2';
import Sigma from 'sigma';
import {
  ActivationField,
  approach,
  cssColorToRgb,
  lerpRgb,
  lerpRgbTuple,
  restingNodeTint,
  settled,
} from './activation.ts';
import { kindColor } from './kindColor.ts';
import { cn } from '../../ui/cn';

export interface GraphCanvasNode {
  id: string;
  label: string;
  kind: string;
  degree: number;
  /**
   * Real, caller-supplied liveness in 0..1 — recency for projects, freshness
   * for stores, whatever this graph's genuine decay signal is. It sets the
   * node's RESTING luminance: a live node burns at full hue, a dormant one
   * sinks back toward the substrate. Never a decoration; omit it when the
   * caller has no such measurement and every node rests at the same
   * (deliberately unremarkable) brightness.
   */
  vitality?: number;
  /**
   * Caller-supplied layout position, in the caller's own coordinate space.
   * Supply it only when the position MEASURES something — an axis the caption
   * names — because a fixed coordinate is read as meaning far more strongly
   * than a force-layout one. When every node carries `x`/`y` the canvas skips
   * ForceAtlas2 and the constellation re-centering entirely and draws the
   * composition it was handed; when any node lacks them the whole graph falls
   * back to the force layout, because a half-placed field would silently mix
   * measured positions with emergent ones.
   */
  x?: number;
  y?: number;
}

export interface GraphCanvasEdge {
  source: string;
  target: string;
  kind?: string;
}

/** Samples the resolved theme tokens Sigma needs; canvas renderers cannot
 * consume CSS variables directly, so we re-sample on every theme flip. */
function palette(element: HTMLElement) {
  const style = getComputedStyle(element);
  const token = (name: string, fallback: string): [number, number, number] => {
    // Sigma's WebGL programs parse colors themselves and accept only
    // hex / rgb() / named forms. Our oklch tokens resolve to `lab(...)`,
    // which they cannot parse — every node and edge silently became black.
    // Normalize through the canvas parser (which does understand lab/oklch)
    // so the renderer always receives a form it can read.
    return cssColorToRgb(style.getPropertyValue(name).trim() || fallback);
  };
  const substrate = token('--raw-surface-1', '#1c2029');
  // Which medium the field is suspended in, measured rather than assumed, so
  // a future theme that is neither of the two shipped ones still resolves.
  const light = (substrate[0] * 299 + substrate[1] * 587 + substrate[2] * 114) / 1000 > 128;
  return {
    hot: token('--raw-accent', '#7aa2f7'),
    edge: token('--raw-edge-subtle', '#333a46'),
    label: token('--raw-text-secondary', '#aab0bd'),
    /** What a node fades INTO as its signal decays: the substrate itself. */
    substrate,
    dim: token('--raw-surface-3', '#3a4150'),
    light,
    /** A dark medium has almost unlimited headroom above it before a glow
     * clips to white; paper has very little, and additive-looking overlap goes
     * muddy long before it goes bright. Resting glow is damped on the light
     * field so a dense cluster stays a cluster instead of a smudge. */
    glowScale: light ? 0.55 : 1,
  };
}

function rgb([r, g, b]: [number, number, number]): string {
  return `rgb(${r}, ${g}, ${b})`;
}

function rgba([r, g, b]: [number, number, number], alpha: number): string {
  return `rgba(${r}, ${g}, ${b}, ${Math.max(0, Math.min(1, alpha)).toFixed(3)})`;
}

/** Managed companion prefixes. These are renderer-owned nodes and edges that
 * carry glow, dendrite geometry and travelling light; reducers pass them
 * through untouched and every topology query filters them out. */
const HALO = '__halo__';
const BLOOM = '__bloom__';
const RING = '__ring__';
const PULSE = '__pulse__';
const WAY = '__way__';

function isManaged(id: string): boolean {
  return (
    id.startsWith(HALO) ||
    id.startsWith(BLOOM) ||
    id.startsWith(RING) ||
    id.startsWith(PULSE) ||
    id.startsWith(WAY)
  );
}

/** One logical relation, rendered as a dendrite: a chain of short segments
 * tracing a quadratic curve between two real nodes. Keeping the polyline lets
 * travelling activation run along the curve rather than cutting the chord. */
interface Strand {
  from: string;
  to: string;
  points: Array<[number, number]>;
}

/** Sigma over Graphology (plan 11a: default connected-graph renderer).
 *
 * Deterministic ForceAtlas2 settle (laid out once, never animated), nodes
 * sized by degree and lit by their real vitality, relations drawn as curved
 * connective tissue rather than chords. Everything that moves is a response to
 * a real event: an activation strike from the live stream, a search that hit,
 * or the pointer. At rest the field is completely still and the render loop is
 * asleep. The synchronized list next to the canvas remains the accessible
 * surface. */
export function GraphCanvas({
  nodes,
  edges,
  selectedId,
  onSelect,
  height = 320,
  fill = false,
  activation,
  canvasClassName,
  caption,
  ariaLabel,
  extent,
}: {
  nodes: GraphCanvasNode[];
  edges: GraphCanvasEdge[];
  selectedId?: string | null;
  onSelect?: (id: string | null) => void;
  height?: number;
  /** Occupy the parent's full height instead of a fixed one. The parent must
   * establish the height (e.g. `flex-1 min-h-0`). */
  fill?: boolean;
  /** External synapse field; when omitted the canvas owns a local one fed by
   * selection strikes. */
  activation?: ActivationField;
  /** Extra classes merged onto the canvas element itself (not the figure) --
   * for a caller that needs to guarantee a minimum rendered height on a
   * breakpoint where its own flex ancestors would otherwise squeeze a `fill`
   * canvas toward zero. */
  canvasClassName?: string;
  /** What this particular field means. The default sentence describes a
   * force-laid symbol graph; any caller composing a different field MUST
   * replace it, because the caption is the only place the reader is told what
   * position, size and brightness encode — leaving the default on a measured
   * layout would state something untrue about the picture. */
  caption?: ReactNode;
  /** Accessible description of the canvas, for the same reason. */
  ariaLabel?: string;
  /** The frame a measured field is drawn in, in the caller's own coordinates.
   * Only meaningful alongside placed nodes. Without it the camera frames the
   * bodies that happen to exist, so a field with an empty region — no dormant
   * projects, say — silently loses that region and the reader is never shown
   * the absence. With it, an empty part of the axis stays empty on screen,
   * which is the finding. */
  extent?: { x: [number, number]; y: [number, number] };
}) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const sigmaRef = useRef<Sigma | null>(null);
  const [, setRetryTick] = useState(0);
  const webglRef = useRef<boolean | null>(null);
  if (webglRef.current === null) webglRef.current = hasWebGl();
  const fieldRef = useRef<ActivationField | null>(null);
  if (activation) fieldRef.current = activation;
  else if (!fieldRef.current) fieldRef.current = new ActivationField();
  // Selection and the select handler are read through refs rather than closed
  // over, so they can change without re-running the mount effect. They used to
  // sit in its dependency list, and `onSelect` is an inline arrow at every call
  // site: every parent render — including one per live SSE pulse — tore the
  // renderer down and re-ran a 200-iteration ForceAtlas2 layout. That both
  // burned a layout per event and hid the sleeping render loop behind a
  // remount. The effect now depends on topology alone.
  const selectedIdRef = useRef<string | null | undefined>(selectedId);
  selectedIdRef.current = selectedId;
  const onSelectRef = useRef<((id: string | null) => void) | undefined>(onSelect);
  onSelectRef.current = onSelect;

  // Selection is a static repaint, not an animation: recolour once and leave
  // the loop asleep.
  useEffect(() => {
    sigmaRef.current?.refresh();
  }, [selectedId]);

  useEffect(() => {
    const container = containerRef.current;
    if (!container || nodes.length === 0 || !webglRef.current) return;
    // Mount race: Sigma throws on zero-width containers (narrow layouts,
    // pre-layout flex). Defer one frame until the container has size.
    if (container.clientWidth === 0 || container.clientHeight === 0) {
      const retry = requestAnimationFrame(() => setRetryTick((tick) => tick + 1));
      return () => cancelAnimationFrame(retry);
    }

    const graph = new Graph({ multi: true, type: 'directed' });
    const maxDegree = Math.max(...nodes.map((n) => n.degree), 1);
    // A field is "placed" only if EVERY node was measured into it. One node
    // without coordinates would otherwise be dropped at the seed circle beside
    // nodes whose position is a claim about their data, and the reader has no
    // way to tell the two apart.
    const placed = nodes.every(
      (node) => Number.isFinite(node.x) && Number.isFinite(node.y),
    );
    // Deterministic circular seed (sorted order) so layouts are stable
    // across reloads of the same subgraph.
    const sorted = [...nodes].sort((a, b) => a.id.localeCompare(b.id));
    const seedLight = palette(container).light;
    // Sigma fits the camera to the graph's extent, so an identical base radius
    // renders far larger on a sparse graph than on a dense one: forty symbols
    // read correctly on Code while seven filled the Brain canvas with discs.
    // Normalising the radius against node count keeps a node the same apparent
    // object however much of the graph is on screen -- degree still sets the
    // relative sizes within a graph, which is the only comparison that means
    // anything. The dense case is left exactly where it was.
    const density = Math.min(1, Math.max(0.45, Math.sqrt(nodes.length / 40)));
    // The second half of the same problem: node radius is in screen pixels but
    // the SPACING between nodes is not, so the identical graph that reads
    // correctly in a 1060px canvas becomes a solid mass of overlapping discs in
    // a 270px one. Measuring the canvas area each node actually gets lets a
    // narrow viewport shrink the bodies instead of fusing them. It only ever
    // reduces -- a roomy canvas is already correct.
    const perNode =
      (container.clientWidth * container.clientHeight) / Math.max(nodes.length, 1);
    const roominess = Math.min(1, Math.sqrt(perNode / 8000));
    // A dense connected component (many edges per node) settles into a
    // tighter FA2 packing than a sparse one of the same node count even
    // after the repulsion tuning below, so its bodies need to shrink
    // further or they fuse into an undifferentiated mass regardless of how
    // roomy the canvas is. Average degree is a cheap, whole-graph proxy for
    // that packing pressure -- exact per-component density is not worth the
    // extra pass here.
    const edgeDensity = nodes.length > 0 ? (2 * edges.length) / nodes.length : 0;
    const densityShrink = Math.min(1, 1.8 / (1 + edgeDensity * 0.55));
    const bodyScale = Math.max(0.32, density * roominess * densityShrink);
    sorted.forEach((node, index) => {
      const angle = (index / sorted.length) * Math.PI * 2;
      const [kr, kg, kb] = cssColorToRgb(kindColor(node.kind, seedLight));
      graph.addNode(node.id, {
        label: node.label,
        kind: node.kind,
        degree: node.degree,
        x: placed ? node.x! : Math.cos(angle),
        y: placed ? node.y! : Math.sin(angle),
        size: (5 + 9 * Math.sqrt(node.degree / maxDegree)) * bodyScale,
        isHub: node.degree >= maxDegree * 0.75,
        // A graph with no vitality measurement rests mid-scale, so an absent
        // signal never masquerades as a dead network.
        vitality:
          node.vitality == null ? 0.6 : Math.max(0, Math.min(1, node.vitality)),
        kindRgb: [kr, kg, kb] as [number, number, number],
      });
    });
    for (const edge of edges) {
      if (graph.hasNode(edge.source) && graph.hasNode(edge.target)) {
        graph.addEdge(edge.source, edge.target, { kind: edge.kind });
      }
    }
    // Emergent layout, for graphs whose shape is the finding. A measured field
    // skips all of it: running a force pass over placed coordinates, or
    // re-centering their components onto a ring, would destroy the very
    // measurement the positions were carrying.
    if (!placed) {
    const fa2 = forceAtlas2.inferSettings(graph);
    forceAtlas2.assign(graph, {
      // A dense component needs more settling time to actually reach the
      // wider spread the settings below ask for; capped so a huge sparse
      // graph doesn't pay for iterations it does not need.
      iterations: Math.round(Math.min(400, 200 + edgeDensity * 40)),
      settings: {
        ...fa2,
        // Small graphs over-spread with inferred gravity; pull clusters in so
        // the tissue reads dense, not lost in the void. A DENSE graph needs
        // the opposite correction on top of that: less pull-to-centre and
        // more repulsion, or its own edges collapse it into a fused hairball
        // no matter how few nodes it has -- node count alone (the previous
        // basis for both knobs) says nothing about how much mutual pull a
        // component's edges apply.
        gravity:
          ((fa2.gravity ?? 1) *
            (nodes.length < 60 ? Math.max(8, 200 / nodes.length) : 2)) /
          (1 + edgeDensity * 0.5),
        scalingRatio: 4 + edgeDensity * 3,
      },
    });
    // Constellation composure: FA2 lets disconnected components drift apart
    // under nothing but mutual repulsion and per-node gravity, which pulls
    // each node toward the origin independently of which component it is
    // in -- it does not pull components toward EACH OTHER. On a small graph
    // (a handful of repos, each an isolated hub-and-spokes component) that
    // reliably produces two or three tight clumps separated by a gap several
    // times their own size: exactly the "vast dead field" the camera then
    // faithfully frames, because there is nothing wrong with the frame, only
    // with how far apart the content drifted before it was fit. A single
    // connected component is a no-op here. The frozen bbox below is computed
    // after this pass, so the camera always frames the composed result, not
    // the raw FA2 scatter.
    {
      const componentOf = new Map<string, number>();
      const membersOf: string[][] = [];
      for (const start of graph.nodes()) {
        if (componentOf.has(start)) continue;
        const index = membersOf.length;
        const members: string[] = [];
        membersOf.push(members);
        const queue = [start];
        componentOf.set(start, index);
        while (queue.length) {
          const current = queue.pop()!;
          members.push(current);
          for (const neighbor of graph.neighbors(current)) {
            if (!componentOf.has(neighbor)) {
              componentOf.set(neighbor, index);
              queue.push(neighbor);
            }
          }
        }
      }
      const componentCount = membersOf.length;
      if (componentCount > 1) {
        const centroids = membersOf.map((members) => {
          let x = 0, y = 0;
          for (const node of members) {
            x += graph.getNodeAttribute(node, 'x') as number;
            y += graph.getNodeAttribute(node, 'y') as number;
          }
          return { x: x / members.length, y: y / members.length };
        });
        const extents = membersOf.map((members, index) => {
          const c = centroids[index]!;
          let extent = 0.15; // floor: an orphan has zero spread of its own.
          for (const node of members) {
            const ex = Math.abs((graph.getNodeAttribute(node, 'x') as number) - c.x);
            const ey = Math.abs((graph.getNodeAttribute(node, 'y') as number) - c.y);
            extent = Math.max(extent, ex, ey);
          }
          return extent;
        });
        let dominant = 0;
        for (let index = 1; index < componentCount; index += 1) {
          if (membersOf[index]!.length > membersOf[dominant]!.length) dominant = index;
        }
        const totalRealNodes = membersOf.reduce((sum, m) => sum + m.length, 0);
        const secondLargest = Math.max(
          0,
          ...membersOf.filter((_, index) => index !== dominant).map((m) => m.length),
        );
        // Two genuinely different shapes share this code path. Brain-style
        // graphs are a constellation of many roughly EQUAL-sized components
        // (one hub-and-spokes cluster per repo) -- there is no "main" body,
        // so the original flat ring (every component equally spaced, sized
        // from the single largest extent) is exactly right and reads as a
        // deliberate necklace. Code-style graphs are the opposite: ONE
        // component holds most of the real nodes and everything else is a
        // stray orphan or two. Forcing the orphan case through the flat-ring
        // formula was the actual defect -- a near-zero-extent orphan was
        // pushed out to the SAME radius the dominant cluster needed, which
        // inflates the camera's fitted bbox far more than it helps legibility
        // ("orphans set the extent," shrinking the real structure to a
        // sliver of the canvas). Detect that shape (one component holding a
        // clear majority, with no other component anywhere close to its
        // size) and anchor everything else to it in a tight margin band
        // instead; otherwise fall back to the flat ring both shapes used to
        // share.
        const hasDominantComponent =
          membersOf[dominant]!.length >= totalRealNodes * 0.5 &&
          membersOf[dominant]!.length >= secondLargest * 3;
        if (hasDominantComponent) {
          const dominantExtent = extents[dominant]!;
          const domCentroid = centroids[dominant]!;
          const others = Array.from({ length: componentCount }, (_, index) => index).filter(
            (index) => index !== dominant,
          );
          others.forEach((component, spokeIndex) => {
            const c = centroids[component]!;
            const spoke = Math.min(
              dominantExtent + extents[component]! * 1.2 + 0.3,
              dominantExtent * 2,
            );
            const angle = (spokeIndex / others.length) * Math.PI * 2;
            const dx = -domCentroid.x + Math.cos(angle) * spoke - c.x;
            const dy = -domCentroid.y + Math.sin(angle) * spoke - c.y;
            for (const node of membersOf[component]!) {
              graph.updateNodeAttribute(node, 'x', (x) => (x as number) + dx);
              graph.updateNodeAttribute(node, 'y', (y) => (y as number) + dy);
            }
          });
          // Re-centre the dominant component itself onto the origin, so the
          // spokes above (measured from its pre-move centroid) and the frame
          // both anchor to where the graph's real mass actually is.
          for (const node of membersOf[dominant]!) {
            graph.updateNodeAttribute(node, 'x', (x) => (x as number) - domCentroid.x);
            graph.updateNodeAttribute(node, 'y', (y) => (y as number) - domCentroid.y);
          }
        } else {
          // The ring only needs to be as large as geometry actually
          // requires: adjacent components sit `2π/N` apart in angle, so the
          // chord between two neighbouring centroids is `2·ring·sin(π/N)`,
          // and that chord must clear twice each component's own extent for
          // their (roughly circular) footprints not to overlap. Solving for
          // ring gives exactly the radius non-overlap needs.
          const maxExtent = Math.max(...extents);
          const ring = (maxExtent / Math.sin(Math.PI / componentCount)) * 1.15;
          membersOf.forEach((members, component) => {
            const c = centroids[component]!;
            const angle = (component / componentCount) * Math.PI * 2;
            const dx = Math.cos(angle) * ring - c.x;
            const dy = Math.sin(angle) * ring - c.y;
            for (const node of members) {
              graph.updateNodeAttribute(node, 'x', (x) => (x as number) + dx);
              graph.updateNodeAttribute(node, 'y', (y) => (y as number) + dy);
            }
          });
        }
      }
    }
    }

    // Real topology, captured before the dendrite pass rewrites the edge set.
    // Strike propagation and hover isolation answer from this map, so the
    // rendering geometry can never be mistaken for the graph's structure.
    const realNodes = graph.nodes();
    const neighborsOf = new Map<string, string[]>();
    for (const node of realNodes) neighborsOf.set(node, graph.neighbors(node));

    // ---- dendrite pass -------------------------------------------------
    // A relation is connective tissue, not a ruled line. Each edge becomes a
    // quadratic curve sampled into short segments; the bow direction is hashed
    // from the endpoint pair so parallel relations separate instead of
    // overprinting, and re-renders of the same subgraph curve identically.
    // Above the segment budget the graph keeps straight edges: legibility at
    // density beats flourish.
    const segments = edges.length <= 120 ? 7 : edges.length <= 400 ? 4 : 0;
    const strands: Strand[] = [];
    if (segments > 1) {
      for (const edge of [...graph.edges()]) {
        const [from, to] = graph.extremities(edge);
        if (!from || !to) continue;
        const a = graph.getNodeAttributes(from);
        const b = graph.getNodeAttributes(to);
        const ax = a['x'] as number;
        const ay = a['y'] as number;
        const bx = b['x'] as number;
        const by = b['y'] as number;
        let hash = 0;
        for (const character of `${from}\u0000${to}`) {
          hash = (hash * 31 + character.charCodeAt(0)) >>> 0;
        }
        const bow = (hash % 2 === 0 ? 1 : -1) * (0.1 + ((hash >>> 3) % 7) * 0.014);
        // Perpendicular offset of the Bézier control point, in layout units.
        const cx = (ax + bx) / 2 - (by - ay) * bow;
        const cy = (ay + by) / 2 + (bx - ax) * bow;
        const points: Array<[number, number]> = [];
        for (let step = 0; step <= segments; step += 1) {
          const t = step / segments;
          const u = 1 - t;
          points.push([
            u * u * ax + 2 * u * t * cx + t * t * bx,
            u * u * ay + 2 * u * t * cy + t * t * by,
          ]);
        }
        const strandIndex = strands.length;
        strands.push({ from, to, points });
        graph.dropEdge(edge);
        let previous = from;
        for (let step = 1; step <= segments; step += 1) {
          const isLast = step === segments;
          const point = points[step]!;
          let node = to;
          if (!isLast) {
            node = `${WAY}${strandIndex}:${step}`;
            graph.addNode(node, {
              x: point[0],
              y: point[1],
              // Geometry only: a waypoint is a joint, never a visible body.
              size: 0.01,
              color: 'rgba(0, 0, 0, 0)',
              label: '',
              zIndex: 0,
            });
          }
          graph.addEdge(previous, node, { srcReal: from, dstReal: to });
          previous = node;
        }
      }
    } else {
      graph.forEachEdge((edge, _attrs, from, to) => {
        graph.setEdgeAttribute(edge, 'srcReal', from);
        graph.setEdgeAttribute(edge, 'dstReal', to);
      });
    }

    let colors = palette(container);
    /** How isolated the hovered neighbourhood currently is, 0..1. Eased rather
     * than switched so focus propagates outward instead of blinking. */
    let hoverT = 0;
    let hoverTarget = 0;
    let hovered: string | null = null;
    let lastFrame = 0;
    const field = fieldRef.current ?? new ActivationField();
    const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

    const renderer = new Sigma(graph, container, {
      renderLabels: true,
      labelRenderedSizeThreshold: nodes.length <= 60 ? 5 : 9,
      labelFont: 'ui-monospace, monospace',
      labelSize: 11,
      labelColor: { color: rgb(colors.label) },
      defaultEdgeColor: rgba(colors.edge, 0.9),
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
        const isSelected = node === selectedIdRef.current;
        const isHovered = node === hovered;
        const isNeighbor =
          hovered != null && (neighborsOf.get(node)?.includes(hovered) === true || isHovered);
        const dim = hovered != null && !isNeighbor ? hoverT : 0;
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
            isSelected || isHovered || heat > 0.5 || data['isHub'] || nodes.length <= 60
              ? data['label']
              : '',
        };
      },
      edgeReducer: (edge, data) => {
        const from = (data['srcReal'] as string | undefined) ?? '';
        const to = (data['dstReal'] as string | undefined) ?? '';
        const dim =
          hovered != null && from !== hovered && to !== hovered ? hoverT : 0;
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
            ? rgba(lerpRgbTuple(colors.edge, colors.hot, Math.min(1, edgeHeat)), Math.min(1, alpha + 0.4 * edgeHeat))
            : rgba(colors.edge, alpha);
        return { ...data, color, size: edgeHeat > 0.05 ? 1 + 2 * edgeHeat : data['size'] };
      },
    });
    sigmaRef.current = renderer;
    if (placed && extent) {
      // A measured field is framed by its AXIS, not by its occupants. Framing
      // the occupants would rescale the picture every time a body enters or
      // leaves a region, and would quietly delete an empty region — which on
      // this kind of field is itself a reading.
      renderer.setCustomBBox({ x: extent.x, y: extent.y });
    } else {
      // Frame the real network only: dendrite waypoints bow outside the node
      // hull and glow companions scale with heat, and neither may be allowed
      // to move the camera.
      let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
      for (const node of realNodes) {
        const x = graph.getNodeAttribute(node, 'x') as number;
        const y = graph.getNodeAttribute(node, 'y') as number;
        if (x < minX) minX = x;
        if (x > maxX) maxX = x;
        if (y < minY) minY = y;
        if (y > maxY) maxY = y;
      }
      // ~15% margin so the constellation fills the viewport instead of
      // floating in an over-large frame, with enough slack left over that a
      // label overhanging its node (rendered outside the node's own radius)
      // never clips at the canvas edge.
      const padX = (maxX - minX || 1) * 0.15;
      const padY = (maxY - minY || 1) * 0.15;
      renderer.setCustomBBox({
        x: [minX - padX, maxX + padX],
        y: [minY - padY, maxY + padY],
      });
    }

    renderer.on('enterNode', ({ node }) => {
      if (isManaged(node)) return;
      hovered = node;
      hoverTarget = 1;
      wake();
    });
    renderer.on('leaveNode', () => {
      hoverTarget = 0;
      wake();
    });
    renderer.on('clickNode', ({ node }) => {
      if (isManaged(node)) return;
      onSelectRef.current?.(node);
      // Traveling activation: the struck node fires now; its neighborhood
      // fires one synaptic delay later (real caller/reference edges only).
      field.strike([node], 1);
      const neighbors = neighborsOf.get(node) ?? [];
      if (reducedMotion) field.strike(neighbors, 0.55);
      else setTimeout(() => { field.strike(neighbors, 0.55); wake(); }, 140);
      wake();
    });
    renderer.on('clickStage', () => onSelectRef.current?.(null));

    // ---- glow companions ------------------------------------------------
    // Every point is a body with falloff, not a flat disc: a tight corona in
    // the node's own hue plus a wide, very faint bloom give depth without a
    // shader. Both ride real signal — vitality at rest, heat when struck — so
    // a quiet graph is genuinely quieter, not merely smaller.
    // Companions are gated on the count of REAL nodes; the dendrite pass adds
    // waypoints that must not push a small graph over the budget.
    const restingGlow = realNodes.length <= 400;
    const syncGlow = () => {
      for (const node of realNodes) {
        const heat = field.heatOf(node);
        const haloId = HALO + node;
        const bloomId = BLOOM + node;
        const ringId = RING + node;
        if (heat > 0.1 || restingGlow) {
          const attrs = graph.getNodeAttributes(node);
          const size = attrs['size'] as number;
          const vitality = (attrs['vitality'] as number | undefined) ?? 0.6;
          const [kr, kg, kb] = (attrs['kindRgb'] as [number, number, number] | undefined) ??
            colors.hot;
          const lit = heat > 0
            ? lerpRgbTuple([kr, kg, kb], colors.hot, Math.min(1, heat))
            : ([kr, kg, kb] as [number, number, number]);
          const shared = { x: attrs['x'], y: attrs['y'], label: '' };
          // Sigma draws each companion as a hard-edged disc, so a corona is
          // really three concentric steps and every step is a visible edge.
          // The old radii (1.55x and 2.9x the body) made those edges read as
          // banding rather than falloff, and turned a modest graph into a field
          // of lollipops. Pulled in tight, the resting glow is a rim on the
          // body instead of a second object beside it -- and a strike still
          // has all the room it needs to swell.
          const glow = colors.glowScale;
          // A hairline for the ember: on paper a fully-dormant body's own
          // fill is deliberately close to the substrate (see
          // `restingNodeTint`), so without help its halo -- which scales
          // with vitality -- would fade to nothing at exactly the moment the
          // body needs it most. The floor only ever raises the light-theme
          // halo of a low-vitality node; a live node's own (already larger)
          // vitality term dominates, and the dark theme's halo already has
          // plenty of headroom, so this is a no-op there.
          const dormantRingFloor = colors.light ? 0.16 * (1 - vitality) : 0;
          upsert(graph, haloId, {
            ...shared,
            size: size * (1.38 + 1.0 * heat),
            color: rgba(lit, Math.max((0.05 + 0.1 * vitality) * glow, dormantRingFloor) + 0.26 * heat),
            zIndex: 1,
          });
          upsert(graph, bloomId, {
            ...shared,
            size: size * (2.2 + 2.0 * heat),
            color: rgba(lit, (0.018 + 0.034 * vitality) * glow + 0.1 * heat),
            zIndex: 0,
          });
          // Impact flare: a wide, faint ring pops on strike and expands as the
          // bloom settles, so a firing is legible even in peripheral vision.
          if (heat > 0.5) {
            upsert(graph, ringId, {
              ...shared,
              size: size * (2.6 + 3.4 * (1 - heat)),
              color: rgba(colors.hot, 0.1 * heat),
              zIndex: 1,
            });
          } else if (graph.hasNode(ringId)) {
            graph.dropNode(ringId);
          }
        } else {
          for (const id of [haloId, bloomId, ringId]) {
            if (graph.hasNode(id)) graph.dropNode(id);
          }
        }
      }
    };

    // ---- travelling light ------------------------------------------------
    // While a relation is warm, one bright point runs the dendrite from the
    // hotter end to the cooler one, following the curve rather than the chord.
    // Pulses exist only while the field is warm, and the frozen bbox keeps
    // them from ever rescaling the camera.
    const syncPulses = (now: number) => {
      const period = 1100;
      const phase = (now % period) / period;
      for (let index = 0; index < strands.length; index += 1) {
        const strand = strands[index]!;
        const heatFrom = field.heatOf(strand.from);
        const heatTo = field.heatOf(strand.to);
        const travel = Math.max(heatFrom, heatTo);
        const pulseId = `${PULSE}${index}`;
        if (travel > 0.18 && !reducedMotion) {
          const forward = heatFrom >= heatTo;
          const points = strand.points;
          const spans = points.length - 1;
          const walked = (forward ? phase : 1 - phase) * spans;
          const span = Math.max(0, Math.min(spans - 1, Math.floor(walked)));
          const local = walked - span;
          const a = points[span]!;
          const b = points[span + 1]!;
          upsert(graph, pulseId, {
            x: a[0] + (b[0] - a[0]) * local,
            y: a[1] + (b[1] - a[1]) * local,
            size: 1.4 + 2 * travel,
            color: rgba(colors.hot, 0.9 * travel),
            label: '',
            zIndex: 4,
          });
        } else if (graph.hasNode(pulseId)) {
          graph.dropNode(pulseId);
        }
      }
    };

    // Render loop: runs only while there is something real to resolve — a warm
    // activation field, or a hover isolation still easing into place. It stops
    // itself the moment both settle, so an idle dashboard costs nothing.
    // Reduced motion never starts it: state is applied in one static refresh.
    let raf = 0;
    const step = (now: number) => {
      const delta = lastFrame === 0 ? 16 : now - lastFrame;
      lastFrame = now;
      const warm = field.tick(now);
      hoverT = approach(hoverT, hoverTarget, delta, 90);
      const focusSettled = settled(hoverT, hoverTarget);
      if (focusSettled) {
        hoverT = hoverTarget;
        if (hoverTarget === 0) hovered = null;
      }
      syncGlow();
      syncPulses(now);
      if (!warm) {
        for (const node of [...graph.nodes()]) {
          if (node.startsWith(PULSE)) graph.dropNode(node);
        }
      }
      renderer.refresh();
      const keepGoing = warm || !focusSettled;
      raf = keepGoing && !reducedMotion ? requestAnimationFrame(step) : 0;
      if (!keepGoing) lastFrame = 0;
    };
    const wake = () => {
      if (reducedMotion) {
        field.tick(performance.now());
        hoverT = hoverTarget;
        if (hoverTarget === 0) hovered = null;
        syncGlow();
        renderer.refresh();
        return;
      }
      if (!raf) {
        lastFrame = 0;
        raf = requestAnimationFrame(step);
      }
    };
    // A caller-owned field is struck from entirely outside this closure: the
    // Brain's SSE effect calls `field.strike(...)` when a real event lands,
    // with no knowledge of this render loop. If the loop is asleep (which,
    // correctly, it is whenever the field is cold) that heat would sit
    // undrawn and undecayed forever. Subscribing turns every real strike,
    // wherever it originates, into exactly one wake — and nothing else can
    // produce one, because the field has no clock.
    const unsubscribeField = field.subscribe(wake);
    // One static composition of the resting field, so the graph is fully
    // rendered before anything ever fires.
    syncGlow();
    renderer.refresh();
    if (field.warm) wake();

    const themeObserver = new MutationObserver(() => {
      const wasLight = colors.light;
      colors = palette(container);
      renderer.setSetting('defaultEdgeColor', rgba(colors.edge, 0.9));
      renderer.setSetting('labelColor', { color: rgb(colors.label) });
      // kindRgb is baked once at construction, so a theme flip used to leave
      // every node wearing the other theme's lightness — the hues only looked
      // right on whichever theme happened to be active at mount. Re-derive
      // them when, and only when, the medium actually changed sides.
      if (colors.light !== wasLight) {
        for (const node of graph.nodes()) {
          if (isManaged(node)) continue;
          const kind = graph.getNodeAttribute(node, 'kind') as string | undefined;
          if (kind == null) continue;
          graph.setNodeAttribute(
            node,
            'kindRgb',
            cssColorToRgb(kindColor(kind, colors.light)),
          );
        }
      }
      syncGlow();
      renderer.refresh();
    });
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme'],
    });

    return () => {
      if (raf) cancelAnimationFrame(raf);
      unsubscribeField();
      themeObserver.disconnect();
      renderer.kill();
      sigmaRef.current = null;
    };
  }, [nodes, edges, extent]);

  if (nodes.length === 0) {
    return (
      <p className="p-6 text-center text-sm text-text-muted">
        no graph neighborhood to draw
      </p>
    );
  }
  // Sigma is WebGL-only and throws during construction without a context,
  // which React Router's error boundary turns into a dead workspace. Browsers
  // with WebGL disabled or blocklisted get the truthful state instead — the
  // symbol list beside the canvas remains the accessible equivalent.
  if (!webglRef.current) {
    return (
      <p className="p-6 text-center text-sm text-text-muted">
        this browser has no WebGL context, so the {nodes.length.toLocaleString()}-symbol
        graph canvas cannot draw — the symbol list carries the same relations
      </p>
    );
  }
  // Scale tier guard (plan 11a graph tiers): this Sigma canvas owns graphs up
  // to ~5k nodes. Larger brains (the profile holds stores up to 1.6M nodes)
  // belong to the GPU tier — render the truthful tier state, never a frozen
  // tab pretending to cope.
  if (nodes.length > 5_000) {
    return (
      <p className="p-6 text-center text-sm text-text-muted">
        {nodes.length.toLocaleString()} symbols exceeds this renderer's tier —
        the GPU canvas (cosmos.gl adapter) owns brains this large; narrow the
        neighborhood to explore here
      </p>
    );
  }
  return (
    <figure className={cn('flex flex-col gap-1.5', fill && 'h-full min-h-0')}>
      <div
        ref={containerRef}
        style={fill ? undefined : { height }}
        className={cn(
          'relative overflow-hidden rounded-[var(--radius-card)] border border-edge-subtle/60',
          // The bezel screen ruling belongs to the chassis; the nebula field
          // behind it belongs to the network. Together the canvas reads as a
          // lit instrument screen rather than a picture pasted onto a panel.
          'td-scanlines [background:var(--raw-graph-field)]',
          'shadow-[inset_0_1px_0_0_var(--raw-membrane-lift),0_18px_44px_-28px_var(--raw-depth)]',
          fill && 'min-h-0 flex-1',
          canvasClassName,
        )}
        role="img"
        aria-label={
          ariaLabel ??
          `Code graph: ${nodes.length} symbols, ${edges.length} relations. The symbol list alongside is the accessible equivalent.`
        }
      />
      <figcaption className="text-2xs text-text-muted">
        {caption ?? (
          <>
            {nodes.length} symbols · {edges.length} relations · size =
            connectedness · brightness = how live the signal is · hover isolates
            a neighbourhood, click fires it and the bloom decays as the
            activation fades
          </>
        )}
      </figcaption>
    </figure>
  );
}

/** Add or update a managed companion node in one call. */
function upsert(graph: Graph, id: string, attributes: Record<string, unknown>): void {
  if (graph.hasNode(id)) graph.mergeNodeAttributes(id, attributes);
  else graph.addNode(id, attributes);
}

/** Whether this browser can give Sigma a WebGL context at all. Probed once per
 * canvas mount against a throwaway element: a blocklisted or disabled GPU
 * stack returns null here rather than throwing inside the renderer. */
function hasWebGl(): boolean {
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
