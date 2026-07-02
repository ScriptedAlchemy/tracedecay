import { renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import {
  usePendingAutomationCounts,
  type PendingAutomationCountsApi,
} from "../holographic/src/curation/usePendingAutomationCounts";

function makeApi(
  overrides: Partial<Record<string, unknown>> = {},
): PendingAutomationCountsApi {
  return {
    getAutomationSchedulerStatus: vi.fn().mockResolvedValue({
      status: "configured",
      paused: false,
      enabled: true,
      scheduler_tick_secs: 60,
      now: 1782283200,
      tasks: [],
      ...overrides,
    }),
  };
}

describe("usePendingAutomationCounts", () => {
  it("sums pending fact proposals and skills from scheduler status", async () => {
    const api = makeApi({ pending_fact_proposals: 2, pending_skills: 1 });
    const { result } = renderHook(() => usePendingAutomationCounts(api));

    await waitFor(() => expect(result.current).toBe(3));
    expect(api.getAutomationSchedulerStatus).toHaveBeenCalledTimes(1);
  });

  it("treats missing counts from older servers as zero", async () => {
    const api = makeApi();
    const { result } = renderHook(() => usePendingAutomationCounts(api));

    await waitFor(() =>
      expect(api.getAutomationSchedulerStatus).toHaveBeenCalledTimes(1),
    );
    expect(result.current).toBe(0);
  });

  it("keeps the last known count when the endpoint errors", async () => {
    const getAutomationSchedulerStatus = vi
      .fn()
      .mockRejectedValue(new Error("offline"));
    const { result } = renderHook(() =>
      usePendingAutomationCounts({ getAutomationSchedulerStatus }),
    );

    await waitFor(() =>
      expect(getAutomationSchedulerStatus).toHaveBeenCalledTimes(1),
    );
    expect(result.current).toBe(0);
  });
});
