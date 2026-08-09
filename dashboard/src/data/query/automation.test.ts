import { afterEach, describe, expect, it, vi } from "vitest";

import {
  AutomationOutcomesPayloadSchema,
  AutomationSchedulerStatusV1Schema,
  setSchedulerPaused,
} from "./automation.ts";

function scheduler(overrides: Record<string, unknown> = {}) {
  return {
    status: "configured",
    paused: false,
    enabled: true,
    scheduler_tick_secs: 300,
    now: 1_700_000_000,
    last_session_activity: 1_699_999_000,
    configuration_revision_id: "configuration.revision.test",
    control_path: "/p/.tracedecay/scheduler-control.json",
    tasks: [],
    ...overrides,
  };
}

function respond(body: unknown, init?: { ok?: boolean; statusCode?: number }) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({
      ok: init?.ok ?? true,
      status: init?.statusCode ?? 200,
      json: async () => body,
    })),
  );
}

afterEach(() => vi.unstubAllGlobals());

describe("setSchedulerPaused", () => {
  it("POSTs and returns the daemon reading after the change", async () => {
    respond(scheduler({ paused: true, status: "paused" }));
    const result = await setSchedulerPaused("/api/automation/scheduler/pause");
    expect(result.outcome).toBe("ok");
    if (result.outcome !== "ok") throw new Error("unreachable");
    expect(result.data.paused).toBe(true);
    expect(result.data.configuration_revision_id).toBe(
      "configuration.revision.test",
    );
    const call = vi.mocked(fetch).mock.calls[0];
    expect(call?.[0]).toBe("/api/automation/scheduler/pause");
    expect((call?.[1] as RequestInit | undefined)?.method).toBe("POST");
  });

  it("does not accept an acknowledgement in place of a reading", async () => {
    respond({ ok: true });
    const result = await setSchedulerPaused("/api/automation/scheduler/resume");
    expect(result.outcome).toBe("unsupported_schema");
  });
});

describe("the generated scheduler contract", () => {
  it("requires the daemon-owned configuration revision and task receipts", () => {
    const parsed = AutomationSchedulerStatusV1Schema.parse(
      scheduler({
        tasks: [
          {
            task: "memory_curator",
            due: false,
            skip_reason: "scheduler_paused",
            last_scheduler_run: null,
          },
        ],
      }),
    );
    expect(parsed.configuration_revision_id).toBe(
      "configuration.revision.test",
    );
    expect(parsed.tasks[0]?.last_scheduler_run).toBeNull();
  });

  it("rejects the retired pending-review scheduler shape", () => {
    const parsed = AutomationSchedulerStatusV1Schema.safeParse(
      scheduler({ legacy_queue: { count: 0 } }),
    );
    expect(parsed.success).toBe(false);
  });
});

describe("automatic outcome payload", () => {
  it("keeps snapshot availability and terminal verdicts typed", () => {
    const parsed = AutomationOutcomesPayloadSchema.parse({
      generated_at: 1_700_000_000,
      skills: [
        {
          skill_id: "skill-1",
          title: "Skill",
          activated_at: 1_699_000_000,
          days_since_activation: 1,
          views_since_activation: 2,
          uses_since_activation: 1,
          verdict: "adopted",
        },
      ],
      facts: [],
      snapshot: {
        available: true,
        skills_refreshed_at: 1_700_000_000,
        facts_refreshed_at: null,
      },
      error: "",
    });
    expect(parsed.skills[0]?.verdict).toBe("adopted");
    expect(parsed.snapshot.facts_refreshed_at).toBeNull();
  });
});
