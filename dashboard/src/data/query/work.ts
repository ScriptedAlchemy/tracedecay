const WORK_QUERY_ROOT = 'work';

const WORK_READ_PARTS = [
  'snapshot',
  'delta',
  'list-attempts',
  'topology',
  'topology-metrics',
  'views',
  'retrieve-evidence',
  'attempt-status',
  'hydrate-artifacts',
  'run-control',
  'placement-status',
] as const;

export type WorkReadPart = (typeof WORK_READ_PARTS)[number];

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
  return WORK_READ_PARTS.map((part) => [WORK_QUERY_ROOT, part, scope]);
}

export function workProjectInvalidationKeys(
  projectId: string,
): ReadonlyArray<ReadonlyArray<string>> {
  return workScopeInvalidationKeys(`project:${projectId}`);
}
