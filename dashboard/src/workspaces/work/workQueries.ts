import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import type {
  WorkProjection,
  WorkProjectionCoverageV1,
  WorkProjectionDeltaV1,
  WorkProjectionResumeCursorV1,
  WorkProjectionSnapshotV1,
} from '../../contracts/index.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import { callWork, type WorkResult, type WorkRoute } from './workApi.ts';
import {
  WORK_DELTA_ROUTE,
  WORK_SNAPSHOT_ROUTE,
} from './workRoutes.ts';

/**
 * Work reads and commands as queries.
 *
 * Scoped through `scopedUrl` like every other surface. That matters more here
 * than elsewhere: the Work routes are nested straight onto an application router
 * that is bound to the *active* project at construction, so calling them
 * unscoped while the scope bar names a different project would draw one
 * project's tasks under another project's name. Routing a selected project
 * through the gateway means the answer is either that project's or a refusal,
 * and never silently the wrong one.
 */

/** How many projections a page asks for. The daemon decides what it can
 * actually return and says so in `coverage`; this is a request, not a promise. */
export const WORK_PAGE_SIZE = 100;

export function workQueryKey(scope: string, part: string, ...rest: readonly unknown[]) {
  return ['work', part, scope, ...rest] as const;
}

/** The resume cursor a coverage reading carries, or `undefined` when it carries
 * none.
 *
 * Only `capped` and `partial` have one — `complete` is the daemon saying there
 * is nothing after this page, so asking for a delta from it would be asking for
 * a continuation that does not exist. */
export function resumeCursor(
  coverage: WorkProjectionCoverageV1,
): WorkProjectionResumeCursorV1 | undefined {
  switch (coverage.state) {
    case 'capped':
    case 'partial':
      return coverage.cursor;
    case 'complete':
      return undefined;
    default: {
      const unhandled: never = coverage;
      return unhandled;
    }
  }
}

export function useWorkSnapshot(pageSize: number = WORK_PAGE_SIZE) {
  const scope = useScope((state) => state.scope);
  const key = scopeKey(scope);
  return useQuery<WorkResult<WorkProjectionSnapshotV1>>({
    queryKey: workQueryKey(key, 'snapshot', pageSize),
    queryFn: () =>
      callWork(
        WORK_SNAPSHOT_ROUTE,
        { page_size: pageSize },
        scopedUrl(scope, WORK_SNAPSHOT_ROUTE.path),
      ),
  });
}

/**
 * The continuation of a capped or partial snapshot.
 *
 * Disabled without a cursor rather than called with a fabricated one: a delta
 * request needs a resume token the daemon minted, and inventing one would ask
 * the daemon to continue from a position it never reported.
 */
export function useWorkDelta(
  cursor: WorkProjectionResumeCursorV1 | undefined,
  pageSize: number = WORK_PAGE_SIZE,
) {
  const scope = useScope((state) => state.scope);
  const key = scopeKey(scope);
  return useQuery<WorkResult<WorkProjectionDeltaV1>>({
    queryKey: workQueryKey(key, 'delta', cursor?.token ?? null, pageSize),
    enabled: cursor !== undefined,
    queryFn: () =>
      callWork(
        WORK_DELTA_ROUTE,
        { cursor: cursor as WorkProjectionResumeCursorV1, page_size: pageSize },
        scopedUrl(scope, WORK_DELTA_ROUTE.path),
      ),
  });
}

/**
 * One Work command.
 *
 * A command that lands invalidates every Work read rather than splicing its
 * returned projection into the cached snapshot. The projection it answers with
 * is authoritative for that one task, but a snapshot is a coherent set at one
 * `sequence` with one `coverage`; writing a newer row into an older set would
 * assemble a snapshot the daemon never produced, and the sequence stamped on it
 * would then be a claim this build invented. Refetching costs a round trip and
 * keeps the page showing a state that actually existed.
 *
 * The returned projection is still handed back to the caller, so a control can
 * report precisely what it committed while the refetch is in flight.
 */
export function useWorkCommand<Request, Response>(route: WorkRoute<Request, Response>) {
  const scope = useScope((state) => state.scope);
  const client = useQueryClient();
  return useMutation<WorkResult<Response>, never, Request>({
    mutationKey: ['work', 'command', route.operation, scopeKey(scope)],
    mutationFn: (request: Request) => callWork(route, request, scopedUrl(scope, route.path)),
    onSuccess: (result) => {
      // Only a committed command changes what a read would return. Invalidating
      // on a refusal would refetch on every rejected keystroke and, worse, make
      // a denied command look like it had moved something.
      if (result.outcome === 'value') {
        void client.invalidateQueries({ queryKey: ['work'] });
      }
    },
  });
}

/**
 * The tasks a snapshot reports, ordered for reading.
 *
 * Sorted by title so the board is stable across refetches; the daemon returns
 * projection order, which is insertion order and shuffles as tasks change.
 */
export function orderedProjections(
  snapshot: WorkProjectionSnapshotV1,
): readonly WorkProjection[] {
  return [...snapshot.projections].sort((left, right) => left.title.localeCompare(right.title));
}
