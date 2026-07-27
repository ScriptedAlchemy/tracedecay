import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useSyncExternalStore,
} from 'react';
import type { ReactNode } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import {
  connectEvents,
  type LiveActivityPulse,
  type SseConnection,
  type SseConnectionState,
} from './connect.ts';
import type { SseBatch } from './types.ts';

/**
 * Period of the render clock. The plan bounds SSE-driven work at "at most ten
 * renders/s/view", so the render layer owns this clock — the reducer owns no
 * timers. The connection notifies once per received frame, which is the full
 * arrival rate (up to the plan's peak of 1,000/s); every subscriber downstream
 * of this clock instead sees a trailing tick. A tick is only scheduled when
 * none is already pending, so consecutive ticks are always at least this far
 * apart and the rate is a ceiling rather than an average.
 */
const RENDER_TICK_MS = 100;

interface RenderClock {
  subscribe(listener: () => void): () => void;
  /** Detach from the connection and cancel a pending tick. */
  stop(): void;
}

function createRenderClock(connection: SseConnection): RenderClock {
  const listeners = new Set<() => void>();
  let timer: ReturnType<typeof setTimeout> | null = null;
  const detach = connection.subscribe(() => {
    if (timer !== null) return;
    timer = setTimeout(() => {
      timer = null;
      for (const listener of [...listeners]) listener();
    }, RENDER_TICK_MS);
  });
  return {
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    stop() {
      detach();
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
    },
  };
}

const EventsContext = createContext<SseConnection | null>(null);
const RenderClockContext = createContext<RenderClock | null>(null);

/** Mounts one event-stream connection for the whole app (plan: workspaces
 * never open ad hoc EventSources). */
export function EventsProvider({ children, url }: { children: ReactNode; url?: string }) {
  const connection = useMemo(() => connectEvents(url), [url]);
  const clock = useMemo(() => createRenderClock(connection), [connection]);
  const queryClient = useQueryClient();
  useEffect(
    () => () => {
      clock.stop();
      connection.close();
    },
    [clock, connection],
  );
  useEffect(() => {
    // A gap or overflow is one canonical invalidation/refetch. `stale` stays
    // sticky until that refetch settles and reseeds the reducer, and settling
    // waits on every active query, which can easily outlast several ticks.
    // Re-issuing per tick would fan one overflow into a refetch storm at the
    // exact moment the client is already behind.
    let reseeding = false;
    return clock.subscribe(() => {
      // A tick also fires for connection-state changes, which carry no batch.
      if (!connection.reducer.hasPending()) return;
      const batch = connection.reducer.takeBatch();
      const canonical = batch.refetch || batch.stale;
      if (canonical && reseeding) return;
      const invalidations = invalidationKeysForBatch(batch).map((queryKey) =>
        queryKey.length === 0
          ? queryClient.invalidateQueries()
          : queryClient.invalidateQueries({ queryKey: [...queryKey] }),
      );
      if (!canonical) return;
      reseeding = true;
      void Promise.allSettled(invalidations).then(() => {
        connection.reducer.reset();
        reseeding = false;
      });
    });
  }, [clock, connection, queryClient]);
  return (
    <EventsContext.Provider value={connection}>
      <RenderClockContext.Provider value={clock}>{children}</RenderClockContext.Provider>
    </EventsContext.Provider>
  );
}

export function invalidationKeysForBatch(
  batch: SseBatch,
): ReadonlyArray<ReadonlyArray<string>> {
  if (batch.refetch || batch.stale) return [[]];
  let storage = false;
  let projects = false;
  for (const event of batch.events) {
    if (!isRecord(event.payload)) continue;
    if (event.payload['family'] === 'storage_telemetry_invalidated') storage = true;
    if (event.payload['family'] === 'project_registry_changed') projects = true;
  }
  const keys: string[][] = [];
  if (storage) keys.push(['storage', 'telemetry']);
  if (projects) keys.push(['projects']);
  return keys;
}

export function useEventsConnection(): SseConnection | null {
  return useContext(EventsContext);
}

/**
 * Subscribe a view to the coalesced render clock. Views must never subscribe to
 * the connection directly: that notifies once per frame and would re-render at
 * the arrival rate instead of the plan's ten renders/s/view ceiling.
 */
function useRenderTick(): (onStoreChange: () => void) => () => void {
  const clock = useContext(RenderClockContext);
  return useCallback(
    (onStoreChange: () => void) => (clock ? clock.subscribe(onStoreChange) : () => {}),
    [clock],
  );
}

export function useEventStreamState(): {
  state: SseConnectionState;
  lastEventAt: number | null;
} {
  const connection = useContext(EventsContext);
  const subscribe = useRenderTick();
  const state = useSyncExternalStore(subscribe, () =>
    connection ? connection.state() : 'offline',
  );
  return { state, lastEventAt: connection?.lastEventAt() ?? null };
}

/**
 * Live pulses for the activation visualizations. The revision is the render
 * trigger (a number — a stable snapshot for `useSyncExternalStore`); callers
 * read the pulse ring and apply only what is newer than what they last drew.
 * The revision advances with every accepted event, but it is only *observed* on
 * a render tick, so a burst redraws the ring at most ten times a second.
 */
export function useLiveActivity(): {
  pulses: readonly LiveActivityPulse[];
  revision: number;
} {
  const connection = useContext(EventsContext);
  const subscribe = useRenderTick();
  const revision = useSyncExternalStore(subscribe, () =>
    connection ? connection.activityRevision() : 0,
  );
  return { pulses: connection?.activity() ?? EMPTY_PULSES, revision };
}

const EMPTY_PULSES: readonly LiveActivityPulse[] = [];

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
