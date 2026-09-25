import { describe, expect, it } from 'vitest';
import { axisTicks, bandScale, clampWindow, fittedWindow, isFitted, tickStepFor, zoomWindow } from './tracks.ts';

const HOUR = 3600;
const DAY = 86_400;

describe('bandScale', () => {
  it('walks hour -> day -> month as the window widens', () => {
    expect(bandScale(6 * HOUR)).toBe('hour');
    expect(bandScale(5 * DAY)).toBe('day');
    expect(bandScale(200 * DAY)).toBe('month');
  });

  it('drops the fine tick step below the calendar band where it can afford to', () => {
    expect(tickStepFor(6 * HOUR, 900)).toBe(30 * 60);
    expect(tickStepFor(5 * DAY, 1800)).toBe(12 * HOUR);
    expect(tickStepFor(200 * DAY, 900)).toBe(14 * DAY);
  });

  it('never picks a step whose label is wider than its pitch', () => {
    // A 7-day window at 1196px: 12-hour ticks would sit 85px apart under
    // "Dec 28, 12:58 PM"-wide labels, so the ruler ticks daily instead.
    const start = 1_700_000_000;
    expect(tickStepFor(7 * DAY, 1196)).toBe(DAY);
    const ticks = axisTicks({ start, end: start + 7 * DAY }, 1196);
    expect(ticks).toHaveLength(7);
    expect(ticks[1]!.x - ticks[0]!.x).toBeCloseTo(1196 / 7, 6);
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
