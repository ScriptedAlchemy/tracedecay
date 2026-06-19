import { describe, expect, it } from "vitest";

import {
  FIT_MAX_ZOOM,
  IDENTITY,
  semanticMapKeyResult,
  fitTransformToPlaced,
  zoomTransformAt,
} from "../holographic/src/semanticMap/transform";
import {
  panTransformFromDrag,
  pickPointAtScreen,
  selectIdsInScreenRect,
} from "../holographic/src/semanticMap/gestures";

const placed: any[] = [
  { point: { fact_id: 11 }, x: 60, y: 60, r: 6 },
  { point: { fact_id: 22 }, x: 150, y: 140, r: 8 },
  { point: { fact_id: 33 }, x: 280, y: 160, r: 10 },
];

describe("semantic map transform helpers", () => {
  it("zoomTransformAt clamps the zoom level and keeps the cursor's base point fixed", () => {
    const prev = { k: 1, tx: 25, ty: -10 };
    const sx = 180;
    const sy = 120;
    const before = { x: (sx - prev.tx) / prev.k, y: (sy - prev.ty) / prev.k };

    const next = zoomTransformAt(prev, sx, sy, 3.25);
    const after = { x: (sx - next.tx) / next.k, y: (sy - next.ty) / next.k };

    expect(next.k).toBe(3.25);
    expect(after.x).toBeCloseTo(before.x);
    expect(after.y).toBeCloseTo(before.y);
    expect(zoomTransformAt({ k: 24, tx: 0, ty: 0 }, 10, 10, 10).k).toBe(24);
  });

  it("fitTransformToPlaced centers the selection and caps tiny clusters at FIT_MAX_ZOOM", () => {
    const fit = fitTransformToPlaced(
      [
        { point: { fact_id: 1 }, x: 100, y: 100, r: 4 },
        { point: { fact_id: 2 }, x: 101, y: 102, r: 4 },
      ] as any[],
      600,
      400,
    );

    expect(fit.k).toBe(FIT_MAX_ZOOM);
    const centerX = (100 + 101) / 2;
    const centerY = (100 + 102) / 2;
    expect(fit.tx).toBeCloseTo(600 / 2 - centerX * FIT_MAX_ZOOM);
    expect(fit.ty).toBeCloseTo(400 / 2 - centerY * FIT_MAX_ZOOM);
  });

  it("semanticMapKeyResult pans, zooms, resets, and clears selection state for keyboard interaction", () => {
    expect(semanticMapKeyResult({ key: "ArrowRight", transform: IDENTITY, width: 400, height: 300 })).toEqual({
      handled: true,
      nextTransform: { k: 1, tx: -48, ty: 0 },
      clearSelection: false,
      clearSelected: false,
    });

    const zoomed = semanticMapKeyResult({ key: "+", transform: IDENTITY, width: 400, height: 300 });
    expect(zoomed.handled).toBe(true);
    expect(zoomed.nextTransform.k).toBeCloseTo(1.3);

    expect(semanticMapKeyResult({ key: "0", transform: { k: 2, tx: 8, ty: -9 }, width: 400, height: 300 })).toEqual({
      handled: true,
      nextTransform: IDENTITY,
      clearSelection: false,
      clearSelected: false,
    });

    expect(semanticMapKeyResult({ key: "Escape", transform: IDENTITY, width: 400, height: 300 })).toEqual({
      handled: true,
      nextTransform: IDENTITY,
      clearSelection: true,
      clearSelected: true,
    });
  });
});

describe("semantic map gesture helpers", () => {
  it("panTransformFromDrag preserves the gesture origin while translating by screen delta", () => {
    expect(
      panTransformFromDrag(
        { k: 2, tx: 10, ty: -20 },
        { x: 80, y: 90 },
        { x: 120, y: 150 },
      ),
    ).toEqual({ k: 2, tx: 50, ty: 40 });
  });

  it("selectIdsInScreenRect ignores tiny drags and otherwise selects enclosed points", () => {
    expect(selectIdsInScreenRect(placed, IDENTITY, { startX: 60, startY: 60, x: 62, y: 61 })).toBeNull();

    expect(
      [...selectIdsInScreenRect(placed, IDENTITY, { startX: 40, startY: 40, x: 180, y: 180 }) ?? []].sort(),
    ).toEqual([11, 22]);
  });

  it("pickPointAtScreen finds the hovered point using the committed transform", () => {
    const picked = pickPointAtScreen(placed, { k: 1.5, tx: 20, ty: -5 }, 245, 205, 22);
    expect(picked?.point.fact_id).toBe(22);
    expect(pickPointAtScreen(placed, IDENTITY, 390, 390, 22)).toBeNull();
  });
});
