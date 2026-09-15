import tdSessions from "../../../profile-pack/proj_ae394425f7837d4f/sessions.json";
import tdMessages from "../../../profile-pack/proj_ae394425f7837d4f/messages-spine.json";
import tdObs from "../../../profile-pack/proj_ae394425f7837d4f/observations-slim.json";
import coreSessions from "../../../profile-pack/proj_2c558f8ce42d5cdf/sessions.json";
import zfsSessions from "../../../profile-pack/proj_e19f6f383c982ea8/sessions.json";
import iosSessions from "../../../profile-pack/proj_8007f36f3654e9be/sessions.json";
import movieSessions from "../../../profile-pack/proj_a812b7edf6fab331/sessions.json";
import mqvpnSessions from "../../../profile-pack/proj_2f28a204e57c569f/sessions.json";
import { PROJECTS, type ProjectBody } from "../../data/fixtures";
import { HUB_ID, type AttributionHop, type LabGrain, type LabIdentity } from "./labUtil";

export type { LabGrain };

type SessionRow = {
  session_id: string;
  project_path?: string;
  parent_session_id?: string | null;
};

type SpineRow = { session_id?: string };

const TRACEDECAY_ID = "proj_ae394425f7837d4f";

const SESSIONS: Record<string, SessionRow[]> = {
  proj_ae394425f7837d4f: tdSessions as SessionRow[],
  proj_2c558f8ce42d5cdf: coreSessions as SessionRow[],
  proj_e19f6f383c982ea8: zfsSessions as SessionRow[],
  proj_8007f36f3654e9be: iosSessions as SessionRow[],
  proj_a812b7edf6fab331: movieSessions as SessionRow[],
  proj_2f28a204e57c569f: mqvpnSessions as SessionRow[],
};

/** First 8 chars, or keep the agent- prefix plus a short tail. */
export function shortSessionLabel(id: string) {
  if (id.startsWith("agent-")) return id.length > 14 ? `${id.slice(0, 14)}` : id;
  return id.slice(0, 8);
}

function messageMass(sessionId: string) {
  let n = 0;
  for (const m of tdMessages as SpineRow[]) {
    if (m.session_id === sessionId) n++;
  }
  return Math.max(8, n);
}

function asIdentity(
  id: string,
  name: string,
  project: ProjectBody,
  i: number,
  mass: number,
  kind: LabIdentity["kind"],
): LabIdentity {
  return {
    id,
    name,
    indexedMass: mass,
    recency: project.recency,
    row: (i % 3) as 0 | 1 | 2,
    kind,
  };
}

function checkoutIdentities(project: ProjectBody): LabIdentity[] {
  return project.checkouts.map((c, i) => asIdentity(c.alias, c.alias, project, i, 16, "checkout"));
}

function isRealPath(path?: string) {
  return Boolean(path && path.startsWith("/"));
}

/** Longest matching checkout path. Never hash. Misses park on canonical/default. */
function checkoutForPath(project: ProjectBody, path?: string): string {
  const canonical = project.checkouts[0]?.alias ?? "";
  if (!isRealPath(path)) return canonical;
  const p = path as string;
  let best: { alias: string; len: number } | null = null;
  for (const c of project.checkouts) {
    if (p === c.path || p.startsWith(`${c.path}/`)) {
      if (!best || c.path.length > best.len) best = { alias: c.alias, len: c.path.length };
    }
  }
  return best?.alias ?? canonical;
}

/** Somas inside a project: real checkouts (trunks). Sessions are tufts, not somas. */
export function interiorIdentities(project: ProjectBody, _grain: LabGrain): LabIdentity[] {
  return checkoutIdentities(project);
}

export type NeighborhoodRing = {
  hubId: string;
  checkouts: LabIdentity[];
  hops: AttributionHop[];
};

export function neighborhoodRing(project: ProjectBody): NeighborhoodRing {
  return {
    hubId: HUB_ID,
    checkouts: checkoutIdentities(project),
    hops: worktreeHops(project),
  };
}

export type SessionTuft = {
  sessionId: string;
  label: string | null;
  checkoutId: string;
  labeled: boolean;
  mass: number;
};

/**
 * Sessions ride trunks. A real filesystem path → labeled tuft on that checkout.
 * Project-key-only rows → unlabeled glow parked on canonical/default.
 * Do not hash onto worktrees.
 */
export function sessionTufts(project: ProjectBody): SessionTuft[] {
  const rows = SESSIONS[project.id] ?? [];
  return rows.map((s) => {
    const labeled = isRealPath(s.project_path);
    return {
      sessionId: s.session_id,
      label: labeled ? shortSessionLabel(s.session_id) : null,
      checkoutId: checkoutForPath(project, s.project_path),
      labeled,
      mass: project.id === TRACEDECAY_ID ? messageMass(s.session_id) : 16,
    };
  }).filter((t) => t.checkoutId);
}

export function layoutNeighborhood2d(
  w: number,
  h: number,
  project: ProjectBody,
  zoom = 1,
) {
  const hub = { x: w * 0.48, y: h * 0.5 };
  const n = project.checkouts.length;
  const R = Math.min(w, h) * 0.22 * zoom;
  const nodes = project.checkouts.map((c, i) => {
    const theta = (i / Math.max(1, n)) * Math.PI * 2 - Math.PI / 2;
    return {
      id: c.alias,
      name: c.alias,
      x: hub.x + Math.cos(theta) * R,
      y: hub.y + Math.sin(theta) * R,
    };
  });
  return { hub, nodes, hops: worktreeHops(project) };
}

function pairKey(a: string, b: string) {
  return a < b ? `${a}|${b}` : `${b}|${a}`;
}

function parentHops(rows: SessionRow[]): AttributionHop[] {
  const ids = new Set(rows.map((r) => r.session_id));
  const hops: AttributionHop[] = [];
  const seen = new Set<string>();
  for (const r of rows) {
    const p = r.parent_session_id;
    if (!p || !ids.has(p) || p === r.session_id) continue;
    const key = pairKey(p, r.session_id);
    if (seen.has(key)) continue;
    seen.add(key);
    hops.push({ aId: p, bId: r.session_id, admitted: true });
  }
  return hops;
}

/**
 * A slim row is a hop only when it *names two enrolled session ids*.
 * Own session_id does not count. Missing payloads (bridge-session kind
 * with no other id) are not hops.
 */
function twoIdHops(enrolled: string[], rows: SpineRow[]): AttributionHop[] {
  const hops: AttributionHop[] = [];
  const seen = new Set<string>();
  for (const row of rows) {
    const blob = JSON.stringify(row);
    const named = enrolled.filter((id) => blob.includes(id));
    if (named.length < 2) continue;
    for (let i = 0; i < named.length; i++) {
      for (let j = i + 1; j < named.length; j++) {
        const key = pairKey(named[i], named[j]);
        if (seen.has(key)) continue;
        seen.add(key);
        hops.push({ aId: named[i], bId: named[j], admitted: true });
      }
    }
  }
  return hops;
}

function worktreeHops(project: ProjectBody): AttributionHop[] {
  // Pipes exist only because these checkouts share git_common_dir.
  // Hub is massless and is not a soma — pairs among checkouts, 1/3 energy.
  if (!project.linkedHub || project.checkouts.length < 2) return [];
  const hops: AttributionHop[] = [];
  for (let i = 0; i < project.checkouts.length; i++) {
    for (let j = i + 1; j < project.checkouts.length; j++) {
      hops.push({
        aId: project.checkouts[i].alias,
        bId: project.checkouts[j].alias,
        admitted: true,
        energy: 1 / 3,
      });
    }
  }
  return hops;
}

function sessionHops(projectId: string): AttributionHop[] {
  const rows = SESSIONS[projectId] ?? [];
  const hops = parentHops(rows);
  if (projectId === TRACEDECAY_ID) {
    const enrolled = rows.map((r) => r.session_id);
    const seen = new Set(hops.map((h) => pairKey(h.aId, h.bId)));
    for (const hop of [
      ...twoIdHops(enrolled, tdMessages as SpineRow[]),
      ...twoIdHops(enrolled, tdObs as SpineRow[]),
    ]) {
      const key = pairKey(hop.aId, hop.bId);
      if (seen.has(key)) continue;
      seen.add(key);
      hops.push(hop);
    }
  }
  return hops;
}

export function interiorHops(projectId: string, _grain: LabGrain): AttributionHop[] {
  const project = PROJECTS.find((p) => p.id === projectId);
  if (!project) return [];
  if (project.checkouts.length) return worktreeHops(project);
  return sessionHops(projectId);
}

export function projectById(id: string) {
  return PROJECTS.find((p) => p.id === id) ?? null;
}
