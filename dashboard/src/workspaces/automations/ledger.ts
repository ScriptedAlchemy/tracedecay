/**
 * Automations read model: pure projections over the daemon's automation
 * authorities. Nothing here fetches, and nothing here invents a value — every
 * function returns either a measurement of its input or a typed absence.
 *
 * The authorities are independent and stay independent through this module:
 *   scheduler status   `/api/automation/scheduler/status` (configuration plus
 *                      the scheduler's own due/skip readings and last runs)
 *   run ledger         `/api/automation/runs` (newest ledger page)
 *   user jobs          `/api/automation/jobs`
 *   fact receipts      `/api/automation/automatic-fact-receipts`
 * A join between two of them is only ever made on an exact recorded identity
 * (`run_id`, `task_key`), never on proximity or a name.
 */
import type {
  AutomationSchedulerStatusV1,
  AutomationTaskStatusV1,
} from '../../contracts/generated.ts';
import {
  SchedulerLastRunSchema,
  type AutomaticFactReceipt,
  type JobRow,
  type ListReading,
  type RunArtifactPayload,
  type RunArtifactRow,
  type RunRow,
  type SchedulerLastRun,
} from '../../data/query/automation.ts';
import type { DomainStateKind } from '../../ui/StateChip.tsx';

/* ---- what the inspector is looking at --------------------------------- */

/** One inspectable identity. Hover and focus set it; click pins it. */
export type Inspected =
  | { kind: 'run'; runId: string }
  | { kind: 'task'; task: string }
  | { kind: 'job'; jobId: string }
  | { kind: 'receipt'; applyId: string };

export function sameInspected(a: Inspected | null, b: Inspected | null): boolean {
  if (a === null || b === null) return a === b;
  if (a.kind !== b.kind) return false;
  switch (a.kind) {
    case 'run':
      return a.runId === (b as Extract<Inspected, { kind: 'run' }>).runId;
    case 'task':
      return a.task === (b as Extract<Inspected, { kind: 'task' }>).task;
    case 'job':
      return a.jobId === (b as Extract<Inspected, { kind: 'job' }>).jobId;
    case 'receipt':
      return a.applyId === (b as Extract<Inspected, { kind: 'receipt' }>).applyId;
    default: {
      const exhaustive: never = a;
      return exhaustive;
    }
  }
}

/* ---- typed-state language ---------------------------------------------- */

/** How a status word is drawn: the lamp utility, the text utility, and the
 * taxonomy state it belongs to. The WORD is always printed beside the lamp;
 * colour never carries the outcome alone. */
export interface Tone {
  readonly lamp: string;
  readonly text: string;
  readonly kind: DomainStateKind;
  /** `hatched`/`dashed` reinforce the degraded and disconnected families
   * without colour, per the design system's typed-state table. */
  readonly pattern: 'solid' | 'hatched' | 'dashed';
}

const READY: Tone = {
  lamp: 'bg-state-ready',
  text: 'text-state-ready',
  kind: 'ready',
  pattern: 'solid',
};
const LOADING: Tone = {
  lamp: 'bg-state-loading',
  text: 'text-text-secondary',
  kind: 'loading',
  pattern: 'solid',
};
const DEGRADED: Tone = {
  lamp: 'bg-state-partial',
  text: 'text-state-partial',
  kind: 'partial',
  pattern: 'hatched',
};
const REFUSED: Tone = {
  lamp: 'bg-state-error',
  text: 'text-state-error',
  kind: 'error',
  pattern: 'solid',
};
const DENIED: Tone = {
  lamp: 'bg-state-denied',
  text: 'text-state-denied',
  kind: 'denied',
  pattern: 'solid',
};
const DISCONNECTED: Tone = {
  lamp: 'bg-state-cancelled',
  text: 'text-text-muted',
  kind: 'cancelled',
  pattern: 'dashed',
};
const UNKNOWN: Tone = {
  lamp: 'bg-state-unknown',
  text: 'text-text-muted',
  kind: 'unknown',
  pattern: 'dashed',
};
const SIGNAL: Tone = {
  lamp: 'bg-accent',
  text: 'text-text-primary',
  kind: 'ready',
  pattern: 'solid',
};

/** `AutomationRunStatus` (run_ledger.rs): queued, running, succeeded, failed,
 * skipped. Any other word is printed as-is under the unknown family — the
 * ledger's word is the truth, the tone only says which family it is in. */
export function runStatusTone(status: string): Tone {
  switch (status) {
    case 'succeeded':
      return READY;
    case 'failed':
      return REFUSED;
    case 'skipped':
      return DISCONNECTED;
    case 'running':
      return SIGNAL;
    case 'queued':
      return LOADING;
    default:
      return UNKNOWN;
  }
}

/** Whether a status word names a settled run. Mirrors
 * `AutomationRunStatus::is_terminal`; an unrecognised word is treated as
 * unsettled so a duration is never computed for it. */
export function runIsTerminal(status: string): boolean {
  return status === 'succeeded' || status === 'failed' || status === 'skipped';
}

/** `AutomaticFactState`: applied or quarantined. */
export function receiptStateTone(state: AutomaticFactReceipt['state']): Tone {
  return state === 'applied' ? READY : DEGRADED;
}

/** `ManagedSkillState`. */
export function skillStateTone(state: string): Tone {
  switch (state) {
    case 'active':
      return READY;
    case 'disabled':
      return DISCONNECTED;
    case 'archived':
      return UNKNOWN;
    default:
      return UNKNOWN;
  }
}

/** The daemon's chain-integrity verdict (automation_run_api.rs
 * `artifact_list`): verified, ledger_publication_mismatch,
 * publication_unavailable, verification_failed. */
export function integrityTone(status: string): Tone {
  switch (status) {
    case 'verified':
      return READY;
    case 'ledger_publication_mismatch':
      return REFUSED;
    case 'publication_unavailable':
      return DISCONNECTED;
    case 'verification_failed':
      return DEGRADED;
    default:
      return UNKNOWN;
  }
}

/** `AgentTaskFailureClass`: retryable, permanent, timeout, unavailable,
 * denied, disconnected, malformed_output. */
export function failureClassTone(classification: string): Tone {
  switch (classification) {
    case 'retryable':
    case 'timeout':
      return DEGRADED;
    case 'permanent':
    case 'malformed_output':
      return REFUSED;
    case 'denied':
      return DENIED;
    case 'unavailable':
    case 'disconnected':
      return DISCONNECTED;
    default:
      return UNKNOWN;
  }
}

/* ---- scheduler -------------------------------------------------------- */

/** The scheduler status word read as a configuration state.
 *
 * `configured` is a CONFIGURATION reading — automation enabled, a backend
 * chosen, not paused. It is not an observation that the scheduler loop is
 * alive; the route serves no heartbeat. The only runtime evidence on this
 * surface is the per-task `last_scheduler_run` ledger record, which
 * {@link observedSchedulerActivity} reads. */
export interface SchedulerReading {
  readonly tone: Tone;
  readonly word: string;
  readonly sentence: string;
}

export function schedulerReading(status: AutomationSchedulerStatusV1): SchedulerReading {
  switch (status.status) {
    case 'configured':
      return {
        tone: SIGNAL,
        word: 'configured',
        sentence: 'automation enabled with a backend; the route serves no liveness heartbeat',
      };
    case 'paused':
      return {
        tone: DEGRADED,
        word: 'paused',
        sentence: 'an operator paused the scheduler; every task reads scheduler_paused',
      };
    case 'automation_disabled':
      return {
        tone: DISCONNECTED,
        word: 'automation disabled',
        sentence: 'automation is switched off in configuration; nothing is scheduled',
      };
    case 'backend_disabled':
      return {
        tone: DISCONNECTED,
        word: 'backend disabled',
        sentence: 'no agent backend is configured, so scheduled tasks cannot run',
      };
    case 'delegated_host':
      return {
        tone: DISCONNECTED,
        word: 'delegated host',
        sentence: 'this daemon delegates automation to another host and runs none itself',
      };
    default:
      return {
        tone: UNKNOWN,
        word: status.status,
        sentence: 'the scheduler reported a status word this build does not recognise',
      };
  }
}

/** What the scheduler said about one task's last scheduler-triggered run. */
export type LastRunReading =
  | { kind: 'none' }
  | { kind: 'unreadable' }
  | { kind: 'run'; run: SchedulerLastRun };

export function readLastSchedulerRun(value: unknown): LastRunReading {
  if (value === null || value === undefined) return { kind: 'none' };
  const parsed = SchedulerLastRunSchema.safeParse(value);
  return parsed.success ? { kind: 'run', run: parsed.data } : { kind: 'unreadable' };
}

/** The newest scheduler-triggered completion across every task: the one
 * observed fact this surface has about the scheduler having run at all.
 * Null when no task carries a readable last run. */
export function observedSchedulerActivity(
  tasks: readonly AutomationTaskStatusV1[],
): { runId: string; task: string; completedAt: number } | null {
  let newest: { runId: string; task: string; completedAt: number } | null = null;
  for (const task of tasks) {
    const reading = readLastSchedulerRun(task.last_scheduler_run);
    if (reading.kind !== 'run') continue;
    const completedAt = epochSeconds(reading.run.completed_at);
    if (completedAt === null) continue;
    if (newest === null || completedAt > newest.completedAt) {
      newest = { runId: reading.run.run_id, task: task.task, completedAt };
    }
  }
  return newest;
}

export interface DueSummary {
  readonly due: number;
  readonly skipped: number;
  readonly total: number;
}

/** How many scheduler tasks the daemon marked due right now, and how many it
 * gave a skip reason. A task can be neither (not due, no reason recorded). */
export function dueSummary(tasks: readonly AutomationTaskStatusV1[]): DueSummary {
  let due = 0;
  let skipped = 0;
  for (const task of tasks) {
    if (task.due) due += 1;
    else if (task.skip_reason !== null) skipped += 1;
  }
  return { due, skipped, total: tasks.length };
}

/* ---- time ------------------------------------------------------------- */

/** Ledger stamps are Unix seconds serialised as strings. Anything that does
 * not parse as a finite non-negative number is returned as `null` so the
 * caller prints the raw stamp rather than a fabricated date. */
export function epochSeconds(stamp: string | null | undefined): number | null {
  if (stamp === null || stamp === undefined) return null;
  const trimmed = stamp.trim();
  if (!/^\d+(\.\d+)?$/.test(trimmed)) return null;
  const value = Number(trimmed);
  return Number.isFinite(value) ? value : null;
}

const pad = (value: number, width = 2) => String(value).padStart(width, '0');

/** `2026-09-17 01:57:02` in UTC — the ledger's own clock, never the browser's
 * locale. */
export function formatUtc(epochSecs: number): string {
  const date = new Date(epochSecs * 1000);
  return `${date.getUTCFullYear()}-${pad(date.getUTCMonth() + 1)}-${pad(date.getUTCDate())} ${pad(date.getUTCHours())}:${pad(date.getUTCMinutes())}:${pad(date.getUTCSeconds())}`;
}

/** `hh:mm:ss`; hours grow past two digits rather than wrapping. */
export function formatDuration(secs: number): string {
  const whole = Math.max(0, Math.floor(secs));
  const hours = Math.floor(whole / 3600);
  const minutes = Math.floor((whole % 3600) / 60);
  const seconds = whole % 60;
  return `${pad(hours)}:${pad(minutes)}:${pad(seconds)}`;
}

/** A run's timing, measured from its own two stamps. */
export type RunTiming =
  /** Both stamps parsed and the run has settled: a real duration. */
  | { kind: 'measured'; startedAt: number; completedAt: number; durationSecs: number }
  /** Started, not settled: the ledger has no end to measure to. */
  | { kind: 'open'; startedAt: number }
  /** Settled, but the stamps run backwards: printed, not measured. */
  | { kind: 'inverted'; startedAt: number; completedAt: number }
  /** One or both stamps are not epoch seconds: shown verbatim. */
  | { kind: 'unparsed'; startedAt: string; completedAt: string };

export function runTiming(run: Pick<RunRow, 'status' | 'started_at' | 'completed_at'>): RunTiming {
  const startedAt = epochSeconds(run.started_at);
  if (startedAt === null) {
    return { kind: 'unparsed', startedAt: run.started_at, completedAt: run.completed_at };
  }
  if (!runIsTerminal(run.status)) return { kind: 'open', startedAt };
  const completedAt = epochSeconds(run.completed_at);
  if (completedAt === null) {
    return { kind: 'unparsed', startedAt: run.started_at, completedAt: run.completed_at };
  }
  if (completedAt < startedAt) return { kind: 'inverted', startedAt, completedAt };
  return { kind: 'measured', startedAt, completedAt, durationSecs: completedAt - startedAt };
}

/* ---- ledger window ---------------------------------------------------- */

export interface StatusTally {
  readonly queued: number;
  readonly running: number;
  readonly succeeded: number;
  readonly failed: number;
  readonly skipped: number;
  readonly other: number;
}

/** The loaded ledger page as a declared window: how many rows it holds,
 * whether the ledger extends past it, and the status tally over exactly those
 * rows. Aggregates on this surface are always "of the loaded page"; there is
 * no route that tallies the whole ledger, so none is claimed. */
export interface LedgerWindow {
  readonly loaded: number;
  /** Older records exist beyond this page, or the reader skipped rows. */
  readonly bounded: boolean;
  readonly boundedReason: string | null;
  readonly tally: StatusTally;
  /** Oldest and newest parseable start stamps in the page. */
  readonly oldestStart: number | null;
  readonly newestStart: number | null;
}

export function ledgerWindow(reading: ListReading<RunRow>): LedgerWindow {
  const tally = { queued: 0, running: 0, succeeded: 0, failed: 0, skipped: 0, other: 0 };
  let oldestStart: number | null = null;
  let newestStart: number | null = null;
  for (const run of reading.rows) {
    switch (run.status) {
      case 'queued':
        tally.queued += 1;
        break;
      case 'running':
        tally.running += 1;
        break;
      case 'succeeded':
        tally.succeeded += 1;
        break;
      case 'failed':
        tally.failed += 1;
        break;
      case 'skipped':
        tally.skipped += 1;
        break;
      default:
        tally.other += 1;
    }
    const started = epochSeconds(run.started_at);
    if (started === null) continue;
    if (oldestStart === null || started < oldestStart) oldestStart = started;
    if (newestStart === null || started > newestStart) newestStart = started;
  }
  // `automationRunsReading` already folds `has_more`, malformed rows and the
  // daemon's own completeness flag into the reading, so the window's bound is
  // that one verdict rather than a second reading of the same flags.
  return {
    loaded: reading.rows.length,
    bounded: !reading.complete,
    boundedReason: reading.complete ? null : reading.reason,
    tally,
    oldestStart,
    newestStart,
  };
}

/* ---- artifact evidence ------------------------------------------------- */

/** Expected chain kinds the ledger entry did not record. Presence and
 * integrity are independent: this is only presence. */
export function missingArtifactKinds(chain: {
  readonly expected_kinds: readonly string[];
  readonly present_kinds: readonly string[];
}): string[] {
  return chain.expected_kinds.filter((kind) => !chain.present_kinds.includes(kind));
}

/** Whether a served artifact payload is the one that was asked for: same run,
 * same kind, same recorded path and digest. A payload that fails this is a
 * body for some other artifact and must not be shown under this one. */
export function artifactPayloadBelongsTo(
  payload: Pick<RunArtifactPayload, 'run_id' | 'artifact'>,
  runId: string,
  artifact: Pick<RunArtifactRow, 'kind' | 'path' | 'sha256'>,
): boolean {
  return (
    payload.run_id === runId &&
    payload.artifact.kind === artifact.kind &&
    payload.artifact.path === artifact.path &&
    payload.artifact.sha256 === artifact.sha256
  );
}

/* ---- joins on exact identity ------------------------------------------ */

export interface RunReceipts {
  readonly applied: number;
  readonly quarantined: number;
  readonly rows: readonly AutomaticFactReceipt[];
}

/** Fact receipts filed under the `run_id` each one recorded. */
export function receiptsByRun(
  receipts: readonly AutomaticFactReceipt[],
): ReadonlyMap<string, RunReceipts> {
  const byRun = new Map<string, { applied: number; quarantined: number; rows: AutomaticFactReceipt[] }>();
  for (const receipt of receipts) {
    const entry = byRun.get(receipt.run_id) ?? { applied: 0, quarantined: 0, rows: [] };
    if (receipt.state === 'applied') entry.applied += 1;
    else entry.quarantined += 1;
    entry.rows.push(receipt);
    byRun.set(receipt.run_id, entry);
  }
  return byRun;
}

/** `job_task_key` (jobs.rs): the exact ledger identity of a user job's runs. */
export function jobTaskKey(jobId: string): string {
  return `user_job:${jobId}`;
}

/** The newest run in the loaded page recorded under `taskKey`. The page is
 * served newest-first, so the first match is the latest. `undefined` means
 * no run for this key is in the loaded page — not that the job never ran. */
export function latestRunForKey(runs: readonly RunRow[], taskKey: string): RunRow | undefined {
  return runs.find((run) => run.task_key === taskKey);
}

/** The newest run in the loaded page for a built-in scheduler task. Built-in
 * tasks record `task_key` equal to their task word; older rows carry only
 * `task`, so both are consulted. */
export function latestRunForTask(runs: readonly RunRow[], task: string): RunRow | undefined {
  return runs.find((run) => run.task_key === task || (run.task_key === null && run.task === task));
}

/** The words a user job's schedule reduces to. Mirrors `parse_schedule`
 * inputs (jobs.rs) without evaluating them: `manual`, an interval, or the
 * schedule expression verbatim. */
export function jobScheduleWord(job: Pick<JobRow, 'schedule' | 'interval_secs'>): string {
  if (job.schedule === 'interval' || (job.schedule == null && job.interval_secs != null)) {
    return job.interval_secs != null ? `every ${formatDuration(job.interval_secs)}` : 'interval (unset)';
  }
  if (job.schedule == null || job.schedule === '') return 'manual';
  return job.schedule;
}

/** `task_key` → the job id it names, or null for a built-in task. */
export function jobIdFromTaskKey(taskKey: string | null): string | null {
  if (taskKey === null || !taskKey.startsWith('user_job:')) return null;
  const id = taskKey.slice('user_job:'.length);
  return id.length > 0 ? id : null;
}
