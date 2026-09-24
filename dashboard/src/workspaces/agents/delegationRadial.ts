import type { DelegationTopologyModel, TopologyMark } from './delegationTopology.ts';

/**
 * The radial delegation field: the fitted topology folded around an origin.
 *
 * A polar re-reading of the same model, not a second layout. Angle is the
 * mark's leaf-slot row, so siblings keep the order and spacing the column
 * field gives them and a parent sits at the mean angle of its children. Radius
 * is generation. A reading with one top puts that session at the centre; a
 * reading with several has no single source, so the centre is the reading's
 * origin, a label and never a node, and every top sits on the first ring. No
 * edge is drawn to the origin: it would be a delegation nobody recorded.
 *
 * Fan-out is drawn as a bracket: a stem from the parent to a ring midway to
 * its children, one arc along that ring spanning them, and a spoke out to
 * each. A parent with one drawn child gets a plain elbow.
 */

export interface RadialPlacement {
  readonly id: string;
  readonly angle: number;
  readonly radius: number;
  readonly x: number;
  readonly y: number;
}

export interface RadialFan {
  readonly parentId: string;
  /** SVG path data for the stem, arc and spokes, relative to the centre. */
  readonly path: string;
  readonly childIds: readonly string[];
}

export interface DelegationRadialModel {
  readonly placements: ReadonlyMap<string, RadialPlacement>;
  readonly fans: readonly RadialFan[];
  /** Radii of the generation rings actually used, innermost first. */
  readonly rings: readonly number[];
  readonly centre: 'top' | 'origin';
  readonly outerRadius: number;
  /** The ray through the widest empty sector, where ring captions sit
   * without overprinting a mark. */
  readonly captionAngle: number;
}

const round = (value: number) => Math.round(value * 100) / 100;

function polar(angle: number, radius: number): { x: number; y: number } {
  return { x: round(Math.cos(angle) * radius), y: round(Math.sin(angle) * radius) };
}

export function layoutDelegationRadial(
  model: DelegationTopologyModel,
  pitch: number,
): DelegationRadialModel {
  const tops = model.marks.filter((mark) => mark.parentId === null);
  const centre: DelegationRadialModel['centre'] = tops.length === 1 ? 'top' : 'origin';
  const ringOf = (generation: number) => (centre === 'top' ? generation : generation + 1);
  const slots = Math.max(1, model.rows);
  const angleOf = (mark: TopologyMark) => -Math.PI / 2 + (2 * Math.PI * (mark.row + 0.5)) / slots;

  const placements = new Map<string, RadialPlacement>();
  for (const mark of model.marks) {
    const radius = ringOf(mark.generation) * pitch;
    const angle = radius === 0 ? 0 : angleOf(mark);
    placements.set(mark.id, { id: mark.id, angle, radius, ...polar(angle, radius) });
  }

  const byParent = new Map<string, TopologyMark[]>();
  for (const mark of model.marks) {
    if (mark.parentId === null) continue;
    const bucket = byParent.get(mark.parentId);
    if (bucket) bucket.push(mark);
    else byParent.set(mark.parentId, [mark]);
  }

  const fans: RadialFan[] = [];
  for (const [parentId, children] of byParent) {
    const parent = placements.get(parentId);
    if (parent === undefined) continue;
    const placed = children
      .map((child) => placements.get(child.id)!)
      .sort((a, b) => a.angle - b.angle);
    const childRadius = placed[0]!.radius;
    const mid = parent.radius + (childRadius - parent.radius) / 2;
    const first = placed[0]!;
    const last = placed[placed.length - 1]!;
    const segments: string[] = [];
    if (parent.radius > 0) {
      const stemEnd = polar(parent.angle, mid);
      segments.push(`M${parent.x},${parent.y} L${stemEnd.x},${stemEnd.y}`);
    }
    // From the centre the spokes already fan; a bracket arc is for a parent
    // on a ring, and a single child sits at its parent's angle.
    if (parent.radius > 0 && placed.length > 1) {
      const from = polar(first.angle, mid);
      const to = polar(last.angle, mid);
      const large = last.angle - first.angle > Math.PI ? 1 : 0;
      segments.push(`M${from.x},${from.y} A${round(mid)},${round(mid)} 0 ${large} 1 ${to.x},${to.y}`);
    }
    for (const child of placed) {
      const start = parent.radius > 0 ? polar(child.angle, mid) : { x: 0, y: 0 };
      segments.push(`M${start.x},${start.y} L${child.x},${child.y}`);
    }
    fans.push({ parentId, path: segments.join(' '), childIds: placed.map((child) => child.id) });
  }

  const rings = [...new Set(model.marks.map((mark) => ringOf(mark.generation) * pitch))]
    .filter((radius) => radius > 0)
    .sort((a, b) => a - b);
  const angles = [...placements.values()]
    .filter((placement) => placement.radius > 0)
    .map((placement) => placement.angle)
    .sort((a, b) => a - b);
  let captionAngle = Math.PI;
  let widest = 0;
  angles.forEach((angle, index) => {
    const next = index + 1 < angles.length ? angles[index + 1]! : angles[0]! + 2 * Math.PI;
    if (next - angle > widest) {
      widest = next - angle;
      captionAngle = round(angle + (next - angle) / 2);
    }
  });
  return { placements, fans, rings, centre, outerRadius: rings[rings.length - 1] ?? 0, captionAngle };
}

export interface LabelBox {
  readonly id: string;
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
}

/**
 * Screen-space label culling: candidates in priority order, each kept only if
 * its box overlaps no box already kept. Deterministic in the order given, so
 * the same reading labels the same marks on every render. A culled label is
 * not lost; hover, selection and the exact tree still name the mark.
 */
export function cullLabels(candidates: readonly LabelBox[]): ReadonlySet<string> {
  const kept: LabelBox[] = [];
  for (const box of candidates) {
    const hit = kept.some(
      (other) =>
        box.x < other.x + other.width &&
        other.x < box.x + box.width &&
        box.y < other.y + other.height &&
        other.y < box.y + box.height,
    );
    if (!hit) kept.push(box);
  }
  return new Set(kept.map((box) => box.id));
}

/** Ring pitch that fits the deepest drawn ring in a square field of `size`
 * pixels. Labels print only through generation 1, so the outer ring needs
 * label room only when it is one of those; deeper rings keep a mark's width. */
export function radialPitch(model: DelegationTopologyModel, size: number): number {
  const tops = model.marks.filter((mark) => mark.parentId === null).length;
  const rings = model.columns - 1 + (tops === 1 ? 0 : 1);
  if (rings <= 0) return 0;
  const labelRoom = model.columns - 1 <= 1 ? 84 : 32;
  return Math.max(28, (size / 2 - labelRoom) / rings);
}
