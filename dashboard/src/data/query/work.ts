const WORK_QUERY_ROOT = 'work';

export type WorkReadPart = 'snapshot' | 'delta' | 'list-attempts';

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
  // execution record under the new project's snapshot.
  return [
    [WORK_QUERY_ROOT, 'snapshot', scope],
    [WORK_QUERY_ROOT, 'delta', scope],
    [WORK_QUERY_ROOT, 'list-attempts', scope],
  ];
}

export function workProjectInvalidationKeys(
  projectId: string,
): ReadonlyArray<ReadonlyArray<string>> {
  return workScopeInvalidationKeys(`project:${projectId}`);
}
