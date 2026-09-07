import type {
  BranchTopologyPolicyV1,
  ExecutionTopologyViewV1,
  ReviewTopologyPolicyV1,
  WorkAttemptListCoverageV1,
  WorkAttemptTopologyBindingV1,
  WorkTopologyExecutionPlacementV1,
  WorkTopologyIntegrationStrategyV1,
} from '../../contracts/index.ts';
import type { WorkResult } from './workApi.ts';
import type { WorkChannel } from './workChannel.ts';
import { absentChannel } from './workChannel.ts';

/**
 * The execution-topology lens consumes `operation.work.topology` directly.
 *
 * `ExecutionTopologyViewV1` is the application projection that binds all four
 * structural dimensions to one verified topology generation. The browser does
 * not group raw attempts into replacement executor or worktree structures:
 * that would make a page-local tally compete with the canonical placement and
 * policy read. Raw attempts remain separately available to the accounting
 * ledger only for facts this topology view does not publish.
 */

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

export interface WorkTopologyReading {
  readonly binding: WorkChannel<WorkAttemptTopologyBindingV1>;
  readonly coverage: WorkChannel<WorkAttemptListCoverageV1>;
  readonly executionPlacement: WorkChannel<WorkTopologyExecutionPlacementV1>;
  readonly branchTopology: WorkChannel<BranchTopologyPolicyV1>;
  readonly reviewTopology: WorkChannel<ReviewTopologyPolicyV1>;
  readonly integrationStrategy: WorkChannel<WorkTopologyIntegrationStrategyV1>;
  /** Attempts carry terminal observations but no start instant; topology
   * structure never manufactures a duration axis. */
  readonly wallClock: WorkChannel<never>;
}

function topologyGap(
  result: WorkResult<ExecutionTopologyViewV1> | undefined,
  measure: string,
): WorkChannel<never> {
  if (result === undefined) {
    return {
      available: false,
      state: 'loading',
      detail: `the canonical execution-topology read has not answered yet, so ${measure} is not drawn`,
    };
  }
  if (result.outcome === 'refused') {
    return {
      available: false,
      state: result.state,
      detail: `the canonical execution-topology read was refused, so ${measure} is not drawn: ${result.detail}`,
    };
  }
  return {
    available: false,
    state: 'denied',
    detail:
      `the canonical execution-topology read states that no Work exists in this scope, so ${measure} has no topology to describe`,
  };
}

/** Converts one generated topology payload into display channels without
 * reconstructing any dimension from raw attempts. */
export function workTopologyReading(
  result: WorkResult<ExecutionTopologyViewV1> | undefined,
): WorkTopologyReading {
  const view =
    result !== undefined && result.outcome === 'value' && result.value.state === 'view'
      ? result.value
      : null;
  const gap = (measure: string) => topologyGap(result, measure);

  return {
    binding: view === null ? gap('the verified topology generation') : { available: true, value: view.topology },
    coverage: view === null ? gap('topology-page coverage') : { available: true, value: view.coverage },
    executionPlacement:
      view === null ? gap('execution placement') : { available: true, value: view.execution_placement },
    branchTopology: view === null ? gap('branch topology') : { available: true, value: view.branch_topology },
    reviewTopology: view === null ? gap('review topology') : { available: true, value: view.review_topology },
    integrationStrategy:
      view === null ? gap('integration strategy') : { available: true, value: view.integration_strategy },
    wallClock: absentChannel('wall_clock'),
  };
}
