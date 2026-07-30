const WORK_QUERY_ROOT = 'work';

export type WorkReadPart = 'snapshot' | 'delta';

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
  return [
    [WORK_QUERY_ROOT, 'snapshot', scope],
    [WORK_QUERY_ROOT, 'delta', scope],
  ];
}

export function workProjectInvalidationKeys(
  projectId: string,
): ReadonlyArray<ReadonlyArray<string>> {
  return workScopeInvalidationKeys(`project:${projectId}`);
}
