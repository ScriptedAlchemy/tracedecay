import * as THREE from "three";
import { tracedecayArbor } from "../data/tracedecay-pack";

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

export type TubeRec = {
  pts: THREE.Vector3[];
  radius: number;
  depth: number;
  sessionId?: string;
  destId?: string;
};

export type SomaRec = {
  id: string;
  kind: "root" | "session" | "worktree" | "thread" | "basal";
  pos: THREE.Vector3;
  radius: number;
  label: string;
  sub: string;
  dim?: boolean;
  sessionId?: string;
};

function shortId(id: string) {
  if (id.startsWith("agent-")) {
    return id.length > 9 ? `${id.slice(0, 9)}…` : id;
  }
  return id.length > 8 ? `${id.slice(0, 8)}…` : id;
}

function catmull(
  from: THREE.Vector3,
  to: THREE.Vector3,
  rng: () => number,
  zJitter: number,
  bowScale = 0.22,
) {
  const along = to.clone().sub(from);
  const len = along.length() || 1;
  const perp = new THREE.Vector3(-along.y, along.x, 0);
  if (perp.lengthSq() < 1e-8) perp.set(1, 0, 0);
  perp.normalize();
  const bin = new THREE.Vector3().crossVectors(along, perp);
  if (bin.lengthSq() < 1e-8) bin.set(0, 0, 1);
  bin.normalize();
  const sign = rng() < 0.5 ? -1 : 1;
  const mag = len * bowScale * (0.82 + rng() * 0.45);
  const bow1 = sign * mag;
  const bow2 = sign * mag * (0.28 + rng() * 0.4);
  const s1 = (rng() - 0.5) * zJitter * 2.6;
  const s2 = (rng() - 0.5) * zJitter * 2.6;
  const m1 = from.clone().lerp(to, 0.26).addScaledVector(perp, bow1).addScaledVector(bin, s1);
  m1.z += (rng() - 0.5) * zJitter;
  const m2 = from.clone().lerp(to, 0.7).addScaledVector(perp, bow2).addScaledVector(bin, s2);
  m2.z += (rng() - 0.5) * zJitter;
  return [from.clone(), m1, m2, to.clone()];
}

const SESSION_X = [-2.22, -0.74, 0.74, 2.22];

export function buildArbor() {
  const arbor = tracedecayArbor();
  const sessionIds = (arbor.sessionIds ?? []).slice(0, 4);
  const worktrees = arbor.sessionWorktrees ?? [];

  const rng = mulberry32(0x71f17e9);
  const tubes: TubeRec[] = [];
  const somas: SomaRec[] = [];
  let sessionCursor = 0;
  const sessionSomas: SomaRec[] = [];

  const basal = new THREE.Vector3(0, -2.42, 0);
  const maxDepth = 6;
  const baseLen = 1.82;
  const shrink = 0.7;

  function branchAng(depth: number) {
    if (depth === 0) return (40 * Math.PI) / 180;
    if (depth === 1) return (30 * Math.PI) / 180;
    return (22 * Math.PI) / 180;
  }

  function grow(
    start: THREE.Vector3,
    angle: number,
    length: number,
    depth: number,
    radius: number,
    sessionId: string | undefined,
    parentSomaId: string | undefined,
  ) {
    const dir = new THREE.Vector3(
      Math.sin(angle),
      Math.cos(angle),
      (rng() - 0.5) * 0.07 * Math.max(0, 1 - depth * 0.14),
    );
    dir.normalize();
    const end = start.clone().addScaledVector(dir, length);

    if (depth === 2 && sessionCursor < sessionIds.length) {
      end.x = THREE.MathUtils.lerp(end.x, SESSION_X[sessionCursor], 0.9);
      end.y = THREE.MathUtils.lerp(end.y, 1.05, 0.4);
      end.z *= 0.35;
    }

    let destId: string | undefined;
    let nextSession = sessionId;
    let nextParent = parentSomaId;

    if (depth === 2 && sessionCursor < sessionIds.length) {
      const sid = sessionIds[sessionCursor];
      const wt = worktrees[sessionCursor] ?? "";
      const soma: SomaRec = {
        id: `session:${sid}`,
        kind: "session",
        pos: end.clone(),
        radius: 0.2,
        label: shortId(sid),
        sub: wt,
        sessionId: sid,
      };
      soma.pos.z += 0.05;
      somas.push(soma);
      sessionSomas.push(soma);
      destId = soma.id;
      nextSession = sid;
      nextParent = soma.id;
      sessionCursor += 1;
    } else if (depth === 3 && sessionId) {
      const soma: SomaRec = {
        id: `fork:${sessionId}:${somas.length}`,
        kind: "worktree",
        pos: end.clone(),
        radius: 0.11,
        label: "",
        sub: "",
        sessionId,
      };
      soma.pos.z += 0.03;
      somas.push(soma);
      destId = soma.id;
      nextParent = soma.id;
    } else if (depth === 4 && end.y > 1.7 && rng() > 0.45) {
      const soma: SomaRec = {
        id: `canopy:${somas.length}`,
        kind: "thread",
        pos: end.clone(),
        radius: 0.085,
        label: "",
        sub: "",
        sessionId,
      };
      soma.pos.z += 0.02;
      somas.push(soma);
      destId = soma.id;
      nextParent = soma.id;
    }

    const bowScale = depth === 0 ? 0.11 : depth === 1 ? 0.3 : depth === 2 ? 0.24 : 0.18;
    tubes.push({
      pts: catmull(start, end, rng, 0.08 + depth * 0.018, bowScale),
      radius,
      depth,
      sessionId: nextSession,
      destId,
    });

    if (depth >= maxDepth) return;
    const jitter = depth <= 1 ? 0.1 : 0.06;
    const lenJ = depth === 2 ? 0.12 : 0.05;
    const ang = branchAng(depth);
    const aL = angle - ang + (rng() - 0.5) * jitter;
    const aR = angle + ang + (rng() - 0.5) * jitter;
    const lL = length * shrink * (1 + (rng() - 0.5) * lenJ);
    const lR = length * shrink * (1 + (rng() - 0.5) * lenJ);
    const rNext = Math.max(0.009, radius * 0.72);
    grow(end, aL, lL, depth + 1, rNext, nextSession, nextParent);
    grow(end, aR, lR, depth + 1, rNext, nextSession, nextParent);
  }

  const stemStart = basal.clone();
  stemStart.y += 0.16;
  grow(stemStart, 0, baseLen, 0, 0.023, undefined, undefined);

  let maxY = -Infinity;
  for (const t of tubes) for (const p of t.pts) if (p.y > maxY) maxY = p.y;

  const root: SomaRec = {
    id: "root",
    kind: "root",
    pos: new THREE.Vector3(0, Math.max(maxY - 0.08, 2.05), 0.22),
    radius: 0.4,
    label: "tracedecay",
    sub: "4 sessions",
  };
  somas.unshift(root);

  const canopyTips = tubes
    .filter((tb) => tb.depth >= 4)
    .map((tb) => tb.pts[tb.pts.length - 1])
    .filter((pt) => pt.y > maxY - 0.7)
    .sort((a, b) => a.distanceToSquared(root.pos) - b.distanceToSquared(root.pos))
    .slice(0, 4);
  for (const tip of canopyTips) {
    tubes.push({
      pts: catmull(root.pos, tip, rng, 0.12, 0.3),
      radius: 0.016,
      depth: 3,
      destId: "root",
    });
  }

  const basalRng = mulberry32(0xba5a1);
  for (let i = 0; i < 5; i++) {
    const a = (i / 5) * Math.PI * 2 + basalRng() * 0.35;
    const r = 0.22 + basalRng() * 0.16;
    const p = new THREE.Vector3(Math.cos(a) * r, basal.y - 0.06 - basalRng() * 0.08, Math.sin(a) * r * 0.4);
    somas.push({
      id: `basal:${i}`,
      kind: "basal",
      pos: p,
      radius: 0.055 + basalRng() * 0.012,
      label: "",
      sub: "",
      dim: true,
    });
    const join = basal.clone();
    join.y += 0.14;
    tubes.push({
      pts: catmull(p, join, basalRng, 0.06, 0.2),
      radius: 0.012,
      depth: 6,
    });
  }

  return { tubes, somas, root, sessionSomas, sessionIds };
}
