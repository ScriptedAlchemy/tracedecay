import { useEffect, useState } from "react";

import type { api as defaultApi } from "../api";

/** Badge freshness only — no need to hammer the scheduler-status endpoint. */
const POLL_MS = 60_000;

export type PendingAutomationCountsApi = Pick<typeof defaultApi, "getAutomationSchedulerStatus">;

/**
 * Managed-skill drafts awaiting human review, sourced from the additive counts
 * on `/api/automation/scheduler/status`. Fact proposal counts remain telemetry
 * and do not drive review badges.
 */
export function usePendingAutomationCounts(api: PendingAutomationCountsApi): number {
  const [total, setTotal] = useState(0);

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const load = async () => {
      try {
        const status = await api.getAutomationSchedulerStatus();
        if (!cancelled) {
          setTotal(status.pending_skills ?? 0);
        }
      } catch {
        // Advisory badge: keep the last known count on transient errors.
      }
      if (!cancelled) timer = setTimeout(() => void load(), POLL_MS);
    };
    void load();

    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, [api]);

  return total;
}
