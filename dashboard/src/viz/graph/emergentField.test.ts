import Graph from 'graphology';
import { describe, expect, it } from 'vitest';
import {
  composeConstellation,
  settleEmergentField,
  type ForceAtlas2Module,
} from './emergentField.ts';
import { prepareField } from './layout.ts';
import type { GraphCanvasEdge, GraphCanvasNode } from './types.ts';

/**
 * The emergent path with its engine handed in, so the settle's tuning and the
 * constellation composure can be read off directly instead of inferred from a
 * picture. Only the last case here touches the real library.
 */

interface RecordedSettle {
  iterations: number;
  settings: Record<string, number | boolean | undefined>;
}

function recordingEngine(recorded: RecordedSettle[]): ForceAtlas2Module {
  return {
    inferSettings: () => ({ gravity: 2, strongGravityMode: true }),
    assign: (_graph: Graph, params: RecordedSettle) => {
      recorded.push(params);
    },
  } as unknown as ForceAtlas2Module;
}

function prepare(nodes: GraphCanvasNode[], edges: GraphCanvasEdge[]) {
  return prepareField({
    nodes,
    edges,
    viewport: { width: 640, height: 320 },
    kindRgb: () => [10, 20, 30],
  });
}

function ring(count: number): GraphCanvasNode[] {
  return Array.from({ length: count }, (_, index) => ({
    id: `n${index}`,
    label: `n${index}`,
    kind: 'k',
  }));
}

function centroid(graph: Graph, ids: string[]): [number, number] {
  let x = 0;
  let y = 0;
  for (const id of ids) {
    x += graph.getNodeAttribute(id, 'x') as number;
    y += graph.getNodeAttribute(id, 'y') as number;
  }
  return [x / ids.length, y / ids.length];
}

describe('settleEmergentField', () => {
  it('tunes gravity, repulsion and settling time against the graph it was handed', () => {
    const recorded: RecordedSettle[] = [];
    // Four nodes, two relations: an average degree of exactly one.
    const prepared = prepare(ring(4), [
      { source: 'n0', target: 'n1' },
      { source: 'n2', target: 'n3' },
    ]);

    settleEmergentField(prepared, recordingEngine(recorded));

    expect(recorded).toHaveLength(1);
    const [settle] = recorded;
    expect(settle!.iterations).toBe(240);
    // A small graph is pulled hard toward the centre so the tissue reads dense,
    // and that pull is relaxed again by however much the edges already apply.
    expect(settle!.settings['gravity']).toBeCloseTo((2 * 50) / 1.5, 10);
    expect(settle!.settings['scalingRatio']).toBe(7);
    // Whatever the engine inferred for itself survives the overrides.
    expect(settle!.settings['strongGravityMode']).toBe(true);
  });

  it('buys a dense component more settling time, but not without limit', () => {
    const recorded: RecordedSettle[] = [];
    const nodes = ring(4);
    const edges = nodes.flatMap((from) =>
      nodes.filter((to) => to.id !== from.id).map((to) => ({ source: from.id, target: to.id })),
    );

    settleEmergentField(prepare(nodes, edges), recordingEngine(recorded));

    expect(recorded[0]!.iterations).toBe(400);
  });
});

describe('composeConstellation', () => {
  function scatter(components: Array<Array<[string, number, number]>>): Graph {
    const graph = new Graph();
    for (const component of components) {
      for (const [id, x, y] of component) graph.addNode(id, { x, y });
      for (let index = 1; index < component.length; index += 1) {
        graph.addEdge(component[index - 1]![0], component[index]![0]);
      }
    }
    return graph;
  }

  it('leaves a single connected component exactly where the settle put it', () => {
    const graph = scatter([[['a', 3, 4], ['b', 9, -2]]]);

    composeConstellation(graph);

    expect([
      graph.getNodeAttribute('a', 'x'),
      graph.getNodeAttribute('a', 'y'),
    ]).toEqual([3, 4]);
  });

  // Equal-sized components are a constellation with no main body, so they are
  // spaced evenly on a ring sized to exactly the radius non-overlap needs.
  it('spaces equal components onto a ring instead of letting them drift apart', () => {
    const graph = scatter([
      [['a', 100, 100], ['b', 101, 100]],
      [['c', -900, -900], ['d', -899, -900]],
    ]);

    composeConstellation(graph);

    const first = centroid(graph, ['a', 'b']);
    const second = centroid(graph, ['c', 'd']);
    // Opposite ends of a diameter, and no longer a thousand units apart.
    expect(first[0]).toBeCloseTo(-second[0], 10);
    expect(first[1]).toBeCloseTo(-second[1], 10);
    expect(Math.hypot(first[0] - second[0], first[1] - second[1])).toBeLessThan(10);
  });

  // The opposite shape: one component holds the real structure and the rest
  // are strays. Pushing a near-zero-extent orphan out to the radius the
  // dominant cluster needs inflates the camera's frame far more than it helps,
  // so the strays are anchored to the mass instead.
  it('anchors strays to the dominant component rather than to a shared ring', () => {
    const graph = scatter([
      [
        ['a', 0, 0],
        ['b', 2, 0],
        ['c', 4, 0],
        ['d', 6, 0],
        ['e', 8, 0],
        ['f', 10, 0],
      ],
      [['orphan', 4000, 4000]],
    ]);

    composeConstellation(graph);

    const mass = centroid(graph, ['a', 'b', 'c', 'd', 'e', 'f']);
    expect(mass[0]).toBeCloseTo(0, 10);
    expect(mass[1]).toBeCloseTo(0, 10);
    // The stray sits in a tight band around the mass, never further out than
    // twice the mass's own extent.
    const orphan = centroid(graph, ['orphan']);
    expect(Math.hypot(orphan[0], orphan[1])).toBeLessThanOrEqual(
      2 * 5 + Number.EPSILON,
    );
  });
});
