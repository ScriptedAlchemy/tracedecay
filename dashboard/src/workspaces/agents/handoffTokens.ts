import {
  ListTaskHandoffsRequestV1Schema,
  ListTaskHandoffsResultV1Schema,
  type ListedTaskHandoffV1,
  type ListTaskHandoffsResultV1,
} from '../../contracts/generated.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';
import type { WorkResult, WorkRoute } from '../work/workApi.ts';

/**
 * The handoff-TOKEN frontier — a different measure from the handoff frontier
 * in `handoff.ts`, and the two must not be conflated.
 *
 * `handoff.ts` reads `WorkItemV1.handoffs` off the work-product graph: the
 * record of one actor having handed a task to another, with the evidence it
 * reached and the questions it left. That is history, and it is complete only
 * for handoffs that actually happened.
 *
 * This module reads the daemon's grant store instead, through
 * `operation.handoff.list_task_handoffs` (`POST /api/application/handoff/list-task`).
 * It answers what the graph cannot: which single-use tokens are OUTSTANDING —
 * issued and not yet redeemed — and which lapsed unredeemed. A dropped handoff
 * leaves no work-graph record at all, so without this read it is invisible.
 *
 * Two properties of the route shape everything here.
 *
 * It carries NO bearer. The request holds a session id and nothing else, and
 * the result holds token digests. The authority never stored a secret, so there
 * is none to leak.
 *
 * It is RECIPIENT-SCOPED. The daemon returns exactly the grants the caller
 * could itself redeem — same session, same authorization scope, same recipient
 * principal that redemption checks. That is why listing grants no new
 * authority, and it is also why an empty answer here means "nothing was
 * addressed to this reader in this session", which is emphatically NOT "no
 * handoff tokens exist". Every caption below is obliged to keep those apart.
 */

/** The mounted route, named for the operation the daemon registers. */
export const HANDOFF_LIST_TASK_ROUTE = {
  operation: 'operation.handoff.list_task_handoffs',
  path: '/api/application/handoff/list-task',
  request: ListTaskHandoffsRequestV1Schema,
  response: ListTaskHandoffsResultV1Schema,
} as const satisfies WorkRoute<unknown, unknown>;

export type HandoffTokenReading =
  /** No session has been named yet, or the answer has not landed. */
  | { readonly state: 'pending' }
  /** There is no session to ask about — not an empty frontier. */
  | { readonly state: 'unasked'; readonly detail: string }
  | { readonly state: 'refused'; readonly chip: DomainStateKind; readonly detail: string }
  | {
      readonly state: 'read';
      readonly sessionId: string;
      readonly outstanding: readonly ListedTaskHandoffV1[];
      readonly lapsed: readonly ListedTaskHandoffV1[];
      readonly redeemed: readonly ListedTaskHandoffV1[];
      readonly observedAtMicros: number;
      readonly truncated: boolean;
    };

/**
 * Split the frontier by what each token's state means for a reader.
 *
 * `expired` is deliberately promoted to its own list rather than folded in with
 * `consumed`: a token that was redeemed is work someone picked up, and a token
 * that lapsed is work that was offered and dropped. Showing them together would
 * hide every dropped handoff behind the successful ones.
 */
export function readHandoffTokens(
  sessionId: string | null,
  result: WorkResult<ListTaskHandoffsResultV1> | undefined,
): HandoffTokenReading {
  if (sessionId === null) {
    return {
      state: 'unasked',
      detail:
        'no session is named on this page yet, so no token frontier has been requested — this is an unasked question, not an empty frontier',
    };
  }
  if (result === undefined) return { state: 'pending' };
  if (result.outcome === 'refused') {
    return { state: 'refused', chip: result.state, detail: result.detail };
  }
  const value = result.value;
  const withState = (wanted: ListedTaskHandoffV1['state']) =>
    value.handoffs.filter((handoff) => handoff.state === wanted);
  return {
    state: 'read',
    sessionId,
    outstanding: withState('open'),
    lapsed: withState('expired'),
    redeemed: withState('consumed'),
    observedAtMicros: value.observed_at,
    truncated: value.truncated,
  };
}

/** What a token points at, without inventing a name for it. */
export function handoffTargetLabel(handoff: ListedTaskHandoffV1): string {
  const target = handoff.target;
  return target.kind === 'task'
    ? `task ${target.task_id} @ v${target.version}`
    : `finding ${target.finding_id}`;
}
