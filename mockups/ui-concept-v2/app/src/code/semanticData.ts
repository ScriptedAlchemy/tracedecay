/*
Authored fixtures adapted from ScriptedAlchemy/tracedecay mockups/code-topography/
prototype/dataset.js, cortex.html, and core-sample.html. Layout is a view transform.
MIT License

Copyright (c) 2025 Enzo Lombardi

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
*/
import { atlasData, cycleGroups, nodeById, owningCrate, type AtlasNode } from "../structure";

export type CodeLens = "cortex" | "trace" | "core";
export type SemanticSource = "authored-fixture" | "measured-snapshot";

export type SemanticNode = {
  id: string;
  name: string;
  kind: string;
  module: string;
  file: string | null;
  startLine: number | null;
  endLine: number | null;
  degree: number;
  x: number;
  y: number;
  ring: number;
  unresolved?: string;
};

export type SemanticEdge = {
  source: string;
  target: string;
  weight: number;
  kind: "call" | "manifest";
  direction: "caller" | "callee" | "inside" | "unknown";
  detail: string;
};

export type SemanticRegion = {
  id: string;
  label: string;
  x: number;
  y: number;
  rx: number;
  ry: number;
  mass: number;
  depth: number;
  density: number;
  warmth: number | null;
  nodeIds: string[];
};

export type SemanticMembrane = {
  id: string;
  label: string;
  nodeIds: string[];
  kind: "impl" | "trait";
};

export type CoreSymbol = {
  id: string;
  name: string;
  kind: string;
  start: number;
  end: number;
  complexity: number;
};

export type CoreFile = {
  path: string;
  lines: number;
  indexedTo?: number;
  unavailableReason?: string;
  symbols: CoreSymbol[];
  internal: Array<{ from: string; to: string; calls: number }>;
};

export type SemanticDataset = {
  source: SemanticSource;
  sourceLabel: string;
  revision: string;
  regions: SemanticRegion[];
  nodes: SemanticNode[];
  edges: SemanticEdge[];
  membranes: SemanticMembrane[];
  files: CoreFile[];
  defaultSelection: string;
  semanticAvailable: boolean;
  external?: Array<{ from: string; to: string; calls: number }>;
};

const ROW = { u3: 108, u2: 220, u1: 338, focus: 470, d1: 602, d2: 714, d3: 822 } as const;

// The reviewed round-two fixture, adapted into the standalone view shape. Every
// value remains authored design data; it is never presented as an index read.
const fixtureNodes: SemanticNode[] = [
  n("dispatch", "dispatch_tool_call", "method", "hooks/session", "crates/tracedecay-hooks/src/session/mod.rs", 84, 132, 31, 205, ROW.u3, -3),
  n("replay", "replay_transcript", "method", "hooks/session", "crates/tracedecay-hooks/src/session/replay.rs", 44, 106, 18, 430, ROW.u3, -3),
  n("mount", "mount_graph_plugin", "function", "api", "crates/tracedecay-api/src/plugins/graph.rs", 52, 95, 9, 710, ROW.u3, -3),
  n("ctest", "contributes_catalog", "test", "tool-catalog + tests", "crates/tracedecay-tool-catalog/tests/catalog.rs", 33, 71, 4, 1060, ROW.u3, -3),
  n("ctxroute", "context_route", "method", "api", "crates/tracedecay-api/src/routes/graph.rs", 112, 184, 24, 270, ROW.u2, -2),
  n("schroute", "search_route", "method", "api", "crates/tracedecay-api/src/routes/graph.rs", 190, 252, 17, 520, ROW.u2, -2),
  n("profile", "build_profile", "function", "tool-catalog + tests", "crates/tracedecay-tool-catalog/src/snapshot.rs", 74, 129, 22, 820, ROW.u2, -2),
  n("bind", "bind_surfaces", "method", "application/git", "crates/tracedecay-application/src/git.rs", 144, 207, 15, 1090, ROW.u2, -2),
  n("hctx", "handle_context_request", "function", "application/handlers", "crates/tracedecay-contracts/src/handlers.rs", 102, 268, 46, 360, ROW.u1, -1),
  n("hsearch", "handle_search_request", "function", "application/handlers", "crates/tracedecay-contracts/src/handlers.rs", 274, 392, 28, 640, ROW.u1, -1),
  n("lookup", "lookup_callable", "method", "application/retrieval", "crates/tracedecay-contracts/src/retrieval/service.rs", 96, 122, 19, 955, ROW.u1, -1),
  n("focus", "resolve_context", "method", "application/retrieval", "crates/tracedecay-contracts/src/retrieval/service.rs", 126, 232, 63, 640, ROW.focus, 0),
  n("sibgraph", "symbol_graph_for", "method", "application/retrieval", "crates/tracedecay-contracts/src/retrieval/service.rs", 238, 306, 21, 850, ROW.focus, 0),
  n("sibcall", "hydrate_callable", "method", "application/retrieval", "crates/tracedecay-contracts/src/retrieval/service.rs", 312, 368, 17, 430, ROW.focus, 0),
  n("neighbors", "neighbors_of", "function", "application/retrieval", "crates/tracedecay-contracts/src/retrieval/catalog.rs", 92, 126, 26, 275, ROW.d1, 1),
  n("assemble", "assemble_context", "method", "application/retrieval", "crates/tracedecay-contracts/src/retrieval/catalog.rs", 132, 214, 14, 525, ROW.d1, 1),
  n("evaluate", "evaluate_decision", "function", "policy", "crates/tracedecay-policy/src/authorization/decision.rs", 102, 208, 20, 775, ROW.d1, 1),
  n("fetchn", "fetch_nodes", "method", "store + rusqlite", "crates/tracedecay-store/src/repository/sqlite.rs", 144, 262, 33, 995, ROW.d1, 1),
  n("fetche", "fetch_edges", "method", "store + rusqlite", "crates/tracedecay-store/src/repository/sqlite.rs", 268, 372, 29, 1160, ROW.d1, 1),
  n("adjacency", "adjacency_of", "function", "domain/graph", "crates/tracedecay-domain/src/graph/mod.rs", 92, 174, 38, 230, ROW.d2, 2),
  n("nodeview", "node_view", "struct", "domain/graph", "crates/tracedecay-domain/src/graph/mod.rs", 181, 247, 41, 505, ROW.d2, 2),
  n("rules", "rules_for", "function", "policy", "crates/tracedecay-policy/src/authorization/decision.rs", 214, 262, 12, 765, ROW.d2, 2),
  n("acquire", "acquire", "method", "store + rusqlite", "crates/tracedecay-store-runtime/src/reader.rs", 88, 156, 22, 1080, ROW.d2, 2),
  n("edgeindex", "edge_index", "struct", "domain/graph", "crates/tracedecay-domain/src/graph/mod.rs", 255, 318, 30, 230, ROW.d3, 3),
  n("checkout", "checkout", "method", "store + rusqlite", "crates/tracedecay-store-runtime/src/reader.rs", 164, 228, 11, 1080, ROW.d3, 3),
  { ...n("unresolved", "dyn ContextSource", "unresolved", "unresolved", null, null, null, 0, 505, ROW.d3, 3), unresolved: "1 authored call site · implementation outside the example graph" },
];

function n(id: string, name: string, kind: string, module: string, file: string | null, startLine: number | null, endLine: number | null, degree: number, x: number, y: number, ring: number): SemanticNode {
  return { id, name, kind, module, file, startLine, endLine, degree, x, y, ring };
}

const fixtureEdges: SemanticEdge[] = [
  e("dispatch", "ctxroute", 41, "caller"), e("replay", "schroute", 18, "caller"), e("mount", "ctxroute", 6, "caller"), e("ctest", "profile", 3, "caller"),
  e("ctxroute", "hctx", 58, "caller"), e("schroute", "hsearch", 23, "caller"), e("profile", "hctx", 9, "caller"), e("bind", "lookup", 14, "caller"),
  e("hctx", "focus", 34, "caller"), e("hsearch", "focus", 12, "caller"), e("lookup", "focus", 7, "caller"),
  e("focus", "sibgraph", 9, "inside"), e("focus", "sibcall", 5, "inside"), e("sibgraph", "neighbors", 21, "callee"),
  e("focus", "assemble", 11, "callee"), e("focus", "evaluate", 8, "callee"), e("sibcall", "fetchn", 16, "callee"), e("fetchn", "fetche", 6, "inside"),
  e("neighbors", "adjacency", 21, "callee"), e("assemble", "nodeview", 11, "callee"), e("evaluate", "rules", 8, "callee"), e("fetche", "acquire", 16, "callee"),
  e("adjacency", "edgeindex", 14, "callee"), e("acquire", "checkout", 16, "callee"), e("nodeview", "unresolved", 1, "unknown"),
];

function e(source: string, target: string, weight: number, direction: SemanticEdge["direction"]): SemanticEdge {
  return { source, target, weight, direction, kind: "call", detail: `${weight} authored call ${weight === 1 ? "site" : "sites"}` };
}

const fixtureRegions: SemanticRegion[] = [
  r("hooks/session", 170, 135, 105, 48, 92, 6, 141 / 92, 21 / 23), r("api", 430, 180, 135, 62, 179, 5, 242 / 179, null),
  r("tool-catalog + tests", 790, 128, 100, 46, 64, 4, 143 / 64, 10 / 23), r("application/handlers", 365, 365, 130, 60, 156, 4, 249 / 156, 19 / 23),
  r("application/retrieval", 680, 472, 175, 76, 214, 3, 690 / 214, 1), r("application/git", 1060, 310, 95, 46, 88, 3, 173 / 88, 8 / 23),
  r("policy", 840, 610, 105, 52, 109, 1, 200 / 109, null), r("store + rusqlite", 1070, 650, 150, 68, 234, 2, 516 / 234, null),
  r("domain/graph", 420, 700, 138, 64, 148, 0, 421 / 148, 6 / 23),
];

function r(id: string, x: number, y: number, rx: number, ry: number, mass: number, depth: number, density: number, warmth: number | null): SemanticRegion {
  return { id, label: id, x, y, rx, ry, mass, depth, density, warmth, nodeIds: fixtureNodes.filter((node) => node.module === id).map((node) => node.id) };
}

// Equal mass has equal ellipse area; row membership encodes authored dependency depth.
for (const region of fixtureRegions) {
  const band = fixtureRegions.filter(peer => peer.depth === region.depth).sort((a,b) => a.id.localeCompare(b.id));
  region.x = 100 + (band.indexOf(region) + .5) / band.length * 1200;
  region.y = 755 - region.depth / 6 * 650;
  region.rx = Math.sqrt(region.mass * 65 * 1.88 / Math.PI);
  region.ry = region.rx / 1.88;
}

// These line spans and complexities belong to the authored Core example, not a Git index.
const authoredCores: Array<{path:string; lines:number; x:number; indexedTo?:number; why?:string; symbols:Array<[string,string,number,number,number]>; internal:Array<[number,number,number]>}> = [
    { path: 'crates/tracedecay-contracts/src/retrieval/service.rs', lines: 742, x: 120, symbols: [
      ['RetrievalService', 'struct', 34, 58, 1], ['ContextRequest', 'struct', 60, 88, 1],
      ['new', 'method', 96, 118, 2], ['resolve_context', 'method', 126, 232, 14],
      ['symbol_graph_for', 'method', 238, 306, 9], ['hydrate_callable', 'method', 312, 368, 7],
      ['assemble_evidence', 'method', 374, 441, 11], ['budget_for', 'function', 448, 486, 5],
      ['RetrievalError', 'enum', 492, 524, 1], ['tests::resolves', 'test', 560, 614, 3],
      ['tests::budgets', 'test', 620, 700, 4],
    ], internal: [[3, 4, 9], [3, 5, 5], [3, 6, 6], [6, 7, 3], [9, 3, 2]] },

    { path: 'crates/tracedecay-contracts/src/retrieval/catalog.rs', lines: 486, x: 336, symbols: [
      ['CatalogEntry', 'struct', 22, 48, 1], ['Catalog', 'struct', 52, 84, 1],
      ['insert', 'method', 92, 126, 4], ['assemble_context', 'method', 132, 214, 12],
      ['prune', 'method', 220, 268, 6], ['merge_contributions', 'function', 274, 342, 8],
      ['tests::merges', 'test', 372, 436, 3],
    ], internal: [[3, 5, 4], [2, 4, 2]] },

    { path: 'crates/tracedecay-contracts/src/handlers.rs', lines: 1118, x: 552, symbols: [
      ['HandlerContext', 'struct', 44, 92, 1], ['handle_context_request', 'function', 102, 268, 18],
      ['handle_search_request', 'function', 274, 392, 13], ['handle_impact_request', 'function', 398, 506, 11],
      ['handle_health_request', 'function', 512, 588, 6], ['dispatch', 'function', 596, 742, 21],
      ['classify_error', 'function', 748, 806, 9], ['HandlerError', 'enum', 812, 868, 1],
      ['tests::dispatches', 'test', 908, 1042, 7],
    ], internal: [[5, 1, 5], [5, 2, 4], [5, 3, 3], [1, 6, 6]] },

    { path: 'crates/tracedecay-policy/src/authorization/decision.rs', lines: 394, x: 768, symbols: [
      ['Decision', 'enum', 18, 52, 1], ['DecisionInput', 'struct', 56, 94, 1],
      ['evaluate_decision', 'function', 102, 208, 16], ['explain', 'method', 214, 262, 5],
      ['redact', 'method', 268, 318, 7], ['tests::denies', 'test', 340, 382, 3],
    ], internal: [[2, 3, 3], [2, 4, 2]] },

    { path: 'crates/tracedecay-store/src/repository/sqlite.rs', lines: 903, x: 984, symbols: [
      ['SqliteRepository', 'struct', 38, 76, 1], ['open', 'method', 84, 138, 5],
      ['fetch_nodes', 'method', 144, 262, 13], ['fetch_edges', 'method', 268, 372, 12],
      ['upsert_node', 'method', 378, 468, 10], ['prune_scope', 'method', 474, 566, 9],
      ['migrate', 'function', 572, 706, 17], ['tests::round_trip', 'test', 742, 864, 6],
    ], internal: [[2, 3, 6], [1, 6, 2], [5, 3, 3]] },

    // The absence beat: banded to 331 and no further.
    { path: 'crates/tracedecay-hooks/src/session/vendor_bridge.rs', lines: 612, x: 1200,
      indexedTo: 331, why: 'extractor bailed at line 331 — unbalanced cfg block', symbols: [
      ['VendorBridge', 'struct', 26, 64, 1], ['attach', 'method', 72, 142, 6],
      ['forward_event', 'method', 148, 268, 10], ['drain', 'method', 274, 330, 4],
    ], internal: [[1, 2, 3]] },
];
const fixtureFiles: CoreFile[] = authoredCores.map((file) => {
  const symbols = file.symbols.map(([name, kind, start, end, complexity]) => ({
    id: fixtureNodes.find(node => node.file === file.path && node.name === name)?.id ?? `core:${file.path}:${name}`,
    name, kind, start, end, complexity,
  }));
  return {path:file.path, lines:file.lines, symbols, indexedTo:file.indexedTo,
    unavailableReason:file.why,
    internal:file.internal.map(([from,to,calls]) => ({from:symbols[from].id,to:symbols[to].id,calls})),
  };
});
// TRACE supplies module membership but no file paths or source spans. Only a matching authored Core
// symbol can provide one; nearby spans belonging to another symbol are not evidence.
for (const node of fixtureNodes) {
  const symbol = fixtureFiles.flatMap(file => file.symbols).find(symbol => symbol.id === node.id);
  if (!symbol) node.file = null;
  node.startLine = symbol?.start ?? null;
  node.endLine = symbol?.end ?? null;
}
const authoredExternal: Array<{from:[number,number];to:[number,number];calls:number}> = [
    { from: [0, 3], to: [1, 3], calls: 11 },
    { from: [0, 4], to: [4, 2], calls: 16 },
    { from: [2, 1], to: [0, 3], calls: 34 },
    { from: [2, 2], to: [0, 3], calls: 12 },
    { from: [0, 3], to: [3, 2], calls: 8 },
    { from: [1, 3], to: [4, 3], calls: 7 },
    { from: [5, 2], to: [2, 5], calls: 5 },
];
const external = authoredExternal.map(edge => ({
  from:fixtureFiles[edge.from[0]].symbols[edge.from[1]].id,
  to:fixtureFiles[edge.to[0]].symbols[edge.to[1]].id, calls:edge.calls,
}));

export const fixtureDataset: SemanticDataset = {
  source: "authored-fixture",
  sourceLabel: "AUTHORED DESIGN FIXTURE · 26 SYMBOLS",
  revision: "concept fixture · no repository revision",
  regions: fixtureRegions,
  nodes: fixtureNodes,
  edges: fixtureEdges,
  membranes: [
    { id: "routes", label: "impl GraphRoutes", nodeIds: ["ctxroute", "schroute"], kind: "impl" },
    { id: "retrieval", label: "impl RetrievalService", nodeIds: ["lookup", "focus", "sibgraph", "sibcall"], kind: "impl" },
    { id: "sqlite", label: "impl SqliteRepository", nodeIds: ["fetchn", "fetche"], kind: "impl" },
    { id: "context-source", label: "trait ContextSource", nodeIds: ["nodeview", "unresolved"], kind: "trait" },
  ],
  files: fixtureFiles,
  external,
  defaultSelection: "focus",
  semanticAvailable: true,
};

// Longest distance to a leaf of the SCC-condensed graph. Parallel declarations
// do not increase depth; cycles share a stratum. Never cap a valid dependency chain.
export function dependencyDepths(ids: string[], edges: Array<{source:string;target:string}>, groups: string[][]): Map<string, number> {
  const component = new Map(ids.map(id => [id,id]));
  groups.forEach(group => { const key = [...group].sort()[0]; group.forEach(id => component.set(id,key)); });
  const dependencies = new Map([...new Set(component.values())].map(id => [id,new Set<string>()]));
  for (const edge of edges) {
    const from = component.get(edge.source), to = component.get(edge.target);
    if (from !== undefined && to !== undefined && from !== to) dependencies.get(from)!.add(to);
  }
  const memo = new Map<string,number>(), visiting = new Set<string>();
  function visit(id:string):number {
    if (memo.has(id)) return memo.get(id)!;
    if (visiting.has(id)) throw new Error('Dependency components must be acyclic');
    visiting.add(id);
    const depth = Math.max(0,...[...dependencies.get(id)!].map(target => 1 + visit(target)));
    visiting.delete(id); memo.set(id,depth); return depth;
  }
  return new Map(ids.map(id => [id,visit(component.get(id)!)]));
}

export function buildSnapshotDataset(): SemanticDataset {
  const crates = atlasData.nodes.filter(node => node.kind === "crate");
  const production = atlasData.edges.filter(edge => !edge.scope.includes("dev-dependencies"));
  const depths = dependencyDepths(crates.map(crate => crate.id), production, cycleGroups);
  const maxDepth = Math.max(...depths.values(), 0);
  const bands = new Map<number, AtlasNode[]>();
  crates.forEach((crate) => { const depth = depths.get(crate.id) ?? 0; const band = bands.get(depth) ?? []; band.push(crate); bands.set(depth, band); });
  const positions = new Map<string, { x: number; y: number }>();
  for (const [depth, band] of bands) band.sort((a, b) => a.path.localeCompare(b.path)).forEach((crate, index) => positions.set(crate.id, {
    x: 100 + ((index + .5) / band.length) * 1200,
    y: 755 - (depth / Math.max(1, maxDepth)) * 650,
  }));
  const areaScale = 34;
  const churnFor = new Map(crates.map(crate => [crate.id, atlasData.churn.files.reduce((sum,file) => sum + (file.path.startsWith(`${crate.id}/`) ? file.touches : 0),0)]));
  // Warmth is relative file-touch count in the snapshot's explicit seven-day window,
  // not cyclomatic complexity, defect probability, or distinct module commits.
  const maxChurn = Math.max(1,...churnFor.values());
  const regions = crates.map((crate): SemanticRegion => {
    const depth = depths.get(crate.id) ?? 0;
    const position = positions.get(crate.id)!;
    const radius = Math.sqrt(crate.files * areaScale / Math.PI);
    const churn = churnFor.get(crate.id)!;
    const incident = production.filter((edge) => edge.source === crate.id || edge.target === crate.id).length;
    return {
      id: crate.id, label: crate.name.replace(/^tracedecay-/, ""),
      x: position.x, y: position.y,
      rx: radius * 1.24, ry: radius * .66,
      mass: crate.files, depth, density: incident,
      warmth: churn / maxChurn, nodeIds: [crate.id],
    };
  });
  const byId = new Map(regions.map((region) => [region.id, region]));
  const nodes = crates.map((crate): SemanticNode => {
    const region = byId.get(crate.id)!;
    return n(crate.id, crate.name, "crate", crate.id, crate.path, null, null, atlasData.edges.filter((edge) => edge.source === crate.id || edge.target === crate.id).length, region.x, region.y, 0);
  });
  // Keep each manifest declaration, including target/dev scopes and optionality.
  // Multiple declarations between one pair remain inspectable rather than becoming calls.
  const edges = atlasData.edges.filter((edge) => byId.has(edge.source) && byId.has(edge.target)).map((edge): SemanticEdge => ({
    source: edge.source, target: edge.target, weight: 1, kind: "manifest", direction: "callee",
    detail: `${edge.scope}${edge.optional ? " · optional" : ""} · ${edge.manifest}`,
  }));
  return {
    source: "measured-snapshot", sourceLabel: "MEASURED GIT + CARGO SNAPSHOT", revision: atlasData.revision,
    regions, nodes, edges, membranes: [], files: [], defaultSelection: nodeById.has("crates/tracedecay") ? "crates/tracedecay" : crates[0]?.id ?? "", semanticAvailable: false,
  };
}

export function atlasNodeForSemantic(dataset: SemanticDataset, id: string): AtlasNode | undefined {
  const semantic = dataset.nodes.find((node) => node.id === id);
  if (!semantic?.file) return undefined;
  return nodeById.get(semantic.file) ?? (owningCrate(semantic.file) ? nodeById.get(owningCrate(semantic.file)!) : undefined);
}
