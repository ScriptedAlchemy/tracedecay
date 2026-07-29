import { describe, expect, it, vi } from "vitest";

import { createClient, type ClientOptions } from "../src/client";
import {
  WORKFLOW_DEFINITION_SCHEMA,
  WORKFLOW_ROUTES,
  type WorkflowDefinitionData,
} from "../src/workflow";

const SCOPE = { project: "project.demo", repository: "repo.demo", worktree: "wt.demo" };

function makeBuilder() {
  const client = createClient({ baseUrl: "http://fixture.invalid", scope: { projectId: SCOPE.project, repositoryId: SCOPE.repository, worktreeId: SCOPE.worktree } });
  return client.workflow.define("review-and-fix", SCOPE);
}

describe("WorkflowDefinitionBuilder", () => {
  it("composes steps, fan-out, and synthesis into one definition document", () => {
    const definition = makeBuilder()
      .inputSchema({ type: "object" })
      .step("review", "tracedecay_review_diff", { base: "main" })
      .step("fix", "tracedecay_fix_build", {}, { after: ["review"], inputRefs: { report: "review.report" } })
      .fanOut("verify", ["fix"], {
        items: ["crate-a", "crate-b"],
        maxConcurrency: 2,
        failure: { kind: "collect" },
        operation: "tracedecay_test_changes",
      })
      .synthesis("merge", { after: ["verify"] })
      .budget({ maxUnits: 32 })
      .definition();

    expect(definition.schema_version).toBe(WORKFLOW_DEFINITION_SCHEMA);
    expect(definition.name).toBe("review-and-fix");
    expect(definition.scope).toEqual(SCOPE);
    expect(definition.steps.map((step) => step.id)).toEqual(["review", "fix"]);
    expect(definition.steps[1].after).toEqual(["review"]);
    expect(definition.steps[1].inputRefs).toEqual({ report: "review.report" });
    expect(definition.fan_out).toHaveLength(1);
    expect(definition.fan_out[0].failure).toEqual({ kind: "collect" });
    expect(definition.synthesis[0].policy).toBe("preserve_minority");
    expect(definition.budget).toEqual({ maxUnits: 32 });
  });

  it("preserves additive unknown fields for forward compatibility", () => {
    const definition = makeBuilder().resultConditions({ future_field: { nested: true } }).definition();
    expect((definition as Record<string, unknown>).result_conditions).toEqual({ future_field: { nested: true } });
  });

  it("rejects duplicate ids, unknown dependencies, and unbounded fan-out", () => {
    expect(() => makeBuilder().step("a", "op").step("a", "op")).toThrow(/duplicate/);
    expect(() => makeBuilder().step("b", "op", {}, { after: ["missing"] })).toThrow(/unknown step/);
    expect(() =>
      makeBuilder().step("a", "op").fanOut("f", ["a"], { items: [1], maxConcurrency: 0, failure: { kind: "fail_fast" }, operation: "op" }),
    ).toThrow(/positive integer/);
    expect(() =>
      makeBuilder().step("a", "op").fanOut("f", ["a"], { items: [], maxConcurrency: 1, failure: { kind: "fail_fast" }, operation: "op" }),
    ).toThrow(/must not be empty/);
  });

  it("renders a topological preview order without claiming validation", () => {
    const order = makeBuilder()
      .step("review", "op")
      .step("fix", "op", {}, { after: ["review"] })
      .fanOut("verify", ["fix"], { items: [1], maxConcurrency: 1, failure: { kind: "fail_fast" }, operation: "op" })
      .synthesis("merge", { after: ["verify"] })
      .previewOrder();
    expect(order.indexOf("review")).toBeLessThan(order.indexOf("fix"));
    expect(order.indexOf("fix")).toBeLessThan(order.indexOf("verify"));
    expect(order.indexOf("verify")).toBeLessThan(order.indexOf("merge"));
  });
});

// ---------------------------------------------------------------------------
// Run lifecycle over a mock transport
// ---------------------------------------------------------------------------

interface RecordedCall {
  url: string;
  method: string;
  body: unknown;
  headers: Record<string, string>;
}

function successEnvelope(value: unknown): unknown {
  return {
    kind: "success",
    value: { binding_id: "binding.fixture", request_id: "request.fixture", value },
  };
}

function mockFetchSequence(responders: Array<(call: RecordedCall) => unknown>) {
  const calls: RecordedCall[] = [];
  const mock = vi.fn(async (url: unknown, init?: { method?: string; body?: string; headers?: Record<string, string> }) => {
    const call: RecordedCall = {
      url: String(url),
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(init.body) : undefined,
      headers: init?.headers ?? {},
    };
    calls.push(call);
    const responder = responders[calls.length - 1];
    if (!responder) throw new Error(`unexpected call ${calls.length}: ${call.url}`);
    return new Response(JSON.stringify(responder(call)), { status: 200 });
  });
  return { calls, mock: mock as unknown as ClientOptions["fetch"] };
}

describe("workflow run lifecycle", () => {
  it("admits a run and addresses controls by run id", async () => {
    const { calls, mock } = mockFetchSequence([
      () => successEnvelope({ run_id: "run.123" }),
      () => successEnvelope({ state: "paused" }),
      () => successEnvelope({ termination: "cancelled" }),
    ]);
    const client = createClient({ baseUrl: "http://fixture.invalid", fetch: mock });

    const handle = await client.workflow.run("def.v7", { alpha: 1 });
    expect(handle.runId).toBe("run.123");
    expect(calls[0].url).toBe(`http://fixture.invalid${WORKFLOW_ROUTES.run}`);
    expect(calls[0].body).toEqual({ definition_version: "def.v7", inputs: { alpha: 1 } });

    await handle.pause();
    expect(calls[1].url).toBe(`http://fixture.invalid${WORKFLOW_ROUTES.control("run.123", "pause")}`);

    await handle.cancel();
    expect(calls[2].url).toBe(`http://fixture.invalid${WORKFLOW_ROUTES.control("run.123", "cancel")}`);
  });

  it("validates definitions through the daemon authority, not locally", async () => {
    const { calls, mock } = mockFetchSequence([
      () => successEnvelope({ valid: false, problems: [{ code: "cycle", message: "a -> b -> a" }] }),
    ]);
    const client = createClient({ baseUrl: "http://fixture.invalid", fetch: mock });
    const definition: WorkflowDefinitionData = makeBuilder().step("a", "op").definition();

    const result = await client.workflow.validate(definition);
    expect(result.value.valid).toBe(false);
    expect(result.value.problems[0].code).toBe("cycle");
    expect(calls[0].url).toBe(`http://fixture.invalid${WORKFLOW_ROUTES.validate}`);
    expect(calls[0].body).toMatchObject({ schema_version: WORKFLOW_DEFINITION_SCHEMA, name: "review-and-fix" });
  });
});
