import { useQuery } from '@tanstack/react-query';
import type {
  ExecutionTopologyMetricsRequestV1,
  ExecutionTopologyMetricsV1,
  ExecutionTopologyViewV1,
  ResolvedScope,
  WorkAttemptListV1,
  WorkGraphReadV1,
} from '../../contracts/index.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import { workQueryKey } from '../../data/query/work.ts';
import { callWork, type WorkResult } from './workApi.ts';
import {
  WORK_LIST_ATTEMPTS_ROUTE,
  WORK_EXECUTION_TOPOLOGY_METRICS_ROUTE,
  WORK_TOPOLOGY_ROUTE,
  WORK_VIEWS_ROUTE,
} from './workRoutes.ts';

/**
 * The two reads behind the four Work projections.
 *
 * `useWorkAttempts` is the execution record — one page of `WorkAttemptV1`.
 * `useWorkGraphViews` is the work-product graph and the projection bundle
 * derived from one version of it. Both are scoped through `scopedUrl`, both ask
 * once and state their coverage, and both are enabled per-projection so a
 * camera position that does not draw a read does not issue it.
 */

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

/** The product-visible accounting window. It is finite so the backend can
 * prove the horizon, while still long enough to cover the normal RC operator
 * review. A capped result stays a typed capped result; this request never
 * chases it into an invented total. */
export const WORK_TOPOLOGY_METRICS_WINDOW_MICROS = 30 * 24 * 60 * 60 * 1_000_000;
export const WORK_TOPOLOGY_METRICS_MAX_EVENTS = 10_000;

/** Build the one canonical accounting request shared by Work, Observatory,
 * and Costs consumers. `untilMicros` is injectable for a stable test, but a
 * real read mints its upper bound at fetch time. */
export function workTopologyMetricsRequest(
  untilMicros: number = Date.now() * 1_000,
): ExecutionTopologyMetricsRequestV1 {
  return {
    horizon: {
      since_micros: untilMicros - WORK_TOPOLOGY_METRICS_WINDOW_MICROS,
      until_micros: untilMicros,
    },
    max_events: WORK_TOPOLOGY_METRICS_MAX_EVENTS,
  };
}

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

/** The canonical structural topology page. It shares the attempt cursor
 * vocabulary but is its own application projection: policy lanes and durable
 * placement state are not reconstructed from attempt envelopes in the browser.
 */
export function useWorkTopology(enabled: boolean, pageSize: number = WORK_ATTEMPT_PAGE_SIZE) {
  const scope = useScope((state) => state.scope);
  const key = scopeKey(scope);
  return useQuery<WorkResult<ExecutionTopologyViewV1>>({
    queryKey: workQueryKey(key, 'topology', pageSize),
    enabled,
    queryFn: () =>
      callWork(
        WORK_TOPOLOGY_ROUTE,
        { cursor: null, page_size: pageSize },
        scopedUrl(scope, WORK_TOPOLOGY_ROUTE.path),
      ),
  });
}

/**
 * The canonical bounded accounting read. It is scoped exactly like the Work
 * topology page, including the selected-project gateway; no dashboard surface
 * reconstructs descriptor cells from attempts, graph rows, or policy lanes.
 */
export function useWorkTopologyMetrics(enabled: boolean) {
  const scope = useScope((state) => state.scope);
  const key = scopeKey(scope);
  return useQuery<WorkResult<ExecutionTopologyMetricsV1>>({
    queryKey: workQueryKey(key, 'topology-metrics', WORK_TOPOLOGY_METRICS_WINDOW_MICROS, WORK_TOPOLOGY_METRICS_MAX_EVENTS),
    enabled,
    queryFn: () =>
      callWork(
        WORK_EXECUTION_TOPOLOGY_METRICS_ROUTE,
        workTopologyMetricsRequest(),
        scopedUrl(scope, WORK_EXECUTION_TOPOLOGY_METRICS_ROUTE.path),
      ),
  });
}

/**
 * The work-product graph read behind the four Work projections.
 *
 * The bootstrap read uses the profile-owned no-Git selection. Once any Work
 * response supplies its canonical `ResolvedScope`, subsequent reads use the
 * exact project/repository relation from that daemon-owned envelope. This is
 * required for provider attempts: atomic admission links them under repository
 * authority, and a later profile-only read cannot truthfully claim that head.
 * The browser never derives a repository or worktree identity from a path,
 * label, or project id.
 *
 * `continuation` is null and stays null. It is a timeline cursor and is legal
 * only on `evolution` and `forensic`; on `current` it would name a position in
 * a timeline this request never asked for. That is the same discipline the
 * attempt read follows: one page, coverage said out loud, no auto-paging. An
 * auto-paging loop over the graph timeline would spend an unbounded number of
 * round trips and still have to abandon the walk when the graph version moved
 * underneath it.
 *
 * `observed_at` is the caller's own observation instant, in microseconds,
 * because `UtcMicros` is microseconds and a millisecond value here would place
 * every read a thousand-fold too early and quietly turn every churn reading
 * into "nothing recent". It is sent rather than defaulted because the authority
 * derives the runtime-dependent halves of the bundle — the ready/running/
 * blocked effort split and both concurrency figures — against the instant the
 * caller names.
 */
export function workGraphReadRequest(observedAt: number, scope?: ResolvedScope) {
  return {
    selection:
      scope === undefined
        ? { selection: 'profile_owned_no_git' as const }
        : {
            selection: 'relations' as const,
            relation_scopes: [
              {
                kind: 'repository' as const,
                project_id: scope.project_id,
                repository_id: scope.repository_id,
              },
            ],
          },
    mode: { mode: 'current' },
    continuation: null,
    observed_at: observedAt,
  } as const;
}

/**
 * @param enabled the graph read feeds the four projections beside the board and
 * nothing on the board itself, so it is issued when one of them is the camera
 * rather than on every visit to the Work page. A disabled query has no data,
 * which the reading reports as pending — correct, because nothing has been
 * asked.
 */
export function useWorkGraphViews(enabled: boolean, authority?: ResolvedScope) {
  const scope = useScope((state) => state.scope);
  const key = scopeKey(scope);
  return useQuery<WorkResult<WorkGraphReadV1>>({
    queryKey: workQueryKey(
      key,
      'views',
      authority === undefined
        ? 'profile-owned-no-git'
        : `${authority.project_id}/${authority.repository_id}`,
    ),
    enabled,
    // The observation instant is minted per fetch rather than per render: as a
    // query-key member it would mint a new cache entry on every render and turn
    // one read into an unbounded stream of them.
    queryFn: () =>
      callWork(
        WORK_VIEWS_ROUTE,
        workGraphReadRequest(Date.now() * 1_000, authority),
        scopedUrl(scope, WORK_VIEWS_ROUTE.path),
      ),
  });
}
