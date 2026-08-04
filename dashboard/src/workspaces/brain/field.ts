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
import {
  type ProjectRegistryEntry,
  type ProjectRepoGroup,
} from '../../contracts/generated.ts';

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

/** What the mass axis actually holds, so the caption can state the shape the
 * axis has rather than leaving the reader to infer it from a crowd of bodies
 * along the bottom. */
export interface MassSummary {
  floor: number;
  ceiling: number;
  median: number;
  /** Projects sitting in the lower half of the LOG axis — the crowd. */
  lowerHalfCount: number;
  total: number;
}

export interface RegistryField {
  nodes: FieldNode[];
  edges: FieldEdge[];
  columns: FieldColumn[];
  /** Largest mass on the field, so a caller can rank against the same ceiling. */
  massCeiling: number;
  mass: MassSummary;
  /**
   * The age, in days, at which a body's brightness reaches zero on THIS field.
   * Relative to the registry rather than fixed, and stated in the caption
   * because of it — see `recencyVitality`.
   */
  vitalityHorizonDays: number;
  /** Repositories that contributed a hub (more than one checkout). */
  sharedRepoCount: number;
  /** The axis frame, so the camera shows the whole scale rather than only the
   * part of it that is currently occupied. An empty column is a reading. */
  extent: { x: [number, number]; y: [number, number] };
}

/** Ordered recency columns. `maxDays` is exclusive; the last column is the
 * catch-all and is bounded by Infinity.
 *
 * Exported because the Delivery field asks a different question of the same
 * registry (branch composition rather than indexed mass) but has to place its
 * bodies on the SAME time ladder — two surfaces that both say "this week" have
 * to mean the same seven days. A second copy of these bounds would drift the
 * first time one of them was tuned. */
export const RECENCY_COLUMNS: ReadonlyArray<{
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

/** Below this the horizon is clamped: a registry whose oldest project was seen
 * an hour ago has no meaningful age range, and dividing by it would turn
 * minutes of drift into the whole luminance scale. */
const MIN_VITALITY_HORIZON_DAYS = 1;

/**
 * Recency as luminance, 0..1: full at this moment, extinguished at `horizon`.
 *
 * Log-shaped because the difference between an hour and a day matters far more
 * than the difference between sixty and ninety days.
 *
 * The horizon is a parameter, and `composeRegistryField` sets it to the age of
 * the OLDEST project on the field rather than leaving it at a fixed quarter.
 * With the fixed 90-day horizon a registry whose projects span ten days
 * occupied only the top half of the scale — today read 1.00 and this-week read
 * 0.85, a luminance difference of fifteen percent that no eye separates on a
 * dark field. Anchored to the observed range the same registry uses the whole
 * scale: today burns, ten days ago is out.
 *
 * This makes brightness a RELATIVE channel, exactly as the mass axis is already
 * relative to the lightest and heaviest projects present, and it is stated on
 * the field's caption for the same reason.
 */
export function recencyVitality(
  lastSeenAt: number,
  nowSeconds: number,
  horizonDays = 90,
): number {
  const days = ageDays(lastSeenAt, nowSeconds);
  const horizon = Math.max(MIN_VITALITY_HORIZON_DAYS, horizonDays);
  const decayed = 1 - Math.log1p(days) / Math.log1p(horizon);
  return Math.max(0, Math.min(1, decayed));
}

/** Beyond a quarter the field already calls a project dormant, and stretching
 * the luminance scale past that buys nothing: the reading is "not recently",
 * not "not recently, precisely". This is also the original fixed horizon, so a
 * registry spanning years behaves exactly as it did before. */
const MAX_VITALITY_HORIZON_DAYS = 90;

/**
 * Where brightness reaches zero on this field.
 *
 * A HIGH QUANTILE of the observed ages rather than the maximum. One registry
 * entry last seen in 2019 would otherwise set the horizon at ninety-four years
 * and collapse every other project back to indistinguishable full brightness —
 * which is the exact compression this parameter exists to remove, reintroduced
 * by a single outlier. At the ninetieth percentile the scale is set by the bulk
 * of the registry and the handful older than it simply rest at the floor,
 * which is what "dormant" should look like anyway.
 */
export function vitalityHorizon(
  projects: readonly ProjectRegistryEntry[],
  nowSeconds: number,
  quantile = 0.9,
): number {
  if (projects.length === 0) return MIN_VITALITY_HORIZON_DAYS;
  const ages = projects
    .map((project) => ageDays(project.last_seen_at, nowSeconds))
    .sort((a, b) => a - b);
  const index = Math.min(
    ages.length - 1,
    Math.max(0, Math.ceil(quantile * ages.length) - 1),
  );
  return Math.min(
    MAX_VITALITY_HORIZON_DAYS,
    Math.max(MIN_VITALITY_HORIZON_DAYS, ages[index] ?? MIN_VITALITY_HORIZON_DAYS),
  );
}

export function columnIndexFor(lastSeenAt: number, nowSeconds: number): number {
  const days = ageDays(lastSeenAt, nowSeconds);
  const index = RECENCY_COLUMNS.findIndex((column) => days < column.maxDays);
  return index === -1 ? RECENCY_COLUMNS.length - 1 : index;
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
  const horizonDays = vitalityHorizon(projects, nowSeconds);

  const columns: FieldColumn[] = RECENCY_COLUMNS.map((column) => ({
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
        degree: mass,
        vitality: recencyVitality(project.last_seen_at, nowSeconds, horizonDays),
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
      degree: group.projects.length,
      // Lit by its most recently seen checkout — the repository is exactly as
      // live as the liveliest copy of it.
      vitality: group.projects.reduce(
        (max, project) =>
          Math.max(max, recencyVitality(project.last_seen_at, nowSeconds, horizonDays)),
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

  // Margins in field units so a body's own radius has room inside the frame
  // rather than being cropped by the bezel.
  //
  // The vertical margins are DERIVED from the bodies that actually sit at the
  // two ends rather than being a flat allowance. A flat 0.55 spent a fifth of
  // the frame's height on clearance the largest body (radius 0.24) does not
  // need and the smallest (0.09) needs far less of — which read as an axis
  // that tops out empty. Horizontal margin stays fixed: it has to clear a
  // body's own radius AND its offset from its column's centre line, and both
  // ends of the x axis are column edges rather than data.
  const bodyPad = 0.08;
  const topPad = bodyRadius(massCeiling, massCeiling) + bodyPad;
  const bottomPad =
    bodyRadius(Number.isFinite(massFloor) ? massFloor : 1, massCeiling) + bodyPad;
  const xMargin = COLUMN_HALF_WIDTH + topPad;
  return {
    nodes,
    edges,
    columns,
    massCeiling,
    mass: summarizeMass(projects, axisLow, axisSpan),
    vitalityHorizonDays: horizonDays,
    sharedRepoCount,
    extent: {
      x: [-xMargin, RECENCY_COLUMNS.length - 1 + xMargin],
      y: [-bottomPad, MASS_AXIS_HEIGHT + topPad],
    },
  };
}

/**
 * What the mass axis holds, so the field's caption can say it.
 *
 * A log axis anchored to the lightest and heaviest projects present is the
 * honest scale, but on a real registry it is also a lopsided one: forty of
 * forty-four projects hold between four and thirteen units while one holds two
 * hundred and forty-seven, so the bodies bunch along the bottom and the upper
 * axis looks like a drawing error. It is not — it is the distribution, and per
 * plan 11a's degenerate rule the view states that rather than leaving the
 * reader to conclude the frame is broken.
 */
function summarizeMass(
  projects: readonly ProjectRegistryEntry[],
  axisLow: number,
  axisSpan: number,
): MassSummary {
  const masses = projects.map(indexedMass).sort((a, b) => a - b);
  if (masses.length === 0) {
    return { floor: 0, ceiling: 0, median: 0, lowerHalfCount: 0, total: 0 };
  }
  const middle = Math.floor(masses.length / 2);
  const median =
    masses.length % 2 === 0
      ? ((masses[middle - 1] ?? 0) + (masses[middle] ?? 0)) / 2
      : (masses[middle] ?? 0);
  const lowerHalfCount = masses.filter(
    (mass) => (Math.log1p(mass) - axisLow) / axisSpan < 0.5,
  ).length;
  return {
    floor: masses[0] ?? 0,
    ceiling: masses[masses.length - 1] ?? 0,
    median,
    lowerHalfCount,
    total: masses.length,
  };
}

/**
 * Smallest horizontal offset from a column's centre line that clears every body
 * already settled in that column, searched outward in alternating directions so
 * a column fills symmetrically. Never leaves the column's own half-width: an
 * offset within a column costs nothing, an offset across one would be a lie
 * about when the project was last seen.
 *
 * A real registry does exhaust that width — thirty-odd projects all seen in the
 * same week, holding much the same amount, want the same point. When no offset
 * clears, the body takes the one that leaves the most room rather than the
 * centre line, so a crowded column reads as a dense band with structure in it
 * instead of a pile on its axis. Bodies there genuinely do overlap, which is
 * the truth: those projects are in the same place because they measure the
 * same.
 */
function clearOffset(
  y: number,
  radius: number,
  settled: ReadonlyArray<{ offset: number; y: number; radius: number }>,
): number {
  const steps = Math.floor(COLUMN_HALF_WIDTH / NUDGE);
  let roomiest = 0;
  let mostRoom = -Infinity;
  for (let step = 0; step <= steps; step += 1) {
    for (const direction of step === 0 ? [0] : [1, -1]) {
      const offset = direction * step * NUDGE;
      let room = Infinity;
      for (const point of settled) {
        room = Math.min(
          room,
          Math.hypot(point.offset - offset, point.y - y) - (point.radius + radius),
        );
      }
      if (room >= 0) return offset;
      if (room > mostRoom) {
        mostRoom = room;
        roomiest = offset;
      }
    }
  }
  return roomiest;
}

/** One of the three counts a registry entry carries, summarised across the
 * whole registry. */
export interface HoldingChannel {
  label: string;
  /** The value every project shares, when they all share one. */
  uniform: number | null;
  /** The most common value, when they do not. */
  mode: number;
  min: number;
  max: number;
}

export interface HoldingsSummary {
  total: number;
  stores: HoldingChannel;
  scopes: HoldingChannel;
  artifacts: HoldingChannel;
  /** The channels that carry no information because every project agrees, as a
   * sentence — or null when they all vary. */
  uniformLine: string | null;
}

function channel(label: string, values: readonly number[]): HoldingChannel {
  if (values.length === 0) {
    return { label, uniform: null, mode: 0, min: 0, max: 0 };
  }
  const counts = new Map<number, number>();
  for (const value of values) counts.set(value, (counts.get(value) ?? 0) + 1);
  const mode = [...counts.entries()].sort((a, b) => b[1] - a[1] || a[0] - b[0])[0]![0];
  const min = Math.min(...values);
  const max = Math.max(...values);
  return { label, uniform: min === max ? min : null, mode, min, max };
}

/**
 * What the registry rail's per-project counts are actually worth.
 *
 * Every row printed "1 st · 4 art", and on a real registry that is very nearly
 * literal: `store_count` is 1 for all forty-four projects and `artifact_count`
 * is 3, 4 or 5. Forty-four repetitions of a constant is not density. The one
 * count that genuinely varies is `graph_scope_count`, which spans 0 to 242 and
 * was not on the row at all.
 *
 * So the constant channels are stated once for the whole rail and the rows
 * carry the channel that differs — plus, per row, any other channel that
 * departs from its mode, because a project holding five artifacts where
 * everything else holds four IS a reading and must not be swallowed by the
 * summary.
 */
export function summarizeHoldings(
  projects: readonly ProjectRegistryEntry[],
): HoldingsSummary | null {
  if (projects.length === 0) return null;
  const stores = channel('store', projects.map((project) => project.store_count));
  const scopes = channel('scope', projects.map((project) => project.graph_scope_count));
  const artifacts = channel('artifact', projects.map((project) => project.artifact_count));

  const parts: string[] = [];
  for (const entry of [stores, scopes, artifacts]) {
    if (entry.uniform != null) {
      parts.push(
        `exactly ${entry.uniform} ${entry.label}${entry.uniform === 1 ? '' : 's'}`,
      );
    } else if (entry.max - entry.min <= 2) {
      parts.push(`${entry.min}–${entry.max} ${entry.label}s`);
    }
  }
  return {
    total: projects.length,
    stores,
    scopes,
    artifacts,
    uniformLine:
      parts.length > 0
        ? `Every project here holds ${joinClauses(parts)}.`
        : null,
  };
}

function joinClauses(parts: readonly string[]): string {
  if (parts.length === 1) return parts[0]!;
  return `${parts.slice(0, -1).join(', ')} and ${parts[parts.length - 1]}`;
}
