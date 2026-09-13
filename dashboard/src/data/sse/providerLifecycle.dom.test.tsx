/**
 * The provider owns the EventSource: a committed effect opens it and that
 * effect's cleanup closes it — never render. These cases pin the two failures
 * a render-time connection has, at the provider boundary and with the real
 * render clock: a render discarded before commit leaks a source nothing will
 * close, and effect setup → cleanup → setup (StrictMode, a URL change) leaves
 * the memoized, already-closed source in context so no event reaches a view.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, render } from '@testing-library/react';
import { StrictMode, Suspense } from 'react';
import type { ReactNode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { FakeEventSource } from '../../test/fakeEventSource.ts';
import { EventsProvider, useEventStreamState, useLiveActivity } from './useEvents.tsx';

/** The render clock's period in `useEvents.tsx`: one tick, one batch boundary. */
const RENDER_TICK_MS = 100;

function frame(revision: number) {
  return {
    stream: 'heartbeat',
    run_id: 'run-1-1700000000000000',
    event_revision: revision,
    entity_revision: null,
    scope: { project_id: null, storage_mode: 'project_local', store_root: '/s' },
    observation_time_micros: 1_700_000_000_000_000 + revision,
    source_watermark: null,
    coverage: { completeness: 'complete', denominator: 1 },
    kind: { family: 'heartbeat' },
  };
}

/** A view on the stream: link state and the accepted-event revision. */
function LiveView() {
  const { state } = useEventStreamState();
  const { revision } = useLiveActivity();
  return (
    <span data-testid="live">
      {state}:{revision}
    </span>
  );
}

function tree(url: string, children: ReactNode, client: QueryClient) {
  return (
    <QueryClientProvider client={client}>
      <EventsProvider url={url}>{children}</EventsProvider>
    </QueryClientProvider>
  );
}

function newClient() {
  return new QueryClient({ defaultOptions: { queries: { retry: false } } });
}

/** Emit one frame on `source` and let the coalescing clock fire once. */
function deliver(source: FakeEventSource, revision: number) {
  act(() => {
    source.emit('heartbeat', frame(revision));
    vi.advanceTimersByTime(RENDER_TICK_MS);
  });
}

/** Every source that is not open was closed exactly once — no double close,
 * and no source left half-owned. */
function expectClosedExactlyOnce(except: readonly FakeEventSource[] = []) {
  for (const source of FakeEventSource.instances) {
    expect(source.closeCalls).toBe(except.includes(source) ? 0 : 1);
  }
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal('EventSource', FakeEventSource);
  FakeEventSource.instances = [];
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  FakeEventSource.instances = [];
});

describe('EventsProvider connection lifecycle', () => {
  it('keeps exactly one live source under StrictMode and delivers events through it', () => {
    const client = newClient();
    const { getByTestId, unmount } = render(
      <StrictMode>{tree('/api/events', <LiveView />, client)}</StrictMode>,
    );

    // StrictMode ran setup → cleanup → setup. Whatever that opened, one source
    // is live, every other one was closed exactly once, and the live one is the
    // one in context: a frame on it reaches the view through the render clock.
    const open = FakeEventSource.open();
    expect(open).toHaveLength(1);
    expectClosedExactlyOnce(open);
    const live = open[0]!;
    expect(live.url).toBe('/api/events');

    deliver(live, 1);
    expect(getByTestId('live').textContent).toBe('live:1');

    unmount();
    expect(FakeEventSource.open()).toHaveLength(0);
    expectClosedExactlyOnce();
  });

  it('closes the previous source once and reconnects when the URL changes', () => {
    const client = newClient();
    const { getByTestId, rerender } = render(tree('/api/events', <LiveView />, client));
    const [unscoped] = FakeEventSource.open();
    expect(unscoped?.url).toBe('/api/events');

    rerender(tree('/api/projects/proj_b/events', <LiveView />, client));

    const open = FakeEventSource.open();
    expect(open).toHaveLength(1);
    const scoped = open[0]!;
    expect(scoped.url).toBe('/api/projects/proj_b/events');
    expect(scoped).not.toBe(unscoped);
    expectClosedExactlyOnce(open);

    // The replacement is the one views hear: the old source's state is gone.
    deliver(scoped, 1);
    expect(getByTestId('live').textContent).toBe('live:1');
  });

  it('opens no source for a render that never commits', () => {
    const client = newClient();
    // A child that suspends forever: the provider's render runs, the tree is
    // discarded for the fallback, and no effect ever commits. A render-time
    // connection here would be one nothing can close.
    const never = new Promise<never>(() => {});
    function Suspender(): null {
      throw never;
    }
    const { getByTestId } = render(
      <Suspense fallback={<span data-testid="fallback" />}>
        {tree('/api/events', <Suspender />, client)}
      </Suspense>,
    );

    expect(getByTestId('fallback')).toBeTruthy();
    expect(FakeEventSource.instances).toHaveLength(0);
  });
});
