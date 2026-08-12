import { createHash } from "node:crypto";

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useScope } from "../../data/scope/store.ts";
import { CurationConsole } from "./CurationConsole.tsx";

afterEach(() => {
  vi.unstubAllGlobals();
  useScope.getState().selectAllProjects();
});

describe("curation console", () => {
  it("shows canonical automatic runs and their post-activation outcomes", async () => {
    const fetchMock = stubRoutes();
    renderConsole();

    await screen.findByText("memory_curator");
    fireEvent.click(screen.getByRole("button", { name: /memory_curator/ }));
    expect(await screen.findByText(/chain integrity: verified/)).toBeTruthy();
    fireEvent.click(
      await screen.findByRole("button", { name: /inspect traces/i }),
    );
    expect(
      (await screen.findByLabelText("traces artifact payload")).textContent,
    ).toContain("rejected_ops");
    expect(await screen.findByText(/recalled and helpful/)).toBeTruthy();
    expect(
      screen.queryByRole("button", {
        name: /^(approve|reject|apply|submit|review|plan|config)\b/i,
      }),
    ).toBeNull();
    expect(
      fetchMock.mock.calls.filter(([, init]) =>
        (init as RequestInit | undefined)?.method === "POST"
      ),
    ).toEqual([]);
  });

  it("keeps computed outcome rows visible when only the activation snapshot failed", async () => {
    stubRoutes({ outcomesError: "snapshot store could not be read" });
    renderConsole();

    expect(await screen.findByText(/recalled and helpful/)).toBeTruthy();
    expect(
      await screen.findByText(/activation snapshot is unavailable: snapshot store could not be read/),
    ).toBeTruthy();
  });

  it("starts the policy-owned automatic curator with closed review bounds", async () => {
    const fetchMock = stubRoutes({
      runResponse: { run: automaticRun("run-dashboard") },
    });
    renderConsole();

    fireEvent.click(
      await screen.findByRole("button", { name: "Run automatic curator now" }),
    );

    expect(await screen.findByText(/run run-dashboard settled completed/)).toBeTruthy();
    const dispatch = fetchMock.mock.calls.find(
      ([input, init]) =>
        String(input) === "/api/application/retained/fact_store_curate" &&
        (init as RequestInit | undefined)?.method === "POST",
    );
    expect(dispatch).toBeTruthy();
    expect(JSON.parse(String((dispatch?.[1] as RequestInit).body))).toEqual({
      fact_review_limit: 24,
      min_confidence_millionths: 720_000,
    });
  });

  it("preserves a committed partial-effect receipt and reconcile action", async () => {
    stubRoutes({ status: 409, runResponse: partialEffectProblem() });
    renderConsole();

    fireEvent.click(
      await screen.findByRole("button", { name: "Run automatic curator now" }),
    );

    expect(await screen.findByText(/curation committed before projection failed/)).toBeTruthy();
    expect(
      await screen.findByText(
        /reconciliation required · committed effect use-case\.application\.retained\.fact-store-curate · request request\.dashboard\.partial/,
      ),
    ).toBeTruthy();
  });

  it("keeps reset-required separate from availability failure", async () => {
    stubRoutes({ status: 503, runResponse: resetRequiredProblem() });
    renderConsole();

    fireEvent.click(
      await screen.findByRole("button", { name: "Run automatic curator now" }),
    );

    expect(
      await screen.findByText(/reset required · the retained memory store must be reset/),
    ).toBeTruthy();
  });

  it("does not dispatch from a selected read-only project", async () => {
    const fetchMock = stubRoutes();
    useScope.getState().selectProject("project-selected", "Selected", "selected");
    renderConsole();

    const button = await screen.findByRole("button", {
      name: "Run automatic curator now",
    });
    expect((button as HTMLButtonElement).disabled).toBe(true);
    expect(await screen.findByText(/not the active project/)).toBeTruthy();
    expect(
      fetchMock.mock.calls.some(
        ([input, init]) =>
          String(input) ===
            "/api/projects/project-active/application/retained/fact_store_curate" &&
          (init as RequestInit | undefined)?.method === "POST",
      ),
    ).toBe(false);
  });

  it("dispatches an active project through the canonical application route", async () => {
    const fetchMock = stubRoutes();
    useScope.getState().selectProject("project-active", "Active", "active");
    renderConsole();

    fireEvent.click(
      await screen.findByRole("button", { name: "Run automatic curator now" }),
    );

    expect(
      await screen.findByText(/run run-dashboard settled completed/),
    ).toBeTruthy();
    expect(
      fetchMock.mock.calls.some(
        ([input, init]) =>
          String(input) ===
            "/api/projects/project-active/application/retained/fact_store_curate" &&
          (init as RequestInit | undefined)?.method === "POST",
      ),
    ).toBe(true);
    expect(
      fetchMock.mock.calls.filter(
        ([input, init]) =>
          String(input) ===
            "/api/projects/project-active/application/retained/fact_store_curate" &&
          (init as RequestInit | undefined)?.method === "POST",
      ).length,
    ).toBe(1);
  });

  it("does not show project A's settled run after scope changes to project B", async () => {
    let settleRun!: (body: unknown) => void;
    const delayedRun = new Promise<unknown>((resolve) => {
      settleRun = resolve;
    });
    stubRoutes({ runResponse: delayedRun });
    useScope.getState().selectProject("project-a", "Project A", "active");
    renderConsole();

    fireEvent.click(
      await screen.findByRole("button", { name: "Run automatic curator now" }),
    );
    act(() => {
      useScope.getState().selectProject("project-b", "Project B", "active");
    });

    await waitFor(() =>
      expect(
        (screen.getByRole("button", {
          name: "Run automatic curator now",
        }) as HTMLButtonElement).disabled,
      ).toBe(false),
    );
    expect(screen.queryByText("Running automatic curator…")).toBeNull();

    act(() => {
      settleRun({ run: automaticRun("run-project-a") });
    });

    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "Run automatic curator now" }),
      ).toBeTruthy(),
    );
    expect(screen.queryByText(/run run-project-a settled/)).toBeNull();
    expect(screen.getByText(/target: Project B/)).toBeTruthy();
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

function automaticRun(runId: string) {
  return {
    run_id: runId,
    task: "memory_curator",
    terminal: {
      status: "completed",
      summary: {
        reviewed_count: 0,
        accepted_count: 0,
        rejected_count: 0,
        skipped_count: 0,
      },
    },
    committed_receipts: [],
  };
}

const sha = (seed: string) => `sha256:${seed.repeat(64)}`;

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
  return JSON.stringify(value);
}

function canonicalSha(value: unknown): string {
  return `sha256:${createHash("sha256").update(canonicalJson(value)).digest("hex")}`;
}

function partialEffectProblem() {
  return automaticProblem("partial_effect");
}

function resetRequiredProblem() {
  return automaticProblem("reset_required");
}

function automaticProblem(kind: "partial_effect" | "reset_required") {
  const requestId = kind === "partial_effect"
    ? "request.dashboard.partial"
    : "request.dashboard.reset";
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
      binding_id: "binding.application.retained.fact-store-curate.http",
      contract: {
        schema_id: "schema.application.retained.fact-store-curate.result",
        schema_revision: 1,
      },
      request_id: requestId,
      problem: {
        revision: 1,
        kind,
        code: `automation.memory-curator.${kind}`,
        message: kind === "partial_effect"
          ? "curation committed before projection failed"
          : "the retained memory store must be reset",
        diagnostic: null,
        committed_receipt: effectReceipt,
        owning_layer: "runtime",
        terminality: "admitted_terminal",
        retryable: false,
        retry: "never",
        retry_scope: null,
        retry_after_millis: null,
        cancellation_stage: null,
        request_id: requestId,
        trace_id: requestId,
        details: [],
        legal_actions: [kind === "partial_effect" ? "reconcile" : "reset"],
        coverage: null,
      },
    },
  };
}

function curatorSuccess(run: unknown) {
  return {
    kind: "success",
    value: {
      binding_id: "binding.application.retained.fact-store-curate.http",
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
        scope_digest: sha("9"),
      },
      outcome: { outcome: "effect", value: { payload: run } },
    },
  };
}

function stubRoutes(options?: {
  status?: number;
  runResponse?: unknown | Promise<unknown>;
  outcomesError?: string;
}) {
  const runs = {
    runs: [
      {
        run_id: "run-1",
        trigger: "scheduler",
        task: "memory_curator",
        backend: "codex_app_server",
        model: null,
        status: "succeeded",
        reviewed_count: 0,
        accepted_count: 1,
        rejected_count: 1,
        skipped_count: 0,
        error: null,
        started_at: "2026-08-08T12:00:00Z",
        completed_at: "2026-08-08T12:00:02Z",
        artifact_kinds: ["traces"],
      },
    ],
    count: 1,
    limit: 50,
    has_more: false,
    malformed_row_count: 0,
    completeness: "known",
    error: "",
  };
  const outcomes = {
    generated_at: 1_700_000_000,
    skills: [],
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
    ],
    snapshot: {
      available: true,
      skills_refreshed_at: 1_700_000_000,
      facts_refreshed_at: 1_700_000_000,
    },
    error: options?.outcomesError ?? "",
  };
  const fetchMock = vi.fn(
    async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      const isRunDispatch =
        (url === "/api/application/retained/fact_store_curate" ||
          /^\/api\/projects\/[^/]+\/application\/retained\/fact_store_curate$/.test(
            url,
          )) &&
        init?.method === "POST";
      const rawRunResponse = isRunDispatch
        ? await (options?.runResponse ?? { run: automaticRun("run-dashboard") })
        : undefined;
      const body = isRunDispatch
        ? options?.status === undefined &&
          typeof rawRunResponse === "object" &&
          rawRunResponse !== null &&
          "run" in rawRunResponse
          ? curatorSuccess((rawRunResponse as { run: unknown }).run)
          : rawRunResponse
        : url.endsWith("/automation/runs/run-1/artifacts/traces")
          ? {
              run_id: "run-1",
              artifact: {
                schema_version: 1,
                kind: "traces",
                path: "runs/run-1/traces.json",
                sha256: "a".repeat(64),
                created_at: "2026-08-08T12:00:02Z",
              },
              payload: {
                curation_result: {
                  status: "succeeded",
                  reviewed_count: 2,
                  accepted_count: 1,
                  rejected_count: 1,
                  applied_ops: [{ op: "normalize_tags" }],
                  rejected_ops: [{ op: "link_facts", reason: "missing evidence" }],
                  validation_report: { decision: "automatic" },
                },
              },
              error: "",
            }
          : url.endsWith("/automation/runs/run-1/artifacts")
            ? {
                run_id: "run-1",
                artifacts: [{
                  schema_version: 1,
                  kind: "traces",
                  path: "runs/run-1/traces.json",
                  sha256: "a".repeat(64),
                  created_at: "2026-08-08T12:00:02Z",
                }],
                artifact_chain: {
                  expected_kinds: ["traces"],
                  present_kinds: ["traces"],
                  metadata_complete: true,
                  complete: true,
                  integrity_status: "verified",
                },
                count: 1,
                error: "",
              }
            : url.endsWith("/automation/runs")
              ? runs
          : url.includes("/automation/outcomes")
            ? outcomes
            : {};
      return new Response(JSON.stringify(body), {
        status: isRunDispatch ? (options?.status ?? 200) : 200,
        headers: { "content-type": "application/json" },
      });
    },
  );
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}
