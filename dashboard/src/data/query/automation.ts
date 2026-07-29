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

import { fetchLegacy, type LegacyResult } from './legacy.ts';
import { scopeKey, scopedUrl, useScope } from '../scope/store.ts';
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
): Promise<LegacyResult<AutomationSchedulerStatusV1>> {
  return fetchLegacy(url, AutomationSchedulerStatusV1Schema, { method: 'POST' });
}

/**
 * The scheduler control as a mutation.
 *
 * On success the returned reading is written straight into the status query's
 * cache entry, so the badge and tiles update from the server's own answer
 * rather than from a refetch that could race, and without a window where the
 * screen shows the pre-control state as though the control had not run.
 */
export function useSchedulerControl() {
  const scope = useScope((s) => s.scope);
  const client = useQueryClient();
  const statusKey = [...automationSchedulerKey, scopeKey(scope)];
  return useMutation({
    mutationFn: (paused: boolean) =>
      setSchedulerPaused(
        scopedUrl(scope, `/api/automation/scheduler/${paused ? 'pause' : 'resume'}`),
      ),
    onSuccess: (result) => {
      // Only a genuine reading may replace the cached one. A transport failure
      // or an unparseable body is reported by the caller from this same result
      // and must leave the last real reading in place.
      if (result.outcome === 'ok') {
        client.setQueryData(statusKey, result);
        return;
      }
      void client.invalidateQueries({ queryKey: statusKey });
    },
  });
}
