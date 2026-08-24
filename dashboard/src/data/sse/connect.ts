import type { SseEventEnvelope } from './types.ts';
import { createSseReducer, type SseReducer } from './reducer.ts';

const DASHBOARD_EVENT_NAMES = [
  'heartbeat',
  'project_registry',
  'storage_telemetry',
  'control',
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
  const deliveryAckUrl = `${url.replace(/\?.*$/, '').replace(/\/$/, '')}/delivery-ack`;

  /** Record an accepted event as a pulse. Only newly accepted events pulse —
   * duplicates, stale generations, and superseded revisions must not light the
   * visualization twice for one real occurrence. */
  const recordActivity = (event: DecodedSseEvent) => {
    const payload = event.payload;
    activity.push({
      projectId: event.projectId,
      family:
        isRecord(payload) && typeof payload.family === 'string' ? payload.family : event.stream.stream_id,
      streamId: event.stream.stream_id,
      at: Date.now(),
    });
    if (activity.length > MAX_ACTIVITY_PULSES) activity.shift();
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
      const parsed = decodeDashboardEvent(raw, event.data.length);
      if (!parsed) return;
      if (isRecord(raw) && typeof raw.delivery_receipt === 'string') {
        void acknowledgeDelivery(deliveryAckUrl, raw.delivery_receipt);
      }
      if (reducer.ingest(parsed)) recordActivity(parsed);
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

async function acknowledgeDelivery(url: string, receipt: string): Promise<void> {
  for (let attempt = 0; attempt < 3; attempt += 1) {
    try {
      const response = await fetch(url, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ receipt }),
        keepalive: true,
      });
      if (response.ok) return;
    } catch {
      // Retry transient transport refusal below.
    }
    if (attempt < 2) {
      await new Promise((resolve) => setTimeout(resolve, 250 * (attempt + 1)));
    }
  }
  // A non-success never becomes browser receipt evidence. A later replay may
  // retry the token; the server owns its exact deadline and terminal state.
}

/** Decode-time extras the reducer and activity ring read without re-walking
 * the raw frame. `SseEventEnvelope` stays the monotone contract; these fields
 * ride alongside it from this decoder only. */
interface DecodedSseEvent extends SseEventEnvelope {
  readonly projectId: string | null;
  readonly frameBytes: number;
}

function decodeDashboardEvent(value: unknown, frameBytes: number): DecodedSseEvent | null {
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
  if (watermark !== null) {
    if (typeof watermark.source !== 'string' || typeof watermark.watermark !== 'string') {
      return null;
    }
    watermarkValue = watermark.watermark;
  }
  const rawProjectId = value.scope.project_id;
  const projectId =
    typeof rawProjectId === 'string' && rawProjectId.length > 0 ? rawProjectId : null;
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
    projectId,
    frameBytes,
  };
}

/** Project id extracted at decode, or parsed from a hand-built envelope's
 * opaque scope string (tests construct those without going through decode). */
export function eventProjectId(event: SseEventEnvelope): string | null {
  if ('projectId' in event) {
    const id = (event as DecodedSseEvent).projectId;
    return typeof id === 'string' && id.length > 0 ? id : null;
  }
  return parseScopeProjectId(event.scope);
}

function parseScopeProjectId(scope: string): string | null {
  try {
    const parsed: unknown = JSON.parse(scope);
    if (!isRecord(parsed)) return null;
    const projectId = parsed['project_id'];
    return typeof projectId === 'string' && projectId.length > 0 ? projectId : null;
  } catch {
    return null;
  }
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
