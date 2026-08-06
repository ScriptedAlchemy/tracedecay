/**
 * The canonical-refresh transaction, driven end to end by the render layer.
 *
 * `coalescing.dom.test.tsx` proves the render ceiling and that one overflow
 * produces one invalidation. These cases prove the other half: what happens to
 * signals and events raised *while* that one invalidation is still in flight.
 * A whole-projection refresh settles only after every active query refetches,
 * which routinely outlasts several 100 ms ticks, so the window is wide and
 * everything that lands in it has to survive.
 *
 * The window is held open explicitly — `invalidateQueries` is replaced by a
 * promise this file settles by hand — so no case depends on a wall clock. The
 * SSE clock is vitest's fake timer, advanced by exactly one tick period.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, render } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { SseConnection } from './connect.ts';
import { MAX_QUEUED_EVENTS, type SseReducerStats } from './types.ts';
import { EventsProvider, useEventsConnection } from './useEvents.tsx';

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

/** The render clock's period in `useEvents.tsx`: one tick, one batch boundary. */
const RENDER_TICK_MS = 100;

/** Generation is the trailing segment of `run_id` (see `connect.ts`). */
const RUN_ID = 'run-1-1700000000000000';
const RECONNECTED_RUN_ID = 'run-2-1700000000000001';

/** A family the batch mapper routes to no targeted key: pure gap/overflow fuel. */
const NEUTRAL_FAMILY = 'code_index_invalidated';

function frame(stream: string, family: string, revision: number, runId = RUN_ID) {
  return {
    stream,
    run_id: runId,
    event_revision: revision,
    entity_revision: revision,
    scope: {
      project_id: 'project.alpha',
      storage_mode: 'profile_sharded',
      store_root: '/stores/project.alpha',
    },
    observation_time_micros: 1_700_000_000_000_000 + revision,
    source_watermark: null,
    coverage: { completeness: 'complete', denominator: 1 },
    kind: { family },
  };
}

/**
 * `invalidateQueries` under this file's control: every call parks until a case
 * resolves or rejects it, which is what keeps a refresh "in flight" for as long
 * as the case needs without any real elapsed time.
 */
function controlledInvalidation(client: QueryClient) {
  const parked: Array<{ resolve: () => void; reject: (reason: unknown) => void }> = [];
  const spy = vi.spyOn(client, 'invalidateQueries').mockImplementation(
    () =>
      new Promise<void>((resolve, reject) => {
        parked.push({ resolve: () => resolve(), reject });
      }),
  );
  return {
    spy,
    resolveAll: () => {
      for (const refresh of parked.splice(0)) refresh.resolve();
    },
    rejectAll: (reason: unknown) => {
      for (const refresh of parked.splice(0)) refresh.reject(reason);
    },
  };
}

/**
 * Settling a refresh starts a short microtask chain: the invalidation settles,
 * the transaction commits or aborts, and any follow-up is issued. Draining a
 * fixed handful of hops inside `act` runs that chain to quiescence
 * deterministically, with no timer and no sleep.
 */
async function settleRefreshes(action: () => void): Promise<void> {
  await act(async () => {
    action();
    for (let hop = 0; hop < 8; hop += 1) await Promise.resolve();
  });
}

let connection: SseConnection | null = null;

function Probe() {
  connection = useEventsConnection();
  return null;
}

function reducerStats(): SseReducerStats {
  if (connection === null) throw new Error('EventsProvider published no connection');
  return connection.reducer.stats();
}

function newClient() {
  return new QueryClient({ defaultOptions: { queries: { retry: false } } });
}

function mount(client: QueryClient) {
  return render(
    <QueryClientProvider client={client}>
      <EventsProvider url="/api/events">
        <Probe />
      </EventsProvider>
    </QueryClientProvider>,
  );
}

/** Emit one tick's worth of frames and let the coalescing clock fire once. */
async function tick(emit: () => void): Promise<void> {
  await act(async () => {
    emit();
    vi.advanceTimersByTime(RENDER_TICK_MS);
  });
}

/** Open a canonical refresh with a revision gap and leave it in flight. */
async function openRefreshWithGap(source: FakeEventSource): Promise<void> {
  await tick(() => {
    source.emit('code_index_activity', frame('code_index_activity', NEUTRAL_FAMILY, 1));
    source.emit('code_index_activity', frame('code_index_activity', NEUTRAL_FAMILY, 5));
  });
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal('EventSource', FakeEventSource);
  FakeEventSource.instances = [];
  connection = null;
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  FakeEventSource.instances = [];
});

describe('SSE canonical refresh — events during the window', () => {
  it('invalidates the targeted key of an event that lands mid-refresh', async () => {
    const client = newClient();
    const refresh = controlledInvalidation(client);
    mount(client);
    const source = FakeEventSource.instances[0]!;

    await openRefreshWithGap(source);
    expect(refresh.spy).toHaveBeenCalledTimes(1);
    expect(refresh.spy).toHaveBeenCalledWith();

    // A registry change lands inside the window, in a batch that also carries a
    // canonical flag of its own. Draining that batch and then discarding it
    // threw away an invalidation the event had already earned.
    await tick(() => {
      source.emit('project_registry', frame('project_registry', 'project_registry_changed', 1));
      source.emit('project_registry', frame('project_registry', 'project_registry_changed', 5));
    });

    expect(refresh.spy).toHaveBeenCalledTimes(2);
    expect(refresh.spy).toHaveBeenLastCalledWith({ queryKey: ['projects'] });
  });
});

describe('SSE canonical refresh — signals during the window', () => {
  it('serves a gap raised mid-refresh with exactly one follow-up', async () => {
    const client = newClient();
    const refresh = controlledInvalidation(client);
    mount(client);
    const source = FakeEventSource.instances[0]!;

    await openRefreshWithGap(source);
    expect(refresh.spy).toHaveBeenCalledTimes(1);

    // A second gap, inside the window. The drain clears the batch's refetch
    // flag, so this signal only survives if the reducer remembers it as an
    // epoch the in-flight refresh cannot claim to have covered.
    await tick(() => {
      source.emit('code_index_activity', frame('code_index_activity', NEUTRAL_FAMILY, 20));
    });
    expect(refresh.spy).toHaveBeenCalledTimes(1);

    // The refresh settles. It was issued before the gap, so it commits nothing
    // and one follow-up runs.
    await settleRefreshes(() => refresh.resolveAll());
    expect(refresh.spy).toHaveBeenCalledTimes(2);
    expect(refresh.spy).toHaveBeenLastCalledWith();

    // One follow-up, not a storm: contiguous traffic keeps the clock ticking
    // through the follow-up's own window without adding a third refresh, and
    // its commit ends the chain because no newer signal arrived.
    for (let revision = 21; revision <= 25; revision += 1) {
      await tick(() => {
        source.emit('code_index_activity', frame('code_index_activity', NEUTRAL_FAMILY, revision));
      });
    }
    expect(refresh.spy).toHaveBeenCalledTimes(2);

    await settleRefreshes(() => refresh.resolveAll());
    for (let revision = 26; revision <= 30; revision += 1) {
      await tick(() => {
        source.emit('code_index_activity', frame('code_index_activity', NEUTRAL_FAMILY, revision));
      });
    }
    expect(refresh.spy).toHaveBeenCalledTimes(2);
    expect(reducerStats().canonicalRefreshOutstanding).toBe(false);
  });

  it('does not forgive an overflow raised mid-refresh', async () => {
    const client = newClient();
    const refresh = controlledInvalidation(client);
    mount(client);
    const source = FakeEventSource.instances[0]!;

    await openRefreshWithGap(source);
    expect(refresh.spy).toHaveBeenCalledTimes(1);
    expect(reducerStats().stale).toBe(false);

    // The queue overflows while that refresh is still in flight.
    await tick(() => {
      for (let revision = 6; revision <= 6 + MAX_QUEUED_EVENTS; revision += 1) {
        source.emit('code_index_activity', frame('code_index_activity', NEUTRAL_FAMILY, revision));
      }
    });
    expect(reducerStats().stale).toBe(true);
    expect(refresh.spy).toHaveBeenCalledTimes(1);

    // Committing a refresh that was issued before the overflow must not clear
    // the staleness the overflow raised: those events are still missing.
    await settleRefreshes(() => refresh.resolveAll());
    expect(reducerStats().stale).toBe(true);
    expect(refresh.spy).toHaveBeenCalledTimes(2);

    // Only the refresh that actually covers the overflow clears it.
    await settleRefreshes(() => refresh.resolveAll());
    expect(reducerStats().stale).toBe(false);
  });
});

describe('SSE canonical refresh — failure is not success', () => {
  it('retains stale, the typed failure, and the watermarks when it rejects', async () => {
    const client = newClient();
    const refresh = controlledInvalidation(client);
    mount(client);
    const source = FakeEventSource.instances[0]!;

    await tick(() => {
      for (let revision = 1; revision <= MAX_QUEUED_EVENTS + 1; revision += 1) {
        source.emit('code_index_activity', frame('code_index_activity', NEUTRAL_FAMILY, revision));
      }
    });
    expect(refresh.spy).toHaveBeenCalledTimes(1);
    expect(reducerStats().lastEventRevision).toBe(MAX_QUEUED_EVENTS);

    await settleRefreshes(() => refresh.rejectAll(new Error('daemon unreachable')));

    const failed = reducerStats();
    expect(failed.stale).toBe(true);
    expect(failed.reseed).toEqual({
      phase: 'failed',
      epoch: 1,
      reason: 'daemon unreachable',
    });
    // A refresh that never happened superseded nothing, so the sequencing
    // memory it would have replaced is still there.
    expect(failed.lastEventRevision).toBe(MAX_QUEUED_EVENTS);
    // Nor is a failure a retry trigger: re-issuing the identical refresh every
    // tick is the storm the coalescing clock exists to prevent.
    await tick(() => {
      source.emit('code_index_activity', frame('code_index_activity', NEUTRAL_FAMILY, MAX_QUEUED_EVENTS + 2));
    });
    expect(refresh.spy).toHaveBeenCalledTimes(1);

    // A genuinely newer signal is a new attempt, though — here a reconnect.
    await tick(() => {
      source.emit('code_index_activity', frame('code_index_activity', NEUTRAL_FAMILY, 1, RECONNECTED_RUN_ID));
    });
    expect(refresh.spy).toHaveBeenCalledTimes(2);
    expect(refresh.spy).toHaveBeenLastCalledWith();
  });

  it('escalates a rejected targeted invalidation to the canonical path', async () => {
    const client = newClient();
    const refresh = controlledInvalidation(client);
    mount(client);
    const source = FakeEventSource.instances[0]!;

    await tick(() => {
      source.emit('project_registry', frame('project_registry', 'project_registry_changed', 1));
    });
    expect(refresh.spy).toHaveBeenCalledTimes(1);
    expect(refresh.spy).toHaveBeenCalledWith({ queryKey: ['projects'] });

    // The targeted refresh failed, so that slice is not fresh. Escalating keeps
    // the rejection observed instead of orphaning it.
    await settleRefreshes(() => refresh.rejectAll(new Error('offline')));
    expect(refresh.spy).toHaveBeenCalledTimes(2);
    expect(refresh.spy).toHaveBeenLastCalledWith();
    expect(reducerStats().refetchReason).toBe('invalidation_failed');
  });
});
