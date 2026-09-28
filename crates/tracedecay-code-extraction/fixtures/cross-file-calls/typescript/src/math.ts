import { clamp } from "./util";

export function total(values: number[]): number {
  return values.reduce((sum, value) => sum + value, 0);
}

export function mean(values: number[]): number {
  return total(values) / values.length;
}

export function scale(value: number, factor: number): number {
  return clamp(value * factor, 0, 100);
}
