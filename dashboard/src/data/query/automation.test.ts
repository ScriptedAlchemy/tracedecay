import { createHash } from "node:crypto";

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  AutomationOutcomesPayloadSchema,
  AutomationSchedulerStatusV1Schema,
  runAutomaticCurator,
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
  const value =
    init?.ok !== false &&
    typeof body === "object" &&
    body !== null &&
    "run" in body
      ? curatorSuccess((body as { run: unknown }).run)
      : body;
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({
      ok: init?.ok ?? true,
      status: init?.statusCode ?? 200,
      json: async () => value,
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

describe("runAutomaticCurator", () => {
  it("returns the started receipt of the run the request admitted", async () => {
    respond({ run: startedReceipt("request.dashboard.success") });

    const result = await runAutomaticCurator();
    expect(result).toEqual({
      outcome: "started",
      receipt: {
        run_id: "request.dashboard.success",
        task: "memory_curator",
        request_digest: automaticRequestDigest(),
        state: "started",
      },
    });
    const call = vi.mocked(fetch).mock.calls[0];
    expect(call?.[0]).toBe("/api/application/retained/fact_store_curate");
    expect(JSON.parse(String((call?.[1] as RequestInit).body))).toEqual({
      fact_review_limit: 24,
      min_confidence_millionths: 720_000,
    });
  });

  it("rejects a receipt for another run, request bounds, or state", async () => {
    respond({ run: startedReceipt("request.dashboard.other") });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");

    respond({
      run: { ...startedReceipt("request.dashboard.success"), request_digest: sha("f") },
    });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");

    respond({
      run: { ...startedReceipt("request.dashboard.success"), state: "completed" },
    });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("rejects the retired run terminal in place of the receipt", async () => {
    const { state: _state, ...identity } = startedReceipt("request.dashboard.success");
    respond({
      run: {
        ...identity,
        terminal: {
          status: "completed",
          summary: { reviewed_count: 0, accepted_count: 0, rejected_count: 0, skipped_count: 0 },
        },
        committed_receipts: [],
      },
    });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("rejects success and admitted-problem envelopes from another HTTP binding", async () => {
    const success = curatorSuccess(startedReceipt("request.dashboard.success"));
    success.value.binding_id = "binding.http.fact_store_get.v1";
    respond(success);
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");

    const problem = automaticProblem("reset_required");
    problem.value.binding_id = "binding.http.fact_store_get.v1";
    respond(problem, { ok: false, statusCode: 503 });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("rejects success and admitted-problem envelopes from another result contract", async () => {
    const success = curatorSuccess(startedReceipt("request.dashboard.success"));
    success.value.contract.schema_id =
      "schema.application.retained.fact-store-get.result";
    respond(success);
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");

    const problem = automaticProblem("reset_required");
    problem.value.contract.schema_revision = 2;
    respond(problem, { ok: false, statusCode: 503 });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("recognizes a concealed 404 without disclosing a binding", async () => {
    respond(automaticProblem("not_found_or_not_authorized"), {
      ok: false,
      statusCode: 404,
    });
    expect(await runAutomaticCurator()).toEqual({
      outcome: "denied",
      detail: "the automatic run was not found or not authorized",
    });
  });

  it("rejects a concealed problem that discloses any binding", async () => {
    for (const bindingId of [
      "binding.http.fact_store_curate.v1",
      "binding.http.fact_store_get.v1",
    ]) {
      const problem = automaticProblem("not_found_or_not_authorized");
      problem.value.binding_id = bindingId;
      respond(problem, { ok: false, statusCode: 404 });
      expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
    }
  });

  it("requires binding identity for ordinary problems", async () => {
    const problem = automaticProblem("conflict");
    delete problem.value.binding_id;
    respond(problem, { ok: false, statusCode: 409 });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("still validates the status and result contract of concealed problems", async () => {
    respond(automaticProblem("not_found_or_not_authorized"), {
      ok: false,
      statusCode: 503,
    });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");

    const wrongContract = automaticProblem("not_found_or_not_authorized");
    wrongContract.value.contract.schema_revision = 2;
    respond(wrongContract, { ok: false, statusCode: 404 });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("rejects malformed success request identity and scope digest", async () => {
    const wrongRequest = curatorSuccess(startedReceipt("request.dashboard.success"));
    wrongRequest.value.request_id = " request.dashboard.success";
    respond(wrongRequest);
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");

    const success = curatorSuccess(startedReceipt("request.dashboard.success"));
    success.value.scope.scope_digest = sha("9");
    respond(success);
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("rejects a run for another automatic-memory task", async () => {
    const run = startedReceipt("request.dashboard.success");
    run.task = "session_reflector";
    respond({ run });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("requires the application effect execution receipt", async () => {
    const body = curatorSuccess(startedReceipt("request.dashboard.success"));
    const { execution: _execution, ...effect } = body.value.outcome.value;
    respond({
      ...body,
      value: { ...body.value, outcome: { outcome: "effect", value: effect } },
    });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("refuses receipts when browser hashing is unavailable", async () => {
    vi.stubGlobal("crypto", undefined);
    respond({ run: startedReceipt("request.dashboard.success") });
    expect(await runAutomaticCurator()).toEqual({
      outcome: "unsupported_schema",
      detail: "the automatic curator result does not match this build",
    });

    respond(automaticProblem("partial_effect"), { ok: false, statusCode: 409 });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("decodes the generated partial-effect terminal", async () => {
    respond(automaticProblem("partial_effect"), { ok: false, statusCode: 409 });
    expect((await runScopedAutomaticCurator()).outcome).toBe("partial_effect");
  });

  it("requires a typed application conflict instead of inferring one from HTTP 409", async () => {
    respond(automaticProblem("conflict"), { ok: false, statusCode: 409 });
    expect((await runScopedAutomaticCurator()).outcome).toBe("conflicting");

    respond({ error: "conflict" }, { ok: false, statusCode: 409 });
    expect((await runScopedAutomaticCurator()).outcome).toBe("unsupported_schema");

    respond(
      {
        kind: "problem",
        value: {
          binding_id: "binding.http.fact_store_curate.v1",
          contract: {
            schema_id: "schema.application.retained.fact-store-curate.result",
            schema_revision: 1,
          },
          request_id: "request.dashboard.conflict",
          problem: { kind: "conflict" },
        },
      },
      { ok: false, statusCode: 409 },
    );
    expect((await runScopedAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("binds a typed conflict to its canonical application request identity", async () => {
    const mismatched = automaticProblem("conflict");
    mismatched.value.problem.request_id = "request.dashboard.other";
    respond(mismatched, { ok: false, statusCode: 409 });
    expect((await runScopedAutomaticCurator()).outcome).toBe("unsupported_schema");

    const malformed = automaticProblem("conflict");
    malformed.value.request_id = " request.dashboard.conflict";
    malformed.value.problem.request_id = malformed.value.request_id;
    respond(malformed, { ok: false, statusCode: 409 });
    expect((await runScopedAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("rejects a partial terminal belonging to another application effect", async () => {
    const body = automaticProblem("partial_effect");
    const receipt = body.value.problem.committed_receipt;
    if (receipt === null) throw new Error("partial fixture drifted");
    receipt.operation = "use-case.application.retained.fact-store-add";
    respond(body, { ok: false, statusCode: 409 });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("requires a committed curation state in a partial-effect receipt", async () => {
    const body = automaticProblem("partial_effect");
    const receipt = body.value.problem.committed_receipt;
    if (receipt === null) throw new Error("partial fixture drifted");
    Object.assign(receipt, { committed_state: null, external_proof: sha("8") });
    respond(body, { ok: false, statusCode: 409 });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("rejects application problems with inconsistent HTTP status or retry scope", async () => {
    respond(automaticProblem("partial_effect"), { ok: false, statusCode: 503 });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");

    respond(automaticProblem("reset_required"), { ok: false, statusCode: 409 });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");

    const conflict = automaticProblem("conflict");
    conflict.value.problem.retry_scope = null;
    respond(conflict, { ok: false, statusCode: 409 });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("rejects non-canonical partial-effect receipt scopes", async () => {
    const invalidIdentity = automaticProblem("partial_effect");
    const invalidIdentityReceipt =
      invalidIdentity.value.problem.committed_receipt;
    if (invalidIdentityReceipt === null) throw new Error("partial fixture drifted");
    invalidIdentityReceipt.scope.project_id = " project.dashboard";
    invalidIdentityReceipt.scope.scope_digest = canonicalSha([
      "tracedecay.application.scope.v1",
      invalidIdentityReceipt.scope.project_id,
      invalidIdentityReceipt.scope.repository_id,
      invalidIdentityReceipt.scope.worktree_id,
      invalidIdentityReceipt.scope.reference,
    ]);
    respond(invalidIdentity, { ok: false, statusCode: 409 });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");

    const wrongDigest = automaticProblem("partial_effect");
    const wrongDigestReceipt = wrongDigest.value.problem.committed_receipt;
    if (wrongDigestReceipt === null) throw new Error("partial fixture drifted");
    wrongDigestReceipt.scope.scope_digest = sha("9");
    respond(wrongDigest, { ok: false, statusCode: 409 });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("binds the problem contract and admitted request", async () => {
    const wrongContract = automaticProblem("reset_required");
    wrongContract.value.contract.schema_revision = 2;
    respond(wrongContract, { ok: false, statusCode: 503 });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");

    const wrongRequest = automaticProblem("reset_required");
    wrongRequest.value.problem.request_id = "request.dashboard.other";
    respond(wrongRequest, { ok: false, statusCode: 503 });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("decodes the generated reset-required terminal", async () => {
    respond(automaticProblem("reset_required"), { ok: false, statusCode: 503 });
    expect((await runScopedAutomaticCurator()).outcome).toBe("reset_required");
  });

});

const sha = (seed: string) => `sha256:${seed.repeat(64)}`;
const SCOPED_CURATOR_URL = "/api/application/retained/fact_store_curate";

function runScopedAutomaticCurator() {
  return runAutomaticCurator(SCOPED_CURATOR_URL);
}

function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) {
    return `[${value.map(canonicalJson).join(",")}]`;
  }
  if (value !== null && typeof value === "object") {
    return `{${Object.entries(value)
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([key, entry]) => `${JSON.stringify(key)}:${canonicalJson(entry)}`)
      .join(",")}}`;
  }
  return JSON.stringify(value) ?? "null";
}

function canonicalSha(value: unknown): string {
  return `sha256:${createHash("sha256").update(canonicalJson(value)).digest("hex")}`;
}

function startedReceipt(runId: string) {
  return {
    run_id: runId,
    task: "memory_curator",
    request_digest: automaticRequestDigest(),
    state: "started",
  };
}

function automaticRequestDigest(): string {
  return canonicalSha([
    "tracedecay.automation-run.request-identity.v1",
    {
      kind: "memory_curator",
      options: {
        fact_review_limit: 24,
        min_confidence_millionths: 720_000,
      },
    },
  ]);
}

function curatorSuccess(run: unknown) {
  return {
    kind: "success",
    value: {
      binding_id: "binding.http.fact_store_curate.v1",
      contract: {
        schema_id: "schema.application.retained.fact-store-curate.result",
        schema_revision: 1,
      },
      request_id: "request.dashboard.success",
      scope: {
        project_id: "project.dashboard",
        repository_id: "repository.dashboard",
        worktree_id: "worktree.dashboard",
        reference: null,
        scope_digest: canonicalSha([
          "tracedecay.application.scope.v1",
          "project.dashboard",
          "repository.dashboard",
          "worktree.dashboard",
          null,
        ]),
      },
      outcome: {
        outcome: "effect",
        value: {
          effect_id: "effect.retained.fact_store_curate.request.dashboard.success",
          effect_class: "administrative",
          idempotency_key: "idempotency.dashboard.success",
          authority: {},
          expected_state: sha("0"),
          reconciliation: "pending",
          receipt: { outcome: "effect_unknown", committed_state: null },
          execution: {
            started_at: 10,
            ended_at: 20,
            effective_deadline: null,
            cancellation: null,
            budget: { units_consumed: 0, bytes_consumed: 0, elapsed_micros: 10 },
            termination: "effect_unknown",
          },
          payload: run,
        },
      },
    },
  };
}

function automaticProblem(
  kind: "conflict" | "partial_effect" | "reset_required" | "not_found_or_not_authorized",
) {
  const requestId = `request.dashboard.${kind}`;
  const concealed = kind === "not_found_or_not_authorized";
  const scope = {
    project_id: "project.dashboard",
    repository_id: "repository.dashboard",
    worktree_id: "worktree.dashboard",
    reference: null,
    scope_digest: canonicalSha([
      "tracedecay.application.scope.v1",
      "project.dashboard",
      "repository.dashboard",
      "worktree.dashboard",
      null,
    ]),
  };
  const effectReceipt = kind === "partial_effect"
    ? {
        actor: "actor.dashboard",
        catalog_digest: sha("1"),
        committed_state: sha("2"),
        configuration_digest: sha("3"),
        effect_class: "administrative",
        expected_state: sha("4"),
        external_proof: null,
        idempotency_key: "idempotency.dashboard",
        input_digest: sha("5"),
        operation: "use-case.application.retained.fact-store-curate",
        outcome: "partial",
        policy_digest: sha("6"),
        privacy_digest: sha("7"),
        request_id: requestId,
        scope: { ...scope },
      }
    : null;
  return {
    kind: "problem",
    value: {
      ...(concealed ? {} : { binding_id: "binding.http.fact_store_curate.v1" }),
      contract: {
        schema_id: "schema.application.retained.fact-store-curate.result",
        schema_revision: 1,
      },
      request_id: requestId,
      problem: {
        revision: 1,
        kind,
        code: concealed
          ? "application.not_found_or_not_authorized"
          : `automation.memory-curator.${kind}`,
        message: concealed
          ? "The operation was not found or not authorized."
          : kind === "partial_effect"
          ? "curation committed before projection failed"
          : kind === "reset_required"
          ? "the retained memory store must be reset"
          : "the retained operation conflicts with current state",
        diagnostic: concealed ? null : {
          code: `application.retained.${kind}`,
          message: "The retained operation could not complete.",
        },
        detail: null,
        committed_receipt: effectReceipt,
        owning_layer: "runtime",
        terminality: kind === "partial_effect" || kind === "reset_required"
          ? "admitted_terminal"
          : "pre_admission",
        retryable: kind === "conflict",
        retry: kind === "conflict" ? "after_revalidate" : "never",
        retry_scope: kind === "conflict" ? "fresh_request" : null,
        retry_after_millis: null,
        cancellation_stage: null,
        execution_failure_classification: null,
        request_id: requestId,
        trace_id: requestId,
        details: [],
        legal_actions: concealed ? [] : [
          kind === "partial_effect"
            ? "reconcile"
            : kind === "reset_required"
            ? "reset"
            : "refresh",
        ],
        coverage: null,
        unavailable_classification: null,
      },
    },
  };
}

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
  it("decodes the producer's terminal fact identity, state, and age fields", () => {
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
      facts: [
        {
          apply_id: "apply-1",
          run_id: "run-1",
          state: "applied",
          canonical_fact_id: "fact-1",
          recorded_at: 1_699_000_000,
          days_since_recorded: 1,
          retrieval_count: 2,
          access_count: 1,
          helpful_count: 1,
          unhelpful_count: 0,
          last_recalled_at: 1_700_000_000,
          still_exists: true,
          verdict: "recalled_and_helpful",
        },
        {
          apply_id: "apply-2",
          state: "quarantined",
          recorded_at: 1_699_000_001,
          days_since_recorded: 1,
          still_exists: false,
          verdict: "quarantined",
        },
        {
          apply_id: "apply-3",
          state: "applied",
          canonical_fact_id: "fact-3",
          recorded_at: 1_699_000_002,
          days_since_recorded: 1,
          still_exists: false,
          verdict: "unavailable",
        },
      ],
      snapshot: {
        available: true,
        skills_refreshed_at: 1_700_000_000,
        facts_refreshed_at: null,
      },
      error: "",
    });
    expect(parsed.skills[0]?.verdict).toBe("adopted");
    expect(parsed.facts[0]?.canonical_fact_id).toBe("fact-1");
    expect(parsed.facts[0]?.days_since_recorded).toBe(1);
    expect(parsed.facts[1]?.verdict).toBe("quarantined");
    expect(parsed.facts[2]?.verdict).toBe("unavailable");
    expect(parsed.snapshot.facts_refreshed_at).toBeNull();
  });

  it("rejects the retired proposal and applied-at fact outcome shape", () => {
    const parsed = AutomationOutcomesPayloadSchema.safeParse({
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
          still_exists: true,
          verdict: "recalled_and_helpful",
        },
      ],
      snapshot: {
        available: true,
        skills_refreshed_at: null,
        facts_refreshed_at: 1_700_000_000,
      },
      error: "",
    });
    expect(parsed.success).toBe(false);
  });

  it("rejects producer-impossible fact state, identity, and telemetry combinations", () => {
    const common = {
      apply_id: "apply-impossible",
      recorded_at: 1_699_000_000,
      days_since_recorded: 1,
    };
    const impossibleFacts = [
      {
        ...common,
        state: "applied",
        still_exists: false,
        verdict: "deleted",
      },
      {
        ...common,
        state: "quarantined",
        canonical_fact_id: "fact-impossible",
        still_exists: false,
        verdict: "quarantined",
      },
      {
        ...common,
        state: "applied",
        canonical_fact_id: "fact-impossible",
        still_exists: true,
        verdict: "never_recalled",
      },
      {
        ...common,
        state: "applied",
        canonical_fact_id: "fact-impossible",
        retrieval_count: 1,
        access_count: 0,
        helpful_count: 0,
        unhelpful_count: 0,
        still_exists: false,
        verdict: "unavailable",
      },
      {
        ...common,
        state: "applied",
        canonical_fact_id: "fact-impossible",
        retrieval_count: 1,
        access_count: 1,
        helpful_count: 0,
        unhelpful_count: 0,
        still_exists: true,
        verdict: "never_recalled",
      },
      {
        ...common,
        state: "applied",
        canonical_fact_id: "fact-impossible",
        retrieval_count: 1,
        access_count: 0,
        helpful_count: 0,
        unhelpful_count: 0,
        still_exists: true,
        verdict: "recalled",
      },
      {
        ...common,
        state: "applied",
        canonical_fact_id: "fact-impossible",
        retrieval_count: 1,
        access_count: 1,
        helpful_count: 1,
        unhelpful_count: 0,
        still_exists: true,
        verdict: "recalled",
      },
    ];

    for (const fact of impossibleFacts) {
      const parsed = AutomationOutcomesPayloadSchema.safeParse({
        generated_at: 1_700_000_000,
        skills: [],
        facts: [fact],
        snapshot: {
          available: true,
          skills_refreshed_at: null,
          facts_refreshed_at: 1_700_000_000,
        },
        error: "",
      });
      expect(parsed.success).toBe(false);
    }
  });
});
