/**
 * Pure helpers for the canvas default view (the seedless "project hubs"
 * slice loaded on tab entry) and the canvas placeholder copy.
 */

/**
 * Node/edge budget for the default view: large enough to be informative,
 * comfortably inside the canvas soft cap and the simulation's 60fps budget.
 */
export const DEFAULT_VIEW_LIMITS = { limit_nodes: 100, limit_edges: 240 } as const;

export interface EmptyStateInput {
  /** Total nodes in the indexed graph (null while the overview is loading). */
  indexedNodes: number | null;
  /** Nodes currently accumulated in the canvas (before filtering). */
  loadedNodes: number;
  /** True while the default view or an expansion request is in flight. */
  loading: boolean;
}

/**
 * Copy for the canvas placeholder, shown only when no nodes are visible.
 * A truly empty index is the only state that asks the user to go index;
 * everything else is either filters hiding loaded nodes or a load in flight.
 */
export function canvasEmptyMessage({ indexedNodes, loadedNodes, loading }: EmptyStateInput): string {
  if (indexedNodes === 0) {
    return "Nothing is indexed yet — run `tracedecay init` in this project (or `tracedecay sync` to refresh an existing index), then reload.";
  }
  if (loadedNodes > 0) {
    return "All loaded nodes are hidden by the current filters.";
  }
  if (loading || indexedNodes === null) {
    return "Loading the project graph…";
  }
  return "The canvas is empty — search a symbol above or pick one from Overview.";
}
