import { render, screen, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { WorkGraphReadV1Schema } from '../../contracts/generated.ts';
import { workGraphRead } from '../../test/workGraphFixture.ts';
import type { WorkGraphReadV1 } from '../../contracts/index.ts';
import type { WorkResult } from '../work/workApi.ts';
import { AgentFailureContext } from './AgentFailureContext.tsx';
import { readAttemptFailures, type AttemptFailureReading } from './failure.ts';

/**
 * Failure context.
 *
 * The invariant under test throughout: an absence is never a zero. A refused
 * attempt read, a runtime projection reporting `unavailable` coverage, and a
 * projection that observed attempts and found none failed are three different
 * readings, and only the last one is entitled to print a nought.
 */

const AT = 1_800_000_000_000_000;
const SECOND = 1_700_000_000;

const OUTCOMES = [
  { outcome: 'success', count: 9_200 },
  { outcome: 'error', count: 640 },
  { outcome: 'timed_out', count: 160 },
];

const EVENTS = [
  { timestamp: SECOND, tool_name: 'tracedecay_grep', event_kind: 'mcp_tool_call', outcome: 'success' },
  { timestamp: SECOND - 30, tool_name: 'tracedecay_read', event_kind: 'mcp_tool_call', outcome: 'error' },
  { timestamp: SECOND - 90, tool_name: 'Bash', event_kind: 'tool_call', outcome: 'timed_out' },
];

function attempts(): WorkResult<WorkGraphReadV1> {
  return {
    outcome: 'value',
    value: WorkGraphReadV1Schema.parse(
      workGraphRead({
        tasks: [{ taskId: 'task.alpha' }, { taskId: 'task.beta' }],
        observedAt: AT,
        runtimeAttempts: [
          { attemptId: 'a1', taskId: 'task.alpha', runId: 'r1', state: 'succeeded' },
          { attemptId: 'a2', taskId: 'task.alpha', runId: 'r1', state: 'failed' },
          { attemptId: 'a3', taskId: 'task.beta', runId: 'r2', state: 'timed_out' },
          { attemptId: 'a4', taskId: 'task.beta', runId: 'r2', state: 'running' },
        ],
      }),
    ),
  };
}

function renderContext(reading: AttemptFailureReading) {
  return render(
    <AgentFailureContext outcomes={OUTCOMES} recentEvents={EVENTS} attempts={reading} />,
  );
}

describe('AgentFailureContext', () => {
  it('accounts for the window outcomes against the population they describe', () => {
    renderContext(readAttemptFailures(attempts()));
    // 800 of 10,000, and the denominator is the accounted set rather than the
    // window's own count — which the fold measures separately.
    expect(screen.getByText(/800/)).toBeTruthy();
    expect(screen.getByText(/8\.00%/)).toBeTruthy();
    expect(
      screen.getByText(/is not necessarily the whole window/),
    ).toBeTruthy();
  });

  it('reports an outcome word it does not classify instead of reading it as success', () => {
    render(
      <AgentFailureContext
        outcomes={[
          { outcome: 'success', count: 90 },
          { outcome: 'quarantined', count: 10 },
        ]}
        recentEvents={EVENTS}
        attempts={readAttemptFailures(attempts())}
      />,
    );

    expect(document.querySelector('[data-agent-outcomes-unclassified="1"]')).toBeTruthy();
    expect(screen.getByText(/quarantined/)).toBeTruthy();
    expect(screen.getByText(/rather than being read as success/)).toBeTruthy();
  });

  it('reads the failures off the served tape and says what the tape is', () => {
    renderContext(readAttemptFailures(attempts()));
    const tape = document.querySelector('[data-agent-failure-tape="2"]')!;
    expect(within(tape as HTMLElement).getByText('tracedecay_read')).toBeTruthy();
    expect(within(tape as HTMLElement).getByText('Bash')).toBeTruthy();
    expect(within(tape as HTMLElement).queryByText('tracedecay_grep')).toBeNull();
    expect(screen.getByText(/nothing here explains why any of them failed/)).toBeTruthy();
  });

  it('keeps a clean tape distinct from no tape at all', () => {
    const clean = render(
      <AgentFailureContext
        outcomes={OUTCOMES}
        recentEvents={[EVENTS[0]!]}
        attempts={readAttemptFailures(attempts())}
      />,
    );
    expect(document.querySelector('[data-agent-failure-tape="clean"]')).toBeTruthy();
    expect(screen.getByText(/None of the 1 events on the served tape failed/)).toBeTruthy();
    clean.unmount();

    render(
      <AgentFailureContext
        outcomes={OUTCOMES}
        recentEvents={[]}
        attempts={readAttemptFailures(attempts())}
      />,
    );
    expect(document.querySelector('[data-agent-failure-tape="none"]')).toBeTruthy();
    expect(screen.getByText(/no tape to read failures off/)).toBeTruthy();
  });

  it('names the attempts that did not come out clean', () => {
    renderContext(readAttemptFailures(attempts()));
    const panel = screen.getByRole('region', { name: 'Attempt failures' });
    expect(panel.getAttribute('data-agent-attempt-failures')).toBe('2');
    // The sentence is broken across `td-value` spans, so it is read off the
    // paragraph's own text rather than matched as one text node.
    expect(panel.textContent).toContain('2 of 4 observed attempts on graph version 4');
    expect(document.querySelector('[data-agent-attempt-state="failed"]')).toBeTruthy();
    expect(document.querySelector('[data-agent-attempt-state="timed_out"]')).toBeTruthy();
    expect(within(panel).getByText(/cannot say why/)).toBeTruthy();
  });

  it('turns every attempt count into a floor when runtime coverage is partial', () => {
    renderContext(
      readAttemptFailures({
        outcome: 'value',
        value: WorkGraphReadV1Schema.parse(
          workGraphRead({
            tasks: [{ taskId: 'task.alpha' }],
            observedAt: AT,
            runtimeAttempts: [
              { attemptId: 'a1', taskId: 'task.alpha', runId: 'r1', state: 'failed' },
            ],
            runtimeCoverage: {
              coverage: 'partial',
              unavailable_attempts: [
                { attempt_id: 'a2', run_id: 'r1', task_id: 'task.alpha' },
              ],
            },
          }),
        ),
      }),
    );

    expect(document.querySelector('[data-agent-attempt-coverage="partial"]')).toBeTruthy();
    expect(screen.getByText(/1 further attempt was/)).toBeTruthy();
    expect(screen.getByText(/is therefore a floor/)).toBeTruthy();
  });

  it('never renders unavailable runtime coverage as zero failed attempts', () => {
    renderContext(
      readAttemptFailures({
        outcome: 'value',
        value: WorkGraphReadV1Schema.parse(
          workGraphRead({
            tasks: [{ taskId: 'task.alpha' }],
            observedAt: AT,
            runtimeAttempts: [],
            runtimeCoverage: { coverage: 'unavailable' },
          }),
        ),
      }),
    );

    expect(document.querySelector('[data-agent-attempt-failures="unavailable"]')).toBeTruthy();
    expect(screen.getByText('Source unavailable')).toBeTruthy();
    expect(screen.getByText(/no failure count is drawn from it/)).toBeTruthy();
    const panel = screen.getByRole('region', { name: 'Attempt failures' });
    expect(within(panel).queryByText(/observed attempts on graph version/)).toBeNull();
  });

  it('never renders a refused attempt read as zero failed attempts', () => {
    renderContext(
      readAttemptFailures({
        outcome: 'refused',
        state: 'denied',
        detail: 'not found, or not authorized for this actor',
      }),
    );

    expect(document.querySelector('[data-agent-attempt-failures="refused"]')).toBeTruthy();
    expect(screen.getByText('Denied')).toBeTruthy();
    expect(screen.getByText(/there is nothing to report as zero/)).toBeTruthy();
    // The analytics half of the panel is untouched: one authority refusing does
    // not blank the other.
    expect(screen.getByText(/8\.00%/)).toBeTruthy();
  });

  it('reports a pending attempt read as pending', () => {
    renderContext(readAttemptFailures(undefined));
    const panel = screen.getByRole('region', { name: 'Attempt failures' });
    expect(within(panel).getByText('Loading')).toBeTruthy();
    expect(panel.getAttribute('data-agent-attempt-failures')).toBeNull();
  });
});
