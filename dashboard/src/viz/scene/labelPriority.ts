/**
 * Which bodies get a printed name at the current camera, and where.
 *
 * Semantic zoom is an information contract: the further in the camera is, the
 * more of the registry is named, and a name is never allowed to print over
 * another. Priority is the caller's (mass, for the registry); `forced` labels
 * — the members of a focused repository, a hovered body — always win a slot
 * and do not spend the budget. Each candidate offers its placements in order
 * of preference (beside the crown, then the other side, then below, above);
 * the first that fits whole inside the aperture and clear of every accepted
 * name is the one printed. Deterministic: same inputs, same result.
 */

export interface LabelPlacement {
  /** Top-left of the label box in CSS pixels. */
  readonly px: number;
  readonly py: number;
}

export interface LabelCandidate {
  readonly id: string;
  /** Higher prints first. */
  readonly priority: number;
  /** Placements in order of preference; at least one. */
  readonly placements: readonly LabelPlacement[];
  readonly width: number;
  readonly height: number;
  readonly forced?: boolean;
}

export interface PlacedLabel extends LabelPlacement {
  /** Index into the candidate's `placements`, so a host can style the side. */
  readonly placement: number;
}

interface Rect {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
}

function intersects(a: Rect, b: Rect, gap: number): boolean {
  return a.x0 < b.x1 + gap && a.x1 + gap > b.x0 && a.y0 < b.y1 + gap && a.y1 + gap > b.y0;
}

export function selectLabels(
  candidates: readonly LabelCandidate[],
  viewport: { readonly width: number; readonly height: number },
  budget: number,
  gap = 4,
): ReadonlyMap<string, PlacedLabel> {
  const ordered = [...candidates].sort(
    (a, b) =>
      Number(Boolean(b.forced)) - Number(Boolean(a.forced))
      || b.priority - a.priority
      || a.id.localeCompare(b.id),
  );
  const accepted: Rect[] = [];
  const chosen = new Map<string, PlacedLabel>();
  // Forced labels do not spend the budget: it bounds the ordinary names the
  // camera earns, and a focused or emphasised body is printed regardless.
  let ordinary = 0;
  for (const candidate of ordered) {
    if (!candidate.forced && ordinary >= budget) break;
    for (let index = 0; index < candidate.placements.length; index += 1) {
      const at = candidate.placements[index]!;
      const rect: Rect = {
        x0: at.px,
        y0: at.py,
        x1: at.px + candidate.width,
        y1: at.py + candidate.height,
      };
      // A name has to fit whole: a label cut by the aperture edge is a label
      // the reader cannot read, so the next placement is tried instead.
      if (rect.x0 < 0 || rect.y0 < 0 || rect.x1 > viewport.width || rect.y1 > viewport.height) continue;
      if (accepted.some((other) => intersects(rect, other, gap))) continue;
      accepted.push(rect);
      chosen.set(candidate.id, { px: at.px, py: at.py, placement: index });
      if (!candidate.forced) ordinary += 1;
      break;
    }
  }
  return chosen;
}

/** Label slots the camera earns: a handful at Fit, every body once the reader
 * has closed in, and never more than the aperture has room to print. `zoom`
 * is relative to Fit (1 = whole field). */
export function labelBudget(
  zoom: number,
  bodyCount: number,
  viewport?: { readonly width: number; readonly height: number },
): number {
  const earned =
    zoom >= 2.4
      ? bodyCount
      : zoom >= 1.5
        ? Math.max(12, Math.ceil(bodyCount * 0.6))
        : Math.max(6, Math.ceil(bodyCount * 0.3));
  if (!viewport) return earned;
  const room = Math.max(1, Math.floor((viewport.width * viewport.height) / 25_000));
  return Math.min(earned, room);
}
