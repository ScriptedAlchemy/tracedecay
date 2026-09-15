import { PACK, type PackLoomEvent, type PackSession } from "../data/pack";

/**
 * The profile pack has no planned Work graph (no tasks, no dependencies).
 * The only honest DAG in the snapshot is the observed delegation spine:
 * parent_session_id links between root threads and subagent threads.
 * Everything below is derived from those records — nothing is invented.
 */

const SESSIONS = PACK.sessions;
const BY_ID = new Map(SESSIONS.map((s) => [s.id, s]));

export type ThreadNode = {
  kind: "session" | "ghost";
  id: string;
  session: PackSession | null;
  /** generation from the pack agents table, null when no agent row exists */
  generation: number | null;
  /** parent link grade: EXACT (resolved in snapshot) | ABSENT (recorded, not copied) | none */
  parentGrade: "EXACT" | "ABSENT" | "none";
};

export type Family = {
  key: string;
  project: string;
  /** root node — a real session, or a ghost when the recorded parent was not copied */
  root: ThreadNode;
  children: ThreadNode[];
  messages: number;
  edgeStyle: "solid" | "dashed";
};

function generationOf(sessionId: string): number | null {
  // agents rows key the spawned session by sessionId; agentId names the agent identity
  const row = PACK.agents.find((a) => a.sessionId === sessionId);
  return row ? row.generation : null;
}

function sessionNode(s: PackSession): ThreadNode {
  const grade = s.parentId ? (BY_ID.has(s.parentId) ? "EXACT" : "ABSENT") : "none";
  return { kind: "session", id: s.id, session: s, generation: generationOf(s.id), parentGrade: grade };
}

function buildFamilies(): Family[] {
  const byParent = new Map<string, PackSession[]>();
  for (const s of SESSIONS) {
    if (!s.parentId) continue;
    const list = byParent.get(s.parentId) ?? [];
    list.push(s);
    byParent.set(s.parentId, list);
  }
  const fams: Family[] = [];
  for (const [parentId, kids] of byParent) {
    const root = BY_ID.get(parentId) ?? null;
    const children = kids
      .slice()
      .sort((a, b) => (a.startedTs ?? 0) - (b.startedTs ?? 0))
      .map(sessionNode);
    const project = root?.project ?? kids[0].project;
    const messages = (root?.messages ?? 0) + kids.reduce((n, k) => n + k.messages, 0);
    fams.push({
      key: parentId,
      project,
      root: root
        ? sessionNode(root)
        : { kind: "ghost", id: parentId, session: null, generation: null, parentGrade: "none" },
      children,
      messages,
      edgeStyle: root ? "solid" : "dashed",
    });
  }
  fams.sort((a, b) => b.messages - a.messages);
  return fams;
}

export const FAMILIES = buildFamilies();
export const DEFAULT_FAMILY = FAMILIES[0];

export function familyById(key: string): Family {
  return FAMILIES.find((f) => f.key === key) ?? DEFAULT_FAMILY;
}

export function nodeInFamily(fam: Family, id: string): ThreadNode | null {
  if (fam.root.id === id) return fam.root;
  return fam.children.find((c) => c.id === id) ?? null;
}

/** chunk children into rows of at most 3 so tiers read like the plate board */
export function childTiers(fam: Family): ThreadNode[][] {
  const rows: ThreadNode[][] = [];
  for (let i = 0; i < fam.children.length; i += 3) rows.push(fam.children.slice(i, i + 3));
  return rows;
}

function topToolName(s: PackSession): string | null {
  const entries = Object.entries(s.tools);
  if (!entries.length) return null;
  entries.sort((a, b) => b[1] - a[1]);
  return entries[0][0];
}

/**
 * Card title in the plate's task-plate voice, still derived from records:
 * the thread kind plus the real dominant tool of its observed spine.
 * No invented task names — "Shell-heavy" is a tally, not a plan.
 */
export function cardTitle(n: ThreadNode): string {
  if (n.kind === "ghost") return "Recorded parent thread";
  const s = n.session!;
  const tool = topToolName(s);
  if (!s.isSubagent) return "Delegating root thread";
  return tool ? `${tool}-heavy subagent` : "Subagent thread";
}

export function threadStatus(n: ThreadNode): { label: string; tone: "cyan" | "amber" } {
  if (n.kind === "ghost") return { label: "NOT IN SNAPSHOT", tone: "amber" };
  return { label: "OBSERVED", tone: "cyan" };
}

/** real tool tallies for the inspector, densest first */
export function toolSummary(s: PackSession | null): string {
  if (!s) return "unavailable";
  const entries = Object.entries(s.tools).sort((a, b) => b[1] - a[1]);
  if (!entries.length) return "no tools";
  const head = entries.slice(0, 2).map(([k, v]) => `${k}×${v}`);
  const rest = entries.length - head.length;
  return rest > 0 ? `${head.join(" · ")} · +${rest}` : head.join(" · ");
}

export function spanLabel(s: PackSession | null): string {
  if (!s || s.startedTs == null) return "—";
  const end = s.endedTs ?? s.startedTs;
  const mins = Math.round((end - s.startedTs) / 60);
  if (mins <= 0) return "<1m";
  if (mins < 60) return `${mins}m`;
  const h = Math.floor(mins / 60);
  return `${h}h ${mins - h * 60}m`;
}

export function topTool(s: PackSession | null): string {
  if (!s) return "—";
  const entries = Object.entries(s.tools);
  if (!entries.length) return "no tools";
  entries.sort((a, b) => b[1] - a[1]);
  return `${entries[0][0]}×${entries[0][1]}`;
}

export function clock(at: string | null): string {
  if (!at) return "—";
  const m = at.match(/(\d{2}:\d{2}:\d{2})/);
  return m ? m[1] : at;
}

export type ActivityRow = {
  key: string;
  at: string;
  project: string;
  sessionId: string;
  event: string;
  detail: string;
  tone: "cyan" | "amber" | "dim";
};

function eventLabel(e: PackLoomEvent): { event: string; detail: string; tone: ActivityRow["tone"] } {
  if (e.kind === "tool_invocation") return { event: `tool.${e.tool ?? "?"}`, detail: "assistant call", tone: "cyan" };
  if (e.kind === "git_pull_request") return { event: "git.pull_request", detail: "EXACT marker · no body", tone: "amber" };
  if (e.kind === "git_branch") return { event: "git.branch", detail: "EXACT marker", tone: "amber" };
  if (e.kind === "reasoning_visible") return { event: "reasoning.visible", detail: "body not copied", tone: "dim" };
  return { event: `message.${e.role ?? "?"}`, detail: "body not copied", tone: "dim" };
}

/** true when any copied spine event belongs to one of the given sessions */
export function hasFamilyActivity(ids: Set<string>): boolean {
  return PACK.loomEvents.some((e) => ids.has(e.sessionId));
}

/** latest distinct spine events across the snapshot, oldest first for display */
export function recentActivity(n: number): ActivityRow[] {
  const seen = new Set<string>();
  const rows: ActivityRow[] = [];
  const sorted = [...PACK.loomEvents].sort((a, b) => (b.ts ?? 0) - (a.ts ?? 0));
  for (const e of sorted) {
    const dedupe = `${e.at}·${e.kind}·${e.tool}·${e.role}·${e.sessionId}`;
    if (seen.has(dedupe)) continue;
    seen.add(dedupe);
    const s = BY_ID.get(e.sessionId);
    const lab = eventLabel(e);
    rows.push({
      key: e.id,
      at: e.at ?? "—",
      project: s?.project ?? "tracedecay",
      sessionId: e.sessionId,
      event: lab.event,
      detail: lab.detail,
      tone: lab.tone,
    });
    if (rows.length >= n) break;
  }
  return rows.reverse();
}

export const WORK_PROJECTIONS = ["DAG", "TIMELINE", "CAUSAL", "WORKLOAD", "TOPOLOGY"] as const;
export type WorkProjection = (typeof WORK_PROJECTIONS)[number];
export type WorkStatus = "proposed" | "ready" | "running" | "waiting" | "blocked" | "review" | "done" | "failed";
export type RelationKind = "gating" | "informational" | "parallel" | "observed" | "delegation";
export type WorkAction = "accept" | "admit" | "replan" | "complete" | "retry";

export type WorkAttempt = {
  id: string;
  status: "running" | "failed" | "succeeded";
  executor: string;
  sessionId: string;
  source: "FIXTURE / EXPLICIT";
};

export type WorkTask = {
  id: string;
  planId: string;
  title: string;
  component: string;
  owner: string;
  priority: "P0" | "P1" | "P2" | "P3";
  estimate: string;
  status: WorkStatus;
  depth: number;
  sequence: number;
  attempts: WorkAttempt[];
  source: "FIXTURE / EXPLICIT";
};

export type WorkRelation = {
  id: string;
  from: string;
  to: string;
  kind: RelationKind;
  label: string;
  grade: "EXPLICIT" | "EXACT";
};

export type WorkEvent = {
  id: string;
  taskId: string;
  at: string;
  event: string;
  detail: string;
};

export type WorkGraph = {
  revision: number;
  tasks: WorkTask[];
  relations: WorkRelation[];
  events: WorkEvent[];
};

export const FIXTURE_PLANS = [
  { id: "PLAN-CFG", title: "Configuration cutover", component: "svc-config" },
  { id: "PLAN-EVID", title: "Attempt evidence", component: "work-runtime" },
  { id: "PLAN-SHIP", title: "Provider delivery", component: "release" },
] as const;

const task = (
  id: string,
  planId: string,
  title: string,
  component: string,
  owner: string,
  priority: WorkTask["priority"],
  estimate: string,
  status: WorkStatus,
  depth: number,
  sequence: number,
  attempts: WorkAttempt[] = [],
): WorkTask => ({ id, planId, title, component, owner, priority, estimate, status, depth, sequence, attempts, source: "FIXTURE / EXPLICIT" });

const attempt = (id: string, status: WorkAttempt["status"], executor: string, sessionId: string): WorkAttempt => ({
  id,
  status,
  executor,
  sessionId,
  source: "FIXTURE / EXPLICIT",
});

export const FIXTURE_GRAPH: WorkGraph = {
  revision: 7,
  tasks: [
    task("T-101", "PLAN-CFG", "Inventory loader variants", "svc-config", "platform", "P1", "35m", "done", 0, 1, [attempt("A-101", "succeeded", "codex", "design:session:agent-001")]),
    task("T-102", "PLAN-CFG", "Normalize config schema", "svc-config", "platform", "P1", "1h 10m", "done", 1, 2, [attempt("A-102", "succeeded", "codex", "design:session:agent-002")]),
    task("T-103", "PLAN-CFG", "Extract loader interface", "lib-config", "platform", "P2", "55m", "ready", 2, 3),
    task("T-104", "PLAN-CFG", "Migrate call sites", "svc-api", "runtime", "P1", "2h 20m", "running", 2, 4, [attempt("A-104", "running", "cursor", "design:session:agent-004")]),
    task("T-105", "PLAN-CFG", "Update integration tests", "lib-config", "quality", "P2", "1h 15m", "blocked", 3, 5),
    task("T-201", "PLAN-EVID", "Capture attempt receipts", "work-runtime", "runtime", "P0", "1h 40m", "done", 0, 6, [attempt("A-201", "succeeded", "codex", "design:session:agent-021")]),
    task("T-202", "PLAN-EVID", "Join task evidence", "work-runtime", "runtime", "P1", "2h", "done", 1, 7, [attempt("A-202", "succeeded", "codex", "design:session:agent-022")]),
    task("T-203", "PLAN-EVID", "Preserve failure provenance", "work-store", "storage", "P0", "1h 25m", "failed", 2, 8, [attempt("A-203", "failed", "claude", "design:session:agent-023")]),
    task("T-204", "PLAN-EVID", "Project causal ledger", "dashboard-api", "ui", "P2", "1h 50m", "blocked", 3, 9),
    task("T-205", "PLAN-EVID", "Review outcome coverage", "dashboard", "quality", "P2", "45m", "waiting", 4, 10),
    task("T-301", "PLAN-SHIP", "Verify worktree placement", "executor", "runtime", "P1", "50m", "done", 0, 11, [attempt("A-301", "succeeded", "cursor", "design:session:agent-066")]),
    task("T-302", "PLAN-SHIP", "Admit provider execution", "executor", "runtime", "P0", "1h 30m", "ready", 1, 12),
    task("T-303", "PLAN-SHIP", "Inspect provider outcome", "delivery", "release", "P1", "40m", "blocked", 2, 13),
    task("T-304", "PLAN-SHIP", "Run independent review", "review", "quality", "P0", "1h", "proposed", 3, 14),
    task("T-305", "PLAN-SHIP", "Publish release artifact", "release", "release", "P1", "30m", "proposed", 4, 15),
  ],
  relations: [
    ["T-101", "T-102", "gating", "schema inventory must land"],
    ["T-102", "T-103", "gating", "normalized schema unlocks interface"],
    ["T-102", "T-104", "gating", "normalized schema unlocks migration"],
    ["T-103", "T-104", "parallel", "planned parallel work"],
    ["T-104", "T-105", "gating", "migrated callers unlock tests"],
    ["T-203", "T-103", "informational", "shares failure evidence only"],
    ["T-201", "T-202", "gating", "receipts unlock evidence join"],
    ["T-202", "T-203", "gating", "joined evidence unlocks provenance"],
    ["T-203", "T-204", "gating", "failure provenance unlocks causal view"],
    ["T-204", "T-205", "gating", "causal ledger unlocks review"],
    ["T-301", "T-302", "gating", "placement verification unlocks admission"],
    ["T-302", "T-303", "gating", "admitted run unlocks outcome inspection"],
    ["T-303", "T-304", "gating", "outcome evidence unlocks review"],
    ["T-304", "T-305", "gating", "review unlocks publication"],
    ["T-104", "T-203", "observed", "attempt started before failure receipt"],
    ["T-302", "T-104", "delegation", "provider handoff record"],
  ].map(([from, to, kind, label], index) => ({ id: `R-${index + 1}`, from, to, kind: kind as RelationKind, label, grade: kind === "observed" ? "EXACT" : "EXPLICIT" })),
  events: [
    { id: "E-1", taskId: "T-102", at: "16:11:02", event: "task.accepted", detail: "graph revision 7" },
    { id: "E-2", taskId: "T-104", at: "16:11:05", event: "attempt.admitted", detail: "A-104 · cursor" },
    { id: "E-3", taskId: "T-203", at: "16:11:07", event: "attempt.failed", detail: "A-203 · evidence retained" },
    { id: "E-4", taskId: "T-103", at: "16:11:09", event: "task.ready", detail: "informational edge ignored" },
  ],
};

const workStatuses = new Set<WorkStatus>(["proposed", "ready", "running", "waiting", "blocked", "review", "done", "failed"]);
const relationKinds = new Set<RelationKind>(["gating", "informational", "parallel", "observed", "delegation"]);

/** Local fixture state is optional view memory, never source authority. */
export function isValidWorkGraph(value: unknown): value is WorkGraph {
  if (!value || typeof value !== "object") return false;
  const graph = value as Partial<WorkGraph>;
  if (!Number.isFinite(graph.revision) || !Array.isArray(graph.tasks) || !graph.tasks.length || !Array.isArray(graph.relations) || !Array.isArray(graph.events)) return false;
  const known = new Set(FIXTURE_GRAPH.tasks.map((task) => task.id));
  const ids = new Set<string>();
  for (const item of graph.tasks) {
    if (!item || typeof item !== "object" || typeof item.id !== "string" || !known.has(item.id) || ids.has(item.id) || typeof item.planId !== "string" || typeof item.title !== "string" || typeof item.component !== "string" || typeof item.owner !== "string" || typeof item.estimate !== "string" || !workStatuses.has(item.status) || !Array.isArray(item.attempts)) return false;
    ids.add(item.id);
  }
  if (ids.size !== known.size) return false;
  return graph.relations.every((relation) => relation && typeof relation.id === "string" && ids.has(relation.from) && ids.has(relation.to) && relationKinds.has(relation.kind)) && graph.events.every((event) => event && typeof event.id === "string" && ids.has(event.taskId) && typeof event.at === "string" && typeof event.event === "string" && typeof event.detail === "string");
}

export function blockingRelations(graph: WorkGraph, taskId: string): WorkRelation[] {
  const done = new Set(graph.tasks.filter((entry) => entry.status === "done").map((entry) => entry.id));
  return graph.relations.filter((relation) => relation.kind === "gating" && relation.to === taskId && !done.has(relation.from));
}

export function legalActions(graph: WorkGraph, taskId: string): WorkAction[] {
  const selected = graph.tasks.find((entry) => entry.id === taskId);
  if (!selected) return [];
  if (selected.status === "proposed") return ["accept"];
  if (selected.status === "ready") return ["admit"];
  if (selected.status === "running") return ["complete"];
  if (selected.status === "failed") return ["retry"];
  if (selected.status === "blocked" && blockingRelations(graph, taskId).length) return ["replan"];
  return [];
}

function deriveReadiness(graph: WorkGraph): WorkGraph {
  return {
    ...graph,
    tasks: graph.tasks.map((entry) => {
      if (entry.status !== "ready" && entry.status !== "blocked") return entry;
      return { ...entry, status: blockingRelations(graph, entry.id).length ? "blocked" : "ready" };
    }),
  };
}

export function applyWorkAction(graph: WorkGraph, taskId: string, action: WorkAction): WorkGraph {
  if (!legalActions(graph, taskId).includes(action)) return graph;
  const now = `16:${String(12 + graph.revision).padStart(2, "0")}:00`;
  let relations = graph.relations;
  let event: string = action;
  const tasks = graph.tasks.map((entry) => {
    if (entry.id !== taskId) return entry;
    if (action === "accept") return { ...entry, status: blockingRelations(graph, taskId).length ? "blocked" : "ready" } as WorkTask;
    if (action === "admit" || action === "retry") {
      const id = `A-${entry.id.slice(2)}-${entry.attempts.length + 1}`;
      event = action === "retry" ? "attempt.retried" : "attempt.admitted";
      return { ...entry, status: "running", attempts: [...entry.attempts, attempt(id, "running", "codex", "design:session:agent-090")] } as WorkTask;
    }
    if (action === "complete") {
      event = "attempt.succeeded";
      return { ...entry, status: "done", attempts: entry.attempts.map((item, index) => index === entry.attempts.length - 1 ? { ...item, status: "succeeded" } : item) } as WorkTask;
    }
    return entry;
  });
  if (action === "replan") {
    const blocker = blockingRelations(graph, taskId)[0];
    relations = graph.relations.map((relation) => relation.id === blocker.id ? { ...relation, kind: "informational", label: `${relation.label} · replanned non-gating` } : relation);
    event = "relation.replanned";
  }
  return deriveReadiness({
    revision: graph.revision + 1,
    tasks,
    relations,
    events: [...graph.events, { id: `E-${graph.events.length + 1}`, taskId, at: now, event, detail: `fixture command · revision ${graph.revision + 1}` }],
  });
}

export function statusTone(status: WorkStatus): "cyan" | "amber" | "green" | "red" | "dim" {
  if (status === "done" || status === "review") return "green";
  if (status === "ready" || status === "running") return "cyan";
  if (status === "blocked" || status === "waiting" || status === "proposed") return "amber";
  if (status === "failed") return "red";
  return "dim";
}
