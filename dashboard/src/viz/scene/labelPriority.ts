/**
 * Which bodies get a printed name at the current camera.
 *
 * Semantic zoom is an information contract: the further in the camera is, the
 * more of the registry is named, and a name is never allowed to print over
 * another. Priority is the caller's (mass, for the registry); `forced` labels
 * — the members of a focused repository, a hovered body — always win a slot.
 * Bodies outside the viewport are skipped so the budget is spent on what is
 * visible. Deterministic: same inputs, same set.
 */

export interface LabelCandidate {
  readonly id: string;
  /** Higher prints first. */
  readonly priority: number;
  /** Anchor in CSS pixels (the label box grows right/down from here). */
  readonly px: number;
  readonly py: number;
  readonly width: number;
  readonly height: number;
  readonly forced?: boolean;
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
): ReadonlySet<string> {
  const ordered = [...candidates].sort(
    (a, b) =>
      Number(Boolean(b.forced)) - Number(Boolean(a.forced))
      || b.priority - a.priority
      || a.id.localeCompare(b.id),
  );
  const accepted: Rect[] = [];
  const chosen = new Set<string>();
  for (const candidate of ordered) {
    if (!candidate.forced && chosen.size >= budget) break;
    const rect: Rect = {
      x0: candidate.px,
      y0: candidate.py,
      x1: candidate.px + candidate.width,
      y1: candidate.py + candidate.height,
    };
    const offscreen =
      rect.x1 < 0 || rect.y1 < 0 || rect.x0 > viewport.width || rect.y0 > viewport.height;
    if (offscreen) continue;
    if (accepted.some((other) => intersects(rect, other, gap))) continue;
    accepted.push(rect);
    chosen.add(candidate.id);
  }
  return chosen;
}

/** Label slots the camera earns: a handful at Fit, every body once the reader
 * has closed in. `zoom` is relative to Fit (1 = whole field). */
export function labelBudget(zoom: number, bodyCount: number): number {
  if (zoom >= 2.4) return bodyCount;
  if (zoom >= 1.5) return Math.max(12, Math.ceil(bodyCount * 0.6));
  return Math.max(6, Math.ceil(bodyCount * 0.3));
}
