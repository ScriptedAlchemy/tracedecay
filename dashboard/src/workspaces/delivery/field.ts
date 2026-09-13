/**
 * The delivery field: how the registry's REPOSITORIES are composed, as a pure
 * function of the `/api/projects` payload.
 *
 * The problem this replaces. Delivery was a flat two-tier list — a repository
 * header above one checkout row, forty-four times. That layout spends a third
 * of the column on repeated chrome and encodes nothing: scrolling it tells you
 * the registry is long, and nothing else. The one question a delivery surface
 * exists to answer at a glance — where is work actually happening — was not
 * asked anywhere on the page.
 *
 * What replaces it. The same measured-field grammar the Brain uses, over a
 * different measurement, because Delivery asks a different question of the same
 * registry:
 *
 *   x — when TraceDecay last indexed the repository, as the shared ordered
 *       recency columns (see `RECENCY_COLUMNS`). An offset inside a column is
 *       packing and costs nothing; an offset across one would be a lie.
 *
 *   y — BRANCH COUNT, log scale, because the registry spans one branch to two
 *       hundred and forty-two and a linear axis would put everything except
 *       the largest repository on the floor.
 *
 *   size — checkouts (primary + worktrees), so a repository worked in several
 *       places at once is a bigger body than one with a single clone.
 *   brightness — recency again, so the live columns burn and the dormant ones
 *       sink toward the substrate.
 *
 * The honesty point that shapes the whole module: a registry entry with no
 * `git_common_dir` is not a repository with zero branches, it is something that
 * is not a git checkout at all — TraceDecay indexes plain directories too. Its
 * branch count is UNKNOWN, not zero, so it gets `y: null` and the renderer puts
 * it in a separate declared band instead of stacking it on the axis floor with
 * genuine single-branch repositories. Eight of the forty-four entries on the
 * real profile are in this class; collapsing them onto zero would invent a
 * measurement for a fifth of the field.
 *
 * `last_seen_at` is when TraceDecay last INDEXED the checkout. It is not the
 * last commit time; the active checkout's bounded commit history is a separate
 * Delivery projection, and every caption that prints this axis has to say so.
 */
import {
  RECENCY_COLUMNS,
  columnIndexFor,
  recencyVitality,
} from '../brain/field.ts';
import {
  type ProjectRepoGroup,
} from '../../contracts/generated.ts';

export interface DeliveryBody {
  id: string;
  label: string;
  /** Branch count, or null when this entry is not a git checkout. */
  branches: number | null;
  /** Checkouts mapped to this repository. */
  checkouts: number;
  /** How many of those are worktrees rather than the primary clone. */
  worktrees: number;
  /** Latest index time across the repository's checkouts. */
  lastSeenAt: number;
  /** Recency as luminance, 0..1. */
  vitality: number;
  /** Recency column index. */
  column: number;
  /** Offset from the column's centre line, in column widths (−0.5..0.5). */
  offset: number;
  /** Branch axis position, 0 (fewest) .. 1 (most). Null when unknown. */
  y: number | null;
  /** Body radius as a 0..1 fraction of the largest, by checkout count. */
  size: number;
  active: boolean;
  defaultBranch: string | null;
}

export interface DeliveryColumn {
  id: string;
  label: string;
  bound: string;
  count: number;
}

export interface DeliveryField {
  bodies: DeliveryBody[];
  columns: DeliveryColumn[];
  /** Most branches on any one repository — the ceiling the y axis is scaled
   * against, printed so the axis can be read rather than guessed at. */
  branchCeiling: number;
  /** Fewest branches on any git repository present. */
  branchFloor: number;
  /** Entries whose branch count is unknown because they are not git checkouts. */
  unknownBranchCount: number;
  /** Repositories with more than one checkout. Zero on the real profile, which
   * is itself worth printing. */
  multiCheckoutCount: number;
  totalBranches: number;
  totalCheckouts: number;
  totalWorktrees: number;
}

/** Half the gap between columns: a body may move this far off its centre line
 * and still be unambiguously a member of its own column. */
const COLUMN_HALF_WIDTH = 0.4;
const NUDGE = 0.075;
/** Clearance in field units, where the y axis is 0..1 and columns are 1 apart.
 * Bodies are drawn small, so this is generous relative to their radius. */
const CLEARANCE = 0.055;

/** Branch count for a group, or null when it is not a git checkout.
 *
 * The group's own `branches` is the union across its checkouts, which is the
 * number this field wants: a repository's branches, not a working copy's view
 * of them. */
export function branchCountOf(group: ProjectRepoGroup): number | null {
  if (group.git_common_dir == null || group.git_common_dir === '') return null;
  return group.branches.length;
}

/** Latest index time across a repository's checkouts. A repository is exactly
 * as recently seen as its liveliest working copy. */
export function latestSeen(group: ProjectRepoGroup): number {
  return group.projects.reduce(
    (max, project) => Math.max(max, project.last_seen_at),
    0,
  );
}

/**
 * Compose the registry into the delivery field. Deterministic: the same payload
 * and the same clock always produce the same coordinates, so a screenshot is
 * stable and a refetch never reshuffles the picture under the reader.
 */
export function composeDeliveryField(
  groups: readonly ProjectRepoGroup[],
  nowSeconds: number = Date.now() / 1000,
): DeliveryField {
  const measured = groups
    .map((group) => branchCountOf(group))
    .filter((count): count is number => count != null);
  const branchCeiling = measured.reduce((max, count) => Math.max(max, count), 0);
  const branchFloor = measured.reduce(
    (min, count) => Math.min(min, count),
    Number.POSITIVE_INFINITY,
  );
  // The axis runs between the smallest and largest repository actually present.
  // Anchored at zero it would spend its lower half on a branch count no git
  // repository in this registry has.
  const axisLow = Math.log1p(Number.isFinite(branchFloor) ? branchFloor : 0);
  const axisSpan = Math.max(Math.log1p(branchCeiling) - axisLow, 0.001);

  const checkoutCeiling = groups.reduce(
    (max, group) => Math.max(max, group.projects.length),
    0,
  );

  const columns: DeliveryColumn[] = RECENCY_COLUMNS.map((column) => ({
    id: column.id,
    label: column.label,
    bound: column.bound,
    count: 0,
  }));

  // Bucket first so the anti-overlap pass only compares bodies that could
  // actually collide, and so its result cannot depend on the order the daemon
  // happened to return repositories in.
  const byColumn = new Map<number, ProjectRepoGroup[]>();
  for (const group of groups) {
    const index = columnIndexFor(latestSeen(group), nowSeconds);
    columns[index]!.count += 1;
    const bucket = byColumn.get(index);
    if (bucket) bucket.push(group);
    else byColumn.set(index, [group]);
  }

  const bodies: DeliveryBody[] = [];
  for (const [index, bucket] of [...byColumn.entries()].sort((a, b) => a[0] - b[0])) {
    // Most branches first, then by id: the busiest repository gets the
    // uncontested centre line, and the id tiebreak keeps the result
    // independent of payload order.
    const ordered = [...bucket].sort((a, b) => {
      const delta = (branchCountOf(b) ?? -1) - (branchCountOf(a) ?? -1);
      return delta !== 0 ? delta : idOf(a).localeCompare(idOf(b));
    });
    const settled: Array<{ offset: number; y: number }> = [];
    for (const group of ordered) {
      const branches = branchCountOf(group);
      const y =
        branches == null ? null : (Math.log1p(branches) - axisLow) / axisSpan;
      // Unknown-branch bodies live in their own band, so they only have to
      // clear each other; they are given a nominal y for that purpose only and
      // it never reaches the caller.
      const packY = y ?? -0.25;
      const offset = clearOffset(packY, settled);
      settled.push({ offset, y: packY });

      const lastSeenAt = latestSeen(group);
      const worktrees = group.projects.filter(
        (project) => project.kind === 'worktree',
      ).length;
      bodies.push({
        id: idOf(group),
        label: group.label,
        branches,
        checkouts: group.projects.length,
        worktrees,
        lastSeenAt,
        vitality: recencyVitality(lastSeenAt, nowSeconds),
        column: index,
        offset,
        y,
        size:
          checkoutCeiling > 0
            ? Math.sqrt(group.projects.length / checkoutCeiling)
            : 0,
        active: group.projects.some((project) => project.is_active === true),
        defaultBranch:
          group.projects.find((project) => project.default_branch != null)
            ?.default_branch ?? null,
      });
    }
  }

  // Most recently indexed first: the reading a delivery surface is opened for.
  bodies.sort(
    (a, b) => b.lastSeenAt - a.lastSeenAt || a.label.localeCompare(b.label),
  );

  return {
    bodies,
    columns,
    branchCeiling,
    branchFloor: Number.isFinite(branchFloor) ? branchFloor : 0,
    unknownBranchCount: bodies.filter((body) => body.branches == null).length,
    multiCheckoutCount: bodies.filter((body) => body.checkouts > 1).length,
    totalBranches: measured.reduce((sum, count) => sum + count, 0),
    totalCheckouts: bodies.reduce((sum, body) => sum + body.checkouts, 0),
    totalWorktrees: bodies.reduce((sum, body) => sum + body.worktrees, 0),
  };
}

function idOf(group: ProjectRepoGroup): string {
  return group.git_common_dir ?? group.label;
}

/**
 * Smallest offset from a column's centre line that clears every body already
 * settled there, searched outward in alternating directions so a column fills
 * symmetrically. Never leaves the column's own half-width.
 *
 * When nothing clears — and a real registry does exhaust the width, since three
 * dozen repositories indexed in the same week want the same point — the body
 * takes the offset with the most room rather than the centre line, so a crowded
 * column reads as a dense band with structure in it rather than a pile on its
 * axis. Bodies that genuinely measure the same genuinely overlap.
 */
function clearOffset(
  y: number,
  settled: ReadonlyArray<{ offset: number; y: number }>,
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
          Math.hypot(point.offset - offset, point.y - y) - CLEARANCE,
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
