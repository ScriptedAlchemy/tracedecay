import type { WorkTaskView } from './workProductView.ts';
import type { WorkDagReading } from './workViewsModel.ts';

/**
 * Deterministic hierarchical layout for the Work dependency graph.
 *
 * Pure and renderer-independent: the same reading and the same options yield
 * the same coordinates on every reload, which is what lets the DOM cards and
 * the SVG relation layer agree without either measuring the other. No force
 * simulation is involved. Depth comes from `workDagReading`'s longest-path
 * strata; order within a stratum comes from one downward barycenter sweep
 * over the gating edges, with the task identity as the tie-break.
 *
 * Three relation kinds are laid out and they are drawn differently on
 * purpose, because they are different claims the plan makes:
 *
 *   gating          `dependencies` — the hard edge. It is the edge the strata
 *                   were layered by, so it always runs downward except inside
 *                   a declared cycle.
 *   informational   `informational_relations` — a named soft relation that
 *                   gates nothing. It may run in any direction.
 *   causal          `causal_candidates` — a nominated possible cause. Also
 *                   soft, also directional (cause → the task nominating it).
 *
 * A relation whose far end the page did not return is reported in
 * `unresolved` rather than drawn to nowhere or dropped.
 */

export type WorkDagRelationKind = 'gating' | 'informational' | 'causal';

export interface WorkDagLayoutOptions {
  readonly cardWidth: number;
  readonly cardHeight: number;
  readonly columnGap: number;
  readonly rowGap: number;
  readonly padding: number;
}

export const WORK_DAG_LAYOUT_DEFAULTS: WorkDagLayoutOptions = {
  cardWidth: 208,
  cardHeight: 84,
  columnGap: 32,
  rowGap: 64,
  padding: 24,
};

export interface WorkDagLayoutNode {
  readonly taskId: string;
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
  readonly depth: number;
  /** Position within the stratum, left to right. */
  readonly column: number;
  readonly component: number;
  readonly cyclic: boolean;
}

export interface WorkDagLayoutEdge {
  readonly id: string;
  /** The dependency, related task, or nominated cause. */
  readonly from: string;
  /** The dependent, relating task, or nominating task. */
  readonly to: string;
  readonly kind: WorkDagRelationKind;
  /** Both ends share a condensation component: the edge runs against the
   * strata rather than down them. Only a gating edge can climb. */
  readonly climb: boolean;
  /** SVG path data in layout coordinates. */
  readonly path: string;
}

export interface WorkDagUnresolvedRelation {
  readonly from: string;
  readonly to: string;
  readonly kind: WorkDagRelationKind;
}

export interface WorkDagLayoutStratum {
  readonly depth: number;
  readonly y: number;
  readonly taskIds: readonly string[];
}

export interface WorkDagLayout {
  /** Every laid-out task, ordered by depth then column — the reading order. */
  readonly nodes: readonly WorkDagLayoutNode[];
  readonly byId: ReadonlyMap<string, WorkDagLayoutNode>;
  readonly edges: readonly WorkDagLayoutEdge[];
  readonly strata: readonly WorkDagLayoutStratum[];
  readonly unresolved: readonly WorkDagUnresolvedRelation[];
  readonly width: number;
  readonly height: number;
  readonly options: WorkDagLayoutOptions;
}

interface StratumComponent {
  readonly index: number;
  readonly taskIds: readonly string[];
}

/** Mean of the given columns, or `null` for a component with no laid-out
 * predecessor — those sort after every anchored one. */
function barycenter(columns: readonly number[]): number | null {
  if (columns.length === 0) return null;
  return columns.reduce((sum, column) => sum + column, 0) / columns.length;
}

function orderStratum(
  components: readonly StratumComponent[],
  reading: WorkDagReading,
  columnOf: ReadonlyMap<string, number>,
): readonly StratumComponent[] {
  const keyed = components.map((component) => {
    const predecessorColumns: number[] = [];
    for (const taskId of component.taskIds) {
      const node = reading.nodes.get(taskId);
      for (const dependency of node?.dependencies ?? []) {
        const column = columnOf.get(dependency);
        if (column !== undefined) predecessorColumns.push(column);
      }
    }
    return { component, center: barycenter(predecessorColumns) };
  });
  keyed.sort((a, b) => {
    if (a.center === null && b.center === null) {
      return (a.component.taskIds[0] ?? '').localeCompare(b.component.taskIds[0] ?? '');
    }
    if (a.center === null) return 1;
    if (b.center === null) return -1;
    return (
      a.center - b.center ||
      (a.component.taskIds[0] ?? '').localeCompare(b.component.taskIds[0] ?? '')
    );
  });
  return keyed.map((entry) => entry.component);
}

/** An orthogonal three-segment path for an edge that runs down the strata. */
function downwardPath(
  fromX: number,
  fromY: number,
  toX: number,
  toY: number,
): string {
  const midY = fromY + (toY - fromY) / 2;
  if (Math.abs(fromX - toX) < 0.5) {
    return `M ${fromX} ${fromY} L ${toX} ${toY}`;
  }
  return `M ${fromX} ${fromY} L ${fromX} ${midY} L ${toX} ${midY} L ${toX} ${toY}`;
}

/** A cubic curve for an edge that does not run down the strata — a climb
 * inside a cycle, or a soft relation between peers or running upward. Curved
 * rather than orthogonal so it cannot be mistaken for a gating edge even in a
 * monochrome rendering. */
function curvedPath(
  fromX: number,
  fromY: number,
  toX: number,
  toY: number,
  reach: number,
): string {
  const bend = Math.max(reach, Math.abs(toY - fromY) / 2);
  return `M ${fromX} ${fromY} C ${fromX + bend} ${fromY}, ${toX + bend} ${toY}, ${toX} ${toY}`;
}

function edgePath(
  from: WorkDagLayoutNode,
  to: WorkDagLayoutNode,
  kind: WorkDagRelationKind,
  options: WorkDagLayoutOptions,
): string {
  const downward = to.depth > from.depth;
  if (downward && kind === 'gating') {
    return downwardPath(
      from.x + from.width / 2,
      from.y + from.height,
      to.x + to.width / 2,
      to.y,
    );
  }
  if (downward) {
    // Soft relations that happen to run downward leave from the right third
    // of the card so they never overprint the gating edge on the centre line.
    return downwardPath(
      from.x + (from.width * 3) / 4,
      from.y + from.height,
      to.x + (to.width * 3) / 4,
      to.y,
    );
  }
  return curvedPath(
    from.x + from.width,
    from.y + from.height / 2,
    to.x + to.width,
    to.y + to.height / 2,
    options.columnGap,
  );
}

export function workDagLayout(
  reading: WorkDagReading,
  projections: readonly WorkTaskView[],
  options: WorkDagLayoutOptions = WORK_DAG_LAYOUT_DEFAULTS,
): WorkDagLayout {
  const columnOf = new Map<string, number>();
  const orderedStrata: { depth: number; taskIds: string[] }[] = [];

  for (const stratum of reading.strata) {
    const components: StratumComponent[] = stratum.components.map((component) => ({
      index: component.index,
      taskIds: component.taskIds,
    }));
    const ordered = orderStratum(components, reading, columnOf);
    const taskIds: string[] = [];
    for (const component of ordered) {
      for (const taskId of component.taskIds) {
        columnOf.set(taskId, taskIds.length);
        taskIds.push(taskId);
      }
    }
    orderedStrata.push({ depth: stratum.depth, taskIds });
  }

  const widest = orderedStrata.reduce((max, stratum) => Math.max(max, stratum.taskIds.length), 0);
  const pitchX = options.cardWidth + options.columnGap;
  const pitchY = options.cardHeight + options.rowGap;
  const width =
    widest === 0
      ? options.padding * 2
      : options.padding * 2 + widest * options.cardWidth + (widest - 1) * options.columnGap;
  const height =
    orderedStrata.length === 0
      ? options.padding * 2
      : options.padding * 2 +
        orderedStrata.length * options.cardHeight +
        (orderedStrata.length - 1) * options.rowGap;

  const nodes: WorkDagLayoutNode[] = [];
  const byId = new Map<string, WorkDagLayoutNode>();
  const strata: WorkDagLayoutStratum[] = [];
  orderedStrata.forEach((stratum, row) => {
    const rowWidth =
      stratum.taskIds.length * options.cardWidth +
      Math.max(0, stratum.taskIds.length - 1) * options.columnGap;
    // Each stratum is centred, so a narrow row hangs under the middle of a
    // wide one the way the concept plate reads, and the centring is arithmetic
    // on the layout width rather than a measurement of the rendered box.
    const offsetX = options.padding + (width - options.padding * 2 - rowWidth) / 2;
    const y = options.padding + row * pitchY;
    strata.push({ depth: stratum.depth, y, taskIds: stratum.taskIds });
    stratum.taskIds.forEach((taskId, column) => {
      const source = reading.nodes.get(taskId);
      const node: WorkDagLayoutNode = {
        taskId,
        x: offsetX + column * pitchX,
        y,
        width: options.cardWidth,
        height: options.cardHeight,
        depth: stratum.depth,
        column,
        component: source?.component ?? -1,
        cyclic: source?.cyclic ?? false,
      };
      nodes.push(node);
      byId.set(taskId, node);
    });
  });

  const edges: WorkDagLayoutEdge[] = [];
  const unresolved: WorkDagUnresolvedRelation[] = [];
  const place = (from: string, to: string, kind: WorkDagRelationKind, climb: boolean) => {
    const source = byId.get(from);
    const target = byId.get(to);
    if (source === undefined || target === undefined) {
      unresolved.push({ from, to, kind });
      return;
    }
    edges.push({
      id: `${kind}:${from}->${to}`,
      from,
      to,
      kind,
      climb,
      path: edgePath(source, target, kind, options),
    });
  };

  for (const edge of reading.edges) {
    place(edge.dependency, edge.dependent, 'gating', edge.climb);
  }
  for (const projection of projections) {
    for (const related of projection.informational_relations) {
      place(projection.task_id, related, 'informational', false);
    }
    for (const cause of projection.causal_candidates) {
      place(cause, projection.task_id, 'causal', false);
    }
  }
  edges.sort((a, b) => a.id.localeCompare(b.id));
  unresolved.sort(
    (a, b) => a.kind.localeCompare(b.kind) || a.from.localeCompare(b.from) || a.to.localeCompare(b.to),
  );

  return { nodes, byId, edges, strata, unresolved, width, height, options };
}

/** The tasks and relations one hop from `taskId`, over every drawn relation
 * kind. This is what a hover or focus isolates; it never mutates selection. */
export function dagNeighborhood(
  layout: WorkDagLayout,
  taskId: string,
): { readonly tasks: ReadonlySet<string>; readonly edges: ReadonlySet<string> } {
  const tasks = new Set<string>([taskId]);
  const edges = new Set<string>();
  for (const edge of layout.edges) {
    if (edge.from === taskId || edge.to === taskId) {
      edges.add(edge.id);
      tasks.add(edge.from);
      tasks.add(edge.to);
    }
  }
  return { tasks, edges };
}

/** Every task reachable from `taskId` by walking gating edges upstream (the
 * work this task waits on), plus the edges walked. Declared data only. */
export function dagPathToRoot(
  reading: WorkDagReading,
  taskId: string,
): { readonly tasks: ReadonlySet<string>; readonly edges: ReadonlySet<string> } {
  return walk(reading, taskId, 'dependencies');
}

/** Every task reachable from `taskId` by walking gating edges downstream (the
 * work that waits on this task), plus the edges walked. Declared data only. */
export function dagPathToOutcome(
  reading: WorkDagReading,
  taskId: string,
): { readonly tasks: ReadonlySet<string>; readonly edges: ReadonlySet<string> } {
  return walk(reading, taskId, 'dependents');
}

function walk(
  reading: WorkDagReading,
  start: string,
  direction: 'dependencies' | 'dependents',
): { tasks: ReadonlySet<string>; edges: ReadonlySet<string> } {
  const tasks = new Set<string>([start]);
  const edges = new Set<string>();
  const queue = [start];
  for (let cursor = queue.shift(); cursor !== undefined; cursor = queue.shift()) {
    const node = reading.nodes.get(cursor);
    if (node === undefined) continue;
    for (const next of node[direction]) {
      const edgeId =
        direction === 'dependencies'
          ? `gating:${next}->${cursor}`
          : `gating:${cursor}->${next}`;
      edges.add(edgeId);
      if (tasks.has(next)) continue;
      tasks.add(next);
      queue.push(next);
    }
  }
  return { tasks, edges };
}
