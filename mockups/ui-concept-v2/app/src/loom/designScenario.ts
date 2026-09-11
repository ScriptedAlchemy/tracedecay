import type { PackLoomEvent } from '../data/pack';
import type { JourneySession } from './source';
import type { EvidenceGrade } from './types';

type DesignDetail = { title: string; risk?: 'high'; outcome?: 'failed'; task?: string; file?: string; body?: string; diff?: string; sourceLabel: string; candidates?: {sessionId:string; label:string; grade:EvidenceGrade; claim:string}[] };
type DesignFeedback = { id: string; targetEventId: string; sourceEventId: string; author: string; marker: string; text: string; lifecycle: { eventId: string; status: 'open' | 'acknowledged' | 'acted-upon' | 'contradicted'; relationId: string | null }[] };
type DesignRelation = { id: string; from: string; to: string; kind: 'sequence' | 'spawn' | 'handoff' | 'rejoin' | 'cause'; grade: EvidenceGrade; sourceRef: string };

// Authored illustration of the supplied plates, deliberately isolated from both
// profile exports. IDs, source labels and bodies retain this boundary in pivots.
const stamp = (clock: string) => Date.parse(`2025-05-12T${clock}Z`) / 1000;
const sid = (agent: number) => `design:session:agent-${String(agent).padStart(3, '0')}`;
const range = (from: number, to: number) => Array.from({ length: to - from + 1 }, (_, i) => sid(from + i));
const groups = [
  { id: 'design:hotpath', name: 'Hotpath profiling', color: '#58b8ff', sessionIds: range(1, 34) },
  { id: 'design:dashboard', name: 'Dashboard review', color: '#43d7ff', sessionIds: [sid(7), ...range(21, 46)] },
  { id: 'design:store', name: 'Store recovery', color: '#8ee9b1', sessionIds: range(48, 65) },
  { id: 'design:cross-host', name: 'Cross-host audit', color: '#efc16d', sessionIds: range(66, 79) },
  { id: 'design:docs', name: 'Docs & plans', color: '#a8d98c', sessionIds: range(80, 90) },
  { id: 'design:other', name: 'Other', color: '#bd8ded', sessionIds: [...range(91, 103), sid(47), ...range(2, 6)] },
];
const events: PackLoomEvent[] = [];
const details: Record<string, DesignDetail> = {};
const relations: DesignRelation[] = [];
function event(agent: number, key: string, clock: string, kind: string, title: string, detail: Omit<DesignDetail, 'title' | 'sourceLabel'> = {}) {
  const id = `design:event:${key}`;
  const ts = stamp(clock);
  events.push({ id, sessionId: sid(agent), kind, role: 'illustrated-agent', tool: kind === 'command' ? 'Shell' : null, ts, at: new Date(ts * 1000).toISOString(), ordinal: null });
  details[id] = { title, ...detail, sourceLabel: `Design illustration · ${key} · not a profile record` };
  return id;
}
function connect(from: string, to: string, kind: DesignRelation['kind'] = 'sequence', grade: EvidenceGrade = 'exact') {
  relations.push({ id: `design:relation:${relations.length + 1}`, from, to, kind, grade, sourceRef: `Design illustration · authored ${kind} relationship (${grade}); not an observed provider receipt` });
}

// Dense plate: 103 identities, with 20 additional explicitly overlapping group
// memberships. Every visible filament can resolve to these illustrative events.
for (let agent = 1; agent <= 103; agent++) {
  const clocks = ['14:06:03.452', ...Array.from({length:4}, (_, i) => {
    const fraction = ((Math.imul(agent, 2654435761) ^ Math.imul(i + 1, 1597334677)) >>> 0) / 4294967296;
    return new Date((stamp('14:09:00') + i * 450 + fraction * 440) * 1000).toISOString().slice(11,23);
  }), '14:42:00'];
  const kinds = ['session', 'task', 'file-read', 'command', 'test', 'message'];
  const group = groups.find(g => g.sessionIds.includes(sid(agent)))!;
  let previous: string | null = null;
  clocks.forEach((clock, index) => {
    const id = event(agent, `agent-${agent}-${index}`, clock, kinds[index], index === 0 ? 'Session start' : index === 5 ? 'Loaded tail' : ['Task assigned', 'Read source', 'Run command', 'Test result'][index - 1], {
      task: group.name,
      body: `Illustrated ${kinds[index]} for agent-${agent} in ${group.name}. This authored example is not an exported transcript or a claim about the real profile.`,
    });
    if (previous) connect(previous, id);
    previous = id;
  });
}

// Close view and replay from plates 01/02: a parent with tester, reviewer and
// builder continuations. The root remains separate from the branching example.
const tailRoot = event(95, 'tail-message', '09:42:00', 'message', 'Message', { task: 'Loom event rendering' });
const tailSummary = event(95, 'tail-summary', '09:44:00', 'summary', 'Summary');
const tailTool = event(95, 'tail-tool', '09:46:00', 'tool', 'Tool call');
const tailCommand = event(95, 'tail-command', '09:47:00', 'command', 'bash');
const tailSpawn = event(95, 'tail-spawn', '09:51:00', 'spawn', 'Spawn tester');
const tailEdit = event(95, 'tail-edit', '09:58:00', 'file-edit', 'File edit', { file: 'src/loom.ts' });
const tailTask = event(95, 'tail-task', '10:00:00', 'task', 'In progress');
const tailBuild = event(95, 'tail-build-spawn', '10:01:00', 'spawn', 'Spawn builder');
const tailJunction = event(95, 'tail-rejoin', '10:05:00', 'rejoin', 'Rejoin tester and builder');
const tailFinalEdit = event(95, 'tail-final-edit', '10:06:00', 'file-edit', 'File edit', { file: 'src/loom.ts' });
const tailCommit = event(95, 'tail-commit', '10:07:00', 'commit', 'Commit');
const tailMessage = event(95, 'tail-ready', '10:08:00', 'message', 'PR ready · illustration');
[tailRoot, tailSummary, tailTool, tailCommand, tailSpawn, tailEdit, tailTask, tailBuild, tailJunction, tailFinalEdit, tailCommit, tailMessage].forEach((id, i, ids) => { if (i) connect(ids[i - 1], id); });
const tester = event(96, 'tester-read', '09:52:00', 'file-read', 'File read', { file: '/tests/loom.test.ts' });
const test = event(96, 'tester-test', '09:54:00', 'test', 'vitest');
const result = event(96, 'tester-result', '09:56:00', 'result', '18 passed · illustration');
connect(tailSpawn, tester, 'spawn'); connect(tester, test); connect(test, result); connect(result, tailJunction, 'rejoin');
const reviewer = event(97, 'reviewer-spawn', '09:52:00', 'spawn', 'Reviewer');
const reviewRead = event(97, 'reviewer-read', '09:53:00', 'file-read', 'File read', { file: 'docs/loom.md' });
const reviewSummary = event(97, 'reviewer-summary', '09:55:00', 'summary', 'Review notes');
const reviewReturn = event(97, 'reviewer-handoff', '09:57:00', 'handoff', 'Handoff to Loom');
connect(tailSpawn, reviewer, 'spawn'); connect(reviewer, reviewRead); connect(reviewRead, reviewSummary); connect(reviewSummary, reviewReturn); connect(reviewReturn, tailTask, 'handoff');
const buildCommand = event(98, 'builder-command', '10:02:00', 'command', 'pnpm build');
const buildTest = event(98, 'builder-test', '10:03:00', 'test', 'vitest');
const buildResult = event(98, 'builder-result', '10:04:00', 'result', '24 passed · illustration');
const buildReturn = event(98, 'builder-handoff', '10:05:00', 'handoff', 'Handoff to Loom');
connect(tailBuild, buildCommand, 'spawn'); connect(buildCommand, buildTest); connect(buildTest, buildResult); connect(buildResult, buildReturn); connect(buildReturn, tailJunction, 'handoff');

// Earlier episode columns are collapsed context, not invented off-page dust.
// Each miniature filament resolves to a dated authored event on the parent.
const episodes = [
  { id: 'design:episode:142', title: 'Refactor', at: stamp('09:26:00'), eventIds: [] as string[] },
  { id: 'design:episode:143', title: 'Auth Flow', at: stamp('09:31:00'), eventIds: [] as string[] },
  { id: 'design:episode:144', title: 'Add Tests', at: stamp('09:36:00'), eventIds: [] as string[] },
];
for (const episode of episodes) {
  const kinds = ['task', 'file-read', 'file-edit', 'command', 'test', 'summary'];
  episode.eventIds = kinds.map((kind, i) => event(95, `episode-${episode.id.split(':').at(-1)}-${kind}`,
    new Date((episode.at + i * 35) * 1000).toISOString().slice(11, 19), kind,
    `${episode.title} · ${kind}`, { task: episode.title,
      body: `Authored design episode: ${episode.title}, ${kind}. This illustrative source supports the collapsed earlier-episode column; it is not a captured transcript.`,
    }));
  episode.eventIds.forEach((id, i, ids) => { if (i) connect(ids[i - 1], id); });
}
for (let i = 0; i < episodes.length; i++) {
  connect(episodes[i].eventIds.at(-1)!, episodes[i + 1]?.eventIds[0] ?? tailRoot);
}

// Plate 03: success returns, failure remains retained, and nested work has no
// invented return. All three are intentionally represented, not auto-rejoined.
const branchSpawn = event(91, 'streaming-spawn', '09:41:22', 'spawn', 'streaming-ingest', { task: 'Streaming ingest' });
const branchRejoin = event(91, 'streaming-rejoin', '10:31:08', 'rejoin', 'Rejoin streaming-ingest');
const success = [
  event(92, 'metrics-task', '09:46:00', 'task', 'Add metrics emission'),
  event(92, 'metrics-session', '09:50:00', 'session', 'Metrics session'),
  event(92, 'metrics-worktree', '09:54:00', 'worktree', 'Metrics worktree'),
  event(92, 'metrics-command', '09:58:00', 'command', 'poetry run mypy src'),
  event(92, 'metrics-edit', '10:02:00', 'file-edit', 'File edit', { file: 'metrics/emitter.py' }),
  event(92, 'metrics-test', '10:06:00', 'test', '18 passed · illustration'),
  event(92, 'metrics-handoff', '10:12:11', 'handoff', 'Handoff metrics result'),
];
const failure = [
  event(93, 'buffer-task', '09:46:00', 'task', 'Refactor buffer management'),
  event(93, 'buffer-session', '09:50:00', 'session', 'Buffer session'),
  event(93, 'buffer-worktree', '09:54:00', 'worktree', 'Buffer worktree'),
  event(93, 'buffer-command', '09:58:00', 'command', 'pytest -k buffer'),
  event(93, 'buffer-edit', '10:02:00', 'file-edit', 'File edit', { file: 'buffer.py' }),
  event(93, 'buffer-test', '10:06:00', 'test', '3 failed · illustration'),
  event(93, 'buffer-failed', '10:10:55', 'result', 'Failed · evidence retained'),
];
const nested = [
  event(94, 'race-task', '10:00:00', 'task', 'Isolate race condition'),
  event(94, 'race-session', '10:02:00', 'session', 'Race session'),
  event(94, 'race-worktree', '10:04:00', 'worktree', 'Race worktree'),
  event(94, 'race-command', '10:06:00', 'command', 'pytest -k race -vv'),
  event(94, 'race-result', '10:08:00', 'result', 'Race reproduced with fixture adjustment'),
  event(94, 'race-no-return', '10:11:00', 'gap', 'Return unavailable', { body: 'Design example: this branch has no recorded handoff or rejoin. The result remains visible without inventing its return to the parent.' }),
];
for (const chain of [success, failure, nested]) chain.forEach((id, i) => { if (i) connect(chain[i - 1], id, 'sequence', id.endsWith('no-return') ? 'unavailable' : 'exact'); });
connect(branchSpawn, success[0], 'spawn'); connect(branchSpawn, failure[0], 'spawn'); connect(failure[3], nested[0], 'spawn'); connect(success[6], branchRejoin, 'rejoin');

// Plate 05 contains example source text. This is explicitly the illustration's
// excerpt, not claimed to be a captured file, transcript, test or commit.
const decision = event(27, 'render-decision', '14:31:58.112', 'decision', 'Explicit decision', { task: 'Dashboard review', body: 'Illustrated persisted decision: keep pinned events visible when they fall outside the visible time window. No private reasoning is included.' });
const visibleTask = event(27, 'render-task', '14:10:02.441', 'task', 'Visible task', { task: 'Dashboard review', body: 'Illustrated task: correct event visibility in the timeline canvas.' });
const read = event(27, 'render-read', '14:31:02.119', 'file-read', 'Read source', { file: 'dashboard/src/components/TimelineCanvas.tsx' });
const selectedEventId = event(27, 'timeline-file-edited', '14:32:17.803', 'file-edit', 'file.edited', {
  task: 'Dashboard review', file: 'dashboard/src/components/TimelineCanvas.tsx',
  body: 'DESIGN ILLUSTRATION — example transcript, not a profile capture.\nAdjust event rendering guard to avoid drawing entries beyond the visible time window while retaining pinned and forced events.',
  diff: 'Illustrated hunk · dashboard/src/components/TimelineCanvas.tsx\n256  const withinWindow = event.time >= windowStart && event.time <= windowEnd;\n257  const isPinned = pinnedIds.has(event.id);\n-258 if (!withinWindow && !isPinned) {\n+258 if (!withinWindow && !isPinned && !event.forceRender) {\n259    return;\n260  }',
});
const symbol = event(27, 'timeline-symbol', '14:32:18.021', 'file-edit', 'Changed symbol · TimelineCanvas', { file: 'dashboard/src/components/TimelineCanvas.tsx' });
const testRun = event(27, 'timeline-test', '14:33:02.441', 'test', 'Test run · 1 failed, 42 passed', { body: 'Illustrated test result. Attribution to the selected edit is inferred; no exact invocation-to-edit link is supplied.' });
const resultMessage = event(27, 'timeline-result', '14:33:03.122', 'message', 'Tests failed in TimelineCanvas', { body: 'Illustrated result message. Association with the selected edit is inferred, not exact.' });
const commit = event(27, 'timeline-commit', '14:34:12.778', 'commit', 'Commit · windowing guard', { body: 'Illustrated local commit. No real commit hash or provider URL is claimed.' });
const delivery = event(27, 'timeline-delivery-gap', '14:38:07.902', 'gap', 'Delivery review unavailable', { body: 'Illustrated missing review source. Refresh/re-ingest is not configured for this authored scenario.' });
for (const id of [decision, visibleTask, read]) connect(id, selectedEventId, 'cause', 'explicit');
connect(selectedEventId, symbol, 'cause'); connect(selectedEventId, testRun, 'cause', 'inferred'); connect(selectedEventId, resultMessage, 'cause', 'inferred'); connect(selectedEventId, commit, 'cause'); connect(selectedEventId, delivery, 'cause', 'unavailable');
const ambiguous = event(27, 'ambiguous-attribution', '14:33:04', 'gap', 'Ambiguous agent attribution', {
  body: 'Design example: candidates agent-27 and agent-19 remain unresolved. A local adjudication records a view; it does not manufacture a source link.',
  candidates: [27, 19].map(agent => ({sessionId:sid(agent), label:`agent-${agent}`, grade:'ambiguous', claim:'Authored candidate only. Session identity is present; authorship of the disputed result is not proven.'})),
});
const stale = event(27, 'stale-review', '14:36:00', 'gap', 'Stale review source', { body: 'Design example: source last checked at 14:20:00Z. Its state after that time is unavailable. This is not current provider data.' });
connect(resultMessage, ambiguous, 'cause', 'ambiguous'); connect(commit, stale, 'cause', 'stale');
const captureGap = event(27, 'transcript-capture-gap', '14:24:11', 'gap', 'Transcript not ingested · 11m 42s', {
  body:'Authored coverage example: agent-27 transcript capture is unavailable from 14:12:29 to 14:24:11. Records before and after remain available. No transcript contents or activity inside the interval are inferred.',
});
const coverageGaps = [{eventId:captureGap, sessionId:sid(27), from:stamp('14:12:29'), to:stamp('14:24:11'), label:'TRANSCRIPT NOT INGESTED · 11m 42s'}];

// Brief 06: an authored historical review is separate from present-day notes.
// Its lifecycle is supported by named example events and explicit relationships;
// a newly saved local comment can never inherit this illustrated outcome.
const feedbackEvent = event(27, 'review-challenge', '14:34:02', 'message', 'Challenge assumption', { task: 'Dashboard review', body: 'DESIGN ILLUSTRATION — authored reviewer note. What guarantees pinned events remain reachable outside the visible window? Target: timeline-file-edited, illustrated hunk L258.' });
events.find(e => e.id === feedbackEvent)!.role = 'illustrated-reviewer';
const reopened = event(27, 'review-task-reopened', '14:34:18', 'task', 'Task reopened', { task: 'Dashboard review', body: 'DESIGN ILLUSTRATION — acknowledgement of review-challenge. Reopen the event-visibility task and verify the pinned-event path.' });
const reviewSpawn = event(27, 'review-subagent-spawned', '14:34:33', 'spawn', 'Subagent spawned', { task: 'Dashboard review', body: 'Illustrated explicit delegation to agent-07 to address review-challenge.' });
const revision = event(7, 'review-revision-edit', '14:36:21', 'file-edit', 'Revision edit · pinned lane', { task: 'Dashboard review', file: 'dashboard/src/components/TimelineCanvas.tsx', body: 'DESIGN ILLUSTRATION — this revision explicitly addresses review-challenge by rendering pinned events in the pinned lane independently of the time window.', diff: 'Illustrated revision · not a captured repository diff\n318 // Pinned events remain reachable in the pinned lane.\n+319 if (isPinned) {\n+320   renderPinned(event);\n+321 }' });
const focusedTest = event(7, 'review-focused-test', '14:37:45', 'test', 'Focused test passed', { task: 'Dashboard review', body: 'Illustrated test: an out-of-window pinned event remains available in the pinned lane. Authored relation explicitly targets review-revision-edit; this is not a real test run.' });
const reviewHandoff = event(7, 'review-handoff', '14:38:08', 'handoff', 'Handoff returned', { task: 'Dashboard review', body: 'Illustrated handoff to agent-27: revision and focused test address review-challenge.' });
const amendment = event(27, 'review-commit-amended', '14:39:12', 'commit', 'Commit amended', { task: 'Dashboard review', body: 'Illustrated commit amendment includes review-revision-edit and its focused test. No real commit hash or provider write is claimed.' });
connect(selectedEventId, feedbackEvent, 'cause', 'explicit');
connect(feedbackEvent, reopened, 'cause', 'explicit');
connect(reopened, reviewSpawn, 'spawn', 'explicit');
connect(reviewSpawn, revision, 'cause', 'explicit');
connect(revision, focusedTest, 'cause', 'exact');
connect(focusedTest, reviewHandoff, 'handoff', 'explicit');
connect(reviewHandoff, amendment, 'rejoin', 'exact');
const feedback: DesignFeedback[] = [{
  id: 'design:feedback:pinned-events', targetEventId: selectedEventId, sourceEventId: feedbackEvent,
  author: 'Illustrated local reviewer', marker: 'Challenge assumption',
  text: 'What guarantees pinned events remain reachable outside the visible window?',
  lifecycle: [
    { eventId: feedbackEvent, status: 'open', relationId: null },
    { eventId: reopened, status: 'acknowledged', relationId: relations.find(r => r.from === feedbackEvent && r.to === reopened)!.id },
    ...[reviewSpawn, revision, focusedTest, reviewHandoff, amendment].map(eventId => ({ eventId, status: (eventId === reviewSpawn ? 'acknowledged' : 'acted-upon') as 'acknowledged' | 'acted-upon', relationId: relations.find(r => r.to === eventId)!.id })),
  ],
}];

events.sort((a, b) => a.ts! - b.ts! || a.id.localeCompare(b.id));
for (const key of ['buffer-failed', 'race-no-return', 'timeline-test', 'review-challenge']) {
  const detail = details[`design:event:${key}`]; if (detail) detail.risk = 'high';
}
for (const key of ['buffer-failed', 'buffer-test', 'timeline-test']) details[`design:event:${key}`].outcome = 'failed';

const ordinal = new Map<string, number>();
for (const item of events) { item.ordinal = (ordinal.get(item.sessionId) ?? 0) + 1; ordinal.set(item.sessionId, item.ordinal); }
const parents: Record<number, number> = { 92: 91, 93: 91, 94: 93, 96: 95, 97: 95, 98: 95 };
const sessions: JourneySession[] = Array.from({ length: 103 }, (_, index) => {
  const agent = index + 1;
  const rows = events.filter(item => item.sessionId === sid(agent));
  const isSubagent = agent >= 83 && agent !== 91 && agent !== 95;
  return { id: sid(agent), provider: 'design-illustration', projectId: 'design:project:tracedecay', project: 'TraceDecay · design example', path: null,
    startedAt: rows[0].at, endedAt: rows.at(-1)!.at, startedTs: rows[0].ts, endedTs: rows.at(-1)!.ts,
    parentId: isSubagent ? sid(parents[agent] ?? 1) : null, isSubagent,
    agentId: `design:agent-${agent}`, messages: rows.length, title: agent === 91 ? 'streaming-ingest' : agent === 95 ? 'Loom event rendering' : `agent-${agent}`, coverage: 'spine-only', threadId: `design:thread:agent-${agent}` };
});

export const DESIGN_SCENARIO: {
  sessions: JourneySession[]; events: PackLoomEvent[];
  groups: { id: string; name: string; color: string; sessionIds: string[] }[];
  episodes: { id: string; title: string; at: number; eventIds: string[] }[];
  relations: DesignRelation[]; feedback: DesignFeedback[]; details: Record<string, DesignDetail>; notes: string[];
  coverageGaps: {eventId:string; sessionId:string; from:number; to:number; label:string}[];
  windows: { tail: [number, number]; dense: [number, number] }; selectedEventId: string;
} = {
  sessions, events, groups, episodes, relations, feedback, details, coverageGaps, selectedEventId,
  windows: { tail: [stamp('09:40:00'), stamp('10:31:08')], dense: [stamp('14:06:00'), stamp('14:42:00')] },
  notes: [
    'CONCEPT / SYNTHETIC DATA — authored from the five supplied Loom plates. This is not a TraceDecay profile capture.',
    '103 illustrative agent identities, including 19 subagents. Group participation counts are 34, 27, 18, 14, 11 and 19; 20 overlapping memberships make 123 participations, not 123 unique agents.',
    'The morning close-view example belongs to Other; the afternoon dense view reuses these identities. Time windows are explicit, never compressed into fabricated chronology.',
    'Exact and explicit grades describe the authored example only. Inferred, ambiguous, stale and unavailable relations remain separate; no grade asserts an observation about either real profile.',
    'Source bodies and the code hunk are labeled illustrations. No real provider result, commit hash, raw transcript, private reasoning or delivered feedback is fabricated.',
    'The failed branch retains its evidence. The nested race branch has no return; temporal proximity never creates a handoff or rejoin.',
  ],
};
