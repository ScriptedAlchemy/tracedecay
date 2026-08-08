const WORK_QUERY_ROOT = 'work';

export type WorkReadPart =
  | 'snapshot'
  | 'delta'
  | 'list-attempts'
  | 'topology'
  | 'topology-metrics'
  | 'views'
  | 'attempt-status'
  | 'hydrate-artifacts'
  | 'run-control'
  | 'placement-status';

export function workQueryKey(
  scope: string,
  part: WorkReadPart,
  ...rest: readonly unknown[]
) {
  return [WORK_QUERY_ROOT, part, scope, ...rest] as const;
}

export function workScopeInvalidationKeys(
  scope: string,
): ReadonlyArray<ReadonlyArray<string>> {
  // Every read part, not just the projection pair: an attempt page left
  // un-invalidated on a scope change would keep drawing another project's
  // execution record under the new project's snapshot. The work-product graph
  // read is in here for the same reason — its projections carry effort,
  // workload and live runtime state, and a version left standing across a
  // change would report another scope's graph beside this scope's board.
  return [
    [WORK_QUERY_ROOT, 'snapshot', scope],
    [WORK_QUERY_ROOT, 'delta', scope],
    [WORK_QUERY_ROOT, 'list-attempts', scope],
    [WORK_QUERY_ROOT, 'topology', scope],
    [WORK_QUERY_ROOT, 'topology-metrics', scope],
    [WORK_QUERY_ROOT, 'views', scope],
    [WORK_QUERY_ROOT, 'attempt-status', scope],
    [WORK_QUERY_ROOT, 'hydrate-artifacts', scope],
    [WORK_QUERY_ROOT, 'run-control', scope],
    [WORK_QUERY_ROOT, 'placement-status', scope],
  ];
}

export function workProjectInvalidationKeys(
  projectId: string,
): ReadonlyArray<ReadonlyArray<string>> {
  return workScopeInvalidationKeys(`project:${projectId}`);
}
