import { render, screen } from "@testing-library/react";
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
      enabled: false,
      backend: "disabled",
      host_mode: "standalone",
      model: null,
      timeout_secs: 60,
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
  getMemoryOplog: vi.fn().mockResolvedValue({ events: [] }),
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
});
