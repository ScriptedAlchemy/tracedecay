import { useQuery } from '@tanstack/react-query';
import type { WorkGraphReadV1 } from '../../contracts/index.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import { callWork, type WorkResult } from '../work/workApi.ts';
import { WORK_VIEWS_ROUTE } from '../work/workRoutes.ts';
import { workGraphReadRequest } from '../work/workViewsQueries.ts';

/**
 * The one work-product graph read the Agents page makes.
 *
 * `operation.work.views` in `current` mode — the same request the Work
 * workspace issues, through the same `callWork` wire and the same generated
 * contracts, because it is the same question asked from a different page.
 * Nothing about the request is restated here: `workGraphReadRequest` owns the
 * selection, the mode and the observation instant, so a change to how this
 * build asks the daemon for a graph version cannot leave two callers asking
 * differently.
 *
 * Two readings come off this one response — the handoff frontier and the
 * attempt failures — rather than two reads, so both describe the same graph
 * version. A second request would let the page draw a frontier from one version
 * beside failures from another and caption them as one picture.
 *
 * The cache key is the Agents page's own. Sharing the Work workspace's key
 * would couple this page's reads to that page's invalidation, and a Work
 * command refetching a read on a page the user is not looking at buys nothing.
 */
export function useAgentWorkGraph() {
  const scope = useScope((state) => state.scope);
  return useQuery<WorkResult<WorkGraphReadV1>>({
    queryKey: ['agents', 'work-views', scopeKey(scope)],
    // The observation instant is minted per fetch and not per render: as a
    // query-key member it would mint a new cache entry every render and turn
    // one read into an unbounded stream of them.
    queryFn: () =>
      callWork(
        WORK_VIEWS_ROUTE,
        workGraphReadRequest(Date.now() * 1_000),
        scopedUrl(scope, WORK_VIEWS_ROUTE.path),
      ),
  });
}
