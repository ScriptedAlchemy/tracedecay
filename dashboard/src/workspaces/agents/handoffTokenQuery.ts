import { skipToken, useQuery } from '@tanstack/react-query';
import type { AnalyticsSubagentTreePayloadV1, ListTaskHandoffsResultV1 } from '../../contracts/generated.ts';
import { scopeKey, scopedUrl, useScope } from '../../data/scope/store.ts';
import { callWork, type WorkResult } from '../work/workApi.ts';
import { HANDOFF_LIST_TASK_ROUTE } from './handoffTokens.ts';

/**
 * The session the token frontier is read for.
 *
 * The route is per-session, and this page holds exactly one honest source of
 * session identity: the delegation tree it already read. The NEWEST top is
 * chosen — the most recently started root — because a frontier is read from its
 * leading edge, and the daemon already returns the tree in pre-order with
 * siblings ordered by start.
 *
 * `null` when the tree named no session. That is the difference between having
 * no question to ask and having asked and been told nothing.
 */
export function newestTreeSession(
  payload: AnalyticsSubagentTreePayloadV1 | null,
): string | null {
  if (payload == null || payload.available === false) return null;
  let newest: { sessionId: string; startedAt: number } | null = null;
  for (const node of payload.nodes) {
    if (node.depth !== 0) continue;
    // A session with no recorded start cannot claim to be the newest, but it is
    // still a usable session when nothing else is on offer.
    const startedAt = node.started_at ?? Number.NEGATIVE_INFINITY;
    if (newest === null || startedAt > newest.startedAt) {
      newest = { sessionId: node.session_id, startedAt };
    }
  }
  return newest?.sessionId ?? null;
}

/**
 * One handoff-token frontier read, for one session.
 *
 * Goes through the same `callWork` wire as the Work routes because the handoff
 * family is mounted on the same application router and answers with the same
 * envelope. With no session the query is skipped: react-query must not be
 * asked for an answer to a question this page has not got.
 */
export function useAgentHandoffTokens(sessionId: string | null) {
  const scope = useScope((state) => state.scope);
  return useQuery<WorkResult<ListTaskHandoffsResultV1>>({
    queryKey: ['agents', 'handoff-tokens', scopeKey(scope), sessionId],
    queryFn:
      sessionId === null
        ? skipToken
        : () =>
            callWork(
              HANDOFF_LIST_TASK_ROUTE,
              { session_id: sessionId },
              scopedUrl(scope, HANDOFF_LIST_TASK_ROUTE.path),
            ),
  });
}
