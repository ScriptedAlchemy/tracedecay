import type { PlacedPoint } from "./hitTest";

export const DENSITY_AUTO_THRESHOLD = 350;

export interface DensityCell {
  x: number;
  y: number;
  w: number;
  h: number;
  opacity: number;
}

export function buildDensity(placed: PlacedPoint[], baseW: number, baseH: number): DensityCell[] {
  const cols = Math.max(8, Math.round(baseW / 30));
  const rows = Math.max(6, Math.round(baseH / 30));
  const cw = baseW / cols;
  const ch = baseH / rows;
  const counts = new Float32Array(cols * rows);
  for (const p of placed) {
    const cx = Math.min(cols - 1, Math.max(0, Math.floor(p.x / cw)));
    const cy = Math.min(rows - 1, Math.max(0, Math.floor(p.y / ch)));
    counts[cy * cols + cx] += 1;
  }
  const blurred = new Float32Array(cols * rows);
  for (let y = 0; y < rows; y++) {
    for (let x = 0; x < cols; x++) {
      let sum = 0;
      let n = 0;
      for (let dy = -1; dy <= 1; dy++) {
        for (let dx = -1; dx <= 1; dx++) {
          const xx = x + dx;
          const yy = y + dy;
          if (xx < 0 || yy < 0 || xx >= cols || yy >= rows) continue;
          sum += counts[yy * cols + xx];
          n += 1;
        }
      }
      blurred[y * cols + x] = sum / n;
    }
  }
  let max = 0;
  for (const v of blurred) if (v > max) max = v;
  if (max <= 0) return [];
  const cells: DensityCell[] = [];
  for (let y = 0; y < rows; y++) {
    for (let x = 0; x < cols; x++) {
      const v = blurred[y * cols + x];
      if (v <= 0.01) continue;
      cells.push({
        x: x * cw,
        y: y * ch,
        w: cw + 0.5,
        h: ch + 0.5,
        opacity: Math.min(0.34, (v / max) * 0.34),
      });
    }
  }
  return cells;
}
