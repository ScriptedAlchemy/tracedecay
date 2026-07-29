import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
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
import type { SseBatch, SseReducerStats } from './types.ts';

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
    // A gap or overflow is one canonical invalidation/refetch, and settling it
    // waits on every active query, which can easily outlast several ticks.
    // Re-issuing per tick would fan one overflow into a refetch storm at the
    // exact moment the client is already behind, so only one runs at a time —
    // but "one at a time" must not mean "the rest are lost". The reducer owns
    // whether a refresh is still owed, so a signal raised inside the window
    // outlives the drain that cleared its batch flag, and the batch itself is
    // always applied rather than thrown away.
    const { reducer } = connection;
    // A refresh outlives a teardown, and its settle can now start a follow-up,
    // so the chain has to know when the provider stopped caring.
    let stopped = false;

    const invalidate = (keys: ReadonlyArray<ReadonlyArray<string>>): Array<Promise<void>> =>
      keys.map((queryKey) =>
        queryKey.length === 0
          ? queryClient.invalidateQueries()
          : queryClient.invalidateQueries({ queryKey: [...queryKey] }),
      );

    function startCanonicalRefresh(keys: ReadonlyArray<ReadonlyArray<string>>): void {
      const token = reducer.beginReseed();
      void refreshFailure(invalidate(keys)).then((failure) => {
        if (failure === null) reducer.commitReseed(token);
        else reducer.abortReseed(token, failure);
        // If a signal arrived while that was in flight, the commit superseded
        // nothing and exactly one follow-up runs here. The reducer refuses a
        // further one until a genuinely newer signal arrives, so the chain is
        // bounded by real signals rather than by ticks.
        pump();
      });
    }

    function pump(): void {
      if (!stopped && reducer.canonicalRefreshOutstanding()) {
        startCanonicalRefresh([CANONICAL_INVALIDATION_KEY]);
      }
    }

    function applyTargeted(batch: SseBatch): void {
      for (const pending of invalidate(targetedInvalidationKeys(batch))) {
        void pending.catch(() => {
          // A targeted refresh that rejected leaves its slice unfresh. Escalate
          // to the canonical path rather than leaving the rejection unobserved.
          reducer.requestCanonicalRefresh('invalidation_failed');
          pump();
        });
      }
    }

    const unsubscribe = clock.subscribe(() => {
      // A tick also fires for connection-state changes, which carry no batch.
      if (!reducer.hasPending()) return;
      const batch = reducer.takeBatch();
      if ((batch.refetch || batch.stale) && reducer.canonicalRefreshOutstanding()) {
        // The whole-projection key this maps a canonical batch to subsumes every
        // targeted key, so it is the only refresh this batch needs.
        startCanonicalRefresh(invalidationKeysForBatch(batch));
        return;
      }
      // Not the batch that starts a refresh — either nothing canonical happened,
      // or one is already in flight and this batch has to wait its turn. Either
      // way its events name query roots that are narrower than the canonical
      // reseed and still worth refreshing now; discarding them was the defect.
      applyTargeted(batch);
      pump();
    });
    return () => {
      stopped = true;
      unsubscribe();
    };
  }, [clock, connection, queryClient]);
  return (
    <EventsContext.Provider value={connection}>
      <RenderClockContext.Provider value={clock}>{children}</RenderClockContext.Provider>
    </EventsContext.Provider>
  );
}

/** The canonical whole-projection refresh: every query, no key filter. */
const CANONICAL_INVALIDATION_KEY: ReadonlyArray<string> = [];

export function invalidationKeysForBatch(
  batch: SseBatch,
): ReadonlyArray<ReadonlyArray<string>> {
  if (batch.refetch || batch.stale) return [CANONICAL_INVALIDATION_KEY];
  return targetedInvalidationKeys(batch);
}

/**
 * The query roots the batch's own events name, with no canonical escalation.
 * These stay valid while a canonical refresh is in flight: each is narrower than
 * the whole-projection reseed, so issuing them costs one refetch apiece and
 * keeps the view honest instead of waiting out a refresh that may take several
 * ticks to settle.
 */
export function targetedInvalidationKeys(
  batch: SseBatch,
): ReadonlyArray<ReadonlyArray<string>> {
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

/**
 * Await every invalidation and report the first rejection instead of treating it
 * as success. `Promise.allSettled` on its own hides failures — a rejected
 * invalidation still resolves the aggregate — which is how a refresh that never
 * happened came to clear the projection's stale flag.
 */
async function refreshFailure(invalidations: Array<Promise<void>>): Promise<string | null> {
  const results = await Promise.allSettled(invalidations);
  for (const result of results) {
    if (result.status === 'rejected') return describeRejection(result.reason);
  }
  return null;
}

function describeRejection(reason: unknown): string {
  if (reason instanceof Error) return reason.message;
  return typeof reason === 'string' && reason.length > 0 ? reason : 'canonical refresh rejected';
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

/**
 * Whether the projection the views are drawing has been reconciled with the
 * stream, as a state that cannot be mistaken for health.
 *
 * A canonical refresh that REJECTS leaves the reducer stale on purpose and does
 * not retry on its own, because retrying a failing refresh every tick is the
 * storm the render clock exists to prevent. That is the right behaviour and it
 * has one consequence: without something rendering this, the dashboard would sit
 * on a projection it knows is stale while the link indicator still read `live`.
 * A connection can be perfectly healthy while the data behind it is not, so the
 * two are reported separately.
 */
export type ProjectionSync =
  /** Reconciled: no canonical signal is owed and none has failed. */
  | { readonly kind: 'synced' }
  /** A canonical refresh is running now. */
  | { readonly kind: 'resyncing' }
  /** Staleness is owed a refresh that has not started or completed. */
  | { readonly kind: 'stale'; readonly reason: string | null }
  /** A canonical refresh rejected; the projection is knowingly behind. */
  | { readonly kind: 'failed'; readonly reason: string }
  /** No connection is mounted, so there is nothing to reconcile against. */
  | { readonly kind: 'unmounted' };

/** Pure derivation, so the mapping is testable without a DOM or a clock. */
export function projectionSyncFrom(stats: SseReducerStats | null): ProjectionSync {
  if (stats === null) return { kind: 'unmounted' };
  const phase = stats.reseed;
  switch (phase.phase) {
    case 'failed':
      return { kind: 'failed', reason: phase.reason };
    case 'in_flight':
      return { kind: 'resyncing' };
    case 'superseded':
      // The refresh that settled was answering an older signal, so a newer one
      // is still owed: behind, not reconciled.
      return { kind: 'stale', reason: stats.refetchReason };
    case 'idle':
    case 'committed':
      return stats.stale || stats.canonicalRefreshOutstanding
        ? { kind: 'stale', reason: stats.refetchReason }
        : { kind: 'synced' };
    default: {
      const unhandled: never = phase;
      return unhandled;
    }
  }
}

/** Identity of a sync reading, so the snapshot stays referentially stable across
 * ticks that did not change it — `useSyncExternalStore` re-renders on every new
 * object, and `stats()` allocates one per call. */
function syncKey(sync: ProjectionSync): string {
  return sync.kind === 'failed' || sync.kind === 'stale'
    ? `${sync.kind}\u0000${sync.reason ?? ''}`
    : sync.kind;
}

export function useProjectionSync(): ProjectionSync {
  const connection = useContext(EventsContext);
  const subscribe = useRenderTick();
  const cache = useRef<{ key: string; value: ProjectionSync } | null>(null);
  const getSnapshot = useCallback(() => {
    const next = projectionSyncFrom(connection ? connection.reducer.stats() : null);
    const key = syncKey(next);
    if (cache.current === null || cache.current.key !== key) cache.current = { key, value: next };
    return cache.current.value;
  }, [connection]);
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
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
