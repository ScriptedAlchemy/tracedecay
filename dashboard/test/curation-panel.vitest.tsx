import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import CurationPanel from "../holographic/src/CurationPanel";

vi.mock("../holographic/src/api", () => ({
  api: {
    getMemoryCuratorPreview: vi.fn().mockResolvedValue({ report: null, saved_at: null }),
    getMemoryCuratorActivity: vi.fn().mockResolvedValue({ events: [] }),
    getMemoryCuratorStatus: vi.fn().mockResolvedValue({ runs: [] }),
    getMemoryOplog: vi.fn().mockResolvedValue({ events: [] }),
    postMemoryCurate: vi.fn(),
  },
}));

describe("CurationPanel", () => {
  it("keeps inactive curation tabs keyboard reachable", () => {
    render(<CurationPanel />);

    const tabs = screen.getAllByRole("tab");

    expect(tabs).toHaveLength(3);
    expect(tabs.map((tab) => tab.getAttribute("tabindex"))).toEqual(["0", "0", "0"]);
  });
});
