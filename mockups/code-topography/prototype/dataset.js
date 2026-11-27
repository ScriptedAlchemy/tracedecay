/**
 * The prototype uses the same TRACE subgraph as
 * `mockups/code-topography/trace.html` so readers can compare motion without
 * changing the data.
 *
 * `deg` comes from the node payload. `calls` counts call sites per edge.
 * `membranes` use `contains` edges from impl or trait nodes to their methods.
 * `regions` use the cortex sheet's module aggregates. `unresolved` records a
 * `calls` edge whose target the extractor could not resolve. The sheet still
 * draws that edge.
 *
 * The coordinates are anchors from the static sheet's 1440x1160 layout. Each
 * node rests at its anchor, so the simulation matches the static layout at rest.
 *
 * @module dataset
 */

/** Each row represents hop distance from the focus, not elevation. */
export const ROW = Object.freeze({
  u3: 118,
  u2: 268,
  u1: 420,
  focus: 590,
  d1: 760,
  d2: 906,
  d3: 1046,
});

export const WORLD = Object.freeze({ width: 1440, height: 1160 });

export const FOCUS_ID = 'focus';

/** @typedef {{id: string, name: string, kind: string, deg: number, mod: string|null, x0: number, y0: number, row: string, unresolved?: string, source?: string}} TraceNode */

/** @type {TraceNode[]} */
export const NODES = [
  { id: 'dispatch', name: 'dispatch_tool_call', kind: 'method', deg: 31, mod: 'hooks/session', x0: 250, y0: ROW.u3, row: 'u3' },
  { id: 'replay', name: 'replay_transcript', kind: 'method', deg: 18, mod: 'hooks/session', x0: 500, y0: ROW.u3, row: 'u3' },
  { id: 'mount', name: 'mount_graph_plugin', kind: 'function', deg: 9, mod: 'api/plugins', x0: 790, y0: ROW.u3, row: 'u3' },
  { id: 'ctest', name: 'contributes_catalog', kind: 'test', deg: 4, mod: 'tests', x0: 1120, y0: ROW.u3, row: 'u3' },

  { id: 'ctxroute', name: 'context_route', kind: 'method', deg: 24, mod: 'api/routes', x0: 300, y0: ROW.u2, row: 'u2' },
  { id: 'schroute', name: 'search_route', kind: 'method', deg: 17, mod: 'api/routes', x0: 545, y0: ROW.u2, row: 'u2' },
  { id: 'profile', name: 'build_profile', kind: 'function', deg: 22, mod: 'tool-catalog/snapshot', x0: 880, y0: ROW.u2, row: 'u2' },
  { id: 'bind', name: 'bind_surfaces', kind: 'method', deg: 15, mod: 'application/git', x0: 1180, y0: ROW.u2, row: 'u2', source: 'no callers ≤3 hops, public surface' },

  { id: 'hctx', name: 'handle_context_request', kind: 'function', deg: 46, mod: 'application/handlers', x0: 405, y0: ROW.u1, row: 'u1' },
  { id: 'hsearch', name: 'handle_search_request', kind: 'function', deg: 28, mod: 'application/handlers', x0: 680, y0: ROW.u1, row: 'u1' },
  { id: 'lookup', name: 'lookup_callable', kind: 'method', deg: 19, mod: 'application/retrieval', x0: 1030, y0: ROW.u1, row: 'u1' },

  { id: 'focus', name: 'resolve_context', kind: 'method', deg: 63, mod: 'application/retrieval', x0: 640, y0: ROW.focus, row: 'focus' },
  { id: 'sibgraph', name: 'symbol_graph_for', kind: 'method', deg: 21, mod: 'application/retrieval', x0: 855, y0: ROW.focus, row: 'focus' },
  { id: 'sibcall', name: 'hydrate_callable', kind: 'method', deg: 17, mod: 'application/retrieval', x0: 430, y0: ROW.focus, row: 'focus' },

  { id: 'neighbors', name: 'neighbors_of', kind: 'function', deg: 26, mod: 'application/retrieval', x0: 300, y0: ROW.d1, row: 'd1' },
  { id: 'assemble', name: 'assemble_context', kind: 'method', deg: 14, mod: 'application/retrieval', x0: 560, y0: ROW.d1, row: 'd1' },
  { id: 'evaluate', name: 'evaluate_decision', kind: 'method', deg: 20, mod: 'policy/authorization', x0: 800, y0: ROW.d1, row: 'd1' },
  { id: 'fetchn', name: 'fetch_nodes', kind: 'method', deg: 33, mod: 'store/repository', x0: 1030, y0: ROW.d1, row: 'd1' },
  { id: 'fetche', name: 'fetch_edges', kind: 'method', deg: 29, mod: 'store/repository', x0: 1250, y0: ROW.d1, row: 'd1' },

  { id: 'adjacency', name: 'adjacency_of', kind: 'function', deg: 38, mod: 'domain/graph', x0: 265, y0: ROW.d2, row: 'd2' },
  { id: 'nodeview', name: 'node_view', kind: 'struct', deg: 41, mod: 'domain/graph', x0: 545, y0: ROW.d2, row: 'd2' },
  { id: 'rules', name: 'rules_for', kind: 'function', deg: 12, mod: 'policy/configuration', x0: 800, y0: ROW.d2, row: 'd2' },
  { id: 'acquire', name: 'acquire', kind: 'method', deg: 22, mod: 'rusqlite/reader', x0: 1140, y0: ROW.d2, row: 'd2' },

  { id: 'edgeindex', name: 'edge_index', kind: 'struct', deg: 30, mod: 'domain/graph', x0: 265, y0: ROW.d3, row: 'd3' },
  { id: 'checkout', name: 'checkout', kind: 'method', deg: 11, mod: 'rusqlite/reader', x0: 1140, y0: ROW.d3, row: 'd3' },
  {
    id: 'unresolved',
    name: 'dyn ContextSource',
    kind: 'unresolved',
    deg: 0,
    mod: null,
    x0: 545,
    y0: ROW.d3,
    row: 'd3',
    unresolved: '1 call site · trait object, impl not in graph',
  },
];

/**
 * Every channel carries its own call-site count. Volume is NOT conserved: a
 * function reached 58 times may call the next one 34 times.
 *
 * `dir` is drawing direction only, `up` tributary, `down` delta, `in` a move
 * between two methods of the same type, `lost` a channel whose target left the
 * graph. The simulation treats all four as the same undirected spring.
 *
 * @type {Array<{a: string, b: string, calls: number, dir: 'up'|'down'|'in'|'lost'}>}
 */
export const EDGES = [
  { a: 'dispatch', b: 'ctxroute', calls: 41, dir: 'up' },
  { a: 'replay', b: 'schroute', calls: 18, dir: 'up' },
  { a: 'mount', b: 'ctxroute', calls: 6, dir: 'up' },
  { a: 'ctest', b: 'profile', calls: 3, dir: 'up' },
  { a: 'ctxroute', b: 'hctx', calls: 58, dir: 'up' },
  { a: 'schroute', b: 'hsearch', calls: 23, dir: 'up' },
  { a: 'profile', b: 'hctx', calls: 9, dir: 'up' },
  { a: 'bind', b: 'lookup', calls: 14, dir: 'up' },
  { a: 'hctx', b: 'focus', calls: 34, dir: 'up' },
  { a: 'hsearch', b: 'focus', calls: 12, dir: 'up' },
  { a: 'lookup', b: 'focus', calls: 7, dir: 'up' },
  { a: 'focus', b: 'sibgraph', calls: 9, dir: 'in' },
  { a: 'focus', b: 'sibcall', calls: 5, dir: 'in' },
  { a: 'sibgraph', b: 'neighbors', calls: 21, dir: 'down' },
  { a: 'focus', b: 'assemble', calls: 11, dir: 'down' },
  { a: 'focus', b: 'evaluate', calls: 8, dir: 'down' },
  { a: 'sibcall', b: 'fetchn', calls: 16, dir: 'down' },
  { a: 'fetchn', b: 'fetche', calls: 6, dir: 'in' },
  { a: 'neighbors', b: 'adjacency', calls: 21, dir: 'down' },
  { a: 'assemble', b: 'nodeview', calls: 11, dir: 'down' },
  { a: 'evaluate', b: 'rules', calls: 8, dir: 'down' },
  { a: 'fetche', b: 'acquire', calls: 16, dir: 'down' },
  { a: 'adjacency', b: 'edgeindex', calls: 14, dir: 'down' },
  { a: 'acquire', b: 'checkout', calls: 16, dir: 'down' },
  { a: 'nodeview', b: 'unresolved', calls: 1, dir: 'lost' },
];

/** `contains` edges from an impl/trait node to its methods. */
export const MEMBRANES = [
  {
    label: 'impl GraphRoutes',
    file: 'crates/tracedecay-api/src/routes/graph.rs',
    of: ['ctxroute', 'schroute'],
    kind: 'impl',
  },
  {
    label: 'impl RetrievalService',
    file: 'crates/tracedecay-contracts/src/retrieval/service.rs',
    of: ['sibcall', 'focus', 'sibgraph'],
    kind: 'impl',
    hero: true,
  },
  {
    label: 'impl SqliteRepository',
    file: 'crates/tracedecay-store/src/repository/sqlite.rs',
    of: ['fetchn', 'fetche'],
    kind: 'impl',
  },
  {
    label: 'trait ContextSource',
    file: 'crates/tracedecay-domain/src/graph/source.rs',
    of: ['nodeview', 'unresolved'],
    kind: 'trait',
  },
];

/**
 * The dimmed background shows only cortex regions containing traced modules.
 * It uses two contours instead of the full set to show whether each outline
 * moves with its member nodes.
 */
export const REGIONS = [
  { mod: 'hooks/session', sym: 92, of: ['dispatch', 'replay'] },
  { mod: 'api', sym: 179, of: ['mount', 'ctxroute', 'schroute'] },
  { mod: 'tool-catalog + tests', sym: 64, of: ['ctest', 'profile'] },
  { mod: 'application/handlers', sym: 156, of: ['hctx', 'hsearch'] },
  { mod: 'application/retrieval', sym: 214, of: ['lookup', 'focus', 'sibgraph', 'sibcall', 'neighbors', 'assemble'] },
  { mod: 'application/git', sym: 88, of: ['bind'] },
  { mod: 'policy', sym: 109, of: ['evaluate', 'rules'] },
  { mod: 'store + rusqlite', sym: 234, of: ['fetchn', 'fetche', 'acquire', 'checkout'] },
  { mod: 'domain/graph', sym: 148, of: ['adjacency', 'nodeview', 'edgeindex'] },
];

/**
 * Compute the hop distance represented by each node's row.
 *
 * An `in` channel connects methods within the same type and costs zero hops.
 * This rule reproduces the static sheet's rows for all 26 nodes, as
 * `sim.test.mjs` checks.
 *
 * The QA check instead uses `hopDistances` in `sim.js`, which counts every
 * undirected channel when deciding which nodes are far away.
 *
 * @returns {Map<string, number>} Hop ring by node ID.
 */
export function hopRings(dataset = DATASET, sourceId = FOCUS_ID) {
  const adjacency = new Map(dataset.nodes.map((node) => [node.id, []]));
  for (const edge of dataset.edges) {
    const cost = edge.dir === 'in' ? 0 : 1;
    adjacency.get(edge.a).push({ other: edge.b, cost });
    adjacency.get(edge.b).push({ other: edge.a, cost });
  }
  // 0–1 BFS: zero-cost channels go to the front of the queue, so a lateral move
  // inside a membrane never advances the ring.
  const distance = new Map([[sourceId, 0]]);
  const deque = [sourceId];
  while (deque.length) {
    const id = deque.shift();
    for (const { other, cost } of adjacency.get(id)) {
      const candidate = distance.get(id) + cost;
      if (distance.has(other) && distance.get(other) <= candidate) continue;
      distance.set(other, candidate);
      if (cost === 0) deque.unshift(other);
      else deque.push(other);
    }
  }
  return distance;
}

/** Readbar figures, all carried from the static sheet. */
export const READOUT = Object.freeze({
  focusName: 'RetrievalService::resolve_context',
  callersUp: '12 nodes · 53 call sites',
  calleesDown: '14 nodes · 61 call sites',
  depthLimit: '3 ↑ / 3 ↓',
  beyondLimit: '41 not drawn',
  membranes: '4 types entered',
  modules: '7 crossed · 9 shorelines',
  focusStats: '53 in · 61 out · degree 63 · cyclomatic 14',
  focusSite: 'retrieval/service.rs:212–318',
});

export const DATASET = Object.freeze({
  world: WORLD,
  row: ROW,
  focusId: FOCUS_ID,
  nodes: NODES,
  edges: EDGES,
  membranes: MEMBRANES,
  regions: REGIONS,
  readout: READOUT,
});

/**
 * Translate the dataset into the simulation's vocabulary: mass IS degree,
 * stiffness IS the call-site count, and the anchor IS the layout position.
 * No shaping, no normalisation that would launder the measurement, the
 * simulation's own parameters do the scaling, in one place, where they can be
 * read off a table.
 */
export function buildSimSpec(dataset = DATASET, { seed = 20260725, params } = {}) {
  return {
    seed,
    params,
    nodes: dataset.nodes.map((node) => ({ id: node.id, mass: node.deg, x0: node.x0, y0: node.y0 })),
    springs: dataset.edges.map((edge) => ({ a: edge.a, b: edge.b, stiffness: edge.calls })),
  };
}

export default DATASET;
