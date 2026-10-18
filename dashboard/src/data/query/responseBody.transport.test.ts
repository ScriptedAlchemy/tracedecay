// @vitest-environment node
import { once } from 'node:events';
import { createServer, type RequestListener, type Server } from 'node:http';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { decodeJsonBody, fetchJsonResponse, UNDECODABLE } from './responseBody.ts';

const servers: Server[] = [];
async function serve(handler: RequestListener): Promise<string> {
  const server = createServer(handler);
  servers.push(server);
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const address = server.address();
  if (address === null || typeof address === 'string') throw new Error('missing loopback port');
  return `http://127.0.0.1:${address.port}`;
}

afterEach(async () => {
  vi.unstubAllGlobals();
  for (const server of servers.splice(0)) {
    server.closeAllConnections();
    await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  }
});

describe('shared JSON transport', () => {
  it('preserves method/body and supplies accept for every header representation', async () => {
    const base = await serve(async (request, response) => {
      let body = '';
      for await (const chunk of request) body += chunk;
      response.end(JSON.stringify({
        accept: request.headers.accept,
        custom: request.headers['x-test'],
        method: request.method,
        body,
      }));
    });
    const forms: HeadersInit[] = [
      { 'x-test': 'value' }, [['x-test', 'value']], new Headers({ 'x-test': 'value' }),
    ];
    for (const headers of forms) {
      const original = [...new Headers(headers)];
      const opened = await fetchJsonResponse(base, { headers, method: 'POST', body: 'input' });
      if (opened.outcome !== 'response') throw new Error(opened.outcome);
      expect(await decodeJsonBody(opened.response, undefined)).toEqual({
        accept: 'application/json', custom: 'value', method: 'POST', body: 'input',
      });
      expect([...new Headers(headers)]).toEqual(original);
    }
  });

  it('retains an explicitly selected accept header', async () => {
    const base = await serve((request, response) => response.end(JSON.stringify(request.headers.accept)));
    const opened = await fetchJsonResponse(base, { headers: { Accept: 'application/problem+json' } });
    if (opened.outcome !== 'response') throw new Error(opened.outcome);
    expect(await decodeJsonBody(opened.response, undefined)).toBe('application/problem+json');
  });

  it('distinguishes authentication refusals and preserves other response bodies', async () => {
    const base = await serve((request, response) => {
      response.writeHead(Number(request.url?.slice(1)), { 'content-type': 'application/json' });
      response.end('{"status":"unmeasured"}');
    });
    expect(await fetchJsonResponse(`${base}/401`)).toEqual({ outcome: 'unauthorized' });
    expect(await fetchJsonResponse(`${base}/403`)).toEqual({ outcome: 'denied' });
    const opened = await fetchJsonResponse(`${base}/503`);
    if (opened.outcome !== 'response') throw new Error(opened.outcome);
    expect(opened.response.status).toBe(503);
    expect(await decodeJsonBody(opened.response, undefined)).toEqual({ status: 'unmeasured' });
  });

  it('cancels a real pending request before headers', async () => {
    let arrive!: () => void;
    const ready = new Promise<void>(resolve => { arrive = resolve; });
    const base = await serve(() => arrive());
    const controller = new AbortController();
    const pending = fetchJsonResponse(base, { signal: controller.signal });
    const rejection = expect(pending).rejects.toMatchObject({ name: 'AbortError' });
    await ready;
    controller.abort();
    await rejection;
  });

  it('cancels a real response during JSON body consumption', async () => {
    const base = await serve((_request, response) => {
      response.writeHead(200, { 'content-type': 'application/json' });
      response.write('{"reading":');
    });
    const controller = new AbortController();
    const opened = await fetchJsonResponse(base, { signal: controller.signal });
    if (opened.outcome !== 'response') throw new Error(opened.outcome);
    const pending = decodeJsonBody(opened.response, controller.signal);
    const rejection = expect(pending).rejects.toMatchObject({ name: 'AbortError' });
    controller.abort();
    await rejection;
  });

  it('does not confuse malformed JSON with a genuine null', async () => {
    expect(await decodeJsonBody(new Response('<html/>'), undefined)).toBe(UNDECODABLE);
    expect(await decodeJsonBody(new Response('null'), undefined)).toBeNull();
  });

  it('keeps network failures and transport AbortErrors distinct', async () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new TypeError('unreachable')));
    expect(await fetchJsonResponse('/x')).toEqual({ outcome: 'offline' });
    const abort = new DOMException('transport cancelled', 'AbortError');
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(abort));
    await expect(fetchJsonResponse('/x')).rejects.toBe(abort);
  });

  it('preserves the original fetch rejection for an aborted caller signal', async () => {
    const controller = new AbortController();
    controller.abort();
    const abort = new Error('custom cancellation');
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(abort));
    await expect(fetchJsonResponse('/x', { signal: controller.signal })).rejects.toBe(abort);
  });
});
