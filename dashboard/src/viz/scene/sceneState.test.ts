import { describe, expect, it } from 'vitest';
import { EMPHASIS_DIM, FOCUS_DIM, bodyState, channelByte, wantsNextFrame } from './sceneState.ts';

const REST = {
  heat: 0,
  vitality: 0.8,
  inFocusNeighborhood: null,
  focusT: 0,
  shown: false,
  outsideEmphasis: null,
} as const;

describe('scene body state channels', () => {
  it('lets inspection touch dim and raise but never heat', () => {
    const hovered = bodyState({ ...REST, inFocusNeighborhood: true, focusT: 1, shown: true });
    const unrelated = bodyState({ ...REST, inFocusNeighborhood: false, focusT: 1 });
    expect(hovered.heat).toBe(0);
    expect(unrelated.heat).toBe(0);
    expect(hovered.raise).toBe(1);
    expect(hovered.dim).toBe(0);
    expect(unrelated.raise).toBe(0);
    expect(unrelated.dim).toBeCloseTo(FOCUS_DIM, 9);
    // Only enough to establish focus: the unrelated body stays legible.
    expect(unrelated.dim).toBeLessThan(0.7);
  });

  it('carries admitted heat through untouched by focus or emphasis', () => {
    const warm = bodyState({ ...REST, heat: 0.6, inFocusNeighborhood: false, focusT: 1, outsideEmphasis: true });
    expect(warm.heat).toBeCloseTo(0.6, 9);
    expect(bodyState({ ...REST, heat: 3 }).heat).toBe(1);
    expect(bodyState({ ...REST, heat: -1 }).heat).toBe(0);
  });

  it('recedes bodies outside a focused repository and keeps the stronger dim', () => {
    expect(bodyState({ ...REST, outsideEmphasis: true }).dim).toBeCloseTo(EMPHASIS_DIM, 9);
    expect(bodyState({ ...REST, outsideEmphasis: false }).dim).toBe(0);
    const both = bodyState({ ...REST, outsideEmphasis: true, inFocusNeighborhood: false, focusT: 1 });
    expect(both.dim).toBeCloseTo(Math.max(EMPHASIS_DIM, FOCUS_DIM), 9);
  });

  it('eases the isolation with the focus strength', () => {
    expect(bodyState({ ...REST, inFocusNeighborhood: false, focusT: 0.5 }).dim).toBeCloseTo(FOCUS_DIM / 2, 9);
    expect(bodyState({ ...REST, shown: true, focusT: 0.25 }).raise).toBe(0.25);
  });

  it('quantises channels to the byte the state texture holds', () => {
    expect(channelByte(0)).toBe(0);
    expect(channelByte(1)).toBe(255);
    expect(channelByte(0.5)).toBe(128);
    expect(channelByte(2)).toBe(255);
  });
});

describe('frame policy', () => {
  it('runs while heat, focus or camera is unsettled and stops otherwise', () => {
    expect(wantsNextFrame({ warm: true, focusSettled: true, cameraMoving: false, reduced: false })).toBe(true);
    expect(wantsNextFrame({ warm: false, focusSettled: false, cameraMoving: false, reduced: false })).toBe(true);
    expect(wantsNextFrame({ warm: false, focusSettled: true, cameraMoving: true, reduced: false })).toBe(true);
    expect(wantsNextFrame({ warm: false, focusSettled: true, cameraMoving: false, reduced: false })).toBe(false);
  });

  it('never asks for a frame under reduced motion, however unsettled the scene is', () => {
    expect(wantsNextFrame({ warm: true, focusSettled: false, cameraMoving: true, reduced: true })).toBe(false);
  });
});
