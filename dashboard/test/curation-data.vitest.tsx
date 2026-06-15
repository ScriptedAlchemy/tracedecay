import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { useCurationData } from "../holographic/src/curation/useCurationData";

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function makeApi(overrides = {}) {
  return {
    getMemoryCuratorPreview: vi.fn().mockResolvedValue({ report: null, saved_at: null }),
    getMemoryCuratorActivity: vi.fn().mockResolvedValue({ events: [] }),
    postMemoryCurate: vi.fn().mockResolvedValue({ dry_run: true, actions: [], counts: {} }),
    getMemoryCuratorStatus: vi.fn().mockResolvedValue({ runs: [] }),
    getMemoryOplog: vi.fn().mockResolvedValue({ events: [] }),
    ...overrides,
  };
}

describe("useCurationData", () => {
  it("preview() flips to activity while running, then lands on plan with a fresh saved preview timestamp", async () => {
    vi.useFakeTimers();
    const run = deferred();
    const savedPreview = { dry_run: true, actions: [{ op: "retag" }], counts: { retag: 1 } };
    const api = makeApi({
      postMemoryCurate: vi.fn().mockImplementation(() => run.promise),
      getMemoryCuratorPreview: vi.fn()
        .mockResolvedValueOnce({ report: null, saved_at: null })
        .mockResolvedValue({ report: savedPreview, saved_at: "2026-06-14T12:00:00.000Z" }),
    });

    const { result } = renderHook(() =>
      useCurationData({
        api,
        now: () => "2026-06-14T12:00:00.000Z",
      }),
    );

    await act(async () => {
      await Promise.resolve();
    });

    let pending;
    act(() => {
      pending = result.current.preview();
    });
    expect(result.current.loading).toBe(true);
    expect(result.current.activeTab).toBe("activity");
    expect(api.getMemoryCuratorActivity).toHaveBeenCalled();

    await act(async () => {
      run.resolve({ dry_run: true, actions: [{ op: "retag" }], counts: { retag: 1 } });
      await pending;
      await Promise.resolve();
    });

    expect(result.current.loading).toBe(false);
    expect(result.current.activeTab).toBe("plan");
    expect(result.current.report).toMatchObject({ dry_run: true, actions: [{ op: "retag" }] });
    expect(result.current.previewSavedAt).toBe("2026-06-14T12:00:00.000Z");
    expect(result.current.previewStale).toBe(false);
    vi.useRealTimers();
  });

  it("apply() clears the saved preview, closes confirmation, and notifies the parent callback", async () => {
    const applyRun = deferred();
    const onApplied = vi.fn();
    const savedPreview = { dry_run: true, actions: [{ op: "delete" }], counts: { delete: 1 } };
    const api = makeApi({
      getMemoryCuratorPreview: vi.fn()
        .mockResolvedValueOnce({ report: null, saved_at: null })
        .mockResolvedValue({ report: savedPreview, saved_at: "2026-06-14T12:00:00.000Z" }),
      postMemoryCurate: vi.fn().mockImplementation(({ dry_run }) =>
        dry_run ? Promise.resolve(savedPreview) : applyRun.promise,
      ),
    });

    const { result } = renderHook(() =>
      useCurationData({ api, onApplied, now: () => "2026-06-14T12:00:00.000Z" }),
    );

    await act(async () => {
      await Promise.resolve();
    });

    await act(async () => {
      await result.current.preview();
    });

    expect(result.current.previewSavedAt).toBe("2026-06-14T12:00:00.000Z");
    act(() => result.current.setConfirmOpen(true));

    let pending;
    act(() => {
      pending = result.current.apply();
    });
    expect(result.current.applying).toBe(true);
    expect(result.current.activeTab).toBe("activity");

    await act(async () => {
      applyRun.resolve({ dry_run: false, actions: [], counts: {}, applied_counts: { delete: 1 } });
      await pending;
      await Promise.resolve();
    });

    expect(result.current.applying).toBe(false);
    expect(result.current.confirmOpen).toBe(false);
    expect(result.current.previewStale).toBe(false);
    expect(onApplied).toHaveBeenCalledTimes(1);
  });

  it("polling respects panel visibility and skips hidden activity tabs", async () => {
    vi.useFakeTimers();
    const api = makeApi();
    const { result } = renderHook(() =>
      useCurationData({ api, pollFastMs: 900, pollIdleMs: 2500 }),
    );

    await act(async () => {
      await Promise.resolve();
    });

    const panel = document.createElement("div");
    Object.defineProperty(panel, "offsetParent", { configurable: true, get: () => null });
    result.current.panelRef.current = panel;
    act(() => result.current.setActiveTab("activity"));
    api.getMemoryCuratorActivity.mockClear();

    await act(async () => {
      vi.advanceTimersByTime(2500);
    });
    expect(api.getMemoryCuratorActivity).not.toHaveBeenCalled();

    Object.defineProperty(panel, "offsetParent", { configurable: true, get: () => ({ nodeName: "DIV" }) });
    await act(async () => {
      vi.advanceTimersByTime(2500);
    });
    expect(api.getMemoryCuratorActivity).toHaveBeenCalledTimes(1);
    vi.useRealTimers();
  });
});
