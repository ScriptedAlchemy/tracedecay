/**
 * Reads and controls for the automation scheduler.
 *
 * `automation_scheduler_api.rs` answers `status`, `pause`, and `resume` with
 * the *same* payload — the controls re-read rather than acknowledge — and that
 * is what makes an honest control possible here. A route that replied
 * `{"ok":true}` would leave this module to assume the new state and flip the
 * toggle on faith; because the server returns the reading it just took, the
 * control can seed the query cache with the server's answer and the UI never
 * shows a pause it has not observed.
 *
 * So there is deliberately no optimistic update below. Optimism is the ordinary
 * React Query idiom for a toggle, and it is the wrong one for this surface: it
 * would paint the scheduler paused the instant a user clicked, which is exactly
 * a control state asserted rather than measured. A failed control leaves the
 * last real reading on screen and reports the failure beside it.
 */
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { z } from "zod";

import { fetchPayloadWrite, type PayloadWriteResult } from "./payload.ts";
import { usePayload } from "./usePayload.ts";
import {
  scopeKey,
  scopeWritable,
  scopedQueryKey,
  scopedUrl,
  useScope,
  type ScopeWritability,
} from "../scope/store.ts";
import {
  AutomationSchedulerStatusV1Schema,
  ApplicationProblemEnvelopeSchema,
  AutomationRunResultV1Schema,
  ResolvedScopeSchema,
  type ApplicationProblemEnvelope,
  type AutomationSchedulerStatusV1,
  type AutomationRunResultV1,
  type ResolvedScope,
} from "../../contracts/generated.ts";

// Re-export the generated validator for the query-layer tests and consumers.
// The generated contract remains the sole schema authority; this is only a
// module boundary convenience, not a second copy of the wire contract.
export { AutomationSchedulerStatusV1Schema };

export const automationSchedulerKey = ["automation", "scheduler"] as const;

export const schedulerStatusUrl = "/api/automation/scheduler/status";

/**
 * Pause or resume the scheduler, returning the reading the server took after
 * applying the change.
 *
 * Pause and resume are separate routes rather than one route taking a boolean,
 * which makes each request idempotent: re-sending `pause` on an already-paused
 * scheduler is a no-op that still returns the true state, so a retry after a
 * dropped response cannot toggle something twice.
 */
export function setSchedulerPaused(
  url: string,
): Promise<PayloadWriteResult<AutomationSchedulerStatusV1>> {
  return fetchPayloadWrite(url, AutomationSchedulerStatusV1Schema, {
    method: "POST",
  });
}

/**
 * What a control attempt produced, including the case where there was no
 * attempt.
 *
 * `not_dispatched` is not a failure of the write — it is the absence of one,
 * and it stays separate for the same reason Settings keeps `unavailable` apart
 * from `error`: nothing was sent, so nothing changed, and the surface must not
 * imply the scheduler was asked and refused.
 */
export type SchedulerControlResult =
  | PayloadWriteResult<AutomationSchedulerStatusV1>
  | { outcome: "not_dispatched"; writability: ScopeWritability };

/**
 * The scope a control attempt was issued under, captured when it was issued.
 *
 * Carried as mutation context rather than read again at settlement, because
 * the two moments can disagree. `useSchedulerControl` derives its key from the
 * scope of the render it last ran in, and React Query invokes the settlement
 * callbacks from the CURRENT options — so a pause dispatched against project A
 * that is still in flight when the reader switches to project B would have
 * settled against B's key: A's scheduler reading written into B's cache entry,
 * or B's entry invalidated because A's write failed. Either way one project's
 * panel would be answering for another's, which is the one thing a scoped
 * surface may never do.
 */
interface SchedulerDispatch {
  /** The status cache entry belonging to the project the write was sent to. */
  readonly statusKey: readonly unknown[];
}

/**
 * The scheduler control as a mutation.
 *
 * On success the returned reading is written straight into the status query's
 * cache entry, so the badge and tiles update from the server's own answer
 * rather than from a refetch that could race, and without a window where the
 * screen shows the pre-control state as though the control had not run. It is
 * written to the entry belonging to the project that was dispatched to — see
 * {@link SchedulerDispatch}.
 *
 * Returns the scope authority alongside the mutation, so the control that
 * renders the button and the mutation that would dispatch it read the same
 * value rather than each taking their own.
 */
export function useSchedulerControl() {
  const scope = useScope((s) => s.scope);
  const client = useQueryClient();
  // The status read's own key, from the authority that builds it, not a second
  // construction of it. `scopeKey(scope)` was the second construction and it
  // disagreed with the read under the all-projects default — see
  // {@link scopedQueryKey}.
  const statusKey = scopedQueryKey(
    scope,
    automationSchedulerKey,
    schedulerStatusUrl,
  );
  // The control's own reading of the scope authority, so what disables the
  // button and what would refuse a dispatch are one value rather than two
  // that can drift.
  const writability = scopeWritable(scope);
  const mutation = useMutation<
    SchedulerControlResult,
    Error,
    boolean,
    SchedulerDispatch
  >({
    // Distinguishes concurrent dispatches by the scope each was sent under, so
    // two projects' controls are two mutations rather than one shared entry.
    mutationKey: [...automationSchedulerKey, scopeKey(scope)],
    // Runs immediately before `mutationFn`, from the same options snapshot, so
    // this is the scope the request is actually about to be sent under.
    onMutate: () => ({ statusKey }),
    mutationFn: async (paused: boolean) => {
      // Nothing leaves the browser unless the scope is known to accept it. The
      // button is disabled on this same reading, so arriving here means the
      // disable was bypassed — and dispatching anyway would trade a stated
      // reason for a 405 that this layer cannot tell apart from a route that
      // has gone away.
      if (writability.state !== "writable") {
        return { outcome: "not_dispatched", writability };
      }
      return setSchedulerPaused(
        scopedUrl(
          scope,
          `/api/automation/scheduler/${paused ? "pause" : "resume"}`,
        ),
      );
    },
    onSuccess: (result, _paused, dispatch) => {
      // The dispatch's own key, never the key of whatever scope is on screen
      // by the time the daemon answers. Read without a fallback on purpose:
      // `?? statusKey` reinstated exactly the race this context exists to
      // close, because the closed-over key belongs to the render that settled
      // rather than to the render that dispatched. `onMutate` establishes this
      // before `mutationFn` runs, so a settled success always has one.
      const target = dispatch.statusKey;
      // Only a genuine reading may replace the cached one. A transport failure
      // or an unparseable body is reported by the caller from this same result
      // and must leave the last real reading in place.
      if (result.outcome === "ok") {
        client.setQueryData(target, result);
        return;
      }
      // A write that never went out cannot have changed the server's reading,
      // so there is nothing to re-read.
      if (result.outcome === "not_dispatched") return;
      void client.invalidateQueries({ queryKey: target });
    },
  });
  return { ...mutation, writability };
}

/* ---- the three list routes ---------------------------------------------- */

/**
 * The list bodies, as the handlers that serve them actually emit them.
 *
 * Every field below is required because the route makes it unconditional:
 * `automation_jobs_api::list` answers `{jobs, count}`, `automation_skills_api::
 * list` answers `{…, count, skills, …}`, and the automatic-fact-receipts
 * list answers `{receipts, count, limit, error}` — each built by a `json!`
 * literal with no conditional key.
 *
 * That requiredness is load-bearing rather than pedantic. These schemas used to
 * make the collection optional (`skills?`, plus an `items?` alternative that no
 * handler has ever sent), and an optional array resolved through `?? []` into a
 * rendered "no managed skills". A store the daemon could not read, a renamed
 * field, a proxy's substituted body — all of them parsed clean and printed as a
 * queue that had been checked and found empty. Required fields route those
 * bodies to `unsupported_schema` in `fetchPayload` instead, which is what
 * `PayloadBoundary` renders as a state rather than as content.
 *
 * They live here, beside the fetchers, rather than on the page that draws them:
 * a wire contract is what the daemon sends, and a surface that owned its own
 * copy of one would be the second authority on a shape it does not serve.
 */
const JobsPayloadSchema = z
  .object({
    jobs: z.array(
      z
        .object({
          id: z.string(),
          name: z.string(),
          schedule: z.string().nullable().optional(),
          enabled: z.boolean(),
          interval_secs: z.number().nullable().optional(),
        })
        .passthrough(),
    ),
    count: z.number(),
  })
  .passthrough();

/** `ManagedSkill` (managed_skill_model.rs): `metadata.id`, `.title` and
 * `.state` are plain required fields on the struct, so they are read directly
 * rather than through the chain of `?? skill['name'] ?? index` fallbacks this
 * card used to carry — every one of which described a payload no route sends,
 * and the last of which printed an array index as if it were a skill. */
const ManagedSkillStateSchema = z.enum(["active", "disabled", "archived"]);

const SkillsPayloadSchema = z
  .object({
    skills: z.array(
      z
        .object({
          metadata: z
            .object({
              id: z.string(),
              title: z.string(),
              state: ManagedSkillStateSchema,
            })
            .passthrough(),
        })
        .passthrough(),
    ),
    count: z.number(),
  })
  .passthrough();

/** Automatic fact receipts are terminal daemon-owned outcomes. A receipt may
 * retain proposal and validation evidence, but this dashboard never sends an
 * approval or apply request. */
const AutomaticFactStateSchema = z.enum(["applied", "quarantined"]);
const AutomaticFactReceiptSchema = z
  .object({
    schema_version: z.number(),
    apply_id: z.string(),
    run_id: z.string(),
    evidence_hash: z.string().optional(),
    state: AutomaticFactStateSchema,
    add_fact_request: z.object({ content: z.string() }).passthrough(),
    item: z.unknown().optional(),
    validation: z.unknown().optional(),
    quarantine_reason: z.string().optional(),
    applied_fact_id: z.string().optional(),
    recorded_at_micros: z.number().int(),
  })
  .passthrough();

const AutomaticFactReceiptsPayloadSchema = z
  .object({
    receipts: z.array(AutomaticFactReceiptSchema),
    count: z.number(),
    limit: z.number(),
    error: z.string(),
  })
  .passthrough();

export type JobRow = z.infer<typeof JobsPayloadSchema>["jobs"][number];
export type SkillRow = z.infer<typeof SkillsPayloadSchema>["skills"][number];
export type ManagedSkillState = z.infer<typeof ManagedSkillStateSchema>;
export type AutomaticFactReceipt = z.infer<typeof AutomaticFactReceiptSchema>;
export type AutomaticFactReceiptsPayload = z.infer<
  typeof AutomaticFactReceiptsPayloadSchema
>;

export function useAutomationJobs() {
  return usePayload(
    ["automation", "jobs"],
    "/api/automation/jobs",
    JobsPayloadSchema,
  );
}

export function useAutomationSkills() {
  return usePayload(
    ["automation", "skills"],
    "/api/automation/skills",
    SkillsPayloadSchema,
  );
}

/** The terminal automatic fact receipt list. */
export function useAutomationFactReceipts() {
  return usePayload(
    ["automation", "automatic-fact-receipts"],
    "/api/automation/automatic-fact-receipts",
    AutomaticFactReceiptsPayloadSchema,
  );
}

/** `automation_run_api::run_list` (`/api/automation/runs`): the newest ledger
 * records, projected by `run_history_row`. Every payload key below is
 * unconditional; `model` and `error` are nullable because the writer emits
 * null when absent. */
const RunsPayloadSchema = z
  .object({
    runs: z.array(
      z
        .object({
          run_id: z.string(),
          task: z.string(),
          trigger: z.string(),
          backend: z.string(),
          model: z.string().nullable(),
          status: z.string(),
          reviewed_count: z.number(),
          accepted_count: z.number(),
          rejected_count: z.number(),
          skipped_count: z.number(),
          error: z.string().nullable(),
          started_at: z.string(),
          completed_at: z.string(),
          artifact_kinds: z.array(z.string()),
        })
        .strict(),
    ),
    count: z.number(),
    limit: z.number(),
    has_more: z.boolean(),
    malformed_row_count: z.number().int().nonnegative(),
    completeness: z.enum(["known", "partial"]),
    error: z.string(),
  })
  .strict();

/** `automation_run_api::artifact_list` (`/api/automation/runs/{id}/artifacts`):
 * the run's recorded artifacts plus the handler's own chain summary, which
 * carries the integrity verdict — verified, mismatched, unavailable, or failed
 * — computed server-side against the published chain. */
const RunArtifactsPayloadSchema = z
  .object({
    run_id: z.string(),
    artifacts: z.array(
      z
        .object({
          kind: z.string(),
          path: z.string(),
          sha256: z.string(),
          summary: z.string().optional(),
          created_at: z.string(),
        })
        .passthrough(),
    ),
    artifact_chain: z
      .object({
        expected_kinds: z.array(z.string()),
        present_kinds: z.array(z.string()),
        metadata_complete: z.boolean(),
        complete: z.boolean(),
        integrity_status: z.string(),
      })
      .passthrough(),
    count: z.number(),
    error: z.string(),
  })
  .passthrough();

/** The existing read-only payload route for one recorded artifact. The
 * artifact kind owns its payload shape, so the dashboard retains that value as
 * unknown and displays the daemon's JSON rather than inventing a parallel DTO. */
const RunArtifactPayloadSchema = z
  .object({
    run_id: z.string(),
    artifact: z
      .object({
        kind: z.string(),
        path: z.string(),
        sha256: z.string(),
        summary: z.string().optional(),
        created_at: z.string(),
      })
      .passthrough(),
    payload: z.unknown(),
    error: z.literal(""),
  })
  .passthrough();

export type RunRow = z.infer<typeof RunsPayloadSchema>["runs"][number];
export type RunsPayload = z.infer<typeof RunsPayloadSchema>;
export type RunArtifactsPayload = z.infer<typeof RunArtifactsPayloadSchema>;
export type RunArtifactRow = RunArtifactsPayload["artifacts"][number];
export type RunArtifactPayload = z.infer<typeof RunArtifactPayloadSchema>;

const SkillOutcomeVerdictSchema = z.enum(["adopted", "ignored", "too_early"]);

const SkillOutcomeSchema = z
  .object({
    skill_id: z.string(),
    title: z.string().nullable().optional(),
    activated_at: z.number(),
    days_since_activation: z.number(),
    views_since_activation: z.number(),
    uses_since_activation: z.number(),
    verdict: SkillOutcomeVerdictSchema,
  })
  .passthrough();

const FactOutcomeIdentityFields = {
  apply_id: z.string(),
  run_id: z.string().optional(),
  recorded_at: z.number().int(),
  days_since_recorded: z.number().int(),
} as const;

const AvailableFactTelemetryFields = {
  retrieval_count: z.number().int().nonnegative(),
  access_count: z.number().int().nonnegative(),
  helpful_count: z.number().int().nonnegative(),
  unhelpful_count: z.number().int().nonnegative(),
  last_recalled_at: z.number().int().optional(),
} as const;

const AbsentFactTelemetryFields = {
  retrieval_count: z.never().optional(),
  access_count: z.never().optional(),
  helpful_count: z.never().optional(),
  unhelpful_count: z.never().optional(),
  last_recalled_at: z.never().optional(),
} as const;

const AvailableFactOutcomeSchema = z
  .object({
    ...FactOutcomeIdentityFields,
    state: z.literal("applied"),
    canonical_fact_id: z.string(),
    ...AvailableFactTelemetryFields,
    still_exists: z.literal(true),
    verdict: z.enum([
      "recalled_and_helpful",
      "recalled",
      "never_recalled",
    ]),
  })
  .passthrough()
  .superRefine((record, context) => {
    const recalled =
      record.access_count > 0 || record.last_recalled_at !== undefined;
    const expectedVerdict =
      recalled && record.helpful_count > 0
        ? "recalled_and_helpful"
        : recalled
          ? "recalled"
          : "never_recalled";
    if (record.verdict !== expectedVerdict) {
      context.addIssue({
        code: "custom",
        path: ["verdict"],
        message: "fact outcome verdict contradicts its recall telemetry",
      });
    }
  });

const FactOutcomeSchema = z.union([
  AvailableFactOutcomeSchema,
  z
    .object({
      ...FactOutcomeIdentityFields,
      state: z.literal("applied"),
      canonical_fact_id: z.string(),
      ...AbsentFactTelemetryFields,
      still_exists: z.literal(false),
      verdict: z.enum(["deleted", "quarantined", "unavailable"]),
    })
    .passthrough(),
  z
    .object({
      ...FactOutcomeIdentityFields,
      state: z.literal("quarantined"),
      canonical_fact_id: z.never().optional(),
      ...AbsentFactTelemetryFields,
      still_exists: z.literal(false),
      verdict: z.literal("quarantined"),
    })
    .passthrough(),
]);

/** Read-only adoption and recall outcomes produced by the daemon. */
export const AutomationOutcomesPayloadSchema = z
  .object({
    generated_at: z.number(),
    skills: z.array(SkillOutcomeSchema),
    facts: z.array(FactOutcomeSchema),
    snapshot: z
      .object({
        available: z.boolean(),
        skills_refreshed_at: z.number().nullable(),
        facts_refreshed_at: z.number().nullable(),
      })
      .passthrough(),
    error: z.string(),
  })
  .passthrough();
export type AutomationOutcomesPayload = z.infer<
  typeof AutomationOutcomesPayloadSchema
>;
export type SkillOutcome = AutomationOutcomesPayload["skills"][number];
export type FactOutcome = AutomationOutcomesPayload["facts"][number];

const FACT_STORE_CURATE_HTTP_BINDING_ID = "binding.http.fact_store_curate.v1";
const FACT_STORE_CURATE_RESULT_SCHEMA_ID =
  "schema.application.retained.fact-store-curate.result";
const FACT_STORE_CURATE_RESULT_SCHEMA_REVISION = 1;
const CanonicalApplicationIdentifierSchema = z.string().refine(
  canonicalApplicationIdentifier,
);
const CuratorResolvedScopeSchema = ResolvedScopeSchema.extend({
  project_id: CanonicalApplicationIdentifierSchema,
  repository_id: CanonicalApplicationIdentifierSchema,
  worktree_id: CanonicalApplicationIdentifierSchema,
  reference: CanonicalApplicationIdentifierSchema.nullable(),
  scope_digest: z.string().regex(/^sha256:[0-9a-f]{64}$/),
});

const AutomaticCuratorResponseSchema = z
  .object({
    kind: z.literal("success"),
    value: z.object({
      binding_id: z.literal(FACT_STORE_CURATE_HTTP_BINDING_ID),
      contract: z
        .object({
          schema_id: z.literal(FACT_STORE_CURATE_RESULT_SCHEMA_ID),
          schema_revision: z.literal(FACT_STORE_CURATE_RESULT_SCHEMA_REVISION),
        })
        .strict(),
      request_id: CanonicalApplicationIdentifierSchema,
      scope: CuratorResolvedScopeSchema,
      outcome: z.object({
        outcome: z.literal("effect"),
        value: z.object({ payload: AutomationRunResultV1Schema }),
      }),
    }),
  })
  .strict();

const ApplicationProblemResponseSchema = z
  .object({
    kind: z.literal("problem"),
    value: ApplicationProblemEnvelopeSchema.extend({
      binding_id: z.literal(FACT_STORE_CURATE_HTTP_BINDING_ID),
    }),
  })
  .strict();

const CuratorApplicationProblemClaimSchema = z.object({
  kind: z.literal("problem"),
  value: z.object({
    problem: z.object({
      kind: z.enum(["conflict", "partial_effect", "reset_required"]),
    }),
  }),
});

export type AutomaticCuratorRun = AutomationRunResultV1;
export type AutomaticCuratorPartialEffect = ApplicationProblemEnvelope;
export type AutomaticCuratorResetRequired = ApplicationProblemEnvelope;

export type AutomaticCuratorResult =
  | { outcome: "ok"; run: AutomaticCuratorRun }
  | { outcome: "partial_effect"; problem: AutomaticCuratorPartialEffect }
  | { outcome: "reset_required"; problem: AutomaticCuratorResetRequired }
  | { outcome: "not_dispatched"; writability: ScopeWritability }
  | {
      outcome:
        | "offline"
        | "unauthorized"
        | "denied"
        | "read_only_scope"
        | "conflicting"
        | "cancelled"
        | "timed_out"
        | "unavailable"
        | "error"
        | "unsupported_schema";
      detail: string;
    };

export async function runAutomaticCurator(
  url = "/api/application/retained/fact_store_curate",
): Promise<AutomaticCuratorResult> {
  let response: Response;
  try {
    response = await fetch(url, {
      method: "POST",
      headers: { accept: "application/json", "content-type": "application/json" },
      body: JSON.stringify({
        fact_review_limit: 24,
        min_confidence_millionths: 720_000,
      }),
    });
  } catch {
    return { outcome: "offline", detail: "the daemon could not be reached" };
  }

  let body: unknown;
  try {
    body = await response.json();
  } catch {
    return {
      outcome: "unsupported_schema",
      detail: "the daemon returned a body that is not JSON",
    };
  }

  if (response.ok) {
    const result = AutomaticCuratorResponseSchema.safeParse(body);
    if (result.success) {
      const run = result.data.value.outcome.value.payload;
      if (
        (await automaticCuratorScopeMatchesEndpoint(result.data.value.scope)) &&
        (await automaticCuratorRunMatchesEndpoint(run))
      ) {
        return { outcome: "ok", run };
      }
    }
    return {
      outcome: "unsupported_schema",
      detail: "the automatic curator result does not match this build",
    };
  }

  const problemResponse = ApplicationProblemResponseSchema.safeParse(body);
  if (problemResponse.success) {
    const problem = problemResponse.data.value;
    if (
      response.status === 409 &&
      await automaticCuratorProblemMatchesEndpoint(problem, "partial_effect")
    ) {
      return { outcome: "partial_effect", problem };
    }
    if (
      response.status === 503 &&
      await automaticCuratorProblemMatchesEndpoint(problem, "reset_required")
    ) {
      return { outcome: "reset_required", problem };
    }
    if (
      response.status === 409 &&
      await automaticCuratorProblemMatchesEndpoint(problem, "conflict")
    ) {
      return { outcome: "conflicting", detail: "the automation request conflicted" };
    }
    if (
      problem.problem.kind === "conflict" ||
      problem.problem.kind === "partial_effect" ||
      problem.problem.kind === "reset_required"
    ) {
      return {
        outcome: "unsupported_schema",
        detail: "the application problem does not match fact_store_curate",
      };
    }
  }
  if (CuratorApplicationProblemClaimSchema.safeParse(body).success) {
    return {
      outcome: "unsupported_schema",
      detail: "the application problem does not match fact_store_curate",
    };
  }

  switch (response.status) {
    case 401:
      return { outcome: "unauthorized", detail: "automation authorization is required" };
    case 403:
      return { outcome: "denied", detail: "the automation authority denied this run" };
    case 405:
      return { outcome: "read_only_scope", detail: "this project scope is read-only" };
    case 409:
      return {
        outcome: "unsupported_schema",
        detail: "the daemon returned HTTP 409 without a matching application problem",
      };
    case 408:
      return { outcome: "cancelled", detail: "the automatic run was cancelled" };
    case 429:
    case 503:
      return { outcome: "unavailable", detail: "the automation authority is unavailable" };
    case 504:
      return { outcome: "timed_out", detail: "the automatic run timed out" };
    default:
      return { outcome: "error", detail: `the automatic run failed with HTTP ${response.status}` };
  }
}

async function automaticCuratorRunMatchesEndpoint(
  run: AutomationRunResultV1,
): Promise<boolean> {
  if (
    run.task !== "memory_curator" ||
    run.request_digest !== await canonicalSha256([
      "tracedecay.automation-run.request-identity.v1",
      {
        kind: "memory_curator",
        options: {
          fact_review_limit: 24,
          min_confidence_millionths: 720_000,
        },
      },
    ])
  ) return false;
  const summary = run.terminal.summary;
  if (run.terminal.status === "skipped") {
    return (
      summary.reviewed_count === 0 &&
      summary.accepted_count === 0 &&
      summary.rejected_count === 0 &&
      summary.skipped_count === 1 &&
      memoryCuratorSkipReason(run.terminal.reason) &&
      run.committed_receipts.length === 0
    );
  }
  if (
    summary.skipped_count !== 0 ||
    summary.reviewed_count !== summary.accepted_count + summary.rejected_count ||
    summary.rejected_count !== 0 ||
    run.committed_receipts.length > 1
  ) {
    return false;
  }
  const receiptsMatch = (await Promise.all(run.committed_receipts.map(
    (receipt) => receipt.kind === "curation" &&
      automaticCurationReceiptMatches(run.run_id, receipt.receipt),
  ))).every(Boolean);
  return receiptsMatch && summary.accepted_count === run.committed_receipts.reduce(
    (count, receipt) =>
      count +
      (receipt.kind === "curation"
        ? receipt.receipt.receipt.accepted_operations
        : 0),
    0,
  );
}

async function automaticCuratorScopeMatchesEndpoint(
  scope: ResolvedScope,
): Promise<boolean> {
  const parsed = CuratorResolvedScopeSchema.safeParse(scope);
  if (!parsed.success) return false;
  const canonical = parsed.data;
  return canonical.scope_digest === await canonicalSha256([
    "tracedecay.application.scope.v1",
    canonical.project_id,
    canonical.repository_id,
    canonical.worktree_id,
    canonical.reference,
  ]);
}

function memoryCuratorSkipReason(reason: string): boolean {
  return [
    "automation_disabled",
    "backend_disabled",
    "delegated_host_mode",
    "memory_curator_disabled",
    "nothing_to_review",
    "partial_coverage_no_candidates",
    "scheduler_cooldown_active",
    "scheduler_cron_not_due",
    "scheduler_idle_window_active",
    "scheduler_interval_not_elapsed",
    "scheduler_lock_active",
    "scheduler_non_retryable_failure",
    "scheduler_schedule_invalid",
    "scheduler_schedule_manual",
    "similarity_authority_unavailable",
    "task_not_schedulable",
  ].includes(reason);
}

const FACT_STORE_CURATE_USE_CASE_ID =
  "use-case.application.retained.fact-store-curate";

async function automaticCuratorProblemMatchesEndpoint(
  terminal: ApplicationProblemEnvelope,
  kind: "conflict" | "partial_effect" | "reset_required",
): Promise<boolean> {
  if (
    terminal.contract.schema_id !== FACT_STORE_CURATE_RESULT_SCHEMA_ID ||
    terminal.contract.schema_revision !== FACT_STORE_CURATE_RESULT_SCHEMA_REVISION ||
    !canonicalApplicationIdentifier(terminal.request_id) ||
    terminal.request_id !== terminal.problem.request_id ||
    terminal.problem.kind !== kind
  ) {
    return false;
  }
  const receipt = terminal.problem.committed_receipt;
  if (kind === "conflict" || kind === "reset_required") return receipt === null;
  return (
    receipt !== null &&
    receipt.operation === FACT_STORE_CURATE_USE_CASE_ID &&
    receipt.request_id === terminal.request_id &&
    receipt.outcome === "partial" &&
    receipt.committed_state !== null &&
    (await automaticCuratorScopeMatchesEndpoint(receipt.scope))
  );
}

type CurationReceipt = Extract<
  AutomationRunResultV1["committed_receipts"][number],
  { kind: "curation" }
>["receipt"];

async function automaticCurationReceiptMatches(
  runId: string,
  settled: CurationReceipt,
): Promise<boolean> {
  const receipt = settled.receipt;
  if (
    receipt.automation_run_id !== runId ||
    !/^[0-9a-f]{64}$/.test(receipt.input_digest) ||
    settled.canonical_digest !== await canonicalSha256([
      "tracedecay.automation-run.curation-receipt.v1",
      receipt,
    ]) ||
    receipt.operation_effects.length === 0 ||
    receipt.operation_effects.length > 256 ||
    receipt.accepted_operations !== receipt.operation_effects.length ||
    receipt.changed_fact_ids.length > 256
  ) {
    return false;
  }
  const changedFactIds: string[] = [];
  const committedEventIds = new Set<string>();
  const ownerDigest = await canonicalSha256([
    "fact-owner.v1",
    receipt.owner,
  ]);
  if (ownerDigest === null) return false;
  const ownerBinding = ownerDigest.slice("sha256:".length);
  let factsAdded = 0;
  let factsUpdated = 0;
  let factsMerged = 0;
  let factsRemoved = 0;
  let normalizedTags = 0;
  let factsLinked = 0;
  let disposition: string | undefined;
  let firstCommit: CurationCommit | undefined;
  const operationIdentities = new Set<string>();
  const appendChanged = (factId: string) => {
    if (!changedFactIds.includes(factId)) changedFactIds.push(factId);
  };
  const acceptCommit = (
    commit: CurationCommit,
    factId: string,
    eventCount: number | undefined,
    assertion: "any" | "present" | "absent",
  ): boolean => {
    if (
      canonicalJson(commit.owner) !== canonicalJson(receipt.owner) ||
      commit.fact_id !== factId ||
      commit.committed_event_ids.length === 0 ||
      (eventCount !== undefined && commit.committed_event_ids.length !== eventCount) ||
      commit.committed_event_ids.at(-1) !== commit.last_event_id ||
      (assertion === "present" && commit.active_assertion_id === null) ||
      (assertion === "absent" && commit.active_assertion_id !== null) ||
      (disposition !== undefined && commit.disposition !== disposition) ||
      commit.committed_event_ids.some((eventId) => {
        if (committedEventIds.has(eventId)) return true;
        committedEventIds.add(eventId);
        return false;
      })
    ) {
      return false;
    }
    disposition = commit.disposition;
    firstCommit ??= commit;
    return true;
  };
  for (const effect of receipt.operation_effects) {
    switch (effect.kind) {
      case "add": {
        const comparisonMatches = effect.closest_fact_id !== null &&
          effect.closest_fact_id !== effect.fact_id &&
          effect.similarity_millionths !== null &&
          effect.similarity_millionths <= 1_000_000;
        const snapshotMatches = effect.disposition === "added"
          ? effect.commit !== null && effect.closest_fact_id === null &&
            effect.similarity_millionths === null
          : effect.disposition === "near_duplicate"
          ? (effect.commit === null && effect.closest_fact_id === effect.fact_id &&
              effect.similarity_millionths === 1_000_000) ||
            (effect.commit !== null && comparisonMatches)
          : effect.commit !== null && comparisonMatches;
        if (
          !snapshotMatches ||
          !factIdMatchesOwner(effect.fact_id, ownerBinding) ||
          (effect.closest_fact_id !== null &&
            !factIdMatchesOwner(effect.closest_fact_id, ownerBinding)) ||
          (effect.commit !== null &&
            !acceptCommit(effect.commit, effect.fact_id, undefined, "present"))
        ) return false;
        if (effect.commit !== null) {
          factsAdded += 1;
          appendChanged(effect.fact_id);
        }
        break;
      }
      case "update":
        if (
          !factIdMatchesOwner(effect.fact_id, ownerBinding) ||
          effect.trust_delta_millionths < -1_000_000 ||
          effect.trust_delta_millionths > 1_000_000 ||
          !acceptCommit(effect.commit, effect.fact_id, undefined, "present")
        ) return false;
        factsUpdated += 1;
        appendChanged(effect.fact_id);
        break;
      case "merge": {
        const outcome = effect.outcome;
        const expectedCommits = outcome.deleted_loser_fact_ids.length +
          (outcome.content_updated ? 1 : 0);
        if (
          !/^[0-9a-f]{64}$/.test(outcome.input_digest) ||
          !factIdMatchesOwner(outcome.winner_fact_id, ownerBinding) ||
          outcome.deleted_loser_fact_ids.length === 0 ||
          outcome.deleted_loser_fact_ids.length > 256 ||
          outcome.commit_receipts.length !== expectedCommits ||
          new Set(outcome.deleted_loser_fact_ids).size !==
            outcome.deleted_loser_fact_ids.length ||
          outcome.deleted_loser_fact_ids.some((factId) =>
            factId === outcome.winner_fact_id ||
            !factIdMatchesOwner(factId, ownerBinding))
        ) return false;
        let commitIndex = 0;
        if (outcome.content_updated) {
          if (!acceptCommit(
            outcome.commit_receipts[0]!,
            outcome.winner_fact_id,
            2,
            "present",
          )) return false;
          appendChanged(outcome.winner_fact_id);
          commitIndex = 1;
        }
        for (const [index, loser] of outcome.deleted_loser_fact_ids.entries()) {
          if (!acceptCommit(
            outcome.commit_receipts[commitIndex + index]!,
            loser,
            2,
            "absent",
          )) return false;
          appendChanged(loser);
        }
        factsMerged += outcome.deleted_loser_fact_ids.length;
        break;
      }
      case "remove":
        if (
          !factIdMatchesOwner(effect.target_fact_id, ownerBinding) ||
          (effect.disposition === "removed") !== (effect.commit !== null) ||
          (effect.disposition !== "removed" && effect.commit !== null) ||
          (effect.commit !== null &&
            !acceptCommit(effect.commit, effect.target_fact_id, 1, "absent"))
        ) return false;
        if (effect.commit !== null) {
          factsRemoved += 1;
          appendChanged(effect.target_fact_id);
        }
        break;
      case "normalize_tags": {
        const identity = `normalize_tags:${effect.fact_id}`;
        if (
          operationIdentities.has(identity) ||
          !factIdMatchesOwner(effect.fact_id, ownerBinding) ||
          !acceptCommit(effect.commit, effect.fact_id, 2, "present")
        ) return false;
        operationIdentities.add(identity);
        normalizedTags += 1;
        appendChanged(effect.fact_id);
        break;
      }
      case "link_facts": {
        const identity = `link_facts:${effect.source_fact_id}:${effect.target_fact_id}:${effect.relation.kind}`;
        if (
          operationIdentities.has(identity) ||
          !factIdMatchesOwner(effect.source_fact_id, ownerBinding) ||
          !factIdMatchesOwner(effect.target_fact_id, ownerBinding) ||
          !automaticCurationRelationMatches(effect, ownerBinding) ||
          (effect.disposition === "linked" &&
            (effect.commit === null ||
              !acceptCommit(effect.commit, effect.source_fact_id, 1, "any"))) ||
          (effect.disposition === "already_linked" && effect.commit !== null)
        ) return false;
        operationIdentities.add(identity);
        if (effect.commit !== null) {
          factsLinked += 1;
          appendChanged(effect.source_fact_id);
          appendChanged(effect.target_fact_id);
        }
        break;
      }
    }
  }
  return (
    receipt.replay_fact_id === (firstCommit?.fact_id ?? null) &&
    receipt.replay_event_id === (firstCommit?.last_event_id ?? null) &&
    receipt.facts_added === factsAdded &&
    receipt.facts_updated === factsUpdated &&
    receipt.facts_merged === factsMerged &&
    receipt.facts_removed === factsRemoved &&
    receipt.normalized_tags === normalizedTags &&
    receipt.facts_linked === factsLinked &&
    receipt.changed_fact_ids.length === changedFactIds.length &&
    receipt.changed_fact_ids.every((factId, index) => factId === changedFactIds[index])
  );
}

type CurationEffect = CurationReceipt["receipt"]["operation_effects"][number];
type CurationCommit = Extract<CurationEffect, { kind: "normalize_tags" }>["commit"];

function automaticCurationRelationMatches(
  effect: Extract<
    Extract<
      AutomationRunResultV1["committed_receipts"][number],
      { kind: "curation" }
    >["receipt"]["receipt"]["operation_effects"][number],
    { kind: "link_facts" }
  >,
  ownerBinding: string,
): boolean {
  const relation = effect.relation;
  const sourceLabel = relation.provenance.source_label;
  const sanitization = relation.provenance.sanitization_receipt;
  return (
    effect.source_fact_id !== effect.target_fact_id &&
    relation.evidence_fact_ids.length > 0 &&
    relation.evidence_fact_ids.length <= 256 &&
    relation.evidence_fact_ids.every((factId) =>
      factIdMatchesOwner(factId, ownerBinding)) &&
    relation.evidence_fact_ids.every(
      (factId, index, facts) => index === 0 || facts[index - 1]! < factId,
    ) &&
    relation.confidence_millionths <= 1_000_000 &&
    sourceLabel.length > 0 &&
    new TextEncoder().encode(sourceLabel).length <= 4_096 &&
    sourceLabel.trim() === sourceLabel &&
    !/\p{Cc}/u.test(sourceLabel) &&
    (sanitization.disposition === "accepted" ||
      sanitization.disposition === "redacted") &&
    sanitization.payload !== null
  );
}

function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value !== null && typeof value === "object") {
    return `{${Object.entries(value)
      .sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0)
      .map(([key, entry]) => `${JSON.stringify(key)}:${canonicalJson(entry)}`)
      .join(",")}}`;
  }
  return JSON.stringify(value) ?? "null";
}

async function canonicalSha256(value: unknown): Promise<string | null> {
  try {
    const bytes = new TextEncoder().encode(canonicalJson(value));
    const digest = await crypto.subtle.digest("SHA-256", bytes);
    return `sha256:${Array.from(new Uint8Array(digest), (byte) =>
      byte.toString(16).padStart(2, "0")).join("")}`;
  } catch {
    return null;
  }
}

function canonicalApplicationIdentifier(value: string): boolean {
  return value.length > 0 &&
    value.trim() === value &&
    new TextEncoder().encode(value).length <= 512 &&
    !/\p{Cc}/u.test(value);
}

function factIdMatchesOwner(
  factId: string,
  ownerBinding: string,
): boolean {
  const match = /^fact\.v1\.([0-9a-f]{64})\.([0-9a-f]{64})$/.exec(factId);
  return match !== null && match[1] === ownerBinding;
}

export function useAutomaticCurator() {
  const scope = useScope((state) => state.scope);
  const writability = scopeWritable(scope);
  const currentScopeKey = scopeKey(scope);
  const client = useQueryClient();
  const dispatch = {
    scopeKey: currentScopeKey,
    url: scopedUrl(scope, "/api/application/retained/fact_store_curate"),
    writability,
  };
  const mutation = useMutation<
    { scopeKey: string; result: AutomaticCuratorResult },
    never,
    typeof dispatch
  >({
    mutationKey: ["automation", "memory-curator", "run", currentScopeKey],
    mutationFn: async (issued) => ({
      scopeKey: issued.scopeKey,
      result:
        issued.writability.state === "writable"
          ? await runAutomaticCurator(issued.url)
          : { outcome: "not_dispatched", writability: issued.writability },
    }),
    onSuccess: ({ result }) => {
      if (
        result.outcome !== "ok" &&
        result.outcome !== "partial_effect" &&
        result.outcome !== "reset_required"
      ) {
        return;
      }
      void client.invalidateQueries({ queryKey: ["automation", "runs"] });
      void client.invalidateQueries({ queryKey: ["automation", "outcomes"] });
      void client.invalidateQueries({
        queryKey: ["automation", "automatic-fact-receipts"],
      });
    },
  });
  return {
    ...mutation,
    isPending:
      mutation.isPending && mutation.variables?.scopeKey === currentScopeKey,
    data:
      mutation.data?.scopeKey === currentScopeKey
        ? mutation.data.result
        : undefined,
    mutate: () => mutation.mutate(dispatch),
    mutateAsync: async () => (await mutation.mutateAsync(dispatch)).result,
    writability,
  };
}

export function useAutomationRuns() {
  return usePayload(
    ["automation", "runs"],
    "/api/automation/runs",
    RunsPayloadSchema,
  );
}

export function useAutomationOutcomes() {
  return usePayload(
    ["automation", "outcomes"],
    "/api/automation/outcomes",
    AutomationOutcomesPayloadSchema,
  );
}

/** The artifact list for one run, fetched only once its disclosure opens:
 * most visits read the history without opening any run, and fifty eager
 * artifact reads per page view would be fifty ledger scans nobody looks at. */
export function useAutomationRunArtifacts(runId: string, enabled: boolean) {
  return usePayload(
    ["automation", "run-artifacts", runId],
    `/api/automation/runs/${encodeURIComponent(runId)}/artifacts`,
    RunArtifactsPayloadSchema,
    { enabled },
  );
}

/** Read one artifact only after its own disclosure opens. */
export function useAutomationRunArtifactPayload(
  runId: string,
  kind: string,
  enabled: boolean,
) {
  return usePayload(
    ["automation", "run-artifact-payload", runId, kind],
    `/api/automation/runs/${encodeURIComponent(runId)}/artifacts/${encodeURIComponent(kind)}`,
    RunArtifactPayloadSchema,
    { enabled },
  );
}

/** Rows, plus whether they are the whole collection the handler named. */
export type ListReading<Row> =
  | { complete: true; rows: readonly Row[] }
  | { complete: false; rows: readonly Row[]; reason: string };

/**
 * Checks a list body against the tally the same handler computed for it.
 *
 * Each of these routes derives `count` from the very vector it serializes as
 * the list, so a body where the two disagree did not reach this browser as the
 * handler wrote it — a truncating proxy, a partial response, a different build.
 * The rows are still shown, because they are real rows; what changes is that
 * they stop being presented as the complete collection. Rendering the array
 * alone would turn a truncated read into a confident inventory, which is the
 * same falsehood as an unread queue rendering as an empty one.
 */
export function tallied<Row>(
  rows: readonly Row[],
  count: number,
  noun: string,
): ListReading<Row> {
  if (rows.length === count) return { complete: true, rows };
  return {
    complete: false,
    rows,
    reason: `the daemon counted ${count} ${noun} and sent ${rows.length}, so this list is not the whole set`,
  };
}

/** A tallied list whose handler also names the request cap it applied. */
export function talliedCapped<Row>(
  rows: readonly Row[],
  count: number,
  limit: number,
  noun: string,
  pageDescription = `the first ${limit} ${noun}`,
): ListReading<Row> {
  const coherent = tallied(rows, count, noun);
  if (!coherent.complete) return coherent;
  if (count < limit) return coherent;
  if (count > limit) {
    return {
      complete: false,
      rows,
      reason: `the daemon sent ${count} ${noun} under a request cap of ${limit}, so this body is not this route's answer`,
    };
  }
  return {
    complete: false,
    rows,
    reason: `this is ${pageDescription}, the request cap, so there may be more`,
  };
}

/** The ledger reader reports truncation and skipped malformed rows directly. */
export function automationRunsReading(
  data: Pick<
    RunsPayload,
    "runs" | "count" | "has_more" | "malformed_row_count" | "completeness"
  >,
): ListReading<RunRow> {
  const coherent = tallied(data.runs, data.count, "runs");
  if (!coherent.complete) return coherent;
  const omissions: string[] = [];
  if (data.has_more) omissions.push("older ledger records were outside this page");
  if (data.malformed_row_count > 0) {
    omissions.push(
      `${data.malformed_row_count} malformed ledger ${data.malformed_row_count === 1 ? "row was" : "rows were"} omitted`,
    );
  }
  if (data.completeness === "known" && omissions.length === 0) return coherent;
  if (omissions.length === 0) {
    omissions.push("the daemon marked ledger coverage partial");
  }
  return { complete: false, rows: data.runs, reason: omissions.join("; ") };
}

/**
 * The same check for the automatic receipt list, which additionally has a cap.
 *
 * `automation_automatic_fact_receipts_api::list` runs its query under
 * `coerce_limit(params.limit, 50, 200)`, and this page sends no `limit`, so it
 * reads the default page of 50. A response holding exactly its own limit is
 * therefore a page, not a total — the same distinction the Agents workspace
 * draws around its analytics cap.
 */
export function talliedFactReceipts(
  rows: readonly AutomaticFactReceipt[],
  count: number,
  limit: number,
): ListReading<AutomaticFactReceipt> {
  return talliedCapped(rows, count, limit, "fact application outcomes");
}
