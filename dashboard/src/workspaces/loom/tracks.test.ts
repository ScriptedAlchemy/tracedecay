import { describe, expect, it } from 'vitest';
import { axisTicks, bandScale, clampWindow, fittedWindow, isFitted, packTrack, tickStepFor, zoomWindow, type LoomSpan } from './tracks.ts';

const HOUR = 3600;
const DAY = 86_400;

function span(id: string, start: number, end: number, weight = 10): LoomSpan {
  return { id, start, end, label: id, weight };
}

describe('packTrack', () => {
  it('keeps non-overlapping spans in a single lane', () => {
    const lanes = packTrack([span('a', 0, 10), span('b', 20, 30), span('c', 40, 50)]);
    expect(lanes).toHaveLength(1);
    expect(lanes[0]).toHaveLength(3);
  });

  it('stacks overlapping spans into separate lanes instead of drawing mud', () => {
    const lanes = packTrack([span('a', 0, 100), span('b', 10, 110), span('c', 20, 120)]);
    expect(lanes).toHaveLength(3);
  });

  it('treats a pixel-sized gap as collision so touching marks stay separated', () => {
    expect(packTrack([span('a', 0, 10), span('b', 12, 20)], 0)).toHaveLength(1);
    expect(packTrack([span('a', 0, 10), span('b', 12, 20)], 5)).toHaveLength(2);
  });

  it('reuses a lane once its previous span has ended', () => {
    const lanes = packTrack([span('a', 0, 100), span('b', 10, 20), span('c', 30, 40)]);
    expect(lanes).toHaveLength(2);
    expect(lanes[1]?.map((s) => s.id)).toEqual(['b', 'c']);
  });
});

describe('bandScale', () => {
  it('walks hour -> day -> month as the window widens', () => {
    expect(bandScale(6 * HOUR)).toBe('hour');
    expect(bandScale(5 * DAY)).toBe('day');
    expect(bandScale(200 * DAY)).toBe('month');
  });

  it('drops the fine tick step below the calendar band where it can afford to', () => {
    for (const seconds of [6 * HOUR, 5 * DAY, 200 * DAY]) {
      const ceiling =
        bandScale(seconds) === 'hour' ? HOUR : bandScale(seconds) === 'day' ? DAY : 30 * DAY;
      expect(tickStepFor(seconds, 900)).toBeLessThan(ceiling);
    }
  });

  it('keeps the coarse step when dropping under the band would crowd the axis', () => {
    // Two years of history: every rung below the month band prints ticks
    // tighter than the axis can label, so the fine row stays coarse.
    expect(900 * DAY / tickStepFor(900 * DAY, 900)).toBeLessThanOrEqual(20);
    expect(60 * DAY / tickStepFor(60 * DAY, 900)).toBeLessThanOrEqual(20);
  });
});

describe('axis', () => {
  it('picks a finer step for a short window than a long one', () => {
    expect(tickStepFor(2 * HOUR, 800)).toBeLessThan(tickStepFor(90 * DAY, 800));
  });

  it('keeps the tick count within a readable band across wild spans', () => {
    for (const seconds of [600, 6 * HOUR, 5 * DAY, 120 * DAY, 900 * DAY]) {
      const ticks = axisTicks({ start: 1_700_000_000, end: 1_700_000_000 + seconds }, 900);
      expect(ticks.length).toBeGreaterThan(1);
      expect(ticks.length).toBeLessThanOrEqual(20);
    }
  });
});

describe('viewport', () => {
  const extent = { start: 0, end: 10 * DAY };

  it('fits with a margin on both sides', () => {
    const fitted = fittedWindow(extent);
    expect(fitted.start).toBeLessThan(extent.start);
    expect(fitted.end).toBeGreaterThan(extent.end);
    expect(isFitted(fitted, extent)).toBe(true);
  });

  it('zooms around the focus point and never past the floor', () => {
    const view = fittedWindow(extent);
    const zoomed = zoomWindow(view, extent, 0.5, 5 * DAY);
    expect(zoomed.end - zoomed.start).toBeCloseTo((view.end - view.start) / 2, 3);
    let deep = view;
    for (let i = 0; i < 40; i += 1) deep = zoomWindow(deep, extent, 0.5, 5 * DAY);
    expect(deep.end - deep.start).toBeGreaterThanOrEqual(60);
  });

  it('refuses to strand the viewport in empty time', () => {
    const view = clampWindow({ start: 500 * DAY, end: 501 * DAY }, extent);
    expect(view.start).toBeLessThan(extent.end + DAY);
  });
});
