import { DESIGN_SCENARIO } from './designScenario';

export type ProximityExample = {
  id: string; label: string;
  kind: 'neighborhood' | 'read-write' | 'write-write' | 'integration' | 'conflict' | 'duplicate';
  from: number; to: number; observedAt: number; expiresAt: number;
  agentSessions: [string, string]; worktrees: [string, string]; heads: [string, string];
  paths: [string, string]; ranges: [string, string]; access: ['read' | 'write', 'read' | 'write'];
  method: string; basis: string; tests: string[]; sourceEventIds: [string, string];
  sourceRef: string; mergeBase?: string; intent?: [string, string];
};

const file = 'dashboard/src/components/TimelineCanvas.tsx';
const sourceRef = 'AUTHORED FIXTURE · proximity rendering example, not a profile observation or production finding';
const time = (clock: string) => Date.parse(`2025-05-12T${clock}Z`) / 1000;

// Footprints below are explicitly authored alternatives over the same illustrative
// event identities. They are not extracted ranges or receipts from those events.
function example(value: Omit<ProximityExample, 'from' | 'to' | 'agentSessions' | 'sourceRef' | 'expiresAt'>): ProximityExample {
  const sources = value.sourceEventIds.map(id => DESIGN_SCENARIO.events.find(event => event.id === id));
  if (!sources[0] || !sources[1] || sources[0].ts === null || sources[1].ts === null) {
    throw new Error(`Authored proximity example ${value.id} references an unavailable event`);
  }
  return { ...value, from: Math.min(sources[0].ts, sources[1].ts), to: Math.max(sources[0].ts, sources[1].ts),
    agentSessions: [sources[0].sessionId, sources[1].sessionId], sourceRef, expiresAt: value.observedAt + 30 };
}

export const PROXIMITY_EXAMPLES: ProximityExample[] = [
  example({
    id: 'fixture:proximity:nearby-reads', label: 'Authored fixture · nearby reads', kind: 'neighborhood',
    observedAt: time('14:23:24'), sourceEventIds: ['design:event:agent-7-2', 'design:event:agent-28-2'],
    worktrees: ['fixture-worktree-main', 'fixture-worktree-main'], heads: ['fixture-head-main', 'fixture-head-main'],
    paths: [file, 'dashboard/src/components/EventGlyph.tsx'], ranges: ['TimelineCanvas:256–321', 'EventGlyph:20–64'], access: ['read', 'read'],
    method: 'Authored direct call-neighborhood example', basis: 'Illustrated source reads touch neighboring components. Read/read adjacency is neither overwrite nor conflict; no shared editing or intent is asserted.',
    tests: ['Illustrated affected test: timeline glyph navigation'],
  }),
  example({
    id: 'fixture:proximity:read-write', label: 'Authored fixture · read/write overlap', kind: 'read-write',
    observedAt: time('14:32:18'), sourceEventIds: ['design:event:agent-7-2', 'design:event:timeline-file-edited'],
    worktrees: ['fixture-worktree-main', 'fixture-worktree-main'], heads: ['fixture-head-main', 'fixture-head-main'],
    paths: [file, file], ranges: ['TimelineCanvas:256–321', 'TimelineCanvas:258–260'], access: ['read', 'write'],
    method: 'Authored same-worktree symbol-range overlap', basis: 'Illustrated reader footprint intersects the edited guard. This is a collision candidate with read/write context, not evidence that the reader used a stale value or that writes conflicted.',
    tests: ['Illustrated affected test: pinned-event visibility'],
  }),
  example({
    id: 'fixture:proximity:write-write', label: 'Authored fixture · two writers', kind: 'write-write',
    observedAt: time('14:36:22'), sourceEventIds: ['design:event:timeline-file-edited', 'design:event:review-revision-edit'],
    worktrees: ['fixture-worktree-main', 'fixture-worktree-main'], heads: ['fixture-head-main', 'fixture-head-main'],
    paths: [file, file], ranges: ['TimelineCanvas:256–321', 'TimelineCanvas:318–321'], access: ['write', 'write'],
    method: 'Authored same-worktree overlapping symbol footprints', basis: 'The authored activity intervals and symbol footprints overlap. No incompatible replacement or recorded error is supplied, so this remains an amber candidate rather than an observed conflict.',
    tests: ['Illustrated affected test: pinned-event visibility', 'Illustrated affected test: forced-event visibility'],
  }),
  example({
    id: 'fixture:proximity:integration', label: 'Authored fixture · prospective integration', kind: 'integration',
    observedAt: time('14:36:24'), sourceEventIds: ['design:event:timeline-file-edited', 'design:event:review-revision-edit'],
    worktrees: ['fixture-worktree-window-guard', 'fixture-worktree-pinned-lane'], heads: ['fixture-head-window-guard', 'fixture-head-pinned-lane'], mergeBase: 'fixture-base-before-visibility-edits',
    paths: [file, file], ranges: ['TimelineCanvas:256–321', 'TimelineCanvas:318–321'], access: ['write', 'write'],
    method: 'Authored diff-region overlap against an explicit common base', basis: 'Independent worktree changes touch the same illustrated symbol. They cannot overwrite each other on disk. This suggests future integration review, without claiming a merge conflict or current production cross-worktree finding.',
    tests: ['Illustrated comparison: pinned-event and forced-event visibility tests'],
  }),
  example({
    id: 'fixture:proximity:git-conflict', label: 'Authored fixture · exact Git conflict', kind: 'conflict',
    observedAt: time('14:39:15'), sourceEventIds: ['design:event:review-commit-amended', 'design:event:review-revision-edit'],
    worktrees: ['fixture-worktree-conflict-source', 'fixture-worktree-conflict-destination'], heads: ['fixture-head-conflict-source', 'fixture-head-conflict-destination'], mergeBase: 'fixture-base-conflict-preview',
    paths: [file, file], ranges: ['TimelineCanvas:318–321', 'TimelineCanvas:318–321'], access: ['write', 'write'],
    method: 'Authored native Git integration-preview result at the named fixture revisions', basis: 'The fixture explicitly declares a mechanical content conflict in this range for these two heads and common base. This is the only conflict example; it is not an executed Git command, production receipt, semantic failure prediction or real-profile observation.',
    tests: ['Illustrated verification remains pending: resolve content conflict, then run visibility tests'],
  }),
  example({
    id: 'fixture:proximity:duplicate', label: 'Authored fixture · duplicate candidate', kind: 'duplicate',
    observedAt: time('14:36:26'), sourceEventIds: ['design:event:timeline-file-edited', 'design:event:review-revision-edit'],
    worktrees: ['fixture-worktree-window-guard', 'fixture-worktree-pinned-lane'], heads: ['fixture-head-window-guard', 'fixture-head-pinned-lane'], mergeBase: 'fixture-base-before-visibility-edits',
    paths: [file, file], ranges: ['TimelineCanvas:256–260', 'TimelineCanvas:318–321'], access: ['write', 'write'],
    method: 'Authored paired-intent and test comparison; no similarity score', basis: 'Both illustrated implementations address pinned-event reachability using different paths. Paired intent and test cases warrant comparison; they do not prove redundancy, equivalent behavior or authorization to delete either implementation.',
    intent: ['Retain pinned events through the window guard.', 'Render pinned events independently in a dedicated lane.'],
    tests: ['Window guard: out-of-window pinned event remains reachable.', 'Pinned lane: out-of-window pinned event remains reachable.', 'Open comparison: forced events, duplicates and keyboard order.'],
  }),
];

export type ProximityStrand = {
  key: string; sessionId: string; groupId: string; groupName: string;
  color: string; home: number; phase: number;
};

/** One stable strand per authored workstream participation. */
export const PROXIMITY_STRANDS: ProximityStrand[] = DESIGN_SCENARIO.groups.flatMap((group, groupIndex) => {
  return group.sessionIds.map((sessionId, lane) => ({
    key: `${group.id}/${sessionId}`, sessionId, groupId: group.id, groupName: group.name,
    color: group.color, home: (groupIndex + .09 + .82 * (lane + .5) / group.sessionIds.length) / DESIGN_SCENARIO.groups.length,
    phase: lane * 2.399 + groupIndex * .83,
  }));
});

const STRAND_BY_KEY = new Map(PROXIMITY_STRANDS.map(strand => [strand.key, strand]));
const groupForEvent = (eventId: string) => {
  const task = DESIGN_SCENARIO.details[eventId]?.task;
  return DESIGN_SCENARIO.groups.find(group => group.name === task)?.id;
};
export const proximityGroupForEvent = (eventId: string) => groupForEvent(eventId);
export function proximityGroupForEncounter(observation: ProximityExample, sessionId: string) {
  const index = observation.agentSessions.indexOf(sessionId);
  return groupForEvent(observation.sourceEventIds[index]) ?? DESIGN_SCENARIO.groups.find(group => group.sessionIds.includes(sessionId))?.id;
}

/** Authored strand layout. Only revealed observations may alter its local geometry. */
export function proximityY(sessionId: string, time: number, observations: ProximityExample[], groupId = DESIGN_SCENARIO.groups.find(group => group.sessionIds.includes(sessionId))?.id) {
  const strand = STRAND_BY_KEY.get(`${groupId}/${sessionId}`);
  if (!strand) return .5;
  const [start, end] = DESIGN_SCENARIO.windows.dense;
  const u = Math.max(0, Math.min(1, (time - start) / (end - start)));
  const base = strand.home + Math.sin(u * Math.PI * 4 + strand.phase) * .004;
  const observation = observations.find(item => item.agentSessions.includes(sessionId) && proximityGroupForEncounter(item,sessionId) === groupId && time >= item.from && time <= item.to);
  if (!observation) return base;
  const partnerId = observation.agentSessions[observation.agentSessions[0] === sessionId ? 1 : 0];
  const partner = STRAND_BY_KEY.get(`${proximityGroupForEncounter(observation,partnerId)}/${partnerId}`);
  if (!partner) return base;
  const progress = (time - observation.from) / Math.max(1, observation.to - observation.from);
  const influence = Math.sin(Math.PI * progress) ** 2;
  return base + (partner.home - base) * .42 * influence;
}

export const proximityTone = (observation: ProximityExample) => observation.kind === 'conflict' ? '#ff7368' : '#ffc04d';

/** Noncausal geometry is local to the observation's declared validity interval. */
export function proximityBend(time: number, observation: ProximityExample) {
  const u = (time - observation.observedAt) / (observation.expiresAt - observation.observedAt);
  return u > 0 && u < 1 ? Math.sin(Math.PI * u) ** 2 : 0;
}
