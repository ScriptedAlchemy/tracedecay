import { createHash } from "node:crypto";

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { AutomationCommittedReceiptV1 } from "../../contracts/generated.ts";
import { useScope } from "../../data/scope/store.ts";
import { CurationConsole } from "./CurationConsole.tsx";

type CurationReceipt = Extract<
  AutomationCommittedReceiptV1,
  { kind: "curation" }
>;

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

  it("renders the canonical commit disposition for every committed effect", async () => {
    stubRoutes({
      runResponse: {
        run: automaticRunWithReceipt(
          "run-dashboard-effects",
          committedEffectsReceipt("run-dashboard-effects"),
        ),
      },
    });
    renderConsole();

    fireEvent.click(
      await screen.findByRole("button", { name: "Run automatic curator now" }),
    );

    const effects = await screen.findByLabelText("Committed curator effects");
    expect(effects.querySelectorAll("li")).toHaveLength(6);
    for (const label of [
      "add fact",
      "link facts",
      "merge facts",
      "normalize tags",
      "remove fact",
      "update fact",
    ]) {
      expect(effects.textContent).toContain(label);
    }
    expect(effects.textContent?.match(/idempotent_replay/g)).toHaveLength(6);
  });

  it("suppresses effects whose canonical commit is absent", async () => {
    stubRoutes({
      runResponse: {
        run: automaticRunWithReceipt(
          "run-dashboard-nullable-effects",
          nullableEffectsReceipt("run-dashboard-nullable-effects"),
        ),
      },
    });
    renderConsole();

    fireEvent.click(
      await screen.findByRole("button", { name: "Run automatic curator now" }),
    );

    expect(
      await screen.findByText(
        /run run-dashboard-nullable-effects settled completed/,
      ),
    ).toBeTruthy();
    expect(screen.queryByLabelText("Committed curator effects")).toBeNull();
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
    request_digest: automaticRequestDigest(),
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
    committed_receipts: [] as CurationReceipt[],
  };
}

function automaticRunWithReceipt(
  runId: string,
  receipt: CurationReceipt,
) {
  const run = automaticRun(runId);
  run.terminal.summary.reviewed_count =
    receipt.receipt.receipt.accepted_operations;
  run.terminal.summary.accepted_count = run.terminal.summary.reviewed_count;
  run.committed_receipts = [receipt];
  return run;
}

function committedEffectsReceipt(runId: string): CurationReceipt {
  const addFact = factId("1");
  const linkSource = factId("2");
  const linkTarget = factId("3");
  const mergeWinner = factId("5");
  const mergeLoser = factId("6");
  const normalizedFact = factId("7");
  const removedFact = factId("8");
  const updatedFact = factId("9");
  const receipt: CurationReceipt = {
    kind: "curation",
    receipt: {
      canonical_digest: "",
      receipt: {
        owner: { kind: "project", project_id: "project.dashboard" },
        operation_id: "operation.dashboard.effects",
        input_digest: "c".repeat(64),
        automation_run_id: runId,
        operation_effects: [
          {
            kind: "add",
            fact_id: addFact,
            closest_fact_id: null,
            similarity_millionths: null,
            disposition: "added",
            commit: factCommit(addFact, "add", 1, "assertion.dashboard.add"),
          },
          {
            kind: "link_facts",
            source_fact_id: linkSource,
            target_fact_id: linkTarget,
            relation: {
              kind: "supports",
              evidence_fact_ids: [factId("4")],
              confidence_millionths: 800_000,
              provenance: {
                source_label: "automation:memory-curator",
                sanitization_receipt: {
                  receipt: {
                    receipt_id: "receipt.dashboard.relation",
                    sanitizer_version: "sanitizer.dashboard.v1",
                  },
                  disposition: "accepted",
                  sensitivity: "non_sensitive",
                  payload: { digest: sha("9"), byte_len: 128 },
                },
              },
            },
            disposition: "linked",
            commit: factCommit(
              linkSource,
              "link",
              1,
              "assertion.dashboard.link",
            ),
          },
          {
            kind: "merge",
            outcome: {
              operation_id: "operation.dashboard.merge",
              input_digest: "d".repeat(64),
              winner_fact_id: mergeWinner,
              deleted_loser_fact_ids: [mergeLoser],
              content_updated: false,
              commit_receipts: [factCommit(mergeLoser, "merge", 2, null)],
            },
          },
          {
            kind: "normalize_tags",
            fact_id: normalizedFact,
            commit: factCommit(
              normalizedFact,
              "normalize",
              2,
              "assertion.dashboard.normalize",
            ),
          },
          {
            kind: "remove",
            target_fact_id: removedFact,
            disposition: "removed",
            remaining_fact_count: 8,
            commit: factCommit(removedFact, "remove", 1, null),
          },
          {
            kind: "update",
            fact_id: updatedFact,
            trust_delta_millionths: 100_000,
            commit: factCommit(
              updatedFact,
              "update",
              1,
              "assertion.dashboard.update",
            ),
          },
        ],
        replay_fact_id: addFact,
        replay_event_id: "event.dashboard.add.1",
        changed_fact_ids: [
          addFact,
          linkSource,
          linkTarget,
          mergeLoser,
          normalizedFact,
          removedFact,
          updatedFact,
        ],
        accepted_operations: 6,
        facts_added: 1,
        facts_updated: 1,
        facts_merged: 1,
        facts_removed: 1,
        normalized_tags: 1,
        facts_linked: 1,
      },
    },
  };
  return refreshCurationDigest(receipt);
}

function nullableEffectsReceipt(runId: string): CurationReceipt {
  const existingFact = factId("a");
  const receipt: CurationReceipt = {
    kind: "curation",
    receipt: {
      canonical_digest: "",
      receipt: {
        owner: { kind: "project", project_id: "project.dashboard" },
        operation_id: "operation.dashboard.nullable-effects",
        input_digest: "e".repeat(64),
        automation_run_id: runId,
        operation_effects: [
          {
            kind: "add",
            fact_id: existingFact,
            closest_fact_id: existingFact,
            similarity_millionths: 1_000_000,
            disposition: "near_duplicate",
            commit: null,
          },
          {
            kind: "link_facts",
            source_fact_id: factId("b"),
            target_fact_id: factId("c"),
            relation: {
              kind: "supports",
              evidence_fact_ids: [factId("d")],
              confidence_millionths: 800_000,
              provenance: {
                source_label: "automation:memory-curator",
                sanitization_receipt: {
                  receipt: {
                    receipt_id: "receipt.dashboard.nullable-relation",
                    sanitizer_version: "sanitizer.dashboard.v1",
                  },
                  disposition: "accepted",
                  sensitivity: "non_sensitive",
                  payload: { digest: sha("8"), byte_len: 128 },
                },
              },
            },
            disposition: "already_linked",
            commit: null,
          },
          {
            kind: "remove",
            target_fact_id: factId("e"),
            disposition: "not_found",
            remaining_fact_count: 9,
            commit: null,
          },
        ],
        replay_fact_id: null,
        replay_event_id: null,
        changed_fact_ids: [],
        accepted_operations: 3,
        facts_added: 0,
        facts_updated: 0,
        facts_merged: 0,
        facts_removed: 0,
        normalized_tags: 0,
        facts_linked: 0,
      },
    },
  };
  return refreshCurationDigest(receipt);
}

function factCommit(
  fact: string,
  eventLabel: string,
  eventCount: number,
  activeAssertionId: string | null,
) {
  const committedEventIds = Array.from(
    { length: eventCount },
    (_, index) => `event.dashboard.${eventLabel}.${index + 1}`,
  );
  return {
    disposition: "idempotent_replay" as const,
    fact_id: fact,
    owner: { kind: "project" as const, project_id: "project.dashboard" },
    committed_event_ids: committedEventIds,
    last_event_id: committedEventIds.at(-1)!,
    active_assertion_id: activeAssertionId,
  };
}

function refreshCurationDigest(receipt: CurationReceipt): CurationReceipt {
  receipt.receipt.canonical_digest = canonicalSha([
    "tracedecay.automation-run.curation-receipt.v1",
    receipt.receipt.receipt,
  ]);
  return receipt;
}

function factId(seed: string): string {
  const ownerBinding = canonicalSha([
    "fact-owner.v1",
    { kind: "project", project_id: "project.dashboard" },
  ]).slice("sha256:".length);
  return `fact.v1.${ownerBinding}.${seed.padStart(64, "0")}`;
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
      binding_id: "binding.http.fact_store_curate.v1",
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
        execution_failure_classification: null,
        request_id: requestId,
        trace_id: requestId,
        details: [],
        legal_actions: [kind === "partial_effect" ? "reconcile" : "reset"],
        coverage: null,
        unavailable_classification: null,
      },
    },
  };
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
