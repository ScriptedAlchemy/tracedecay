import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useScope } from "../../data/scope/store.ts";
import { CurationConsole } from "./CurationConsole.tsx";

afterEach(() => {
  vi.unstubAllGlobals();
  useScope.getState().selectAllProjects();
});

describe("curation console", () => {
  it("shows automatic runs and outcomes without a shadow manual-effect ledger", async () => {
    const fetchMock = stubRoutes();
    renderConsole();

    expect(
      await screen.findByText(/scheduler-owned automatic runs/),
    ).toBeTruthy();
    expect(await screen.findByText(/future scheduler runs/)).toBeTruthy();
    expect(
      screen.queryByRole("region", { name: "Retained curation effects" }),
    ).toBeNull();
    expect(
      screen.queryByRole("region", { name: "Retained curation activity" }),
    ).toBeNull();
    expect(await screen.findByText(/deployment complete/)).toBeTruthy();
    expect(await screen.findByText(/recalled and helpful/)).toBeTruthy();
    expect(
      screen.queryByRole("button", {
        name: /plan|apply|approve|delete|merge/i,
      }),
    ).toBeNull();
    expect(
      fetchMock.mock.calls.some(([url]) =>
        String(url).includes("/curation/plan"),
      ),
    ).toBe(false);
    expect(
      fetchMock.mock.calls.some(([url]) =>
        String(url).includes("/curate/apply"),
      ),
    ).toBe(false);
    expect(
      fetchMock.mock.calls.some(([url]) =>
        String(url).includes("/curation/status"),
      ),
    ).toBe(false);
    expect(
      fetchMock.mock.calls.some(([url]) =>
        String(url).includes("/curation/activity"),
      ),
    ).toBe(false);
  });

  it("sends only enabled, revision CAS, and caller-stable idempotency on config change", async () => {
    useScope.setState({
      scope: {
        kind: "project",
        projectId: "proj_active",
        label: "Active",
        activation: "active",
      },
    });
    const fetchMock = stubRoutes();
    renderConsole();

    const checkbox = await screen.findByRole("checkbox", {
      name: /Automation enabled/i,
    });
    await userEvent.click(checkbox);
    await waitFor(() => {
      expect(
        fetchMock.mock.calls.some(([, init]) => init?.method === "PATCH"),
      ).toBe(true);
    });
    const patch = fetchMock.mock.calls.find(
      ([, init]) => init?.method === "PATCH",
    );
    expect(JSON.parse(String(patch?.[1]?.body))).toEqual({
      enabled: false,
      expected_revision_id: "configuration.revision.curation.test",
      idempotency_key: expect.stringMatching(
        /^idempotency\.dashboard-settings\./,
      ),
    });
  });
});

function renderConsole() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  render(
    <QueryClientProvider client={client}>
      <CurationConsole />
    </QueryClientProvider>,
  );
}

function stubRoutes() {
  const runs = {
    records: [
      {
        run_id: "run-1",
        trigger: "scheduler",
        task: "memory_curator",
        backend: "codex_app_server",
        status: "completed",
        reviewed_count: 0,
        accepted_count: 1,
        rejected_count: 1,
        skipped_count: 0,
        started_at: "2026-08-08T12:00:00Z",
        completed_at: "2026-08-08T12:00:02Z",
        validation_repairs: [{ field: "content" }],
        deployment: {
          status: "complete",
          exports: [],
          materialization_scopes: [],
          errors: [],
          retry_required: false,
        },
      },
    ],
    count: 1,
    limit: 50,
    error: "",
  };
  const outcomes = {
    generated_at: 1_700_000_000,
    skills: [],
    facts: [
      {
        proposal_id: "apply-1",
        run_id: "run-1",
        fact_id: "fact-1",
        applied_at: 1_699_000_000,
        days_since_applied: 1,
        retrieval_count: 2,
        access_count: 1,
        helpful_count: 1,
        unhelpful_count: 0,
        last_recalled_at: 1_700_000_000,
        still_exists: true,
        verdict: "recalled_and_helpful",
      },
    ],
    snapshot: {
      available: true,
      skills_refreshed_at: 1_700_000_000,
      facts_refreshed_at: 1_700_000_000,
    },
    error: "",
  };
  const config = {
    configuration_revision_id: "configuration.revision.curation.test",
    source: "daemon_pinned_snapshot",
    effective: {
      schema_version: 1,
      enabled: true,
      backend: "codex_app_server",
      host_mode: "standalone",
      model_id: "gpt-5.6-mini",
      timeout_secs: 60,
      scheduler_tick_secs: 60,
      combine_due_tasks: true,
      allow_job_commands: false,
      tasks: {
        memory_curator: {
          enabled: true,
          schedule: "interval",
          interval_secs: 3600,
          cooldown_secs: 300,
          min_idle_secs: 30,
          stale_lock_secs: 3600,
        },
        session_reflector: {
          enabled: true,
          schedule: "interval",
          interval_secs: 900,
          cooldown_secs: 300,
          min_idle_secs: 30,
          stale_lock_secs: 3600,
        },
        skill_writer: {
          enabled: true,
          schedule: "interval",
          interval_secs: 900,
          cooldown_secs: 300,
          min_idle_secs: 30,
          stale_lock_secs: 3600,
        },
      },
    },
    backend_availability: {
      backend: "codex_app_server",
      available: true,
      executable: "codex",
      reason: null,
    },
  };
  const automaticReceipt = {
    schema_version: 1,
    apply_id: "apply-1",
    run_id: "run-1",
    state: "applied",
    add_fact_request: { content: "A recorded project fact." },
    applied_canonical_fact_id: "fact-1",
    applied_fact_id: 1,
    recorded_at: 1_700_000_000,
  };
  const fetchMock = vi.fn(
    async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      const body = url.includes("/curation/runs")
        ? runs
        : url.includes("/automation/outcomes")
          ? outcomes
          : url.includes("/automatic-fact-receipts")
            ? {
                receipts: [automaticReceipt],
                count: 1,
                limit: 50,
                error: "",
              }
            : url.includes("/curation/config")
              ? config
              : {};
      void init;
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { "content-type": "application/json" },
      });
    },
  );
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}
