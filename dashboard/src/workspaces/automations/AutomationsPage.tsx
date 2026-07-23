import { z } from 'zod';
import { CircleCheck, CirclePause, CirclePlay } from 'lucide-react';
import { OverviewCard, OverviewGrid } from '../../ui/archetypes/OverviewGrid';
import { LegacyBoundary, StatTile } from '../../ui/LegacyStates.tsx';
import { AnyObject } from '../../data/query/legacy.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';
import { cn } from '../../ui/cn';

/** Wire-true shapes from automation_scheduler_api.rs / automation_jobs_api.rs. */
const SchedulerStatusSchema = z
  .object({
    status: z.string(),
    paused: z.boolean(),
    enabled: z.boolean().optional(),
    scheduler_tick_secs: z.number().optional(),
    pending_fact_proposals: z.number().optional(),
    pending_skills: z.number().optional(),
    last_session_activity: z.number().nullable().optional(),
  })
  .passthrough();

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
    <div className="flex h-full flex-col overflow-auto">
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
        {(data) => (
          <div className="grid grid-cols-2 gap-3 p-4 md:grid-cols-4">
            <StatTile label="state" value={data.status} />
            <StatTile
              label="tick interval"
              value={
                data.scheduler_tick_secs != null ? `${data.scheduler_tick_secs}s` : '—'
              }
            />
            <StatTile
              label="pending proposals"
              value={data.pending_fact_proposals ?? 0}
            />
            <StatTile label="pending skills" value={data.pending_skills ?? 0} />
          </div>
        )}
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
