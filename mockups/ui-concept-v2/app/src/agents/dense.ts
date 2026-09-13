export type FixtureStatus = "active" | "ended" | "failed" | "ambiguous";
export type RelationKind = "parentage" | "handoff" | "rejoin";
export type EvidenceGrade = "EXACT" | "EXPLICIT";

export type FixtureAgent = {
  id: string;
  label: string;
  branch: string;
  depth: number;
  parentId: string | null;
  events: number;
  status: FixtureStatus;
};

export type FixtureRelation = {
  id: string;
  from: string;
  to: string;
  kind: RelationKind;
  grade: EvidenceGrade;
  source: string;
};

export type FixtureBranch = {
  id: string;
  label: string;
  outcome: string;
  agents: FixtureAgent[];
  events: number;
  failures: number;
  gaps: number;
};

const BRANCH_SPECS = [
  ["ingest", "INGEST ROUTING", "indexed corpus"],
  ["graph", "GRAPH ASSEMBLY", "sealed graph"],
  ["rank", "RANK + RETRIEVAL", "ranked evidence"],
  ["memory", "MEMORY HYDRATION", "durable context"],
  ["review", "CHANGE REVIEW", "review decision"],
  ["delivery", "DELIVERY", "release outcome"],
] as const;

const MISSING_PARENTS = new Map([
  ["ingest-17", "missing:external-parser"],
  ["memory-18", "missing:history-loader"],
  ["review-16", "missing:policy-reviewer"],
  ["delivery-19", "missing:release-agent"],
]);

function statusFor(branch: number, index: number): FixtureStatus {
  if ((branch * 20 + index) % 29 === 0) return "failed";
  if ((branch * 20 + index) % 23 === 0) return "ambiguous";
  return index % 5 === 0 ? "ended" : "active";
}

const agents: FixtureAgent[] = [{
  id: "fixture-root",
  label: "brain-core",
  branch: "control",
  depth: 0,
  parentId: null,
  events: 184_221,
  status: "active",
}];

BRANCH_SPECS.forEach(([branch, label], branchIndex) => {
  for (let index = 0; index < 20; index += 1) {
    const id = `${branch}-${String(index).padStart(2, "0")}`;
    const parentId = index === 0
      ? "fixture-root"
      : index <= 4
        ? `${branch}-00`
        : `${branch}-${String(((index - 5) % 4) + 1).padStart(2, "0")}`;
    agents.push({
      id,
      label: index === 0 ? label.toLowerCase().replaceAll(" ", "-") : `${branch}-${String(index).padStart(2, "0")}`,
      branch,
      depth: index === 0 ? 1 : index <= 4 ? 2 : 3,
      parentId: MISSING_PARENTS.get(id) ?? parentId,
      events: 900 + branchIndex * 137 + index * 83,
      status: statusFor(branchIndex, index),
    });
  }
});

const parentage: FixtureRelation[] = agents.flatMap((agent) => agent.parentId ? [{
  id: `parent:${agent.id}`,
  from: agent.parentId,
  to: agent.id,
  kind: "parentage" as const,
  grade: "EXACT" as const,
  source: `session:${agent.id}.parent_session_id`,
}] : []);

const handoffs: FixtureRelation[] = [
  ["ingest-17", "graph-02"], ["graph-18", "rank-03"], ["rank-16", "memory-04"],
  ["memory-19", "review-02"], ["review-18", "delivery-03"], ["delivery-17", "review-04"],
].map(([from, to], index) => ({
  id: `handoff:${index + 1}`,
  from,
  to,
  kind: "handoff",
  grade: "EXACT",
  source: `handoff-ledger:fx-${String(index + 1).padStart(3, "0")}`,
}));

const rejoins: FixtureRelation[] = [
  ["ingest-19", "ingest-00"], ["graph-17", "graph-00"], ["rank-18", "rank-00"],
  ["memory-17", "memory-00"], ["review-19", "review-00"], ["delivery-18", "fixture-root"],
].map(([from, to], index) => ({
  id: `rejoin:${index + 1}`,
  from,
  to,
  kind: "rejoin",
  grade: "EXPLICIT",
  source: `result-ledger:fx-r${String(index + 1).padStart(2, "0")}`,
}));

export const FIXTURE_AGENTS = agents;
export const FIXTURE_RELATIONS = [...parentage, ...handoffs, ...rejoins];
export const FIXTURE_BRANCHES: FixtureBranch[] = BRANCH_SPECS.map(([id, label, outcome]) => {
  const members = agents.filter((agent) => agent.branch === id);
  return {
    id,
    label,
    outcome,
    agents: members,
    events: members.reduce((sum, agent) => sum + agent.events, 0),
    failures: members.filter((agent) => agent.status === "failed").length,
    gaps: members.filter((agent) => agent.parentId?.startsWith("missing:")).length,
  };
});

export const FIXTURE_TOTALS = {
  agents: FIXTURE_AGENTS.length,
  subagents: FIXTURE_AGENTS.filter((agent) => agent.parentId).length,
  events: FIXTURE_AGENTS.reduce((sum, agent) => sum + agent.events, 0),
  relations: FIXTURE_RELATIONS.length,
  gaps: MISSING_PARENTS.size,
};

export function fixtureAgent(id: string | null) {
  return id ? FIXTURE_AGENTS.find((agent) => agent.id === id) ?? null : null;
}

export function fixtureRelation(id: string | null) {
  return id ? FIXTURE_RELATIONS.find((relation) => relation.id === id) ?? null : null;
}

export function pathToRoot(id: string | null) {
  const path = new Set<string>();
  let current = fixtureAgent(id);
  while (current && !path.has(current.id)) {
    path.add(current.id);
    current = fixtureAgent(current.parentId);
  }
  return path;
}
