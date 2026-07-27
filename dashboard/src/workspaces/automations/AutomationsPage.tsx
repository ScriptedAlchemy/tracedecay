import { z } from 'zod';
import { CircleCheck, CirclePause, CirclePlay } from 'lucide-react';
import { OverviewCard, OverviewGrid } from '../../ui/archetypes/OverviewGrid';
import { LegacyBoundary, StatTile } from '../../ui/LegacyStates.tsx';
import { EvidencePattern } from '../../ui/EvidencePattern.tsx';
import { AnyObject, type LegacyResult } from '../../data/query/legacy.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import {
  automationSchedulerKey,
  schedulerStatusUrl,
  useSchedulerControl,
} from '../../data/query/automation.ts';
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

const SkillsPayloadSchema = z
  .object({ skills: z.array(AnyObject).optional(), items: z.array(AnyObject).optional() })
  .passthrough();

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
    AnyObject,
  );

  return (
    // Scrollable regions need keyboard operation (WCAG 2.1.1). At narrow widths
    // this column scrolls while everything inside it is read-out — counts,
    // reasons, job rows — with nothing to tab to, and it grows taller exactly
    // when a queue is unreadable and the reason paragraph appears. The column
    // takes the tab stop itself, the same remedy Explorer's filter rail uses.
    <div tabIndex={0} className="flex h-full flex-col overflow-auto">
      <div className="flex items-center gap-3 border-b border-edge-subtle px-4 py-2">
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
              onToggle={(paused) => control.mutate(paused)}
            />
          </>
        ) : null}
      </div>
      <LegacyBoundary title="Scheduler" pending={scheduler.isPending} result={scheduler.data}>
        {(data) => {
          const proposals = reviewQueue(data, 'fact_proposals');
          const drafts = reviewQueue(data, 'skills');
          const unread = (
            [
              ['fact proposals', proposals],
              ['skill drafts', drafts],
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
                <ReviewQueueTile label="pending proposals" reading={proposals} />
                <ReviewQueueTile label="pending skills" reading={drafts} />
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
            {(data) =>
              data.jobs.length === 0 ? (
                <p className="text-2xs text-text-muted">no automation jobs defined</p>
              ) : (
                <div className="flex flex-col">
                  {data.jobs.map((job) => (
                    <div
                      key={job.id}
                      className="flex items-center gap-2 border-b border-edge-subtle py-1.5 last:border-b-0"
                    >
                      {job.enabled ? (
                        <CirclePlay aria-hidden size={13} className="shrink-0 text-accent" />
                      ) : (
                        <CirclePause
                          aria-hidden
                          size={13}
                          className="shrink-0 text-text-muted"
                        />
                      )}
                      <span className="min-w-0 flex-1 truncate text-xs">{job.name}</span>
                      <span className="tabular shrink-0 text-2xs text-text-muted">
                        {job.schedule ??
                          (job.interval_secs != null ? `every ${job.interval_secs}s` : 'manual')}
                      </span>
                    </div>
                  ))}
                </div>
              )
            }
          </LegacyBoundary>
        </OverviewCard>
        <OverviewCard title="Managed skills">
          <LegacyBoundary title="Skills" pending={skills.isPending} result={skills.data}>
            {(data) => {
              const rows = data.skills ?? data.items ?? [];
              if (rows.length === 0)
                return <p className="text-2xs text-text-muted">no managed skills</p>;
              return (
                <div className="flex flex-col">
                  {rows.map((skill, i) => {
                    const metadata = (skill['metadata'] ?? {}) as Record<string, unknown>;
                    const id = String(metadata['id'] ?? skill['id'] ?? skill['skill_id'] ?? i);
                    const title = String(metadata['title'] ?? skill['title'] ?? skill['name'] ?? id);
                    const state = String(metadata['state'] ?? skill['state'] ?? skill['status'] ?? '');
                    return (
                      <div
                        key={id}
                        className="flex items-center gap-2 border-b border-edge-subtle py-1.5 last:border-b-0"
                      >
                        <CircleCheck aria-hidden size={13} className="shrink-0 text-text-muted" />
                        <span className="min-w-0 flex-1 truncate text-xs">{title}</span>
                        {state ? (
                          <span className="shrink-0 rounded-[var(--radius-chip)] border border-edge-subtle px-1.5 text-2xs text-text-muted">
                            {state}
                          </span>
                        ) : null}
                      </div>
                    );
                  })}
                </div>
              );
            }}
          </LegacyBoundary>
        </OverviewCard>
        <OverviewCard title="Fact proposals">
          <LegacyBoundary title="Proposals" pending={proposals.isPending} result={proposals.data}>
            {(data) => {
              const rows = (data['proposals'] ?? data['items'] ?? []) as Array<
                Record<string, unknown>
              >;
              if (!Array.isArray(rows) || rows.length === 0)
                return <p className="text-2xs text-text-muted">no pending fact proposals</p>;
              return (
                <div className="flex flex-col">
                  {rows.map((proposal, i) => {
                    const id = String(proposal['id'] ?? proposal['proposal_id'] ?? i);
                    const request = (proposal['add_fact_request'] ?? {}) as Record<string, unknown>;
                    const content = String(
                      request['content'] ??
                        request['fact'] ??
                        proposal['content'] ??
                        proposal['fact'] ??
                        proposal['summary'] ??
                        id,
                    );
                    return (
                      <p
                        key={id}
                        className="truncate border-b border-edge-subtle py-1.5 text-xs last:border-b-0"
                        title={content}
                      >
                        {content}
                      </p>
                    );
                  })}
                </div>
              );
            }}
          </LegacyBoundary>
        </OverviewCard>
      </OverviewGrid>
    </div>
  );
}

/** Why the last control attempt did not produce a reading, or null if it did.
 *
 * A control that failed must not read as a control that did nothing, so every
 * non-`ok` outcome gets words. `unsupported_schema` is called out separately
 * because it means the request very likely *did* take effect and only the reply
 * was unreadable — the opposite advice from `offline`. */
function controlFailure(result: LegacyResult<SchedulerStatus> | undefined): string | null {
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
  onToggle,
}: {
  paused: boolean;
  pending: boolean;
  failure: string | null;
  onToggle: (paused: boolean) => void;
}) {
  return (
    <div className="flex items-center gap-2">
      <button
        type="button"
        disabled={pending}
        onClick={() => onToggle(!paused)}
        className={cn(
          'inline-flex h-5 items-center gap-1 rounded-[var(--radius-chip)] border border-edge-subtle px-1.5 text-2xs',
          'hover:bg-surface-2 disabled:opacity-50',
        )}
      >
        {pending ? 'working…' : paused ? 'Resume scheduler' : 'Pause scheduler'}
      </button>
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
