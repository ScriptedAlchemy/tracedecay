import { useEffect, useRef } from 'react';
import Graph from 'graphology';
import forceAtlas2 from 'graphology-layout-forceatlas2';
import Sigma from 'sigma';
import { ActivationField, cssColorToRgb, lerpRgb } from './activation.ts';

export interface GraphCanvasNode {
  id: string;
  label: string;
  kind: string;
  degree: number;
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
  const token = (name: string, fallback: string) =>
    style.getPropertyValue(name).trim() || fallback;
  return {
    node: token('--raw-text-muted', '#8a90a0'),
    nodeSelected: token('--raw-accent', '#7aa2f7'),
    edge: token('--raw-edge-subtle', '#333a46'),
    label: token('--raw-text-secondary', '#aab0bd'),
    dim: token('--raw-surface-3', '#3a4150'),
  };
}

/** Sigma over Graphology (plan 11a: default connected-graph renderer).
 * Deterministic ForceAtlas2 settle (no animation loop — calm density,
 * reduced-motion safe), degree-sized nodes, hover dims non-neighbors,
 * click selects. The canvas is supplementary: the synchronized list next
 * to it remains the accessible surface. */
export function GraphCanvas({
  nodes,
  edges,
  selectedId,
  onSelect,
  height = 320,
  activation,
}: {
  nodes: GraphCanvasNode[];
  edges: GraphCanvasEdge[];
  selectedId?: string | null;
  onSelect?: (id: string | null) => void;
  height?: number;
  /** External synapse field; when omitted the canvas owns a local one fed by
   * selection strikes. */
  activation?: ActivationField;
}) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const sigmaRef = useRef<Sigma | null>(null);
  const fieldRef = useRef<ActivationField | null>(null);
  if (activation) fieldRef.current = activation;
  else if (!fieldRef.current) fieldRef.current = new ActivationField();

  useEffect(() => {
    const container = containerRef.current;
    if (!container || nodes.length === 0) return;

    const graph = new Graph({ multi: true, type: 'directed' });
    const maxDegree = Math.max(...nodes.map((n) => n.degree), 1);
    // Deterministic circular seed (sorted order) so layouts are stable
    // across reloads of the same subgraph.
    const sorted = [...nodes].sort((a, b) => a.id.localeCompare(b.id));
    sorted.forEach((node, index) => {
      const angle = (index / sorted.length) * Math.PI * 2;
      graph.addNode(node.id, {
        label: node.label,
        kind: node.kind,
        degree: node.degree,
        x: Math.cos(angle),
        y: Math.sin(angle),
        size: 3 + 8 * Math.sqrt(node.degree / maxDegree),
      });
    });
    for (const edge of edges) {
      if (graph.hasNode(edge.source) && graph.hasNode(edge.target)) {
        graph.addEdge(edge.source, edge.target, { kind: edge.kind });
      }
    }
    forceAtlas2.assign(graph, {
      iterations: 200,
      settings: forceAtlas2.inferSettings(graph),
    });

    let colors = palette(container);
    let hovered: string | null = null;
    const field = fieldRef.current ?? new ActivationField();
    let nodeRgb = cssColorToRgb(colors.node);
    let hotRgb = cssColorToRgb(colors.nodeSelected);
    const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

    const renderer = new Sigma(graph, container, {
      renderLabels: true,
      labelRenderedSizeThreshold: 9,
      labelFont: 'ui-monospace, monospace',
      labelSize: 10,
      labelColor: { color: colors.label },
      defaultEdgeColor: colors.edge,
      nodeReducer: (node, data) => {
        const isSelected = node === selectedId;
        const isHovered = node === hovered;
        const isNeighbor =
          hovered != null && (graph.areNeighbors(node, hovered) || isHovered);
        const dimmed = hovered != null && !isNeighbor;
        const heat = field.heatOf(node);
        // Synapse heat: color lerps toward the accent and the node swells —
        // a strike blooms then decays to dark (exponential half-life).
        const baseColor = dimmed ? colors.dim : colors.node;
        const color =
          isSelected || isHovered
            ? colors.nodeSelected
            : heat > 0
              ? lerpRgb(cssColorToRgb(baseColor), hotRgb, Math.min(1, heat))
              : baseColor;
        return {
          ...data,
          color,
          size: (data['size'] as number) * (1 + 0.5 * heat),
          zIndex: isSelected || isHovered || heat > 0.4 ? 2 : 1,
          label:
            isSelected || isHovered || heat > 0.5 || data['degree'] >= maxDegree * 0.6
              ? data['label']
              : '',
        };
      },
      edgeReducer: (edge, data) => {
        const dimmed =
          hovered != null &&
          !graph.extremities(edge).some((end) => end === hovered);
        const [from, to] = graph.extremities(edge);
        const edgeHeat = Math.min(field.heatOf(from ?? ''), field.heatOf(to ?? ''));
        // Edges glow while both endpoints are warm: the visible synapse.
        const color =
          edgeHeat > 0.05
            ? lerpRgb(cssColorToRgb(colors.edge), hotRgb, Math.min(1, edgeHeat))
            : dimmed
              ? 'transparent'
              : colors.edge;
        return { ...data, color, size: edgeHeat > 0.05 ? 1 + 2 * edgeHeat : data['size'] };
      },
    });
    sigmaRef.current = renderer;

    renderer.on('enterNode', ({ node }) => {
      if (node.startsWith(HALO)) return;
      hovered = node;
      renderer.refresh();
    });
    renderer.on('leaveNode', () => {
      hovered = null;
      renderer.refresh();
    });
    renderer.on('clickNode', ({ node }) => {
      if (node.startsWith(HALO)) return;
      onSelect?.(node);
      // Traveling activation: the struck node fires now; its neighborhood
      // fires one synaptic delay later (real caller/reference edges only).
      field.strike([node], 1);
      const neighbors = graph.neighbors(node);
      if (reducedMotion) field.strike(neighbors, 0.55);
      else setTimeout(() => { field.strike(neighbors, 0.55); wake(); }, 140);
      wake();
    });
    renderer.on('clickStage', () => onSelect?.(null));

    // Bloom halos: each warm node carries a companion low-alpha disc behind
    // it (managed here, invisible to reducers) — shader-free glow.
    const HALO = '__halo__';
    const syncHalos = () => {
      const [hr, hg, hb] = hotRgb;
      for (const node of [...graph.nodes()]) {
        if (node.startsWith(HALO)) continue;
        const heat = field.heatOf(node);
        const haloId = HALO + node;
        if (heat > 0.12) {
          const attrs = graph.getNodeAttributes(node);
          const halo = {
            x: attrs['x'],
            y: attrs['y'],
            size: (attrs['size'] as number) * (1.6 + 1.4 * heat),
            color: `rgba(${hr}, ${hg}, ${hb}, ${(0.22 * heat).toFixed(3)})`,
            label: '',
            zIndex: 0,
          };
          if (graph.hasNode(haloId)) graph.mergeNodeAttributes(haloId, halo);
          else graph.addNode(haloId, halo);
        } else if (graph.hasNode(haloId)) {
          graph.dropNode(haloId);
        }
      }
    };

    // Decay loop: runs only while warm; reduced-motion snaps to a single
    // static refresh per strike instead of animating.
    let raf = 0;
    const step = (now: number) => {
      const warm = field.tick(now);
      syncHalos();
      renderer.refresh();
      raf = warm && !reducedMotion ? requestAnimationFrame(step) : 0;
    };
    const wake = () => {
      if (reducedMotion) { field.tick(performance.now()); syncHalos(); renderer.refresh(); return; }
      if (!raf) raf = requestAnimationFrame(step);
    };
    if (field.warm) wake();

    const themeObserver = new MutationObserver(() => {
      colors = palette(container);
      nodeRgb = cssColorToRgb(colors.node);
      hotRgb = cssColorToRgb(colors.nodeSelected);
      void nodeRgb;
      renderer.setSetting('defaultEdgeColor', colors.edge);
      renderer.setSetting('labelColor', { color: colors.label });
      renderer.refresh();
    });
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme'],
    });

    return () => {
      if (raf) cancelAnimationFrame(raf);
      themeObserver.disconnect();
      renderer.kill();
      sigmaRef.current = null;
    };
  }, [nodes, edges, selectedId, onSelect]);

  if (nodes.length === 0) {
    return (
      <p className="p-6 text-center text-sm text-text-muted">
        no graph neighborhood to draw
      </p>
    );
  }
  return (
    <figure className="flex flex-col gap-1">
      <div
        ref={containerRef}
        style={{ height }}
        className="overflow-hidden rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-0"
        role="img"
        aria-label={`Code graph: ${nodes.length} symbols, ${edges.length} relations. The symbol list alongside is the accessible equivalent.`}
      />
      <figcaption className="text-2xs text-text-muted">
        {nodes.length} symbols · {edges.length} relations · size = connectedness ·
        hover isolates, click fires the synapse (bloom decays as activation
        fades)
      </figcaption>
    </figure>
  );
}
