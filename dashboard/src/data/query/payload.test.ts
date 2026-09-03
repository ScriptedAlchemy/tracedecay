/**
 * The typed payload transport's refusal readings.
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

import { fetchPayload, fetchPayloadWrite } from './payload.ts';
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

describe('fetchPayloadWrite', () => {
  it('reports the gateway read-only refusal as its own outcome, carrying the daemon reason', async () => {
    stub(405, readOnlyBody);
    const result = await fetchPayloadWrite('/api/projects/proj_b/automation/scheduler/pause', PayloadSchema, {
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
    const result = await fetchPayloadWrite('/api/projects/proj_b/x', PayloadSchema, {
      method: 'POST',
    });
    expect(result.outcome).toBe('error');
    if (result.outcome !== 'error') throw new Error('unreachable');
    expect(result.detail).toBe('HTTP 405');
  });

  it('reports a 405 whose body is not JSON at all as a plain error', async () => {
    stub(405, null, { invalidJson: true });
    const result = await fetchPayloadWrite('/api/projects/proj_b/x', PayloadSchema, {
      method: 'POST',
    });
    expect(result).toEqual({ outcome: 'error', detail: 'HTTP 405' });
  });

  it('keeps the authorization refusals distinct from the scope refusal', async () => {
    stub(401, {});
    expect((await fetchPayloadWrite('/api/x', PayloadSchema, { method: 'POST' })).outcome).toBe(
      'unauthorized',
    );
    stub(403, {});
    expect((await fetchPayloadWrite('/api/x', PayloadSchema, { method: 'POST' })).outcome).toBe(
      'denied',
    );
  });

  it('still decodes a successful write body', async () => {
    stub(200, { status: 'paused' });
    const result = await fetchPayloadWrite('/api/x', PayloadSchema, { method: 'POST' });
    expect(result).toEqual({ outcome: 'ok', data: { status: 'paused' } });
  });
});

describe('fetchPayload', () => {
  it('folds the refusal into error carrying the daemon sentence, not a bare status', async () => {
    // The read result type has no arm for a refusal a read cannot provoke, so
    // a mutating caller on this helper gets `error`. It carries the daemon's
    // own sentence rather than `HTTP 405`, so even the folded reading says
    // what happened — but a control that needs to disable itself has to use
    // `fetchPayloadWrite` to get the outcome.
    stub(405, readOnlyBody);
    const result = await fetchPayload('/api/projects/proj_b/x', PayloadSchema, { method: 'POST' });
    expect(result).toEqual({ outcome: 'error', detail: REFUSAL_DETAIL });
  });

  it('reports an undecodable success body as unsupported schema, never as empty', async () => {
    stub(200, null, { invalidJson: true });
    expect((await fetchPayload('/api/x', PayloadSchema)).outcome).toBe('unsupported_schema');
  });

  it('reports a body that decoded to null as unsupported schema', async () => {
    // Distinct from the case above, and the reason the reader keeps a sentinel
    // for "not JSON": a literal `null` decodes fine and must still fail the
    // schema rather than being mistaken for a decode failure or for data.
    stub(200, null);
    expect((await fetchPayload('/api/x', PayloadSchema)).outcome).toBe('unsupported_schema');
  });

  it('reports a network failure as offline', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new TypeError('network down');
      }),
    );
    expect((await fetchPayload('/api/x', PayloadSchema)).outcome).toBe('offline');
  });
});

/**
 * The statuses the canonical project routes actually answer with.
 *
 * Verbatim from `src/dashboard/projects.rs`: 503 for `missing_registry` and
 * `registry_unavailable`, 404 for `not_found`, each with the generated payload
 * in the body. Reading only the status code discarded that body, so three
 * conditions with three different remedies all arrived as `HTTP 503`/`HTTP
 * 404` — and every payload branch written to render them was unreachable.
 *
 * Stubbed at those statuses on purpose. A 200 fixture would exercise a shape
 * the daemon never sends and prove nothing about the path that was broken.
 */
describe('fetchPayload on the canonical failure statuses', () => {
  const RegistrySchema = z.object({
    status: z.string(),
    error: z.string().nullable().optional(),
    project: z.unknown().optional(),
  });

  it('carries the 404 not_found body instead of reporting HTTP 404', async () => {
    stub(404, { status: 'not_found', error: 'no project registered with id proj_ghost', project: null });
    expect(await fetchPayload('/api/projects/proj_ghost', RegistrySchema)).toEqual({
      outcome: 'unavailable',
      httpStatus: 404,
      status: 'not_found',
      reason: 'no project registered with id proj_ghost',
      data: { status: 'not_found', error: 'no project registered with id proj_ghost', project: null },
    });
  });

  it.each(['missing_registry', 'registry_unavailable'])(
    'carries the 503 %s body and its reason',
    async (status) => {
      stub(503, { status, error: 'registry database could not be opened' });
      const result = await fetchPayload('/api/projects', RegistrySchema);
      expect(result).toMatchObject({
        outcome: 'unavailable',
        httpStatus: 503,
        status,
        reason: 'registry database could not be opened',
      });
    },
  );

  it('reports no reason rather than an empty one when the payload sent none', async () => {
    stub(503, { status: 'registry_unavailable', error: '' });
    expect(await fetchPayload('/api/projects', RegistrySchema)).toMatchObject({
      outcome: 'unavailable',
      reason: null,
    });
  });

  it('leaves a 404 without a canonical status as a plain error', async () => {
    // An ordinary not-found from anywhere else in the stack, including a
    // proxy. Nothing named a condition, so nothing is reported as one — the
    // open record schemas here would otherwise accept any object at all.
    stub(404, { detail: 'no route' });
    expect(await fetchPayload('/api/x', z.record(z.string(), z.unknown()))).toEqual({
      outcome: 'error',
      detail: 'HTTP 404',
    });
  });

  it('leaves an unparseable 503 body as a plain error', async () => {
    stub(503, null, { invalidJson: true });
    expect(await fetchPayload('/api/projects', RegistrySchema)).toEqual({
      outcome: 'error',
      detail: 'HTTP 503',
    });
  });

  it('reports a named condition this build cannot read as a build mismatch', async () => {
    // The body says which condition it is, but the rest of it does not match
    // this build's contract. Reporting it as a typed payload would be a claim
    // about a shape that failed to validate.
    stub(503, { status: 'registry_unavailable', error: 7 });
    expect(await fetchPayload('/api/projects', z.object({ error: z.string() }))).toEqual({
      outcome: 'unsupported_schema',
    });
  });

  it('still reports 401 and 403 as refusals rather than conditions', async () => {
    stub(401, { status: 'not_found' });
    expect((await fetchPayload('/api/projects', RegistrySchema)).outcome).toBe('unauthorized');
    stub(403, { status: 'not_found' });
    expect((await fetchPayload('/api/projects', RegistrySchema)).outcome).toBe('denied');
  });

  it('still reports a 500 read failure as an error', async () => {
    // `graph_api.rs` answers 500 `read_failed`, which is not one of the two
    // admitted statuses. Unknown error behaviour is preserved.
    stub(500, { status: 'read_failed', error: 'failed to query counts' });
    expect(await fetchPayload('/api/plugins/graph/overview', RegistrySchema)).toEqual({
      outcome: 'error',
      detail: 'HTTP 500',
    });
  });
});

/**
 * Cancellation, which is not a reading.
 *
 * `fetch` rejects the same way whether the network failed or the caller
 * aborted, and the caller that aborts here is a scope change: selecting
 * another project abandons the previous project's in-flight reads. Folding
 * that into `offline` would mint a daemon-is-down state out of a request this
 * dashboard cancelled — and cache it against the abandoned scope, so returning
 * to that project would show a failure nobody ever received.
 */
describe('fetchPayload under cancellation', () => {
  /** Rejects only once aborted, like a real request in flight. */
  function stubPendingUntilAbort(): void {
    vi.stubGlobal(
      'fetch',
      vi.fn(
        (_url: string, init?: RequestInit) =>
          new Promise<Response>((_resolve, reject) => {
            const signal = init?.signal;
            if (!signal) return;
            signal.addEventListener('abort', () =>
              reject(new DOMException('The operation was aborted.', 'AbortError')),
            );
          }),
      ),
    );
  }

  it('rethrows an abort rather than reporting the daemon offline', async () => {
    stubPendingUntilAbort();
    const controller = new AbortController();
    const pending = fetchPayload('/api/projects/proj_a', PayloadSchema, {
      signal: controller.signal,
    });
    controller.abort();
    // The whole point: `offline` here would be a fabricated reading.
    await expect(pending).rejects.toThrow(/abort/i);
  });

  it('passes the caller signal to fetch, so an abandoned read is really cancelled', async () => {
    stubPendingUntilAbort();
    const controller = new AbortController();
    const pending = fetchPayload('/api/projects/proj_a', PayloadSchema, {
      signal: controller.signal,
    });
    const call = vi.mocked(fetch).mock.calls[0];
    expect((call?.[1] as RequestInit | undefined)?.signal).toBe(controller.signal);
    controller.abort();
    await expect(pending).rejects.toThrow();
  });

  it('still reports a genuine network failure as offline when nothing was aborted', async () => {
    // The guard keys on the signal, not on the error, so a real failure on a
    // request that merely *carries* a signal stays a truthful `offline`.
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new TypeError('network down');
      }),
    );
    const controller = new AbortController();
    const result = await fetchPayload('/api/x', PayloadSchema, { signal: controller.signal });
    expect(result.outcome).toBe('offline');
  });
});
