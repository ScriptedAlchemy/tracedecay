import type { SseEventEnvelope } from './types.ts';
import { createSseReducer, type SseReducer } from './reducer.ts';

const DASHBOARD_EVENT_NAMES = [
  'heartbeat',
  'project_registry',
  'storage_telemetry',
  'code_index',
] as const;

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
  const receive = (event: Event) => {
    if (!('data' in event) || typeof event.data !== 'string') return;
    lastEventAt = Date.now();
    setState('live');
    try {
      const parsed = decodeDashboardEvent(JSON.parse(event.data));
      if (!parsed) return;
      reducer.ingest(parsed);
      notify();
    } catch {
      // Malformed frames are dropped; gap detection triggers a canonical
      // refetch when the next well-formed frame arrives.
    }
  };
  source.onmessage = receive;
  for (const eventName of DASHBOARD_EVENT_NAMES) {
    source.addEventListener(eventName, receive);
  }

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

function decodeDashboardEvent(value: unknown): SseEventEnvelope | null {
  if (!isRecord(value) || !isRecord(value.scope) || !isRecord(value.kind)) return null;
  if (
    typeof value.stream !== 'string'
    || typeof value.run_id !== 'string'
    || !isRevision(value.event_revision)
    || (value.entity_revision !== null && !isRevision(value.entity_revision))
    || !Number.isSafeInteger(value.observation_time_micros)
    || (value.source_watermark !== null && !isRecord(value.source_watermark))
  ) {
    return null;
  }
  const generation = Number(value.run_id.split('-').at(-1));
  if (!Number.isSafeInteger(generation) || generation < 0) return null;
  const watermark = value.source_watermark;
  let watermarkValue = '';
  if (
    watermark !== null
    && (typeof watermark.source !== 'string' || typeof watermark.watermark !== 'string')
  ) {
    return null;
  }
  if (watermark !== null && typeof watermark.watermark === 'string') {
    watermarkValue = watermark.watermark;
  }
  return {
    stream: {
      stream_id: value.stream,
      generation,
    },
    event_id: `${value.run_id}:${value.stream}:${value.event_revision}`,
    revision: {
      event_revision: value.event_revision,
      entity_revision: value.entity_revision ?? value.event_revision,
    },
    scope: JSON.stringify(value.scope),
    observation_time: String(value.observation_time_micros),
    watermark: watermarkValue,
    coverage: value.coverage,
    payload: value.kind,
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isRevision(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0;
}
