import type { PackLoomEvent } from '../data/pack';
import { SOURCE } from './source';
import { eventGrade, eventKind, KIND_LABEL } from './packScenes';
import type { EventKind, EvidenceGrade } from './types';

export const LEVELS = ['outcome', 'workstream', 'agent', 'episode', 'event'] as const;
export type ZoomLevel = typeof LEVELS[number];
export type Workspace = 'weave' | 'evidence' | 'feedback' | 'gaps';
export const EVIDENCE_MODES = { story: 'STORY', code: 'CODE & IMPACT', evidence: 'EVIDENCE' } as const;
export type EvidenceMode = keyof typeof EVIDENCE_MODES;
export type EvidenceLayout = { tab: 'source' | 'context' | 'links'; focus: boolean; width: number };

export const WEAVE_COLORS = ['#58b8ff', '#43d7ff', '#77dcd8', '#c8b078', '#929cae', '#8aa2af'];

/** A presentation bundle: co-timed endpoint aggregates retain their inspectable identities.
 * The envelope is keyed to loaded source time, so pan/replay cannot move lanes.
 * It changes Y only; joins still come exclusively from source relations.
 */
export function fiberPosition(time:number, home:number, group:number, extent:readonly number[]) {
  const u=Math.max(0,Math.min(1,(time-extent[0])/Math.max(1,extent[1]-extent[0])));
  const smooth=(n:number)=>{const t=Math.max(0,Math.min(1,n));return t*t*(3-2*t);};
  const opening=smooth(u/.29)*smooth((1-u)/.26);
  const spread=.008+.992*opening;
  return .515+(home-.515)*spread+(.004*Math.sin(u*7+group*4)+.006*Math.sin(u*9+home*8))*opening;
}

export const branchLane = (childIndex: number, history = false) => childIndex < 0 ? history ? .42 : .38 : (history ? [0, .78, 1] : [.08, .68, 1, .23, .84])[childIndex];
export const designLaneColor = (id: string) => ['097', '093', '094'].some(suffix => id.endsWith(suffix)) ? '#e3ad55' : '#48d4ed';
export const EVENTS = [...SOURCE.events].sort((a, b) => (a.ts ?? Infinity) - (b.ts ?? Infinity) || (a.ordinal ?? 0) - (b.ordinal ?? 0));
export const SESSIONS = [...SOURCE.sessions].sort((a, b) => (a.startedTs ?? Infinity) - (b.startedTs ?? Infinity) || a.id.localeCompare(b.id));
export const EVENT_BY_ID = new Map(EVENTS.map(e => [e.id, e]));
export const SESSION_BY_ID = new Map(SESSIONS.map(s => [s.id, s]));
export function sessionSearch(cutoff: number | null) {
  const index = new Map(SESSIONS.map(session => [session.id, `${session.id} ${session.title ?? ''} ${session.agentId ?? ''} ${session.project} ${session.provider}`.toLowerCase()]));
  for (const event of EVENTS) {
    if (cutoff !== null && (event.ts === null || event.ts > cutoff)) continue;
    const detail = SOURCE.design?.details[event.id];
    index.set(event.sessionId, `${index.get(event.sessionId) ?? ''} ${event.kind} ${event.tool ?? ''} ${detail?.task ?? ''} ${detail?.file ?? ''} ${detail?.title ?? ''}`.toLowerCase());
  }
  return index;
}
const times = [...(SOURCE.loadedBounds ?? []), ...SESSIONS.flatMap(s => [s.startedTs, s.endedTs]), ...EVENTS.map(e => e.ts)].filter((t): t is number => t !== null);
export const BOUNDS: [number, number] = [Math.min(...times), Math.max(...times)];
const COLORS = ['#55cfff', '#4ce1e4', '#edb85b', '#b094d9', '#79cfa2', '#8299ab'];
export const GROUPS = SOURCE.design ? SOURCE.design.groups.map(g => ({ ...g, sessions: SESSIONS.filter(s => g.sessionIds.includes(s.id)) })) : [...new Set(SESSIONS.map(s => s.project))].map((name, i) => ({
  id: name, name, color: COLORS[i % COLORS.length], sessions: SESSIONS.filter(s => s.project === name),
}));
export function inGroup(sessionId: string, name: string | null) {
  return !name || !!GROUPS.find(g => g.name === name)?.sessions.some(s => s.id === sessionId);
}
export type JourneyNode = { id: string; session: string; time: number; kind: EventKind; grade: EvidenceGrade; title: string; detail: string; events: PackLoomEvent[] };

/** A relation filter retains its source endpoints without upgrading their row grade. */
export function matchesEvidence(event: PackLoomEvent, grades: EvidenceGrade[], cutoff: number | null): boolean {
  if (!grades.length || grades.includes(eventGrade(event))) return true;
  return !!SOURCE.design?.relations.some(relation => {
    if (!grades.includes(relation.grade) || (relation.from !== event.id && relation.to !== event.id)) return false;
    const other = EVENT_BY_ID.get(relation.from === event.id ? relation.to : relation.from);
    return other?.ts !== null && other?.ts !== undefined && (cutoff === null || other.ts <= cutoff);
  });
}

/** Adjacent records aggregate only inside the same session, kind and minute. */
export function journeyNodes(level: ZoomLevel, kinds: EventKind[], grades: EvidenceGrade[], cutoff: number | null): JourneyNode[] {
  const nodes: JourneyNode[] = [];
  for (const e of EVENTS) {
    if (e.ts === null || (cutoff !== null && e.ts > cutoff)) continue;
    const kind = eventKind(e), grade = eventGrade(e);
    if ((kinds.length && !kinds.includes(kind)) || !matchesEvidence(e, grades, cutoff)) continue;
    const prev = nodes[nodes.length - 1];
    if (level === 'episode' && prev?.session === e.sessionId && prev.kind === kind && e.ts - prev.time < 60) {
      prev.events.push(e);
      prev.detail = `${prev.events.length} recorded events`;
    } else nodes.push({ id: e.id, session: e.sessionId, time: e.ts, kind, grade, title: KIND_LABEL[kind], detail: SOURCE.design?.details[e.id]?.title ?? e.tool ?? e.role ?? e.kind, events: [e] });
  }
  return nodes;
}

export function branchIds(id: string): Set<string> {
  const ids = new Set([id]);
  // Parent IDs form a forest in the snapshot; the set also bounds malformed cycles.
  for (let size = -1; size !== ids.size;) {
    size = ids.size;
    for (const session of SESSIONS) if (session.parentId && ids.has(session.parentId)) ids.add(session.id);
  }
  return ids;
}
export function rootId(id: string): string {
  const seen = new Set<string>();
  let current = id;
  while (!seen.has(current)) {
    seen.add(current);
    const parent = SESSION_BY_ID.get(current)?.parentId;
    if (!parent || !SESSION_BY_ID.has(parent)) break;
    current = parent;
  }
  return current;
}
export function sessionWindow(ids: Set<string>): [number, number] {
  const ts = [...SESSIONS.filter(s => ids.has(s.id)).flatMap(s => [s.startedTs, s.endedTs]), ...EVENTS.filter(event => ids.has(event.sessionId)).map(event => event.ts)].filter((t): t is number => t !== null);
  if (!ts.length) return BOUNDS;
  const a = Math.min(...ts), b = Math.max(...ts), pad = Math.max(30, (b - a) * .06);
  return [Math.max(BOUNDS[0], a - pad), Math.min(BOUNDS[1], b + pad)];
}
export function stamp(ts: number, date = false): string {
  return new Date(ts * 1000).toISOString().slice(date ? 5 : 11, 19).replace('T', ' ');
}

/** Recorded-time axis: equal elapsed intervals always occupy equal distances. */
export function timeAxis(from: number, to: number) {
  const duration = Math.max(1, to - from);
  return {
    position: (time: number) => (time - from) / duration,
    time: (position: number) => from + Math.max(0, Math.min(1, position)) * duration,
  };
}
