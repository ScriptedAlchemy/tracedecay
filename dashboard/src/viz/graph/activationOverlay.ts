import type Graph from 'graphology';
import {
  approach,
  lerpRgbTuple,
  restingNodeTint,
  settled,
  type ActivationField,
} from './activation.ts';
import { BLOOM, HALO, PULSE, RING, upsert, type Strand } from './managed.ts';
import { rgba, type ThemeBox } from './palette.ts';
import type { FocusState } from './renderer.ts';

/**
 * The activation overlay: everything on the field that is a response to a real
 * event rather than a fact about the graph.
 *
 * It owns the glow companions (a body's corona and bloom), the light that
 * travels a warm dendrite, and the render loop that resolves both. The loop
 * runs only while something real is unresolved — a warm activation field, or a
 * hover isolation still easing into place — and stops itself the moment both
 * settle, so an idle dashboard costs nothing. It also advances the hover
 * easing, because that easing shares the same frames; the renderer owns where
 * the hover points, this owns how fast it gets there.
 */

export interface ActivationOverlayOptions {
  graph: Graph;
  /** The caller's own nodes; companions are gated on this count, never on the
   * graph's, which the dendrite pass has already inflated with waypoints. */
  realNodes: readonly string[];
  strands: readonly Strand[];
  field: ActivationField;
  neighborsOf: ReadonlyMap<string, string[]>;
  theme: ThemeBox;
  focus: FocusState;
  /** Repaint the renderer. A no-op once the scene is gone. */
  paint: () => void;
  /** Read per call, never captured: the reader can change this while the field
   * is on screen and every decision below must see the new answer. */
  isReduced: () => boolean;
}

export interface ActivationOverlay {
  /** Start (or keep) the loop, or compose statically under reduced motion. */
  wake(): void;
  /** The no-motion composition: jump every eased quantity to its destination,
   * remove the travelling light entirely, and paint once. */
  settle(): void;
  /** One static composition of the resting field. */
  repaintResting(): void;
  /** A node was struck by the pointer: it fires now, its neighbourhood one
   * synaptic delay later, along real caller/reference edges only. */
  fireNeighborhood(node: string): void;
  stop(): void;
}

export function createActivationOverlay({
  graph,
  realNodes,
  strands,
  field,
  neighborsOf,
  theme,
  focus,
  paint,
  isReduced,
}: ActivationOverlayOptions): ActivationOverlay {
  // ---- glow companions ------------------------------------------------
  // Every point is a body with falloff, not a flat disc: a tight corona in
  // the node's own hue plus a wide, very faint bloom give depth without a
  // shader. Both ride real signal — vitality at rest, heat when struck — so
  // a quiet graph is genuinely quieter, not merely smaller.
  const restingGlow = realNodes.length <= 400;
  const syncGlow = (): void => {
    const colors = theme.colors;
    for (const node of realNodes) {
      const heat = field.heatOf(node);
      const haloId = HALO + node;
      const bloomId = BLOOM + node;
      const ringId = RING + node;
      if (heat > 0.1 || restingGlow) {
        const attrs = graph.getNodeAttributes(node);
        const size = attrs['size'] as number;
        const vitality = (attrs['vitality'] as number | undefined) ?? 0.6;
        const [kr, kg, kb] =
          (attrs['kindRgb'] as [number, number, number] | undefined) ?? colors.hot;
        const resting = restingNodeTint(
          colors.substrate,
          [kr, kg, kb],
          vitality,
          colors.light,
        );
        const lit = heat > 0
          ? lerpRgbTuple(resting, colors.hot, Math.min(1, heat))
          : resting;
        const shared = { x: attrs['x'], y: attrs['y'], label: '' };
        // Sigma draws each companion as a hard-edged disc, so a corona is
        // really three concentric steps and every step is a visible edge.
        // The old radii (1.55x and 2.9x the body) made those edges read as
        // banding rather than falloff, and turned a modest graph into a field
        // of lollipops. Pulled in tight, the resting glow is a rim on the
        // body instead of a second object beside it -- and a strike still
        // has all the room it needs to swell.
        upsert(graph, haloId, {
          ...shared,
          // Tight enough to read as a luminous rim, not a second donut body.
          size: size * (1.11 + 0.55 * heat),
          color: rgba(lit, 0.045 + 0.07 * vitality + 0.22 * heat),
          zIndex: 1,
        });
        upsert(graph, bloomId, {
          ...shared,
          size: size * (1.68 + 1.3 * heat),
          color: rgba(lit, 0.012 + 0.022 * vitality + 0.08 * heat),
          zIndex: 0,
        });
        // Impact flare: a wide, faint ring pops on strike and expands as the
        // bloom settles, so a firing is legible even in peripheral vision.
        if (heat > 0.5) {
          upsert(graph, ringId, {
            ...shared,
            size: size * (2.1 + 2.2 * (1 - heat)),
            color: rgba(colors.hot, 0.075 * heat),
            zIndex: 1,
          });
        } else if (graph.hasNode(ringId)) {
          graph.dropNode(ringId);
        }
      } else {
        for (const id of [haloId, bloomId, ringId]) {
          if (graph.hasNode(id)) graph.dropNode(id);
        }
      }
    }
  };

  // ---- travelling light ------------------------------------------------
  // While a relation is warm, one bright point runs the dendrite from the
  // hotter end to the cooler one, following the curve rather than the chord.
  // Pulses exist only while the field is warm, and the frozen bbox keeps
  // them from ever rescaling the camera.
  const syncPulses = (now: number): void => {
    const period = 1100;
    const phase = (now % period) / period;
    for (let index = 0; index < strands.length; index += 1) {
      const strand = strands[index]!;
      const heatFrom = field.heatOf(strand.from);
      const heatTo = field.heatOf(strand.to);
      const travel = Math.max(heatFrom, heatTo);
      const pulseId = `${PULSE}${index}`;
      if (travel > 0.18 && !isReduced()) {
        const forward = heatFrom >= heatTo;
        const points = strand.points;
        const spans = points.length - 1;
        const walked = (forward ? phase : 1 - phase) * spans;
        const span = Math.max(0, Math.min(spans - 1, Math.floor(walked)));
        const local = walked - span;
        const a = points[span]!;
        const b = points[span + 1]!;
        upsert(graph, pulseId, {
          x: a[0] + (b[0] - a[0]) * local,
          y: a[1] + (b[1] - a[1]) * local,
          size: 1.4 + 2 * travel,
          color: rgba(theme.colors.hot, 0.9 * travel),
          label: '',
          zIndex: 4,
        });
      } else if (graph.hasNode(pulseId)) {
        graph.dropNode(pulseId);
      }
    }
  };

  const dropPulses = (): void => {
    for (const node of [...graph.nodes()]) {
      if (node.startsWith(PULSE)) graph.dropNode(node);
    }
  };

  let stopped = false;
  let lastFrame = 0;
  let raf = 0;

  // Reduced motion never starts the loop: state is applied in one static
  // refresh instead.
  const step = (now: number): void => {
    const delta = lastFrame === 0 ? 16 : now - lastFrame;
    lastFrame = now;
    const warm = field.tick(now);
    focus.t = approach(focus.t, focus.target, delta, 90);
    const focusSettled = settled(focus.t, focus.target);
    if (focusSettled) {
      focus.t = focus.target;
      if (focus.target === 0) focus.node = null;
    }
    syncGlow();
    syncPulses(now);
    if (!warm) dropPulses();
    paint();
    const keepGoing = !stopped && (warm || !focusSettled);
    raf = keepGoing && !isReduced() ? requestAnimationFrame(step) : 0;
    if (!keepGoing) lastFrame = 0;
  };

  /** Nothing here is "faster" — the intermediate frames do not exist. */
  const settle = (): void => {
    if (raf) {
      cancelAnimationFrame(raf);
      raf = 0;
    }
    lastFrame = 0;
    field.tick(performance.now());
    focus.t = focus.target;
    if (focus.target === 0) focus.node = null;
    // A pulse is pure travel, so under reduced motion it has no resting form
    // to snap to; it is dropped rather than parked somewhere along its curve.
    dropPulses();
    syncGlow();
    paint();
  };

  const wake = (): void => {
    if (isReduced()) {
      settle();
      return;
    }
    if (!raf) {
      lastFrame = 0;
      raf = requestAnimationFrame(step);
    }
  };

  return {
    wake,
    settle,
    repaintResting: () => {
      syncGlow();
      paint();
    },
    fireNeighborhood: (node: string) => {
      // Traveling activation: the struck node fires now; its neighborhood
      // fires one synaptic delay later (real caller/reference edges only).
      field.strike([node], 1);
      const neighbors = neighborsOf.get(node) ?? [];
      // Under reduced motion the synaptic delay collapses: the neighbourhood is
      // lit in the same paint as the struck node, so the propagation is still
      // fully legible as a state without anything travelling across the screen.
      if (isReduced()) field.strike(neighbors, 0.55);
      else setTimeout(() => { field.strike(neighbors, 0.55); wake(); }, 140);
      wake();
    },
    stop: () => {
      stopped = true;
      if (raf) {
        cancelAnimationFrame(raf);
        raf = 0;
      }
    },
  };
}
