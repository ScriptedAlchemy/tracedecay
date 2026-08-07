import { useQuery } from '@tanstack/react-query';
import type { WorkAttemptListV1 } from '../../contracts/index.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import { workQueryKey } from '../../data/query/work.ts';
import { callWork, type WorkResult } from './workApi.ts';
import { WORK_LIST_ATTEMPTS_ROUTE } from './workRoutes.ts';

/**
 * The execution read behind the four Work projections.
 *
 * Scoped exactly like every other Work read — `scopedUrl` rewrites to the
 * project gateway when the scope bar names a project — so the attempts on the
 * page belong to the same project as the snapshot drawn beside them.
 *
 * One page, deliberately. The projections state their coverage rather than
 * chasing it: an auto-paging loop would spend an unbounded number of round
 * trips to turn a `capped` reading into a `complete` one, and would have to
 * abandon the walk anyway the moment the topology generation moved underneath
 * it. Asking once and drawing what came back — with the cap said out loud —
 * is the reading this build can defend.
 */

/** How many attempts a page asks for. The contract admits 1..=1000; the daemon
 * decides what it can actually return and says so in `coverage`. */
export const WORK_ATTEMPT_PAGE_SIZE = 250;

/**
 * @param enabled the execution record is drawn by one projection, so the read
 * is issued when that projection is on screen rather than on every visit to the
 * Work page. A disabled query has no data, which the reading reports as pending
 * — correct, because nothing has been asked.
 */
export function useWorkAttempts(enabled: boolean, pageSize: number = WORK_ATTEMPT_PAGE_SIZE) {
  const scope = useScope((state) => state.scope);
  const key = scopeKey(scope);
  return useQuery<WorkResult<WorkAttemptListV1>>({
    queryKey: workQueryKey(key, 'list-attempts', pageSize),
    enabled,
    queryFn: () =>
      callWork(
        WORK_LIST_ATTEMPTS_ROUTE,
        // No cursor: the first page is the only page this read asks for, and a
        // cursor invented here would name a generation the daemon never minted.
        { cursor: null, page_size: pageSize },
        scopedUrl(scope, WORK_LIST_ATTEMPTS_ROUTE.path),
      ),
  });
}
