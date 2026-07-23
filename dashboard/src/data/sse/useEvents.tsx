import { createContext, useContext, useEffect, useMemo, useSyncExternalStore } from 'react';
import type { ReactNode } from 'react';
import { connectEvents, type SseConnection, type SseConnectionState } from './connect.ts';

const EventsContext = createContext<SseConnection | null>(null);

/** Mounts one event-stream connection for the whole app (plan: workspaces
 * never open ad hoc EventSources). */
export function EventsProvider({ children, url }: { children: ReactNode; url?: string }) {
  const connection = useMemo(() => connectEvents(url), [url]);
  useEffect(() => () => connection.close(), [connection]);
  return <EventsContext.Provider value={connection}>{children}</EventsContext.Provider>;
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
