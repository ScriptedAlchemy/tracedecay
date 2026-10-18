import { PACK, type PackAgent, type PackLoomEvent, type PackSession } from "../data/pack";

export const SESSION_IDS = new Set(PACK.sessions.map((s) => s.id));

export type AgentNode = {
  session: PackSession;
  depth: number;
  children: AgentNode[];
  agents: PackAgent[];
  parentInSnapshot: boolean;
};

function agentsFor(sessionId: string): PackAgent[] {
  return PACK.agents.filter((a) => a.sessionId === sessionId);
}

function depthOf(s: PackSession, byId: Map<string, PackSession>, seen: Set<string>): number {
  if (seen.has(s.id)) return 0;
  seen.add(s.id);
  if (!s.parentId) return 0;
  const parent = byId.get(s.parentId);
  if (!parent) return 1;
  return 1 + depthOf(parent, byId, seen);
}

function buildForest(): AgentNode[] {
  const byId = new Map(PACK.sessions.map((s) => [s.id, s]));
  const nodes = new Map<string, AgentNode>();
  for (const s of PACK.sessions) {
    nodes.set(s.id, {
      session: s,
      depth: depthOf(s, byId, new Set()),
      children: [],
      agents: agentsFor(s.id),
      parentInSnapshot: Boolean(s.parentId && SESSION_IDS.has(s.parentId)),
    });
  }
  const roots: AgentNode[] = [];
  for (const n of nodes.values()) {
    if (n.parentInSnapshot && n.session.parentId) {
      nodes.get(n.session.parentId)!.children.push(n);
    } else {
      roots.push(n);
    }
  }
  for (const n of nodes.values()) {
    n.children.sort((a, b) => b.session.messages - a.session.messages);
  }
  roots.sort((a, b) => {
    const ac = a.children.length;
    const bc = b.children.length;
    if (bc !== ac) return bc - ac;
    return b.session.messages - a.session.messages;
  });
  return roots;
}

export const FOREST = buildForest();
export const FAMILIES = FOREST.filter((n) => n.children.length > 0);
export const UNLINKED = FOREST.filter((n) => n.children.length === 0 && !n.session.parentId);
export const DANGLING = FOREST.filter((n) => n.session.parentId && !n.parentInSnapshot);

export const PARENT_LINKS = PACK.sessions.filter((s) => s.parentId).length;
export const PARENT_RESOLVED = PACK.sessions.filter((s) => s.parentId && SESSION_IDS.has(s.parentId)).length;
export const PARENT_MISSING = PARENT_LINKS - PARENT_RESOLVED;

export const MAX_LAYOUT_DEPTH = Math.max(0, ...FOREST.flatMap(function walk(n: AgentNode): number[] {
  return [n.depth, ...n.children.flatMap(walk)];
}));

export const PACK_GENERATIONS = [...new Set(PACK.agents.map((a) => a.generation))].sort((a, b) => a - b);

export type NamedCount = { name: string; n: number; pct: number };

function namedCounts(entries: [string, number][]): NamedCount[] {
  const total = entries.reduce((s, [, n]) => s + n, 0) || 1;
  return entries
    .sort((a, b) => b[1] - a[1])
    .map(([name, n]) => ({ name, n, pct: (n / total) * 100 }));
}

function tally(pick: (s: PackSession) => Record<string, number>): NamedCount[] {
  const m = new Map<string, number>();
  for (const s of PACK.sessions) {
    for (const [k, v] of Object.entries(pick(s))) m.set(k, (m.get(k) ?? 0) + v);
  }
  return namedCounts([...m.entries()]);
}

export const KIND_COUNTS = tally((s) => s.kinds);
export const TOOL_COUNTS = tally((s) => s.tools);

export const RECENT_TOOLS: PackLoomEvent[] = [...PACK.loomEvents]
  .filter((e) => e.tool)
  .sort((a, b) => (b.ts ?? 0) - (a.ts ?? 0));

export const CAPTURED_AT = PACK.capturedAt;

export const TOTALS = {
  sessions: PACK.totals.sessions,
  messages: PACK.totals.messages,
  agents: PACK.totals.agents,
  agentRows: PACK.totals.agentRows,
  families: FAMILIES.length,
  unlinked: UNLINKED.length,
  dangling: DANGLING.length,
};

export function nodeById(id: string): AgentNode | null {
  const stack = [...FOREST];
  while (stack.length) {
    const n = stack.pop()!;
    if (n.session.id === id) return n;
    stack.push(...n.children);
  }
  return null;
}

export function packGeneration(n: AgentNode): string {
  const gens = [...new Set(n.agents.map((a) => a.generation))];
  if (!gens.length) return "unavailable";
  return gens.join(", ");
}

export function defaultSelectedId(): string {
  const richest = FAMILIES[0]?.children[0] ?? FAMILIES[0] ?? FOREST[0];
  return richest?.session.id ?? "";
}

export function clock(at: string | null): string {
  if (!at) return "—";
  const m = at.match(/(\d{2}:\d{2}:\d{2})/);
  return m ? m[1] : at.replace(/ UTC$/, "");
}
