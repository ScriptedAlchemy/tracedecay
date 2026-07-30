/**
 * The legacy transport's refusal readings.
 *
 * The failure this file exists to catch is a refused write that reads as
 * something else: a generic `error` the reader can only retry, or — worse — a
 * successful empty payload. The project gateway answers a write against a
 * non-active project with 405 and a body naming the cause, and that is the one
 * refusal a control can actually act on, so it gets its own outcome and its
 * own tests.
 *
 * Every case asserts the negative too. A malformed 405 must never produce a
 * `read_only_scope`, because that outcome asserts a specific cause and offers a
 * specific remedy; and no refusal, malformed or not, may ever surface as `ok`.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { z } from 'zod';

import { fetchLegacy, fetchLegacyWrite } from './legacy.ts';
import { READ_ONLY_SCOPE_STATUS } from '../scope/store.ts';

const PayloadSchema = z.object({ status: z.string() });

const REFUSAL_DETAIL = 'project-scoped dashboard APIs are read-only for non-active projects';

/** The gateway's 405, verbatim from `project_scoped_api_gateway`. */
const readOnlyBody = {
  status: READ_ONLY_SCOPE_STATUS,
  detail: REFUSAL_DETAIL,
  project_id: 'proj_b',
};

function stub(status: number, body: unknown, options: { invalidJson?: boolean } = {}): void {
  vi.stubGlobal(
    'fetch',
    vi.fn(
      async () =>
        new Response(options.invalidJson === true ? 'not json' : JSON.stringify(body), {
          status,
          headers: { 'content-type': 'application/json' },
        }),
    ),
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('fetchLegacyWrite', () => {
  it('reports the gateway read-only refusal as its own outcome, carrying the daemon reason', async () => {
    stub(405, readOnlyBody);
    const result = await fetchLegacyWrite('/api/projects/proj_b/automation/scheduler/pause', PayloadSchema, {
      method: 'POST',
    });
    expect(result.outcome).toBe('read_only_scope');
    if (result.outcome !== 'read_only_scope') throw new Error('unreachable');
    expect(result.refusal).toEqual({ projectId: 'proj_b', detail: REFUSAL_DETAIL });
  });

  // A 405 the dashboard cannot account for. It was refused — that much is
  // known — but the read-only explanation is not this dashboard's to assume.
  it.each([
    ['a different status', { status: 'not_found', detail: 'gone', project_id: 'proj_b' }],
    ['no status field', { detail: REFUSAL_DETAIL, project_id: 'proj_b' }],
    ['no project id', { status: READ_ONLY_SCOPE_STATUS, detail: REFUSAL_DETAIL }],
    ['no detail', { status: READ_ONLY_SCOPE_STATUS, project_id: 'proj_b' }],
    ['an empty object', {}],
    ['an array', [readOnlyBody]],
    ['a bare string', 'read_only_project'],
    ['null', null],
  ])('reports a 405 with %s as a plain error, never a read-only scope', async (_name, body) => {
    stub(405, body);
    const result = await fetchLegacyWrite('/api/projects/proj_b/x', PayloadSchema, {
      method: 'POST',
    });
    expect(result.outcome).toBe('error');
    if (result.outcome !== 'error') throw new Error('unreachable');
    expect(result.detail).toBe('HTTP 405');
  });

  it('reports a 405 whose body is not JSON at all as a plain error', async () => {
    stub(405, null, { invalidJson: true });
    const result = await fetchLegacyWrite('/api/projects/proj_b/x', PayloadSchema, {
      method: 'POST',
    });
    expect(result).toEqual({ outcome: 'error', detail: 'HTTP 405' });
  });

  it('keeps the authorization refusals distinct from the scope refusal', async () => {
    stub(401, {});
    expect((await fetchLegacyWrite('/api/x', PayloadSchema, { method: 'POST' })).outcome).toBe(
      'unauthorized',
    );
    stub(403, {});
    expect((await fetchLegacyWrite('/api/x', PayloadSchema, { method: 'POST' })).outcome).toBe(
      'denied',
    );
  });

  it('still decodes a successful write body', async () => {
    stub(200, { status: 'paused' });
    const result = await fetchLegacyWrite('/api/x', PayloadSchema, { method: 'POST' });
    expect(result).toEqual({ outcome: 'ok', data: { status: 'paused' } });
  });
});

describe('fetchLegacy', () => {
  it('folds the refusal into error carrying the daemon sentence, not a bare status', async () => {
    // The read result type has no arm for a refusal a read cannot provoke, so
    // a mutating caller on this helper gets `error`. It carries the daemon's
    // own sentence rather than `HTTP 405`, so even the folded reading says
    // what happened — but a control that needs to disable itself has to use
    // `fetchLegacyWrite` to get the outcome.
    stub(405, readOnlyBody);
    const result = await fetchLegacy('/api/projects/proj_b/x', PayloadSchema, { method: 'POST' });
    expect(result).toEqual({ outcome: 'error', detail: REFUSAL_DETAIL });
  });

  it('reports an undecodable success body as unsupported schema, never as empty', async () => {
    stub(200, null, { invalidJson: true });
    expect((await fetchLegacy('/api/x', PayloadSchema)).outcome).toBe('unsupported_schema');
  });

  it('reports a body that decoded to null as unsupported schema', async () => {
    // Distinct from the case above, and the reason the reader keeps a sentinel
    // for "not JSON": a literal `null` decodes fine and must still fail the
    // schema rather than being mistaken for a decode failure or for data.
    stub(200, null);
    expect((await fetchLegacy('/api/x', PayloadSchema)).outcome).toBe('unsupported_schema');
  });

  it('reports a network failure as offline', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new TypeError('network down');
      }),
    );
    expect((await fetchLegacy('/api/x', PayloadSchema)).outcome).toBe('offline');
  });
});
