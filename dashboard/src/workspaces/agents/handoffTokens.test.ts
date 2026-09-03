import { describe, expect, it } from 'vitest';
import {
  ListTaskHandoffsResultV1Schema,
  type ListTaskHandoffsResultV1,
} from '../../contracts/generated.ts';
import type { WorkResult } from '../work/workApi.ts';
import {
  HANDOFF_LIST_TASK_ROUTE,
  handoffTargetLabel,
  readHandoffTokens,
} from './handoffTokens.ts';

const DIGEST = `sha256:${'a'.repeat(64)}`;

function token(overrides: Record<string, unknown>) {
  return {
    token_digest: DIGEST,
    issued_request_id: 'request.issue',
    session_id: 'lsp-session.task',
    kind: 'task',
    target: {
      kind: 'task',
      task_id: 'task.alpha',
      version: 9,
      owner_version_digest: `sha256:${'b'.repeat(64)}`,
    },
    issued_at: 1_000_000,
    expires_at: 61_000_000,
    state: 'open',
    consumed_at: null,
    ...overrides,
  };
}

/** Parsed through the generated contract, so no test can pass against a
 * payload the daemon could not send. */
function landed(handoffs: Record<string, unknown>[]): WorkResult<ListTaskHandoffsResultV1> {
  const counted = (state: string) => handoffs.filter((entry) => entry.state === state).length;
  return {
    outcome: 'value',
    value: ListTaskHandoffsResultV1Schema.parse({
      observed_at: 2_000_000,
      handoffs,
      open_count: counted('open'),
      consumed_count: counted('consumed'),
      expired_count: counted('expired'),
      truncated: false,
    }),
  };
}

describe('HANDOFF_LIST_TASK_ROUTE', () => {
  it('names the operation and path the daemon actually mounts', () => {
    expect(HANDOFF_LIST_TASK_ROUTE.operation).toBe('operation.handoff.list_task_handoffs');
    expect(HANDOFF_LIST_TASK_ROUTE.path).toBe('/api/application/handoff/list-task');
  });

  it('accepts a bare session id and refuses a request carrying a bearer', () => {
    expect(HANDOFF_LIST_TASK_ROUTE.request.safeParse({ session_id: 's' }).success).toBe(true);
    // The contract is `.strict()`: a client cannot smuggle a token onto this
    // request even by accident.
    expect(
      HANDOFF_LIST_TASK_ROUTE.request.safeParse({ session_id: 's', token: 'secret' }).success,
    ).toBe(false);
  });
});

describe('readHandoffTokens', () => {
  it('keeps an unasked question apart from an empty frontier', () => {
    const reading = readHandoffTokens(null, undefined);
    expect(reading.state).toBe('unasked');
    expect(reading.state === 'unasked' && reading.detail).toMatch(/not an empty frontier/i);
  });

  it('keeps a read still in flight apart from an empty frontier', () => {
    expect(readHandoffTokens('lsp-session.task', undefined).state).toBe('pending');
  });

  it('carries a refusal through with its own state and sentence', () => {
    const reading = readHandoffTokens('lsp-session.task', {
      outcome: 'refused',
      state: 'unavailable',
      detail: 'the daemon is not reachable',
    });
    expect(reading.state).toBe('refused');
    expect(reading.state === 'refused' && reading.chip).toBe('unavailable');
  });

  it('separates outstanding, lapsed, and redeemed tokens', () => {
    const reading = readHandoffTokens(
      'lsp-session.task',
      landed([
        token({ state: 'open', token_digest: `sha256:${'1'.repeat(64)}` }),
        token({ state: 'expired', token_digest: `sha256:${'2'.repeat(64)}` }),
        token({
          state: 'consumed',
          token_digest: `sha256:${'3'.repeat(64)}`,
          consumed_at: 1_500_000,
        }),
      ]),
    );

    expect(reading.state).toBe('read');
    if (reading.state !== 'read') return;
    expect(reading.outstanding).toHaveLength(1);
    // A lapsed token is a DROPPED handoff and must not be folded in with the
    // redeemed ones, which would hide it behind someone else's success.
    expect(reading.lapsed).toHaveLength(1);
    expect(reading.redeemed).toHaveLength(1);
    expect(reading.observedAtMicros).toBe(2_000_000);
  });

  it('reads an empty frontier as read, not as pending or refused', () => {
    const reading = readHandoffTokens('lsp-session.task', landed([]));
    expect(reading.state).toBe('read');
    if (reading.state !== 'read') return;
    expect(reading.outstanding).toEqual([]);
    expect(reading.lapsed).toEqual([]);
  });

  it('never surfaces a bearer, because the contract carries none', () => {
    const reading = readHandoffTokens('lsp-session.task', landed([token({})]));
    if (reading.state !== 'read') throw new Error('expected a read');
    const rendered = JSON.stringify(reading);
    expect(rendered).toContain('token_digest');
    expect(rendered).not.toMatch(/"token"\s*:/);
    expect(rendered).not.toMatch(/secret/i);
  });
});

describe('handoffTargetLabel', () => {
  it('names a task target by id and version', () => {
    expect(handoffTargetLabel(ListTaskHandoffsResultV1Schema.parse({
      observed_at: 1,
      handoffs: [token({})],
      open_count: 1,
      consumed_count: 0,
      expired_count: 0,
      truncated: false,
    }).handoffs[0]!)).toBe('task task.alpha @ v9');
  });

  it('names an investigation target by finding', () => {
    const parsed = ListTaskHandoffsResultV1Schema.parse({
      observed_at: 1,
      handoffs: [
        token({
          kind: 'investigation',
          target: {
            kind: 'investigation',
            finding_id: 'finding.beta',
            owner_version_digest: `sha256:${'c'.repeat(64)}`,
          },
        }),
      ],
      open_count: 1,
      consumed_count: 0,
      expired_count: 0,
      truncated: false,
    });
    expect(handoffTargetLabel(parsed.handoffs[0]!)).toBe('finding finding.beta');
  });
});
