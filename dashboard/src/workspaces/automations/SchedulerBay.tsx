import type { ReactNode } from 'react';
import { Pause, Play } from 'lucide-react';

import type {
  AutomationSchedulerStatusV1,
  AutomationTaskStatusV1,
} from '../../contracts/generated.ts';
import type { RunRow, SchedulerControlResult } from '../../data/query/automation.ts';
import { scopeWriteSentence, type ScopeWritability } from '../../data/scope/store.ts';
import { cn } from '../../ui/cn';
import { Panel, ReadoutBar, type ReadoutItem } from '../../ui/instrument.tsx';
import { relativeAge } from '../../ui/time.ts';
import { Absent, Cell, InspectRow, LedgerTable, Term, ToneWord } from './LedgerTable.tsx';
import {
  dueSummary,
  epochSeconds,
  formatUtc,
  latestRunForTask,
  observedSchedulerActivity,
  readLastSchedulerRun,
  runStatusTone,
  sameInspected,
  schedulerReading,
  type Inspected,
  type LedgerWindow,
} from './ledger.ts';

/**
 * The scheduler bay: the configuration reading, the one real control this
 * surface has, the scheduler's own due/skip readings per task, and the
 * observed evidence that the scheduler has run at all.
 *
 * Configuration and observation are kept in separate boxes on purpose. The
 * status word comes from the pinned configuration snapshot; the only runtime
 * evidence is the last scheduler-triggered ledger record each task carries.
 * `configured` never turns into `running` here.
 */
export function SchedulerBay({
  status,
  control,
  runs,
  inspected,
  pinned,
  onInspect,
  onSelect,
  onLeave,
}: {
  status: AutomationSchedulerStatusV1;
  control: {
    pending: boolean;
    failure: string | null;
    writability: ScopeWritability;
    onToggle: (paused: boolean) => void;
  };
  /** Loaded ledger rows, or null while that read is blocked. */
  runs: readonly RunRow[] | null;
  inspected: Inspected | null;
  pinned: Inspected | null;
  onInspect: (next: Inspected) => void;
  onSelect: (next: Inspected) => void;
  onLeave: () => void;
}) {
  const reading = schedulerReading(status);
  const observed = observedSchedulerActivity(status.tasks);
  const due = dueSummary(status.tasks);
  return (
    <div className="grid gap-3 xl:grid-cols-[minmax(0,2fr)_minmax(0,3fr)]">
      <Panel legend="Scheduler status" tone="signal">
        <div className="flex min-w-0 flex-col gap-3">
          <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-2">
            <div className="flex min-w-0 flex-col gap-1">
              <ToneWord tone={reading.tone} word={reading.word} className="text-base font-medium" />
              <span className="text-3xs leading-relaxed text-text-muted">{reading.sentence}</span>
            </div>
            <SchedulerControl paused={status.paused} {...control} />
          </div>
          <dl className="grid grid-cols-2 gap-x-3 gap-y-2 border-t border-edge-subtle pt-2 sm:grid-cols-3">
            <Term label="automation">{status.enabled ? 'enabled' : 'disabled'}</Term>
            <Term label="tick interval" mono>{status.scheduler_tick_secs}s</Term>
            <Term label="daemon clock (utc)" mono>{formatUtc(status.now)}</Term>
            <Term label="last session activity" mono>
              {status.last_session_activity === null ? (
                <Absent>none recorded</Absent>
              ) : (
                relativeAge(status.last_session_activity, status.now) ?? String(status.last_session_activity)
              )}
            </Term>
            <Term label="configuration revision" mono>{status.configuration_revision_id}</Term>
            <Term label="control path" mono>{status.control_path}</Term>
          </dl>
          <div className="flex min-w-0 flex-col gap-1 border-t border-edge-subtle pt-2">
            <span className="td-legend">observed scheduler run</span>
            {observed === null ? (
              <span className="text-2xs text-text-muted">
                no scheduler-triggered run is recorded for any task, the status above is configuration, not liveness
              </span>
            ) : (
              <span className="td-value text-2xs text-text-secondary">
                {observed.task} · {observed.runId} · completed {formatUtc(observed.completedAt)} UTC
              </span>
            )}
          </div>
        </div>
      </Panel>

      <Panel legend="Scheduler tasks · due window" elevation="well" bodyClassName="p-0">
        <div className="flex flex-wrap items-baseline gap-x-4 gap-y-1 border-b border-edge-subtle px-3 py-2 text-2xs">
          <span>
            <span className="td-value text-base text-text-primary">{due.due}</span>
            <span className="td-unit ml-1">of {due.total} due now</span>
          </span>
          <span>
            <span className="td-value text-base text-text-primary">{due.skipped}</span>
            <span className="td-unit ml-1">with a skip reason</span>
          </span>
          <span className="min-w-0 text-3xs text-text-muted">
            due timestamps are not served, the scheduler reports a due flag per tick
          </span>
        </div>
        <LedgerTable
          columns={['task', 'due', 'skip reason', 'last scheduler run', 'outcome']}
          caption="Scheduler tasks with their due flag, skip reason and last scheduler-triggered run"
          onPointerLeave={onLeave}
        >
          {status.tasks.map((task) => {
            const identity: Inspected = { kind: 'task', task: task.task };
            return (
              <TaskLine
                key={task.task}
                task={task}
                runs={runs}
                inspected={sameInspected(inspected, identity)}
                selected={sameInspected(pinned, identity)}
                onInspect={() => onInspect(identity)}
                onSelect={() => onSelect(identity)}
              />
            );
          })}
        </LedgerTable>
        {status.tasks.length === 0 ? (
          <p className="px-3 py-2 text-2xs text-text-muted">no scheduler task readings are available</p>
        ) : null}
      </Panel>
    </div>
  );
}

function TaskLine({
  task,
  runs,
  inspected,
  selected,
  onInspect,
  onSelect,
}: {
  task: AutomationTaskStatusV1;
  runs: readonly RunRow[] | null;
  inspected: boolean;
  selected: boolean;
  onInspect: () => void;
  onSelect: () => void;
}) {
  const last = readLastSchedulerRun(task.last_scheduler_run);
  // The scheduler's own last-run record is the authority for this row; the
  // loaded ledger page is consulted only when the scheduler attached none.
  const fallback = last.kind === 'none' && runs !== null ? latestRunForTask(runs, task.task) : undefined;
  return (
    <InspectRow
      testId={`task-row-${task.task}`}
      label={`Scheduler task ${task.task}: ${task.due ? 'due' : (task.skip_reason ?? 'not due')}`}
      inspected={inspected}
      selected={selected}
      onInspect={onInspect}
      onSelect={onSelect}
      identity={<span className="td-value truncate text-2xs text-text-primary">{task.task}</span>}
    >
      <Cell>{task.due ? <span className="text-accent">due</span> : <span className="text-text-muted">not due</span>}</Cell>
      <Cell>
        {task.skip_reason ? (
          <span className="td-value text-2xs text-text-secondary">{task.skip_reason}</span>
        ) : (
          <Absent>none</Absent>
        )}
      </Cell>
      <Cell numeric>
        {last.kind === 'run' ? (
          <LastRunStamp completedAt={last.run.completed_at} />
        ) : last.kind === 'unreadable' ? (
          <Absent>record unreadable</Absent>
        ) : fallback ? (
          <span className="flex flex-col gap-0.5">
            <LastRunStamp completedAt={fallback.completed_at} />
            <span className="text-3xs text-text-muted">from loaded ledger page</span>
          </span>
        ) : (
          <Absent>none recorded</Absent>
        )}
      </Cell>
      <Cell>
        {last.kind === 'run' ? (
          <ToneWord tone={runStatusTone(last.run.status)} word={last.run.status} />
        ) : fallback ? (
          <ToneWord tone={runStatusTone(fallback.status)} word={fallback.status} />
        ) : (
          <Absent>n/a</Absent>
        )}
      </Cell>
    </InspectRow>
  );
}

function LastRunStamp({ completedAt }: { completedAt: string }) {
  const secs = epochSeconds(completedAt);
  return <>{secs === null ? completedAt || <Absent>empty stamp</Absent> : formatUtc(secs)}</>;
}

/** Pause / resume: the one scheduler mutation the daemon exposes to the
 * dashboard. Both buttons are always drawn so the pair reads as a control
 * group; the one matching the current reading is disabled because re-sending
 * it would be a no-op the daemon still answers. Both disable when the scope
 * refuses writes, with the reason printed beside them. */
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
  const blocked = writability.state !== 'writable';
  return (
    <div className="flex min-w-0 flex-col items-end gap-1">
      <div className="flex items-center gap-1" role="group" aria-label="Scheduler control">
        <ControlButton
          label="Pause scheduler"
          icon={<Pause aria-hidden size={11} />}
          active={!paused}
          disabled={pending || blocked || paused}
          onClick={() => onToggle(true)}
        />
        <ControlButton
          label="Resume scheduler"
          icon={<Play aria-hidden size={11} />}
          active={paused}
          disabled={pending || blocked || !paused}
          onClick={() => onToggle(false)}
        />
      </div>
      <span
        id="scheduler-control-scope"
        data-scope-writability={writability.state}
        className="max-w-[16rem] text-right text-3xs leading-relaxed text-text-muted"
      >
        {pending
          ? 'waiting for the daemon to re-read the scheduler…'
          : scopeWriteSentence(writability, { writable: (target) => `Applies to ${target}.` })}
      </span>
      {failure ? (
        <span role="status" className="max-w-[16rem] text-right text-3xs leading-relaxed text-text-secondary">
          {failure}
        </span>
      ) : null}
    </div>
  );
}

function ControlButton({
  label,
  icon,
  active,
  disabled,
  onClick,
}: {
  label: string;
  icon: ReactNode;
  active: boolean;
  disabled: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      aria-describedby="scheduler-control-scope"
      onClick={onClick}
      className="td-hit group disabled:cursor-not-allowed"
    >
      <span
        className={cn(
          'inline-flex h-6 items-center gap-1.5 border px-2 text-2xs',
          active
            ? 'border-accent/60 bg-accent/10 text-text-primary group-hover:bg-accent/20'
            : 'border-edge-subtle text-text-muted group-hover:bg-surface-2',
          disabled && 'opacity-50 group-hover:bg-transparent',
        )}
      >
        {icon}
        {label}
      </span>
    </button>
  );
}

/** The KPI bar: the scheduler's due count plus the status tally of exactly
 * the loaded ledger page. Every figure names its population; a blocked ledger
 * read prints an em dash with the reason rather than a zero. */
export function LedgerReadouts({
  status,
  window,
  ledgerBlocked,
}: {
  status: AutomationSchedulerStatusV1 | null;
  window: LedgerWindow | null;
  /** The blocked state's word when the ledger read produced no rows. */
  ledgerBlocked: string | null;
}) {
  const due = status ? dueSummary(status.tasks) : null;
  const population =
    window === null
      ? ledgerBlocked ?? 'ledger unread'
      : `of ${window.loaded} loaded ${window.loaded === 1 ? 'run' : 'runs'}${window.bounded ? ' · bounded' : ''}`;
  const tally = (value: number | undefined): string => (value === undefined ? '—' : String(value));
  const items: ReadoutItem[] = [
    {
      label: 'due now',
      value: due ? String(due.due) : '—',
      note: due ? `of ${due.total} scheduler tasks` : 'scheduler unread',
    },
    { label: 'running', value: tally(window?.tally.running), note: population },
    { label: 'queued', value: tally(window?.tally.queued), note: population },
    { label: 'succeeded', value: tally(window?.tally.succeeded), note: population },
    { label: 'failed', value: tally(window?.tally.failed), note: population },
    { label: 'skipped', value: tally(window?.tally.skipped), note: population },
  ];
  return <ReadoutBar items={items} size="lg" elevation="raised" label="Ledger window tallies" />;
}
