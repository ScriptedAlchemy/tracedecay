import { afterEach, describe, expect, it, vi } from 'vitest';
import { mintBrowserIdempotencyKey } from './identity.ts';

const originalCrypto = globalThis.crypto;

afterEach(() => {
  Object.defineProperty(globalThis, 'crypto', {
    configurable: true,
    value: originalCrypto,
  });
  vi.restoreAllMocks();
});

describe('browser idempotency identity', () => {
  it('mints a distinct stable intent key for each logical effect', () => {
    const randomUUID = vi
      .fn()
      .mockReturnValueOnce('00000000-0000-4000-8000-000000000001')
      .mockReturnValueOnce('00000000-0000-4000-8000-000000000002');
    Object.defineProperty(globalThis, 'crypto', {
      configurable: true,
      value: { randomUUID },
    });

    const first = mintBrowserIdempotencyKey('dashboard-doctor');
    const second = mintBrowserIdempotencyKey('dashboard-doctor');

    expect(first).not.toBe(second);
    expect(first).toBe(
      'idempotency.dashboard-doctor.00000000-0000-4000-8000-000000000001',
    );
  });

  it('fails closed instead of reusing a same-millisecond fallback', () => {
    Object.defineProperty(globalThis, 'crypto', {
      configurable: true,
      value: undefined,
    });
    vi.spyOn(Date, 'now').mockReturnValue(1_000);

    expect(() => mintBrowserIdempotencyKey('dashboard-doctor')).toThrow(
      'secure browser idempotency identity is unavailable',
    );
  });
});
