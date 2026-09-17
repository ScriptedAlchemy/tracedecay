/**
 * The luminous body of one project: a particulate crown, a handful of
 * dendritic filaments trailing below it, and a few soft emission anchors.
 *
 * Everything here is composition, not measurement. The ONLY quantities a body
 * carries from the registry are its crown radius (indexed mass, set by the
 * caller) and — applied later by the renderer — its resting brightness
 * (recency) and any admitted heat. Filaments are visual texture inside one
 * identity; they are never additional graph relations, and no particle is a
 * fact. The generator is seeded from the project id so the same registry
 * always draws the same body, on every reload and every renderer.
 *
 * Units are the caller's field units (columns one unit apart). Positions are
 * body-local: the crown centre is the origin, filaments hang toward -y.
 */

export interface NeuralBodySpec {
  /** Stable identity; the sole seed. */
  readonly id: string;
  /** Crown radius in field units — the caller's mass measurement. */
  readonly radius: number;
  /** Depth layer, 0 (front) to 2 (back). Softens and thins the body. */
  readonly depth: number;
}

export interface NeuralBodyGeometry {
  /** Dust particle count. */
  readonly count: number;
  /** xyz per dust particle, body-local. */
  readonly positions: Float32Array;
  /** Resting alpha per dust particle, 0..1. */
  readonly alphas: Float32Array;
  /** Sprite diameter per dust particle, field units. */
  readonly sizes: Float32Array;
  /** Filament polylines as segment pairs: x0 y0 x1 y1 per segment. */
  readonly filaments: Float32Array;
  /** Segment count in {@link filaments}. */
  readonly filamentSegments: number;
  /** Soft emission anchors: x y diameter alpha per anchor. */
  readonly glows: Float32Array;
  readonly glowCount: number;
  /** How far below the crown centre the filaments reach, field units. */
  readonly depthReach: number;
}

/** Deterministic PRNG; the same seed yields the same body forever. */
export function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** FNV-1a over UTF-16 code units. */
export function hashId(value: string): number {
  let hash = 2166136261;
  for (let index = 0; index < value.length; index += 1) {
    hash ^= value.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return hash >>> 0;
}

/** Normal deviate clipped at three sigma, so a body's envelope is bounded by
 * design rather than by luck: no particle wanders off into another column. */
function gaussian(random: () => number): number {
  const u = Math.max(1e-9, random());
  const v = random();
  return Math.max(-3, Math.min(3, Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v)));
}

/** Particle budget per body: heavier crowns hold more dust, bounded so a large
 * registry stays well inside one draw call's comfort. */
export function dustBudget(radius: number): number {
  return Math.round(Math.min(2600, Math.max(700, 9000 * radius)));
}

/** Depth softens: a body one layer back is thinner and its dust larger and
 * fainter, which the eye reads as defocus. */
function depthFactors(depth: number): { alpha: number; size: number } {
  const layer = Math.max(0, Math.min(2, Math.round(depth)));
  return {
    alpha: [1, 0.74, 0.52][layer]!,
    size: [1, 1.3, 1.65][layer]!,
  };
}

function cubic(
  t: number,
  p0: number,
  p1: number,
  p2: number,
  p3: number,
): number {
  const u = 1 - t;
  return u * u * u * p0 + 3 * u * u * t * p1 + 3 * u * t * t * p2 + t * t * t * p3;
}

export function buildNeuralBody(spec: NeuralBodySpec): NeuralBodyGeometry {
  const random = mulberry32(hashId(spec.id) ^ 0xb0d7);
  const r = Math.max(0.01, spec.radius);
  const { alpha: depthAlpha, size: depthSize } = depthFactors(spec.depth);
  const reach = r * (0.85 + random() * 0.45);
  const crownWidth = 0.78 + random() * 0.44;

  const count = dustBudget(r);
  const positions = new Float32Array(count * 3);
  const alphas = new Float32Array(count);
  const sizes = new Float32Array(count);
  let cursor = 0;
  const dot = (x: number, y: number, alpha: number, size: number): void => {
    if (cursor >= count) return;
    positions[cursor * 3] = x;
    positions[cursor * 3 + 1] = y;
    // A hair of z jitter so additive overlap never z-fights into banding.
    positions[cursor * 3 + 2] = (random() - 0.5) * 0.002;
    alphas[cursor] = Math.min(1, alpha * depthAlpha);
    sizes[cursor] = size * depthSize;
    cursor += 1;
  };

  const filamentPoints: number[] = [];
  const glowList: number[] = [];
  const glow = (x: number, y: number, diameter: number, alpha: number): void => {
    glowList.push(x, y, diameter * depthSize, alpha * depthAlpha);
  };

  // Roots: continuous filaments from a foot below the body up through a neck
  // into the crown. Their dust follows the same cubic, so the silhouette is a
  // hanging arbor rather than a comet.
  const roots = 5 + Math.floor(random() * 3);
  const rootShare = Math.floor(count * 0.3 / roots);
  for (let root = 0; root < roots; root += 1) {
    const footX = gaussian(random) * r * 0.08;
    const footY = -reach * (0.88 + random() * 0.14);
    const crownX = (root / (roots - 1) - 0.5) * r * 1.5 * crownWidth;
    const crownY = r * (0.18 + random() * 0.45);
    const neckX = crownX * 0.24;
    const c1x = footX + crownX * 0.2;
    const c1y = -reach * 0.58;
    const c2x = neckX;
    const c2y = -reach * 0.34;
    const steps = 14;
    let px = footX;
    let py = footY;
    for (let step = 1; step <= steps; step += 1) {
      const t = step / steps;
      const x = cubic(t, footX, c1x, c2x, neckX);
      const y = cubic(t, footY, c1y, c2y, 0);
      filamentPoints.push(px, py, x, y);
      px = x;
      py = y;
    }
    // Neck to crown: a second, shorter cubic that fans out.
    for (let step = 1; step <= 8; step += 1) {
      const t = step / 8;
      const x = cubic(t, neckX, neckX, crownX * 0.7, crownX);
      const y = cubic(t, 0, r * 0.22, crownY * 0.55, crownY);
      filamentPoints.push(px, py, x, y);
      px = x;
      py = y;
    }
    for (let index = 0; index < rootShare; index += 1) {
      const t = random();
      const x = cubic(t, footX, c1x, c2x, neckX);
      const y = cubic(t, footY, c1y, c2y, 0);
      dot(
        x + gaussian(random) * r * 0.045,
        y + gaussian(random) * r * 0.03,
        0.14 + random() ** 2 * 0.5,
        r * (0.02 + random() * 0.03),
      );
    }
    // Twigs off the crown branch.
    for (let branch = 0; branch < 3; branch += 1) {
      const t = 0.25 + branch * 0.25;
      const x = neckX + (crownX - neckX) * t * t;
      const y = crownY * t;
      const endX = crownX + (random() - 0.5) * r * 0.7;
      const endY = crownY + random() * r * 0.3;
      filamentPoints.push(x, y, endX, endY);
    }
    glow(footX, footY, r * 0.2, 0.3);
  }

  // The crown: a gaussian cloud, elliptical, sitting a little above the
  // origin so the filaments read as hanging from it.
  const crownShare = Math.floor(count * 0.52);
  for (let index = 0; index < crownShare; index += 1) {
    const angle = random() * Math.PI * 2;
    const radius = Math.abs(gaussian(random)) * r * 0.56;
    const x = Math.cos(angle) * radius * crownWidth;
    const y = Math.sin(angle) * radius * 0.82 + r * 0.18;
    dot(x, y, 0.12 + random() ** 3 * 0.85, r * (0.024 + random() * 0.04));
  }
  // Basal knot where the roots meet, so the foot is a place and not a fade.
  const basalShare = Math.floor(count * 0.1);
  for (let index = 0; index < basalShare; index += 1) {
    dot(
      gaussian(random) * r * 0.22,
      -reach + gaussian(random) * r * 0.1,
      0.08 + random() ** 2 * 0.42,
      r * (0.02 + random() * 0.03),
    );
  }
  // Faint outer scatter to give the body an atmosphere the eye can measure
  // the crown against.
  while (cursor < count) {
    const angle = random() * Math.PI * 2;
    const radius = r * (0.9 + random() * 0.9);
    dot(
      Math.cos(angle) * radius,
      Math.sin(angle) * radius * 0.9 + r * 0.1,
      0.03 + random() * 0.12,
      r * (0.02 + random() * 0.026),
    );
  }

  glow(0, -reach, r * 0.6, 0.4);
  glow(0, -reach * 0.5, r * 0.5, 0.18);
  glow(0, r * 0.2, r * 2.2, 0.34);
  glow(0, r * 0.2, r * 1.1, 0.42);
  glow(0, r * 0.2, r * 0.4, 0.85);

  return {
    count,
    positions,
    alphas,
    sizes,
    filaments: Float32Array.from(filamentPoints),
    filamentSegments: filamentPoints.length / 4,
    glows: Float32Array.from(glowList),
    glowCount: glowList.length / 4,
    depthReach: reach,
  };
}
