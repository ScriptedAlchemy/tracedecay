/**
 * URL state for the Loom temporal field. Every addressable piece of reader
 * intent lives in the query string, so a link reproduces the same window,
 * branch set, filters and playback position — and a refetch never resets them.
 *
 * Parsing is defensive: a malformed value is ignored rather than turned into a
 * window or a filter the reader never asked for.
 */
import {
  JOURNEY_EVENT_KINDS,
  type JourneyEventKind,
  type SceneWindow,
  type SemanticZoom,
} from '../../viz/temporal/types.ts';

export const LOOM_PARAMS = {
  session: 'loomSession',
  event: 'loomEvent',
  window: 'loomWindow',
  collapsed: 'loomCollapsed',
  expanded: 'loomExpanded',
  hidden: 'loomHide',
  zoom: 'loomZoom',
  encounter: 'loomEncounter',
} as const;

/** Earliest epoch second a Loom window may name. Anything smaller is not a
 * time this store could have recorded and is treated as malformed. */
const EPOCH_FLOOR = 1_000_000_000;

export function parseWindow(raw: string | null): SceneWindow | null {
  if (raw == null) return null;
  const parts = raw.split(',').map(Number);
  if (parts.length !== 2) return null;
  const [start, end] = parts;
  if (start == null || end == null) return null;
  if (!Number.isFinite(start) || !Number.isFinite(end)) return null;
  if (start < EPOCH_FLOOR || end <= start) return null;
  return { start, end };
}

export function serializeWindow(window: SceneWindow): string {
  return `${Math.round(window.start)},${Math.round(window.end)}`;
}

/** Lane ids are themselves JSON tuples, so a set of them travels as one JSON
 * array rather than a delimiter that a session id could contain. */
export function parseLaneSet(raw: string | null): ReadonlySet<string> {
  if (raw == null || raw.length === 0) return new Set();
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return new Set();
    return new Set(parsed.filter((entry): entry is string => typeof entry === 'string'));
  } catch {
    return new Set();
  }
}

export function serializeLaneSet(ids: ReadonlySet<string>): string | null {
  if (ids.size === 0) return null;
  return JSON.stringify([...ids].sort());
}

export function parseHiddenKinds(raw: string | null): ReadonlySet<JourneyEventKind> {
  if (raw == null || raw.length === 0) return new Set();
  const known = new Set<string>(JOURNEY_EVENT_KINDS);
  return new Set(
    raw
      .split(',')
      .filter((kind): kind is JourneyEventKind => known.has(kind)),
  );
}

export function serializeHiddenKinds(kinds: ReadonlySet<JourneyEventKind>): string | null {
  if (kinds.size === 0) return null;
  return JOURNEY_EVENT_KINDS.filter((kind) => kinds.has(kind)).join(',');
}

/** Only the two reader-selectable levels travel in the URL; `event` is
 * implied by a selected session whose transcript page is loaded. */
export function parseZoom(raw: string | null): Exclude<SemanticZoom, 'event'> {
  return raw === 'workstream' ? 'workstream' : 'agent';
}

export function toggleInSet<T>(set: ReadonlySet<T>, value: T): ReadonlySet<T> {
  const next = new Set(set);
  if (next.has(value)) next.delete(value);
  else next.add(value);
  return next;
}
