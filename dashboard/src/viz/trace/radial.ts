/**
 * Layout for the RADIAL NEIGHBOURHOOD candidate.
 *
 * The focus at the centre; concentric rings are hop distance; angular
 * sectors are the MODULE (directory of the file each row carried; a row with
 * no path shares one `file absent` sector, never a guessed one). Sectors are ordered by the mean side
 * of their symbols, so caller-heavy files sweep the left and top and
 * callee-heavy files the right and bottom. Channels are thin curves pulled
 * through their sector's spoke, so one file's calls bundle into one stroke.
 *
 * What the frame does not draw is aggregated per sector just outside ring 2:
 * symbols the fetched lists named but the budget left off, and edges incident
 * on the sector's drawn symbols that no channel carries. Both are exact
 * counts, printed apart, never summed into one number.
 *
 * Pure and DOM-free.
 */
import type { UndrawnNeighbour } from './model.ts';
import type { TraceChannel, TraceModel, TraceNode } from './types.ts';
import { channelKey } from './variants.ts';

export interface RadialSector {
  readonly key: string;
  readonly path: string | null;
  readonly label: string;
  readonly a0: number;
  readonly a1: number;
  readonly hiddenSymbols: number;
  /** Σ undrawn edges of drawn symbols; null when any member's degree is absent. */
  readonly hiddenEdges: number | null;
  readonly degreeAbsent: number;
}

export interface RadialNode {
  readonly node: TraceNode;
  readonly x: number;
  readonly y: number;
  readonly angle: number;
  readonly radius: number;
  readonly sector: string;
}

export interface RadialEdge {
  readonly key: string;
  readonly channel: TraceChannel;
  readonly d: string;
  readonly width: number;
  readonly upstream: boolean;
}

export interface RadialLayout {
  readonly width: number;
  readonly height: number;
  readonly compact: boolean;
  readonly cx: number;
  readonly cy: number;
  readonly rings: readonly { hop: number; r: number }[];
  readonly aggregateR: number;
  readonly sectors: readonly RadialSector[];
  readonly nodes: readonly RadialNode[];
  readonly edges: readonly RadialEdge[];
}

export const RADIAL_COMPACT_BELOW = 560;
const GAP = 0.035;

/** The directory a row's file sits in: the sector a symbol belongs to. */
export function moduleOf(path: string | null): string | null {
  if (path === null) return null;
  const slash = path.lastIndexOf('/');
  return slash < 0 ? '.' : path.slice(0, slash);
}

export function arcPath(cx: number, cy: number, r: number, a0: number, a1: number): string {
  const x0 = cx + r * Math.cos(a0);
  const y0 = cy + r * Math.sin(a0);
  const x1 = cx + r * Math.cos(a1);
  const y1 = cy + r * Math.sin(a1);
  const large = a1 - a0 > Math.PI ? 1 : 0;
  return `M${x0.toFixed(1)},${y0.toFixed(1)} A${r},${r} 0 ${large} 1 ${x1.toFixed(1)},${y1.toFixed(1)}`;
}

export function layoutRadial(
  model: TraceModel,
  undrawn: readonly UndrawnNeighbour[],
  width: number,
): RadialLayout {
  const W = Math.max(240, width);
  const compact = W < RADIAL_COMPACT_BELOW;
  // Sector labels sit left and right of the rim, so the side margin is wide
  // and the top and bottom one only clears a label line.
  const R = compact ? W / 2 - 16 : Math.min(W / 2 - 150, 270);
  const size = compact ? W : 2 * R + 84;
  const cx = W / 2;
  const cy = size / 2;
  const maxHop = Math.max(1, ...model.nodes.map((node) => Math.abs(node.ring)));
  const ringR = (hop: number) => (hop === 0 ? 0 : maxHop === 1 ? R * 0.62 : hop === 1 ? R * 0.4 : R * 0.9);
  const rings = Array.from({ length: maxHop }, (_, i) => ({ hop: i + 1, r: ringR(i + 1) }));
  const aggregateR = ringR(maxHop) + (compact ? 10 : 18);

  /* ---- sectors ---------------------------------------------------------- */
  const keyOf = (path: string | null) => moduleOf(path) ?? '\0absent';
  const groups = new Map<string, { path: string | null; members: TraceNode[]; hidden: number }>();
  for (const node of model.nodes) {
    if (node.id === model.focusId) continue;
    const key = keyOf(node.filePath);
    const group = groups.get(key) ?? { path: moduleOf(node.filePath), members: [], hidden: 0 };
    group.members.push(node);
    groups.set(key, group);
  }
  for (const entry of undrawn) {
    const key = keyOf(entry.filePath);
    const group = groups.get(key) ?? { path: moduleOf(entry.filePath), members: [], hidden: 0 };
    group.hidden += 1;
    groups.set(key, group);
  }
  const meanRing = (members: TraceNode[]) =>
    members.length ? members.reduce((sum, node) => sum + node.ring, 0) / members.length : 0;
  const ordered = [...groups].sort(
    ([ka, a], [kb, b]) => meanRing(a.members) - meanRing(b.members) || ka.localeCompare(kb),
  );
  const slots = ordered.map(([, group]) =>
    Math.max(
      1,
      ...rings.map((ring) => group.members.filter((node) => Math.abs(node.ring) === ring.hop).length),
    ),
  );
  const totalSlots = slots.reduce((sum, n) => sum + n, 0);
  const sweep = Math.PI * 2 - GAP * ordered.length;
  let angle = -Math.PI;
  const sectors: RadialSector[] = [];
  const nodes: RadialNode[] = [];
  ordered.forEach(([key, group], i) => {
    const span = (slots[i]! / Math.max(1, totalSlots)) * sweep;
    const a0 = angle + GAP / 2;
    const a1 = a0 + span;
    angle += span + GAP;
    const absentDegrees = group.members.filter((node) => node.undrawnEdges === null).length;
    sectors.push({
      key,
      path: group.path,
      label: group.path ?? 'file absent',
      a0,
      a1,
      hiddenSymbols: group.hidden,
      hiddenEdges:
        absentDegrees > 0 ? null : group.members.reduce((sum, node) => sum + (node.undrawnEdges ?? 0), 0),
      degreeAbsent: absentDegrees,
    });
    for (const ring of rings) {
      const onRing = group.members
        .filter((node) => Math.abs(node.ring) === ring.hop)
        .sort((a, b) => a.ring - b.ring || a.name.localeCompare(b.name));
      onRing.forEach((node, k) => {
        const theta = a0 + ((k + 0.5) * (a1 - a0)) / onRing.length;
        nodes.push({
          node,
          angle: theta,
          radius: ring.r,
          x: cx + ring.r * Math.cos(theta),
          y: cy + ring.r * Math.sin(theta),
          sector: key,
        });
      });
    }
  });
  const focusNode = model.nodes.find((node) => node.id === model.focusId);
  if (focusNode) nodes.push({ node: focusNode, angle: 0, radius: 0, x: cx, y: cy, sector: '' });

  /* ---- edges ------------------------------------------------------------ */
  const placed = new Map(nodes.map((entry) => [entry.node.id, entry]));
  const sectorMid = new Map(sectors.map((sector) => [sector.key, (sector.a0 + sector.a1) / 2]));
  const edges: RadialEdge[] = [];
  const f = (n: number) => n.toFixed(1);
  for (const channel of model.channels) {
    const p = placed.get(channel.a);
    const q = placed.get(channel.b);
    if (!p || !q) continue;
    const [inner, outer] = p.radius <= q.radius ? [p, q] : [q, p];
    let d: string;
    if (inner.radius === 0) {
      // Focus spoke: every channel into one file leaves along that file's
      // mid-angle, so the file's calls read as one bundle before they fan.
      const mid = sectorMid.get(outer.sector)!;
      const rc = outer.radius * 0.55;
      d = `M${f(inner.x)},${f(inner.y)} Q${f(cx + rc * Math.cos(mid))},${f(cy + rc * Math.sin(mid))} ${f(outer.x)},${f(outer.y)}`;
    } else if (inner.sector === outer.sector) {
      const mid = sectorMid.get(inner.sector)!;
      const rc = (inner.radius + outer.radius) / 2;
      d = `M${f(inner.x)},${f(inner.y)} Q${f(cx + rc * Math.cos(mid))},${f(cy + rc * Math.sin(mid))} ${f(outer.x)},${f(outer.y)}`;
    } else {
      // Across modules: a chord bowed toward the centre, so cross-module
      // traffic reads as the web it is rather than hiding behind a sector.
      const mx = cx + ((inner.x + outer.x) / 2 - cx) * 0.55;
      const my = cy + ((inner.y + outer.y) / 2 - cy) * 0.55;
      d = `M${f(inner.x)},${f(inner.y)} Q${f(mx)},${f(my)} ${f(outer.x)},${f(outer.y)}`;
    }
    edges.push({
      key: channelKey(channel),
      channel,
      d,
      width: 0.8 + Math.sqrt(Math.max(0, channel.calls)) * 0.45,
      upstream: channel.dir === 'up' || (channel.dir === 'in' && Math.min(p.node.ring, q.node.ring) < 0),
    });
  }
  edges.sort((a, b) => a.channel.calls - b.channel.calls);

  return {
    width: W,
    height: size,
    compact,
    cx,
    cy,
    rings,
    aggregateR,
    sectors,
    nodes,
    edges,
  };
}

/** The aggregate reading printed on one sector's outer arc. */
export function sectorReading(sector: RadialSector): string {
  const parts: string[] = [];
  if (sector.hiddenSymbols > 0) parts.push(`+${sector.hiddenSymbols} sym`);
  if (sector.hiddenEdges === null) parts.push(`${sector.degreeAbsent} degree absent`);
  else if (sector.hiddenEdges > 0) parts.push(`${sector.hiddenEdges} edges`);
  return parts.join(' · ');
}
