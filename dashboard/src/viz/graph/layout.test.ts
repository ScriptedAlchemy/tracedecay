import Graph from 'graphology';
import { describe, expect, it } from 'vitest';
import {
  isMeasuredField,
  prepareField,
} from './layout.ts';
import type { GraphCanvasEdge, GraphCanvasNode } from './types.ts';

/**
 * Layout preparation runs in the plain `node` project: it takes the canvas box
 * as a number and its kind hues as an injected resolver, so none of the
 * arithmetic below needs a document, a canvas context or a WebGL renderer to
 * be checked. That is the point of the module, the geometry the reader ends
 * up looking at is decided here, and it is now decidable in isolation.
 */

const KIND_RGB = (): [number, number, number] => [10, 20, 30];
const VIEWPORT = { width: 640, height: 320 };

function prepare(
  nodes: GraphCanvasNode[],
  edges: GraphCanvasEdge[] = [],
  viewport = VIEWPORT,
) {
  return prepareField({ nodes, edges, viewport, kindRgb: KIND_RGB });
}

function position(graph: Graph, id: string): [number, number] {
  return [
    graph.getNodeAttribute(id, 'x') as number,
    graph.getNodeAttribute(id, 'y') as number,
  ];
}

describe('isMeasuredField', () => {
  it('is true only when every node carries finite coordinates', () => {
    expect(
      isMeasuredField([
        { id: 'a', label: 'a', kind: 'k', x: 0, y: 0 },
        { id: 'b', label: 'b', kind: 'k', x: 3, y: -4 },
      ]),
    ).toBe(true);
    expect(
      isMeasuredField([
        { id: 'a', label: 'a', kind: 'k', x: 0, y: 0 },
        { id: 'b', label: 'b', kind: 'k', x: 3 },
      ]),
    ).toBe(false);
    expect(
      isMeasuredField([{ id: 'a', label: 'a', kind: 'k', x: Number.NaN, y: 0 }]),
    ).toBe(false);
  });
});

describe('prepareField', () => {
  // The invariant the `placed` flag exists for: one node without coordinates
  // would otherwise be dropped at the seed circle beside nodes whose position
  // is a claim about their data, and nothing on screen tells the two apart.
  it('drops the whole field to the seed circle when a single node is unplaced', () => {
    const { graph, placed } = prepare([
      { id: 'a', label: 'a', kind: 'k', x: -12, y: 7 },
      { id: 'b', label: 'b', kind: 'k' },
    ]);

    expect(placed).toBe(false);
    expect(position(graph, 'a')).not.toEqual([-12, 7]);
  });

  it('seeds an unplaced field deterministically, in sorted id order', () => {
    const first = prepare([
      { id: 'b', label: 'b', kind: 'k' },
      { id: 'a', label: 'a', kind: 'k' },
    ]);
    // The same subgraph handed over in the other order lays out identically.
    const second = prepare([
      { id: 'a', label: 'a', kind: 'k' },
      { id: 'b', label: 'b', kind: 'k' },
    ]);

    const [ax, ay] = position(first.graph, 'a');
    expect(ax).toBeCloseTo(1, 10);
    expect(ay).toBeCloseTo(0, 10);
    expect(position(first.graph, 'b')[0]).toBeCloseTo(-1, 10);
    expect(position(second.graph, 'a')).toEqual(position(first.graph, 'a'));
    expect(position(second.graph, 'b')).toEqual(position(first.graph, 'b'));
  });

  it('keeps absent connectedness absent rather than coercing it to zero', () => {
    const { graph } = prepare([
      { id: 'known', label: 'known', kind: 'k', degree: 4 },
      { id: 'unknown', label: 'unknown', kind: 'k' },
    ]);

    expect(graph.getNodeAttribute('unknown', 'degree')).toBeUndefined();
    // It still gets a body: the minimum marker, not a zero-radius nothing.
    expect(graph.getNodeAttribute('unknown', 'size')).toBeGreaterThan(0);
    expect(graph.getNodeAttribute('unknown', 'isHub')).toBe(false);
    expect(graph.getNodeAttribute('known', 'isHub')).toBe(true);
  });

  it('rests a field with no vitality measurement mid-scale, and clamps a real one', () => {
    const { graph } = prepare([
      { id: 'absent', label: 'absent', kind: 'k' },
      { id: 'over', label: 'over', kind: 'k', vitality: 4 },
      { id: 'under', label: 'under', kind: 'k', vitality: -1 },
    ]);

    expect(graph.getNodeAttribute('absent', 'vitality')).toBe(0.6);
    expect(graph.getNodeAttribute('over', 'vitality')).toBe(1);
    expect(graph.getNodeAttribute('under', 'vitality')).toBe(0);
  });

  it('clamps a dense field to its screen-space ceiling so a hub cannot swallow its neighbours', () => {
    const nodes = Array.from({ length: 40 }, (_, index) => ({
      id: `node-${index}`,
      label: `Node ${index}`,
      kind: 'function',
      degree: index === 0 ? 39 : 1,
    }));
    const edges = nodes.slice(1).map((node) => ({ source: 'node-0', target: node.id }));

    const { graph, denseField, roominess } = prepare(nodes, edges);

    expect(denseField).toBe(true);
    // 7.5 px relaxed by the room this canvas actually gives each body.
    const ceiling = 7.5 * Math.max(0.62, roominess);
    expect(graph.getNodeAttribute('node-0', 'size') as number).toBeCloseTo(ceiling, 10);
    const sizes = graph.nodes().map((id) => graph.getNodeAttribute(id, 'size') as number);
    expect(Math.max(...sizes)).toBeLessThanOrEqual(ceiling);
    // Rank inside the field survives the ceiling: the hub is still the largest.
    expect(Math.min(...sizes)).toBeLessThan(ceiling);
  });

  it('keeps repository hubs a fixed categorical size regardless of checkout count', () => {
    const { graph } = prepare([
      { id: 'small-repo', label: 'small', kind: 'repository', degree: 2 },
      { id: 'large-repo', label: 'large', kind: 'repository', degree: 40 },
      { id: 'project', label: 'project', kind: 'project', degree: 40 },
    ]);

    expect(graph.getNodeAttribute('small-repo', 'size')).toBe(
      graph.getNodeAttribute('large-repo', 'size'),
    );
    expect(graph.getNodeAttribute('large-repo', 'size') as number).toBeLessThan(
      graph.getNodeAttribute('project', 'size') as number,
    );
  });

  it('shrinks bodies for a narrow canvas instead of fusing them', () => {
    const nodes = Array.from({ length: 20 }, (_, index) => ({
      id: `n${index}`,
      label: `n${index}`,
      kind: 'function',
      degree: 3,
    }));

    const roomy = prepare(nodes, [], { width: 1060, height: 600 });
    const cramped = prepare(nodes, [], { width: 270, height: 200 });

    expect(cramped.roominess).toBeLessThan(roomy.roominess);
    expect(cramped.graph.getNodeAttribute('n0', 'size') as number).toBeLessThan(
      roomy.graph.getNodeAttribute('n0', 'size') as number,
    );
  });

  it('records only relations whose two ends are actually drawn', () => {
    const { graph, realNodes } = prepare(
      [
        { id: 'a', label: 'a', kind: 'k' },
        { id: 'b', label: 'b', kind: 'k' },
      ],
      [
        { source: 'a', target: 'b' },
        { source: 'a', target: 'offscreen' },
      ],
    );

    expect(graph.size).toBe(1);
    expect(realNodes.sort()).toEqual(['a', 'b']);
  });
});
