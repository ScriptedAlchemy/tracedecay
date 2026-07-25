/**
 * The all-projects field: how the registry is COMPOSED, as a pure function of
 * the registry payload.
 *
 * The problem this replaces. The registry is, in practice, several dozen
 * repositories with exactly one checkout each. Drawing a synthetic hub node per
 * repository turned each of those into an isolated two-node component, and the
 * renderer's constellation packer then arranged the components on a ring — so
 * the whole surface read as one big circle of paired dots. Nothing about that
 * circle was true. The ring is a packing artifact; a reader looking at it sees
 * a cycle, an ordering, a centre and a periphery, and the registry has none of
 * those. The synthetic hubs were not true either: a repository with a single
 * checkout is that checkout, so the hub duplicated the node beside it and the
 * edge between them asserted a relation with no content.
 *
 * What replaces it. Every position on this field is a measurement:
 *
 *   x — time since TraceDecay last saw the project (`last_seen_at`), as ordered
 *       columns: today, this week, this month, this quarter, dormant. Columns
 *       rather than a continuous axis because the underlying quantity spans
 *       minutes to months, and because a column has a width to spread inside,
 *       which is what keeps bodies from fusing without ever moving one into a
 *       neighbouring column — an offset within a column costs nothing, an
 *       offset across one would be a lie about when the project was last seen.
 *
 *   y — indexed mass: how much TraceDecay actually holds for the project
 *       (stores + graph scopes + artifacts), on a log scale because the
 *       registry spans one artifact to several hundred.
 *
 *   size — the same mass, so the heavy brains are also the big bodies.
 *   brightness — recency again (`vitality`), so the left-hand columns burn and
 *       the dormant right-hand column sinks toward the substrate.
 *
 * A repository hub is materialised only when the repository genuinely has more
 * than one checkout. Then, and only then, the edges to its checkouts state
 * something real: these working copies share one git directory. The hub sits at
 * its checkouts' centroid, which is the honest position for "these ones".
 */
import type { ProjectRegistryEntry, ProjectRepoGroup } from './contracts.ts';

const DAY_SECONDS = 86_400;

export interface FieldNode {
  id: string;
  label: string;
  kind: string;
  /** Indexed mass; the canvas maps it to body radius. */
  degree: number;
  /** Recency, 0..1; the canvas maps it to resting luminance. */
  vitality: number;
  x: number;
  y: number;
}

export interface FieldEdge {
  source: string;
  target: string;
  kind: string;
}

export interface FieldColumn {
  id: string;
  /** Axis tick label. */
  label: string;
  /** The measurement the column actually bounds. */
  bound: string;
  count: number;
}

export interface RegistryField {
  nodes: FieldNode[];
  edges: FieldEdge[];
  columns: FieldColumn[];
  /** Largest mass on the field, so a caller can rank against the same ceiling. */
  massCeiling: number;
  /** Repositories that contributed a hub (more than one checkout). */
  sharedRepoCount: number;
  /** The axis frame, so the camera shows the whole scale rather than only the
   * part of it that is currently occupied. An empty column is a reading. */
  extent: { x: [number, number]; y: [number, number] };
}

/** Ordered recency columns. `maxDays` is exclusive; the last column is the
 * catch-all and is bounded by Infinity. */
const COLUMNS: ReadonlyArray<{
  id: string;
  label: string;
  bound: string;
  maxDays: number;
}> = [
  { id: 'today', label: 'today', bound: '< 24h', maxDays: 1 },
  { id: 'week', label: 'this week', bound: '< 7d', maxDays: 7 },
  { id: 'month', label: 'this month', bound: '< 30d', maxDays: 30 },
  { id: 'quarter', label: 'this quarter', bound: '< 90d', maxDays: 90 },
  { id: 'dormant', label: 'dormant', bound: '90d +', maxDays: Infinity },
];

/** How much TraceDecay holds for a project. Every term is a real registry
 * count; nothing here is weighted or invented. */
export function indexedMass(project: ProjectRegistryEntry): number {
  return project.store_count + project.graph_scope_count + project.artifact_count;
}

/** Age in days, floored at zero so a clock skew never reads as the future. */
export function ageDays(lastSeenAt: number, nowSeconds: number): number {
  return Math.max(0, (nowSeconds - lastSeenAt) / DAY_SECONDS);
}

/** Recency as luminance, 0..1: full at this moment, extinguished at a quarter.
 * Log-shaped because the difference between an hour and a day matters far more
 * than the difference between sixty and ninety days. */
export function recencyVitality(lastSeenAt: number, nowSeconds: number): number {
  const days = ageDays(lastSeenAt, nowSeconds);
  const decayed = 1 - Math.log1p(days) / Math.log1p(90);
  return Math.max(0, Math.min(1, decayed));
}

export function columnIndexFor(lastSeenAt: number, nowSeconds: number): number {
  const days = ageDays(lastSeenAt, nowSeconds);
  const index = COLUMNS.findIndex((column) => days < column.maxDays);
  return index === -1 ? COLUMNS.length - 1 : index;
}

/** Layout geometry, in the abstract units the canvas frames. Columns are one
 * unit apart; a body may move up to `COLUMN_HALF_WIDTH` off its column's centre
 * line, which is comfortably less than half the gap, so a body can never be
 * mistaken for a member of the column next door. */
const COLUMN_HALF_WIDTH = 0.42;
const MASS_AXIS_HEIGHT = 2.9;
/** Step size for the sideways nudges that keep bodies off each other. */
const NUDGE = 0.06;
/** A body's drawn radius in field units, so the clearance test knows how much
 * room each one actually takes. The canvas sizes bodies by the square root of
 * mass, and this mirrors that curve — otherwise the two heaviest projects in a
 * column, which are also the two largest discs, are the pair most likely to be
 * left overlapping by a clearance tuned for the small ones. */
function bodyRadius(mass: number, ceiling: number): number {
  return 0.09 + 0.15 * Math.sqrt(mass / Math.max(ceiling, 1));
}

/**
 * Compose the registry into the field. Deterministic: the same payload and the
 * same clock always produce the same coordinates, so screenshots are stable and
 * a re-render never reshuffles the picture under the reader.
 */
export function composeRegistryField(
  groups: readonly ProjectRepoGroup[],
  nowSeconds: number = Date.now() / 1000,
): RegistryField {
  const projects = groups.flatMap((group) => group.projects);
  const massCeiling = projects.reduce(
    (max, project) => Math.max(max, indexedMass(project)),
    0,
  );
  // The axis runs between the lightest and heaviest projects actually present.
  // Anchoring the floor at zero instead would spend a quarter of the field on a
  // mass no registered project can have — every project holds at least one
  // store — and squash the range that carries the reading.
  const massFloor = projects.reduce(
    (min, project) => Math.min(min, indexedMass(project)),
    Infinity,
  );
  const axisLow = Math.log1p(Math.max(1, Number.isFinite(massFloor) ? massFloor : 1));
  const axisSpan = Math.max(Math.log1p(Math.max(massCeiling, 1)) - axisLow, 0.001);

  const columns: FieldColumn[] = COLUMNS.map((column) => ({
    id: column.id,
    label: column.label,
    bound: column.bound,
    count: 0,
  }));

  // Place column by column so the anti-overlap pass only ever compares bodies
  // that could actually collide, and so its result cannot depend on the order
  // repositories happen to arrive in.
  const byColumn = new Map<number, ProjectRegistryEntry[]>();
  for (const project of projects) {
    const index = columnIndexFor(project.last_seen_at, nowSeconds);
    columns[index]!.count += 1;
    const bucket = byColumn.get(index);
    if (bucket) bucket.push(project);
    else byColumn.set(index, [project]);
  }

  const placedById = new Map<string, { x: number; y: number }>();
  const nodes: FieldNode[] = [];
  for (const [index, bucket] of [...byColumn.entries()].sort((a, b) => a[0] - b[0])) {
    // Heaviest first, then by id: mass decides who gets the uncontested centre
    // line, and the id tiebreak keeps the result independent of payload order.
    const ordered = [...bucket].sort(
      (a, b) =>
        indexedMass(b) - indexedMass(a) || a.project_id.localeCompare(b.project_id),
    );
    const settledHere: Array<{ offset: number; y: number; radius: number }> = [];
    for (const project of ordered) {
      const mass = indexedMass(project);
      // Sigma's y grows DOWNWARD on screen, so heavier has to be the larger
      // y for mass to read as height.
      const y =
        ((Math.log1p(mass) - axisLow) / axisSpan) * MASS_AXIS_HEIGHT;
      const radius = bodyRadius(mass, massCeiling);
      const offset = clearOffset(y, radius, settledHere);
      const x = index + offset;
      settledHere.push({ offset, y, radius });
      placedById.set(project.project_id, { x, y });
      nodes.push({
        id: project.project_id,
        label: project.label,
        kind: project.kind,
        degree: Math.max(mass, 1),
        vitality: recencyVitality(project.last_seen_at, nowSeconds),
        x,
        y,
      });
    }
  }

  // Shared-checkout repositories, and only those. The hub is the git directory
  // itself; its edges say "these working copies are the same repository", which
  // is the one relation this registry actually knows.
  const edges: FieldEdge[] = [];
  let sharedRepoCount = 0;
  for (const group of groups) {
    if (group.projects.length < 2) continue;
    sharedRepoCount += 1;
    const hubId = `repo:${group.git_common_dir ?? group.label}`;
    const anchors = group.projects
      .map((project) => placedById.get(project.project_id))
      .filter((point): point is { x: number; y: number } => point != null);
    if (anchors.length === 0) continue;
    const centroid = anchors.reduce(
      (acc, point) => ({ x: acc.x + point.x / anchors.length, y: acc.y + point.y / anchors.length }),
      { x: 0, y: 0 },
    );
    nodes.push({
      id: hubId,
      label: group.label,
      kind: 'repository',
      // A hub carries no mass of its own; it is sized by how many working
      // copies it actually binds together.
      degree: Math.max(group.projects.length, 1),
      // Lit by its most recently seen checkout — the repository is exactly as
      // live as the liveliest copy of it.
      vitality: group.projects.reduce(
        (max, project) =>
          Math.max(max, recencyVitality(project.last_seen_at, nowSeconds)),
        0,
      ),
      x: centroid.x,
      y: centroid.y,
    });
    for (const project of group.projects) {
      if (!placedById.has(project.project_id)) continue;
      edges.push({ source: hubId, target: project.project_id, kind: 'checkout' });
    }
  }

  // Margin in field units so a body's own radius and its label have room
  // inside the frame rather than being cropped by the bezel.
  const margin = 0.55;
  return {
    nodes,
    edges,
    columns,
    massCeiling,
    sharedRepoCount,
    extent: {
      x: [-margin, COLUMNS.length - 1 + margin],
      y: [-margin, MASS_AXIS_HEIGHT + margin],
    },
  };
}

/**
 * Smallest horizontal offset from a column's centre line that clears every body
 * already settled in that column, searched outward in alternating directions so
 * a column fills symmetrically. Capped at the column's own half-width: past
 * that the body stays put and is allowed to overlap, because two projects with
 * the same recency AND the same mass genuinely are in the same place, and
 * saying so is better than moving one of them somewhere it does not belong.
 */
function clearOffset(
  y: number,
  radius: number,
  settled: ReadonlyArray<{ offset: number; y: number; radius: number }>,
): number {
  const steps = Math.floor(COLUMN_HALF_WIDTH / NUDGE);
  for (let step = 0; step <= steps; step += 1) {
    for (const direction of step === 0 ? [0] : [1, -1]) {
      const offset = direction * step * NUDGE;
      const clear = settled.every(
        (point) =>
          Math.hypot(point.offset - offset, point.y - y) >= point.radius + radius,
      );
      if (clear) return offset;
    }
  }
  return 0;
}
