/**
 * URL state for the Loom temporal field. Every addressable piece of reader
 * intent lives in the query string, so a link reproduces the same window,
 * branch set, filters and playback position, and a refetch never resets them.
 *
 * Parsing is defensive: a malformed value is ignored rather than turned into a
 * window or a filter the reader never asked for.
 */
import { assertNever, type LoomSessionRowV1 } from '../../contracts/generated.ts';
import { sessionLaneId, UNRECORDED_PROVIDER } from '../../viz/temporal/journey.ts';
import { joinIndexRow, type SessionSelection } from '../sessions/model.ts';
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

/**
 * `loomSession` has two shipped forms: the lane id, a JSON tuple
 * `["provider","session_id"]` that Loom itself writes, and a bare session id
 * (Delivery's link), which names no provider. Anything that is not a
 * two-string tuple is a bare id. Loom never rewrites either; a click writes the
 * lane id, so links converge without an effect.
 */
type LoomSessionParam =
  | { readonly kind: 'lane'; readonly laneId: string; readonly selection: SessionSelection }
  | { readonly kind: 'bare_id'; readonly selection: SessionSelection };

export function parseLoomSession(raw: string | null): LoomSessionParam | null {
  if (raw == null || raw === '') return null;
  const tuple = parseTuple(raw);
  if (tuple === null) return { kind: 'bare_id', selection: { provider: null, sessionId: raw } };
  const [provider, sessionId] = tuple;
  return {
    kind: 'lane',
    laneId: sessionLaneId(provider, sessionId),
    // The lane sentinel covers a recorded empty provider, so it names none.
    selection: { provider: provider === '' || provider === UNRECORDED_PROVIDER ? null : provider, sessionId },
  };
}

function parseTuple(raw: string): [string, string] | null {
  try {
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed) &&
      parsed.length === 2 &&
      typeof parsed[0] === 'string' &&
      typeof parsed[1] === 'string'
      ? [parsed[0], parsed[1]]
      : null;
  } catch {
    return null;
  }
}

/** The lane id the field draws for a stored session. */
export function loomSessionValue(session: { provider: string; sessionId: string }): string {
  return sessionLaneId(session.provider, session.sessionId);
}

export type LoomSelection =
  | { readonly kind: 'none' }
  | { readonly kind: 'lane'; readonly laneId: string; readonly row: LoomSessionRowV1 }
  /** A bare id more than one provider answers to on this page. */
  | { readonly kind: 'ambiguous'; readonly selection: SessionSelection; readonly count: number }
  /** Named, but not on the loaded page. */
  | { readonly kind: 'outside_page'; readonly selection: SessionSelection };

export function resolveLoomSession(
  rows: readonly LoomSessionRowV1[],
  param: LoomSessionParam | null,
): LoomSelection {
  if (param === null) return { kind: 'none' };
  if (param.kind === 'lane') {
    const row = rows.find((candidate) => sessionLaneId(candidate.provider, candidate.session_id) === param.laneId);
    return row ? { kind: 'lane', laneId: param.laneId, row } : { kind: 'outside_page', selection: param.selection };
  }
  const join = joinIndexRow(rows, param.selection);
  switch (join.kind) {
    case 'exact':
      return { kind: 'lane', laneId: sessionLaneId(join.row.provider, join.row.session_id), row: join.row };
    case 'ambiguous':
      return { kind: 'ambiguous', selection: param.selection, count: join.rows.length };
    case 'absent':
      return { kind: 'outside_page', selection: param.selection };
    default:
      return assertNever(join);
  }
}

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
