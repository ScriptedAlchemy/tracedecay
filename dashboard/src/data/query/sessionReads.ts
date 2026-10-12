/**
 * Session reads that two workspaces make as the same request, so each has one
 * key and one cache entry. A read belongs here only when every parameter
 * matches; a look-alike stays in its page under its own key:
 *
 *   Sessions' index page `/api/loom/temporal?limit=<rows>&offset=<n>` is not
 *   Loom's 200-row temporal page `/api/loom/temporal?limit=200`.
 *   Sessions' transcript `session/{id}?limit=100` with a cursor is not Loom's
 *   chain `session/{id}?limit=200`.
 */
import {
  AnalyticsSubagentTreePayloadV1Schema,
  LcmTimelinePayloadV1Schema,
} from '../../contracts/generated.ts';
import { useEnvelope } from './useEnvelope.ts';

export function useSubagentTree() {
  return useEnvelope(
    ['analytics', 'subagent-tree'],
    '/api/plugins/analytics/subagent-tree',
    AnalyticsSubagentTreePayloadV1Schema,
  );
}

/** Both parameters always travel, so a reader relying on the route's
 * defaults and one naming them are the same request. */
export function useLcmTimeline(bucket: 'day' | 'hour', limit: number) {
  return useEnvelope(
    ['lcm', 'timeline', bucket, limit],
    `/api/plugins/hermes-lcm/timeline?bucket=${bucket}&limit=${limit}`,
    LcmTimelinePayloadV1Schema,
  );
}
