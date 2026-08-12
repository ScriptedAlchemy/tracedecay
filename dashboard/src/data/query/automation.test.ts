import { createHash } from "node:crypto";

import { afterEach, describe, expect, it, vi } from "vitest";

import type { AutomationCommittedReceiptV1 } from "../../contracts/generated.ts";
import {
  AutomationOutcomesPayloadSchema,
  AutomationSchedulerStatusV1Schema,
  runAutomaticCurator,
  setSchedulerPaused,
} from "./automation.ts";

type CurationReceipt = Extract<
  AutomationCommittedReceiptV1,
  { kind: "curation" }
>;
type LinkCurationEffect = Extract<
  CurationReceipt["receipt"]["receipt"]["operation_effects"][number],
  { kind: "link_facts" }
>;
type NormalizeCurationEffect = Extract<
  CurationReceipt["receipt"]["receipt"]["operation_effects"][number],
  { kind: "normalize_tags" }
>;

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
  it("decodes the retained application terminal with explicit caller bounds", async () => {
    respond({ run: automaticRun("run-dashboard") });

    const result = await runAutomaticCurator();
    expect(result.outcome).toBe("ok");
    const call = vi.mocked(fetch).mock.calls[0];
    expect(call?.[0]).toBe("/api/application/retained/fact_store_curate");
    expect(JSON.parse(String((call?.[1] as RequestInit).body))).toEqual({
      fact_review_limit: 24,
      min_confidence_millionths: 720_000,
    });
  });

  it("rejects success and admitted-problem envelopes from another HTTP binding", async () => {
    const success = curatorSuccess(automaticRun("run-dashboard"));
    success.value.binding_id = "binding.http.fact_store_get.v1";
    respond(success);
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");

    const problem = automaticProblem("reset_required");
    problem.value.binding_id = "binding.http.fact_store_get.v1";
    respond(problem, { ok: false, statusCode: 503 });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("rejects success and admitted-problem envelopes from another result contract", async () => {
    const success = curatorSuccess(automaticRun("run-dashboard"));
    success.value.contract.schema_id =
      "schema.application.retained.fact-store-get.result";
    respond(success);
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");

    const problem = automaticProblem("reset_required");
    problem.value.contract.schema_revision = 2;
    respond(problem, { ok: false, statusCode: 503 });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("rejects malformed success request identity and scope digest", async () => {
    const wrongRequest = curatorSuccess(automaticRun("run-dashboard"));
    wrongRequest.value.request_id = " request.dashboard.success";
    respond(wrongRequest);
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");

    const success = curatorSuccess(automaticRun("run-dashboard"));
    success.value.scope.scope_digest = sha("9");
    respond(success);
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("rejects a run for another automatic-memory task", async () => {
    const run = automaticRun("run-dashboard");
    run.task = "session_reflector";
    respond({ run });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("binds zero-effect terminals to the exact admitted curator bounds", async () => {
    const completed = automaticRun("run-dashboard");
    completed.request_digest = sha("f");
    respond({ run: completed });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");

    respond({
      run: {
        ...automaticRun("run-dashboard"),
        request_digest: sha("f"),
        terminal: {
          status: "skipped",
          reason: "nothing_to_review",
          summary: {
            reviewed_count: 0,
            accepted_count: 0,
            rejected_count: 0,
            skipped_count: 1,
          },
        },
      },
    });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("binds a curation receipt to the run and summary", async () => {
    const accepted = automaticRun("run-dashboard");
    accepted.terminal.summary.reviewed_count = 1;
    accepted.terminal.summary.accepted_count = 1;
    accepted.committed_receipts = [curationReceipt("run-dashboard")];
    respond({ run: accepted });
    expect((await runAutomaticCurator()).outcome).toBe("ok");

    const run = automaticRun("run-dashboard");
    run.terminal.summary.reviewed_count = 1;
    run.terminal.summary.accepted_count = 1;
    run.committed_receipts = [curationReceipt("run-other")];
    respond({ run });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("accepts the Rust curation digest domain and rejects the retired legacy domain", async () => {
    const canonical = curationReceipt("run-dashboard");
    expect(canonical.receipt.receipt.operation_effects).not.toHaveLength(0);
    respond({ run: automaticRunWithReceipt("run-dashboard", canonical) });
    expect((await runAutomaticCurator()).outcome).toBe("ok");

    const legacy = curationReceipt("run-dashboard");
    legacy.receipt.canonical_digest = canonicalSha([
      "tracedecay.memory-automation-run.curation-receipt.v1",
      legacy.receipt.receipt,
    ]);
    respond({ run: automaticRunWithReceipt("run-dashboard", legacy) });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("rejects curation effects whose canonical commit ledger is inconsistent", async () => {
    const run = automaticRun("run-dashboard");
    run.terminal.summary.reviewed_count = 1;
    run.terminal.summary.accepted_count = 1;
    const receipt = curationReceipt("run-dashboard");
    firstNormalizeEffect(receipt).commit.last_event_id = "event.dashboard.not-the-tail";
    refreshCurationDigest(receipt);
    run.committed_receipts = [receipt];
    respond({ run });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("rejects more than 256 otherwise canonical curation effects", async () => {
    const receipt = curationReceipt("run-dashboard");
    const template = firstNormalizeEffect(receipt);
    receipt.receipt.receipt.operation_effects = Array.from(
      { length: 257 },
      (_, index) => {
        const id = factId(index.toString(16).padStart(64, "0"));
        return {
          ...template,
          fact_id: id,
          commit: {
            ...template.commit,
            fact_id: id,
            committed_event_ids: [
              `event.dashboard.${index}.fact`,
              `event.dashboard.${index}.assertion`,
            ],
            last_event_id: `event.dashboard.${index}.assertion`,
          },
        };
      },
    );
    receipt.receipt.receipt.changed_fact_ids =
      receipt.receipt.receipt.operation_effects.map((effect) => {
        if (effect.kind !== "normalize_tags") throw new Error("normalize fixture drifted");
        return effect.fact_id;
      });
    receipt.receipt.receipt.normalized_tags = 257;
    receipt.receipt.receipt.accepted_operations = 257;
    refreshCurationDigest(receipt);

    respond({ run: automaticRunWithReceipt("run-dashboard", receipt) });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("rejects a duplicate operation identity even with fresh commit events", async () => {
    const receipt = curationReceipt("run-dashboard");
    const duplicate = structuredClone(firstNormalizeEffect(receipt));
    duplicate.commit.committed_event_ids = [
      "event.dashboard.duplicate.fact",
      "event.dashboard.duplicate.assertion",
    ];
    duplicate.commit.last_event_id = "event.dashboard.duplicate.assertion";
    receipt.receipt.receipt.operation_effects.push(duplicate);
    receipt.receipt.receipt.normalized_tags = 2;
    receipt.receipt.receipt.accepted_operations = 2;
    refreshCurationDigest(receipt);

    respond({ run: automaticRunWithReceipt("run-dashboard", receipt) });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("rejects mixed commit dispositions within one curation receipt", async () => {
    const receipt = curationReceipt("run-dashboard");
    const second = structuredClone(firstNormalizeEffect(receipt));
    const secondFactId = factId("2");
    second.fact_id = secondFactId;
    second.commit = {
      ...second.commit,
      disposition: "idempotent_replay",
      fact_id: secondFactId,
      committed_event_ids: [
        "event.dashboard.replay.fact",
        "event.dashboard.replay.assertion",
      ],
      last_event_id: "event.dashboard.replay.assertion",
    };
    receipt.receipt.receipt.operation_effects.push(second);
    receipt.receipt.receipt.changed_fact_ids.push(secondFactId);
    receipt.receipt.receipt.normalized_tags = 2;
    receipt.receipt.receipt.accepted_operations = 2;
    refreshCurationDigest(receipt);

    respond({ run: automaticRunWithReceipt("run-dashboard", receipt) });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("rejects link effects with foreign identities or non-durable provenance", async () => {
    const invalidReceipts = [
      mutateLinkReceipt((effect) => {
        if (effect.commit === null) throw new Error("link fixture omitted its commit");
        effect.commit.owner = { kind: "project", project_id: "project.other" };
      }),
      mutateLinkReceipt((effect) => {
        effect.relation.evidence_fact_ids = [foreignFactId("4")];
      }),
      mutateLinkReceipt((effect, receipt) => {
        if (effect.commit === null) throw new Error("link fixture omitted its commit");
        const foreignSource = foreignFactId("1");
        effect.source_fact_id = foreignSource;
        effect.commit.fact_id = foreignSource;
        receipt.receipt.receipt.replay_fact_id = foreignSource;
        receipt.receipt.receipt.changed_fact_ids = [
          foreignSource,
          effect.target_fact_id,
        ];
      }),
      mutateLinkReceipt((effect, receipt) => {
        const foreignTarget = foreignFactId("2");
        effect.target_fact_id = foreignTarget;
        receipt.receipt.receipt.changed_fact_ids = [
          effect.source_fact_id,
          foreignTarget,
        ];
      }),
      mutateLinkReceipt((effect) => {
        effect.relation.evidence_fact_ids = [factId("3"), factId("3")];
      }),
      mutateLinkReceipt((effect) => {
        effect.relation.evidence_fact_ids = Array.from(
          { length: 257 },
          (_, index) => factId(index.toString(16).padStart(64, "0")),
        );
      }),
      mutateLinkReceipt((effect) => {
        effect.relation.provenance.source_label = "automation:\u0085memory-curator";
      }),
      mutateLinkReceipt((effect) => {
        effect.relation.provenance.source_label = "é".repeat(2_049);
      }),
      mutateLinkReceipt((effect) => {
        effect.relation.provenance.sanitization_receipt.disposition = "rejected";
      }),
      mutateLinkReceipt((effect) => {
        effect.relation.provenance.sanitization_receipt.payload = null;
      }),
    ];

    for (const receipt of invalidReceipts) {
      respond({ run: automaticRunWithReceipt("run-dashboard", receipt) });
      expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
    }
  });

  it("accepts a semantic already-linked effect without fabricating a mutation", async () => {
    const receipt = linkCurationReceipt("run-dashboard");
    const effect = receipt.receipt.receipt.operation_effects[0];
    if (effect?.kind !== "link_facts") throw new Error("link fixture drifted");
    effect.disposition = "already_linked";
    effect.commit = null;
    receipt.receipt.receipt.replay_fact_id = null;
    receipt.receipt.receipt.replay_event_id = null;
    receipt.receipt.receipt.changed_fact_ids = [];
    receipt.receipt.receipt.facts_linked = 0;
    refreshCurationDigest(receipt);

    respond({ run: automaticRunWithReceipt("run-dashboard", receipt) });
    expect((await runAutomaticCurator()).outcome).toBe("ok");
  });

  it("rejects malformed raw input and canonical receipt digests", async () => {
    const malformed = [
      mutateCurationReceipt((receipt) => {
        receipt.receipt.receipt.input_digest = sha("c");
        refreshCurationDigest(receipt);
      }),
      mutateCurationReceipt((receipt) => {
        receipt.receipt.receipt.input_digest = "C".repeat(64);
        refreshCurationDigest(receipt);
      }),
      mutateCurationReceipt((receipt) => {
        receipt.receipt.canonical_digest = "b".repeat(64);
      }),
      mutateCurationReceipt((receipt) => {
        receipt.receipt.canonical_digest = sha("b");
      }),
    ];

    for (const receipt of malformed) {
      respond({ run: automaticRunWithReceipt("run-dashboard", receipt) });
      expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
    }
  });

  it("rejects non-canonical curator skipped and rejected summaries", async () => {
    respond({
      run: {
        ...automaticRun("run-dashboard"),
        terminal: {
          status: "skipped",
          reason: "nothing_to_review",
          summary: {
            reviewed_count: 0,
            accepted_count: 0,
            rejected_count: 0,
            skipped_count: 2,
          },
        },
      },
    });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");

    const run = automaticRun("run-dashboard");
    run.terminal.summary.reviewed_count = 1;
    run.terminal.summary.rejected_count = 1;
    respond({ run });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("rejects a session-reflector-only skip reason for the curator", async () => {
    respond({
      run: {
        ...automaticRun("run-dashboard"),
        terminal: {
          status: "skipped",
          reason: "session_evidence_unavailable",
          summary: {
            reviewed_count: 0,
            accepted_count: 0,
            rejected_count: 0,
            skipped_count: 1,
          },
        },
      },
    });
    expect((await runAutomaticCurator()).outcome).toBe("unsupported_schema");
  });

  it("decodes the generated partial-effect terminal", async () => {
    respond(automaticProblem("partial_effect"), { ok: false, statusCode: 409 });
    expect((await runScopedAutomaticCurator()).outcome).toBe("partial_effect");
  });

  it("rejects a partial terminal belonging to another application effect", async () => {
    const body = automaticProblem("partial_effect");
    const receipt = body.value.problem.committed_receipt;
    if (receipt === null) throw new Error("partial fixture drifted");
    receipt.operation = "use-case.application.retained.fact-store-add";
    respond(body, { ok: false, statusCode: 409 });
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

  it("accepts canonical partial and reset terminals from the active-project route", async () => {
    respond(automaticProblem("partial_effect"), { ok: false, statusCode: 409 });
    expect((await runAutomaticCurator()).outcome).toBe("partial_effect");

    respond(automaticProblem("reset_required"), { ok: false, statusCode: 503 });
    expect((await runAutomaticCurator()).outcome).toBe("reset_required");
  });

});

const sha = (seed: string) => `sha256:${seed.repeat(64)}`;
const PROJECT_OWNER_BINDING =
  "cdab36393497f7ad3d6e0144484b711458ed01517e7108bc1dbf8cc0e3b33f88";
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

function factId(seed: string): string {
  return `fact.v1.${PROJECT_OWNER_BINDING}.${seed.padStart(64, "0")}`;
}

function foreignFactId(seed: string): string {
  return `fact.v1.${"e".repeat(64)}.${seed.padStart(64, "0")}`;
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

function curationReceipt(runId: string): CurationReceipt {
  const normalizedFactId = factId("1");
  const receipt: CurationReceipt = {
    kind: "curation" as const,
    receipt: {
      canonical_digest: "",
      receipt: {
        owner: { kind: "project" as const, project_id: "project.dashboard" },
        operation_id: "operation.dashboard",
        input_digest: "c".repeat(64),
        automation_run_id: runId,
        operation_effects: [
          {
            kind: "normalize_tags" as const,
            fact_id: normalizedFactId,
            commit: {
              disposition: "committed" as const,
              fact_id: normalizedFactId,
              owner: { kind: "project" as const, project_id: "project.dashboard" },
              committed_event_ids: ["event.dashboard.fact", "event.dashboard.assertion"],
              last_event_id: "event.dashboard.assertion",
              active_assertion_id: "assertion.dashboard",
            },
          },
        ],
        replay_fact_id: normalizedFactId,
        replay_event_id: "event.dashboard.assertion",
        changed_fact_ids: [normalizedFactId],
        accepted_operations: 1,
        facts_added: 0,
        facts_updated: 0,
        facts_merged: 0,
        facts_removed: 0,
        normalized_tags: 1,
        facts_linked: 0,
      },
    },
  };
  return refreshCurationDigest(receipt);
}

function linkCurationReceipt(runId: string): CurationReceipt {
  const sourceFactId = factId("1");
  const targetFactId = factId("2");
  const receipt: CurationReceipt = {
    kind: "curation" as const,
    receipt: {
      canonical_digest: "",
      receipt: {
        owner: { kind: "project" as const, project_id: "project.dashboard" },
        operation_id: "operation.dashboard.link",
        input_digest: "d".repeat(64),
        automation_run_id: runId,
        operation_effects: [
          {
            kind: "link_facts" as const,
            source_fact_id: sourceFactId,
            target_fact_id: targetFactId,
            relation: {
              kind: "supports" as const,
              evidence_fact_ids: [factId("3")],
              confidence_millionths: 800_000,
              provenance: {
                source_label: "automation:memory-curator",
                sanitization_receipt: {
                  receipt: {
                    receipt_id: "receipt.dashboard.relation",
                    sanitizer_version: "sanitizer.dashboard.v1",
                  },
                  disposition: "accepted" as const,
                  sensitivity: "non_sensitive" as const,
                  payload: { digest: sha("9"), byte_len: 128 },
                },
              },
            },
            disposition: "linked" as const,
            commit: {
              disposition: "committed" as const,
              fact_id: sourceFactId,
              owner: { kind: "project" as const, project_id: "project.dashboard" },
              committed_event_ids: ["event.dashboard.link"],
              last_event_id: "event.dashboard.link",
              active_assertion_id: "assertion.dashboard.link",
            },
          },
        ],
        replay_fact_id: sourceFactId,
        replay_event_id: "event.dashboard.link",
        changed_fact_ids: [sourceFactId, targetFactId],
        accepted_operations: 1,
        facts_added: 0,
        facts_updated: 0,
        facts_merged: 0,
        facts_removed: 0,
        normalized_tags: 0,
        facts_linked: 1,
      },
    },
  };
  return refreshCurationDigest(receipt);
}

function refreshCurationDigest(receipt: CurationReceipt): CurationReceipt {
  receipt.receipt.canonical_digest = canonicalSha([
    "tracedecay.automation-run.curation-receipt.v1",
    receipt.receipt.receipt,
  ]);
  return receipt;
}

function firstNormalizeEffect(
  receipt: CurationReceipt,
): NormalizeCurationEffect {
  const effect = receipt.receipt.receipt.operation_effects[0];
  if (effect?.kind !== "normalize_tags") throw new Error("normalize fixture drifted");
  return effect;
}

function mutateCurationReceipt(
  mutate: (receipt: CurationReceipt) => void,
) {
  const receipt = curationReceipt("run-dashboard");
  mutate(receipt);
  return receipt;
}

function mutateLinkReceipt(
  mutate: (effect: LinkCurationEffect, receipt: CurationReceipt) => void,
) {
  const receipt = linkCurationReceipt("run-dashboard");
  const effect = receipt.receipt.receipt.operation_effects[0];
  if (effect?.kind !== "link_facts") throw new Error("link fixture drifted");
  mutate(effect, receipt);
  refreshCurationDigest(receipt);
  return receipt;
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
        value: { payload: run },
      },
    },
  };
}

function automaticProblem(kind: "partial_effect" | "reset_required") {
  const requestId = `request.dashboard.${kind}`;
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
