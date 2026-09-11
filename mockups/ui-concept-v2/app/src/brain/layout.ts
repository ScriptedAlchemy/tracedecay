import { PROJECTS, RECENCY_AXIS, type ProjectBody, type RecencyBucket } from "../data/fixtures";
import { capRFor } from "./particles";
import { recencyPeerOffsets } from "./recencyLayout";

const BUCKET_INDEX: Record<RecencyBucket, number> = {
  "<5min": 0,
  "5min-1h": 1,
  "1h-1d": 2,
  "1d-1w": 3,
  "1w-1m": 4,
  "1m-6m": 5,
  "6m-1y": 6,
  ">1y": 7,
};

export const BRIGHTNESS: Record<RecencyBucket, number> = {
  "<5min": 1,
  "5min-1h": 0.9,
  "1h-1d": 0.82,
  "1d-1w": 0.72,
  "1w-1m": 0.6,
  "1m-6m": 0.52,
  "6m-1y": 0.4,
  ">1y": 0.26,
};

export type BodyLayout = {
  project: ProjectBody;
  x: number;
  y: number;
  capR: number;
  /** label anchor offset from the soma */
  labelDx: number;
  labelDy: number;
};

export type FieldLayout = {
  bodies: BodyLayout[];
  hub: { x: number; y: number };
  width: number;
  height: number;
};

function separateBodies(bodies: BodyLayout[], top: number, bottom: number) {
  const ordered = [...bodies].sort((a, b) => a.y - b.y || (a.project.id < b.project.id ? -1 : 1));
  for (let pass = 0; pass < 64; pass++) {
    let moved = false;
    for (let i = 0; i < ordered.length; i++) {
      for (let j = i + 1; j < ordered.length; j++) {
        const a = ordered[i], b = ordered[j];
        const dy = b.y - a.y;
        const needed = a.capR + b.capR + 4;
        const separation = Math.sqrt(Math.max(0, needed * needed - (b.x - a.x) ** 2));
        if (Math.abs(dy) >= separation) continue;
        const push = separation - Math.abs(dy) + 0.1;
        const upper = a, lower = b;
        const up = Math.min(push / 2, upper.y - (top + upper.capR));
        const down = push - up;
        upper.y -= up;
        lower.y += down;
        moved = true;
      }
    }
    for (const body of bodies) body.y = Math.max(top + body.capR, Math.min(bottom - body.capR, body.y));
    if (!moved) break;
  }
  // A final ordered sweep prevents a later pair from re-compressing an
  // already-separated vertical lane. The field has room for this registry.
  for (let i = 0; i < ordered.length; i++) for (let j = i + 1; j < ordered.length; j++) {
    const a = ordered[i], b = ordered[j];
    const separation = Math.sqrt(Math.max(0, (a.capR + b.capR + 4) ** 2 - (b.x - a.x) ** 2));
    b.y = Math.max(b.y, a.y + separation);
  }
}

function circleHitsBox(circle: BodyLayout, box: { x: number; y: number; w: number; h: number }) {
  const x = Math.max(box.x, Math.min(circle.x, box.x + box.w));
  const y = Math.max(box.y, Math.min(circle.y, box.y + box.h));
  return Math.hypot(circle.x - x, circle.y - y) < circle.capR + 3;
}

/**
 * x = measured recency bucket. y = indexed mass (log), high mass at top.
 * Peers inside one bucket stay inside their column band; only mass
 * separates them vertically.
 */
export function layoutField(width: number, height: number): FieldLayout {
  const left = 86;
  const right = 30;
  const top = 118;
  const bottom = 108;
  const innerW = Math.max(200, width - left - right);
  const innerH = Math.max(200, height - top - bottom);
  const colW = innerW / RECENCY_AXIS.length;
  const peerOffsets = recencyPeerOffsets(PROJECTS, colW);
  const maxLog = Math.max(...PROJECTS.map((p) => Math.log10(p.indexedMass + 1)));

  const bodies = PROJECTS.map((project) => {
    const bi = BUCKET_INDEX[project.recency];
    const capR = capRFor(project.indexedMass);
    const spreadX = peerOffsets.get(project.id) ?? 0;
    const x = left + (bi + 0.5) * colW + spreadX;
    const massNorm = Math.log10(project.indexedMass + 1) / maxLog;
    const y = top + capR * 0.9 + (1 - massNorm) * (innerH - capR * 0.9 - 60);
    return {
      project,
      x,
      y,
      capR,
      labelDx: capR * 0.66 + 12,
      labelDy: -capR * 0.62 - 8,
    };
  });

  // Recency owns x. A stable, bounded relaxation assigns only y, so close
  // timestamps can never make their indexed-mass bodies overlap.
  separateBodies(bodies, top, top + innerH);

  // Keep captions near their soma without putting text over arbors or the HUD.
  type Box = { x: number; y: number; w: number; h: number };
  const overlap = (a: Box, b: Box) => Math.max(0, Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x))
    * Math.max(0, Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y));
  const occupied: Box[] = [
    { x: 0, y: 0, w: width, h: 92 },
    { x: 0, y: 92, w: 42, h: height - 92 },
    { x: 16, y: height - 230, w: 218, h: 218 },
    { x: width - 160, y: height - 56, w: 152, h: 48 },
    { x: left + innerW * 0.5 - 80, y: height - 100, w: 160, h: 66 },
    ...bodies.flatMap((b) => [
      { x: b.x - b.capR * 1.1, y: b.y - b.capR * 0.85, w: b.capR * 2.2, h: b.capR * 1.15 },
      { x: b.x - b.capR * 0.3, y: b.y + b.capR * 0.3, w: b.capR * 0.6, h: b.capR * 1.45 },
    ]),
  ];
  for (const body of [...bodies].sort((a, b) => b.project.name.length - a.project.name.length)) {
    const w = Math.max(body.project.name.length * 7, 100) + 8;
    const h = 68;
    const r = body.capR;
    const candidates: Box[] = [];
    for (const gap of [8, 44, 80, 116]) {
      for (const [dx, dy] of [
        [r * 0.3 + gap, r * 0.3 + gap], [-r * 0.3 - gap - w, r * 0.3 + gap],
        [r * 1.1 + gap, -h], [-r * 1.1 - gap - w, -h],
        [r * 1.1 + gap, -h / 2], [-r * 1.1 - gap - w, -h / 2],
        [-w / 2, -r * 0.85 - gap - h], [-w / 2, r * 1.75 + gap],
        [r * 1.1 + gap, -r * 0.85 - gap - h],
        [-r * 1.1 - gap - w, -r * 0.85 - gap - h],
        [r * 1.1 + gap, r * 1.75 + gap],
        [-r * 1.1 - gap - w, r * 1.75 + gap],
      ]) candidates.push({ x: Math.max(18, Math.min(width - w - 12, body.x + dx)),
        y: Math.max(96, Math.min(height - h - 12, body.y + dy)), w, h });
    }
    const cost = (box: Box) => occupied.reduce((sum, other) => sum + overlap(box, other), 0) * 10000
      + bodies.filter(other => other !== body).reduce((sum, other) => sum + (circleHitsBox(other, box) ? 1e9 : 0), 0)
      + Math.hypot(box.x + w / 2 - body.x, box.y + h / 2 - body.y)
      + Math.hypot(box.x - body.x - body.labelDx, box.y - body.y - body.labelDy) * 0.15;
    const best = candidates.reduce((a, b) => cost(a) <= cost(b) ? a : b);
    body.labelDx = best.x - body.x;
    body.labelDy = best.y - body.y;
    occupied.push(best);
  }

  return {
    bodies,
    hub: { x: left + innerW * 0.5, y: height - 82 },
    width,
    height,
  };
}

export function hitTest(field: FieldLayout, mx: number, my: number): string | null {
  let best: { id: string; d: number } | null = null;
  for (const b of field.bodies) {
    const d = Math.hypot(mx - b.x, (my - b.y) / 1.25);
    const r = Math.max(30, b.capR * 0.95);
    if (d < r && (!best || d < best.d)) best = { id: b.project.id, d };
  }
  return best?.id ?? null;
}
