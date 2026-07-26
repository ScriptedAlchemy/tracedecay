import { z } from 'zod';
import { CircleCheck, CirclePause, CirclePlay } from 'lucide-react';
import { OverviewCard, OverviewGrid } from '../../ui/archetypes/OverviewGrid';
import { LegacyBoundary, StatTile } from '../../ui/LegacyStates.tsx';
import { EvidencePattern } from '../../ui/EvidencePattern.tsx';
import { AnyObject } from '../../data/query/legacy.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { cn } from '../../ui/cn';

/** One human-review queue as `automation_scheduler_api.rs` reports it: either a
 * measured count, or the reason the queue could not be read. */
const PendingReviewSchema = z
  .object({
    state: z.enum(['measured', 'unreadable']),
    count: z.number().nullable().optional(),
    reason: z.string().nullable().optional(),
  })
  .passthrough();

/** Wire-true shapes from automation_scheduler_api.rs / automation_jobs_api.rs. */
const SchedulerStatusSchema = z
  .object({
    status: z.string(),
    paused: z.boolean(),
    enabled: z.boolean().optional(),
    scheduler_tick_secs: z.number().optional(),
    // Nullable, not optional-with-a-zero-default: the daemon sends null for a
    // queue it could not read, and null must never round down to 0 here.
    pending_fact_proposals: z.number().nullable().optional(),
    pending_skills: z.number().nullable().optional(),
    pending_review: z
      .object({ fact_proposals: PendingReviewSchema, skills: PendingReviewSchema })
      .optional(),
    last_session_activity: z.number().nullable().optional(),
  })
  .passthrough();

type SchedulerStatus = z.infer<typeof SchedulerStatusSchema>;

/** What this dashboard can actually say about one review queue. */
type ReviewQueueReading =
  | { quality: 'measured'; count: number }
  | { quality: 'unknown'; reason: string };

/**
 * Resolves one review queue from the scheduler payload.
 *
 * These two counts are the whole human-approval step of the automation
 * pipeline: they are what tells a person that agent-proposed facts and skill
 * drafts are waiting. So there is no zero fallback anywhere on this path. A
 * daemon too old to send `pending_review` still gets read truthfully — its
 * bare number is a measured count, and a null or absent number is unknown.
 */
function reviewQueue(
  data: SchedulerStatus,
  queue: 'fact_proposals' | 'skills',
  legacyCount: number | null | undefined,
): ReviewQueueReading {
  const reported = data.pending_review?.[queue];
  if (reported?.state === 'unreadable') {
    return { quality: 'unknown', reason: reported.reason ?? 'the daemon did not say why' };
  }
  const count = reported?.state === 'measured' ? reported.count : legacyCount;
  if (typeof count !== 'number') {
    return { quality: 'unknown', reason: 'the daemon reported no reading for this queue' };
  }
  return { quality: 'measured', count };
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
 * real /api/automation surfaces. Bounded controls land with the actions
 * phase; this ships the truthful read layer. */
export function AutomationsPage() {
  const scheduler = useLegacy(
    ['automation', 'scheduler'],
    '/api/automation/scheduler/status',
    SchedulerStatusSchema,
  );
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
          <SchedulerBadge
            status={scheduler.data.data.status}
            paused={scheduler.data.data.paused}
          />
        ) : null}
      </div>
      <LegacyBoundary title="Scheduler" pending={scheduler.isPending} result={scheduler.data}>
        {(data) => {
          const proposals = reviewQueue(data, 'fact_proposals', data.pending_fact_proposals);
          const drafts = reviewQueue(data, 'skills', data.pending_skills);
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
                <StatTile
                  label="tick interval"
                  value={
                    data.scheduler_tick_secs != null ? `${data.scheduler_tick_secs}s` : '—'
                  }
                />
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
