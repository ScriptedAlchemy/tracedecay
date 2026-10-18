/**
 * Only reads whose every parameter matches share a key here. These look alike
 * and must stay in their pages under their own keys:
 *
 *   Sessions' index `/api/loom/temporal?limit=<rows>&offset=<n>` and Loom's
 *   200-row page `/api/loom/temporal?limit=200`.
 *   Sessions' transcript `session/{id}?limit=100` with a cursor and Loom's
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

export function useLcmTimeline(bucket: 'day' | 'hour', limit: number) {
  return useEnvelope(
    ['lcm', 'timeline', bucket, limit],
    `/api/plugins/hermes-lcm/timeline?bucket=${bucket}&limit=${limit}`,
    LcmTimelinePayloadV1Schema,
  );
}
