import { describe, expect, it, vi } from 'vitest';
import { ActivationField, luma, restingNodeTint } from './activation.ts';

describe('ActivationField subscription', () => {
  it('notifies on a strike that lands heat, and stays silent on one that carries no ids', () => {
    const field = new ActivationField();
    const listener = vi.fn();
    field.subscribe(listener);
    field.strike([], 1);
    expect(listener).toHaveBeenCalledTimes(0);
    expect(field.warm).toBe(false);

    field.strike(['a'], 0.5);
    expect(listener).toHaveBeenCalledTimes(1);
    expect(field.warm).toBe(true);
    expect(field.heatOf('a')).toBe(0.5);
  });

  it('never fires on its own: decay is not an event', () => {
    // The field has no clock. `tick` is decay bookkeeping driven by whoever is
    // already drawing; if it notified, a renderer would wake itself forever.
    const field = new ActivationField({ halfLifeMs: 100 });
    const listener = vi.fn();
    field.subscribe(listener);
    field.strike(['a'], 1);
    expect(listener).toHaveBeenCalledTimes(1);
    expect(field.tick(1_000)).toBe(true);
    expect(field.tick(2_000)).toBe(false);
    expect(listener).toHaveBeenCalledTimes(1);
    expect(field.warm).toBe(false);
  });

  it('stops notifying once unsubscribed, while strikes still land', () => {
    const field = new ActivationField();
    const listener = vi.fn();
    const unsubscribe = field.subscribe(listener);
    field.strike(['a'], 1);
    expect(listener).toHaveBeenCalledTimes(1);

    unsubscribe();
    field.strike(['b'], 1);
    expect(listener).toHaveBeenCalledTimes(1);
    expect(field.heatOf('b')).toBe(1);
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
  it('leaves the dark theme on the plain substrate-to-kind mix: headroom already clears the floor', () => {
    expect(restingNodeTint(DARK_SUBSTRATE, DARK_KIND, 0, false)).toEqual([56, 90, 97]);
    expect(restingNodeTint(DARK_SUBSTRATE, DARK_KIND, 0.25, false)).toEqual([69, 118, 126]);
    expect(restingNodeTint(DARK_SUBSTRATE, DARK_KIND, 0.6, false)).toEqual([88, 159, 168]);
    expect(restingNodeTint(DARK_SUBSTRATE, DARK_KIND, 1, false)).toEqual([110, 205, 215]);
  });

  it('keeps a fully-dormant light-theme node from washing into the substrate', () => {
    // The un-nudged 0.34 mix of these fixtures is [213, 220, 223], about 28
    // luma units under the paper; the nudge darkens it to clear the floor, up
    // to integer-channel rounding.
    const tint = restingNodeTint(LIGHT_SUBSTRATE, LIGHT_KIND, 0, true);
    expect(tint).toEqual([199, 206, 208]);
    expect(luma(LIGHT_SUBSTRATE) - luma(tint)).toBeGreaterThanOrEqual(41);
  });

});
