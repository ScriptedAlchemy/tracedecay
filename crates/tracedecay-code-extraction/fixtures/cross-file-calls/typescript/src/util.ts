export function normalize(text: string): string {
  return text.trim().toLowerCase();
}

export function clamp(value: number, low: number, high: number): number {
  return Math.max(low, Math.min(value, high));
}
