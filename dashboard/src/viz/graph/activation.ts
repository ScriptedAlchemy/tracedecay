/** Activation field for the synapse view: per-node heat that decays
 * exponentially toward dark, struck by real events (search hits, selection,
 * caller-edge traversal, SSE activity). Pure model — renderers sample it. */

export interface ActivationOptions {
  /** Half-life of a strike, in milliseconds. */
  halfLifeMs?: number;
  /** Heat below this is treated as cold and dropped. */
  floor?: number;
}

export class ActivationField {
  private heat = new Map<string, number>();
  private lastTick = 0;
  private readonly halfLifeMs: number;
  private readonly floor: number;

  constructor(options: ActivationOptions = {}) {
    this.halfLifeMs = options.halfLifeMs ?? 2600;
    this.floor = options.floor ?? 0.02;
  }

  /** Strike nodes with energy (clamped to 1). Cumulative with existing heat. */
  strike(ids: Iterable<string>, energy = 1): void {
    for (const id of ids) {
      this.heat.set(id, Math.min(1, (this.heat.get(id) ?? 0) + energy));
    }
  }

  /** Advance decay to `now` (ms clock). Returns true while anything is warm. */
  tick(now: number): boolean {
    if (this.lastTick === 0) {
      this.lastTick = now;
      return this.heat.size > 0;
    }
    const dt = now - this.lastTick;
    this.lastTick = now;
    if (dt <= 0) return this.heat.size > 0;
    const factor = Math.pow(0.5, dt / this.halfLifeMs);
    for (const [id, value] of this.heat) {
      const next = value * factor;
      if (next < this.floor) this.heat.delete(id);
      else this.heat.set(id, next);
    }
    return this.heat.size > 0;
  }

  heatOf(id: string): number {
    return this.heat.get(id) ?? 0;
  }

  get warm(): boolean {
    return this.heat.size > 0;
  }

  clear(): void {
    this.heat.clear();
  }
}

const rgbCache = new Map<string, [number, number, number]>();

/** Resolve any CSS color (incl. oklch) to rgb via the canvas parser, so heat
 * lerps can be computed numerically while colors stay token-derived.
 * Memoized: reducers call this per node per animation frame. */
export function cssColorToRgb(color: string): [number, number, number] {
  const cached = rgbCache.get(color);
  if (cached) return cached;
  const canvas = document.createElement('canvas');
  canvas.width = canvas.height = 1;
  const context = canvas.getContext('2d', { willReadFrequently: true });
  if (!context) return [128, 128, 128];
  context.fillStyle = color;
  context.fillRect(0, 0, 1, 1);
  const [r, g, b] = context.getImageData(0, 0, 1, 1).data;
  const resolved: [number, number, number] = [r ?? 128, g ?? 128, b ?? 128];
  rgbCache.set(color, resolved);
  return resolved;
}

export function lerpRgb(
  from: [number, number, number],
  to: [number, number, number],
  t: number,
): string {
  const mix = (a: number, b: number) => Math.round(a + (b - a) * t);
  return `rgb(${mix(from[0], to[0])}, ${mix(from[1], to[1])}, ${mix(from[2], to[2])})`;
}
