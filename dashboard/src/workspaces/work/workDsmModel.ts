import type { WorkDagLayout, WorkDagRelationKind } from './workDagLayout.ts';

/**
 * The dependency structure matrix over one layered layout.
 *
 * Both axes are the layered board's reading order (stratum, then position in
 * the stratum), which is a topological order of the condensation. A row is
 * the task that declares the relation, a column the task it names: a gating
 * cell at (row, column) reads "row needs column". In that order every gating
 * edge of an acyclic plan falls below the diagonal, so a gating cell above it
 * is a back-edge, which can only happen inside a declared cycle, and a climb
 * the layout already flagged is one wherever it lands. Soft relations may sit
 * on either side; they gate nothing and are never called back-edges.
 */

export interface DsmCell {
  readonly id: string;
  readonly row: number;
  readonly column: number;
  readonly kind: WorkDagRelationKind;
  /** A gating cell above the diagonal, or one the layout marked as a climb. */
  readonly back: boolean;
}

export interface DsmBlock {
  readonly depth: number;
  readonly start: number;
  readonly end: number;
}

export interface DsmModel {
  readonly order: readonly string[];
  readonly index: ReadonlyMap<string, number>;
  readonly cells: readonly DsmCell[];
  /** Consecutive runs of one stratum along the diagonal. */
  readonly blocks: readonly DsmBlock[];
  readonly backEdges: number;
}

export function workDsm(layout: WorkDagLayout): DsmModel {
  const order = layout.nodes.map((node) => node.taskId);
  const index = new Map(order.map((taskId, position) => [taskId, position]));
  const cells: DsmCell[] = [];
  for (const edge of layout.edges) {
    const row = index.get(edge.to);
    const column = index.get(edge.from);
    if (row === undefined || column === undefined) continue;
    cells.push({
      id: edge.id,
      row,
      column,
      kind: edge.kind,
      back: edge.kind === 'gating' && (edge.climb || column > row),
    });
  }
  cells.sort((a, b) => a.row - b.row || a.column - b.column || a.kind.localeCompare(b.kind));

  const blocks: DsmBlock[] = [];
  layout.nodes.forEach((node, position) => {
    const last = blocks[blocks.length - 1];
    if (last !== undefined && last.depth === node.depth) {
      blocks[blocks.length - 1] = { ...last, end: position };
    } else {
      blocks.push({ depth: node.depth, start: position, end: position });
    }
  });

  return { order, index, cells, blocks, backEdges: cells.filter((cell) => cell.back).length };
}

/** The next active row for a listbox key, or null when the key is not one. */
export function dsmStep(key: string, active: number | null, count: number): number | null {
  if (count === 0) return null;
  const from = active ?? -1;
  switch (key) {
    case 'ArrowDown':
      return Math.min(count - 1, from + 1);
    case 'ArrowUp':
      return Math.max(0, from < 0 ? 0 : from - 1);
    case 'Home':
      return 0;
    case 'End':
      return count - 1;
    case 'PageDown':
      return Math.min(count - 1, Math.max(0, from) + 10);
    case 'PageUp':
      return Math.max(0, from - 10);
    default:
      return null;
  }
}
