import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { z } from 'zod';
import { fixtureEnvelope } from '../../test/fixtureEnvelope.ts';
import { responseWithBodyPendingUntilAbort } from '../../test/pendingBody.ts';
import { scopeKey, scopedUrl, useScope } from '../scope/store.ts';
import { fetchStructure, useStructure } from './structure.ts';

const ReadSchema = z.discriminatedUnion('status', [
  z.object({ status: z.literal('measured'), measurement: z.object({ hop_count: z.number() }) }),
  z.object({ status: z.literal('unmeasured'), reason: z.string(), detail: z.string() }),
  z.object({ status: z.literal('failed'), code: z.string(), detail: z.string(), retryable: z.boolean() }),
]);
const initialScope = useScope.getState().scope;
const clients: QueryClient[] = [];
const route = '/api/plugins/graph/strata';

function mountRead() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  clients.push(client);
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
  return {
    client,
    ...renderHook(() => useStructure(['structure', 'strata'], route, ReadSchema), { wrapper }),
  };
}

afterEach(() => {
  cleanup();
  for (const client of clients.splice(0)) client.clear();
  useScope.setState({ scope: initialScope });
  vi.unstubAllGlobals();
});

describe('structure query cancellation', () => {
  it('aborts the actual pending fetch on unmount without caching an offline result', async () => {
    const signals: Array<AbortSignal | null | undefined> = [];
    vi.stubGlobal('fetch', vi.fn((_url: string, init?: RequestInit) => {
      const signal = init?.signal;
      signals.push(signal);
      return new Promise<Response>((_resolve, reject) => {
        signal?.addEventListener('abort', () => reject(signal.reason), { once: true });
      });
    }));
    const { client, result, unmount } = mountRead();
    await waitFor(() => expect(signals).toHaveLength(1));
    expect(signals[0]).toBeInstanceOf(AbortSignal);
    expect(result.current.data).toBeUndefined();
    unmount();
    expect(signals[0]!.aborted).toBe(true);
    expect(client.getQueryData(['structure', 'strata', scopeKey(initialScope)])).toBeUndefined();
  });

  it('aborts an old scope body and publishes only the new project reading', async () => {
    useScope.getState().selectProject('proj_old', 'Old', 'selected');
    const oldScope = useScope.getState().scope;
    const signals: Array<AbortSignal | null | undefined> = [];
    const urls: string[] = [];
    let readingBody = false;
    vi.stubGlobal('fetch', vi.fn(async (url: string, init?: RequestInit) => {
      signals.push(init?.signal);
      urls.push(url);
      if (urls.length === 1) {
        const response = responseWithBodyPendingUntilAbort(200, init?.signal);
        const json = response.json.bind(response);
        vi.spyOn(response, 'json').mockImplementation(() => {
          readingBody = true;
          return json();
        });
        return response;
      }
      return new Response(JSON.stringify(fixtureEnvelope({
        status: 'measured', measurement: { hop_count: 7 },
      })), { status: 200 });
    }));
    const { client, result } = mountRead();
    await waitFor(() => expect(readingBody).toBe(true));
    expect(signals[0]).toBeInstanceOf(AbortSignal);
    act(() => useScope.getState().selectProject('proj_new', 'New', 'active'));
    await waitFor(() => expect(result.current.data).toEqual({
      outcome: 'measured', measurement: { hop_count: 7 },
    }));
    expect(signals[0]!.aborted).toBe(true);
    expect(urls).toEqual([
      scopedUrl(oldScope, route),
      scopedUrl(useScope.getState().scope, route),
    ]);
    expect(client.getQueryData(['structure', 'strata', scopeKey(oldScope)])).toBeUndefined();
    expect(result.current.error).toBeNull();
  });

  it('passes direct caller cancellation into response-body consumption', async () => {
    const controller = new AbortController();
    let observed: AbortSignal | null | undefined;
    vi.stubGlobal('fetch', vi.fn(async (_url: string, init?: RequestInit) => {
      observed = init?.signal;
      return responseWithBodyPendingUntilAbort(200, observed);
    }));
    const pending = fetchStructure(route, ReadSchema, { signal: controller.signal });
    const rejection = expect(pending).rejects.toThrow(/abort/i);
    controller.abort();
    await rejection;
    expect(observed).toBe(controller.signal);
  });
});
