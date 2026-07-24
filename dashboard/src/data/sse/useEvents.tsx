import { createContext, useContext, useEffect, useMemo, useSyncExternalStore } from 'react';
import type { ReactNode } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import {
  connectEvents,
  type LiveActivityPulse,
  type SseConnection,
  type SseConnectionState,
} from './connect.ts';
import type { SseBatch } from './types.ts';

const EventsContext = createContext<SseConnection | null>(null);

/** Mounts one event-stream connection for the whole app (plan: workspaces
 * never open ad hoc EventSources). */
export function EventsProvider({ children, url }: { children: ReactNode; url?: string }) {
  const connection = useMemo(() => connectEvents(url), [url]);
  const queryClient = useQueryClient();
  useEffect(() => () => connection.close(), [connection]);
  useEffect(() => {
    let flushTimer: ReturnType<typeof setTimeout> | null = null;
    const flush = () => {
      flushTimer = null;
      const batch = connection.reducer.takeBatch();
      const invalidations = invalidationKeysForBatch(batch).map((queryKey) =>
        queryKey.length === 0
          ? queryClient.invalidateQueries()
          : queryClient.invalidateQueries({ queryKey: [...queryKey] }),
      );
      if (batch.refetch || batch.stale) {
        void Promise.allSettled(invalidations).then(() => connection.reducer.reset());
      }
    };
    const unsubscribe = connection.subscribe(() => {
      if (!connection.reducer.hasPending() || flushTimer !== null) return;
      flushTimer = setTimeout(flush, 100);
    });
    return () => {
      unsubscribe();
      if (flushTimer !== null) clearTimeout(flushTimer);
    };
  }, [connection, queryClient]);
  return <EventsContext.Provider value={connection}>{children}</EventsContext.Provider>;
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

export function useEventStreamState(): {
  state: SseConnectionState;
  lastEventAt: number | null;
} {
  const connection = useContext(EventsContext);
  const state = useSyncExternalStore(
    (cb) => (connection ? connection.subscribe(cb) : () => {}),
    () => (connection ? connection.state() : 'offline'),
  );
  return { state, lastEventAt: connection?.lastEventAt() ?? null };
}

/**
 * Live pulses for the activation visualizations. The revision is the render
 * trigger (a number — a stable snapshot for `useSyncExternalStore`); callers
 * read the pulse ring and apply only what is newer than what they last drew.
 */
export function useLiveActivity(): {
  pulses: readonly LiveActivityPulse[];
  revision: number;
} {
  const connection = useContext(EventsContext);
  const revision = useSyncExternalStore(
    (cb) => (connection ? connection.subscribe(cb) : () => {}),
    () => (connection ? connection.activityRevision() : 0),
  );
  return { pulses: connection?.activity() ?? EMPTY_PULSES, revision };
}

const EMPTY_PULSES: readonly LiveActivityPulse[] = [];

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
