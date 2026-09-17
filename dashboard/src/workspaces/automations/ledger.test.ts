import { describe, expect, it } from 'vitest';

import type {
  AutomationSchedulerStatusV1,
  AutomationTaskStatusV1,
} from '../../contracts/generated.ts';
import type { RunRow } from '../../data/query/automation.ts';
import {
  dueSummary,
  epochSeconds,
  formatDuration,
  formatUtc,
  integrityTone,
  jobIdFromTaskKey,
  jobScheduleWord,
  jobTaskKey,
  latestRunForKey,
  latestRunForTask,
  ledgerWindow,
  observedSchedulerActivity,
  readLastSchedulerRun,
  receiptsByRun,
  runStatusTone,
  runTiming,
  sameInspected,
  schedulerReading,
} from './ledger.ts';

function run(overrides: Partial<RunRow> & { run_id: string }): RunRow {
  return {
    task: 'memory_curator',
    task_key: 'memory_curator',
    trigger: 'scheduler',
    backend: 'codex_app_server',
    model: null,
    status: 'succeeded',
    reviewed_count: 0,
    accepted_count: 0,
    rejected_count: 0,
    skipped_count: 0,
    error: null,
    error_classification: null,
    error_retryable: null,
    backend_attempt_count: 1,
    started_at: '1754000000',
    completed_at: '1754000060',
    artifact_kinds: [],
    ...overrides,
  };
}

describe('epochSeconds', () => {
  it('reads the ledger stamp as unix seconds and refuses anything else', () => {
    expect(epochSeconds('1754000000')).toBe(1_754_000_000);
    expect(epochSeconds(' 1754000000 ')).toBe(1_754_000_000);
    expect(epochSeconds('2026-06-24T00:00:00Z')).toBeNull();
    expect(epochSeconds('')).toBeNull();
    expect(epochSeconds('-5')).toBeNull();
    expect(epochSeconds(null)).toBeNull();
  });
});

describe('formatting', () => {
  it('prints the ledger clock in UTC and durations as hh:mm:ss', () => {
    expect(formatUtc(1_754_000_000)).toBe('2025-07-31 22:13:20');
    expect(formatUtc(0)).toBe('1970-01-01 00:00:00');
    expect(formatDuration(18.7)).toBe('00:00:18');
    expect(formatDuration(3661)).toBe('01:01:01');
    expect(formatDuration(360_000)).toBe('100:00:00');
  });
});

describe('runTiming', () => {
  it('measures a settled run from its own two stamps', () => {
    expect(runTiming(run({ run_id: 'a' }))).toEqual({
      kind: 'measured',
      startedAt: 1_754_000_000,
      completedAt: 1_754_000_060,
      durationSecs: 60,
    });
  });

  it('leaves an unsettled run open rather than measuring to a placeholder end', () => {
    expect(runTiming(run({ run_id: 'a', status: 'running', completed_at: '' }))).toEqual({
      kind: 'open',
      startedAt: 1_754_000_000,
    });
    // An unrecognised status word is not known to be terminal either.
    expect(runTiming(run({ run_id: 'a', status: 'settling' })).kind).toBe('open');
  });

  it('names inverted and unparseable stamps instead of inventing a duration', () => {
    expect(runTiming(run({ run_id: 'a', started_at: '1754000060', completed_at: '1754000000' }))).toEqual({
      kind: 'inverted',
      startedAt: 1_754_000_060,
      completedAt: 1_754_000_000,
    });
    expect(runTiming(run({ run_id: 'a', started_at: 'yesterday' }))).toEqual({
      kind: 'unparsed',
      startedAt: 'yesterday',
      completedAt: '1754000060',
    });
  });
});

describe('typed tones', () => {
  it('files every ledger status word under a typed-state family and prints unknown words as unknown', () => {
    expect(runStatusTone('succeeded').kind).toBe('ready');
    expect(runStatusTone('failed').kind).toBe('error');
    expect(runStatusTone('skipped').kind).toBe('cancelled');
    expect(runStatusTone('skipped').pattern).toBe('dashed');
    expect(runStatusTone('queued').kind).toBe('loading');
    expect(runStatusTone('completed').kind).toBe('unknown');
  });

  it('never upgrades a non-verified integrity verdict', () => {
    expect(integrityTone('verified').kind).toBe('ready');
    expect(integrityTone('ledger_publication_mismatch').kind).toBe('error');
    expect(integrityTone('publication_unavailable').kind).toBe('cancelled');
    expect(integrityTone('verification_failed').kind).toBe('partial');
    expect(integrityTone('probably_fine').kind).toBe('unknown');
  });
});

describe('schedulerReading', () => {
  const status = (
    word: AutomationSchedulerStatusV1['status'],
  ): AutomationSchedulerStatusV1 => ({
    status: word,
    paused: word === 'paused',
    enabled: word !== 'automation_disabled',
    scheduler_tick_secs: 900,
    now: 1_754_000_000,
    last_session_activity: null,
    configuration_revision_id: 'rev',
    control_path: '/x',
    tasks: [],
  });

  it('reads configured as a configuration state, not a liveness claim', () => {
    const reading = schedulerReading(status('configured'));
    expect(reading.word).toBe('configured');
    expect(reading.sentence).toMatch(/no liveness heartbeat/);
  });

  it('keeps paused, disabled and delegated states distinct', () => {
    expect(schedulerReading(status('paused')).tone.kind).toBe('partial');
    expect(schedulerReading(status('automation_disabled')).tone.kind).toBe('cancelled');
    expect(schedulerReading(status('backend_disabled')).word).toBe('backend disabled');
    expect(schedulerReading(status('delegated_host')).word).toBe('delegated host');
  });
});

describe('scheduler task readings', () => {
  it('distinguishes no last run from an unreadable one', () => {
    expect(readLastSchedulerRun(null)).toEqual({ kind: 'none' });
    expect(readLastSchedulerRun(undefined)).toEqual({ kind: 'none' });
    expect(readLastSchedulerRun({ run_id: 'r' })).toEqual({ kind: 'unreadable' });
    expect(
      readLastSchedulerRun({
        run_id: 'r',
        status: 'failed',
        started_at: '1',
        completed_at: '2',
        error: 'boom',
        artifacts: [],
      }),
    ).toMatchObject({ kind: 'run', run: { run_id: 'r', status: 'failed' } });
  });

  it('reports the newest readable scheduler completion across tasks', () => {
    const tasks: AutomationTaskStatusV1[] = [
      { task: 'memory_curator', due: false, skip_reason: null, last_scheduler_run: null },
      {
        task: 'session_reflector',
        due: true,
        skip_reason: null,
        last_scheduler_run: { run_id: 'old', status: 'succeeded', started_at: '10', completed_at: '20' },
      },
      {
        task: 'skill_writer',
        due: false,
        skip_reason: 'scheduler_cooldown_active',
        last_scheduler_run: { run_id: 'new', status: 'failed', started_at: '30', completed_at: '40' },
      },
    ];
    expect(observedSchedulerActivity(tasks)).toEqual({
      runId: 'new',
      task: 'skill_writer',
      completedAt: 40,
    });
    expect(observedSchedulerActivity([tasks[0]!])).toBeNull();
    expect(dueSummary(tasks)).toEqual({ due: 1, skipped: 1, total: 3 });
  });
});

describe('ledgerWindow', () => {
  it('tallies exactly the loaded rows and carries the reading bound', () => {
    const rows = [
      run({ run_id: 'a', status: 'succeeded', started_at: '300' }),
      run({ run_id: 'b', status: 'failed', started_at: '200' }),
      run({ run_id: 'c', status: 'running', started_at: '100', completed_at: '' }),
      run({ run_id: 'd', status: 'weird', started_at: 'x' }),
    ];
    const window = ledgerWindow({ complete: false, rows, reason: 'older ledger records were outside this page' });
    expect(window.loaded).toBe(4);
    expect(window.bounded).toBe(true);
    expect(window.boundedReason).toMatch(/outside this page/);
    expect(window.tally).toEqual({ queued: 0, running: 1, succeeded: 1, failed: 1, skipped: 0, other: 1 });
    expect(window.oldestStart).toBe(100);
    expect(window.newestStart).toBe(300);
  });

  it('marks a complete page unbounded with no reason', () => {
    const window = ledgerWindow({ complete: true, rows: [] });
    expect(window.bounded).toBe(false);
    expect(window.boundedReason).toBeNull();
    expect(window.oldestStart).toBeNull();
  });
});

describe('exact-identity joins', () => {
  it('files receipts under their recorded run id', () => {
    const receipt = (apply_id: string, run_id: string, state: 'applied' | 'quarantined') => ({
      schema_version: 1,
      apply_id,
      run_id,
      state,
      add_fact_request: { content: 'fact' },
      recorded_at_micros: 1,
    });
    const byRun = receiptsByRun([
      receipt('a', 'run-1', 'applied'),
      receipt('b', 'run-1', 'quarantined'),
      receipt('c', 'run-2', 'applied'),
    ]);
    expect(byRun.get('run-1')).toMatchObject({ applied: 1, quarantined: 1 });
    expect(byRun.get('run-1')?.rows.map((row) => row.apply_id)).toEqual(['a', 'b']);
    expect(byRun.get('run-2')).toMatchObject({ applied: 1, quarantined: 0 });
    expect(byRun.get('run-3')).toBeUndefined();
  });

  it('joins a user job to its runs only on the exact task key', () => {
    const rows = [
      run({ run_id: 'newest-other', task: 'user_job', task_key: 'user_job:other' }),
      run({ run_id: 'newest-nightly', task: 'user_job', task_key: 'user_job:nightly' }),
      run({ run_id: 'older-nightly', task: 'user_job', task_key: 'user_job:nightly' }),
      run({ run_id: 'legacy', task: 'user_job', task_key: null }),
    ];
    expect(jobTaskKey('nightly')).toBe('user_job:nightly');
    expect(latestRunForKey(rows, jobTaskKey('nightly'))?.run_id).toBe('newest-nightly');
    // A pre-`task_key` user-job row belongs to no job.
    expect(latestRunForKey(rows, jobTaskKey('legacy'))).toBeUndefined();
    expect(jobIdFromTaskKey('user_job:nightly')).toBe('nightly');
    expect(jobIdFromTaskKey('memory_curator')).toBeNull();
    expect(jobIdFromTaskKey('user_job:')).toBeNull();
    expect(jobIdFromTaskKey(null)).toBeNull();
  });

  it('joins a built-in task on its task key, falling back to the task word only for keyless rows', () => {
    const rows = [
      run({ run_id: 'keyed', task: 'memory_curator', task_key: 'memory_curator' }),
      run({ run_id: 'keyless', task: 'skill_writer', task_key: null }),
      run({ run_id: 'user', task: 'user_job', task_key: 'user_job:x' }),
    ];
    expect(latestRunForTask(rows, 'memory_curator')?.run_id).toBe('keyed');
    expect(latestRunForTask(rows, 'skill_writer')?.run_id).toBe('keyless');
    expect(latestRunForTask(rows, 'user_job')).toBeUndefined();
  });
});

describe('jobScheduleWord', () => {
  it('reduces a job schedule to manual, an interval, or the expression verbatim', () => {
    expect(jobScheduleWord({ schedule: null, interval_secs: null })).toBe('manual');
    expect(jobScheduleWord({ schedule: 'manual', interval_secs: null })).toBe('manual');
    expect(jobScheduleWord({ schedule: 'interval', interval_secs: 3600 })).toBe('every 01:00:00');
    expect(jobScheduleWord({ schedule: null, interval_secs: 90 })).toBe('every 00:01:30');
    expect(jobScheduleWord({ schedule: 'interval', interval_secs: null })).toBe('interval (unset)');
    expect(jobScheduleWord({ schedule: '0 3 * * *', interval_secs: null })).toBe('0 3 * * *');
  });
});

describe('sameInspected', () => {
  it('compares identities by kind and id', () => {
    expect(sameInspected(null, null)).toBe(true);
    expect(sameInspected({ kind: 'run', runId: 'a' }, null)).toBe(false);
    expect(sameInspected({ kind: 'run', runId: 'a' }, { kind: 'run', runId: 'a' })).toBe(true);
    expect(sameInspected({ kind: 'run', runId: 'a' }, { kind: 'job', jobId: 'a' })).toBe(false);
    expect(sameInspected({ kind: 'receipt', applyId: 'a' }, { kind: 'receipt', applyId: 'b' })).toBe(false);
  });
});
