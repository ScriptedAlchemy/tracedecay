import type { SseEventEnvelope } from './types.ts';
import { createSseReducer, type SseReducer } from './reducer.ts';

const DASHBOARD_EVENT_NAMES = [
  'heartbeat',
  'project_registry',
  'storage_telemetry',
  // Live agent activity, coalesced server-side to at most 2/s per family per
  // project. Named events are opt-in, so a new family must be listed here.
  'hook_activity',
  'session_ingest',
  'code_index_activity',
  'tool_call',
  // Work task mutations. The daemon enumerates this family and emits it under
  // this name; without it listed here the dashboard would drop every frame it
  // sends, so subscribing is what makes the family reach a reader at all.
  'task_activity',
] as const;

export type SseConnectionState = 'connecting' | 'live' | 'offline';

/** How many accepted pulses the connection keeps for the live visualizations.
 * Small on purpose: this is a decay window, not a log. */
const MAX_ACTIVITY_PULSES = 64;

/**
 * One accepted live event projected to the identities a visualization can
 * light up. Product semantics stay out of the reducer (it only sequences
 * envelopes); this projection lives beside the dashboard-specific decoder that
 * already knows the daemon's event shape.
 */
export interface LiveActivityPulse {
  /** Registered project id from the event's exact scope, when profile-backed. */
  projectId: string | null;
  /** Event family (`heartbeat`, `project_registry_changed`, …). */
  family: string;
  /** Stream that carried it. */
  streamId: string;
  /** Client receipt time (ms since epoch). */
  at: number;
}

export interface SseConnection {
  readonly reducer: SseReducer;
  state(): SseConnectionState;
  lastEventAt(): number | null;
  /** Recent accepted pulses, oldest first. Non-consuming: reading this never
   * disturbs the reducer's batch boundary. */
  activity(): readonly LiveActivityPulse[];
  /** Monotone counter — a stable `useSyncExternalStore` snapshot. */
  activityRevision(): number;
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

  let activity: LiveActivityPulse[] = [];
  let activityRevision = 0;

  /** Record an accepted event as a pulse. Only newly accepted events pulse —
   * duplicates, stale generations, and superseded revisions must not light the
   * visualization twice for one real occurrence. */
  const recordActivity = (raw: unknown, streamId: string) => {
    const scope = isRecord(raw) && isRecord(raw.scope) ? raw.scope : null;
    const kind = isRecord(raw) && isRecord(raw.kind) ? raw.kind : null;
    activity = [
      ...activity,
      {
        projectId: typeof scope?.project_id === 'string' ? scope.project_id : null,
        family: typeof kind?.family === 'string' ? kind.family : streamId,
        streamId,
        at: Date.now(),
      },
    ].slice(-MAX_ACTIVITY_PULSES);
    activityRevision += 1;
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
      const raw: unknown = JSON.parse(event.data);
      const parsed = decodeDashboardEvent(raw);
      if (!parsed) return;
      if (reducer.ingest(parsed)) recordActivity(raw, parsed.stream.stream_id);
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
    activity: () => activity,
    activityRevision: () => activityRevision,
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
  const generation = streamGeneration(value.run_id);
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

/**
 * The stream epoch a run id carries, or zero when it carries none.
 *
 * Poll lanes mint a run id per connection (`run-<pid>-<micros>`) and its tail is
 * the epoch: a reconnect raises it and the reducer reseeds against the new one.
 * The durable activity lane has no epoch to raise. It publishes under one
 * constant run id and orders itself by the monotone row id it puts in
 * `event_revision`, so its tail is not a number.
 *
 * Refusing those frames is not a stricter check, it is a silent outage: it drops
 * every hook, session-ingest, code-index, tool-call and task frame the daemon
 * sends, while `receive` has already reported the link as live. A run id with no
 * epoch is a lane that never rotates one, which is generation zero — and the
 * reducer still orders the lane by revision, so nothing is loosened by saying
 * so. Malformation is caught by the field checks above, which this never was.
 */
function streamGeneration(runId: string): number {
  const epoch = Number(runId.split('-').at(-1));
  return Number.isSafeInteger(epoch) && epoch >= 0 ? epoch : 0;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isRevision(value: unknown): value is number {
  return Number.isSafeInteger(value) && (value as number) >= 0;
}
