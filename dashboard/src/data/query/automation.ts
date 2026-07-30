/**
 * Reads and controls for the automation scheduler.
 *
 * `automation_scheduler_api.rs` answers `status`, `pause`, and `resume` with
 * the *same* payload — the controls re-read rather than acknowledge — and that
 * is what makes an honest control possible here. A route that replied
 * `{"ok":true}` would leave this module to assume the new state and flip the
 * toggle on faith; because the server returns the reading it just took, the
 * control can seed the query cache with the server's answer and the UI never
 * shows a pause it has not observed.
 *
 * So there is deliberately no optimistic update below. Optimism is the ordinary
 * React Query idiom for a toggle, and it is the wrong one for this surface: it
 * would paint the scheduler paused the instant a user clicked, which is exactly
 * a control state asserted rather than measured. A failed control leaves the
 * last real reading on screen and reports the failure beside it.
 */
import { useMutation, useQueryClient } from '@tanstack/react-query';

import { fetchLegacyWrite, type LegacyWriteResult } from './legacy.ts';
import {
  scopeKey,
  scopeWritable,
  scopedUrl,
  useScope,
  type ScopeWritability,
} from '../scope/store.ts';
import {
  AutomationSchedulerStatusV1Schema,
  type AutomationSchedulerStatusV1,
} from '../../contracts/generated.ts';

export const automationSchedulerKey = ['automation', 'scheduler'] as const;

export const schedulerStatusUrl = '/api/automation/scheduler/status';

/**
 * Pause or resume the scheduler, returning the reading the server took after
 * applying the change.
 *
 * Pause and resume are separate routes rather than one route taking a boolean,
 * which makes each request idempotent: re-sending `pause` on an already-paused
 * scheduler is a no-op that still returns the true state, so a retry after a
 * dropped response cannot toggle something twice.
 */
export function setSchedulerPaused(
  url: string,
): Promise<LegacyWriteResult<AutomationSchedulerStatusV1>> {
  return fetchLegacyWrite(url, AutomationSchedulerStatusV1Schema, { method: 'POST' });
}

/**
 * What a control attempt produced, including the case where there was no
 * attempt.
 *
 * `not_dispatched` is not a failure of the write — it is the absence of one,
 * and it stays separate for the same reason Settings keeps `unavailable` apart
 * from `error`: nothing was sent, so nothing changed, and the surface must not
 * imply the scheduler was asked and refused.
 */
export type SchedulerControlResult =
  | LegacyWriteResult<AutomationSchedulerStatusV1>
  | { outcome: 'not_dispatched'; writability: ScopeWritability };

/**
 * The scope a control attempt was issued under, captured when it was issued.
 *
 * Carried as mutation context rather than read again at settlement, because
 * the two moments can disagree. `useSchedulerControl` derives its key from the
 * scope of the render it last ran in, and React Query invokes the settlement
 * callbacks from the CURRENT options — so a pause dispatched against project A
 * that is still in flight when the reader switches to project B would have
 * settled against B's key: A's scheduler reading written into B's cache entry,
 * or B's entry invalidated because A's write failed. Either way one project's
 * panel would be answering for another's, which is the one thing a scoped
 * surface may never do.
 */
interface SchedulerDispatch {
  /** The status cache entry belonging to the project the write was sent to. */
  readonly statusKey: readonly unknown[];
}

/**
 * The scheduler control as a mutation.
 *
 * On success the returned reading is written straight into the status query's
 * cache entry, so the badge and tiles update from the server's own answer
 * rather than from a refetch that could race, and without a window where the
 * screen shows the pre-control state as though the control had not run. It is
 * written to the entry belonging to the project that was dispatched to — see
 * {@link SchedulerDispatch}.
 *
 * Returns the scope authority alongside the mutation, so the control that
 * renders the button and the mutation that would dispatch it read the same
 * value rather than each taking their own.
 */
export function useSchedulerControl() {
  const scope = useScope((s) => s.scope);
  const client = useQueryClient();
  const statusKey = [...automationSchedulerKey, scopeKey(scope)];
  // The control's own reading of the scope authority, so what disables the
  // button and what would refuse a dispatch are one value rather than two
  // that can drift.
  const writability = scopeWritable(scope);
  const mutation = useMutation<SchedulerControlResult, Error, boolean, SchedulerDispatch>({
    // Runs immediately before `mutationFn`, from the same options snapshot, so
    // this is the scope the request is actually about to be sent under.
    onMutate: () => ({ statusKey }),
    mutationFn: async (paused: boolean) => {
      // Nothing leaves the browser unless the scope is known to accept it. The
      // button is disabled on this same reading, so arriving here means the
      // disable was bypassed — and dispatching anyway would trade a stated
      // reason for a 405 that this layer cannot tell apart from a route that
      // has gone away.
      if (writability.state !== 'writable') {
        return { outcome: 'not_dispatched', writability };
      }
      return setSchedulerPaused(
        scopedUrl(scope, `/api/automation/scheduler/${paused ? 'pause' : 'resume'}`),
      );
    },
    onSuccess: (result, _paused, dispatch) => {
      // The dispatch's own key, never the key of whatever scope is on screen
      // by the time the daemon answers.
      const target = dispatch?.statusKey ?? statusKey;
      // Only a genuine reading may replace the cached one. A transport failure
      // or an unparseable body is reported by the caller from this same result
      // and must leave the last real reading in place.
      if (result.outcome === 'ok') {
        client.setQueryData(target, result);
        return;
      }
      // A write that never went out cannot have changed the server's reading,
      // so there is nothing to re-read.
      if (result.outcome === 'not_dispatched') return;
      void client.invalidateQueries({ queryKey: target });
    },
  });
  return { ...mutation, writability };
}
