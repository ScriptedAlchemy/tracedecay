import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { WorkAttemptListV1Schema, type WorkAttemptListV1 } from '../../contracts/index.ts';
import { workAttempt, workAttemptList } from '../../test/workAttemptFixture.ts';
import type { WorkResult } from './workApi.ts';
import { WorkTopologyAccounting } from './views/WorkTopologyAccounting.tsx';

describe('Work topology accounting', () => {
  it('renders a capped page’s full authorized denominator without promoting page counts to totals', () => {
    const attemptList: WorkResult<WorkAttemptListV1> = {
      outcome: 'value',
      value: WorkAttemptListV1Schema.parse(
        workAttemptList([workAttempt({ taskId: 'alpha', runId: 'run-1', attemptId: 'a-1' })], {
          coverage: 'capped',
          remaining: 41,
          resume: {
            generation: 'generation-7',
            start_after: { attempt_id: 'a-1', run_id: 'run-1', task_id: 'alpha' },
          },
          returned: 1,
        }),
      ),
    };

    render(<WorkTopologyAccounting attemptList={attemptList} graph={{ state: 'pending' }} />);

    const reruns = screen.getByRole('region', { name: 'Reruns' });
    expect(reruns.textContent).toContain('1 attempts');
    expect(reruns.textContent).toContain('42 attempts');
    expect(reruns.textContent).toContain('capped at 1 of 42 attempts');
    expect(reruns.textContent).toContain('every count below is a floor');
    expect(reruns.textContent).not.toContain('42 runtime reruns');
  });
});
