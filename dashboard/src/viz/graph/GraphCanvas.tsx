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
        size: 5 + 9 * Math.sqrt(node.degree / maxDegree),
        isHub: node.degree >= maxDegree * 0.75,
      });
    });
    for (const edge of edges) {
      if (graph.hasNode(edge.source) && graph.hasNode(edge.target)) {
        graph.addEdge(edge.source, edge.target, { kind: edge.kind });
      }
    }
    const fa2 = forceAtlas2.inferSettings(graph);
    forceAtlas2.assign(graph, {
      iterations: 200,
      // Small graphs over-spread with inferred gravity; pull clusters in so
      // the tissue reads dense, not lost in the void.
      settings: { ...fa2, gravity: (fa2.gravity ?? 1) * (nodes.length < 60 ? 8 : 2), scalingRatio: 4 },
    });

    let colors = palette(container);
    let hovered: string | null = null;
    const field = fieldRef.current ?? new ActivationField();
    let nodeRgb = cssColorToRgb(colors.node);
    let hotRgb = cssColorToRgb(colors.nodeSelected);
    const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

    const renderer = new Sigma(graph, container, {
      renderLabels: true,
      labelRenderedSizeThreshold: nodes.length <= 60 ? 5 : 9,
      labelFont: 'ui-monospace, monospace',
      labelSize: 11,
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
        const resting = data['isHub'] ? colors.label : baseColor;
        const color =
          isSelected || isHovered
            ? colors.nodeSelected
            : heat > 0
              ? lerpRgb(cssColorToRgb(resting), hotRgb, Math.min(1, heat))
              : resting;
        return {
          ...data,
          color,
          size: (data['size'] as number) * (1 + 0.5 * heat),
          zIndex: isSelected || isHovered || heat > 0.4 ? 2 : 1,
          label:
            isSelected || isHovered || heat > 0.5 || data['isHub'] || nodes.length <= 60
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
    {
      let minX = Infinity, maxX = -Infinity, minY = Infinity, maxY = -Infinity;
      graph.forEachNode((_, attrs) => {
        const x = attrs['x'] as number;
        const y = attrs['y'] as number;
        if (x < minX) minX = x;
        if (x > maxX) maxX = x;
        if (y < minY) minY = y;
        if (y > maxY) maxY = y;
      });
      const padX = (maxX - minX || 1) * 0.08;
      const padY = (maxY - minY || 1) * 0.08;
      renderer.setCustomBBox({
        x: [minX - padX, maxX + padX],
        y: [minY - padY, maxY + padY],
      });
    }

    renderer.on('enterNode', ({ node }) => {
      if (node.startsWith(HALO) || node.startsWith(PULSE)) return;
      hovered = node;
      renderer.refresh();
    });
    renderer.on('leaveNode', () => {
      hovered = null;
      renderer.refresh();
    });
    renderer.on('clickNode', ({ node }) => {
      if (node.startsWith(HALO) || node.startsWith(PULSE)) return;
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
    const PULSE = '__pulse__';
    // Traveling light (Glass Brain grammar): while an edge is warm, one
    // bright point runs from the hotter endpoint to the cooler one. Pulse
    // nodes exist only while warm and the frozen bbox keeps them from ever
    // rescaling the camera.
    const syncPulses = (now: number) => {
      const [hr, hg, hb] = hotRgb;
      const period = 900;
      const phase = (now % period) / period;
      for (const edge of graph.edges()) {
        const [from, to] = graph.extremities(edge);
        if (!from || !to || from.startsWith(HALO) || to.startsWith(HALO)) continue;
        if (from.startsWith(PULSE) || to.startsWith(PULSE)) continue;
        const heatFrom = field.heatOf(from);
        const heatTo = field.heatOf(to);
        const travel = Math.max(heatFrom, heatTo);
        const pulseId = PULSE + edge;
        if (travel > 0.18 && !reducedMotion) {
          const a = graph.getNodeAttributes(heatFrom >= heatTo ? from : to);
          const b = graph.getNodeAttributes(heatFrom >= heatTo ? to : from);
          const pulse = {
            x: (a['x'] as number) + ((b['x'] as number) - (a['x'] as number)) * phase,
            y: (a['y'] as number) + ((b['y'] as number) - (a['y'] as number)) * phase,
            size: 1.5 + 1.8 * travel,
            color: `rgba(${hr}, ${hg}, ${hb}, ${(0.85 * travel).toFixed(3)})`,
            label: '',
            zIndex: 3,
          };
          if (graph.hasNode(pulseId)) graph.mergeNodeAttributes(pulseId, pulse);
          else graph.addNode(pulseId, pulse);
        } else if (graph.hasNode(pulseId)) {
          graph.dropNode(pulseId);
        }
      }
    };
    const syncHalos = () => {
      const [hr, hg, hb] = hotRgb;
      for (const node of [...graph.nodes()]) {
        if (node.startsWith(HALO) || node.startsWith(PULSE)) continue;
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
          // Impact flare (Gource grammar): a wide, faint ring pops on strike
          // and expands as the bloom settles.
          const ringId = haloId + 'r';
          if (heat > 0.55) {
            const ring = {
              x: attrs['x'],
              y: attrs['y'],
              size: (attrs['size'] as number) * (2.4 + 2.8 * (1 - heat)),
              color: `rgba(${hr}, ${hg}, ${hb}, ${(0.09 * heat).toFixed(3)})`,
              label: '',
              zIndex: 0,
            };
            if (graph.hasNode(ringId)) graph.mergeNodeAttributes(ringId, ring);
            else graph.addNode(ringId, ring);
          } else if (graph.hasNode(ringId)) {
            graph.dropNode(ringId);
          }
        } else if (graph.hasNode(haloId)) {
          graph.dropNode(haloId);
          if (graph.hasNode(haloId + 'r')) graph.dropNode(haloId + 'r');
        }
      }
    };

    // Decay loop: runs only while warm; reduced-motion snaps to a single
    // static refresh per strike instead of animating.
    let raf = 0;
    const step = (now: number) => {
      const warm = field.tick(now);
      syncHalos();
      syncPulses(now);
      if (!warm) for (const node of [...graph.nodes()]) if (node.startsWith(PULSE)) graph.dropNode(node);
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
        className="overflow-hidden rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-0 [background:radial-gradient(120%_90%_at_50%_40%,var(--raw-surface-1)_0%,var(--raw-surface-0)_58%,oklch(0.11_0.01_260)_100%)]"
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
