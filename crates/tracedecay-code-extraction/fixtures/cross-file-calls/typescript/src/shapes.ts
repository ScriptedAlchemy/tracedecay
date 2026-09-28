import * as math from "./math";
import { clamp } from "./util";

export function area(width: number, height: number): number {
  return clamp(width, 0, 100) * height;
}

export function perimeter(width: number, height: number): number {
  return math.total([clamp(width, 0, 100), height]) * 2;
}
