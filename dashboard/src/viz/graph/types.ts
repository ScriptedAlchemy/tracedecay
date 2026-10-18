/** The field's public vocabulary: what a caller hands the canvas, and what the
 * canvas promises to say about it. Kept apart from the React component so the
 * pure layout modules can name these shapes without importing a renderer. */

export interface GraphCanvasNode {
  id: string;
  label: string;
  kind: string;
  /** Connectedness when the endpoint supplied it. Absence stays absent: the
   * renderer uses its minimum marker and prints that this is unknown, not 0. */
  degree?: number;
  /**
   * Real, caller-supplied liveness in 0..1, recency for projects, freshness
   * for stores, whatever this graph's genuine decay signal is. It sets the
   * node's RESTING luminance: a live node burns at full hue, a dormant one
   * sinks back toward the substrate. Never a decoration; omit it when the
   * caller has no such measurement and every node rests at the same
   * (deliberately unremarkable) brightness.
   */
  vitality?: number;
  /**
   * Caller-supplied layout position, in the caller's own coordinate space.
   * Supply it only when the position MEASURES something, an axis the caption
   * names, because a fixed coordinate is read as meaning far more strongly
   * than a force-layout one. When every node carries `x`/`y` the canvas skips
   * ForceAtlas2 and the constellation re-centering entirely and draws the
   * composition it was handed; when any node lacks them the whole graph falls
   * back to the force layout, because a half-placed field would silently mix
   * measured positions with emergent ones.
   */
  x?: number;
  y?: number;
}

export interface GraphCanvasEdge {
  source: string;
  target: string;
  kind?: string;
}
