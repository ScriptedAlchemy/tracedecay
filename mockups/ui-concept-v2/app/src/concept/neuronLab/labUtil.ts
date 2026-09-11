import * as THREE from "three";
import { PROJECTS, type RecencyBucket } from "../../data/fixtures";

export function mulberry32(seed: number) {
  let a = seed >>> 0;
  return () => {
    a += 0x6d2b79f5;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export function hash32(s: string) {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

export const RECENCY_I: Record<RecencyBucket, number> = {
  "<5min": 0,
  "5min-1h": 1,
  "1h-1d": 2,
  "1d-1w": 3,
  "1w-1m": 4,
  "1m-6m": 5,
  "6m-1y": 6,
  ">1y": 7,
};

export const WARM: Record<RecencyBucket, number> = {
  "<5min": 1,
  "5min-1h": 0.9,
  "1h-1d": 0.8,
  "1d-1w": 0.68,
  "1w-1m": 0.56,
  "1m-6m": 0.46,
  "6m-1y": 0.36,
  ">1y": 0.22,
};

export type LabKind = "project" | "checkout" | "hub" | "session";

/** Identity for a lab soma. Projects or checkouts — sessions are tufts, hub is a glyph. */
export type LabIdentity = {
  id: string;
  name: string;
  indexedMass: number;
  recency: RecencyBucket;
  row: 0 | 1 | 2;
  kind: LabKind;
};

export function asProjectIdentity(p: (typeof PROJECTS)[number]): LabIdentity {
  return {
    id: p.id,
    name: p.name,
    indexedMass: p.indexedMass,
    recency: p.recency,
    row: p.row,
    kind: "project",
  };
}

export function projectIdentities(): LabIdentity[] {
  return PROJECTS.map(asProjectIdentity);
}

export type LabBody = {
  project: LabIdentity;
  pos: THREE.Vector3;
  somaR: number;
  warm: number;
};

/** Fan-out. Recency → azimuth bias, mass → height, row → radius. One body sits at origin. */
export function layoutLabBodies(list: readonly LabIdentity[] = projectIdentities()): LabBody[] {
  if (!list.length) return [];
  const maxMass = Math.max(...list.map((p) => p.indexedMass), 8);
  if (list.length === 1) {
    const project = list[0];
    const massN = Math.log10(Math.max(8, project.indexedMass)) / Math.log10(maxMass);
    return [
      {
        project,
        pos: new THREE.Vector3(0, 0, 0),
        somaR: 0.22 + massN * 0.2,
        warm: WARM[project.recency],
      },
    ];
  }
  return list.map((project, i) => {
    const rng = mulberry32(hash32(project.id) ^ 0x51a7);
    const recN = RECENCY_I[project.recency] / 7;
    const massN = Math.log10(Math.max(8, project.indexedMass)) / Math.log10(maxMass);
    const theta = (i / list.length) * Math.PI * 2 - 0.55 + (rng() - 0.5) * 0.18;
    const radius = 3.35 + project.row * 0.62 + recN * 0.35;
    const x = Math.cos(theta) * radius * 1.18 + (recN - 0.22) * 1.15;
    const y = (massN - 0.42) * 3.05 + (0.45 - recN) * 0.55;
    const z = Math.sin(theta) * radius * 0.92;
    const somaR = 0.2 + massN * 0.22;
    return {
      project,
      pos: new THREE.Vector3(x, y, z),
      somaR,
      warm: WARM[project.recency],
    };
  });
}

export type Pipe = { a: LabBody; b: LabBody; warm: number; admitted: boolean; energy: number };

/**
 * A project-to-project pipe exists only when TraceDecay attribution already
 * recorded a hop (agent / thread / session naming two distinct project ids).
 *
 * Slim-pack scan (CONCEPT / PROFILE SNAPSHOT, profile-pack/ + src/data):
 *   sessions, agents, session_threads, observations-slim, tracedecay-pack.ts
 *   — every ActivityPulse / session is project-scoped; no row names two of
 *     the six registry project ids.
 * Not hops (never drawn as inter-project pipes):
 *   a) worktrees sharing git_common_dir are ONE soma already
 *   b) hub is massless and does not scope
 *   c) Kruskal / distance union, or invented tracedecay↔ZeroFS / ZeroFS↔ios
 *
 * Conduction later: a pipe lights only when both ends are warm AND the hop
 * was admitted. Hover stays inspect-only.
 */
export type AttributionHop = {
  aId: string;
  bId: string;
  admitted: boolean;
  energy?: number;
};

/** Unscoped six-project field: slim pack has no project-to-project hop. */
export const ATTRIBUTED_HOPS: AttributionHop[] = [];

/** Massless origin. Glyph, not a holdings soma, never scopes. */
export const HUB_ID = "repo:git_common_dir";

export type HubLayout = {
  hub: THREE.Vector3;
  bodies: LabBody[];
  radius: number;
};

/**
 * Checkout orbs around the massless hub (CONCEPT / PROFILE SNAPSHOT).
 * Hub is not in `bodies`. Hierarchy inverted vs. the plumbing pass: checkout
 * somaR is categorical LARGE (plate-sphere / orb scale), hub stays a small
 * grey glyph. Ring 5.4 / 6.6 / 7.4 so three orbs have dark air and short
 * hairlines. Nearly coplanar (y ≈ ±0.28) so a high 3/4 camera reads a
 * triangle of spheres, not edge-on trunks.
 */
export function layoutAroundHub(list: readonly LabIdentity[]): HubLayout {
  const hub = new THREE.Vector3(0, 0, 0);
  if (!list.length) return { hub, bodies: [], radius: 7.4 };
  const maxMass = Math.max(...list.map((p) => p.indexedMass), 8);
  const n = list.length;
  const radius = n === 1 ? 5.4 : n === 2 ? 6.6 : 7.4;
  const bodies = list.map((project, i) => {
    const massN = Math.log10(Math.max(8, project.indexedMass)) / Math.log10(maxMass);
    const theta = (i / n) * Math.PI * 2 - Math.PI / 2;
    const y = n <= 1 ? 0 : 0.28 * Math.cos((i * 2 * Math.PI) / n);
    const pos = new THREE.Vector3(Math.cos(theta) * radius, y, Math.sin(theta) * radius);
    return {
      project,
      pos,
      somaR: 2.48 + massN * 0.42,
      warm: WARM[project.recency],
    };
  });
  return { hub, bodies, radius };
}

export function attributionPipes(
  nodes: LabBody[],
  hops: readonly AttributionHop[] = ATTRIBUTED_HOPS,
): Pipe[] {
  const byId = new Map(nodes.map((n) => [n.project.id, n]));
  const picked: Pipe[] = [];
  const seen = new Set<string>();
  for (const hop of hops) {
    if (!hop.admitted) continue;
    if (hop.aId === hop.bId) continue;
    if (hop.aId === HUB_ID || hop.bId === HUB_ID) continue;
    const a = byId.get(hop.aId);
    const b = byId.get(hop.bId);
    if (!a || !b) continue;
    const key = hop.aId < hop.bId ? `${hop.aId}|${hop.bId}` : `${hop.bId}|${hop.aId}`;
    if (seen.has(key)) continue;
    seen.add(key);
    picked.push({
      a,
      b,
      warm: Math.min(a.warm, b.warm),
      admitted: true,
      energy: hop.energy ?? 1,
    });
  }
  return picked;
}

export type LabGrain = "session" | "worktree";

export type BrainState = "overview" | "hover" | "repo-zoom" | "scoped" | "synapse";

export type LabHooks = {
  onFocus?: (id: string) => void;
  onEnterScope?: (projectId: string) => void;
  onLeaveScope?: () => void;
  scope?: string | null;
  grain?: LabGrain;
  brainState?: BrainState;
  focusId?: string | null;
};

export function catmullBend(
  from: THREE.Vector3,
  to: THREE.Vector3,
  rng: () => number,
  wander: number,
) {
  const n = 4 + Math.floor(rng() * 3);
  const dir = to.clone().sub(from);
  const len = dir.length() || 1;
  dir.normalize();
  const up = Math.abs(dir.y) < 0.85 ? new THREE.Vector3(0, 1, 0) : new THREE.Vector3(1, 0, 0);
  const n1 = new THREE.Vector3().crossVectors(dir, up).normalize();
  const n2 = new THREE.Vector3().crossVectors(dir, n1).normalize();
  const pts: THREE.Vector3[] = [];
  for (let i = 0; i <= n; i++) {
    const t = i / n;
    const p = from.clone().lerp(to, t);
    if (i > 0 && i < n) {
      const mag = wander * len * (0.07 + rng() * 0.2) * Math.sin(t * Math.PI);
      p.addScaledVector(n1, (rng() - 0.5) * 2 * mag);
      p.addScaledVector(n2, (rng() - 0.5) * 2 * mag);
    }
    pts.push(p);
  }
  return pts;
}

export type TubeRec = { pts: THREE.Vector3[]; radius: number };

/** Irregular dendrite wrap — not a binary L-system.
 *  `packed`: keep every segment inside a spherical envelope of radius somaR
 *  (checkout nebula orbs). Overview passes false and is unchanged.
 */
export function growDendrites(
  origin: THREE.Vector3,
  somaR: number,
  rng: () => number,
  scale: number,
  packed = false,
) {
  const tubes: TubeRec[] = [];
  const nPrimary = packed ? 16 + Math.floor(rng() * 6) : 8 + Math.floor(rng() * 5);
  const envelope = packed ? somaR * 0.97 : Infinity;
  const clampTo = (p: THREE.Vector3) => {
    const d = p.distanceTo(origin);
    if (d > envelope) p.copy(origin).addScaledVector(p.clone().sub(origin).normalize(), envelope);
    return p;
  };
  for (let i = 0; i < nPrimary; i++) {
    const y = ((i / Math.max(1, nPrimary - 1)) * 2 - 1) * (0.62 + rng() * 0.28);
    const phi = i * 2.399 + rng() * 0.9;
    const rr = Math.sqrt(Math.max(0, 1 - y * y));
    const dir = new THREE.Vector3(
      rr * Math.cos(phi),
      packed ? y * 0.94 + (rng() - 0.5) * 0.16 : y * 0.72 + (rng() - 0.5) * 0.38,
      rr * Math.sin(phi),
    ).normalize();
    const len = packed ? somaR * (0.52 + rng() * 0.34) : scale * (0.5 + rng() * 0.95);
    const startR = packed ? somaR * 0.16 : somaR * 0.62;
    const start = origin.clone().addScaledVector(dir, startR);
    const end = clampTo(origin.clone().addScaledVector(dir, startR + len));
    const radius = packed ? 0.011 + rng() * 0.01 : (0.026 + rng() * 0.02) * scale;
    tubes.push({ pts: catmullBend(start, end, rng, packed ? 0.48 : 1.05), radius });
    const twigs = Math.floor(rng() * (packed ? 2.6 : 3.4));
    for (let t = 0; t < twigs; t++) {
      const along = 0.28 + rng() * 0.55;
      const originT = start.clone().lerp(end, along);
      const side = new THREE.Vector3(rng() - 0.5, rng() - 0.5, rng() - 0.5).normalize();
      const tlen = packed ? somaR * (0.12 + rng() * 0.22) : len * (0.22 + rng() * 0.42);
      const tend = clampTo(originT.clone().addScaledVector(side, tlen));
      const tr = radius * (0.32 + rng() * 0.28);
      tubes.push({ pts: catmullBend(originT, tend, rng, packed ? 0.7 : 1.25), radius: tr });
      if (rng() > 0.5) {
        const o2 = originT.clone().lerp(tend, 0.4 + rng() * 0.45);
        const s2 = new THREE.Vector3(rng() - 0.5, rng() - 0.5, rng() - 0.5).normalize();
        const e2 = clampTo(o2.clone().addScaledVector(s2, packed ? tlen * 0.42 : tlen * 0.48));
        tubes.push({ pts: catmullBend(o2, e2, rng, packed ? 0.85 : 1.45), radius: tr * 0.45 });
      }
    }
  }
  return tubes;
}

export function axonPts(a: THREE.Vector3, b: THREE.Vector3, rng: () => number) {
  const dir = b.clone().sub(a);
  const n = new THREE.Vector3().crossVectors(dir, new THREE.Vector3(0, 1, 0));
  if (n.lengthSq() < 1e-8) n.set(1, 0, 0);
  n.normalize();
  const bin = new THREE.Vector3().crossVectors(dir, n).normalize();
  const len = dir.length();
  const bow = len * (0.1 + rng() * 0.16);
  const m1 = a
    .clone()
    .lerp(b, 0.28)
    .addScaledVector(n, bow * (rng() < 0.5 ? -1 : 1))
    .addScaledVector(bin, (rng() - 0.5) * bow * 0.6);
  const m2 = a
    .clone()
    .lerp(b, 0.7)
    .addScaledVector(n, -bow * 0.45)
    .addScaledVector(bin, (rng() - 0.5) * bow * 0.5);
  return [a.clone(), m1, m2, b.clone()];
}

export function disposeObject(root: THREE.Object3D) {
  const seen = new Set<THREE.BufferGeometry>();
  const mats = new Set<THREE.Material>();
  root.traverse((obj) => {
    const mesh = obj as THREE.Mesh;
    if (mesh.geometry) seen.add(mesh.geometry);
    const mat = mesh.material as THREE.Material | THREE.Material[] | undefined;
    if (Array.isArray(mat)) mat.forEach((m) => mats.add(m));
    else if (mat) mats.add(mat);
  });
  for (const g of seen) g.dispose();
  for (const m of mats) m.dispose();
}
