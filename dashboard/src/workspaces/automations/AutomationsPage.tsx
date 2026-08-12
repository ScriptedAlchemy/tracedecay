import { CircleCheck, CirclePause, CirclePlay } from "lucide-react";

import {
  automationSchedulerKey,
  schedulerStatusUrl,
  tallied,
  talliedFactReceipts,
  useAutomationJobs,
  useAutomationFactReceipts,
  useAutomationSkills,
  useSchedulerControl,
  type JobRow,
  type ListReading,
  type AutomaticFactReceipt,
  type SchedulerControlResult,
  type SkillRow,
} from "../../data/query/automation.ts";
import { usePayload } from "../../data/query/usePayload.ts";
import {
  scopeWriteSentence,
  type ScopeWritability,
} from "../../data/scope/store.ts";
import {
  AutomationSchedulerStatusV1Schema,
  type AutomationSchedulerStatusV1,
} from "../../contracts/generated.ts";
import { OverviewCard, OverviewGrid } from "../../ui/archetypes/OverviewGrid";
import { cn } from "../../ui/cn";
import { PayloadBoundary } from "../../ui/ReadSection.tsx";
import { StatTile } from "../../ui/LegacyStates.tsx";
import { RunHistory } from "./RunHistory.tsx";

type SchedulerStatus = AutomationSchedulerStatusV1;

/** Automation is daemon-owned. This page reports scheduler receipts and
 * application outcomes; it never asks a browser operator to approve a draft. */
export function AutomationsPage() {
  const scheduler = usePayload(
    automationSchedulerKey,
    schedulerStatusUrl,
    AutomationSchedulerStatusV1Schema,
  );
  const control = useSchedulerControl();
  const jobs = useAutomationJobs();
  const skills = useAutomationSkills();
  const receipts = useAutomationFactReceipts();

  return (
    <div
      tabIndex={0}
      role="region"
      aria-label="Automations content"
      className="flex h-full flex-col overflow-auto"
    >
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-edge-subtle px-4 py-2">
        <h1 className="text-sm font-semibold tracking-tight">Automations</h1>
        {scheduler.data?.outcome === "ok" ? (
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

      <PayloadBoundary
        title="Scheduler"
        pending={scheduler.isPending}
        result={scheduler.data}
      >
        {(data) => <SchedulerBody data={data} />}
      </PayloadBoundary>

      <OverviewGrid>
        <OverviewCard title="Jobs">
          <PayloadBoundary
            title="Jobs"
            pending={jobs.isPending}
            result={jobs.data}
          >
            {(data) => <JobsBody data={data.jobs} count={data.count} />}
          </PayloadBoundary>
        </OverviewCard>
        <OverviewCard title="Managed skills">
          <PayloadBoundary
            title="Managed skills"
            pending={skills.isPending}
            result={skills.data}
          >
            {(data) => <SkillsBody data={data.skills} count={data.count} />}
          </PayloadBoundary>
        </OverviewCard>
        <OverviewCard title="Fact application outcomes">
          <PayloadBoundary
            title="Fact application outcomes"
            pending={receipts.isPending}
            result={receipts.data}
          >
            {(data) => (
              <FactReceiptsBody
                data={data.receipts}
                count={data.count}
                limit={data.limit}
              />
            )}
          </PayloadBoundary>
        </OverviewCard>
        <OverviewCard title="Run history">
          <RunHistory />
        </OverviewCard>
      </OverviewGrid>
    </div>
  );
}

function SchedulerBody({ data }: { data: SchedulerStatus }) {
  return (
    <div className="flex flex-col gap-3 p-4">
      <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
        <StatTile label="state" value={data.status} />
        <StatTile
          label="automation"
          value={data.enabled ? "enabled" : "disabled"}
        />
        <StatTile
          label="tick interval"
          value={`${data.scheduler_tick_secs}s`}
        />
        <StatTile
          label="configuration revision"
          value={data.configuration_revision_id}
        />
      </div>
      <div className="flex flex-col gap-1 border-t border-edge-subtle pt-2">
        <p className="text-3xs leading-relaxed text-text-muted">
          Validation, curation, and skill activation run automatically. The rows
          below are the scheduler&apos;s latest due/skip readings, not an
          operator queue.
        </p>
        {data.tasks.length === 0 ? (
          <p className="text-2xs text-text-muted">
            no scheduler task readings are available
          </p>
        ) : (
          <ul className="flex flex-col gap-1">
            {data.tasks.map((task) => (
              <li
                key={task.task}
                className="flex flex-wrap items-baseline justify-between gap-x-2 border-b border-edge-subtle py-1 last:border-b-0"
              >
                <span className="text-2xs text-text-primary">{task.task}</span>
                <span className="text-3xs text-text-secondary">
                  {task.due ? "due" : (task.skip_reason ?? "not due")}
                  {task.last_scheduler_run
                    ? " · last run recorded"
                    : " · no run recorded"}
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}

function JobsBody({ data, count }: { data: readonly JobRow[]; count: number }) {
  const reading = tallied(data, count, "jobs");
  if (reading.rows.length === 0) {
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
}

function SkillsBody({
  data,
  count,
}: {
  data: readonly SkillRow[];
  count: number;
}) {
  const reading = tallied(data, count, "managed skills");
  if (reading.rows.length === 0) {
    return reading.complete ? (
      <p className="text-2xs text-text-muted">
        no managed skills have been activated
      </p>
    ) : (
      <PartialNotice reason={reading.reason} />
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
}

function FactReceiptsBody({
  data,
  count,
  limit,
}: {
  data: readonly AutomaticFactReceipt[];
  count: number;
  limit: number;
}) {
  const reading = talliedFactReceipts(data, count, limit);
  if (reading.rows.length === 0) {
    return reading.complete ? (
      <p className="text-2xs text-text-muted">
        no fact application outcomes are recorded
      </p>
    ) : (
      <PartialNotice reason={reading.reason} />
    );
  }
  return (
    <>
      {reading.complete ? null : <PartialNotice reason={reading.reason} />}
      <div className="flex flex-col">
        {reading.rows.map((receipt) => (
          <FactReceiptRowLine key={receipt.apply_id} receipt={receipt} />
        ))}
      </div>
    </>
  );
}

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
        <CirclePause
          aria-hidden
          size={13}
          className="shrink-0 text-text-muted"
        />
      )}
      <span className="min-w-0 flex-1 truncate text-xs">{job.name}</span>
      <span className="tabular shrink-0 text-2xs text-text-muted">
        {job.schedule ??
          (job.interval_secs != null
            ? `every ${job.interval_secs}s`
            : "manual")}
      </span>
    </div>
  );
}

function SkillRowLine({ skill }: { skill: SkillRow }) {
  return (
    <div className="flex items-center gap-2 border-b border-edge-subtle py-1.5 last:border-b-0">
      <CircleCheck aria-hidden size={13} className="shrink-0 text-text-muted" />
      <span className="min-w-0 flex-1 truncate text-xs">
        {skill.metadata.title}
      </span>
      <StateLabel state={skill.metadata.state} />
    </div>
  );
}

function FactReceiptRowLine({ receipt }: { receipt: AutomaticFactReceipt }) {
  const content = receipt.add_fact_request.content;
  return (
    <div className="flex flex-col gap-1 border-b border-edge-subtle py-1.5 last:border-b-0">
      <div className="flex items-center gap-2">
        {content !== undefined ? (
          <span className="min-w-0 flex-1 truncate text-xs" title={content}>
            {content}
          </span>
        ) : (
          <span className="min-w-0 flex-1 truncate text-xs text-text-muted">
            receipt carries no fact text
          </span>
        )}
        <StateLabel state={receipt.state} />
      </div>
      <p className="break-all font-mono text-3xs text-text-muted">
        apply {receipt.apply_id} · run {receipt.run_id}
        {receipt.applied_fact_id ? ` · fact ${receipt.applied_fact_id}` : ""}
        {receipt.evidence_hash ? ` · evidence ${receipt.evidence_hash}` : ""}
      </p>
      {receipt.validation !== undefined ? (
        <pre className="max-h-32 overflow-auto whitespace-pre-wrap break-words text-3xs text-text-secondary">
          validation {JSON.stringify(receipt.validation, null, 2)}
        </pre>
      ) : null}
      {receipt.quarantine_reason ? (
        <p className="text-2xs leading-relaxed text-state-error">
          quarantine: {receipt.quarantine_reason}
        </p>
      ) : null}
    </div>
  );
}

function StateLabel({ state }: { state: string }) {
  return (
    <span className="shrink-0 rounded-[var(--radius-chip)] border border-edge-subtle px-1.5 text-2xs text-text-muted">
      {state}
    </span>
  );
}

function controlFailure(
  result: SchedulerControlResult | undefined,
): string | null {
  if (result === undefined || result.outcome === "ok") return null;
  switch (result.outcome) {
    case "offline":
      return "The daemon did not answer, so the scheduler was not changed.";
    case "unauthorized":
      return "The daemon accepted no identity for the change, so the scheduler was not changed.";
    case "denied":
      return "This identity is not permitted to control the scheduler, so it was not changed.";
    case "error":
      return `The daemon refused the change (${result.detail}).`;
    case "unsupported_schema":
      return "The daemon answered in a shape this dashboard cannot read, so whether the scheduler changed is unknown — reload to re-read it.";
    case "unavailable":
      return `The scheduler was not changed: ${result.reason ?? result.status}.`;
    case "read_only_scope":
      return `The scheduler was not changed: ${result.refusal.detail}.`;
    case "not_dispatched":
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
  const blocked = writability.state !== "writable";
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
      <button
        type="button"
        disabled={pending || blocked}
        aria-describedby="scheduler-control-scope"
        onClick={() => onToggle(!paused)}
        className="td-hit group disabled:opacity-50"
      >
        <span
          className={cn(
            "inline-flex h-5 items-center gap-1 rounded-[var(--radius-chip)] border border-edge-subtle px-1.5 text-2xs",
            "group-hover:bg-surface-2",
          )}
        >
          {pending
            ? "working…"
            : paused
              ? "Resume scheduler"
              : "Pause scheduler"}
        </span>
      </button>
      <span
        id="scheduler-control-scope"
        data-scope-writability={writability.state}
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

function SchedulerBadge({
  status,
  paused,
}: {
  status: string;
  paused: boolean;
}) {
  return (
    <span
      className={cn(
        "inline-flex h-5 items-center gap-1 rounded-[var(--radius-chip)] border px-1.5 text-2xs",
        paused
          ? "border-edge-subtle text-text-muted"
          : "border-accent/40 bg-accent/10 text-text-primary",
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
