import { useQuery } from '@tanstack/react-query';
import {
  FeedbackProximityReadRequestV1Schema,
  FeedbackProximityReadResultV1Schema,
  type FeedbackProximityReadResultV1,
} from '../../contracts/index.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import {
  callWork,
  type WorkResult,
  type WorkRoute,
} from '../../workspaces/work/workApi.ts';

export const FEEDBACK_PROXIMITY_ROUTE = {
  operation: 'feedback_proximity',
  path: '/api/feedback/proximity',
  request: FeedbackProximityReadRequestV1Schema,
  response: FeedbackProximityReadResultV1Schema,
} as const satisfies WorkRoute<unknown, unknown>;

export function useFeedbackProximity(enabled: boolean) {
  const scope = useScope((state) => state.scope);
  return useQuery<WorkResult<FeedbackProximityReadResultV1>>({
    queryKey: ['feedback', 'proximity', scopeKey(scope)],
    enabled,
    queryFn: () =>
      callWork(
        FEEDBACK_PROXIMITY_ROUTE,
        { observed_at: Date.now() * 1_000 },
        scopedUrl(scope, FEEDBACK_PROXIMITY_ROUTE.path),
      ),
  });
}
