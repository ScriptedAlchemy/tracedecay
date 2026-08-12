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

  it("starts the policy-owned automatic curator with an empty request", async () => {
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
        String(input) === "/api/automation/run/memory-curator" &&
        (init as RequestInit | undefined)?.method === "POST",
    );
    expect(dispatch).toBeTruthy();
    expect((dispatch?.[1] as RequestInit).body).toBe("{}");
  });

  it("renders all six committed effect variants with exact durable identities", async () => {
    stubRoutes({ runResponse: { run: mixedAutomaticRun("run-dashboard") } });
    renderConsole();

    fireEvent.click(
      await screen.findByRole("button", { name: "Run automatic curator now" }),
    );

    expect(await screen.findByText(/add fact · fact fact\.v1\..* · added · committed · events event\.add/)).toBeTruthy();
    expect(await screen.findByText(/update fact · fact fact\.v1\..* · trust delta 25000 · committed · events event\.update/)).toBeTruthy();
    expect(await screen.findByText(/merge facts · winner fact\.v1\..* · deleted losers fact\.v1\..* · content unchanged · events event\.merge\.loser, event\.merge\.tombstone/)).toBeTruthy();
    expect(await screen.findByText(/remove fact · target fact\.v1\..* · removed · remaining 4 · committed · events event\.remove/)).toBeTruthy();
    expect(await screen.findByText(/normalize tags · fact fact\.v1\..* · committed · events event\.normalize\.fact, event\.normalize\.assertion/)).toBeTruthy();
    expect(await screen.findByText(/link facts · fact\.v1\..* → fact\.v1\..* · supports · committed · events event\.link/)).toBeTruthy();
  });

  it("renders accepted no-op effects without inventing commits", async () => {
    stubRoutes({ runResponse: { run: allNoopAutomaticRun("run-dashboard") } });
    renderConsole();

    fireEvent.click(
      await screen.findByRole("button", { name: "Run automatic curator now" }),
    );

    expect(await screen.findByText(/add fact · fact fact\.v1\..* · near duplicate · closest fact\.v1\..* · similarity 1000000 · no commit/)).toBeTruthy();
    expect(await screen.findByText(/remove fact · target fact\.v1\..* · not found · remaining 9 · no commit/)).toBeTruthy();
    expect(await screen.findByText("reviewed 2 · accepted 2 · rejected 0")).toBeTruthy();
  });

  it("preserves a committed partial-effect receipt and reconcile action", async () => {
    stubRoutes({ status: 409, runResponse: partialEffectProblem() });
    renderConsole();

    fireEvent.click(
      await screen.findByRole("button", { name: "Run automatic curator now" }),
    );

    expect(await screen.findByText(/curation committed before projection failed/)).toBeTruthy();
    expect(await screen.findByText("committed before failure · 1 accepted effect")).toBeTruthy();
    expect(
      await screen.findByText(
        /reconciliation required · 1 canonical receipt · admitted effect use-case\.application\.retained\.memory-automation-run · request request\.dashboard\.partial/,
      ),
    ).toBeTruthy();
    expect(await screen.findByText(/normalize tags · fact fact\.v1\./)).toBeTruthy();
    expect(await screen.findByText(/events event\.dashboard\.fact, event\.dashboard\.assertion/)).toBeTruthy();
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
          String(input) === "/api/automation/run/memory-curator" &&
          (init as RequestInit | undefined)?.method === "POST",
      ),
    ).toBe(false);
  });

  it("dispatches a selected active project through its exact project gateway", async () => {
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
            "/api/projects/project-active/automation/run/memory-curator" &&
          (init as RequestInit | undefined)?.method === "POST",
      ),
    ).toBe(true);
    expect(
      fetchMock.mock.calls.some(
        ([input, init]) =>
          String(input) === "/api/automation/run/memory-curator" &&
          (init as RequestInit | undefined)?.method === "POST",
      ),
    ).toBe(false);
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

function domFactId(seed: string): string {
  const ownerHash = createHash("sha256")
    .update(canonicalJson([
      "fact-owner.v1",
      { kind: "project", project_id: "project.dashboard" },
    ]))
    .digest("hex");
  return `fact.v1.${ownerHash}.${seed.padStart(64, "0")}`;
}

function domCommit(
  factId: string,
  events: string[],
  assertion: string | null,
) {
  return {
    disposition: "committed",
    fact_id: factId,
    owner: { kind: "project", project_id: "project.dashboard" },
    committed_event_ids: events,
    last_event_id: events.at(-1)!,
    active_assertion_id: assertion,
  };
}

function automaticRunWithCuration(
  runId: string,
  receipt: Record<string, unknown>,
  accepted: number,
) {
  return {
    run_id: runId,
    task: "memory_curator",
    terminal: {
      status: "completed",
      summary: {
        reviewed_count: accepted,
        accepted_count: accepted,
        rejected_count: 0,
        skipped_count: 0,
      },
    },
    committed_receipts: [{
      kind: "curation",
      receipt: {
        canonical_digest: canonicalSha([
          "tracedecay.memory-automation-run.curation-receipt.v1",
          receipt,
        ]),
        receipt,
      },
    }],
  };
}

function mixedAutomaticRun(runId: string) {
  const ids = Array.from({ length: 9 }, (_, index) => domFactId(String(index + 1)));
  const receipt = {
    owner: { kind: "project", project_id: "project.dashboard" },
    operation_id: "operation.dashboard.mixed",
    input_digest: "a".repeat(64),
    automation_run_id: runId,
    operation_effects: [
      {
        kind: "add",
        fact_id: ids[0],
        disposition: "added",
        closest_fact_id: null,
        similarity_millionths: null,
        commit: domCommit(ids[0]!, ["event.add"], "assertion.add"),
      },
      {
        kind: "update",
        fact_id: ids[1],
        trust_delta_millionths: 25_000,
        commit: domCommit(ids[1]!, ["event.update"], "assertion.update"),
      },
      {
        kind: "merge",
        outcome: {
          operation_id: "operation.dashboard.merge",
          input_digest: "b".repeat(64),
          winner_fact_id: ids[2],
          content_updated: false,
          deleted_loser_fact_ids: [ids[3]],
          commit_receipts: [domCommit(
            ids[3]!,
            ["event.merge.loser", "event.merge.tombstone"],
            null,
          )],
        },
      },
      {
        kind: "remove",
        target_fact_id: ids[4],
        disposition: "removed",
        remaining_fact_count: 4,
        commit: domCommit(ids[4]!, ["event.remove"], null),
      },
      {
        kind: "normalize_tags",
        fact_id: ids[5],
        commit: domCommit(
          ids[5]!,
          ["event.normalize.fact", "event.normalize.assertion"],
          "assertion.normalize",
        ),
      },
      {
        kind: "link_facts",
        source_fact_id: ids[6],
        target_fact_id: ids[7],
        relation: {
          kind: "supports",
          evidence_fact_ids: [ids[8]],
          confidence_millionths: 800_000,
          provenance: {
            source_label: "automation:memory-curator",
            sanitization_receipt: {
              receipt: {
                receipt_id: "receipt.dashboard.mixed",
                sanitizer_version: "sanitizer.dashboard.v1",
              },
              disposition: "accepted",
              sensitivity: "non_sensitive",
              payload: { digest: sha("9"), byte_len: 128 },
            },
          },
        },
        commit: domCommit(ids[6]!, ["event.link"], "assertion.link"),
      },
    ],
    replay_fact_id: ids[0],
    replay_event_id: "event.add",
    changed_fact_ids: [ids[0], ids[1], ids[3], ids[4], ids[5], ids[6], ids[7]],
    accepted_operations: 6,
    facts_added: 1,
    facts_updated: 1,
    facts_merged: 1,
    facts_removed: 1,
    normalized_tags: 1,
    facts_linked: 1,
  };
  return automaticRunWithCuration(runId, receipt, 6);
}

function allNoopAutomaticRun(runId: string) {
  const duplicate = domFactId("a");
  const receipt = {
    owner: { kind: "project", project_id: "project.dashboard" },
    operation_id: "operation.dashboard.noop",
    input_digest: "e".repeat(64),
    automation_run_id: runId,
    operation_effects: [
      {
        kind: "add",
        fact_id: duplicate,
        disposition: "near_duplicate",
        closest_fact_id: duplicate,
        similarity_millionths: 1_000_000,
        commit: null,
      },
      {
        kind: "remove",
        target_fact_id: domFactId("b"),
        disposition: "not_found",
        remaining_fact_count: 9,
        commit: null,
      },
    ],
    replay_fact_id: null,
    replay_event_id: null,
    changed_fact_ids: [],
    accepted_operations: 2,
    facts_added: 0,
    facts_updated: 0,
    facts_merged: 0,
    facts_removed: 0,
    normalized_tags: 0,
    facts_linked: 0,
  };
  return automaticRunWithCuration(runId, receipt, 2);
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
  const committedReceipts = kind === "partial_effect"
    ? [(() => {
        const ownerHash = createHash("sha256")
          .update(canonicalJson([
            "fact-owner.v1",
            { kind: "project", project_id: "project.dashboard" },
          ]))
          .digest("hex");
        const factId = `fact.v1.${ownerHash}.${"d".repeat(64)}`;
        const receipt = {
          owner: { kind: "project", project_id: "project.dashboard" },
          operation_id: "operation.dashboard",
          input_digest: "c".repeat(64),
          automation_run_id: "run.dashboard.partial",
          operation_effects: [{
            kind: "normalize_tags",
            fact_id: factId,
            commit: {
              disposition: "committed",
              fact_id: factId,
              owner: { kind: "project", project_id: "project.dashboard" },
              committed_event_ids: [
                "event.dashboard.fact",
                "event.dashboard.assertion",
              ],
              last_event_id: "event.dashboard.assertion",
              active_assertion_id: "assertion.dashboard",
            },
          }],
          replay_fact_id: factId,
          replay_event_id: "event.dashboard.assertion",
          changed_fact_ids: [factId],
          accepted_operations: 1,
          facts_added: 0,
          facts_updated: 0,
          facts_merged: 0,
          facts_removed: 0,
          normalized_tags: 1,
          facts_linked: 0,
        };
        return {
        kind: "curation",
        receipt: {
          canonical_digest: canonicalSha([
            "tracedecay.memory-automation-run.curation-receipt.v1",
            receipt,
          ]),
          receipt,
        },
      };
      })()]
    : [];
  const effectReceipt = kind === "partial_effect"
    ? {
        actor: "actor.dashboard",
        catalog_digest: sha("1"),
        committed_state: canonicalSha([
          "tracedecay.memory-automation-run.partial-state.v1",
          "run.dashboard.partial",
          committedReceipts,
        ]),
        configuration_digest: sha("3"),
        effect_class: "administrative",
        expected_state: sha("4"),
        external_proof: null,
        idempotency_key: "idempotency.dashboard",
        input_digest: sha("5"),
        operation: "use-case.application.retained.memory-automation-run",
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
      run_id: "run.dashboard.partial",
      task: "memory_curator",
      scope,
      problem: {
        contract: {
          schema_id: "schema.application.retained.memory-automation-run.result",
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
          unavailable_classification: null,
          execution_failure_classification: null,
          request_id: requestId,
          trace_id: requestId,
          details: [],
          legal_actions: [kind === "partial_effect" ? "reconcile" : "reset"],
          coverage: null,
        },
      },
      committed_receipts: committedReceipts,
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
        url.endsWith("/automation/run/memory-curator") &&
        init?.method === "POST";
      const body = isRunDispatch
        ? await (options?.runResponse ?? { run: automaticRun("run-dashboard") })
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
