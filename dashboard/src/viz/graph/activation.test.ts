import { describe, expect, it, vi } from 'vitest';
import { ActivationField, lerpRgbTuple, luma, restingNodeTint } from './activation.ts';

describe('ActivationField subscription', () => {
  it('notifies once per strike so an outside striker can wake a renderer', () => {
    const field = new ActivationField();
    const listener = vi.fn();
    field.subscribe(listener);
    field.strike(['a', 'b'], 0.5);
    expect(listener).toHaveBeenCalledTimes(1);
    field.strike(['a'], 0.5);
    expect(listener).toHaveBeenCalledTimes(2);
  });

  it('stays silent when a strike carries no ids — nothing real happened', () => {
    const field = new ActivationField();
    const listener = vi.fn();
    field.subscribe(listener);
    field.strike([], 1);
    expect(listener).not.toHaveBeenCalled();
  });

  it('never fires on its own: decay is not an event', () => {
    // The field has no clock. `tick` is decay bookkeeping driven by whoever is
    // already drawing; if it notified, a renderer would wake itself forever.
    const field = new ActivationField({ halfLifeMs: 100 });
    field.strike(['a'], 1);
    const listener = vi.fn();
    field.subscribe(listener);
    field.tick(0);
    field.tick(1_000);
    field.tick(2_000);
    expect(listener).not.toHaveBeenCalled();
    expect(field.warm).toBe(false);
  });

  it('stops notifying once unsubscribed', () => {
    const field = new ActivationField();
    const listener = vi.fn();
    field.subscribe(listener)();
    field.strike(['a'], 1);
    expect(listener).not.toHaveBeenCalled();
  });
});

describe('luma', () => {
  it('reads 0 for black and 255 for white', () => {
    expect(luma([0, 0, 0])).toBe(0);
    expect(luma([255, 255, 255])).toBe(255);
  });
});

// Approximate rendered RGB of the two themes' `--raw-surface-1` (the
// substrate a resting node fades toward) and a representative mid-tone kind
// hue at each theme's pinned lightness -- close enough to the real tokens to
// exercise the same headroom the renderer actually has, without depending on
// oklch->rgb conversion in a unit test.
const DARK_SUBSTRATE: [number, number, number] = [28, 30, 36];
const LIGHT_SUBSTRATE: [number, number, number] = [245, 246, 248];
const DARK_KIND: [number, number, number] = [110, 205, 215];
// A worst-case light-theme kind hue: still legitimately "a colour", but its
// luma sits close enough to the near-white substrate that the un-nudged 0.34
// floor mix used to land within a handful of luma units of the paper -- the
// defect this function exists to close.
const LIGHT_KIND: [number, number, number] = [150, 170, 175];

describe('restingNodeTint', () => {
  it('leaves the dark theme unchanged: headroom already clears the floor', () => {
    for (const vitality of [0, 0.25, 0.6, 1]) {
      const mix = 0.34 + 0.66 * vitality;
      const raw = lerpRgbTuple(DARK_SUBSTRATE, DARK_KIND, mix);
      expect(restingNodeTint(DARK_SUBSTRATE, DARK_KIND, vitality, false)).toEqual(raw);
    }
  });

  it('keeps a fully-dormant light-theme node from washing into the substrate', () => {
    const tint = restingNodeTint(LIGHT_SUBSTRATE, LIGHT_KIND, 0, true);
    const rawMix = lerpRgbTuple(LIGHT_SUBSTRATE, LIGHT_KIND, 0.34);
    const rawOffset = luma(LIGHT_SUBSTRATE) - luma(rawMix);
    const nudgedOffset = luma(LIGHT_SUBSTRATE) - luma(tint);
    // The un-nudged 0.34 mix of these fixtures clears well under the floor;
    // the nudge must close nearly all of that gap (integer-channel rounding
    // accounts for the last fraction of a luma unit).
    expect(rawOffset).toBeLessThan(30);
    expect(nudgedOffset).toBeGreaterThanOrEqual(41);
    expect(nudgedOffset).toBeGreaterThan(rawOffset);
  });

  it('nudges the light theme darker, never lighter than the substrate', () => {
    const tint = restingNodeTint(LIGHT_SUBSTRATE, LIGHT_KIND, 0, true);
    expect(luma(tint)).toBeLessThan(luma(LIGHT_SUBSTRATE));
  });

  it('a live node needs no nudge: full vitality already clears the offset', () => {
    const mix = 0.34 + 0.66 * 1;
    const raw = lerpRgbTuple(LIGHT_SUBSTRATE, LIGHT_KIND, mix);
    expect(restingNodeTint(LIGHT_SUBSTRATE, LIGHT_KIND, 1, true)).toEqual(raw);
  });
});
