/**
 * The render-coalescing ceiling from the plan's performance envelope
 * (docs/plans/tracedecay-v2/11-dashboard-frontend.md):
 *
 *   "sustain 100 SSE events/s for ten minutes and 1,000/s for ten seconds,
 *    coalesce to at most ten renders/s/view ... Overflow marks the projection
 *    stale and performs one canonical invalidation/refetch."
 *
 * `throughput.test.ts` proves the reducer's side of that arithmetic. This file
 * proves the part only React can answer: how many times a *view* function body
 * actually runs while the stream is saturated.
 *
 * What "renders/s/view" counts here: every invocation of a component function
 * that subscribes to the live event stream through `useLiveActivity` and
 * `useEventStreamState` — the exact pair `BrainPage` uses. React re-invokes it
 * whenever a subscribed external store notifies with a changed snapshot, so
 * counting invocations counts renders.
 *
 * Why each frame is delivered inside its own `act()`: a browser hands every SSE
 * frame to the page as a separate task, so React cannot auto-batch across them.
 * Emitting a whole second's frames inside one `act()` would collapse them into
 * a single render and the ceiling would pass vacuously. One `act()` per frame,
 * with fake timers advanced between frames, models the real arrival pattern —
 * and is still deterministic, because the clock is simulated end to end.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, render } from '@testing-library/react';
import type { ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { MAX_QUEUED_EVENTS } from './types.ts';
import { EventsProvider, useEventStreamState, useLiveActivity } from './useEvents.tsx';

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

/**
 * The render clock's period in `useEvents.tsx`. Advancing by exactly this much
 * fires exactly one coalesced tick, which is what ties the render count in
 * these cases to the plan's ten-per-second ceiling.
 */
const RENDER_TICK_MS = 100;

function deferred(): { promise: Promise<void>; resolve: () => void } {
  let release = () => {};
  const promise = new Promise<void>((resolveWith) => {
    release = () => resolveWith();
  });
  return { promise, resolve: () => release() };
}

function frame(revision: number) {
  return {
    stream: 'code_index_activity',
    run_id: 'run-1-1700000000000000',
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
    kind: { family: 'code_index_activity', files: 1 },
  };
}

/** A family the batch mapper routes to a targeted query key. */
function registryFrame() {
  return {
    ...frame(1),
    stream: 'project_registry',
    kind: { family: 'project_registry_changed', project_count: 3 },
  };
}

/** Render counter for the view under test. Reset per case. */
let renders = 0;

/** A view shaped like `BrainPage`: both live-stream hooks, one subscription. */
function LiveView() {
  renders += 1;
  const { revision } = useLiveActivity();
  const { state } = useEventStreamState();
  return (
    <span data-testid="live">
      {state}:{revision}
    </span>
  );
}

function mount(children: ReactNode, client: QueryClient) {
  return render(
    <QueryClientProvider client={client}>
      <EventsProvider url="/api/events">{children}</EventsProvider>
    </QueryClientProvider>,
  );
}

function newClient() {
  return new QueryClient({ defaultOptions: { queries: { retry: false } } });
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal('EventSource', FakeEventSource);
  FakeEventSource.instances = [];
  renders = 0;
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
  FakeEventSource.instances = [];
});

describe('SSE render coalescing — at most ten renders/s/view', () => {
  const rates = [
    { label: '100 events/s', perSecond: 100 },
    { label: '1,000 events/s', perSecond: 1_000 },
  ] as const;

  it.each(rates)(
    'holds a subscribed view to ten renders across one simulated second at $label',
    ({ perSecond }) => {
      const client = newClient();
      const { getByTestId } = mount(<LiveView />, client);
      const source = FakeEventSource.instances[0]!;
      const rendersAtMount = renders;

      // One simulated second of frames, each in its own task, spaced evenly.
      const gapMs = 1_000 / perSecond;
      for (let revision = 1; revision <= perSecond; revision += 1) {
        act(() => {
          source.emit('code_index_activity', frame(revision));
          vi.advanceTimersByTime(gapMs);
        });
      }

      const rendersInSecond = renders - rendersAtMount;
      expect(rendersInSecond).toBeGreaterThan(0); // the view did update
      // Measured: exactly 10 at both rates — the coalescing clock is the only
      // thing setting this number, so the bound is tight, not slack.
      expect(rendersInSecond).toBeLessThanOrEqual(10);
      // Coalescing is not sampling: the view still ends the second showing the
      // newest revision, so every frame was accepted and none was lost.
      expect(getByTestId('live').textContent).toBe(`live:${perSecond}`);
    },
  );

  it('renders once per coalescing tick, not once per frame, over ten seconds', () => {
    const client = newClient();
    const { getByTestId } = mount(<LiveView />, client);
    const source = FakeEventSource.instances[0]!;
    const rendersAtMount = renders;

    // The plan's peak burst: 1,000 events/s for ten seconds.
    let revision = 0;
    for (let second = 0; second < 10; second += 1) {
      for (let i = 0; i < 1_000; i += 1) {
        revision += 1;
        act(() => {
          source.emit('code_index_activity', frame(revision));
          vi.advanceTimersByTime(1);
        });
      }
    }
    act(() => {
      vi.advanceTimersByTime(RENDER_TICK_MS);
    });

    const total = renders - rendersAtMount;
    expect(revision).toBe(10_000);
    expect(total).toBeGreaterThan(0);
    // Ten seconds x ten renders/s. The trailing flush may add one.
    expect(total).toBeLessThanOrEqual(10 * 10 + 1);
    expect(getByTestId('live').textContent).toBe('live:10000');
  });

  it('does not re-render a view for events it already coalesced away', () => {
    const client = newClient();
    mount(<LiveView />, client);
    const source = FakeEventSource.instances[0]!;
    const rendersAtMount = renders;

    // Twenty duplicate frames: one real occurrence, so at most one tick's worth
    // of render work — and the reducer must not queue them twice.
    for (let i = 0; i < 20; i += 1) {
      act(() => {
        source.emit('code_index_activity', frame(1));
        vi.advanceTimersByTime(1);
      });
    }
    act(() => {
      vi.advanceTimersByTime(RENDER_TICK_MS);
    });

    expect(renders - rendersAtMount).toBeLessThanOrEqual(2);
  });
});

describe('SSE overflow — exactly one canonical invalidation', () => {
  it('invalidates once for a 5,000-event overflow, not once per dropped event', async () => {
    const client = newClient();
    // The canonical invalidation awaits the refetch of every active query, so in
    // production it routinely outlasts the next coalescing tick. Holding it open
    // is what makes "exactly one" falsifiable rather than incidental: `stale` is
    // sticky until the refetch reseeds, so every tick in between sees it set.
    const refetch = deferred();
    const invalidate = vi
      .spyOn(client, 'invalidateQueries')
      .mockImplementation(() => refetch.promise);

    mount(<LiveView />, client);
    const source = FakeEventSource.instances[0]!;

    // Overflow the real 5,000-event ceiling before any tick can drain it.
    act(() => {
      for (let revision = 1; revision <= MAX_QUEUED_EVENTS + 1; revision += 1) {
        source.emit('code_index_activity', frame(revision));
      }
    });
    await act(async () => {
      vi.advanceTimersByTime(RENDER_TICK_MS);
    });
    // No query key: the canonical whole-projection reseed, issued once.
    expect(invalidate).toHaveBeenCalledTimes(1);
    expect(invalidate).toHaveBeenCalledWith();

    // The stream does not stop while that refetch is in flight.
    for (let revision = 6_000; revision < 6_010; revision += 1) {
      await act(async () => {
        source.emit('code_index_activity', frame(revision));
        vi.advanceTimersByTime(RENDER_TICK_MS);
      });
    }
    expect(invalidate).toHaveBeenCalledTimes(1);

    // Once it settles the reducer reseeds, and an ordinary event goes back to
    // the targeted path instead of the canonical one.
    await act(async () => {
      refetch.resolve();
    });
    await act(async () => {
      source.emit('project_registry', registryFrame());
      vi.advanceTimersByTime(RENDER_TICK_MS);
    });
    expect(invalidate).toHaveBeenCalledTimes(2);
    expect(invalidate).toHaveBeenLastCalledWith({ queryKey: ['projects'] });
    invalidate.mockRestore();
  });
});
