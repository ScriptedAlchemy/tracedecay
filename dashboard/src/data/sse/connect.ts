import type { SseEventEnvelope } from './types.ts';
import { createSseReducer, type SseReducer } from './reducer.ts';

export type SseConnectionState = 'connecting' | 'live' | 'offline';

export interface SseConnection {
  readonly reducer: SseReducer;
  state(): SseConnectionState;
  lastEventAt(): number | null;
  subscribe(listener: () => void): () => void;
  close(): void;
}

/** Connects the daemon's /api/events stream to the monotone reducer.
 * Reconnection is EventSource-native; the reducer's per-stream generation
 * gates and gap detection handle replays and missed events (refetch signal).
 */
export function connectEvents(url = '/api/events'): SseConnection {
  const reducer = createSseReducer();
  const listeners = new Set<() => void>();
  let state: SseConnectionState = 'connecting';
  let lastEventAt: number | null = null;

  const notify = () => listeners.forEach((l) => l());
  const setState = (next: SseConnectionState) => {
    if (state !== next) {
      state = next;
      notify();
    }
  };

  const source = new EventSource(url);
  source.onopen = () => setState('live');
  source.onerror = () =>
    setState(source.readyState === EventSource.CLOSED ? 'offline' : 'connecting');
  source.onmessage = (msg) => {
    lastEventAt = Date.now();
    setState('live');
    try {
      const parsed = JSON.parse(msg.data) as SseEventEnvelope;
      reducer.ingest(parsed);
      notify();
    } catch {
      // Malformed frames are dropped; gap detection triggers a canonical
      // refetch when the next well-formed frame arrives.
    }
  };

  return {
    reducer,
    state: () => state,
    lastEventAt: () => lastEventAt,
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    close() {
      source.close();
      setState('offline');
    },
  };
}
