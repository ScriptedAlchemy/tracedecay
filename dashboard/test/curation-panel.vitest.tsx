import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import CurationPanel from "../holographic/src/CurationPanel";

const apiMock = vi.hoisted(() => ({
  getMemoryCuratorPreview: vi.fn().mockResolvedValue({ report: null, saved_at: null }),
  getMemoryCuratorActivity: vi.fn().mockResolvedValue({ events: [] }),
  getMemoryCuratorStatus: vi.fn().mockResolvedValue({
    provider: "tracedecay",
    state: { paused: false, run_count: 0 },
    config: { enabled: false },
    snapshots: [],
  }),
  getMemoryAutomationConfig: vi.fn().mockResolvedValue({
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
        memory_curator: { enabled: true, schedule: null },
        session_reflector: { enabled: true, schedule: null },
        skill_writer: { enabled: true, schedule: null },
      },
    },
    backend_availability: {
      backend: "codex_app_server",
      available: true,
      executable: "/usr/bin/codex",
      reason: null,
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
      backend_availability: {
        backend: patch.backend ?? "disabled",
        available: (patch.backend ?? "disabled") === "codex_app_server",
        executable: "/usr/bin/codex",
        reason: null,
      },
    }),
  ),
  resetMemoryAutomationConfig: vi.fn().mockResolvedValue({
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
        memory_curator: { enabled: true, schedule: null },
        session_reflector: { enabled: true, schedule: null },
        skill_writer: { enabled: true, schedule: null },
      },
    },
    backend_availability: {
      backend: "codex_app_server",
      available: true,
      executable: "/usr/bin/codex",
      reason: null,
    },
  }),
  getAutomationSchedulerStatus: vi.fn().mockResolvedValue({
    status: "configured",
    paused: false,
    enabled: true,
    scheduler_tick_secs: 60,
    now: 1782283200,
    tasks: [
      { task: "memory_curator", due: true, skip_reason: null },
      { task: "session_reflector", due: false, skip_reason: "task_disabled" },
      { task: "skill_writer", due: false, skip_reason: "scheduler_schedule_manual" },
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
  getMemoryAutomationRuns: vi.fn().mockResolvedValue({
    records: [
      {
        schema_version: 1,
        run_id: "run-dashboard-1",
        trigger: "dashboard",
        task: "memory_curator",
        backend: "codex_app_server",
        model: "gpt-test",
        status: "skipped",
        evidence_hash: null,
        proposed_ops: null,
        accepted_count: 0,
        rejected_count: 0,
        error: "automation_disabled",
        artifacts: [
          {
            schema_version: 1,
            kind: "codex_handoff",
            path: "automation_artifacts/run-dashboard-1/codex_handoff.json",
            sha256: "sha256:handoff",
            summary: "handoff ready",
            created_at: "2026-06-24T12:00:01Z",
          },
        ],
        started_at: "2026-06-24T12:00:00Z",
        completed_at: "2026-06-24T12:00:01Z",
      },
    ],
    count: 1,
    limit: 20,
    error: "",
  }),
  getMemoryAutomationRunArtifacts: vi.fn().mockResolvedValue({
    run_id: "run-dashboard-1",
    artifacts: [
      {
        schema_version: 1,
        kind: "codex_handoff",
        path: "automation_artifacts/run-dashboard-1/codex_handoff.json",
        sha256: "sha256:handoff",
        summary: "handoff ready",
        created_at: "2026-06-24T12:00:01Z",
      },
    ],
    artifact_chain: {
      expected_kinds: [
        "traces",
        "feedback",
        "generated_evals",
        "validation_gate",
        "optimizer_diagnosis",
        "codex_handoff",
      ],
      present_kinds: ["codex_handoff"],
      complete: false,
    },
    count: 1,
    error: "",
  }),
  getMemoryAutomationRunArtifact: vi.fn().mockResolvedValue({
    run_id: "run-dashboard-1",
    artifact: {
      schema_version: 1,
      kind: "codex_handoff",
      path: "automation_artifacts/run-dashboard-1/codex_handoff.json",
      sha256: "sha256:handoff",
      summary: "handoff ready",
      created_at: "2026-06-24T12:00:01Z",
    },
    payload: {
      status: "ready_for_review",
      next_actions: ["review dashboard artifact payload"],
    },
    error: "",
  }),
  getFactProposals: vi.fn().mockResolvedValue({
    proposals: [
      {
        schema_version: 1,
        proposal_id: "prop-dashboard-1",
        run_id: "run-dashboard-1",
        state: "pending_approval",
        add_fact_request: {
          content: "Prefer bounded evidence before adding durable memory.",
          category: "workflow",
          tags: ["memory", "review"],
        },
        created_at: 1782283200,
        updated_at: 1782283200,
      },
    ],
    count: 1,
    limit: 50,
    error: "",
  }),
  applyFactProposal: vi.fn().mockResolvedValue({
    proposal: {
      schema_version: 1,
      proposal_id: "prop-dashboard-1",
      run_id: "run-dashboard-1",
      state: "applied",
      add_fact_request: {
        content: "Prefer bounded evidence before adding durable memory.",
        category: "workflow",
        tags: ["memory", "review"],
      },
      applied_fact_id: 42,
      created_at: 1782283200,
      updated_at: 1782283300,
    },
  }),
  rejectFactProposal: vi.fn(),
  getManagedSkills: vi.fn().mockResolvedValue({
    skills: [
      {
        metadata: {
          id: "repo-hygiene",
          title: "Repo Hygiene",
          summary: "Keep repository maintenance tasks consistent.",
          category: "workflow",
          state: "pending_approval",
          checksum: "abc123",
          pinned: false,
          created_at: 1782302400,
          updated_at: 1782302400,
          provenance: {
            source: "automation_run",
            actor: "skill_writer",
            run_id: "run-dashboard-1",
          },
        },
        body_markdown: "Use focused checks before broad suites.",
        support_files: [],
      },
    ],
    skill_metadata: [],
    usage_summaries: [
      {
        schema_version: 1,
        skill_id: "repo-hygiene",
        title: "Repo Hygiene",
        category: "workflow",
        state: "pending_approval",
        pinned: false,
        created_by: "skill_writer",
        provenance_source: "automation_run",
        targets: ["codex", "cursor"],
        view_count: 2,
        use_count: 1,
        patch_count: 0,
        first_seen_at: 1782283200,
        last_activity_at: 1782283200,
        last_viewed_at: 1782283200,
        last_used_at: 1782283200,
        last_patched_at: null,
      },
    ],
    stale_recommendations: [
      {
        skill_id: "repo-hygiene",
        stale: false,
        recommendation: "keep",
        reason: "recent or meaningful activity is present",
        evidence: ["uses=1"],
      },
    ],
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
    error: "",
  }),
  getManagedSkill: vi.fn().mockResolvedValue({
    skill: {
      metadata: {
        id: "repo-hygiene",
        title: "Repo Hygiene",
        summary: "Keep repository maintenance tasks consistent.",
        category: "workflow",
        state: "pending_approval",
        checksum: "abc123",
        pinned: false,
        created_at: 1782302400,
        updated_at: 1782302400,
        provenance: {
          source: "automation_run",
          actor: "skill_writer",
          run_id: "run-dashboard-1",
        },
      },
      body_markdown: "Use focused checks before broad suites.",
      support_files: [],
    },
    usage_summary: {
      schema_version: 1,
      skill_id: "repo-hygiene",
      title: "Repo Hygiene",
      category: "workflow",
      state: "pending_approval",
      pinned: false,
      created_by: "skill_writer",
      provenance_source: "automation_run",
      targets: ["codex", "cursor"],
      view_count: 2,
      use_count: 1,
      patch_count: 0,
      first_seen_at: 1782283200,
      last_activity_at: 1782283200,
      last_viewed_at: 1782283200,
      last_used_at: 1782283200,
      last_patched_at: null,
    },
    stale_recommendation: {
      skill_id: "repo-hygiene",
      stale: false,
      recommendation: "keep",
      reason: "recent or meaningful activity is present",
      evidence: ["uses=1"],
    },
    improvement_recommendation: {
      skill_id: "repo-hygiene",
      improvement: true,
      recommendation: "patch_review",
      reason: "repeated patches suggest the skill instructions may still be unstable",
      priority: "medium",
      evidence: ["patches=2"],
    },
  }),
  approveManagedSkill: vi.fn().mockResolvedValue({
    skill: {
      metadata: {
        id: "repo-hygiene",
        title: "Repo Hygiene",
        summary: "Keep repository maintenance tasks consistent.",
        category: "workflow",
        state: "active",
        checksum: "abc123",
        pinned: false,
        created_at: 1782302400,
        updated_at: 1782302460,
        provenance: {
          source: "automation_run",
          actor: "skill_writer",
          run_id: "run-dashboard-1",
        },
      },
      body_markdown: "Use focused checks before broad suites.",
      support_files: [],
    },
  }),
  disableManagedSkill: vi.fn(),
  archiveManagedSkill: vi.fn(),
  restoreManagedSkill: vi.fn(),
  getMemoryOplog: vi.fn().mockResolvedValue({ events: [] }),
  postAutomationRunMemoryCurator: vi.fn().mockResolvedValue({
    run_id: "queued-memory-curator",
    dry_run: true,
    status: "queued",
    report: { queued: true },
    ledger_record: {
      schema_version: 2,
      run_id: "queued-memory-curator",
      trigger: "dashboard",
      task: "memory_curator",
      backend: "codex_app_server",
      host_mode: "standalone",
      model: null,
      status: "queued",
      accepted_count: 0,
      rejected_count: 0,
      started_at: "2026-06-24T12:00:02Z",
      completed_at: "2026-06-24T12:00:02Z",
    },
  }),
  postAutomationRunSessionReflection: vi.fn().mockResolvedValue({
    run_id: "queued-session-reflector",
    dry_run: true,
    status: "queued",
    ledger_record: {
      schema_version: 2,
      run_id: "queued-session-reflector",
      trigger: "dashboard",
      task: "session_reflector",
      backend: "codex_app_server",
      host_mode: "standalone",
      model: null,
      status: "queued",
      accepted_count: 0,
      rejected_count: 0,
      started_at: "2026-06-24T12:00:03Z",
      completed_at: "2026-06-24T12:00:03Z",
    },
  }),
  postAutomationRunSkillWriting: vi.fn().mockResolvedValue({
    run_id: "queued-skill-writer",
    dry_run: true,
    status: "queued",
    ledger_record: {
      schema_version: 2,
      run_id: "queued-skill-writer",
      trigger: "dashboard",
      task: "skill_writer",
      backend: "codex_app_server",
      host_mode: "standalone",
      model: null,
      status: "queued",
      accepted_count: 0,
      rejected_count: 0,
      started_at: "2026-06-24T12:00:04Z",
      completed_at: "2026-06-24T12:00:04Z",
    },
  }),
  postMemoryCurate: vi.fn(),
}));

vi.mock("../holographic/src/api", () => ({
  api: apiMock,
}));

describe("CurationPanel", () => {
  it("keeps inactive curation tabs keyboard reachable", () => {
    render(<CurationPanel />);

    const tabs = screen.getAllByRole("tab");

    expect(tabs).toHaveLength(3);
    expect(tabs.map((tab) => tab.getAttribute("tabindex"))).toEqual(["0", "0", "0"]);
  });

  it("saves automation runtime limits from dashboard controls", async () => {
    render(<CurationPanel />);

    fireEvent.click(screen.getByRole("tab", { name: /history/i }));

    const maxTokens = await screen.findByLabelText("Max tokens");
    const temperature = await screen.findByLabelText("Temperature");
    const schedulerTick = await screen.findByLabelText("Scheduler tick seconds");

    fireEvent.change(maxTokens, { target: { value: "4096" } });
    fireEvent.change(temperature, { target: { value: "0.2" } });
    fireEvent.change(schedulerTick, { target: { value: "15" } });
    fireEvent.click(screen.getByRole("button", { name: /save config/i }));

    await waitFor(() => {
      expect(apiMock.patchMemoryAutomationConfig).toHaveBeenCalledWith(
        expect.objectContaining({
          scheduler_tick_secs: 15,
          max_tokens: 4096,
          temperature: 0.2,
        }),
      );
    });
  });

  it("resets automation overrides from dashboard controls", async () => {
    render(<CurationPanel />);

    fireEvent.click(screen.getByRole("tab", { name: /history/i }));

    await screen.findByLabelText("Max tokens");
    fireEvent.click(screen.getByRole("button", { name: /reset defaults/i }));

    await waitFor(() => {
      expect(apiMock.resetMemoryAutomationConfig).toHaveBeenCalled();
    });
  });

  it("shows scheduler state and pauses scheduler from dashboard controls", async () => {
    render(<CurationPanel />);

    fireEvent.click(screen.getByRole("tab", { name: /history/i }));

    expect(await screen.findByText("Scheduler")).toBeTruthy();
    expect(await screen.findByText("memory curator")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /^pause$/i }));

    await waitFor(() => {
      expect(apiMock.pauseAutomationScheduler).toHaveBeenCalled();
    });
  });

  it("disables automation runs when the configured backend is unavailable", async () => {
    apiMock.getMemoryAutomationConfig.mockResolvedValueOnce({
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
          memory_curator: { enabled: true, schedule: null },
          session_reflector: { enabled: true, schedule: null },
          skill_writer: { enabled: true, schedule: null },
        },
      },
      backend_availability: {
        backend: "codex_app_server",
        available: false,
        executable: "/missing/codex",
        reason: "codex app-server backend executable '/missing/codex' was not found",
      },
    });

    render(<CurationPanel />);

    fireEvent.click(screen.getByRole("tab", { name: /history/i }));

    expect(await screen.findByText(/was not found/i)).toBeTruthy();
    const runButtons = await screen.findAllByRole("button", { name: /^run$/i });
    expect((runButtons[0] as HTMLButtonElement).disabled).toBe(true);
    expect(runButtons[0].getAttribute("title")).toContain("was not found");
  });

  it("renders automation config validation errors inline", async () => {
    apiMock.patchMemoryAutomationConfig.mockRejectedValueOnce(
      Object.assign(
        new Error("automation config validation failed"),
        {
          body: {
            validation_errors: [
              {
                field: "timeout_secs",
                message: "automation timeout_secs must be greater than zero",
              },
              {
                field: "backend",
                message: "automation backend is not selectable",
              },
              {
                field: "host_mode",
                message: "automation host_mode is invalid",
              },
              {
                field: "memory_curator.schedule",
                message: "memory curator schedule is invalid",
              },
              {
                field: "session_reflector.interval_secs",
                message: "session reflector interval_secs must be greater than zero",
              },
              {
                field: "skill_writer.stale_lock_secs",
                message: "skill writer stale_lock_secs must be greater than zero",
              },
            ],
          },
        },
      ),
    );

    render(<CurationPanel />);

    fireEvent.click(screen.getByRole("tab", { name: /history/i }));

    const timeoutInput = await screen.findByLabelText("Timeout seconds");
    fireEvent.change(timeoutInput, { target: { value: "0" } });
    fireEvent.click(screen.getByRole("button", { name: /save config/i }));

    expect(await screen.findByText("automation config validation failed")).toBeTruthy();
    expect(await screen.findByText("automation timeout_secs must be greater than zero")).toBeTruthy();
    expect(await screen.findByText("automation backend is not selectable")).toBeTruthy();
    expect(await screen.findByText("automation host_mode is invalid")).toBeTruthy();
    expect(await screen.findByText("memory curator schedule is invalid")).toBeTruthy();
    expect(
      await screen.findByText("session reflector interval_secs must be greater than zero"),
    ).toBeTruthy();
    expect(
      await screen.findByText("skill writer stale_lock_secs must be greater than zero"),
    ).toBeTruthy();
  });

  it("does not offer the unimplemented external command backend", async () => {
    render(<CurationPanel />);

    fireEvent.click(screen.getByRole("tab", { name: /history/i }));

    const backend = (await screen.findByLabelText("Backend")) as HTMLSelectElement;
    const values = Array.from(backend.options).map((option) => option.value);
    expect(values).toEqual(["disabled", "codex_app_server"]);
  });

  it("renders automation run ledger entries in history", async () => {
    render(<CurationPanel />);

    fireEvent.click(screen.getByRole("tab", { name: /history/i }));

    await waitFor(() => {
      expect(apiMock.getMemoryAutomationRuns).toHaveBeenCalledWith({ limit: 20 });
    });
    expect(await screen.findByText("Automation runs")).toBeTruthy();
    expect(
      screen.getAllByText(/memory_curator · dashboard · codex_app_server\/gpt-test/).length,
    ).toBeGreaterThan(0);
    expect(screen.getAllByText(/automation_disabled/).length).toBeGreaterThan(0);

    fireEvent.click(screen.getByRole("button", { name: /codex_handoff/i }));

    await waitFor(() => {
      expect(apiMock.getMemoryAutomationRunArtifacts).toHaveBeenCalledWith(
        "run-dashboard-1",
      );
      expect(apiMock.getMemoryAutomationRunArtifact).toHaveBeenCalledWith(
        "run-dashboard-1",
        "codex_handoff",
      );
    });
    expect(await screen.findByText("chain pending")).toBeTruthy();
    expect(await screen.findByText(/ready_for_review/)).toBeTruthy();
    expect(screen.getByText(/review dashboard artifact payload/)).toBeTruthy();
  });

  it("runs standalone automation tasks from history controls", async () => {
    render(<CurationPanel />);

    fireEvent.click(screen.getByRole("tab", { name: /history/i }));

    await screen.findByLabelText("Session reflector schedule");
    const runButtons = screen.getAllByRole("button", { name: /^run$/i });

    fireEvent.click(runButtons[1]);

    await waitFor(() => {
      expect(apiMock.postAutomationRunSessionReflection).toHaveBeenCalledWith({
        dry_run: true,
      });
    });

    fireEvent.click(runButtons[2]);

    await waitFor(() => {
      expect(apiMock.postAutomationRunSkillWriting).toHaveBeenCalledWith({
        dry_run: true,
      });
    });

    fireEvent.click(runButtons[0]);

    await waitFor(() => {
      expect(apiMock.postAutomationRunMemoryCurator).toHaveBeenCalledWith({
        dry_run: true,
      });
    });
  });

  it("renders and applies fact proposals in history", async () => {
    render(<CurationPanel />);

    fireEvent.click(screen.getByRole("tab", { name: /history/i }));

    await waitFor(() => {
      expect(apiMock.getFactProposals).toHaveBeenCalledWith({ limit: 50 });
    });
    expect(await screen.findByText("Fact proposals")).toBeTruthy();
    expect(screen.getByText(/Prefer bounded evidence/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /apply fact/i }));

    await waitFor(() => {
      expect(apiMock.applyFactProposal).toHaveBeenCalledWith("prop-dashboard-1");
    });
  });

  it("renders managed skill approvals in history", async () => {
    render(<CurationPanel />);

    fireEvent.click(screen.getByRole("tab", { name: /history/i }));

    await waitFor(() => {
      expect(apiMock.getManagedSkills).toHaveBeenCalled();
    });
    expect(await screen.findByText("Managed skills")).toBeTruthy();
    expect(screen.getAllByText("Repo Hygiene").length).toBeGreaterThan(0);
    expect(screen.getByText(/Use focused checks before broad suites/)).toBeTruthy();
    expect(screen.getByText("uses=1")).toBeTruthy();
    expect(screen.getByText(/recent or meaningful activity/)).toBeTruthy();
    expect(screen.getByText("patch_review")).toBeTruthy();
    expect(screen.getByText(/repeated patches/)).toBeTruthy();
    expect(screen.getByText(/priority=medium/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: /approve/i }));

    await waitFor(() => {
      expect(apiMock.approveManagedSkill).toHaveBeenCalledWith("repo-hygiene");
    });
  });
});
