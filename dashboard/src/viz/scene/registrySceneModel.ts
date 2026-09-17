/**
 * The registry field as a renderer-neutral scene: bodies, hubs, evidenced
 * paths, axis ticks. A pure transform of `composeRegistryField`'s output. It
 * adds no relation and moves no coordinate — the field already measured every
 * position — it only names what a scene runtime has to draw and in what order.
 *
 * Every body carries the field's own mass radius so the three consumers of
 * that number (the anti-overlap pass, the crown geometry, the pick test)
 * cannot disagree about how big a project is.
 */
import {
  MASS_AXIS_HEIGHT,
  RECENCY_COLUMNS,
  bodyRadius,
  type FieldNode,
  type RegistryField,
} from '../../workspaces/brain/field.ts';
import type { FieldExtent } from '../graph/types.ts';

export type SceneBodyKind = 'project' | 'repository';

export interface SceneBody {
  readonly id: string;
  readonly label: string;
  readonly kind: SceneBodyKind;
  /** The registry's own `kind` word (`primary`, `worktree`, …); hue source. */
  readonly hueKey: string;
  readonly x: number;
  readonly y: number;
  /** Crown radius in field units. A hub's is a fixed categorical size. */
  readonly radius: number;
  /** Indexed mass for a project; checkout count for a hub. */
  readonly mass: number;
  /** Recency 0..1; resting luminance. */
  readonly vitality: number;
  /** Depth layer 0..2 — recency again, so dormant bodies recede. */
  readonly depth: number;
}

export interface ScenePath {
  readonly id: string;
  /** The project body; the path leaves from below its crown. */
  readonly from: string;
  /** The repository hub. */
  readonly to: string;
  /** The one relation this registry knows: same git directory. */
  readonly relation: 'checkout';
  readonly grade: 'exact';
}

export interface SceneColumn {
  readonly id: string;
  readonly label: string;
  readonly bound: string;
  readonly count: number;
  /** Column centre line, world x. */
  readonly x: number;
}

export interface RegistrySceneModel {
  readonly bodies: readonly SceneBody[];
  readonly paths: readonly ScenePath[];
  readonly columns: readonly SceneColumn[];
  /** Vertical hairlines between columns, world x. */
  readonly columnDividers: readonly number[];
  readonly massAxis: { readonly low: number; readonly high: number };
  readonly extent: FieldExtent;
  readonly byId: ReadonlyMap<string, SceneBody>;
  readonly pathsByBody: ReadonlyMap<string, readonly ScenePath[]>;
}

/** Fixed categorical size for a repository hub: it is an identity, not a
 * holding, so it must never read as a small project. */
export const HUB_RADIUS = 0.055;

/**
 * The drawn crown is larger than the field's clearance radius. The clearance
 * pass keeps body CENTRES apart; the luminous bodies themselves are additive
 * and may overlap into one field, which is what makes a crowded column read
 * as a dense band with structure in it rather than a row of separate discs.
 */
export const CROWN_SCALE = 1.6;

/** Depth is recency restated: live in front, dormant behind. */
export function depthFor(vitality: number): number {
  if (vitality >= 0.55) return 0;
  if (vitality >= 0.22) return 1;
  return 2;
}

/**
 * How far apart the recency columns are drawn, relative to the field's own
 * unit spacing, so the categorical axis fills a wide aperture instead of
 * leaving a band in its middle. Columns are ordered categories: their spacing
 * carries no measurement, so stretching it is not a lie about any project.
 * Body geometry is never stretched — only where its centre sits. Bounded so a
 * narrow viewport compresses at most to the field's own spacing.
 */
export function columnSpread(extent: FieldExtent, viewport: { width: number; height: number }): number {
  const spanX = Math.max(1e-6, extent.x[1] - extent.x[0]);
  const spanY = Math.max(1e-6, extent.y[1] - extent.y[0]);
  const aspect = viewport.height > 0 ? viewport.width / viewport.height : 1;
  return Math.max(1, Math.min(2.4, (aspect * spanY) / spanX));
}

/** The world x a body's centre is drawn at under a spread. */
export function spreadX(x: number, spread: number): number {
  return x * spread;
}

export function spreadExtent(extent: FieldExtent, spread: number): FieldExtent {
  return { x: [extent.x[0] * spread, extent.x[1] * spread], y: extent.y };
}

/**
 * The sampled curve of one evidenced path under a spread: it leaves from below
 * the project's crown — where the filaments hang — and sags toward the hub, so
 * the relation reads as connective tissue rather than a ruled chord.
 */
export function samplePath(
  model: RegistrySceneModel,
  path: ScenePath,
  spread: number,
  segments = 24,
): Array<readonly [number, number]> {
  const project = model.byId.get(path.from);
  const hub = model.byId.get(path.to);
  if (!project || !hub) return [];
  const from: readonly [number, number] = [spreadX(project.x, spread), project.y - project.radius * 1.35];
  const to: readonly [number, number] = [spreadX(hub.x, spread), hub.y];
  const midX = (from[0] + to[0]) / 2;
  const midY = (from[1] + to[1]) / 2;
  const sag = Math.min(0.35, Math.hypot(to[0] - from[0], to[1] - from[1]) * 0.22);
  const control: readonly [number, number] = [midX, midY - sag];
  const points: Array<readonly [number, number]> = [];
  for (let step = 0; step <= segments; step += 1) {
    const t = step / segments;
    const u = 1 - t;
    points.push([
      u * u * from[0] + 2 * u * t * control[0] + t * t * to[0],
      u * u * from[1] + 2 * u * t * control[1] + t * t * to[1],
    ]);
  }
  return points;
}

function toBody(node: FieldNode, massCeiling: number): SceneBody {
  const isHub = node.kind === 'repository';
  return {
    id: node.id,
    label: node.label,
    kind: isHub ? 'repository' : 'project',
    hueKey: node.kind,
    x: node.x,
    y: node.y,
    radius: isHub ? HUB_RADIUS : bodyRadius(node.degree, massCeiling) * CROWN_SCALE,
    mass: node.degree,
    vitality: node.vitality,
    depth: isHub ? 0 : depthFor(node.vitality),
  };
}

export function buildRegistryScene(field: RegistryField): RegistrySceneModel {
  const bodies = field.nodes.map((node) => toBody(node, field.massCeiling));
  const byId = new Map(bodies.map((body) => [body.id, body] as const));

  const paths: ScenePath[] = [];
  const pathsByBody = new Map<string, ScenePath[]>();
  for (const edge of field.edges) {
    const hub = byId.get(edge.source);
    const project = byId.get(edge.target);
    if (!hub || !project || hub.kind !== 'repository' || project.kind !== 'project') continue;
    const path: ScenePath = {
      id: `${edge.source}->${edge.target}`,
      from: project.id,
      to: hub.id,
      relation: 'checkout',
      grade: 'exact',
    };
    paths.push(path);
    for (const id of [project.id, hub.id]) {
      const list = pathsByBody.get(id);
      if (list) list.push(path);
      else pathsByBody.set(id, [path]);
    }
  }

  const columns: SceneColumn[] = field.columns.map((column, index) => ({
    ...column,
    x: index,
  }));
  const columnDividers = RECENCY_COLUMNS.slice(1).map((_, index) => index + 0.5);

  // The field's own frame clears the clearance radius; the drawn body is a
  // larger crown with filaments hanging below it, so the frame grows to hold
  // the tallest crown and the deepest tail actually present. Horizontal
  // margins are column edges, not data, and stay as the field set them.
  let top = field.extent.y[1];
  let bottom = field.extent.y[0];
  for (const body of bodies) {
    if (body.kind !== 'project') continue;
    top = Math.max(top, body.y + body.radius * 0.95);
    bottom = Math.min(bottom, body.y - body.radius * 1.7);
  }

  return {
    bodies,
    paths,
    columns,
    columnDividers,
    massAxis: { low: 0, high: MASS_AXIS_HEIGHT },
    extent: { x: field.extent.x, y: [bottom, top] },
    byId,
    pathsByBody,
  };
}

/**
 * The body under a world point: the smallest crown containing it, so a light
 * project sitting over a heavy one is still reachable. Hubs use their own
 * fixed radius. Returns null over empty field.
 */
export function pickBody(
  model: RegistrySceneModel,
  x: number,
  y: number,
  spread = 1,
): SceneBody | null {
  let best: SceneBody | null = null;
  for (const body of model.bodies) {
    // The crown sits a little above the origin and the filaments hang below;
    // the hit disc is centred between so either half of the body picks.
    const dx = x - spreadX(body.x, spread);
    const dy = y - (body.y - body.radius * 0.2);
    const reach = body.kind === 'repository' ? body.radius * 2.2 : body.radius * 1.05;
    if (dx * dx + dy * dy > reach * reach) continue;
    if (best === null || body.radius < best.radius) best = body;
  }
  return best;
}

/** World bounds of a set of bodies with their crowns and hanging filaments. */
export function bodiesBounds(
  model: RegistrySceneModel,
  ids: Iterable<string>,
  spread = 1,
): { x: [number, number]; y: [number, number] } | null {
  let minX = Infinity;
  let maxX = -Infinity;
  let minY = Infinity;
  let maxY = -Infinity;
  for (const id of ids) {
    const body = model.byId.get(id);
    if (!body) continue;
    const reach = body.kind === 'repository' ? body.radius * 3 : body.radius * 1.9;
    const cx = spreadX(body.x, spread);
    minX = Math.min(minX, cx - reach);
    maxX = Math.max(maxX, cx + reach);
    minY = Math.min(minY, body.y - reach);
    maxY = Math.max(maxY, body.y + body.radius * 1.2);
  }
  if (!Number.isFinite(minX)) return null;
  return { x: [minX, maxX], y: [minY, maxY] };
}
