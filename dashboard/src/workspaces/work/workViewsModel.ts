import type { WorkDagEdgeV1, WorkProjection } from '../../contracts/index.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import {
  attemptPageOf,
  type WorkAttemptLineage,
  type WorkAttemptReading,
  type WorkCancellationLadder,
  type WorkExecutorReading,
  type WorkTerminalObservation,
} from './workAttemptModel.ts';
import { absentChannel, type WorkChannel } from './workChannel.ts';
import {
  WORK_CHURN_WINDOW_MICROS,
  churnReading,
  concurrencyReading,
  criticalPathReading,
  effortSplit,
  graphChannel,
  graphChannelGap,
  graphEntryOf,
  runtimeReading,
  timelineInstants,
  type WorkChurnReading,
  type WorkConcurrencyReading,
  type WorkCriticalPathReading,
  type WorkEffortMassReading,
  type WorkGraphReading,
  type WorkRuntimeReading,
  type WorkTimelineInstantReading,
} from './workGraphModel.ts';

/**
 * The four Work projections of plan 11c, derived from the reads this build can
 * actually make.
 *
 * Plan 11c maps each projection onto a grammar the app has already proved:
 * the DAG onto transit strata, the timeline onto the loom weave, the causal
 * view onto the disagreement field, and the workload onto cortex aggregation.
 * The data arrives from three reads, and which read a channel comes from is the
 * thing to keep straight:
 *
 *   the snapshot        `WorkProjection` — the declared dependency graph, its
 *                       strata and cycles, the run/task incidence, the evidence
 *                       each task carries, which tasks no run has touched
 *   the attempt list    `WorkAttemptV1` — the execution record: who ran what
 *                       (`requested_route`/`actual_route`), the retry chains,
 *                       the typed cancellation ladder, and the instant each
 *                       terminated attempt was observed to finish
 *   the graph read      `WorkGraphReadV1` — one immutable work-product graph
 *                       version and the whole `WorkProductProjectionBundleV1`
 *                       derived from that same version: declared effort and the
 *                       effort-weighted critical path, the gating edge set, the
 *                       declared causal candidates, the per-task timeline
 *                       instants, the workload figures, and the live runtime
 *                       attempt projection with its own coverage
 *
 * All three are mounted. The graph read is `operation.work.views`, and it is
 * what closed 11c's effort, concurrency and churn gaps: they were absent
 * because no route carried `WorkProductProjectionBundleV1`, and one does now.
 *
 * TWO ABSENCES SURVIVE THE MOUNT, and they are the interesting ones, because a
 * new read is exactly the moment a gap is most likely to be quietly filled with
 * something adjacent.
 *
 *   wall clock       the attempt record has an end and no start.
 *                    `WorkLeaseFenceV1` is `{epoch, lease_id}` and
 *                    `WorkAttemptProgressV1` is `{completed, total}` — neither
 *                    is a clock — so a terminated attempt yields an instant and
 *                    never a width. The bundle does not change that: its
 *                    runtime projection carries an attempt's identity and its
 *                    STATE, and no instant at all.
 *   observed order   the causal projection's `candidate_edges` are DECLARED
 *                    data — what the plan nominated as a possible cause — and
 *                    the graph read carries no execution order to hold them
 *                    against. An empty candidate set means nobody declared one.
 *                    It must never be filled in from the order things were
 *                    observed to finish, which is a different question with its
 *                    own honest ceiling in the weave's `observedOrder`.
 *
 * A channel is never estimated to fill a gap, and a channel that goes live is
 * live because a read proved it rather than because a read arrived. 11c is
 * explicit: attempt counts, queue ages and wall-times are real or absent, and a
 * degenerate distribution is said rather than drawn.
 */

/*
 * The channel vocabulary — `WorkChannel`, the two surviving `WorkChannelGap`
 * absences and their sentences — lives in `workChannel.ts`, and the
 * work-product graph read that feeds the graph-fed channels below lives in
 * `workGraphModel.ts`. This module owns the four projections themselves.
 */
// --- DAG / critical path: transit strata ------------------------------------

/**
 * One condensation component: a task, or the set of tasks that mutually depend
 * on each other.
 *
 * Cycles are condensed rather than broken, the same Tarjan discipline the code
 * strata use. A component with more than one member is a declared dependency
 * cycle, which 11c says to draw and caption as an observation — it is a real
 * reading of the plan, not a rendering error.
 */
export interface WorkDagComponent {
  readonly index: number;
  readonly taskIds: readonly string[];
  readonly depth: number;
}

export interface WorkDagNode {
  readonly taskId: string;
  readonly title: string;
  readonly depth: number;
  readonly component: number;
  readonly cyclic: boolean;
  readonly dependencies: readonly string[];
  readonly dependents: readonly string[];
}

export interface WorkDagEdge {
  readonly dependency: string;
  readonly dependent: string;
  /** Both ends sit in one condensation component, so the edge runs backward
   * against the strata it crosses. */
  readonly climb: boolean;
}

/** A declared dependency on a task the snapshot did not return. The edge is
 * real; the task at its far end is outside this page, so no stratum can hold
 * it and nothing about it may be assumed. */
export interface WorkDagUnresolvedEdge {
  readonly dependency: string;
  readonly dependent: string;
}

export interface WorkDagStratum {
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
 * condensation — Tarjan's natural output order.
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
  projections: readonly WorkProjection[],
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
  // thing that depends on it — which is what lets a single sweep settle the
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

// --- Timeline / attempts: loom weave ----------------------------------------

/** One crossing of a run over a task. `crossings` counts the evidence records
 * the run attached to that task — a repeated crossing of the same landing,
 * which is the weave's rendering of a retry. */
export interface WorkWeaveLanding {
  readonly taskId: string;
  readonly title: string;
  readonly crossings: number;
  readonly terminal: boolean;
}

export interface WorkWeaveThread {
  readonly runId: string;
  readonly landings: readonly WorkWeaveLanding[];
  readonly crossings: number;
  readonly terminalLandings: number;
}

export interface WorkWeaveReading {
  readonly threads: readonly WorkWeaveThread[];
  /** Tasks no run has landed on. 11c requires these to occupy an explicit
   * band rather than being omitted from the weave. */
  readonly unwoven: readonly { readonly taskId: string; readonly title: string }[];
  readonly crossings: number;
  readonly wallClock: WorkChannel<never>;
  readonly executorIdentity: WorkChannel<readonly WorkExecutorReading[]>;
  readonly observedOrder: WorkChannel<readonly WorkTerminalObservation[]>;
  readonly retryWeave: WorkChannel<readonly WorkAttemptLineage[]>;
  readonly cancellationLadder: WorkChannel<WorkCancellationLadder>;
  /**
   * The instants the work-product graph records per task: when it was created,
   * when it last changed, when it is scheduled for, and what it is due by.
   *
   * Four instants, and still no span. These place a task on a calendar; they do
   * not say how long any attempt at it took, which is why `wallClock` sits
   * beside them and stays absent.
   */
  readonly instants: WorkChannel<readonly WorkTimelineInstantReading[]>;
  /** What the attempt read itself answered, so the view states the page's
   * coverage and its refusals rather than inferring them from empty channels. */
  readonly attempts: WorkAttemptReading;
  /** What the graph read answered, for the same reason. */
  readonly graph: WorkGraphReading;
}

/**
 * Why a channel the attempt list would have supplied has no value.
 *
 * Distinct from `channelGap` on purpose. These are not schema absences — the
 * contract carries the measurement — so reporting them as `unsupported_schema`
 * would tell a reader the build cannot do something it can. Each one is the
 * state the read actually returned, in that state's own words.
 */
export function attemptChannelGap(
  reading: WorkAttemptReading,
  measure: string,
): WorkChannel<never> {
  switch (reading.state) {
    case 'pending':
      return {
        available: false,
        state: 'loading',
        detail: `the attempt list has not answered yet, so ${measure} is not drawn`,
      };
    case 'refused':
      return {
        available: false,
        state: reading.chip,
        detail: `${measure} is read from the attempt list, and that read was refused: ${reading.detail}`,
      };
    case 'absent':
      return {
        available: false,
        state: 'denied',
        detail: `the daemon reports no Work attempts in this scope — a typed absence its policy makes indistinguishable from a denial, so ${measure} is neither drawn nor guessed`,
      };
    case 'listed':
      return {
        available: false,
        state: 'complete_zero_findings',
        detail: `the attempt page was read and no attempt on it records ${measure}`,
      };
    default: {
      const unhandled: never = reading;
      return unhandled;
    }
  }
}

/** A channel that is present exactly when the page proved at least one row of
 * it, and otherwise carries the read's own reason. */
function attemptChannel<T>(
  reading: WorkAttemptReading,
  measure: string,
  rows: readonly T[] | undefined,
): WorkChannel<readonly T[]> {
  if (rows === undefined || rows.length === 0) return attemptChannelGap(reading, measure);
  return { available: true, value: rows };
}

/**
 * The run/task weave, with the execution record bound onto it.
 *
 * Warp threads stay runs and landings stay tasks: that incidence is the
 * snapshot's reading and the attempt list does not replace it. What the attempt
 * list replaces is the inference around it. A thread's executor is now read
 * from `actual_route` instead of being refused, and a retry is now a link in a
 * recovery chain instead of a second evidence row that merely looked like one —
 * `retryWeave` is the measured version of `WorkWeaveLanding.crossings`, and the
 * two are kept side by side rather than one overwriting the other, because they
 * count different things and disagreeing is informative.
 *
 * Every mark stays hollow. The page brought an end instant and no start, so the
 * weave gained `observedOrder` and did not gain a width; `wallClock` still says
 * so. The graph read does not change that either — it brought four calendar
 * instants per task and no duration anywhere — which is exactly why `instants`
 * and `wallClock` are two channels and not one.
 *
 * Both read arguments default to `pending` so a caller that has not issued a
 * read draws its channels as not-yet-answered rather than as absent.
 */
export function workWeaveReading(
  projections: readonly WorkProjection[],
  attempts: WorkAttemptReading = { state: 'pending' },
  graph: WorkGraphReading = { state: 'pending' },
): WorkWeaveReading {
  const threads = new Map<string, Map<string, { crossings: number; terminal: boolean }>>();
  const unwoven: { taskId: string; title: string }[] = [];

  for (const projection of projections) {
    if (projection.runtime_evidence.length === 0) {
      unwoven.push({ taskId: projection.task_id, title: projection.title });
      continue;
    }
    for (const evidence of projection.runtime_evidence) {
      const landings = threads.get(evidence.run_id) ?? new Map();
      threads.set(evidence.run_id, landings);
      const landing = landings.get(projection.task_id);
      landings.set(projection.task_id, {
        crossings: (landing?.crossings ?? 0) + 1,
        terminal: (landing?.terminal ?? false) || evidence.terminal,
      });
    }
  }

  const titles = new Map(projections.map((p) => [p.task_id, p.title]));
  const woven: WorkWeaveThread[] = [...threads.entries()]
    .map(([runId, landings]) => {
      const rows: WorkWeaveLanding[] = [...landings.entries()]
        .map(([taskId, landing]) => ({
          taskId,
          title: titles.get(taskId) ?? taskId,
          crossings: landing.crossings,
          terminal: landing.terminal,
        }))
        .sort((a, b) => a.taskId.localeCompare(b.taskId));
      return {
        runId,
        landings: rows,
        crossings: rows.reduce((total, row) => total + row.crossings, 0),
        terminalLandings: rows.filter((row) => row.terminal).length,
      };
    })
    .sort((a, b) => b.crossings - a.crossings || a.runId.localeCompare(b.runId));

  const page = attemptPageOf(attempts);
  const entry = graphEntryOf(graph);
  return {
    threads: woven,
    unwoven: unwoven.sort((a, b) => a.taskId.localeCompare(b.taskId)),
    crossings: woven.reduce((total, thread) => total + thread.crossings, 0),
    // Still absent with the attempt page in hand, and now for a narrower
    // reason: the record has an end and no start.
    wallClock: absentChannel('wall_clock'),
    executorIdentity: attemptChannel(attempts, 'executor identity', page?.executors),
    observedOrder: attemptChannel(attempts, 'a terminal instant', page?.terminalOrder),
    retryWeave: attemptChannel(attempts, 'an attempt chain', page?.lineages),
    // The ladder is a reading even when every rung is zero: "nothing was
    // cancelled" is a fact about the page, unlike an empty list of executors.
    cancellationLadder:
      page === null
        ? attemptChannelGap(attempts, 'the cancellation ladder')
        : { available: true, value: page.ladder },
    // Declared calendar data, so an empty set is a graph with no tasks in it
    // rather than a measurement that failed.
    instants: graphChannel(
      graph,
      'the recorded task instants',
      entry === null ? null : timelineInstants(entry),
    ),
    attempts,
    graph,
  };
}

// --- Causal: disagreement field ---------------------------------------------

/**
 * What one declared dependency edge reads as against the evidence attached to
 * its two ends.
 *
 * `dependent_ahead` is the loud state: a task carries terminal evidence while
 * the task it declares a dependency on carries none. The dependency either did
 * not gate the work or is not the dependency the plan says it is, and 11c
 * names that hidden coupling in the plan itself.
 *
 * `order_unread` is the honest middle. Both ends finished, and with no
 * timestamp anywhere in this read there is nothing to order them by. It is not
 * agreement and must not be drawn as agreement.
 */
export type WorkCausalReadingKind =
  | 'dependent_ahead'
  | 'consistent'
  | 'order_unread'
  | 'unobserved'
  | 'unresolved';

export interface WorkCausalEdge {
  readonly dependency: string;
  readonly dependent: string;
  readonly kind: WorkCausalReadingKind;
}

export interface WorkCausalReading {
  readonly edges: readonly WorkCausalEdge[];
  readonly disagreements: readonly WorkCausalEdge[];
  readonly counts: Readonly<Record<WorkCausalReadingKind, number>>;
  readonly declared: number;
  readonly observedOrder: WorkChannel<never>;
  /** Executed-before-but-undeclared, the field's other half. It needs an
   * observed order to find an edge the plan never declared. */
  readonly undeclared: WorkChannel<never>;
  /**
   * The causal candidates the work-product graph DECLARES — what the plan
   * nominated as a possible cause, edge by edge.
   *
   * The one channel on this page whose empty value is a reading. An empty
   * candidate set means nobody declared a candidate, and it is available and
   * empty rather than absent, because the authority answered the question. It
   * is emphatically not the undeclared half of the field: filling it from the
   * order things were observed to finish would manufacture exactly the
   * measurement `observedOrder` says this build cannot take.
   */
  readonly candidates: WorkChannel<readonly WorkDagEdgeV1[]>;
  /** What the graph read answered, so the view states its state rather than
   * inferring it from an empty channel. */
  readonly graph: WorkGraphReading;
}

export function causalReadingLabel(kind: WorkCausalReadingKind): string {
  switch (kind) {
    case 'dependent_ahead':
      return 'Dependent finished first';
    case 'consistent':
      return 'Consistent so far';
    case 'order_unread':
      return 'Both finished, order unread';
    case 'unobserved':
      return 'Nothing observed yet';
    case 'unresolved':
      return 'Dependency outside this page';
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

export function causalReadingState(kind: WorkCausalReadingKind): DomainStateKind {
  switch (kind) {
    case 'dependent_ahead':
      return 'conflicting';
    case 'consistent':
      return 'ready';
    case 'order_unread':
      return 'unknown';
    case 'unobserved':
      return 'partial';
    case 'unresolved':
      return 'unavailable';
    default: {
      const unhandled: never = kind;
      return unhandled;
    }
  }
}

export function workCausalReading(
  projections: readonly WorkProjection[],
  graph: WorkGraphReading = { state: 'pending' },
): WorkCausalReading {
  const entry = graphEntryOf(graph);
  const terminal = new Map(
    projections.map((p) => [p.task_id, p.runtime_evidence.some((e) => e.terminal)]),
  );

  const edges: WorkCausalEdge[] = [];
  for (const projection of projections) {
    for (const dependency of projection.dependencies) {
      const dependencyTerminal = terminal.get(dependency);
      const dependentTerminal = terminal.get(projection.task_id) ?? false;
      const kind: WorkCausalReadingKind =
        dependencyTerminal === undefined
          ? 'unresolved'
          : dependentTerminal && !dependencyTerminal
            ? 'dependent_ahead'
            : dependentTerminal && dependencyTerminal
              ? 'order_unread'
              : dependencyTerminal
                ? 'consistent'
                : 'unobserved';
      edges.push({ dependency, dependent: projection.task_id, kind });
    }
  }

  const counts: Record<WorkCausalReadingKind, number> = {
    dependent_ahead: 0,
    consistent: 0,
    order_unread: 0,
    unobserved: 0,
    unresolved: 0,
  };
  for (const edge of edges) counts[edge.kind] += 1;

  return {
    edges: edges.sort(
      (a, b) => a.dependent.localeCompare(b.dependent) || a.dependency.localeCompare(b.dependency),
    ),
    disagreements: edges.filter((edge) => edge.kind === 'dependent_ahead'),
    counts,
    declared: edges.length,
    observedOrder: absentChannel('observed_order'),
    undeclared: absentChannel('observed_order'),
    candidates: graphChannel(
      graph,
      'the declared causal candidates',
      entry === null ? null : entry.projections.causal.candidate_edges,
    ),
    graph,
  };
}

// --- Workload: cortex aggregation -------------------------------------------

export interface WorkloadRegion {
  readonly runId: string;
  readonly taskCount: number;
  readonly evidenceCount: number;
  readonly terminalCount: number;
}

export interface WorkloadReading {
  readonly regions: readonly WorkloadRegion[];
  /** Tasks the store attaches to no run. 11c draws unattributed work hollow
   * rather than guessing who did it. */
  readonly unattributed: readonly { readonly taskId: string; readonly title: string }[];
  readonly taskCount: number;
  readonly evidenceCount: number;
  /** Declared effort mass over the whole work-product graph, with its runtime
   * split nested inside as its own channel. */
  readonly effortMass: WorkChannel<WorkEffortMassReading>;
  readonly concurrency: WorkChannel<WorkConcurrencyReading>;
  /** Recent change, measured against the instant the graph version was observed
   * at — which is the clock this projection did not have until the graph read
   * was mounted. */
  readonly churn: WorkChannel<WorkChurnReading>;
  /** The live attempt projection under this graph version, and the one coverage
   * state that is an absence rather than a value. */
  readonly runtime: WorkChannel<WorkRuntimeReading>;
  /** What the graph read answered, so the view states its state rather than
   * inferring it from an empty channel. */
  readonly graph: WorkGraphReading;
}

/**
 * Runs as regions, task mass as area.
 *
 * The aggregation ratio a cortex view must print is `taskCount` against
 * `regions.length`. Area stays TASK COUNT and says so: the regions are the
 * snapshot's run/task incidence, and `effortMass` — the measurement 11c
 * actually asks for — is a property of the whole work-product graph rather than
 * of any run. The two are kept as separate readings instead of one being
 * rescaled by the other, because the graph read and the snapshot page need not
 * cover the same tasks and a bar weighted across that seam would be a number
 * neither read holds.
 *
 * The graph argument defaults to `pending` so a caller that has not issued the
 * read draws its four channels as not-yet-answered rather than as absent.
 */
export function workloadReading(
  projections: readonly WorkProjection[],
  graph: WorkGraphReading = { state: 'pending' },
  churnWindow: number = WORK_CHURN_WINDOW_MICROS,
): WorkloadReading {
  const entry = graphEntryOf(graph);
  const regions = new Map<string, { tasks: Set<string>; evidence: number; terminal: number }>();
  const unattributed: { taskId: string; title: string }[] = [];
  let evidenceCount = 0;

  for (const projection of projections) {
    if (projection.runtime_evidence.length === 0) {
      unattributed.push({ taskId: projection.task_id, title: projection.title });
      continue;
    }
    for (const evidence of projection.runtime_evidence) {
      evidenceCount += 1;
      const region = regions.get(evidence.run_id) ?? {
        tasks: new Set<string>(),
        evidence: 0,
        terminal: 0,
      };
      regions.set(evidence.run_id, region);
      region.tasks.add(projection.task_id);
      region.evidence += 1;
      if (evidence.terminal) region.terminal += 1;
    }
  }

  return {
    regions: [...regions.entries()]
      .map(([runId, region]) => ({
        runId,
        taskCount: region.tasks.size,
        evidenceCount: region.evidence,
        terminalCount: region.terminal,
      }))
      .sort((a, b) => b.taskCount - a.taskCount || a.runId.localeCompare(b.runId)),
    unattributed: unattributed.sort((a, b) => a.taskId.localeCompare(b.taskId)),
    taskCount: projections.length,
    evidenceCount,
    effortMass: graphChannel(
      graph,
      'declared effort mass',
      entry === null
        ? null
        : {
            total: entry.projections.workload.total_effort,
            split: effortSplit(entry),
          },
    ),
    // Not routed through `graphChannel`: this one has two ways to be absent —
    // the read did not answer, and the read answered without the figures — and
    // they carry different states and different sentences.
    concurrency:
      entry === null
        ? graphChannelGap(graph, 'requested and actual concurrency')
        : concurrencyReading(entry),
    churn: graphChannel(
      graph,
      'recent churn',
      entry === null ? null : churnReading(entry, churnWindow),
    ),
    // `entry.runtime` rather than `entry.projections.runtime`: the version entry
    // carries the runtime projection the whole bundle was derived against, and
    // reading it from the entry keeps this channel bound to the same version as
    // the effort split and the concurrency figures that are gated on it.
    runtime:
      entry === null
        ? graphChannelGap(graph, 'the live attempt projection')
        : runtimeReading(entry.runtime),
    graph,
  };
}
