import { z } from 'zod';
import { CircleCheck, CirclePause, CirclePlay } from 'lucide-react';
import { OverviewCard, OverviewGrid } from '../../ui/archetypes/OverviewGrid';
import { LegacyBoundary, StatTile } from '../../ui/LegacyStates.tsx';
import { EvidencePattern } from '../../ui/EvidencePattern.tsx';
import { type LegacyResult } from '../../data/query/legacy.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import {
  automationSchedulerKey,
  schedulerStatusUrl,
  useSchedulerControl,
  type SchedulerControlResult,
} from '../../data/query/automation.ts';
import { scopeWriteSentence, type ScopeWritability } from '../../data/scope/store.ts';
import {
  AutomationSchedulerStatusV1Schema,
  type AutomationSchedulerStatusV1,
} from '../../contracts/generated.ts';
import { cn } from '../../ui/cn';

/**
 * The scheduler shape is the generated one.
 *
 * It used to be declared here by hand, because `automation_scheduler_api.rs`
 * built its response with `json!` and so had no Rust type for the contract
 * generator to export — the whole automation surface sat outside the generated
 * boundary. That handler is typed now, so this reads the real contract, and the
 * pending-review union comes across as a discriminated union rather than the
 * looser `state` enum with two independently-optional fields that a hand
 * transcription could only approximate.
 */
type SchedulerStatus = AutomationSchedulerStatusV1;

/** What this dashboard can actually say about one review queue. */
type ReviewQueueReading =
  | { quality: 'measured'; count: number }
  | { quality: 'unknown'; reason: string };

/**
 * Resolves one review queue from the scheduler payload.
 *
 * These two counts are the whole human-approval step of the automation
 * pipeline: they are what tells a person that agent-proposed facts and skill
 * drafts are waiting. So there is no zero fallback anywhere on this path — a
 * queue the daemon could not read reads as unknown.
 *
 * `pending_review` is the authority, not the flat `pending_*` mirrors. The
 * bundle is embedded in the binary that answers this route, so the two always
 * ship together and a payload without the discriminated union fails contract
 * parsing into `unsupported_schema` rather than reaching a fallback here.
 */
function reviewQueue(
  data: SchedulerStatus,
  queue: 'fact_proposals' | 'skills',
): ReviewQueueReading {
  const reported = data.pending_review[queue];
  switch (reported.state) {
    case 'unreadable':
      return { quality: 'unknown', reason: reported.reason };
    case 'measured':
      return { quality: 'measured', count: reported.count };
    default: {
      // Compile-time exhaustiveness, so a third reading state cannot be added
      // to the contract without this switch failing to build. It is not a
      // runtime path: the discriminated union rejects an unknown `state`
      // upstream, in `fetchLegacy`. The arm still returns `unknown` instead of
      // throwing, because the one thing it must never do is pick a number.
      const unhandled: never = reported;
      return {
        quality: 'unknown',
        reason: `the daemon reported an unrecognized reading state: ${JSON.stringify(unhandled)}`,
      };
    }
  }
}

/**
 * A review queue as this page can read it right now, including the case where
 * the scheduler read that carries the queue never produced a payload.
 *
 * The list cards below consult a queue to decide whether they may call
 * themselves empty, and they are rendered independently of the scheduler panel
 * — so "the scheduler read failed" has to arrive here as a reading, not as an
 * absent argument that a caller would have to remember to handle.
 */
function queueReading(
  result: LegacyResult<SchedulerStatus> | undefined,
  pending: boolean,
  queue: 'fact_proposals' | 'skills',
): ReviewQueueReading {
  if (pending) return { quality: 'unknown', reason: 'the scheduler read has not returned yet' };
  if (result === undefined) {
    return { quality: 'unknown', reason: 'no scheduler read has been recorded' };
  }
  switch (result.outcome) {
    case 'ok':
      return reviewQueue(result.data, queue);
    case 'offline':
      return { quality: 'unknown', reason: 'the daemon did not answer the scheduler read' };
    case 'unauthorized':
      return {
        quality: 'unknown',
        reason: 'the daemon accepted no identity for the scheduler read',
      };
    case 'denied':
      return {
        quality: 'unknown',
        reason: 'this identity is not permitted to read the scheduler',
      };
    case 'error':
      return { quality: 'unknown', reason: `the scheduler read failed (${result.detail})` };
    case 'unsupported_schema':
      return {
        quality: 'unknown',
        reason: 'the scheduler answered in a shape this dashboard cannot read',
      };
    case 'unavailable':
      return {
        quality: 'unknown',
        reason: `the scheduler reported it cannot serve this (${result.reason ?? result.status})`,
      };
    default: {
      const unhandled: never = result;
      return {
        quality: 'unknown',
        reason: `the scheduler read reported an unrecognized outcome: ${JSON.stringify(unhandled)}`,
      };
    }
  }
}

/** A review queue as a tile. An unread queue prints an em dash under the
 * `unknown` evidence pattern — never a zero, which would read as a queue that
 * was checked and found empty. */
function ReviewQueueTile({ label, reading }: { label: string; reading: ReviewQueueReading }) {
  return (
    <StatTile
      label={label}
      value={reading.quality === 'measured' ? reading.count : '—'}
      hint={<EvidencePattern quality={reading.quality} />}
    />
  );
}

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

type JobRow = z.infer<typeof JobsPayloadSchema>['jobs'][number];
type SkillRow = z.infer<typeof SkillsPayloadSchema>['skills'][number];
type ProposalRow = z.infer<typeof FactProposalsPayloadSchema>['proposals'][number];

/** Rows, plus whether they are the whole collection the handler named. */
type ListReading<Row> =
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
function tallied<Row>(rows: readonly Row[], count: number, noun: string): ListReading<Row> {
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
function talliedProposals(
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

/**
 * Whether an empty list may be reported as an empty review queue.
 *
 * The two reads count different populations and are deliberately not compared
 * as numbers: the list routes return every state under a cap, while
 * `pending_review` counts only what awaits human approval. What is comparable
 * is the containment — a pending item is one of the items the list enumerates —
 * so an empty list and a measured, non-empty queue cannot both be true, and an
 * empty list says nothing at all about a queue nobody could read.
 *
 * This is the agreement the page previously lacked. Its scheduler tile printed
 * an em dash for an unreadable proposal queue and explained that the count was
 * unknown rather than zero, while the card thirty lines below asserted "no
 * pending fact proposals" from the same screen.
 */
type EmptyClaim =
  | { verdict: 'empty' }
  | { verdict: 'unknown'; reason: string }
  | { verdict: 'contradicted'; pending: number };

function emptyClaim(queue: ReviewQueueReading): EmptyClaim {
  switch (queue.quality) {
    case 'unknown':
      return { verdict: 'unknown', reason: queue.reason };
    case 'measured':
      return queue.count === 0
        ? { verdict: 'empty' }
        : { verdict: 'contradicted', pending: queue.count };
    default: {
      const unhandled: never = queue;
      return {
        verdict: 'unknown',
        reason: `the queue reported an unrecognized reading: ${JSON.stringify(unhandled)}`,
      };
    }
  }
}

/** Automations: scheduler health, jobs, managed skills, fact proposals — all
 * real /api/automation surfaces. The actions phase begins here with the
 * scheduler's pause and resume, the two controls whose route is typed; the
 * remaining bounded controls follow as their handlers enter the generated
 * contract boundary, and until then those surfaces stay read-only. */
export function AutomationsPage() {
  const scheduler = useLegacy(
    automationSchedulerKey,
    schedulerStatusUrl,
    AutomationSchedulerStatusV1Schema,
  );
  const control = useSchedulerControl();
  const jobs = useLegacy(['automation', 'jobs'], '/api/automation/jobs', JobsPayloadSchema);
  const skills = useLegacy(
    ['automation', 'skills'],
    '/api/automation/skills',
    SkillsPayloadSchema,
  );
  const proposals = useLegacy(
    ['automation', 'fact-proposals'],
    '/api/automation/fact-proposals',
    FactProposalsPayloadSchema,
  );

  // Resolved once here rather than inside each card, so the scheduler is the
  // single authority both cards and the tiles above them read the queues from.
  const proposalQueue = queueReading(scheduler.data, scheduler.isPending, 'fact_proposals');
  const skillQueue = queueReading(scheduler.data, scheduler.isPending, 'skills');

  return (
    // Scrollable regions need keyboard operation (WCAG 2.1.1). At narrow widths
    // this column scrolls while everything inside it is read-out — counts,
    // reasons, job rows — with nothing to tab to, and it grows taller exactly
    // when a queue is unreadable and the reason paragraph appears. The column
    // takes the tab stop itself, the same remedy Explorer's filter rail uses.
    <div tabIndex={0} className="flex h-full flex-col overflow-auto">
      {/* `flex-wrap`, because the scheduler control carries a sentence rather
        * than a chip: under a read-only scope it explains which project a
        * write would reach and how to reach it, and that runs to two lines of
        * prose. Held on one row it laid the remedy out past the right edge at
        * 320 CSS px and at 400% zoom — the reader saw a disabled button and
        * the first few words of the way to enable it. */}
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-edge-subtle px-4 py-2">
        <h1 className="text-sm font-semibold tracking-tight">Automations</h1>
        {scheduler.data?.outcome === 'ok' ? (
          <>
            <SchedulerBadge
              status={scheduler.data.data.status}
              paused={scheduler.data.data.paused}
            />
            <SchedulerControl
              paused={scheduler.data.data.paused}
              pending={control.isPending}
              failure={controlFailure(control.data)}
              writability={control.writability}
              onToggle={(paused) => control.mutate(paused)}
            />
          </>
        ) : null}
      </div>
      <LegacyBoundary title="Scheduler" pending={scheduler.isPending} result={scheduler.data}>
        {(data) => {
          const proposalTile = reviewQueue(data, 'fact_proposals');
          const draftTile = reviewQueue(data, 'skills');
          const unread = (
            [
              ['fact proposals', proposalTile],
              ['skill drafts', draftTile],
            ] as const
          ).flatMap(([label, reading]) =>
            reading.quality === 'unknown' ? [{ label, reason: reading.reason }] : [],
          );
          return (
            <div className="flex flex-col gap-3 p-4">
              <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
                <StatTile label="state" value={data.status} />
                {/* No null branch: `scheduler_tick_secs` is a plain `u64` on
                  * the wire and non-nullable in the contract, so an em dash
                  * here could only ever be a fallback for a payload the schema
                  * has already rejected — the same unreachable fallback the
                  * review queues used to carry. */}
                <StatTile label="tick interval" value={`${data.scheduler_tick_secs}s`} />
                <ReviewQueueTile label="pending proposals" reading={proposalTile} />
                <ReviewQueueTile label="pending skills" reading={draftTile} />
              </div>
              {/* The tile can only carry the evidence class; the reason belongs
                * in full, unclipped, because this is the queue that gates human
                * approval and "why can nobody read it" is the actionable part. */}
              {unread.length > 0 ? (
                <p className="border border-edge-subtle bg-surface-1 px-3 py-2 text-2xs leading-relaxed text-text-secondary">
                  Awaiting-review counts are unknown, not zero.{' '}
                  {unread.map((queue) => `The ${queue.label} queue: ${queue.reason}.`).join(' ')}{' '}
                  Nothing here says whether anything is waiting for your approval.
                </p>
              ) : null}
            </div>
          );
        }}
      </LegacyBoundary>
      <OverviewGrid>
        <OverviewCard title="Jobs">
          <LegacyBoundary title="Jobs" pending={jobs.isPending} result={jobs.data}>
            {(data) => {
              const reading = tallied(data.jobs, data.count, 'jobs');
              if (reading.rows.length === 0) {
                // Jobs are a plain configured list with no review queue behind
                // them, so an empty body really is an empty list — provided the
                // handler's own tally agrees, which `tallied` has just checked.
                return reading.complete ? (
                  <p className="text-2xs text-text-muted">no automation jobs defined</p>
                ) : (
                  <PartialNotice reason={reading.reason} />
                );
              }
              return (
                <>
                  {reading.complete ? null : <PartialNotice reason={reading.reason} />}
                  <div className="flex flex-col">
                    {reading.rows.map((job) => (
                      <JobRowLine key={job.id} job={job} />
                    ))}
                  </div>
                </>
              );
            }}
          </LegacyBoundary>
        </OverviewCard>
        <OverviewCard title="Managed skills">
          <LegacyBoundary title="Skills" pending={skills.isPending} result={skills.data}>
            {(data) => {
              const reading = tallied(data.skills, data.count, 'managed skills');
              if (reading.rows.length === 0) {
                return (
                  <EmptyList
                    reading={reading}
                    claim={emptyClaim(skillQueue)}
                    empty="no managed skills"
                    queue="skill drafts"
                  />
                );
              }
              return (
                <>
                  {reading.complete ? null : <PartialNotice reason={reading.reason} />}
                  <div className="flex flex-col">
                    {reading.rows.map((skill) => (
                      <SkillRowLine key={skill.metadata.id} skill={skill} />
                    ))}
                  </div>
                </>
              );
            }}
          </LegacyBoundary>
        </OverviewCard>
        <OverviewCard title="Fact proposals">
          <LegacyBoundary title="Proposals" pending={proposals.isPending} result={proposals.data}>
            {(data) => {
              const reading = talliedProposals(data.proposals, data.count, data.limit);
              if (reading.rows.length === 0) {
                return (
                  <EmptyList
                    reading={reading}
                    claim={emptyClaim(proposalQueue)}
                    empty="no pending fact proposals"
                    queue="fact proposals"
                  />
                );
              }
              return (
                <>
                  {reading.complete ? null : <PartialNotice reason={reading.reason} />}
                  <div className="flex flex-col">
                    {reading.rows.map((proposal) => (
                      <ProposalRowLine key={proposal.proposal_id} proposal={proposal} />
                    ))}
                  </div>
                </>
              );
            }}
          </LegacyBoundary>
        </OverviewCard>
      </OverviewGrid>
    </div>
  );
}

/** A list that came back with no rows, said as much as the reads support.
 *
 * The empty sentence is only printed when two independent reads agree that
 * nothing is there: this list, and the scheduler's count of the queue the
 * sentence is about. Anything else states what is unknown instead. */
function EmptyList({
  reading,
  claim,
  empty,
  queue,
}: {
  reading: ListReading<unknown>;
  claim: EmptyClaim;
  empty: string;
  queue: string;
}) {
  // An incoherent body is the stronger finding: nothing about the queue can be
  // concluded from a list that does not match its own tally.
  if (!reading.complete) return <PartialNotice reason={reading.reason} />;
  switch (claim.verdict) {
    case 'empty':
      return <p className="text-2xs text-text-muted">{empty}</p>;
    case 'unknown':
      return (
        <p role="status" className="text-2xs leading-relaxed text-text-secondary">
          This read returned no rows, but whether the {queue} queue is empty is unknown:{' '}
          {claim.reason}.
        </p>
      );
    case 'contradicted':
      return (
        <p role="status" className="text-2xs leading-relaxed text-state-error">
          This read returned no rows while the scheduler counted {claim.pending} awaiting review,
          so the two disagree and neither is presented as the answer.
        </p>
      );
    default: {
      const unhandled: never = claim;
      return <PartialNotice reason={`unrecognized queue verdict: ${JSON.stringify(unhandled)}`} />;
    }
  }
}

/** A list that is real but is not the whole set it names. */
function PartialNotice({ reason }: { reason: string }) {
  return (
    <p role="status" className="text-2xs leading-relaxed text-text-secondary">
      Showing a partial list: {reason}.
    </p>
  );
}

function JobRowLine({ job }: { job: JobRow }) {
  return (
    <div className="flex items-center gap-2 border-b border-edge-subtle py-1.5 last:border-b-0">
      {job.enabled ? (
        <CirclePlay aria-hidden size={13} className="shrink-0 text-accent" />
      ) : (
        <CirclePause aria-hidden size={13} className="shrink-0 text-text-muted" />
      )}
      <span className="min-w-0 flex-1 truncate text-xs">{job.name}</span>
      <span className="tabular shrink-0 text-2xs text-text-muted">
        {job.schedule ?? (job.interval_secs != null ? `every ${job.interval_secs}s` : 'manual')}
      </span>
    </div>
  );
}

function SkillRowLine({ skill }: { skill: SkillRow }) {
  return (
    <div className="flex items-center gap-2 border-b border-edge-subtle py-1.5 last:border-b-0">
      <CircleCheck aria-hidden size={13} className="shrink-0 text-text-muted" />
      <span className="min-w-0 flex-1 truncate text-xs">{skill.metadata.title}</span>
      <StateLabel state={skill.metadata.state} />
    </div>
  );
}

/** One proposal row.
 *
 * The list route filters on no state by default, so these are proposals in
 * every state and the row says which — the pending subset is the scheduler's
 * count, not this list's length. A record that carries no `add_fact_request`
 * has no fact text at all; it says so rather than printing its identifier in
 * the slot where the fact belongs, which is what the old `?? id` chain did. */
function ProposalRowLine({ proposal }: { proposal: ProposalRow }) {
  const content = proposal.add_fact_request?.content;
  return (
    <div className="flex items-center gap-2 border-b border-edge-subtle py-1.5 last:border-b-0">
      {content !== undefined ? (
        <span className="min-w-0 flex-1 truncate text-xs" title={content}>
          {content}
        </span>
      ) : (
        <span className="min-w-0 flex-1 truncate text-xs text-text-muted">
          this proposal carries no fact request
        </span>
      )}
      <StateLabel state={proposal.state} />
    </div>
  );
}

/** The record's own state word, passed through rather than interpreted: the
 * page displays it and never branches on it, so a state added in Rust reads
 * correctly here without this file knowing about it. */
function StateLabel({ state }: { state: string }) {
  if (state.length === 0) return null;
  return (
    <span className="shrink-0 rounded-[var(--radius-chip)] border border-edge-subtle px-1.5 text-2xs text-text-muted">
      {state}
    </span>
  );
}

/** Why the last control attempt did not produce a reading, or null if it did.
 *
 * A control that failed must not read as a control that did nothing, so every
 * non-`ok` outcome gets words. `unsupported_schema` is called out separately
 * because it means the request very likely *did* take effect and only the reply
 * was unreadable — the opposite advice from `offline`. */
function controlFailure(result: SchedulerControlResult | undefined): string | null {
  if (result === undefined) return null;
  switch (result.outcome) {
    case 'ok':
      return null;
    case 'offline':
      return 'The daemon did not answer, so the scheduler was not changed.';
    case 'unauthorized':
      return 'The daemon accepted no identity for the change, so the scheduler was not changed.';
    case 'denied':
      return 'This identity is not permitted to control the scheduler, so it was not changed.';
    case 'error':
      return `The daemon refused the change (${result.detail}).`;
    case 'unsupported_schema':
      return 'The daemon answered in a shape this dashboard cannot read, so whether the scheduler changed is unknown — reload to re-read it.';
    // The daemon answered, in its own contract, that it cannot serve this at
    // all — so unlike `error` the change definitely did not take effect, and
    // the reason it gave is the one worth repeating.
    case 'unavailable':
      return `The scheduler was not changed: ${result.reason ?? result.status}.`;
    // The gateway refused the write because this project is not the active one.
    // Its own sentence is repeated rather than reworded, and it is stated as a
    // scope fact so the remedy is switching scope rather than retrying.
    case 'read_only_scope':
      return `The scheduler was not changed: ${result.refusal.detail}.`;
    // No request was made, so the phrasing has to be about this dashboard
    // declining rather than the daemon refusing.
    case 'not_dispatched':
      return scopeWriteSentence(result.writability, {
        writable: (target) =>
          `Nothing was sent, though writes to ${target} are accepted — reload to re-read the scheduler.`,
        refused: (reason) => `Nothing was sent. ${reason}`,
      });
    default: {
      const exhaustive: never = result;
      return exhaustive;
    }
  }
}

/**
 * Pause and resume, the first two bounded controls on this page.
 *
 * No confirmation step, and that is a deliberate reading of the product's
 * existing rule rather than an omission: the Doctor inspector gates on a
 * remediation descriptor's declared `action_confirmation`, which exists because
 * owner remediations mutate stores and can be lossy. Pausing the scheduler is
 * reversible by the adjacent button, destroys nothing, and is idempotent on the
 * server, so adding a checkbox here would be new confirmation machinery for a
 * toggle rather than the established treatment of a destructive act.
 *
 * The label always names the action against the *server's* reported state, and
 * the button is disabled while a control is in flight, so it can never be read
 * as "already paused" before the daemon has said so.
 */
function SchedulerControl({
  paused,
  pending,
  failure,
  writability,
  onToggle,
}: {
  paused: boolean;
  pending: boolean;
  failure: string | null;
  writability: ScopeWritability;
  onToggle: (paused: boolean) => void;
}) {
  // Disabled before dispatch, on the same reading the mutation refuses on. The
  // reason is on the control itself rather than only in a status line, because
  // a disabled button with the explanation elsewhere reads as a broken button.
  const blocked = writability.state !== 'writable';
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
      <button
        type="button"
        disabled={pending || blocked}
        // Described in every state: the target of an accepted write is as much
        // a thing to know before pressing as the reason for a refused one.
        aria-describedby="scheduler-control-scope"
        onClick={() => onToggle(!paused)}
        // A 17.5px chip beside the scheduler badge it matches. The chip keeps
        // that size — it is paired with `SchedulerBadge` and the two have to
        // read as one register — while the button's own box becomes the 44px
        // target. See `.td-hit`.
        className="td-hit group disabled:opacity-50"
      >
        <span
          className={cn(
            'inline-flex h-5 items-center gap-1 rounded-[var(--radius-chip)] border border-edge-subtle px-1.5 text-2xs',
            'group-hover:bg-surface-2',
          )}
        >
          {pending ? 'working…' : paused ? 'Resume scheduler' : 'Pause scheduler'}
        </span>
      </button>
      {/* Present in every state, including the writable one: a write under the
        * all-projects aggregate lands on a single project, and the reader is
        * told which rather than left to assume the change fans out. */}
      <span
        id="scheduler-control-scope"
        data-scope-writability={writability.state}
        // `min-w-0`: the sentence is the widest thing on this row and has to
        // be allowed to wrap inside it rather than push itself off the edge.
        className="min-w-0 text-2xs text-text-secondary"
      >
        {scopeWriteSentence(writability, {
          writable: (target) => `Applies to ${target}.`,
        })}
      </span>
      {failure ? (
        <span role="status" className="text-2xs text-text-secondary">
          {failure}
        </span>
      ) : null}
    </div>
  );
}

function SchedulerBadge({ status, paused }: { status: string; paused: boolean }) {
  return (
    <span
      className={cn(
        'inline-flex h-5 items-center gap-1 rounded-[var(--radius-chip)] border px-1.5 text-2xs',
        paused
          ? 'border-edge-subtle text-text-muted'
          : 'border-accent/40 bg-accent/10 text-text-primary',
      )}
    >
      {paused ? (
        <CirclePause aria-hidden size={11} />
      ) : (
        <CirclePlay aria-hidden size={11} />
      )}
      {status}
    </span>
  );
}
