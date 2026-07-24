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
  private readonly listeners = new Set<() => void>();

  constructor(options: ActivationOptions = {}) {
    this.halfLifeMs = options.halfLifeMs ?? 2600;
    this.floor = options.floor ?? 0.02;
  }

  /** Strike nodes with energy (clamped to 1). Cumulative with existing heat.
   *
   * Notifies subscribers, because a field can be struck from outside whoever
   * draws it: the Brain's SSE effect calls this from a React effect that knows
   * nothing about the canvas's render loop. Without the notification that heat
   * lands on the field while the loop is asleep, so nothing ever draws it —
   * and nothing ever decays it either, since {@link tick} only runs inside the
   * loop. The strike is the real event; this is how it reaches the renderer. */
  strike(ids: Iterable<string>, energy = 1): void {
    let struck = false;
    for (const id of ids) {
      this.heat.set(id, Math.min(1, (this.heat.get(id) ?? 0) + energy));
      struck = true;
    }
    if (struck) for (const listener of this.listeners) listener();
  }

  /** Subscribe to strikes on this field; returns an unsubscribe function. The
   * field owns no clock of its own — this is purely the seam a renderer uses
   * to hear about strikes it did not itself cause. Nothing here fires on a
   * timer, so a subscriber is woken by real events and by nothing else. */
  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
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

/**
 * Frame-rate-independent approach toward a target — the "settle" primitive.
 *
 * Renderers use this so a state change propagates over ~a tenth of a second
 * instead of snapping between two frames. It is driven by elapsed time, so a
 * slow frame does not overshoot, and it converges (never oscillates), which
 * means a caller can stop its loop as soon as {@link settled} reports true.
 * Motion here is always a response to a real event; nothing calls this on a
 * timer of its own.
 */
export function approach(
  current: number,
  target: number,
  deltaMs: number,
  timeConstantMs: number,
): number {
  if (deltaMs <= 0 || timeConstantMs <= 0) return target;
  return target + (current - target) * Math.exp(-deltaMs / timeConstantMs);
}

/** Whether an approach has converged closely enough to stop animating. */
export function settled(current: number, target: number, epsilon = 0.004): boolean {
  return Math.abs(current - target) <= epsilon;
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
  const [r, g, b] = lerpRgbTuple(from, to, t);
  return `rgb(${r}, ${g}, ${b})`;
}

/** The same mix as {@link lerpRgb} but left as components, for callers that
 * need to apply their own alpha rather than a solid `rgb()` string. */
export function lerpRgbTuple(
  from: [number, number, number],
  to: [number, number, number],
  t: number,
): [number, number, number] {
  const mix = (a: number, b: number) => Math.round(a + (b - a) * t);
  return [mix(from[0], to[0]), mix(from[1], to[1]), mix(from[2], to[2])];
}
