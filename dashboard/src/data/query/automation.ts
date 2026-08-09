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
import { payloadQueryKey, usePayload } from "./usePayload.ts";
import {
  scopeKey,
  scopeWritable,
  scopedUrl,
  useScope,
  type ScopeWritability,
} from "../scope/store.ts";
import {
  AutomationSchedulerStatusV1Schema,
  type AutomationSchedulerStatusV1,
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
  // {@link payloadQueryKey}.
  const statusKey = payloadQueryKey(
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
    applied_canonical_fact_id: z.string().optional(),
    applied_fact_id: z.number().nullable().optional(),
    recorded_at: z.number(),
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

/** A deployment receipt may accompany a skill activation. The arrays are
 * deliberately open because each host materializer owns its item shape. */
const SkillDeploymentSchema = z
  .object({
    status: z.enum(["complete", "partial_failure", "unavailable"]),
    exports: z.array(z.unknown()),
    materialization_scopes: z.array(
      z
        .object({
          scope: z.string(),
          materialized: z.array(z.unknown()),
          removed: z.array(z.unknown()),
          errors: z.array(z.string()),
        })
        .passthrough(),
    ),
    errors: z.array(z.string()),
    reason: z.string().optional(),
    retry_required: z.boolean(),
  })
  .passthrough();

export type ManagedSkillDeploymentReceipt = z.infer<
  typeof SkillDeploymentSchema
>;

/** Optional automatic-policy receipts are read as open values: the daemon may
 * add a new receipt without invalidating an otherwise readable run row. */
const RunReceiptFields = {
  activation_policy: z.string().optional(),
  created_skills: z.array(z.unknown()).optional(),
  updated_skills: z.array(z.unknown()).optional(),
  applied_consolidations: z.array(z.unknown()).optional(),
  rejected_skills: z.array(z.unknown()).optional(),
  validation_repairs: z.array(z.unknown()).optional(),
  receipts: z.array(z.unknown()).optional(),
  llm_apply: z.unknown().optional(),
  deployment: SkillDeploymentSchema.optional(),
  curation_policy: z.unknown().optional(),
};

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
          ...RunReceiptFields,
        })
        .passthrough(),
    ),
    count: z.number(),
    limit: z.number(),
    error: z.string(),
  })
  .passthrough();

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

export type RunRow = z.infer<typeof RunsPayloadSchema>["runs"][number];
export type RunArtifactsPayload = z.infer<typeof RunArtifactsPayloadSchema>;
export type RunArtifactRow = RunArtifactsPayload["artifacts"][number];

const SkillOutcomeVerdictSchema = z.enum(["adopted", "ignored", "too_early"]);
const FactOutcomeVerdictSchema = z.enum([
  "recalled_and_helpful",
  "recalled",
  "never_recalled",
  "deleted",
]);

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

const FactOutcomeSchema = z
  .object({
    proposal_id: z.string(),
    run_id: z.string(),
    fact_id: z.string(),
    applied_at: z.number(),
    days_since_applied: z.number(),
    retrieval_count: z.number(),
    access_count: z.number(),
    helpful_count: z.number(),
    unhelpful_count: z.number(),
    last_recalled_at: z.number().nullable().optional(),
    still_exists: z.boolean(),
    verdict: FactOutcomeVerdictSchema,
  })
  .passthrough();

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
  const coherent = tallied(rows, count, "fact application outcomes");
  if (!coherent.complete) return coherent;
  if (count < limit) return coherent;
  if (count > limit) {
    // The query cannot return more rows than the cap it ran under, so this is
    // the same class of incoherent body as a mismatched tally and must not be
    // described as a full page — it would understate what arrived.
    return {
      complete: false,
      rows,
      reason: `the daemon sent ${count} fact application outcomes under a request cap of ${limit}, so this body is not this route's answer`,
    };
  }
  return {
    complete: false,
    rows,
    reason: `this is the first ${limit} fact application outcomes, the request cap, so there may be more`,
  };
}
