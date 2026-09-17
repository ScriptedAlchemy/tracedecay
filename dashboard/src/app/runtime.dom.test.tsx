import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, render, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useScope } from '../data/scope/store.ts';
import { ScopedEventsProvider } from './runtime.tsx';

type Listener = (event: MessageEvent<string>) => void;

class FakeEventSource {
  static readonly CLOSED = 2;
  static instances: FakeEventSource[] = [];

  readonly listeners = new Map<string, Listener[]>();
  readyState = 1;
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: Listener | null = null;

  constructor(readonly url: string) {
    FakeEventSource.instances.push(this);
  }

  addEventListener(name: string, listener: Listener) {
    const listeners = this.listeners.get(name) ?? [];
    listeners.push(listener);
    this.listeners.set(name, listeners);
  }

  emit(name: string, data: unknown) {
    const event = { data: JSON.stringify(data) } as MessageEvent<string>;
    for (const listener of this.listeners.get(name) ?? []) listener(event);
  }

  close() {
    this.readyState = FakeEventSource.CLOSED;
  }
}

function heartbeat(receipt: string) {
  return {
    stream: 'heartbeat',
    run_id: 'run-42-1700000000000000',
    event_revision: 1,
    entity_revision: null,
    scope: { project_id: 'proj-b', storage_mode: 'profile_sharded', store_root: '/s' },
    observation_time_micros: 1_700_000_000_000_000,
    source_watermark: null,
    coverage: { completeness: 'unknown' },
    delivery_receipt: receipt,
    kind: { family: 'heartbeat' },
  };
}

function mountScopedEvents() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <ScopedEventsProvider>
        <span>child</span>
      </ScopedEventsProvider>
    </QueryClientProvider>,
  );
}

describe('ScopedEventsProvider', () => {
  beforeEach(() => {
    FakeEventSource.instances = [];
    vi.stubGlobal('EventSource', FakeEventSource);
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(new Response(null, { status: 202 })),
    );
    useScope.setState({ scope: { kind: 'all' } });
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    useScope.setState({ scope: { kind: 'all' } });
  });

  it('connects the unscoped stream under all-projects and acks there', async () => {
    mountScopedEvents();
    expect(FakeEventSource.instances).toHaveLength(1);
    expect(FakeEventSource.instances[0]?.url).toBe('/api/events');

    const receipt = `dsa1:${'a'.repeat(64)}`;
    FakeEventSource.instances[0]?.emit('heartbeat', heartbeat(receipt));

    await waitFor(() => {
      expect(fetch).toHaveBeenCalledWith(
        '/api/events/delivery-ack',
        expect.objectContaining({ method: 'POST' }),
      );
    });
    const ackUrls = vi
      .mocked(fetch)
      .mock.calls.map(([input]) => String(input))
      .filter((url) => url.includes('delivery-ack'));
    expect(ackUrls).toEqual(['/api/events/delivery-ack']);
  });

  it('reconnects the stream and acks on the selected project gateway', async () => {
    mountScopedEvents();
    const unscoped = FakeEventSource.instances[0];
    expect(unscoped?.url).toBe('/api/events');

    act(() => {
      useScope.getState().selectProject('proj-b', 'Beta', 'selected');
    });

    const scoped = FakeEventSource.instances.at(-1);
    expect(scoped?.url).toBe('/api/projects/proj-b/events');
    expect(unscoped?.readyState).toBe(FakeEventSource.CLOSED);

    const receipt = `dsa1:${'b'.repeat(64)}`;
    scoped?.emit('heartbeat', heartbeat(receipt));

    await waitFor(() => {
      expect(fetch).toHaveBeenCalledWith(
        '/api/projects/proj-b/events/delivery-ack',
        expect.objectContaining({
          method: 'POST',
          body: JSON.stringify({ receipt }),
        }),
      );
    });
    expect(fetch).not.toHaveBeenCalledWith(
      '/api/events/delivery-ack',
      expect.anything(),
    );
  });
});
