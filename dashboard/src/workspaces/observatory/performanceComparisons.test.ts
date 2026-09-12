import { describe, expect, it } from 'vitest';
import { dispositionPresentation } from './performanceComparisons.ts';

describe('dispositionPresentation', () => {
  it('gives insufficient evidence its own state, distinct from reject', () => {
    const insufficient = dispositionPresentation('insufficient_evidence');
    const reject = dispositionPresentation('reject');
    expect(insufficient.state).toBe('unknown');
    expect(reject.state).toBe('denied');
    expect(insufficient.state).not.toBe(reject.state);
    expect(insufficient.label).not.toBe(reject.label);
  });
});
