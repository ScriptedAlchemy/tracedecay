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
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { z } from 'zod';

import { fetchLegacyWrite, type LegacyWriteResult } from './legacy.ts';
import { legacyQueryKey, useLegacy } from './useLegacy.ts';
import {
  scopeKey,
  scopeWritable,
  scopedUrl,
  useScope,
  type ScopeWritability,
} from '../scope/store.ts';
import {
  AutomationSchedulerStatusV1Schema,
  type AutomationSchedulerStatusV1,
} from '../../contracts/generated.ts';

export const automationSchedulerKey = ['automation', 'scheduler'] as const;

export const schedulerStatusUrl = '/api/automation/scheduler/status';

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
): Promise<LegacyWriteResult<AutomationSchedulerStatusV1>> {
  return fetchLegacyWrite(url, AutomationSchedulerStatusV1Schema, { method: 'POST' });
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
  | LegacyWriteResult<AutomationSchedulerStatusV1>
  | { outcome: 'not_dispatched'; writability: ScopeWritability };

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
  // {@link legacyQueryKey}.
  const statusKey = legacyQueryKey(scope, automationSchedulerKey, schedulerStatusUrl);
  // The control's own reading of the scope authority, so what disables the
  // button and what would refuse a dispatch are one value rather than two
  // that can drift.
  const writability = scopeWritable(scope);
  const mutation = useMutation<SchedulerControlResult, Error, boolean, SchedulerDispatch>({
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
      if (writability.state !== 'writable') {
        return { outcome: 'not_dispatched', writability };
      }
      return setSchedulerPaused(
        scopedUrl(scope, `/api/automation/scheduler/${paused ? 'pause' : 'resume'}`),
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
      if (result.outcome === 'ok') {
        client.setQueryData(target, result);
        return;
      }
      // A write that never went out cannot have changed the server's reading,
      // so there is nothing to re-read.
      if (result.outcome === 'not_dispatched') return;
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
 * list` answers `{…, count, skills, …}`, and `automation_fact_proposals_api::
 * list` answers `{proposals, count, limit, error}` — each built by a `json!`
 * literal with no conditional key.
 *
 * That requiredness is load-bearing rather than pedantic. These schemas used to
 * make the collection optional (`skills?`, plus an `items?` alternative that no
 * handler has ever sent), and an optional array resolved through `?? []` into a
 * rendered "no managed skills". A store the daemon could not read, a renamed
 * field, a proxy's substituted body — all of them parsed clean and printed as a
 * queue that had been checked and found empty. Required fields route those
 * bodies to `unsupported_schema` in `fetchLegacy` instead, which is what
 * `LegacyBoundary` renders as a state rather than as content.
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
const SkillsPayloadSchema = z
  .object({
    skills: z.array(
      z
        .object({
          metadata: z
            .object({ id: z.string(), title: z.string(), state: z.string() })
            .passthrough(),
        })
        .passthrough(),
    ),
    count: z.number(),
  })
  .passthrough();

/** `FactProposalRecord` (fact_proposals.rs). `add_fact_request` is the one
 * optional member — it carries `skip_serializing_if = "Option::is_none"`, so a
 * record without one omits the key and genuinely has no fact text to show. */
const FactProposalsPayloadSchema = z
  .object({
    proposals: z.array(
      z
        .object({
          proposal_id: z.string(),
          state: z.string(),
          add_fact_request: z.object({ content: z.string() }).passthrough().optional(),
        })
        .passthrough(),
    ),
    count: z.number(),
    limit: z.number(),
    error: z.string(),
  })
  .passthrough();

export type JobRow = z.infer<typeof JobsPayloadSchema>['jobs'][number];
export type SkillRow = z.infer<typeof SkillsPayloadSchema>['skills'][number];
export type ProposalRow = z.infer<typeof FactProposalsPayloadSchema>['proposals'][number];

export function useAutomationJobs() {
  return useLegacy(['automation', 'jobs'], '/api/automation/jobs', JobsPayloadSchema);
}

export function useAutomationSkills() {
  return useLegacy(['automation', 'skills'], '/api/automation/skills', SkillsPayloadSchema);
}

/**
 * Fact proposals, from the automation namespace — deliberately, not from the
 * memory one.
 *
 * The daemon mounts this family twice. `automation_fact_proposals_api` serves
 * `/api/automation/fact-proposals` and `memory_api` serves
 * `/api/plugins/holographic/fact-proposals`, and they are aliases over one
 * authority: both list through `automation::fact_proposals::
 * list_fact_proposals`, both apply through `apply_fact_proposal_with_result`
 * (refreshing the memory digest on a newly promoted fact), both reject through
 * `reject_fact_proposal`, and both build the identical
 * `{proposals, count, limit, error}` list body from the same `json!` literal.
 * There is no payload, filter, or authority difference to choose between.
 *
 * The one behavioural difference is reviewer attribution on the writes: the
 * memory namespace accepts an explicit `reviewer` in the request body, while
 * this one records `"dashboard"`. That settles the choice rather than
 * complicating it. This dashboard has no reviewer identity to supply — nothing
 * in the shell authenticates a person — so sending a reviewer would mean
 * inventing one, and a fixed, honest `"dashboard"` attribution is the true
 * record of who applied the proposal.
 *
 * So the namespace stays, and the memory-side alias stays unconsumed on
 * purpose. Two surfaces applying the same proposal through two routes would be
 * two apply paths over one ledger, and the Knowledge workspace links to this
 * queue rather than duplicating it.
 */
export function useAutomationProposals() {
  return useLegacy(
    ['automation', 'fact-proposals'],
    '/api/automation/fact-proposals',
    FactProposalsPayloadSchema,
  );
}

/** `automation_run_api::run_list` (`/api/automation/runs`): the newest ledger
 * records, projected by `run_history_row`. Every key below is unconditional in
 * that `json!` literal; `model` and `error` are `Option` fields serialized as
 * `null` when absent, so they are `nullable()` rather than `optional()` — a
 * body missing the key did not come from this handler. */
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

export type RunRow = z.infer<typeof RunsPayloadSchema>['runs'][number];
export type RunArtifactsPayload = z.infer<typeof RunArtifactsPayloadSchema>;
export type RunArtifactRow = RunArtifactsPayload['artifacts'][number];

export function useAutomationRuns() {
  return useLegacy(['automation', 'runs'], '/api/automation/runs', RunsPayloadSchema);
}

/** The artifact list for one run, fetched only once its disclosure opens:
 * most visits read the history without opening any run, and fifty eager
 * artifact reads per page view would be fifty ledger scans nobody looks at. */
export function useAutomationRunArtifacts(runId: string, enabled: boolean) {
  return useLegacy(
    ['automation', 'run-artifacts', runId],
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
 * The same check for the proposal list, which additionally has a cap.
 *
 * `automation_fact_proposals_api::list` runs its query under
 * `coerce_limit(params.limit, 50, 200)`, and this page sends no `limit`, so it
 * reads the default page of 50. A response holding exactly its own limit is
 * therefore a page, not a total — the same distinction the Agents workspace
 * draws around its analytics cap.
 */
export function talliedProposals(
  rows: readonly ProposalRow[],
  count: number,
  limit: number,
): ListReading<ProposalRow> {
  const coherent = tallied(rows, count, 'fact proposals');
  if (!coherent.complete) return coherent;
  if (count < limit) return coherent;
  if (count > limit) {
    // The query cannot return more rows than the cap it ran under, so this is
    // the same class of incoherent body as a mismatched tally and must not be
    // described as a full page — it would understate what arrived.
    return {
      complete: false,
      rows,
      reason: `the daemon sent ${count} proposals under a request cap of ${limit}, so this body is not this route's answer`,
    };
  }
  return {
    complete: false,
    rows,
    reason: `this is the first ${limit} proposals, the request cap, so there may be more`,
  };
}
