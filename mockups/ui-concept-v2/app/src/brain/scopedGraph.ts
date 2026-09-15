import tdSessions from "../../profile-pack/proj_ae394425f7837d4f/sessions.json";
import tdMessages from "../../profile-pack/proj_ae394425f7837d4f/messages-spine.json";
import tdObs from "../../profile-pack/proj_ae394425f7837d4f/observations-slim.json";
import { type GraphEdge, type GraphNode, type ProjectBody } from "../data/fixtures";
import { sessionTufts, shortSessionLabel } from "../concept/neuronLab/interior";
import { hash, mulberry32 } from "./particles";

const TRACEDECAY_ID = "proj_ae394425f7837d4f";

export type ScopedNode = GraphNode & {
  isHub: boolean;
  degree: number;
  /** typed absence: unsealed store rendered as a hollow, dim identity */
  dim?: boolean;
};

export type ScopedGraph = {
  nodes: ScopedNode[];
  edges: GraphEdge[];
  clusters: string[];
  /** typed absences printed in the footer */
  absences: string[];
};

const PALETTE = ["#5ee7ff", "#f0b429", "#9be15d", "#c084fc", "#7dd3fc", "#fbbf24", "#67e8f9"];

const CLUSTER_COLORS: Record<string, string> = {
  sessions: "#5ee7ff",
  messages: "#7dd3fc",
  observations: "#f0b429",
  checkouts: "#9be15d",
  tools: "#fbbf24",
  agents: "#c084fc",
  sealed: "#64748b",
};

type SessionRow = {
  session_id: string;
  provider?: string;
  project_path?: string;
};
type MessageRow = {
  session_id: string;
  kind?: string;
  model?: string;
  tool_names?: string[] | string | null;
};
type ObsRow = { session_id?: string; kind?: string };

function count<T>(rows: T[], key: (r: T) => string | null | undefined) {
  const m = new Map<string, number>();
  for (const r of rows) {
    const k = key(r);
    if (!k) continue;
    m.set(k, (m.get(k) ?? 0) + 1);
  }
  return [...m.entries()].sort((a, b) => b[1] - a[1]);
}

function trimLabel(s: string, max = 26) {
  return s.length > max ? `${s.slice(0, max - 1)}…` : s;
}

/**
 * WHAT TRACEDECAY KNOWS, bound to the slim profile pack only: enrolled
 * sessions, message spine (kinds, models, tools), observation kinds,
 * real checkouts. Facts/index are unsealed in the pack, so they surface
 * as typed absence — never as invented node counts.
 */
function tracedecayGraph(): { nodes: GraphNode[]; edges: GraphEdge[]; absences: string[] } {
  const nodes: GraphNode[] = [];
  const edges: GraphEdge[] = [];
  const C = CLUSTER_COLORS;
  const sessions = tdSessions as SessionRow[];
  const messages = tdMessages as MessageRow[];
  const obs = tdObs as ObsRow[];

  const hub = (id: string, label: string, cluster: string) => {
    nodes.push({ id, label, cluster, color: C[cluster] });
  };
  const sat = (id: string, label: string, cluster: string, hubId: string) => {
    nodes.push({ id, label, cluster, color: C[cluster] });
    edges.push({ a: hubId, b: id });
  };

  hub("h:sessions", `sessions · ${sessions.length}`, "sessions");
  for (const s of sessions) {
    sat(
      `s:${s.session_id}`,
      `${shortSessionLabel(s.session_id)} · ${s.provider ?? "?"}`,
      "sessions",
      "h:sessions",
    );
  }

  hub("h:messages", `messages · ${messages.length}`, "messages");
  for (const [kind, n] of count(messages, (m) => m.kind)) {
    sat(`mk:${kind}`, `${kind} · ${n}`, "messages", "h:messages");
  }

  hub("h:obs", `observations · ${obs.length}`, "observations");
  for (const [kind, n] of count(obs, (o) => o.kind).slice(0, 12)) {
    sat(`ok:${kind}`, `${kind} · ${n}`, "observations", "h:obs");
  }

  const checkoutAliases = ["redesign", "ui-concept-first-party", "ui-concept-v2-final-followup"];
  hub("h:checkouts", `checkouts · ${checkoutAliases.length}`, "checkouts");
  for (const alias of checkoutAliases) {
    sat(`co:${alias}`, alias, "checkouts", "h:checkouts");
  }

  const toolCounts = count(
    messages.flatMap((m) => {
      const tn = m.tool_names;
      if (!tn) return [] as string[];
      return Array.isArray(tn) ? tn : String(tn).split(",");
    }).map((t) => ({ t: t.trim() })),
    (r) => r.t || null,
  );
  const nToolCalls = messages.filter((m) => m.kind === "tool_invocation").length;
  hub("h:tools", `tool_calls · ${nToolCalls}`, "tools");
  for (const [tool, n] of toolCounts.slice(0, 6)) {
    sat(`tool:${tool}`, `${trimLabel(tool)} · ${n}`, "tools", "h:tools");
  }

  const providers = count(sessions, (s) => s.provider);
  const models = count(messages, (m) => (m.model && m.model !== "<synthetic>" ? m.model : null));
  hub("h:agents", `providers · ${providers.length}`, "agents");
  for (const [p, n] of providers) sat(`prov:${p}`, `${p} · ${n} sessions`, "agents", "h:agents");
  for (const [mdl, n] of models.slice(0, 2)) sat(`model:${mdl}`, `${mdl} · ${n}`, "agents", "h:agents");

  // typed absence: the pack ships no facts/index tables — unsealed, not zeroed
  hub("h:facts", "facts · absent", "sealed");
  sat("h:anchors", "retrieval_anchors · 0", "sealed", "h:facts");

  // evidenced cross-relations only
  edges.push({ a: "h:sessions", b: "h:messages" });
  edges.push({ a: "h:sessions", b: "h:obs" });
  edges.push({ a: "h:sessions", b: "h:agents" });
  edges.push({ a: "mk:tool_invocation", b: "h:tools" });
  for (const [mdl] of models.slice(0, 2)) edges.push({ a: "h:messages", b: `model:${mdl}` });
  // only 01a045da carries a real filesystem path -> canonical checkout
  for (const s of sessions) {
    if (s.project_path?.startsWith("/")) {
      edges.push({ a: `s:${s.session_id}`, b: "co:redesign" });
    }
  }

  return { nodes, edges, absences: ["facts · absent", "retrieval_anchors · 0"] };
}

export function buildScopedGraph(project: ProjectBody): ScopedGraph {
  let nodes: GraphNode[];
  let edges: GraphEdge[];
  let absences: string[] = [];
  if (project.id === TRACEDECAY_ID) {
    const g = tracedecayGraph();
    nodes = g.nodes;
    edges = g.edges;
    absences = g.absences;
  } else if (project.branches?.length) {
    nodes = [];
    edges = [];
    const groups = new Map<string, string[]>();
    for (const b of project.branches) {
      const slash = b.indexOf("/");
      const g = slash > 0 ? b.slice(0, slash) : "(root)";
      const arr = groups.get(g) ?? [];
      arr.push(b);
      groups.set(g, arr);
    }
    const top = [...groups.entries()].sort((a, b) => b[1].length - a[1].length).slice(0, 7);
    top.forEach(([name, members], gi) => {
      const color = PALETTE[gi % PALETTE.length];
      const hubId = `grp:${name}`;
      nodes.push({ id: hubId, label: `${name}/ (${members.length})`, cluster: name, color });
      for (const m of members.slice(0, 6)) {
        nodes.push({ id: `br:${m}`, label: m, cluster: name, color });
        edges.push({ a: hubId, b: `br:${m}` });
      }
    });
    absences = ["facts · absent", "retrieval_anchors · 0"];
  } else {
    nodes = [];
    edges = [];
    project.checkouts.forEach((c, ci) => {
      const color = PALETTE[ci % PALETTE.length];
      nodes.push({ id: `co:${c.alias}`, label: c.alias, cluster: c.alias, color });
    });
    for (const t of sessionTufts(project)) {
      const hubId = `co:${t.checkoutId}`;
      const hub = nodes.find((n) => n.id === hubId);
      if (!hub) continue;
      nodes.push({
        id: `s:${t.sessionId}`,
        label: t.labeled ? shortSessionLabel(t.sessionId) : "(unlabeled)",
        cluster: t.checkoutId,
        color: hub.color,
      });
      edges.push({ a: hubId, b: `s:${t.sessionId}` });
    }
    absences = ["facts · absent", "retrieval_anchors · 0"];
  }

  const degree = new Map<string, number>();
  for (const e of edges) {
    degree.set(e.a, (degree.get(e.a) ?? 0) + 1);
    degree.set(e.b, (degree.get(e.b) ?? 0) + 1);
  }
  const clusters = [...new Set(nodes.map((n) => n.cluster))];
  const hubOf = new Map<string, string>();
  for (const c of clusters) {
    const members = nodes.filter((n) => n.cluster === c);
    let best = members[0];
    for (const m of members) {
      if ((degree.get(m.id) ?? 0) > (degree.get(best.id) ?? 0)) best = m;
    }
    hubOf.set(c, best.id);
  }
  return {
    nodes: nodes.map((n) => ({
      ...n,
      degree: degree.get(n.id) ?? 0,
      isHub: hubOf.get(n.cluster) === n.id,
      dim: n.cluster === "sealed",
    })),
    edges,
    clusters,
    absences,
  };
}

export type PlacedNode = ScopedNode & {
  x: number;
  y: number;
  r: number;
  labelSide: "left" | "right";
};

export type ScopedLayout = {
  nodes: PlacedNode[];
  byId: Map<string, PlacedNode>;
  edges: GraphEdge[];
};

/** Plate cluster geography for the tracedecay constellation. */
const TD_CENTERS: Record<string, [number, number]> = {
  messages: [0.265, 0.235],
  observations: [0.66, 0.225],
  sessions: [0.44, 0.5],
  checkouts: [0.215, 0.66],
  tools: [0.45, 0.79],
  agents: [0.76, 0.5],
  sealed: [0.665, 0.76],
};

export function layoutScoped(w: number, h: number, g: ScopedGraph): ScopedLayout {
  const m = Math.min(w, h);
  const centers = new Map<string, { x: number; y: number }>();
  g.clusters.forEach((c, i) => {
    const td = TD_CENTERS[c];
    if (td) {
      centers.set(c, { x: td[0] * w, y: td[1] * h });
      return;
    }
    const n = g.clusters.length;
    const th = (i / Math.max(1, n)) * Math.PI * 2 - Math.PI / 2;
    const rad = n === 1 ? 0 : m * 0.28;
    centers.set(c, { x: w * 0.47 + Math.cos(th) * rad, y: h * 0.48 + Math.sin(th) * rad * 0.9 });
  });

  const placed: PlacedNode[] = [];
  const byId = new Map<string, PlacedNode>();
  for (const c of g.clusters) {
    const center = centers.get(c)!;
    const members = g.nodes.filter((n) => n.cluster === c);
    const sats = members.filter((n) => !n.isHub);
    // clusters with many satellites earn a wider orbit
    const orbit = 0.82 + Math.min(0.6, sats.length * 0.055);
    members.forEach((n) => {
      let x = center.x;
      let y = center.y;
      if (!n.isHub) {
        const si = sats.indexOf(n);
        const rng = mulberry32(hash(n.id));
        let th = (si / Math.max(1, sats.length)) * Math.PI * 2 + (rng() - 0.5) * 0.55 + hash(c) % 7;
        // keep the band right of the hub clear: the hub label lives there
        if (Math.cos(th) > 0.86) th += Math.sin(th) >= 0 ? 0.5 : -0.5;
        let rad = m * (0.095 + (si % 2) * 0.05 + rng() * 0.03) * orbit;
        if (Math.cos(th) > 0.45) rad *= 1.3;
        x += Math.cos(th) * rad * 1.25;
        y += Math.sin(th) * rad * 0.92;
      }
      const r = n.isHub ? 5.5 + Math.min(6, n.degree * 0.55) : 2 + Math.min(2.4, n.degree * 0.5);
      x = Math.max(14, Math.min(w - 14, x));
      y = Math.max(40, Math.min(h - 46, y));
      const p: PlacedNode = { ...n, x, y, r, labelSide: "right" };
      placed.push(p);
      byId.set(n.id, p);
    });
  }

  // resolve label collisions: hubs anchor, satellites give way
  type Rect = { x0: number; y0: number; x1: number; y1: number };
  const rects: Rect[] = [];
  const hits = (r: Rect) =>
    rects.some((o) => r.x0 < o.x1 && r.x1 > o.x0 && r.y0 < o.y1 && r.y1 > o.y0);
  const ordered = [...placed].sort((a, b) => Number(b.isHub) - Number(a.isHub));
  for (const n of ordered) {
    const est = n.label.length * (n.isHub ? 6.8 : 5.6);
    const anchor = centers.get(n.cluster)!;
    let side: "left" | "right" = !n.isHub && n.x < anchor.x - 2 ? "left" : "right";
    if (side === "left" && n.x - n.r - 8 - est < 4) side = "right";
    if (side === "right" && n.x + n.r + 8 + est > w - 4) side = "left";
    const rectAt = (y: number): Rect => {
      const x0 = side === "right" ? n.x + n.r + 6 : n.x - n.r - 6 - est;
      return { x0, y0: y - 7, x1: x0 + est, y1: y + 7 };
    };
    let done = false;
    for (const dy of n.isHub ? [0] : [0, 13, -13, 26, -26, 39, -39]) {
      const r = rectAt(n.y + dy);
      if (!hits(r)) {
        n.y += dy;
        rects.push(r);
        done = true;
        break;
      }
    }
    if (!done) rects.push(rectAt(n.y));
    n.labelSide = side;
  }
  return { nodes: placed, byId, edges: g.edges };
}
