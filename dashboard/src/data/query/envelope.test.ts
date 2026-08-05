/**
 * The envelope transport's refusal readings.
 *
 * `EnvelopeResult` is keyed by domain state rather than by a transport outcome,
 * so the gateway's read-only refusal is reported in that vocabulary: `locked`,
 * the taxonomy's word for a surface that will not accept a change. The failure
 * this file guards against is that refusal arriving as `error` — a state whose
 * only implied next action is retry, which will be refused again — or as a
 * decodable-but-absent envelope.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { z } from 'zod';

import { fetchEnvelope } from './envelope.ts';
import { READ_ONLY_SCOPE_STATUS } from '../scope/store.ts';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';

const PayloadSchema = z.object({ note: z.string() });

const REFUSAL_DETAIL = 'project-scoped dashboard APIs are read-only for non-active projects';

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
        new Response(options.invalidJson === true ? '<html/>' : JSON.stringify(body), {
          status,
          headers: { 'content-type': 'application/json' },
        }),
    ),
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('fetchEnvelope', () => {
  it('reports the gateway read-only refusal as locked, carrying the daemon reason', async () => {
    stub(405, readOnlyBody);
    const result = await fetchEnvelope('/api/projects/proj_b/doctor/remediations/apply', PayloadSchema, {
      method: 'POST',
    });
    expect(result).toEqual({ outcome: 'transport', state: 'locked', detail: REFUSAL_DETAIL });
  });

  // A 405 the dashboard cannot account for stays a plain error: `locked` would
  // tell the reader to change scope, and nothing here established that scope is
  // why the request was refused.
  it.each([
    ['a different status', { status: 'not_found', detail: 'gone', project_id: 'proj_b' }],
    ['no status field', { detail: REFUSAL_DETAIL, project_id: 'proj_b' }],
    ['no project id', { status: READ_ONLY_SCOPE_STATUS, detail: REFUSAL_DETAIL }],
    ['an empty object', {}],
    ['null', null],
  ])('reports a 405 with %s as an error, never locked', async (_name, body) => {
    stub(405, body);
    const result = await fetchEnvelope('/api/projects/proj_b/x', PayloadSchema, { method: 'POST' });
    expect(result).toEqual({ outcome: 'transport', state: 'error', detail: 'HTTP 405' });
  });

  it('reports a 405 whose body is not JSON at all as an error', async () => {
    stub(405, null, { invalidJson: true });
    const result = await fetchEnvelope('/api/projects/proj_b/x', PayloadSchema, { method: 'POST' });
    expect(result).toEqual({ outcome: 'transport', state: 'error', detail: 'HTTP 405' });
  });

  it('leaves every other non-2xx as an error naming its status', async () => {
    stub(500, {});
    expect(await fetchEnvelope('/api/x', PayloadSchema)).toEqual({
      outcome: 'transport',
      state: 'error',
      detail: 'HTTP 500',
    });
  });

  it('reports an undecodable envelope as unsupported schema, never as empty', async () => {
    stub(200, { payload: { note: 'hi' } });
    expect(await fetchEnvelope('/api/x', PayloadSchema)).toEqual({
      outcome: 'transport',
      state: 'unsupported_schema',
    });
  });

  it('preserves an error envelope with no payload as the daemon error state', async () => {
    stub(200, fixtureEnvelope(null, 'error'));

    expect(await fetchEnvelope('/api/x', PayloadSchema)).toMatchObject({
      outcome: 'transport',
      state: 'error',
    });
  });

  it('reports a network failure as offline', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new TypeError('network down');
      }),
    );
    expect(await fetchEnvelope('/api/x', PayloadSchema)).toEqual({
      outcome: 'transport',
      state: 'offline',
    });
  });

  it('propagates an aborted request instead of reporting the daemon offline', async () => {
    const controller = new AbortController();
    controller.abort();
    const abort = new DOMException('The operation was aborted', 'AbortError');
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw abort;
      }),
    );

    await expect(
      fetchEnvelope('/api/x', PayloadSchema, { signal: controller.signal }),
    ).rejects.toBe(abort);
  });
});
