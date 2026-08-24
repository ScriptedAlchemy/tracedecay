import { kindFamily, languageForPath } from "./types";
import type { GraphEdge, GraphNode } from "./types";

export interface GraphFilters {
  kindFilters?: ReadonlySet<string>;
  langFilters?: ReadonlySet<string>;
  dirScope?: string;
}

export interface FocusHistoryEntry {
  id: string;
  name: string;
}

export function edgeKey(edge: Pick<GraphEdge, "source" | "target" | "kind">): string {
  return `${edge.source}>${edge.target}:${edge.kind}`;
}

export function mergeNodesInto(
  previous: ReadonlyMap<string, GraphNode>,
  nodes: GraphNode[],
): Map<string, GraphNode> {
  const next = new Map(previous);
  for (const node of nodes) next.set(node.id, { ...next.get(node.id), ...node });
  return next;
}

export function mergeEdgesInto(
  previous: ReadonlyMap<string, GraphEdge>,
  edges: GraphEdge[],
): Map<string, GraphEdge> {
  const next = new Map(previous);
  for (const edge of edges) next.set(edgeKey(edge), edge);
  return next;
}

export function applyGraphFilters(
  graphNodes: Iterable<GraphNode>,
  graphEdges: Iterable<GraphEdge>,
  { kindFilters = new Set(), langFilters = new Set(), dirScope = "" }: GraphFilters = {},
) {
  const nodes: GraphNode[] = [];
  const keep = new Set<string>();
  for (const node of graphNodes) {
    if (kindFilters.size > 0 && !kindFilters.has(kindFamily(node.kind))) continue;
    if (langFilters.size > 0 && !langFilters.has(languageForPath(node.file_path))) continue;
    if (dirScope && !node.file_path.startsWith(dirScope)) continue;
    nodes.push(node);
    keep.add(node.id);
  }
  const edges: GraphEdge[] = [];
  for (const edge of graphEdges) {
    if (keep.has(edge.source) && keep.has(edge.target)) edges.push(edge);
  }
  return { nodes, edges };
}

export function deriveChipOptions(graphNodes: Iterable<GraphNode>) {
  const families = new Set<string>();
  const languages = new Set<string>();
  for (const node of graphNodes) {
    families.add(kindFamily(node.kind));
    languages.add(languageForPath(node.file_path));
  }
  return { families: [...families].sort(), languages: [...languages].sort() };
}

export function toggleStringSet(values: ReadonlySet<string>, value: string): Set<string> {
  const next = new Set(values);
  if (next.has(value)) next.delete(value);
  else next.add(value);
  return next;
}

export function appendFocusHistory(
  previous: FocusHistoryEntry[],
  node: Pick<FocusHistoryEntry, "id" | "name">,
): FocusHistoryEntry[] {
  const trimmed = previous.filter((entry) => entry.id !== node.id);
  return [...trimmed.slice(-7), { id: node.id, name: node.name }];
}
