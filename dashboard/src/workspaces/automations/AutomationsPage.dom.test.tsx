import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AutomationsPage } from "./AutomationsPage.tsx";

afterEach(() => vi.unstubAllGlobals());

describe("AutomationsPage automatic outcomes", () => {
  it("renders scheduler readings and terminal fact receipts without approval controls", async () => {
    const fetchMock = stubAutomation();
    renderAutomations();

    expect(await screen.findByText("configuration revision")).toBeTruthy();
    const outcomes = await settledPanel("Fact application outcomes");
    expect(
      within(outcomes).getAllByText("A recorded project fact."),
    ).toHaveLength(2);
    expect(within(outcomes).getByText("applied")).toBeTruthy();
    expect(within(outcomes).getByText("quarantined")).toBeTruthy();
    expect(within(outcomes).getByText(/apply apply-1 · run session-reflector-0/)).toBeTruthy();
    expect(within(outcomes).getByText(/fact fact\.apply-1/)).toBeTruthy();
    expect(within(outcomes).getAllByText(/validation/).length).toBeGreaterThan(0);
    expect(within(outcomes).getByText(/quarantine: validation failed/)).toBeTruthy();
    expect(
      screen.queryByRole("button", { name: /approve|apply|review|plan/i }),
    ).toBeNull();
    expect(
      fetchMock.mock.calls.some(([url]) =>
        String(url).includes("/curation/plan"),
      ),
    ).toBe(false);
  });

  it("reports an empty receipt list only when its own tally agrees", async () => {
    stubAutomation({
      "automatic-fact-receipts": receiptsBody([]),
    });
    renderAutomations();
    const panel = await settledPanel("Fact application outcomes");
    expect(
      within(panel).getByText(/no fact application outcomes are recorded/i),
    ).toBeTruthy();
    expect(within(panel).queryByRole("status")).toBeNull();
  });

  it("names a capped receipt page instead of claiming it is complete", async () => {
    const receipts = Array.from({ length: 50 }, (_, index) =>
      receipt(`apply-${index}`),
    );
    stubAutomation({
      "automatic-fact-receipts": receiptsBody(receipts),
    });
    renderAutomations();
    const panel = await settledPanel("Fact application outcomes");
    expect(within(panel).getByRole("status").textContent).toContain(
      "this is the first 50 fact application outcomes",
    );
  });
});

describe("AutomationsPage scheduler control", () => {
  it("uses the daemon response after pausing rather than optimistic state", async () => {
    let paused = false;
    const fetchMock = stubAutomation({
      status: () => jsonResponse(scheduler({ paused })),
      pause: () => {
        paused = true;
        return jsonResponse(scheduler({ paused: true, status: "paused" }));
      },
    });
    renderAutomations();

    await userEvent.click(
      await screen.findByRole("button", { name: "Pause scheduler" }),
    );
    expect(
      await screen.findByRole("button", { name: "Resume scheduler" }),
    ).toBeTruthy();
    const call = fetchMock.mock.calls.find(([url]) =>
      String(url).endsWith("/scheduler/pause"),
    );
    expect(call?.[1]?.method).toBe("POST");
  });
});

async function settledPanel(name: string): Promise<HTMLElement> {
  const panel = await screen.findByRole("region", { name });
  await waitFor(() =>
    expect(panel.querySelector('[data-state="loading"]')).toBeNull(),
  );
  return panel;
}

function renderAutomations() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  render(
    <QueryClientProvider client={client}>
      <AutomationsPage />
    </QueryClientProvider>,
  );
}

function scheduler(overrides: { paused?: boolean; status?: string } = {}) {
  return {
    status: overrides.status ?? "configured",
    paused: overrides.paused ?? false,
    enabled: true,
    scheduler_tick_secs: 900,
    now: Math.floor(Date.now() / 1000),
    last_session_activity: Math.floor(Date.now() / 1000) - 1200,
    configuration_revision_id: "configuration.revision.automation.test",
    control_path: "/x/automation.control.json",
    tasks: [
      {
        task: "memory_curator",
        due: false,
        skip_reason: "cooldown",
        last_scheduler_run: null,
      },
    ],
  };
}

function jobsBody(jobs: unknown[], count = jobs.length) {
  return { jobs, count };
}

function skillsBody(skills: unknown[], count = skills.length) {
  return {
    profile_root: "/home/x/.tracedecay",
    skills_root: "/home/x/.tracedecay/managed-skills",
    count,
    skills,
    skill_metadata: [],
    usage_summaries: [],
    stale_recommendations: [],
    improvement_recommendations: [],
  };
}

function receiptsBody(receipts: unknown[], count = receipts.length) {
  return { receipts, count, limit: 50, error: "" };
}

function receipt(id: string, state: "applied" | "quarantined" = "applied") {
  return {
    schema_version: 1,
    apply_id: id,
    run_id: "session-reflector-0",
    state,
    evidence_hash: `evidence.${id}`,
    add_fact_request: {
      content: "A recorded project fact.",
      category: "preference",
    },
    quarantine_reason:
      state === "quarantined" ? "validation failed" : undefined,
    validation: {
      disposition: state === "applied" ? "accepted" : "rejected",
      policy: "automatic-memory-v1",
    },
    applied_fact_id: state === "applied" ? `fact.${id}` : undefined,
    recorded_at_micros: Date.now() * 1_000,
  };
}

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

type Reply = unknown | (() => Response);

function stubAutomation(overrides: Record<string, Reply> = {}) {
  const fallbacks: Record<string, unknown> = {
    status: scheduler(),
    jobs: jobsBody([
      {
        id: "nightly-sweep",
        name: "Nightly sweep",
        schedule: "0 3 * * *",
        enabled: true,
        interval_secs: null,
      },
    ]),
    skills: skillsBody([
      {
        metadata: {
          id: "code-slop",
          title: "Code Slop Cleanup",
          state: "active",
        },
      },
    ]),
    "automatic-fact-receipts": receiptsBody([
      receipt("apply-1"),
      receipt("apply-2", "quarantined"),
    ]),
    runs: {
      runs: [],
      count: 0,
      limit: 50,
      has_more: false,
      malformed_row_count: 0,
      completeness: "known",
      error: "",
    },
  };
  const fetchMock = vi.fn(
    async (input: RequestInfo | URL, init?: RequestInit) => {
      const parts = String(input).split("?")[0]?.split("/") ?? [];
      const endpoint = parts.at(-1) ?? "";
      const reply =
        endpoint in overrides
          ? overrides[endpoint]
          : (fallbacks[endpoint] ?? {});
      void init;
      return typeof reply === "function" ? reply() : jsonResponse(reply);
    },
  );
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}
