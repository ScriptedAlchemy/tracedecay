/**
 * What the board is allowed to say about a task.
 *
 * `WorkProjection` carries no status field, so every stage here has to be a
 * reading of a field the contract really has. The test that matters most is the
 * last one: a stage must never be inferred from the absence of information.
 */
import { describe, expect, it } from 'vitest';
import type { WorkTaskView } from './workProductView.ts';

import { coverageReading, stageState, workStage, type WorkStage } from './workModel.ts';

function projection(overrides: Partial<WorkTaskView> = {}): WorkTaskView {
  return {
    accepted_proposal: null,
    acceptance_evidence_required: false,
    dependencies: [],
    execution_admitted: false,
    history_len: 1,
    relation_replan: null,
    task_accepted: false,
    task_id: 'task-1',
    title: 'A task',
    version: 1,
    ...overrides,
  };
}

describe('the stage a projection reads as', () => {
  it('reads each gate the contract records', () => {
    expect(workStage(projection())).toBe('proposal_open');
    expect(workStage(projection({ accepted_proposal: 'proposal-1' }))).toBe('proposal_accepted');
    expect(workStage(projection({ accepted_proposal: 'p', task_accepted: true }))).toBe(
      'task_accepted',
    );
    expect(
      workStage(projection({ accepted_proposal: 'p', task_accepted: true, execution_admitted: true })),
    ).toBe('execution_admitted');
  });

  /** Terminal evidence outranks admission: a finished task is finished even
   * though `execution_admitted` is still true underneath it. */
  it('reports terminal evidence as the furthest gate', () => {
    expect(
      workStage(
        projection({
          accepted_proposal: 'p',
          task_accepted: true,
          execution_admitted: true,
        }),
        true,
      ),
    ).toBe('evidence_terminal');
  });

  /** Non-terminal evidence is work in flight, not a result. */
  it('does not treat non-terminal evidence as a result', () => {
    expect(
      workStage(
        projection({ execution_admitted: true, task_accepted: true }),
        false,
      ),
    ).toBe('execution_admitted');
  });

  it('calls only a terminal task ready, and never calls a task complete-with-nothing', () => {
    const states = (['proposal_open', 'proposal_accepted', 'task_accepted', 'execution_admitted'] as const).map(
      stageState,
    );
    expect(new Set(states)).toEqual(new Set(['partial']));
    expect(stageState('evidence_terminal')).toBe('ready');
    for (const stage of [
      'proposal_open',
      'proposal_accepted',
      'task_accepted',
      'execution_admitted',
      'evidence_terminal',
    ] satisfies WorkStage[]) {
      expect(stageState(stage)).not.toBe('complete_zero_findings');
    }
  });
});

describe('how much of the board is being shown', () => {
  it('separates an empty complete board from a withheld one', () => {
    expect(coverageReading({ state: 'complete', returned: 0, total: 0 })).toMatchObject({
      state: 'complete_zero_findings',
    });
    expect(coverageReading({ state: 'complete', returned: 4, total: 4 })).toMatchObject({
      state: 'ready',
    });
  });
});
