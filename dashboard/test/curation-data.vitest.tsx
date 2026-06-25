import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useCurationData, type CurationApi } from "../holographic/src/curation/useCurationData";

afterEach(() => {
  vi.useRealTimers();
});

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T | PromiseLike<T>) => void;
  reject: (reason?: unknown) => void;
}

function deferred<T = unknown>(): Deferred<T> {
  let resolve!: Deferred<T>["resolve"];
  let reject!: Deferred<T>["reject"];
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function makeApi(overrides: Partial<CurationApi> = {}): CurationApi {
  return {
    getMemoryCuratorPreview: vi.fn().mockResolvedValue({ report: null, saved_at: null }),
    getMemoryCuratorActivity: vi.fn().mockResolvedValue({ events: [] }),
    getMemoryAutomationRuns: vi.fn().mockResolvedValue({ records: [] }),
    getAutomationSchedulerStatus: vi.fn().mockResolvedValue({
      status: "paused",
      paused: true,
      enabled: false,
      scheduler_tick_secs: 60,
      now: 1782283200,
      tasks: [
        { task: "memory_curator", due: false, skip_reason: "automation_disabled" },
        { task: "session_reflector", due: false, skip_reason: "automation_disabled" },
        { task: "skill_writer", due: false, skip_reason: "automation_disabled" },
      ],
    }),
    pauseAutomationScheduler: vi.fn().mockResolvedValue({
      status: "paused",
      paused: true,
      enabled: false,
      scheduler_tick_secs: 60,
      now: 1782283200,
      tasks: [],
    }),
    resumeAutomationScheduler: vi.fn().mockResolvedValue({
      status: "configured",
      paused: false,
      enabled: true,
      scheduler_tick_secs: 60,
      now: 1782283200,
      tasks: [],
    }),
    getMemoryAutomationRunArtifacts: vi.fn().mockResolvedValue({ artifacts: [], count: 0 }),
    getMemoryAutomationRunArtifact: vi.fn().mockResolvedValue({
      run_id: "memory-run",
      artifact: {
        schema_version: 1,
        kind: "codex_handoff",
        path: "automation_artifacts/memory-run/codex_handoff.json",
        sha256: "sha256:test",
        created_at: "2026-06-24T00:00:00Z",
      },
      payload: {},
    }),
    getFactProposals: vi.fn().mockResolvedValue({ proposals: [], count: 0, limit: 50 }),
    applyFactProposal: vi.fn(),
    rejectFactProposal: vi.fn(),
    getManagedSkills: vi.fn().mockResolvedValue({ skills: [], skill_metadata: [], count: 0 }),
    getManagedSkill: vi.fn(),
    approveManagedSkill: vi.fn(),
    discardManagedSkillUpdate: vi.fn(),
    disableManagedSkill: vi.fn(),
    archiveManagedSkill: vi.fn(),
    restoreManagedSkill: vi.fn(),
    postMemoryCurate: vi.fn().mockResolvedValue({ dry_run: true, actions: [], counts: {} }),
    postAutomationRunMemoryCurator: vi.fn().mockResolvedValue({
      run_id: "memory-run",
      dry_run: true,
      status: "skipped",
      report: { dry_run: true, actions: [], counts: {}, reason: "automation_disabled" },
      ledger_record: {
        schema_version: 1,
        run_id: "memory-run",
        trigger: "dashboard",
        task: "memory_curator",
        backend: "disabled",
        status: "skipped",
        accepted_count: 0,
        rejected_count: 0,
        error: "automation_disabled",
        started_at: "2026-06-24T00:00:00Z",
        completed_at: "2026-06-24T00:00:01Z",
      },
    }),
    postAutomationRunSessionReflection: vi.fn().mockResolvedValue({
      run_id: "reflection-run",
      dry_run: true,
      status: "skipped",
      report: { status: "skipped", reason: "automation_disabled" },
      ledger_record: {
        schema_version: 1,
        run_id: "reflection-run",
        trigger: "dashboard",
        task: "session_reflector",
        backend: "disabled",
        status: "skipped",
        accepted_count: 0,
        rejected_count: 0,
        error: "automation_disabled",
        started_at: "2026-06-24T00:00:00Z",
        completed_at: "2026-06-24T00:00:01Z",
      },
    }),
    postAutomationRunSkillWriting: vi.fn().mockResolvedValue({
      run_id: "skill-run",
      dry_run: true,
      status: "skipped",
      report: { status: "skipped", reason: "automation_disabled" },
      ledger_record: {
        schema_version: 1,
        run_id: "skill-run",
        trigger: "dashboard",
        task: "skill_writer",
        backend: "disabled",
        status: "skipped",
        accepted_count: 0,
        rejected_count: 0,
        error: "automation_disabled",
        started_at: "2026-06-24T00:00:00Z",
        completed_at: "2026-06-24T00:00:01Z",
      },
    }),
    getMemoryCuratorStatus: vi.fn().mockResolvedValue({ runs: [] }),
    getMemoryAutomationConfig: vi.fn().mockResolvedValue({
      global: null,
      project: null,
      effective: {
        enabled: false,
        backend: "disabled",
        host_mode: "standalone",
        model: null,
        timeout_secs: 60,
        scheduler_tick_secs: 60,
        max_tokens: null,
        temperature: null,
        require_dashboard_approval: true,
        auto_apply_memory_ops: false,
        auto_enable_skills: false,
        tasks: {
          memory_curator: { enabled: false, schedule: null },
          session_reflector: { enabled: false, schedule: null },
          skill_writer: { enabled: false, schedule: null },
        },
      },
    }),
    patchMemoryAutomationConfig: vi.fn().mockImplementation((patch) =>
      Promise.resolve({
        global: null,
        project: patch,
        effective: {
          enabled: patch.enabled ?? false,
          backend: patch.backend ?? "disabled",
          host_mode: patch.host_mode ?? "standalone",
          model: patch.model ?? null,
          timeout_secs: patch.timeout_secs ?? 60,
          scheduler_tick_secs: patch.scheduler_tick_secs ?? 60,
          max_tokens: patch.max_tokens ?? null,
          temperature: patch.temperature ?? null,
          require_dashboard_approval: patch.require_dashboard_approval ?? true,
          auto_apply_memory_ops: patch.auto_apply_memory_ops ?? false,
          auto_enable_skills: patch.auto_enable_skills ?? false,
          tasks: {
            memory_curator: patch.memory_curator ?? { enabled: false, schedule: null },
            session_reflector: patch.session_reflector ?? { enabled: false, schedule: null },
            skill_writer: patch.skill_writer ?? { enabled: false, schedule: null },
          },
        },
      }),
    ),
    resetMemoryAutomationConfig: vi.fn().mockResolvedValue({
      global: null,
      project: null,
      effective: {
        enabled: false,
        backend: "disabled",
        host_mode: "standalone",
        model: null,
        timeout_secs: 60,
        scheduler_tick_secs: 60,
        max_tokens: null,
        temperature: null,
        require_dashboard_approval: true,
        auto_apply_memory_ops: false,
        auto_enable_skills: false,
        tasks: {
          memory_curator: { enabled: false, schedule: null },
          session_reflector: { enabled: false, schedule: null },
          skill_writer: { enabled: false, schedule: null },
        },
      },
    }),
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

    let pending!: Promise<unknown>;
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

    let pending!: Promise<unknown>;
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
    vi.mocked(api.getMemoryCuratorActivity).mockClear();

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

  it("loads, edits, saves, and resets the automation config draft", async () => {
    const api = makeApi();
    const { result } = renderHook(() => useCurationData({ api }));

    await waitFor(() => {
      expect(result.current.configDraft?.enabled).toBe(false);
    });

    expect(result.current.configDraft?.enabled).toBe(false);

    act(() => {
      result.current.updateConfigDraft({
        enabled: true,
        model: "project-model",
        scheduler_tick_secs: 15,
        max_tokens: 4096,
        temperature: 0.2,
      });
      result.current.updateConfigTaskDraft("memory_curator", {
        enabled: true,
        schedule: "manual",
        interval_secs: 900,
        cooldown_secs: 300,
        min_idle_secs: 120,
        stale_lock_secs: 3600,
      });
    });

    expect(result.current.configDirty).toBe(true);
    expect(result.current.configDraft.enabled).toBe(true);
    expect(result.current.configDraft.tasks.memory_curator.schedule).toBe("manual");

    await act(async () => {
      await result.current.saveConfigDraft();
    });

    expect(api.patchMemoryAutomationConfig).toHaveBeenCalledWith(
      expect.objectContaining({
        enabled: true,
        model: "project-model",
        scheduler_tick_secs: 15,
        max_tokens: 4096,
        temperature: 0.2,
        memory_curator: {
          enabled: true,
          schedule: "manual",
          interval_secs: 900,
          cooldown_secs: 300,
          min_idle_secs: 120,
          stale_lock_secs: 3600,
        },
      }),
    );
    expect(result.current.configDirty).toBe(false);

    act(() => result.current.updateConfigDraft({ model: "changed" }));
    expect(result.current.configDirty).toBe(true);
    act(() => result.current.resetConfigDraft());
    expect(result.current.configDraft.model).toBe("project-model");
    expect(result.current.configDirty).toBe(false);
  });

  it("resets persisted automation overrides back to defaults", async () => {
    const api = makeApi({
      getMemoryAutomationConfig: vi.fn().mockResolvedValue({
        global: null,
        project: { model: "project-model" },
        effective: {
          enabled: true,
          backend: "codex_app_server",
          host_mode: "standalone",
          model: "project-model",
          timeout_secs: 90,
          scheduler_tick_secs: 20,
          max_tokens: 4096,
          temperature: 0.2,
          require_dashboard_approval: true,
          auto_apply_memory_ops: false,
          auto_enable_skills: false,
          tasks: {
            memory_curator: { enabled: true, schedule: "manual" },
            session_reflector: { enabled: false, schedule: null },
            skill_writer: { enabled: false, schedule: null },
          },
        },
      }),
    });
    const { result } = renderHook(() => useCurationData({ api }));

    await waitFor(() => {
      expect(result.current.configDraft?.model).toBe("project-model");
    });

    await act(async () => {
      await result.current.resetConfigToDefaults();
    });

    expect(api.resetMemoryAutomationConfig).toHaveBeenCalledTimes(1);
    expect(result.current.configResponse?.project).toBeNull();
    expect(result.current.configDraft?.model).toBeNull();
    expect(result.current.configDirty).toBe(false);
  });

  it("pauses and resumes scheduler through the dashboard scheduler API", async () => {
    const api = makeApi({
      getMemoryAutomationConfig: vi
        .fn()
        .mockResolvedValueOnce({
          global: null,
          project: null,
          effective: {
            enabled: true,
            backend: "codex_app_server",
            host_mode: "standalone",
            model: null,
            timeout_secs: 60,
            scheduler_tick_secs: 60,
            max_tokens: null,
            temperature: null,
            require_dashboard_approval: true,
            auto_apply_memory_ops: false,
            auto_enable_skills: false,
            tasks: {
              memory_curator: { enabled: true, schedule: "interval" },
              session_reflector: { enabled: false, schedule: null },
              skill_writer: { enabled: false, schedule: null },
            },
          },
        })
        .mockResolvedValueOnce({
          global: null,
          project: null,
          effective: {
            enabled: true,
            backend: "codex_app_server",
            host_mode: "standalone",
            model: null,
            timeout_secs: 60,
            scheduler_tick_secs: 60,
            max_tokens: null,
            temperature: null,
            require_dashboard_approval: true,
            auto_apply_memory_ops: false,
            auto_enable_skills: false,
            tasks: {
              memory_curator: { enabled: true, schedule: "interval" },
              session_reflector: { enabled: false, schedule: null },
              skill_writer: { enabled: false, schedule: null },
            },
          },
        })
        .mockResolvedValueOnce({
          global: null,
          project: null,
          effective: {
            enabled: true,
            backend: "codex_app_server",
            host_mode: "standalone",
            model: null,
            timeout_secs: 60,
            scheduler_tick_secs: 60,
            max_tokens: null,
            temperature: null,
            require_dashboard_approval: true,
            auto_apply_memory_ops: false,
            auto_enable_skills: false,
            tasks: {
              memory_curator: { enabled: true, schedule: "interval" },
              session_reflector: { enabled: false, schedule: null },
              skill_writer: { enabled: false, schedule: null },
            },
          },
        }),
      pauseAutomationScheduler: vi.fn().mockResolvedValue({
        status: "paused",
        paused: true,
        enabled: true,
        scheduler_tick_secs: 60,
        now: 1782283200,
        tasks: [{ task: "memory_curator", due: false, skip_reason: "scheduler_paused" }],
      }),
      resumeAutomationScheduler: vi.fn().mockResolvedValue({
        status: "configured",
        paused: false,
        enabled: true,
        scheduler_tick_secs: 60,
        now: 1782283200,
        tasks: [{ task: "memory_curator", due: true, skip_reason: null }],
      }),
    });
    const { result } = renderHook(() => useCurationData({ api }));

    await waitFor(() => {
      expect(result.current.configDraft?.enabled).toBe(true);
    });

    await act(async () => {
      await result.current.setSchedulerPaused(true);
    });

    expect(api.pauseAutomationScheduler).toHaveBeenCalledTimes(1);
    expect(api.getMemoryAutomationConfig).toHaveBeenCalledTimes(2);
    expect(result.current.schedulerStatus?.paused).toBe(true);
    expect(result.current.configDraft?.enabled).toBe(true);

    await act(async () => {
      await result.current.setSchedulerPaused(false);
    });

    expect(api.resumeAutomationScheduler).toHaveBeenCalledTimes(1);
    expect(api.getMemoryAutomationConfig).toHaveBeenCalledTimes(3);
    expect(result.current.schedulerStatus?.paused).toBe(false);
    expect(result.current.configDraft?.enabled).toBe(true);
  });

  it("indexes backend automation config validation errors by field", async () => {
    const validationError = Object.assign(
      new Error("automation timeout_secs must be greater than zero"),
      {
        body: {
          validation_errors: [
            {
              field: "timeout_secs",
              message: "automation timeout_secs must be greater than zero",
            },
          ],
        },
      },
    );
    const api = makeApi({
      patchMemoryAutomationConfig: vi.fn().mockRejectedValue(validationError),
    });
    const { result } = renderHook(() => useCurationData({ api }));

    await waitFor(() => {
      expect(result.current.configDraft).toBeTruthy();
    });

    act(() => {
      result.current.updateConfigDraft({
        timeout_secs: 0,
      });
    });

    let saveError: unknown;
    await act(async () => {
      try {
        await result.current.saveConfigDraft();
      } catch (err) {
        saveError = err;
      }
    });

    expect(saveError).toBe(validationError);
    expect(result.current.configFieldErrors.timeout_secs).toContain(
      "timeout_secs",
    );
    expect(result.current.configDirty).toBe(true);
  });

  it("loads automation run history when the history tab opens", async () => {
    const api = makeApi({
      getMemoryAutomationRuns: vi.fn().mockResolvedValue({
        records: [
          {
            schema_version: 1,
            run_id: "run-1",
            trigger: "dashboard",
            task: "memory_curator",
            backend: "disabled",
            status: "skipped",
            accepted_count: 0,
            rejected_count: 0,
            error: "automation_disabled",
            started_at: "2026-06-24T00:00:00Z",
            completed_at: "2026-06-24T00:00:01Z",
          },
        ],
        count: 1,
        limit: 20,
        error: "",
      }),
    });
    const { result } = renderHook(() => useCurationData({ api }));

    await waitFor(() => {
      expect(result.current.configDraft).toBeTruthy();
    });

    act(() => result.current.setActiveTab("history"));

    await waitFor(() => {
      expect(api.getMemoryAutomationRuns).toHaveBeenCalledWith({ limit: 20 });
      expect(result.current.automationRuns).toHaveLength(1);
    });
  });

  it("loads verified automation run artifact payloads", async () => {
    const api = makeApi({
      getMemoryAutomationRunArtifact: vi.fn().mockResolvedValue({
        run_id: "run-1",
        artifact: {
          schema_version: 1,
          kind: "codex_handoff",
          path: "automation_artifacts/run-1/codex_handoff.json",
          sha256: "sha256:payload",
          created_at: "2026-06-24T00:00:00Z",
        },
        payload: {
          status: "ready_for_review",
          next_actions: ["review dashboard artifact payload"],
        },
        error: "",
      }),
    });
    const { result } = renderHook(() => useCurationData({ api }));

    await waitFor(() => {
      expect(result.current.configDraft).toBeTruthy();
    });

    await act(async () => {
      await result.current.loadAutomationRunArtifact("run-1", "codex_handoff");
    });

    expect(api.getMemoryAutomationRunArtifact).toHaveBeenCalledWith("run-1", "codex_handoff");
    expect(result.current.automationRunArtifact?.payload).toMatchObject({
      status: "ready_for_review",
    });
  });

  it("runs standalone automation tasks and refreshes dependent dashboard data", async () => {
    const api = makeApi();
    const { result } = renderHook(() => useCurationData({ api }));

    await waitFor(() => {
      expect(result.current.configDraft).toBeTruthy();
    });

    await act(async () => {
      await result.current.runAutomationTask("session_reflector");
    });

    expect(api.postAutomationRunSessionReflection).toHaveBeenCalledWith({ dry_run: true });
    expect(api.getMemoryAutomationRuns).toHaveBeenCalledWith({ limit: 20 });
    expect(api.getFactProposals).toHaveBeenCalledWith({ limit: 50 });

    await act(async () => {
      await result.current.runAutomationTask("skill_writer");
    });

    expect(api.postAutomationRunSkillWriting).toHaveBeenCalledWith({ dry_run: true });
    expect(api.getManagedSkills).toHaveBeenCalled();

    await act(async () => {
      await result.current.runAutomationTask("memory_curator");
    });

    expect(api.postAutomationRunMemoryCurator).toHaveBeenCalledWith({ dry_run: true });
    expect(api.getMemoryCuratorActivity).toHaveBeenCalled();
  });

  it("tracks queued automation runs without requiring a report and polls until terminal", async () => {
    const queuedRun = {
      schema_version: 2,
      run_id: "queued-memory-run",
      trigger: "dashboard",
      task: "memory_curator",
      backend: "codex_app_server",
      status: "queued",
      accepted_count: 0,
      rejected_count: 0,
      started_at: "2026-06-24T00:00:00Z",
      completed_at: "2026-06-24T00:00:00Z",
    };
    const succeededRun = {
      ...queuedRun,
      status: "succeeded",
      completed_at: "2026-06-24T00:00:01Z",
    };
    const api = makeApi({
      postAutomationRunMemoryCurator: vi.fn().mockResolvedValue({
        run_id: "queued-memory-run",
        dry_run: true,
        status: "queued",
        ledger_record: queuedRun,
      }),
      getMemoryAutomationRuns: vi.fn()
        .mockResolvedValueOnce({ records: [queuedRun], count: 1, limit: 20, error: "" })
        .mockResolvedValueOnce({ records: [queuedRun], count: 1, limit: 20, error: "" })
        .mockResolvedValueOnce({ records: [succeededRun], count: 1, limit: 20, error: "" }),
    });
    const { result } = renderHook(() =>
      useCurationData({ api, pollFastMs: 25 }),
    );

    await waitFor(() => {
      expect(result.current.configDraft).toBeTruthy();
    });

    vi.useFakeTimers();

    await act(async () => {
      await result.current.runAutomationTask("memory_curator");
    });

    expect(result.current.report).toBeNull();
    expect(result.current.automationRuns[0]).toMatchObject({
      run_id: "queued-memory-run",
      status: "queued",
    });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(25);
    });

    expect(result.current.automationRuns[0]).toMatchObject({
      run_id: "queued-memory-run",
      status: "succeeded",
    });
  });

  it("refreshes automation outputs when polling observes terminal runs", async () => {
    const queuedMemoryRun = {
      schema_version: 2,
      run_id: "queued-memory-run",
      trigger: "dashboard",
      task: "memory_curator",
      backend: "codex_app_server",
      status: "queued",
      accepted_count: 0,
      rejected_count: 0,
      started_at: "2026-06-24T00:00:00Z",
      completed_at: "2026-06-24T00:00:00Z",
    };
    const queuedReflectionRun = {
      ...queuedMemoryRun,
      run_id: "queued-reflection-run",
      task: "session_reflector",
    };
    const queuedSkillRun = {
      ...queuedMemoryRun,
      run_id: "queued-skill-run",
      task: "skill_writer",
    };
    const api = makeApi({
      getMemoryAutomationRuns: vi.fn()
        .mockResolvedValueOnce({
          records: [queuedMemoryRun, queuedReflectionRun, queuedSkillRun],
          count: 3,
          limit: 20,
          error: "",
        })
        .mockResolvedValueOnce({
          records: [
            { ...queuedMemoryRun, status: "succeeded" },
            { ...queuedReflectionRun, status: "succeeded" },
            { ...queuedSkillRun, status: "succeeded" },
          ],
          count: 3,
          limit: 20,
          error: "",
        }),
    });
    const { result } = renderHook(() =>
      useCurationData({ api, pollFastMs: 25 }),
    );

    await waitFor(() => {
      expect(result.current.configDraft).toBeTruthy();
    });

    vi.useFakeTimers();

    await act(async () => {
      await result.current.loadAutomationRuns();
    });

    vi.mocked(api.getMemoryCuratorPreview).mockClear();
    vi.mocked(api.getMemoryCuratorActivity).mockClear();
    vi.mocked(api.getMemoryCuratorStatus).mockClear();
    vi.mocked(api.getFactProposals).mockClear();
    vi.mocked(api.getManagedSkills).mockClear();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(25);
    });

    expect(api.getMemoryCuratorPreview).toHaveBeenCalledTimes(1);
    expect(api.getMemoryCuratorActivity).toHaveBeenCalledTimes(1);
    expect(api.getMemoryCuratorStatus).toHaveBeenCalledTimes(1);
    expect(api.getFactProposals).toHaveBeenCalledTimes(1);
    expect(api.getManagedSkills).toHaveBeenCalledTimes(1);
  });

  it("loads and applies fact proposals from the history tab", async () => {
    const pendingProposal = {
      schema_version: 1,
      proposal_id: "prop-1",
      run_id: "run-1",
      state: "pending_approval",
      add_fact_request: { content: "Prefer bounded evidence." },
      created_at: 1782283200,
      updated_at: 1782283200,
    };
    const appliedProposal = {
      ...pendingProposal,
      state: "applied",
      applied_fact_id: 42,
      updated_at: 1782283300,
    };
    const api = makeApi({
      getFactProposals: vi.fn()
        .mockResolvedValueOnce({
          proposals: [pendingProposal],
          count: 1,
          limit: 50,
        })
        .mockResolvedValue({
          proposals: [appliedProposal],
          count: 1,
          limit: 50,
        }),
      applyFactProposal: vi.fn().mockResolvedValue({ proposal: appliedProposal }),
    });
    const { result } = renderHook(() => useCurationData({ api }));

    await waitFor(() => {
      expect(result.current.configDraft).toBeTruthy();
    });

    act(() => result.current.setActiveTab("history"));

    await waitFor(() => {
      expect(api.getFactProposals).toHaveBeenCalledWith({ limit: 50 });
      expect(result.current.factProposals[0].proposal_id).toBe("prop-1");
    });

    await act(async () => {
      await result.current.runFactProposalAction("apply", "prop-1");
    });

    expect(api.applyFactProposal).toHaveBeenCalledWith("prop-1");
    expect(result.current.factProposals[0].state).toBe("applied");
  });

  it("loads and approves managed skills from the history tab", async () => {
    const pendingSkill = {
      metadata: {
        id: "repo-hygiene",
        title: "Repo Hygiene",
        summary: "Keep repo maintenance consistent.",
        category: "workflow",
        state: "pending_approval",
        checksum: "abc123",
        pinned: false,
        created_at: 1782259200,
        updated_at: 1782259200,
        provenance: { source: "automation_run", actor: "skill_writer", run_id: "run-1" },
      },
      body_markdown: "Use focused checks before broad suites.",
      support_files: [],
    };
    const activeSkill = {
      ...pendingSkill,
      metadata: { ...pendingSkill.metadata, state: "active" },
    };
    const api = makeApi({
      getManagedSkills: vi.fn()
        .mockResolvedValueOnce({
          skills: [pendingSkill],
          skill_metadata: [pendingSkill.metadata],
          improvement_recommendations: [
            {
              skill_id: "repo-hygiene",
              improvement: true,
              recommendation: "patch_review",
              reason: "repeated patches suggest the skill instructions may still be unstable",
              priority: "medium",
              evidence: ["patches=2"],
            },
          ],
          count: 1,
        })
        .mockResolvedValue({
          skills: [activeSkill],
          skill_metadata: [activeSkill.metadata],
          improvement_recommendations: [
            {
              skill_id: "repo-hygiene",
              improvement: true,
              recommendation: "patch_review",
              reason: "repeated patches suggest the skill instructions may still be unstable",
              priority: "medium",
              evidence: ["patches=2"],
            },
          ],
          count: 1,
        }),
      getManagedSkill: vi.fn()
        .mockResolvedValueOnce({ skill: pendingSkill })
        .mockResolvedValue({ skill: activeSkill }),
      approveManagedSkill: vi.fn().mockResolvedValue({ skill: activeSkill }),
    });
    const { result } = renderHook(() => useCurationData({ api }));

    await waitFor(() => {
      expect(result.current.configDraft).toBeTruthy();
    });

    act(() => result.current.setActiveTab("history"));

    await waitFor(() => {
      expect(api.getManagedSkills).toHaveBeenCalled();
      expect(result.current.selectedManagedSkill?.metadata.id).toBe("repo-hygiene");
    });
    expect(
      result.current.managedSkillImprovementRecommendations["repo-hygiene"].recommendation,
    ).toBe("patch_review");

    await act(async () => {
      await result.current.runManagedSkillAction("approve", "repo-hygiene");
    });

    expect(api.approveManagedSkill).toHaveBeenCalledWith("repo-hygiene");
    expect(result.current.selectedManagedSkill?.metadata.state).toBe("active");
  });
});
