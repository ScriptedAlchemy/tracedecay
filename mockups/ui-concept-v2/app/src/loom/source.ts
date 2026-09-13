import { DESIGN_SCENARIO } from "./designScenario";
import { PACK, type PackLoomEvent } from "../data/pack";
import ubuntuSessions from "../../profile-pack/ubuntu-main/proj_a5b3d7e3ebe14ca7/sessions.json";
import ubuntuAgents from "../../profile-pack/ubuntu-main/proj_a5b3d7e3ebe14ca7/session_agents.json";
import ubuntuThreads from "../../profile-pack/ubuntu-main/proj_a5b3d7e3ebe14ca7/session_threads.json";

export type JourneySession = {
  id: string;
  provider: string;
  projectId: string;
  project: string;
  path: string | null;
  startedAt: string | null;
  endedAt: string | null;
  startedTs: number | null;
  endedTs: number | null;
  parentId: string | null;
  isSubagent: boolean;
  agentId: string | null;
  messages: number | null;
  title: string | null;
  coverage: "spine-only" | "identity-only";
  threadId: string | null;
};

export const SOURCE_OPTIONS = [
  { id: "mac", label: "Mac profile · event spine" },
  { id: "ubuntu", label: "Ubuntu profile · session identities" },
  { id: "design", label: "Design example · synthetic" },
] as const;

const requestedSource = typeof window === "undefined" ? null : (new URLSearchParams(window.location.search).get("data") === "fixture" ? "design" : new URLSearchParams(window.location.search).get("loom_source"));
export const SOURCE_ID: "mac" | "ubuntu" | "design" = requestedSource === "ubuntu" || requestedSource === "design" ? requestedSource : "mac";

const params = typeof window === 'undefined' ? new URLSearchParams() : new URLSearchParams(window.location.search);
const requestedPage = params.get('loom_page');
const frame = params.get('state') ?? '01';
const designPage = requestedPage === 'tail' || requestedPage === 'morning' || requestedPage === 'full' ? requestedPage : ['01', '02'].includes(frame) ? 'tail' : frame === '03' ? 'morning' : 'full';
// This loaded prefix ends at the last authored morning parent event. Later
// branch/afternoon records require another page; it is not a global live tail.
const tailEnd = Math.max(...DESIGN_SCENARIO.events.filter(event => event.sessionId === 'design:session:agent-095' && event.ts !== null && event.ts <= DESIGN_SCENARIO.windows.tail[1]).map(event => event.ts!));
const designEnd = designPage === 'tail' ? tailEnd : designPage === 'morning' ? DESIGN_SCENARIO.windows.tail[1] : DESIGN_SCENARIO.windows.dense[1];
const designEvents = DESIGN_SCENARIO.events.filter(event => event.ts !== null && event.ts <= designEnd);
const designIds = new Set(designEvents.map(event => event.id));
const designSessions = DESIGN_SCENARIO.sessions.filter(session => session.startedTs !== null && session.startedTs <= designEnd).map(session => ({ ...session,
  endedTs: session.endedTs !== null && session.endedTs <= designEnd ? session.endedTs : null,
  endedAt: session.endedTs !== null && session.endedTs <= designEnd ? session.endedAt : null,
  messages: null,
}));
const loadedDesign = { ...DESIGN_SCENARIO, sessions: designSessions, events: designEvents,
  relations: DESIGN_SCENARIO.relations.filter(relation => designIds.has(relation.from) && designIds.has(relation.to)),
  details: Object.fromEntries(Object.entries(DESIGN_SCENARIO.details).filter(([id]) => designIds.has(id))),
  feedback: DESIGN_SCENARIO.feedback.filter(note => designIds.has(note.sourceEventId)),
  coverageGaps: DESIGN_SCENARIO.coverageGaps.filter(gap => designIds.has(gap.eventId)),
};

// Repeated generations record the same native thread identity. Conflicting
// identities stay unavailable rather than silently choosing an association.
const nativeThreads = new Map<string, Set<string>>();
for (const row of ubuntuThreads) {
  if (JSON.parse(row.grouping_provenance).kind !== "provider_native") continue;
  const ids = nativeThreads.get(row.session_id) ?? new Set<string>();
  ids.add(row.thread_id);
  nativeThreads.set(row.session_id, ids);
}

export const SOURCE: {
  id: "mac" | "ubuntu" | "design";
  design?: typeof DESIGN_SCENARIO;
  page?: 'tail' | 'morning' | 'full';
  loadedBounds?: [number, number];
  label: string;
  capturedAt: string | null;
  sessions: JourneySession[];
  events: PackLoomEvent[];
  agents: { sessionId: string; agentId: string }[];
  notes: string[];
} = SOURCE_ID === "design" ? {
  id: "design", label: SOURCE_OPTIONS[2].label, capturedAt: null,
  page: designPage, loadedBounds: [Math.min(...designEvents.flatMap(event=>event.ts===null?[]:[event.ts])), designEnd],
  sessions: designSessions, events: designEvents,
  agents: designSessions.flatMap(s => s.agentId ? [{sessionId: s.id, agentId: s.agentId}] : []),
  notes: [`${designPage.toUpperCase()} EXAMPLE PAGE · ${designEvents.length} illustrative events loaded through ${new Date(designEnd * 1000).toISOString()}.`, ...DESIGN_SCENARIO.notes.filter((_, i) => designPage === 'full' || i !== 1)], design: loadedDesign,
} : SOURCE_ID === "ubuntu" ? {
  id: "ubuntu",
  label: SOURCE_OPTIONS[1].label,
  capturedAt: null,
  sessions: ubuntuSessions.map((row): JourneySession => {
    const threads = nativeThreads.get(row.session_id);
    return {
      id: row.session_id,
      provider: row.provider,
      projectId: row.project_key,
      project: "tracedecay",
      path: row.project_path,
      startedAt: row.started_at == null ? null : new Date(row.started_at * 1000).toISOString(),
      endedAt: row.ended_at == null ? null : new Date(row.ended_at * 1000).toISOString(),
      startedTs: row.started_at,
      endedTs: row.ended_at,
      parentId: row.parent_session_id,
      isSubagent: row.is_subagent === 1,
      agentId: row.agent_id,
      messages: null,
      title: row.title,
      coverage: "identity-only",
      threadId: threads?.size === 1 ? [...threads][0] : null,
    };
  }),
  events: [],
  agents: ubuntuAgents.map((row) => ({ sessionId: row.session_id, agentId: row.agent_id })),
  notes: [
    "Ubuntu-main is a separate profile; its identities are not joined to Mac events.",
    "The source README reports capture at 2026-08-30 ~1:16 AM PT; an exact capture timestamp is unavailable.",
    "Identity-only export: message counts, event spines, transcript bodies, observations and outcomes are unavailable.",
    "Agent association rows may contain multiple identities per session; unique agents are counted from distinct agent IDs.",
    "Native thread IDs and parent session IDs are recorded associations, not inferred workstream names or outcomes.",
  ],
} : {
  id: "mac",
  label: SOURCE_OPTIONS[0].label,
  capturedAt: PACK.capturedAt,
  sessions: PACK.sessions.map((session) => ({ ...session, title: null, threadId: null })),
  events: PACK.loomEvents,
  agents: PACK.agents.map(({ sessionId, agentId }) => ({ sessionId, agentId })),
  notes: PACK.notes,
};
