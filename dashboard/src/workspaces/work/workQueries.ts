import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import type {
  WorkProjection,
  WorkProjectionCoverageV1,
  WorkProjectionDeltaV1,
  WorkProjectionResumeCursorV1,
  WorkProjectionSnapshotV1,
} from '../../contracts/index.ts';
import { type DashboardScope, scopeKey, useScope } from '../../data/scope/store.ts';
import { callWork, type WorkResult, type WorkRoute } from './workApi.ts';
import {
  WORK_DELTA_ROUTE,
  WORK_SNAPSHOT_ROUTE,
} from './workRoutes.ts';

/**
 * Work reads and commands as queries.
 *
 * These routes are the one dashboard surface that is not project-scopable, and
 * the scope handling below exists entirely because of it.
 *
 * `src/dashboard/mod.rs` nests `/api/work` straight onto an application router
 * built with the *active* project's id, and it does not add those routes to
 * `project_api_router`. So the project gateway cannot serve them: a
 * `/api/projects/{id}/work/...` request is rewritten into a router with no such
 * path and comes back 404, for the active project as much as for any other.
 *
 * That leaves one honest arrangement. Where the active project is what the
 * scope bar is asking about, call the route unprefixed and get real data. Where
 * it is not, do not call at all and say why — sending the request anyway would
 * either 404 as "not authorized", which is not what happened, or, if the
 * mounting ever changed, quietly answer with the active project's tasks under
 * another project's name.
 */

/** Whether this scope is one the Work routes can answer for, and the reason
 * when it is not. */
export function workScopeAvailability(
  scope: DashboardScope,
): { available: true } | { available: false; detail: string } {
  if (scope.kind === 'all') return { available: true };
  switch (scope.activation) {
    case 'active':
      return { available: true };
    case 'selected':
      return {
        available: false,
        detail: `Work is served only for the active project, and ${scope.label} is selected rather than active`,
      };
    case 'unresolved':
      return {
        available: false,
        detail: `whether ${scope.label} is the active project is still unresolved`,
      };
    default: {
      const unhandled: never = scope.activation;
      return unhandled;
    }
  }
}

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

/** The refusal an out-of-scope read reports, without issuing a request.
 *
 * `locked` rather than `denied`: nothing was refused by an authority, the
 * surface simply will not answer for this scope, and the remedy is to change
 * scope rather than to gain permission. */
function outOfScope<T>(detail: string): WorkResult<T> {
  return { outcome: 'refused', state: 'locked', detail };
}

export function useWorkSnapshot(pageSize: number = WORK_PAGE_SIZE) {
  const scope = useScope((state) => state.scope);
  const key = scopeKey(scope);
  const availability = workScopeAvailability(scope);
  return useQuery<WorkResult<WorkProjectionSnapshotV1>>({
    queryKey: workQueryKey(key, 'snapshot', pageSize),
    queryFn: () =>
      availability.available
        ? callWork(WORK_SNAPSHOT_ROUTE, { page_size: pageSize }, WORK_SNAPSHOT_ROUTE.path)
        : Promise.resolve(outOfScope<WorkProjectionSnapshotV1>(availability.detail)),
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
  const availability = workScopeAvailability(scope);
  return useQuery<WorkResult<WorkProjectionDeltaV1>>({
    queryKey: workQueryKey(key, 'delta', cursor?.token ?? null, pageSize),
    enabled: cursor !== undefined && availability.available,
    queryFn: () =>
      callWork(
        WORK_DELTA_ROUTE,
        { cursor: cursor as WorkProjectionResumeCursorV1, page_size: pageSize },
        WORK_DELTA_ROUTE.path,
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
  const availability = workScopeAvailability(scope);
  return useMutation<WorkResult<Response>, never, Request>({
    mutationKey: ['work', 'command', route.operation, scopeKey(scope)],
    mutationFn: (request: Request) =>
      availability.available
        ? callWork(route, request, route.path)
        : Promise.resolve(outOfScope<Response>(availability.detail)),
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
