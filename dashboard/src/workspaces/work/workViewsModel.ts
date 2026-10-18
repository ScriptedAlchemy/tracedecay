import type { WorkDagEdgeV1 } from '../../contracts/generated.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import type { WorkChannel } from './workChannel.ts';
import {
  criticalPathReading,
  graphChannel,
  graphEntryOf,
  type WorkCriticalPathReading,
  type WorkGraphReading,
} from './workGraphModel.ts';
import type { WorkTaskView } from './workProductView.ts';

/**
 * The DAG projection, derived from the reads this build can actually make.
 *
 * The DAG maps onto transit strata: the declared dependency graph of the
 * snapshot, layered by longest path over its Tarjan condensation. Two channels
 * come from the work-product graph read (`WorkGraphReadV1`, one immutable graph
 * version and the `WorkProductProjectionBundleV1` derived from it): the
 * effort-weighted critical path and the declared gating edge set. A channel is
 * never estimated to fill a gap, and a channel that goes live is live because a
 * read proved it rather than because a read arrived.
 */

/*
 * The channel vocabulary, `WorkChannel`, and its absences live in
 * `workChannel.ts`, and the work-product graph read that feeds the graph-fed
 * channels below lives in `workGraphModel.ts`. This module owns the DAG itself.
 */
// --- DAG / critical path: transit strata ------------------------------------

/**
 * One condensation component: a task, or the set of tasks that mutually depend
 * on each other.
 *
 * Cycles are condensed rather than broken, the same Tarjan discipline the code
 * strata use. A component with more than one member is a declared dependency
 * cycle, which must be drawn and captioned as an observation, it is a real
 * reading of the plan, not a rendering error.
 */
interface WorkDagComponent {
  readonly index: number;
  readonly taskIds: readonly string[];
  readonly depth: number;
}

interface WorkDagNode {
  readonly taskId: string;
  readonly title: string;
  readonly depth: number;
  readonly component: number;
  readonly cyclic: boolean;
  readonly dependencies: readonly string[];
  readonly dependents: readonly string[];
}

interface WorkDagEdge {
  readonly dependency: string;
  readonly dependent: string;
  /** Both ends sit in one condensation component, so the edge runs backward
   * against the strata it crosses. */
  readonly climb: boolean;
}

/** A declared dependency on a task the snapshot did not return. The edge is
 * real; the task at its far end is outside this page, so no stratum can hold
 * it and nothing about it may be assumed. */
interface WorkDagUnresolvedEdge {
  readonly dependency: string;
  readonly dependent: string;
}

interface WorkDagStratum {
  readonly depth: number;
  readonly components: readonly WorkDagComponent[];
}

export interface WorkDagReading {
  readonly nodes: ReadonlyMap<string, WorkDagNode>;
  readonly components: readonly WorkDagComponent[];
  readonly strata: readonly WorkDagStratum[];
  readonly edges: readonly WorkDagEdge[];
  readonly unresolved: readonly WorkDagUnresolvedEdge[];
  readonly cycles: readonly WorkDagComponent[];
  /** The deepest chain of components, which is the widest channel on the
   * transit map. UNWEIGHTED, and kept beside `effort` rather than replaced by
   * it: this chain is the longest path over the edges the snapshot returned,
   * and the effort-weighted path is the authority's own chain over the whole
   * work-product graph. They answer different questions over different
   * populations and disagreeing is informative. */
  readonly longestChain: readonly WorkDagComponent[];
  readonly widestStratum: number;
  /** The authority's effort-weighted critical path, from the graph read. */
  readonly effort: WorkChannel<WorkCriticalPathReading>;
  /** The gating edge set the work-product graph declares. Declared data: an
   * empty set is the plan gating nothing, not a measurement that failed. */
  readonly gating: WorkChannel<readonly WorkDagEdgeV1[]>;
  /** What the graph read itself answered, so the view states its state rather
   * than inferring it from an empty channel. */
  readonly graph: WorkGraphReading;
}

/**
 * Tarjan's strongly connected components, iteratively.
 *
 * Iterative rather than recursive because the depth of the recursion is the
 * depth of the dependency chain, which is data rather than something this
 * module controls, and a blown call stack would take the whole page down.
 *
 * Returns one component index per task, in reverse topological order of the
 * condensation, Tarjan's natural output order.
 */
function stronglyConnected(
  taskIds: readonly string[],
  edgesFrom: ReadonlyMap<string, readonly string[]>,
): ReadonlyMap<string, number> {
  const index = new Map<string, number>();
  const low = new Map<string, number>();
  const onStack = new Set<string>();
  const stack: string[] = [];
  const component = new Map<string, number>();
  let counter = 0;
  let components = 0;

  for (const root of taskIds) {
    if (index.has(root)) continue;
    // Each frame is a task plus how many of its successors have been walked.
    const frames: { task: string; next: number }[] = [{ task: root, next: 0 }];
    index.set(root, counter);
    low.set(root, counter);
    counter += 1;
    stack.push(root);
    onStack.add(root);

    while (frames.length > 0) {
      const frame = frames[frames.length - 1];
      if (frame === undefined) break;
      const successors = edgesFrom.get(frame.task) ?? [];
      if (frame.next < successors.length) {
        const successor = successors[frame.next];
        frame.next += 1;
        if (successor === undefined) continue;
        if (!index.has(successor)) {
          index.set(successor, counter);
          low.set(successor, counter);
          counter += 1;
          stack.push(successor);
          onStack.add(successor);
          frames.push({ task: successor, next: 0 });
        } else if (onStack.has(successor)) {
          low.set(
            frame.task,
            Math.min(low.get(frame.task) ?? 0, index.get(successor) ?? 0),
          );
        }
        continue;
      }

      frames.pop();
      const parent = frames[frames.length - 1];
      if (parent !== undefined) {
        low.set(
          parent.task,
          Math.min(low.get(parent.task) ?? 0, low.get(frame.task) ?? 0),
        );
      }
      if (low.get(frame.task) === index.get(frame.task)) {
        for (;;) {
          const member = stack.pop();
          if (member === undefined) break;
          onStack.delete(member);
          component.set(member, components);
          if (member === frame.task) break;
        }
        components += 1;
      }
    }
  }

  return component;
}

/**
 * The declared task graph, condensed and layered.
 *
 * Depth is the longest path over the condensation, so a task sits one stratum
 * below the deepest thing it depends on. Every member of a cycle shares its
 * component's depth, because a cycle has no internal order to layer by.
 *
 * The graph argument defaults to `pending` so a caller that has not issued the
 * read draws its two channels as not-yet-answered rather than as absent.
 */
export function workDagReading(
  projections: readonly WorkTaskView[],
  graph: WorkGraphReading = { state: 'pending' },
): WorkDagReading {
  const entry = graphEntryOf(graph);
  const titles = new Map(projections.map((p) => [p.task_id, p.title]));
  const taskIds = projections.map((p) => p.task_id);

  const resolvedEdges: WorkDagEdge[] = [];
  const unresolved: WorkDagUnresolvedEdge[] = [];
  const dependentsOf = new Map<string, string[]>(taskIds.map((id) => [id, []]));
  const dependenciesOf = new Map<string, string[]>(taskIds.map((id) => [id, []]));
  for (const projection of projections) {
    for (const dependency of projection.dependencies) {
      if (!titles.has(dependency)) {
        unresolved.push({ dependency, dependent: projection.task_id });
        continue;
      }
      dependentsOf.get(dependency)?.push(projection.task_id);
      dependenciesOf.get(projection.task_id)?.push(dependency);
    }
  }

  const componentOf = stronglyConnected(taskIds, dependentsOf);
  const members = new Map<number, string[]>();
  for (const taskId of taskIds) {
    const index = componentOf.get(taskId) ?? 0;
    const bucket = members.get(index);
    if (bucket === undefined) members.set(index, [taskId]);
    else bucket.push(taskId);
  }

  // Tarjan completes a component only after everything reachable from it, so
  // it emits the condensation in REVERSE topological order along the
  // dependency-to-dependent edges it walked. Descending index is therefore
  // topological order, and one pass down it visits every dependency before the
  // thing that depends on it, which is what lets a single sweep settle the
  // longest path.
  const ordered = [...members.keys()].sort((a, b) => b - a);
  const depthOf = new Map<number, number>();
  const cameFrom = new Map<number, number>();
  for (const index of ordered) {
    let depth = 0;
    let predecessor: number | undefined;
    for (const taskId of members.get(index) ?? []) {
      for (const dependency of dependenciesOf.get(taskId) ?? []) {
        const from = componentOf.get(dependency);
        if (from === undefined || from === index) continue;
        const candidate = (depthOf.get(from) ?? 0) + 1;
        if (candidate > depth) {
          depth = candidate;
          predecessor = from;
        }
      }
    }
    depthOf.set(index, depth);
    if (predecessor !== undefined) cameFrom.set(index, predecessor);
  }

  // Ordered for rendering rather than for the sweep above: by stratum, then by
  // the first task in each component, so the drawing is stable across reads
  // that return the same graph.
  const components: WorkDagComponent[] = ordered
    .map((index) => ({
      index,
      taskIds: [...(members.get(index) ?? [])].sort(),
      depth: depthOf.get(index) ?? 0,
    }))
    .sort(
      (a, b) => a.depth - b.depth || (a.taskIds[0] ?? '').localeCompare(b.taskIds[0] ?? ''),
    );
  const byIndex = new Map(components.map((component) => [component.index, component]));

  const nodes = new Map<string, WorkDagNode>();
  for (const projection of projections) {
    const index = componentOf.get(projection.task_id) ?? 0;
    const component = byIndex.get(index);
    nodes.set(projection.task_id, {
      taskId: projection.task_id,
      title: projection.title,
      depth: component?.depth ?? 0,
      component: index,
      cyclic: (component?.taskIds.length ?? 1) > 1,
      dependencies: [...(dependenciesOf.get(projection.task_id) ?? [])].sort(),
      dependents: [...(dependentsOf.get(projection.task_id) ?? [])].sort(),
    });
  }

  for (const projection of projections) {
    for (const dependency of dependenciesOf.get(projection.task_id) ?? []) {
      resolvedEdges.push({
        dependency,
        dependent: projection.task_id,
        climb: componentOf.get(dependency) === componentOf.get(projection.task_id),
      });
    }
  }

  const byDepth = new Map<number, WorkDagComponent[]>();
  for (const component of components) {
    const bucket = byDepth.get(component.depth);
    if (bucket === undefined) byDepth.set(component.depth, [component]);
    else bucket.push(component);
  }
  const strata: WorkDagStratum[] = [...byDepth.keys()]
    .sort((a, b) => a - b)
    .map((depth) => ({ depth, components: byDepth.get(depth) ?? [] }));

  let deepest: WorkDagComponent | undefined;
  for (const component of components) {
    if (deepest === undefined || component.depth > deepest.depth) deepest = component;
  }
  const longestChain: WorkDagComponent[] = [];
  for (let cursor = deepest; cursor !== undefined; ) {
    longestChain.unshift(cursor);
    const previous = cameFrom.get(cursor.index);
    cursor = previous === undefined ? undefined : byIndex.get(previous);
  }

  return {
    nodes,
    components,
    strata,
    edges: resolvedEdges,
    unresolved,
    cycles: components.filter((component) => component.taskIds.length > 1),
    longestChain,
    widestStratum: strata.reduce((widest, s) => Math.max(widest, s.components.length), 0),
    effort: graphChannel(
      graph,
      'the effort-weighted critical path',
      entry === null ? null : criticalPathReading(entry),
    ),
    gating: graphChannel(
      graph,
      'the declared gating edge set',
      entry === null ? null : entry.projections.dag.gating_edges,
    ),
    graph,
  };
}

