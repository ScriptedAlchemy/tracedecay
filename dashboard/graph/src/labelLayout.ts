/**
 * Pure screen-space label selection for the graph canvas.
 *
 * Labels are chosen by priority (hovered/selected first, then path, then
 * hover-neighborhood/focus, then everything else by degree) and placed
 * greedily: a label is only drawn if its screen rect does not overlap any
 * already-placed label and the area-based cap has room. Sticky boxes
 * (hovered/selected) always render and never count against the cap, so a
 * hover always reveals that node's label.
 */

export interface LabelBox {
  id: string;
  /** Lower renders first: 0 hovered, 1 selected, 2 path, 3 highlight/focus, 4 rest. */
  priority: number;
  /** Full-graph degree; tie-break inside a priority band (hubs win). */
  degree: number;
  left: number;
  top: number;
  right: number;
  bottom: number;
  /** Always shown, exempt from the cap and from collision rejection. */
  sticky?: boolean;
}

/** Padding (px) inflating each placed rect during overlap tests. */
const LABEL_PAD = 3;

/**
 * How many labels fit a viewport: one per ~30k px² with a floor of 6, so a
 * desktop canvas allows a few dozen and a narrow panel only a handful.
 */
export function labelCapForArea(width: number, height: number): number {
  return Math.max(6, Math.floor((width * height) / 30_000));
}

function overlaps(a: LabelBox, b: LabelBox): boolean {
  return (
    a.left - LABEL_PAD < b.right + LABEL_PAD &&
    a.right + LABEL_PAD > b.left - LABEL_PAD &&
    a.top - LABEL_PAD < b.bottom + LABEL_PAD &&
    a.bottom + LABEL_PAD > b.top - LABEL_PAD
  );
}

/** Returns the ids whose labels should render, in placement order. */
export function selectLabels(boxes: LabelBox[], cap: number): string[] {
  const ordered = [...boxes].sort(
    (a, b) => a.priority - b.priority || b.degree - a.degree || a.id.localeCompare(b.id),
  );
  const placed: LabelBox[] = [];
  const chosen: string[] = [];
  let capped = 0;
  for (const box of ordered) {
    if (box.sticky) {
      placed.push(box);
      chosen.push(box.id);
      continue;
    }
    if (capped >= cap) continue;
    if (placed.some((other) => overlaps(box, other))) continue;
    placed.push(box);
    chosen.push(box.id);
    capped++;
  }
  return chosen;
}
