/**
 * Brain particle bakes — plate species:
 *  - bakeBody: one project body. Branched dendrites, a particulate crown,
 *    a basal dust knot, and selective emission bloom. Size = indexed mass (named measure). Brightness = recency.
 *  - geodesicBall: repo-neighborhood checkout orb (wireframe constellation).
 */

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

export function hash(s: string) {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

/** Kept for the profile pack binding (real session/message placements). */
export type NeuralArbor = {
  worktrees: string[];
  sessions: number;
  sessionIds?: string[];
  sessionWorktrees?: string[];
  messages: number;
  messagePlacements?: { sessionId: string; id: string }[];
  observations: number;
  observationPlacements?: { sessionId: string; kind: string; i: number }[];
  hook?: boolean;
  branches?: string[];
};

function hexToRgb(hex: string) {
  const h = hex.replace("#", "");
  return {
    r: parseInt(h.slice(0, 2), 16),
    g: parseInt(h.slice(2, 4), 16),
    b: parseInt(h.slice(4, 6), 16),
  };
}

/** Cap radius in css px for an indexed mass. Shared with the field layout. */
export function capRFor(mass: number) {
  return Math.min(84, 15 + 15.5 * Math.log10(Math.max(0, mass) + 2));
}

export type BodySpec = {
  id: string;
  color: string;
  mass: number;
  brightness: number;
  hook?: boolean;
};

export type BakedBody = {
  canvas: HTMLCanvasElement;
  /** soma origin inside canvas, device px (dpr 2) */
  ox: number;
  oy: number;
  capR: number;
};

const gauss = (r: () => number) => {
  const u = Math.max(1e-6, r());
  const v = r();
  return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v);
};

export type BodyAppearance = { glow: number; dust: number; branch: number };
export const DEFAULT_BODY_APPEARANCE: BodyAppearance = { glow: 1, dust: 1, branch: 1 };

export function bakeBody(spec: BodySpec, appearance = DEFAULT_BODY_APPEARANCE): BakedBody {
  const rng = mulberry32(hash(spec.id) ^ 0xb0d7);
  const capR = capRFor(spec.mass);
  const depth = capR * (0.95 + rng() * 0.65);
  const crownWidth = 0.75 + rng() * 0.5;
  const extent = capR * 2.2 + 24;
  const dpr = 2;
  const src = document.createElement("canvas");
  src.width = Math.ceil(extent * 2) * dpr;
  src.height = Math.ceil(extent + depth + 36) * dpr;
  const ctx = src.getContext("2d")!;
  const light = document.createElement("canvas");
  light.width = src.width;
  light.height = src.height;
  const emission = light.getContext("2d")!;
  for (const layer of [ctx, emission]) {
    layer.scale(dpr, dpr);
    layer.translate(extent, extent);
    layer.globalCompositeOperation = "lighter";
  }
  const rgb = hexToRgb(spec.color);
  const B = spec.brightness;
  const color = (a: number) => `rgba(${rgb.r},${rgb.g},${rgb.b},${a * B})`;
  const hot = (a: number) => `rgba(${Math.min(255, rgb.r + 125)},${Math.min(255, rgb.g + 100)},${Math.min(255, rgb.b + 125)},${a * B})`;
  const dot = (x: number, y: number, a: number, size = 0.75) => {
    ctx.fillStyle = color(a);
    ctx.fillRect(x, y, size, size);
  };
  const glow = (x: number, y: number, radius: number, strength: number) => {
    const g = emission.createRadialGradient(x, y, 0, x, y, radius);
    g.addColorStop(0, hot(strength));
    g.addColorStop(0.12, color(strength * 0.7));
    g.addColorStop(0.45, color(strength * 0.12));
    g.addColorStop(1, color(0));
    emission.fillStyle = g;
    emission.fillRect(x - radius, y - radius, radius * 2, radius * 2);
  };

  // Silhouette comes from continuous dendrites; dust must not bury the gaps.
  // These filaments are visual texture, not additional graph relationships.
  const roots = 5 + Math.floor(rng() * 3);
  for (let root = 0; root < roots; root++) {
    const footX = gauss(rng) * capR * 0.07;
    const footY = depth * (0.9 + rng() * 0.12);
    const crownX = (root / (roots - 1) - 0.5) * capR * 1.65 * crownWidth;
    const crownY = -capR * (0.2 + rng() * 0.5);
    const neckX = crownX * 0.24;
    const path = new Path2D();
    path.moveTo(footX, footY);
    path.bezierCurveTo(footX + crownX * 0.2, depth * 0.58, neckX, depth * 0.34, neckX, 0);
    path.bezierCurveTo(neckX, -capR * 0.22, crownX * 0.7, crownY * 0.55, crownX, crownY);
    ctx.strokeStyle = color(0.17 + rng() * 0.16);
    ctx.lineWidth = 0.4 * appearance.branch;
    ctx.stroke(path);
    emission.strokeStyle = color(0.14);
    emission.lineWidth = 1.2 * appearance.branch;
    emission.stroke(path);
    // Sample the same cubic as the stem so its dust follows the arbor.
    for (let i = 0; i < 110 * appearance.dust; i++) {
      const t = rng();
      const u = 1 - t;
      const x = u ** 3 * footX + 3 * u * u * t * (footX + crownX * 0.2)
        + 3 * u * t * t * neckX + t ** 3 * neckX;
      const y = u ** 3 * footY + 3 * u * u * t * depth * 0.58
        + 3 * u * t * t * depth * 0.34;
      dot(x + gauss(rng) * 2.5, y + gauss(rng) * 1.5,
        0.12 + rng() ** 2 * 0.48, 0.4 + rng() * 0.65);
    }
    for (let branch = 0; branch < 4; branch++) {
      const t = 0.18 + branch * 0.18;
      const x = neckX + (crownX - neckX) * t * t;
      const y = crownY * t;
      const endX = crownX + (rng() - 0.5) * capR * 0.72;
      const endY = crownY - rng() * capR * 0.25;
      ctx.beginPath();
      ctx.moveTo(x, y);
      ctx.quadraticCurveTo(x + (endX - x) * 0.15, endY * 0.7, endX, endY);
      ctx.strokeStyle = color(0.13 + rng() * 0.18);
      ctx.lineWidth = 0.4 * appearance.branch;
      ctx.stroke();
      for (let i = 0; i < 24; i++) {
        const u = rng();
        dot(x + (endX - x) * u + gauss(rng) * 2.4,
          y + (endY - y) * u + gauss(rng) * 2.4, 0.15 + rng() * 0.3);
      }
    }
    glow(footX, footY, 3, 0.3);
  }

  const count = Math.round(2400 * capR / 60 * appearance.dust);
  for (let i = 0; i < count; i++) {
    const angle = rng() * Math.PI * 2;
    const radius = Math.abs(gauss(rng)) * capR * 0.47;
    const x = Math.cos(angle) * radius * crownWidth;
    const y = Math.sin(angle) * radius * 0.75 - capR * 0.24;
    dot(x, y, 0.1 + rng() ** 3 * 0.75, 0.45 + rng() * 0.75);
    if (i % 75 === 0) glow(x, y, 2.5, 0.5);
  }
  // A separate basal cloud makes the body read as an arbor, not a comet.
  for (let i = 0; i < 550 * appearance.dust; i++) {
    dot(gauss(rng) * capR * 0.24, depth + gauss(rng) * capR * 0.09,
      0.07 + rng() ** 2 * 0.42, 0.5 + rng() * 0.6);
  }
  for (let i = 0; i < 210 * appearance.dust; i++) {
    const angle = rng() * Math.PI * 2;
    const radius = capR * (0.85 + rng() * 0.85);
    dot(Math.cos(angle) * radius, Math.sin(angle) * radius * 0.9,
      0.025 + rng() * 0.12, 0.55);
  }
  glow(0, depth, capR * 0.28, 0.65);
  glow(0, 0, capR * 0.6, 0.8);
  glow(0, 0, 6, 0.95);
  ctx.fillStyle = hot(0.8);
  ctx.fillRect(-0.65, -0.65, 1.3, 1.3);
  if (spec.hook) glow(capR * 0.2, -capR * 0.25, 3, 0.7);

  // Bloom only emission. Blurring all the dust turns the canopy into fog.
  const out = document.createElement("canvas");
  out.width = src.width;
  out.height = src.height;
  const composite = out.getContext("2d")!;
  composite.globalCompositeOperation = "lighter";
  composite.filter = "blur(7px)";
  composite.globalAlpha = Math.min(1, 0.5 * appearance.glow);
  composite.drawImage(light, 0, 0);
  composite.filter = "none";
  composite.globalAlpha = Math.min(1, appearance.glow);
  composite.drawImage(light, 0, 0);
  composite.globalAlpha = 1;
  composite.drawImage(src, 0, 0);
  return { canvas: out, ox: extent * dpr, oy: extent * dpr, capR };
}

/**
 * Geodesic checkout orb for the repository neighborhood: points on a sphere,
 * short chords between neighbors, brighter front hemisphere, faint rim.
 */
export function geodesicBall(id: string, color: string, radius: number): HTMLCanvasElement {
  const rng = mulberry32(hash(id) ^ 0x6e0);
  const dpr = 2;
  const size = Math.ceil(radius * 2.55);
  const c = document.createElement("canvas");
  c.width = size * dpr;
  c.height = size * dpr;
  const ctx = c.getContext("2d")!;
  ctx.scale(dpr, dpr);
  ctx.translate(size / 2, size / 2);
  ctx.globalCompositeOperation = "lighter";
  const rgb = hexToRgb(color);
  const wr = Math.min(255, rgb.r + 150);
  const wg = Math.min(255, rgb.g + 140);
  const wb = Math.min(255, rgb.b + 130);

  const n = Math.round(84 + radius * 0.95);
  const rot = rng() * Math.PI * 2;
  const pts: { x: number; y: number; z: number }[] = [];
  const ga = Math.PI * (3 - Math.sqrt(5));
  for (let i = 0; i < n; i++) {
    const yy = 1 - ((i + 0.5) / n) * 2;
    const rr = Math.sqrt(Math.max(0, 1 - yy * yy));
    const th = ga * i + rot;
    const X = Math.cos(th) * rr;
    const Z = Math.sin(th) * rr;
    // jitter keeps it constellation-like rather than a perfect lattice
    const jx = (rng() - 0.5) * 0.24;
    const jy = (rng() - 0.5) * 0.24;
    pts.push({ x: (X + jx) * radius, y: (yy + jy) * radius * 0.96, z: Z });
  }

  const maxChord = radius * 0.46;
  for (let i = 0; i < n; i++) {
    for (let j = i + 1; j < n; j++) {
      const a = pts[i];
      const b = pts[j];
      const dx = a.x - b.x;
      const dy = a.y - b.y;
      const dz = (a.z - b.z) * radius;
      const d = Math.sqrt(dx * dx + dy * dy + dz * dz);
      if (d > maxChord || rng() < 0.15) continue;
      const front = Math.max(0, (a.z + b.z) / 2 + 1) / 2;
      ctx.strokeStyle = `rgba(${rgb.r},${rgb.g},${rgb.b},${0.05 + front * 0.16})`;
      ctx.lineWidth = 0.55;
      ctx.beginPath();
      ctx.moveTo(a.x, a.y);
      ctx.lineTo(b.x, b.y);
      ctx.stroke();
    }
  }

  for (const p of pts) {
    const front = Math.max(0, p.z + 1) / 2;
    const hot = rng() < 0.045 && p.z > -0.1;
    if (hot) {
      const g = ctx.createRadialGradient(p.x, p.y, 0, p.x, p.y, 5.5);
      g.addColorStop(0, `rgba(${wr},${wg},${wb},0.9)`);
      g.addColorStop(1, `rgba(${rgb.r},${rgb.g},${rgb.b},0)`);
      ctx.fillStyle = g;
      ctx.beginPath();
      ctx.arc(p.x, p.y, 5.5, 0, Math.PI * 2);
      ctx.fill();
    }
    ctx.fillStyle = hot
      ? `rgba(${wr},${wg},${wb},0.95)`
      : `rgba(${rgb.r},${rgb.g},${rgb.b},${0.2 + front * 0.5})`;
    ctx.beginPath();
    ctx.arc(p.x, p.y, hot ? 1.6 : 0.45 + front * 0.45, 0, Math.PI * 2);
    ctx.fill();
  }

  // interior nebula dust so the orb reads as a filled cloud, not a cage
  for (let i = 0; i < 260; i++) {
    const u = rng() * 2 - 1;
    const phi = rng() * Math.PI * 2;
    const rr = Math.cbrt(rng()) * radius * 0.94;
    const sq = Math.sqrt(Math.max(0, 1 - u * u));
    const x = sq * Math.cos(phi) * rr;
    const y = u * rr * 0.96;
    const z = sq * Math.sin(phi);
    const front = Math.max(0, z + 1) / 2;
    ctx.fillStyle = `rgba(${rgb.r},${rgb.g},${rgb.b},${0.05 + front * 0.12 + rng() * 0.05})`;
    ctx.fillRect(x, y, rng() < 0.08 ? 1.4 : 0.9, 0.9);
  }

  // bright center knot + faint rim
  const knot = ctx.createRadialGradient(0, 0, 0, 0, 0, radius * 0.045);
  knot.addColorStop(0, `rgba(${wr},${wg},${wb},0.75)`);
  knot.addColorStop(1, `rgba(${rgb.r},${rgb.g},${rgb.b},0)`);
  ctx.fillStyle = knot;
  ctx.beginPath();
  ctx.arc(0, 0, radius * 0.045, 0, Math.PI * 2);
  ctx.fill();
  ctx.strokeStyle = `rgba(${rgb.r},${rgb.g},${rgb.b},0.16)`;
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.arc(0, 0, radius * 1.06, 0, Math.PI * 2);
  ctx.setLineDash([0.7, 5]);
  ctx.stroke();

  const out = document.createElement("canvas");
  out.width = c.width;
  out.height = c.height;
  const o = out.getContext("2d")!;
  o.globalCompositeOperation = "lighter";
  o.filter = "blur(5px)";
  o.globalAlpha = 0.22;
  o.drawImage(c, 0, 0);
  o.filter = "none";
  o.globalAlpha = 1;
  o.drawImage(c, 0, 0);
  return out;
}
