import { describe, expect, it } from 'vitest';

import { payloadReadState } from './ReadSection.tsx';

describe('payloadReadState', () => {
  it.each(['offline', 'unauthorized', 'denied', 'unsupported_schema'] as const)(
    'preserves the %s outcome as the blocked domain state',
    (outcome) => {
      expect(payloadReadState(false, { outcome })).toEqual({ kind: 'blocked', state: outcome });
    },
  );

  it('preserves an error detail', () => {
    expect(payloadReadState(false, { outcome: 'error', detail: 'HTTP 500' })).toEqual({
      kind: 'blocked',
      state: 'error',
      detail: 'HTTP 500',
    });
  });

  it('preserves a typed unavailable payload and its reason', () => {
    const data = { status: 'missing_registry' };
    expect(
      payloadReadState(false, {
        outcome: 'unavailable',
        httpStatus: 503,
        status: data.status,
        reason: 'registry is not configured',
        data,
      }),
    ).toEqual({
      kind: 'blocked',
      state: 'unavailable',
      detail: 'registry is not configured',
      payload: data,
    });
  });

  it('keeps loading and unknown distinct from transport outcomes', () => {
    expect(payloadReadState(true, undefined)).toEqual({ kind: 'blocked', state: 'loading' });
    expect(payloadReadState(false, undefined)).toEqual({ kind: 'blocked', state: 'unknown' });
  });
});
