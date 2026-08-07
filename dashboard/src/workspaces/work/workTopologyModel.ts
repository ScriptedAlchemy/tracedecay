import type {
  WorkAttemptListCoverageV1,
  WorkAttemptListV1,
  WorkAttemptTopologyBindingV1,
  WorkAttemptV1,
  WorkProviderBackendV1,
} from '../../contracts/index.ts';
import type { WorkResult } from './workApi.ts';
import { workAttemptReading, type WorkAttemptReading } from './workAttemptModel.ts';
import { absentChannel, type WorkChannel } from './workChannel.ts';
import { attemptChannelGap } from './workViewsModel.ts';

/**
 * The execution-topology lens, read off the contracts this build actually has.
 *
 * Plan 11's presentation contract for this lens decodes an application-owned
 * `ExecutionTopologyViewV1` with four independently decoded dimensions —
 * `execution_placement`, `branch_topology`, `review_topology`,
 * `integration_strategy` — plus dependency-commit and merge-order rails,
 * conflict/proximity evidence, proposals, receipts, and frame playback. That
 * DTO is not in this build's generated contract catalog
 * (`contracts/generated.ts` carries no `ExecutionTopologyViewV1Schema`) and no
 * `operation.work.topology` read is mounted, so THIS MODULE IS THE BINDING
 * POINT: when codegen emits the view, declare its route in `workRoutes.ts`,
 * import the schema here, and replace the three `unsupported_schema` dimension
 * channels below with decoded lanes. Until then those dimensions are stated as
 * the typed absences they are — Plan 11 requires unsupported lane families to
 * remain explicit rather than disappear — and nothing here invents a lane a
 * contract did not carry.
 *
 * What IS readable today comes from one mounted read, `operation.work.
 * list_attempts`, whose page this module walks rather than restates:
 *
 *   the topology binding   `WorkAttemptTopologyBindingV1` — the verified Work
 *                          topology generation the page was read under, which
 *                          is the identity the lens pins
 *   execution placement    each attempt's `WorkExecutionEnvelopeV1` names the
 *                          exact repository, worktree, root, commit, and
 *                          (nullable) ref the attempt was admitted against,
 *                          and its `requested_route`/`actual_route` name the
 *                          executor — so the placement dimension is a real
 *                          derivation over generated contract data, exactly as
 *                          the four 11c projections derive theirs
 *
 * The weave over that placement follows 11c's loom row: executors as warp
 * threads (hue = the stable executor identity, app-wide), tasks as landings,
 * a retry as a repeated crossing of the same landing. Every mark is hollow
 * because the record has an end and no start; `wallClock` says so in the
 * channel vocabulary the other projections already speak.
 */

// --- The four dimensions -----------------------------------------------------

/** Plan 11's four independently decoded topology dimensions. Order is display
 * order: the one this build can read first, the three it cannot after it. */
export type WorkTopologyDimension =
  | 'execution_placement'
  | 'branch_topology'
  | 'review_topology'
  | 'integration_strategy';

export const WORK_TOPOLOGY_DIMENSIONS: readonly WorkTopologyDimension[] = [
  'execution_placement',
  'branch_topology',
  'review_topology',
  'integration_strategy',
];

export function topologyDimensionLabel(dimension: WorkTopologyDimension): string {
  switch (dimension) {
    case 'execution_placement':
      return 'Execution placement';
    case 'branch_topology':
      return 'Branch topology';
    case 'review_topology':
      return 'Review topology';
    case 'integration_strategy':
      return 'Integration strategy';
    default: {
      const unhandled: never = dimension;
      return unhandled;
    }
  }
}

/** A dimension no contract in this build carries. Named `unsupported_schema`
 * deliberately: the gap is in the schema catalog, not in a read that failed —
 * the moment `ExecutionTopologyViewV1` reaches `contracts/generated.ts` these
 * sentences become lies and the compiler will not say so, which is why each
 * one names the missing DTO a reviewer can grep for. */
export function topologyDimensionGap(
  dimension: Exclude<WorkTopologyDimension, 'execution_placement'>,
): WorkChannel<never> {
  switch (dimension) {
    case 'branch_topology':
      return {
        available: false,
        state: 'unsupported_schema',
        detail:
          'no branch-stack lanes can be drawn: the generated contract catalog carries no ExecutionTopologyViewV1, so no route answers branch stacks, worktree lifecycle state, or dependency-commit edges — the attempt envelope pins each attempt to one worktree and commit, and that placement is drawn where it is read, without synthesizing a stack from it',
      };
    case 'review_topology':
      return {
        available: false,
        state: 'unsupported_schema',
        detail:
          'no review lanes can be drawn: the generated contract catalog carries no ExecutionTopologyViewV1, so no route answers pull-request stacks, provider capability state, or review positions — and provider order may never be inferred from the local task DAG',
      };
    case 'integration_strategy':
      return {
        available: false,
        state: 'unsupported_schema',
        detail:
          'no integration lanes can be drawn: the generated contract catalog carries no ExecutionTopologyViewV1, so no route answers merge-order rails, integration proposals, receipts, or conflict/proximity evidence — an order of integration may never be read off lane positions',
      };
    default: {
      const unhandled: never = dimension;
      return unhandled;
    }
  }
}

// --- Execution placement: the executor weave ---------------------------------

/** One crossing of an executor over a task. `crossings` counts attempts this
 * executor made at the task on this page — a repeated crossing of the same
 * landing is the weave's rendering of a retry. */
export interface WorkTopologyLanding {
  readonly taskId: string;
  readonly crossings: number;
  /** At least one attempt here carries terminal evidence. */
  readonly terminal: boolean;
  /** At least one attempt here has not terminated. */
  readonly open: boolean;
}

/** One warp thread: an executor (a provider route), and the landings it
 * crossed. `executorKey` is the stable app-wide identity the hue is hashed
 * from — the same provider route always wears the same hue on every screen. */
export interface WorkTopologyThread {
  readonly providerId: string;
  readonly routeId: string;
  readonly executorKey: string;
  /** Backends and models observed in the admitted execution snapshots of the
   * attempts on this thread. Sets, because admission can vary per attempt. */
  readonly backends: readonly WorkProviderBackendV1[];
  readonly models: readonly string[];
  readonly attempts: number;
  /** Attempts requested on another route that actually ran here. */
  readonly diverted: number;
  /** Attempts requested here whose actual route this read has not observed. */
  readonly unobserved: number;
  readonly landings: readonly WorkTopologyLanding[];
}

/** One exact ref snapshot an attempt was admitted against. `reference` is the
 * daemon's nullable ref id: `null` is recorded as itself, never as a branch
 * name guessed from the root path. */
export interface WorkTopologyRefPin {
  readonly reference: string | null;
  readonly commit: string;
}

/** One worktree lane: every attempt on the page the execution envelope placed
 * in this worktree, with the exact repository/ref/commit identities pinned. */
export interface WorkTopologyWorktreeLane {
  readonly worktreeId: string;
  readonly worktreeRoot: string;
  /** A set, defensively: the contract implies one repository per worktree, and
   * if a page ever disagrees the disagreement is shown rather than resolved. */
  readonly repositoryIds: readonly string[];
  readonly refs: readonly WorkTopologyRefPin[];
  readonly attempts: number;
  readonly taskIds: readonly string[];
  readonly executorKeys: readonly string[];
}

// --- The reading -------------------------------------------------------------

export interface WorkTopologyReading {
  /** What the attempt read itself answered, so the view states the page's
   * refusals in the daemon's own taxonomy rather than inferring them. */
  readonly attempts: WorkAttemptReading;
  /** The verified topology generation the page was read under — the identity
   * this lens pins, from `WorkAttemptTopologyBindingV1`. */
  readonly binding: WorkChannel<WorkAttemptTopologyBindingV1>;
  /** How much of the authorized attempt set the page covers. Carried so the
   * view can caption its population; under `capped` every count is a floor. */
  readonly coverage: WorkChannel<WorkAttemptListCoverageV1>;
  /** The executor weave — the readable half of `execution_placement`. */
  readonly threads: WorkChannel<readonly WorkTopologyThread[]>;
  /** The worktree lanes — the other half, pinned to exact identities. */
  readonly worktreeLanes: WorkChannel<readonly WorkTopologyWorktreeLane[]>;
  /** The three dimensions no contract carries, each a stated absence. */
  readonly branchTopology: WorkChannel<never>;
  readonly reviewTopology: WorkChannel<never>;
  readonly integrationStrategy: WorkChannel<never>;
  /** Why every mark is hollow: an attempt records an end and no start. */
  readonly wallClock: WorkChannel<never>;
}

export function executorKeyOf(providerId: string, routeId: string): string {
  return `${providerId}/${routeId}`;
}

function threadReadings(attempts: readonly WorkAttemptV1[]): WorkTopologyThread[] {
  const rows = new Map<
    string,
    {
      providerId: string;
      routeId: string;
      backends: Set<WorkProviderBackendV1>;
      models: Set<string>;
      attempts: number;
      diverted: number;
      unobserved: number;
      landings: Map<string, { crossings: number; terminal: boolean; open: boolean }>;
    }
  >();

  for (const attempt of attempts) {
    const requested = attempt.requested_route;
    const actual = attempt.actual_route;
    const effective = actual ?? requested;
    const key = executorKeyOf(effective.provider_id, effective.route_id);
    const row = rows.get(key) ?? {
      providerId: effective.provider_id,
      routeId: effective.route_id,
      backends: new Set<WorkProviderBackendV1>(),
      models: new Set<string>(),
      attempts: 0,
      diverted: 0,
      unobserved: 0,
      landings: new Map(),
    };
    rows.set(key, row);
    row.attempts += 1;
    if (actual === null) row.unobserved += 1;
    else if (
      actual.provider_id !== requested.provider_id ||
      actual.route_id !== requested.route_id
    ) {
      row.diverted += 1;
    }
    const snapshot = attempt.execution.execution_snapshot;
    row.backends.add(snapshot.backend);
    row.models.add(snapshot.model);

    const taskId = attempt.identity.task_id;
    const landing = row.landings.get(taskId) ?? { crossings: 0, terminal: false, open: false };
    row.landings.set(taskId, {
      crossings: landing.crossings + 1,
      terminal: landing.terminal || attempt.terminal !== null,
      open: landing.open || attempt.terminal === null,
    });
  }

  return [...rows.entries()]
    .map(([executorKey, row]) => ({
      providerId: row.providerId,
      routeId: row.routeId,
      executorKey,
      backends: [...row.backends].sort(),
      models: [...row.models].sort(),
      attempts: row.attempts,
      diverted: row.diverted,
      unobserved: row.unobserved,
      landings: [...row.landings.entries()]
        .map(([taskId, landing]) => ({ taskId, ...landing }))
        .sort((a, b) => a.taskId.localeCompare(b.taskId)),
    }))
    .sort((a, b) => b.attempts - a.attempts || a.executorKey.localeCompare(b.executorKey));
}

function worktreeLaneReadings(attempts: readonly WorkAttemptV1[]): WorkTopologyWorktreeLane[] {
  const lanes = new Map<
    string,
    {
      worktreeRoot: string;
      repositoryIds: Set<string>;
      refs: Map<string, WorkTopologyRefPin>;
      attempts: number;
      taskIds: Set<string>;
      executorKeys: Set<string>;
    }
  >();

  for (const attempt of attempts) {
    const execution = attempt.execution;
    const lane = lanes.get(execution.worktree_id) ?? {
      worktreeRoot: execution.worktree_root,
      repositoryIds: new Set<string>(),
      refs: new Map<string, WorkTopologyRefPin>(),
      attempts: 0,
      taskIds: new Set<string>(),
      executorKeys: new Set<string>(),
    };
    lanes.set(execution.worktree_id, lane);
    lane.attempts += 1;
    lane.repositoryIds.add(execution.repository_id);
    const pin: WorkTopologyRefPin = { reference: execution.reference, commit: execution.commit };
    lane.refs.set(`${pin.reference ?? ' '} ${pin.commit}`, pin);
    lane.taskIds.add(attempt.identity.task_id);
    const effective = attempt.actual_route ?? attempt.requested_route;
    lane.executorKeys.add(executorKeyOf(effective.provider_id, effective.route_id));
  }

  return [...lanes.entries()]
    .map(([worktreeId, lane]) => ({
      worktreeId,
      worktreeRoot: lane.worktreeRoot,
      repositoryIds: [...lane.repositoryIds].sort(),
      refs: [...lane.refs.values()].sort(
        (a, b) =>
          (a.reference ?? '').localeCompare(b.reference ?? '') ||
          a.commit.localeCompare(b.commit),
      ),
      attempts: lane.attempts,
      taskIds: [...lane.taskIds].sort(),
      executorKeys: [...lane.executorKeys].sort(),
    }))
    .sort((a, b) => b.attempts - a.attempts || a.worktreeId.localeCompare(b.worktreeId));
}

/** A channel proved by at least one row of the page, otherwise carrying the
 * read's own reason — the same rule the 11c projections apply. */
function pageChannel<T>(
  reading: WorkAttemptReading,
  measure: string,
  rows: readonly T[] | undefined,
): WorkChannel<readonly T[]> {
  if (rows === undefined || rows.length === 0) return attemptChannelGap(reading, measure);
  return { available: true, value: rows };
}

/**
 * The execution-topology reading, from the raw attempt-list result.
 *
 * Takes the raw `WorkResult` rather than a `WorkAttemptReading` because the
 * placement derivations walk the attempts' execution envelopes, which the
 * derived attempt page deliberately does not restate. The reading state is
 * still computed once, by `workAttemptReading`, so a refusal is reported in
 * exactly the words every other Work projection reports it in.
 */
export function workTopologyReading(
  result: WorkResult<WorkAttemptListV1> | undefined,
): WorkTopologyReading {
  const reading = workAttemptReading(result);
  const listed =
    result !== undefined && result.outcome === 'value' && result.value.state === 'listed'
      ? result.value
      : null;

  return {
    attempts: reading,
    binding:
      listed === null
        ? attemptChannelGap(reading, 'the verified topology generation')
        : { available: true, value: listed.topology },
    coverage:
      listed === null
        ? attemptChannelGap(reading, 'the attempt-page coverage')
        : { available: true, value: listed.coverage },
    threads: pageChannel(
      reading,
      'an executor placement',
      listed === null ? undefined : threadReadings(listed.attempts),
    ),
    worktreeLanes: pageChannel(
      reading,
      'a worktree placement',
      listed === null ? undefined : worktreeLaneReadings(listed.attempts),
    ),
    branchTopology: topologyDimensionGap('branch_topology'),
    reviewTopology: topologyDimensionGap('review_topology'),
    integrationStrategy: topologyDimensionGap('integration_strategy'),
    wallClock: absentChannel('wall_clock'),
  };
}
