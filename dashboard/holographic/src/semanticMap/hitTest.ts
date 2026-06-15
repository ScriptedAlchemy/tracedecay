import type { MemoryProjectionPoint } from "../types";

export const GRID_CELL = 48;

export interface PlacedPoint {
  point: MemoryProjectionPoint;
  x: number;
  y: number;
  r: number;
  color: string;
}

export function buildGrid(placed: PlacedPoint[]): Map<string, number[]> {
  const grid = new Map<string, number[]>();
  placed.forEach((p, i) => {
    const key = `${Math.floor(p.x / GRID_CELL)},${Math.floor(p.y / GRID_CELL)}`;
    const cell = grid.get(key);
    if (cell) cell.push(i);
    else grid.set(key, [i]);
  });
  return grid;
}

export function findNearest(
  placed: PlacedPoint[],
  grid: Map<string, number[]>,
  bx: number,
  by: number,
  radius: number,
): PlacedPoint | null {
  const c0 = Math.floor((bx - radius) / GRID_CELL);
  const c1 = Math.floor((bx + radius) / GRID_CELL);
  const r0 = Math.floor((by - radius) / GRID_CELL);
  const r1 = Math.floor((by + radius) / GRID_CELL);
  let best: PlacedPoint | null = null;
  let bestD = radius * radius;
  for (let cx = c0; cx <= c1; cx++) {
    for (let cy = r0; cy <= r1; cy++) {
      const cell = grid.get(`${cx},${cy}`);
      if (!cell) continue;
      for (const i of cell) {
        const p = placed[i];
        const dx = p.x - bx;
        const dy = p.y - by;
        const d = dx * dx + dy * dy;
        if (d <= bestD) {
          bestD = d;
          best = p;
        }
      }
    }
  }
  return best;
}
