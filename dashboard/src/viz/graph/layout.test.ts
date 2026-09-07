import Graph from 'graphology';
import { describe, expect, it } from 'vitest';
import {
  buildDendrites,
  isMeasuredField,
  nodeHullFrame,
  prepareField,
} from './layout.ts';
import type { GraphCanvasEdge, GraphCanvasNode } from './types.ts';

/**
 * Layout preparation runs in the plain `node` project: it takes the canvas box
 * as a number and its kind hues as an injected resolver, so none of the
 * arithmetic below needs a document, a canvas context or a WebGL renderer to
 * be checked. That is the point of the module — the geometry the reader ends
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
  it('draws a measured field exactly where the caller placed it', () => {
    const { graph, placed } = prepare([
      { id: 'a', label: 'a', kind: 'k', x: -12, y: 7 },
      { id: 'b', label: 'b', kind: 'k', x: 40, y: -3 },
    ]);

    expect(placed).toBe(true);
    expect(position(graph, 'a')).toEqual([-12, 7]);
    expect(position(graph, 'b')).toEqual([40, -3]);
  });

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
    const { graph, neighborsOf, realNodes } = prepare(
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
    expect(neighborsOf.get('a')).toEqual(['b']);
    expect(neighborsOf.get('b')).toEqual(['a']);
  });
});

describe('buildDendrites', () => {
  it('replaces each relation with a curve whose ends are its two real nodes', () => {
    const { graph, realNodes } = prepare(
      [
        { id: 'a', label: 'a', kind: 'k', x: 0, y: 0 },
        { id: 'b', label: 'b', kind: 'k', x: 10, y: 0 },
      ],
      [{ source: 'a', target: 'b' }],
    );

    const strands = buildDendrites(graph, 1);

    expect(strands).toHaveLength(1);
    expect(strands[0]!.from).toBe('a');
    expect(strands[0]!.to).toBe('b');
    expect(strands[0]!.points[0]).toEqual([0, 0]);
    expect(strands[0]!.points.at(-1)).toEqual([10, 0]);
    // The curve bows off the chord, which is what a travelling pulse follows.
    expect(strands[0]!.points.some(([, y]) => y !== 0)).toBe(true);
    // Waypoints are joints, never bodies, and never part of the real topology.
    const waypoints = graph.nodes().filter((id) => id.startsWith('__way__'));
    expect(waypoints).toHaveLength(strands[0]!.points.length - 2);
    expect(realNodes).not.toContain(waypoints[0]);
  });

  it('curves the same subgraph identically every time', () => {
    const build = () => {
      const { graph } = prepare(
        [
          { id: 'a', label: 'a', kind: 'k', x: 0, y: 0 },
          { id: 'b', label: 'b', kind: 'k', x: 10, y: 4 },
        ],
        [{ source: 'a', target: 'b' }],
      );
      return buildDendrites(graph, 1)[0]!.points;
    };

    expect(build()).toEqual(build());
  });

  // Legibility at density beats flourish: past the budget the relations stay
  // straight, but they must still name their real endpoints or activation
  // would have no idea which relation conducts.
  it('keeps straight edges past the segment budget and still names their ends', () => {
    const { graph } = prepare(
      [
        { id: 'a', label: 'a', kind: 'k', x: 0, y: 0 },
        { id: 'b', label: 'b', kind: 'k', x: 10, y: 0 },
      ],
      [{ source: 'a', target: 'b' }],
    );

    const strands = buildDendrites(graph, 401);

    expect(strands).toEqual([]);
    expect(graph.nodes().filter((id) => id.startsWith('__way__'))).toEqual([]);
    const edge = graph.edges()[0]!;
    expect(graph.getEdgeAttribute(edge, 'srcReal')).toBe('a');
    expect(graph.getEdgeAttribute(edge, 'dstReal')).toBe('b');
  });
});

describe('nodeHullFrame', () => {
  it('frames the real network with a margin, ignoring anything the renderer added', () => {
    const graph = new Graph();
    graph.addNode('a', { x: 0, y: 0 });
    graph.addNode('b', { x: 10, y: 20 });
    // A dendrite waypoint bows outside the node hull; it may not move the camera.
    graph.addNode('__way__0:1', { x: 500, y: -500 });

    const frame = nodeHullFrame(graph, ['a', 'b']);

    expect(frame.x).toEqual([-1.5, 11.5]);
    expect(frame.y).toEqual([-3, 23]);
  });

  it('gives a single body a frame with real width rather than a zero-size one', () => {
    const graph = new Graph();
    graph.addNode('only', { x: 4, y: 4 });

    const frame = nodeHullFrame(graph, ['only']);

    expect(frame.x[1] - frame.x[0]).toBeGreaterThan(0);
    expect(frame.y[1] - frame.y[0]).toBeGreaterThan(0);
  });
});
