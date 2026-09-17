import { describe, expect, it } from 'vitest';
import { labelBudget, selectLabels, type LabelCandidate } from './labelPriority.ts';

const VIEWPORT = { width: 600, height: 300 };

function candidate(
  id: string,
  priority: number,
  px: number,
  py: number,
  forced = false,
  more: ReadonlyArray<{ px: number; py: number }> = [],
): LabelCandidate {
  return { id, priority, placements: [{ px, py }, ...more], width: 80, height: 14, forced };
}

describe('label priority', () => {
  it('prints the heaviest bodies first and never over one another', () => {
    const chosen = selectLabels(
      [
        candidate('light', 5, 100, 100),
        candidate('heavy', 300, 104, 104),
        candidate('far', 50, 400, 200),
      ],
      VIEWPORT,
      10,
    );
    expect(chosen.has('heavy')).toBe(true);
    expect(chosen.has('light')).toBe(false);
    expect(chosen.has('far')).toBe(true);
    expect(chosen.get('heavy')).toEqual({ px: 104, py: 104, placement: 0 });
  });

  it('spends the budget only on names that print whole inside the viewport', () => {
    const chosen = selectLabels(
      [candidate('offscreen', 999, -500, -500), candidate('clipped', 500, 560, 10), candidate('visible', 1, 10, 10)],
      VIEWPORT,
      1,
    );
    expect([...chosen.keys()]).toEqual(['visible']);
  });

  it('always prints a forced label past the budget, and lets it claim its space first', () => {
    const chosen = selectLabels(
      [
        candidate('a', 100, 10, 10),
        candidate('b', 90, 200, 10),
        candidate('focused', 1, 12, 12, true),
      ],
      VIEWPORT,
      1,
    );
    expect(chosen.has('focused')).toBe(true);
    // The forced label took the top-left slot, so `a` collides and `b` is
    // the one ordinary slot the budget allows.
    expect(chosen.has('a')).toBe(false);
    expect(chosen.has('b')).toBe(true);
  });

  it('falls back to the next placement when the preferred one is taken or clipped', () => {
    const chosen = selectLabels(
      [
        candidate('hub', 1000, 300, 100, true),
        // Prefers the same spot as the hub, then offers below it.
        candidate('member', 1, 302, 102, true, [{ px: 302, py: 140 }]),
        // Prefers a clipped spot at the right edge, then flips left.
        candidate('edge', 2, 560, 200, false, [{ px: 470, py: 200 }]),
      ],
      VIEWPORT,
      5,
    );
    expect(chosen.get('member')).toEqual({ px: 302, py: 140, placement: 1 });
    expect(chosen.get('edge')).toEqual({ px: 470, py: 200, placement: 1 });
  });

  it('is deterministic under ties', () => {
    const tied = [candidate('z', 1, 10, 10), candidate('a', 1, 300, 10)];
    expect([...selectLabels(tied, VIEWPORT, 1).keys()]).toEqual(['a']);
    expect([...selectLabels([...tied].reverse(), VIEWPORT, 1).keys()]).toEqual(['a']);
  });

  it('earns more labels as the camera closes in, bounded by the aperture', () => {
    expect(labelBudget(1, 40)).toBe(12);
    expect(labelBudget(1.6, 40)).toBe(24);
    expect(labelBudget(3, 40)).toBe(40);
    expect(labelBudget(1, 4)).toBe(6);
    expect(labelBudget(3, 40, { width: 320, height: 200 })).toBe(2);
    expect(labelBudget(1, 40, { width: 1400, height: 800 })).toBe(12);
  });
});
