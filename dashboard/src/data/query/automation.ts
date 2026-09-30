/**
 * Reads and controls for the automation scheduler.
 *
 * `automation_scheduler_api.rs` answers `status`, `pause`, and `resume` with
 * the *same* payload, the controls re-read rather than acknowledge, and that
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

import { fetchPayloadWrite, type PayloadResult, type PayloadWriteResult } from "./payload.ts";
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
  AutomaticFactReceiptsPayloadV1Schema,
  AutomationJobsPayloadV1Schema,
  AutomationOutcomesPayloadV1Schema,
  AutomationRunArtifactPayloadV1Schema,
  AutomationRunArtifactsPayloadV1Schema,
  AutomationRunsPayloadV1Schema,
  AutomationSchedulerStatusV1Schema,
  AutomationSkillsPayloadV1Schema,
  ApplicationProblemEnvelopeSchema,
  FactStoreCurateResultV1Schema,
  ResolvedScopeSchema,
  type ApplicationProblemEnvelope,
  type AutomaticFactReceipt,
  type AutomationRunRowV1,
  type AutomationRunsPayloadV1,
  type AutomationSchedulerStatusV1,
  type FactStoreCurateResultV1,
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
 * `not_dispatched` is not a failure of the write, it is the absence of one,
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
 * callbacks from the CURRENT options, so a pause dispatched against project A
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
 * written to the entry belonging to the project that was dispatched to, see
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
  // disagreed with the read under the all-projects default, see
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
      // disable was bypassed, and dispatching anyway would trade a stated
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

/* ---- list, run, and outcome reads ---------------------------------------- */

/** One applied or quarantined fact outcome's shape is fixed by its state:
 * a surviving applied fact carries its identity and full recall telemetry and
 * a recall verdict consistent with it; a lost applied fact carries identity
 * and no telemetry; a quarantined receipt carries neither. JSON Schema cannot
 * express these joint constraints, so a body that breaks one fails the parse
 * rather than rendering a verdict its own numbers contradict. */
export const AutomationOutcomesPayloadSchema = AutomationOutcomesPayloadV1Schema.superRefine(
  (payload, context) => {
    payload.facts.forEach((fact, index) => {
      const issue = (message: string) =>
        context.addIssue({ code: "custom", path: ["facts", index], message });
      const telemetry = [
        fact.retrieval_count,
        fact.access_count,
        fact.helpful_count,
        fact.unhelpful_count,
      ];
      const hasTelemetry = telemetry.every((value) => value != null);
      const noTelemetry =
        telemetry.every((value) => value == null) && fact.last_recalled_at == null;
      if (fact.state === "quarantined") {
        if (fact.canonical_fact_id != null || !noTelemetry || fact.still_exists || fact.verdict !== "quarantined") {
          issue("a quarantined receipt carries no fact identity, telemetry, or recall verdict");
        }
        return;
      }
      if (fact.canonical_fact_id == null) {
        issue("an applied fact outcome must name its canonical fact");
        return;
      }
      if (!fact.still_exists) {
        if (!noTelemetry || !["deleted", "quarantined", "unavailable"].includes(fact.verdict)) {
          issue("a lost applied fact carries no telemetry and a loss verdict");
        }
        return;
      }
      if (!hasTelemetry) {
        issue("a surviving applied fact must carry its recall telemetry");
        return;
      }
      const recalled = (fact.access_count ?? 0) > 0 || fact.last_recalled_at != null;
      const expected =
        recalled && (fact.helpful_count ?? 0) > 0
          ? "recalled_and_helpful"
          : recalled
            ? "recalled"
            : "never_recalled";
      if (fact.verdict !== expected) {
        issue("fact outcome verdict contradicts its recall telemetry");
      }
    });
  },
);

export function useAutomationJobs() {
  return usePayload(
    ["automation", "jobs"],
    "/api/automation/jobs",
    AutomationJobsPayloadV1Schema,
  );
}

export function useAutomationSkills() {
  return usePayload(
    ["automation", "skills"],
    "/api/automation/skills",
    AutomationSkillsPayloadV1Schema,
  );
}

/** The terminal automatic fact receipt list. */
export function useAutomationFactReceipts() {
  return usePayload(
    ["automation", "automatic-fact-receipts"],
    "/api/automation/automatic-fact-receipts",
    AutomaticFactReceiptsPayloadV1Schema,
  );
}

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

export const AutomaticCuratorResponseSchema = z
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
        value: z.object({ payload: FactStoreCurateResultV1Schema }),
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

/** The admitted run; its terminal is the run-ledger row for `run_id`. */
export type AutomaticCuratorReceipt = FactStoreCurateResultV1;
export type AutomaticCuratorPartialEffect = ApplicationProblemEnvelope;
export type AutomaticCuratorResetRequired = ApplicationProblemEnvelope;

export type AutomaticCuratorResult =
  | { outcome: "started"; receipt: AutomaticCuratorReceipt }
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
      const receipt = result.data.value.outcome.value.payload;
      if (
        receipt.run_id === result.data.value.request_id &&
        (await automaticCuratorScopeMatchesEndpoint(result.data.value.scope)) &&
        (await automaticCuratorReceiptMatchesEndpoint(receipt))
      ) {
        return { outcome: "started", receipt };
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

async function automaticCuratorReceiptMatchesEndpoint(
  receipt: FactStoreCurateResultV1,
): Promise<boolean> {
  return (
    receipt.task === "memory_curator" &&
    receipt.state === "started" &&
    receipt.request_digest === await canonicalSha256([
      "tracedecay.automation-run.request-identity.v1",
      {
        kind: "memory_curator",
        options: {
          fact_review_limit: 24,
          min_confidence_millionths: 720_000,
        },
      },
    ])
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
        result.outcome !== "started" &&
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

export function useAutomationRuns(options?: {
  refetchInterval?: (latest: PayloadResult<AutomationRunsPayloadV1> | undefined) => number | false;
}) {
  return usePayload(
    ["automation", "runs"],
    "/api/automation/runs",
    AutomationRunsPayloadV1Schema,
    options,
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
    AutomationRunArtifactsPayloadV1Schema,
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
    AutomationRunArtifactPayloadV1Schema,
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
 * handler wrote it, a truncating proxy, a partial response, a different build.
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
    AutomationRunsPayloadV1,
    "runs" | "count" | "has_more" | "malformed_row_count" | "completeness"
  >,
): ListReading<AutomationRunRowV1> {
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
 * therefore a page, not a total, the same distinction the Agents workspace
 * draws around its analytics cap.
 */
export function talliedFactReceipts(
  rows: readonly AutomaticFactReceipt[],
  count: number,
  limit: number,
): ListReading<AutomaticFactReceipt> {
  return talliedCapped(rows, count, limit, "fact application outcomes");
}
