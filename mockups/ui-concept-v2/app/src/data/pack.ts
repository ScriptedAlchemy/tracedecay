import raw from "./pack-index.json";
import { PROFILE_SOURCE } from "./fixtures";

export type PackSession = {
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
  messages: number;
  kinds: Record<string, number>;
  roles: Record<string, number>;
  tools: Record<string, number>;
  tokens: null;
  coverage: "spine-only";
  status: string;
};

export type PackAgent = {
  sessionId: string;
  agentId: string;
  generation: number;
  projectId: string;
  project: string;
  createdAt: number;
};

export type PackLoomEvent = {
  id: string;
  sessionId: string;
  kind: string;
  role: string | null;
  tool: string | null;
  ts: number | null;
  at: string | null;
  ordinal: number | null;
};

export type PackProject = {
  id: string;
  name: string;
  sessions: number;
  messages: number;
  observations: number;
  agents: number;
  sessionThreads: number;
  graphVerifiedHeads: number | null;
  retrievalAnchors: number | null;
  factsTable: string | null;
  indexNote: string | null;
  root: string;
  role: string | null;
};

export type PackIndex = {
  kind: string;
  capturedAt: string;
  notes: string[];
  totals: {
    sessions: number;
    messages: number;
    agents: number;
    agentRows: number;
    prMarkers: number;
    loomEvents: number;
  };
  projects: PackProject[];
  sessions: PackSession[];
  agents: PackAgent[];
  loomEvents: PackLoomEvent[];
  prMarkers: {
    projectId: string;
    project: string;
    sessionId: string;
    messageId: string;
    at: string | null;
    grade: string;
    note: string;
  }[];
};

export const PACK = raw as unknown as PackIndex;

export function shortId(id: string, n = 12) {
  return id.length <= n ? id : `${id.slice(0, n)}…`;
}

export function providerCounts() {
  const m = new Map<string, number>();
  for (const s of PACK.sessions) m.set(s.provider, (m.get(s.provider) ?? 0) + 1);
  return [...m.entries()].sort((a, b) => b[1] - a[1]);
}

export function sessionById(id: string) {
  return PACK.sessions.find((s) => s.id === id) ?? null;
}

export const PROFILE = PROFILE_SOURCE;
