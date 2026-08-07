import type { WorkProjection } from '../../contracts/index.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import {
  attemptPageOf,
  type WorkAttemptLineage,
  type WorkAttemptReading,
  type WorkCancellationLadder,
  type WorkExecutorReading,
  type WorkTerminalObservation,
} from './workAttemptModel.ts';

/**
 * The four Work projections of plan 11c, derived from the snapshot this build
 * can actually read.
 *
 * Plan 11c maps each projection onto a grammar the app has already proved:
 * the DAG onto transit strata, the timeline onto the loom weave, the causal
 * view onto the disagreement field, and the workload onto cortex aggregation.
 * The data arrives from two reads, and which read a channel comes from is the
 * thing to keep straight:
 *
 *   the snapshot        `WorkProjection` — the declared dependency graph, its
 *                       strata and cycles, the run/task incidence, the evidence
 *                       each task carries, which tasks no run has touched
 *   the attempt list    `WorkAttemptV1` — the execution record: who ran what
 *                       (`requested_route`/`actual_route`), the retry chains,
 *                       the typed cancellation ladder, and the instant each
 *                       terminated attempt was observed to finish
 *
 * The attempt list is mounted (`operation.work.list_attempts`) and its channels
 * are live, derived in `workAttemptModel.ts` and bound onto the weave below.
 * What is still not mounted is the product projection: 11c's effort mass,
 * concurrency, churn, gating edges and the effort-weighted critical path all
 * need `WorkProductProjectionBundleV1` (`work_product_projection.rs`), a domain
 * type with no route and no generated dashboard contract in this build. Those
 * stay named absences.
 *
 * Wall clock stays absent too, and for a reason worth stating precisely rather
 * than filing under "no data": the attempt record has an end and no start.
 * `WorkLeaseFenceV1` is `{epoch, lease_id}` and `WorkAttemptProgressV1` is
 * `{completed, total}` — neither is a clock — so a terminated attempt yields an
 * instant and never a width. The weave gained an order it can prove and did not
 * gain a duration, and it says both.
 *
 * A channel is never estimated to fill a gap. 11c is explicit: attempt counts,
 * queue ages and wall-times are real or absent, and a degenerate distribution
 * is said rather than drawn.
 */

/**
 * One measurement channel, either proved or explained.
 *
 * There is no third case and no default value. A view that renders
 * `available: false` renders the state and the sentence, so an absence can
 * never be mistaken for a zero.
 */
export type WorkChannel<T> =
  | { readonly available: true; readonly value: T }
  | { readonly available: false; readonly state: DomainStateKind; readonly detail: string };

/**
 * The measurements plan 11c asks each projection to encode that no mounted read
 * in this build can supply.
 *
 * Each is named for the measurement rather than for the route that would
 * supply it, because the gap is in the read model rather than in the transport.
 * Executor identity is deliberately not among them any more: the attempt list
 * carries `requested_route` and `actual_route`, so a thread that cannot name
 * its executor now says which attempt read failed it, in
 * `attemptChannelGap` — a transient reason, not a schema one.
 */
export type WorkChannelGap =
  | 'effort'
  | 'wall_clock'
  | 'observed_order'
  | 'concurrency'
  | 'churn';

const CONTRACT_GAP =
  'no generated contract in this build carries it — the product-graph read is not mounted';

export function channelGap(gap: WorkChannelGap): {
  state: DomainStateKind;
  detail: string;
} {
  switch (gap) {
    case 'effort':
      return {
        state: 'unsupported_schema',
        detail: `task effort is not a field of WorkProjection, so mass and the effort-weighted critical path cannot be measured — ${CONTRACT_GAP}`,
      };
    case 'wall_clock':
      return {
        state: 'unsupported_schema',
        detail:
          'no span can be drawn: an attempt records the instant it was observed to finish, and nothing anywhere records when it started — WorkLeaseFenceV1 is {epoch, lease_id} and WorkAttemptProgressV1 is {completed, total}, so every mark has an end and never a width',
      };
    case 'observed_order':
      return {
        state: 'unsupported_schema',
        detail: `this projection is derived from WorkProjection alone, which carries no timestamp, so the order its tasks ran in cannot be read here — the attempt list carries terminal instants and the weave draws them, but nothing binds a task's completion to an attempt's — ${CONTRACT_GAP}`,
      };
    case 'concurrency':
      return {
        state: 'unsupported_schema',
        detail: `concurrency needs overlapping attempt spans, and this read has neither spans nor live attempt state — ${CONTRACT_GAP}`,
      };
    case 'churn':
      return {
        state: 'unsupported_schema',
        detail: `recent churn needs a clock to be recent against, and this read carries no timestamp — ${CONTRACT_GAP}`,
      };
    default: {
      const unhandled: never = gap;
      return unhandled;
    }
  }
}

export function absentChannel(gap: WorkChannelGap): WorkChannel<never> {
  const { state, detail } = channelGap(gap);
  return { available: false, state, detail };
}

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
   * transit map. Unweighted: see `effort`. */
  readonly longestChain: readonly WorkDagComponent[];
  readonly widestStratum: number;
  readonly effort: WorkChannel<never>;
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
 */
export function workDagReading(projections: readonly WorkProjection[]): WorkDagReading {
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
    effort: absentChannel('effort'),
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
  /** What the attempt read itself answered, so the view states the page's
   * coverage and its refusals rather than inferring them from empty channels. */
  readonly attempts: WorkAttemptReading;
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
 * so.
 *
 * The attempt argument defaults to `pending` so a caller that has not issued
 * the read draws the channels as not-yet-answered rather than as absent.
 */
export function workWeaveReading(
  projections: readonly WorkProjection[],
  attempts: WorkAttemptReading = { state: 'pending' },
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
    attempts,
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

export function workCausalReading(projections: readonly WorkProjection[]): WorkCausalReading {
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
  readonly effortMass: WorkChannel<never>;
  readonly concurrency: WorkChannel<never>;
  readonly churn: WorkChannel<never>;
}

/**
 * Runs as regions, task mass as area.
 *
 * The aggregation ratio a cortex view must print is `taskCount` against
 * `regions.length`. Area is task count rather than effort, and says so:
 * `effortMass` is the measurement 11c actually asks for and it is absent.
 */
export function workloadReading(projections: readonly WorkProjection[]): WorkloadReading {
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
    effortMass: absentChannel('effort'),
    concurrency: absentChannel('concurrency'),
    churn: absentChannel('churn'),
  };
}
