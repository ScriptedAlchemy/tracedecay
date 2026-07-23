import { useEffect, useRef } from 'react';
import Graph from 'graphology';
import forceAtlas2 from 'graphology-layout-forceatlas2';
import Sigma from 'sigma';

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
}: {
  nodes: GraphCanvasNode[];
  edges: GraphCanvasEdge[];
  selectedId?: string | null;
  onSelect?: (id: string | null) => void;
  height?: number;
}) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const sigmaRef = useRef<Sigma | null>(null);

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
        return {
          ...data,
          color: isSelected || isHovered ? colors.nodeSelected : dimmed ? colors.dim : colors.node,
          zIndex: isSelected || isHovered ? 2 : 1,
          label: isSelected || isHovered || data['degree'] >= maxDegree * 0.6 ? data['label'] : '',
        };
      },
      edgeReducer: (edge, data) => {
        const dimmed =
          hovered != null &&
          !graph.extremities(edge).some((end) => end === hovered);
        return { ...data, color: dimmed ? 'transparent' : colors.edge };
      },
    });
    sigmaRef.current = renderer;

    renderer.on('enterNode', ({ node }) => {
      hovered = node;
      renderer.refresh();
    });
    renderer.on('leaveNode', () => {
      hovered = null;
      renderer.refresh();
    });
    renderer.on('clickNode', ({ node }) => onSelect?.(node));
    renderer.on('clickStage', () => onSelect?.(null));

    const themeObserver = new MutationObserver(() => {
      colors = palette(container);
      renderer.setSetting('defaultEdgeColor', colors.edge);
      renderer.setSetting('labelColor', { color: colors.label });
      renderer.refresh();
    });
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme'],
    });

    return () => {
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
        hover isolates a neighborhood, click selects
      </figcaption>
    </figure>
  );
}
