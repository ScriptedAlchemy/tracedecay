/**
 * The drawn field: one canvas, one simulation, and the loop between them.
 *
 * `buildSimSpec` decides the forces and `render.ts` decides nothing, so what is
 * left is the wiring those two need to live in a browser — a viewport measured
 * from the host box, a palette resampled when the theme flips, pointer gestures
 * translated into world coordinates, and the frame that steps the integrator.
 * That is a separate job from saying what the picture means, which is why it is
 * a separate module: nothing here reads a payload or writes a caption.
 *
 * Reduced motion is a rendering MODE, not a switched-off feature. The identical
 * `step()` sequence is run to rest by `settle` and painted once, with tension
 * drawn as thickness, so a reader who never sees a frame move still sees every
 * quantity the animated path encodes.
 *
 * Two states are printed instead of drawn, because a field that cannot be
 * measured must not be shown: a column narrower than `MIN_FIELD_WIDTH`, and a
 * browser that hands back no 2D context. In both, the accessible list below
 * carries every figure this would have drawn — which is what keeps the canvas
 * legitimately one `role="img"` rather than the only copy of the data.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';

import { cn } from '../../ui/cn';
import { buildSimSpec, fieldDescription } from '../../viz/trace/model.ts';
import { createRenderer, type TraceRenderer } from '../../viz/trace/render.ts';
import { bloomStep, createSimulation, type Simulation } from '../../viz/trace/sim.ts';
import { resolveTracePalette } from '../../viz/trace/palette.ts';
import type { TraceModel } from '../../viz/trace/types.ts';

/** One fixed step per frame. Wall-clock jitter never reaches the integrator. */
const DT = 1 / 60;
/** Narrowest column the field can be read in. Below it, the list stands alone. */
const MIN_FIELD_WIDTH = 460;

export function TraceCanvas({
  model,
  reduced,
  onHoverChange,
  onDragChange,
}: {
  model: TraceModel;
  reduced: boolean;
  onHoverChange: (id: string | null) => void;
  onDragChange: (id: string | null) => void;
}) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [narrow, setNarrow] = useState(false);

  const simRef = useRef<Simulation | null>(null);
  const rendererRef = useRef<TraceRenderer | null>(null);
  const bloomRef = useRef<Float64Array>(new Float64Array(model.nodes.length));
  const hoveredRef = useRef<string | null>(null);
  const draggingRef = useRef<string | null>(null);
  /** Set by the effect below; lets the pointer handlers re-settle in reduced
   * motion without being torn down and rebuilt on every gesture. */
  const reSettleRef = useRef<(() => void) | null>(null);
  /** Mirror of `narrow` readable from inside the draw loop. */
  const narrowRef = useRef(false);

  const description = useMemo(() => fieldDescription(model), [model]);

  useEffect(() => {
    const host = hostRef.current;
    const canvas = canvasRef.current;
    if (!host || !canvas) return;

    let renderer: TraceRenderer;
    try {
      renderer = createRenderer(canvas, model);
    } catch {
      // jsdom and any context-starved environment land here. The field is a
      // picture of data that is fully present in the list below, so the
      // surface stays usable rather than blank-screening.
      setUnavailable(true);
      return;
    }
    rendererRef.current = renderer;
    renderer.setPalette(resolveTracePalette(host));

    const sim = createSimulation(buildSimSpec(model));
    simRef.current = sim;
    const bloom = new Float64Array(model.nodes.length);
    bloomRef.current = bloom;

    let raf = 0;
    let disposed = false;

    const paint = () => {
      if (narrowRef.current) return;
      renderer.draw({
        positions: sim.positions(),
        stretches: sim.stretches(),
        bloom,
        draggingId: draggingRef.current,
        hoveredId: hoveredRef.current,
        reducedMotion: reduced,
      });
    };

    /** Reduced motion: the identical step sequence, run to rest, painted once. */
    const settleAndPaint = () => {
      sim.settle({ dt: DT });
      // Bloom is a hover channel, not physics; at rest it is simply resolved to
      // its target so a keyboard reader sees the same emphasis a pointer would.
      for (let i = 0; i < bloom.length; i += 1) {
        bloom[i] = model.nodes[i]!.id === hoveredRef.current ? 1 : 0;
      }
      paint();
    };

    const frame = () => {
      if (disposed) return;
      sim.step(DT);
      model.nodes.forEach((node, i) => {
        const target = node.id === hoveredRef.current ? 1 : 0;
        bloom[i] = bloomStep(bloom[i]!, target, node.degree ?? 0, DT);
      });
      paint();
      raf = requestAnimationFrame(frame);
    };

    const resize = () => {
      const box = host.getBoundingClientRect();
      const width = Math.max(320, Math.round(box.width));
      // Below this the field stops being a reading of the data and becomes a
      // texture: sills fall under a pixel, labels overprint, and a reader would
      // be looking at something that cannot be measured. The ranked list below
      // carries every figure the field encodes, so the honest move is to say
      // the picture does not fit rather than to draw an unreadable one.
      // A zero measurement means "not laid out yet" (or a detached/jsdom
      // tree), which is not the same claim as "too narrow" — so it is not
      // treated as one.
      const tooNarrow = box.width > 0 && box.width < MIN_FIELD_WIDTH;
      narrowRef.current = tooNarrow;
      setNarrow(tooNarrow);
      // The world is 1440x1160; holding that ratio keeps the hop rings evenly
      // spaced at every width instead of squashing them at narrow viewports.
      const height = Math.round((width * model.world.height) / model.world.width);
      renderer.setViewport({ width, height, dpr: window.devicePixelRatio || 1 });
      if (reduced) paint();
    };
    resize();

    const observer =
      typeof ResizeObserver === 'function' ? new ResizeObserver(resize) : null;
    observer?.observe(host);

    const themeObserver =
      typeof MutationObserver === 'function'
        ? new MutationObserver(() => {
            renderer.setPalette(resolveTracePalette(host));
            paint();
          })
        : null;
    themeObserver?.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme'],
    });

    if (reduced) settleAndPaint();
    else raf = requestAnimationFrame(frame);

    reSettleRef.current = settleAndPaint;

    return () => {
      disposed = true;
      if (raf) cancelAnimationFrame(raf);
      observer?.disconnect();
      themeObserver?.disconnect();
      rendererRef.current = null;
      simRef.current = null;
      reSettleRef.current = null;
    };
  }, [model, reduced]);

  const toWorld = useCallback((event: React.PointerEvent<HTMLCanvasElement>) => {
    const renderer = rendererRef.current;
    if (!renderer) return null;
    const rect = event.currentTarget.getBoundingClientRect();
    return renderer.toWorld(event.clientX - rect.left, event.clientY - rect.top);
  }, []);

  const onPointerMove = useCallback(
    (event: React.PointerEvent<HTMLCanvasElement>) => {
      const sim = simRef.current;
      const renderer = rendererRef.current;
      const world = toWorld(event);
      if (!sim || !renderer || !world) return;
      if (draggingRef.current) {
        sim.applyDrag(draggingRef.current, world.x, world.y);
        if (reduced) reSettleRef.current?.();
        return;
      }
      const hit = renderer.hitTest(sim.positions(), world.x, world.y);
      if (hit !== hoveredRef.current) {
        hoveredRef.current = hit;
        onHoverChange(hit);
        if (reduced) reSettleRef.current?.();
      }
    },
    [toWorld, reduced, onHoverChange],
  );

  const onPointerDown = useCallback(
    (event: React.PointerEvent<HTMLCanvasElement>) => {
      const sim = simRef.current;
      const renderer = rendererRef.current;
      const world = toWorld(event);
      if (!sim || !renderer || !world) return;
      const hit = renderer.hitTest(sim.positions(), world.x, world.y);
      if (!hit) return;
      draggingRef.current = hit;
      onDragChange(hit);
      event.currentTarget.setPointerCapture?.(event.pointerId);
      sim.applyDrag(hit, world.x, world.y);
      if (reduced) reSettleRef.current?.();
    },
    [toWorld, reduced, onDragChange],
  );

  const endDrag = useCallback(
    (event: React.PointerEvent<HTMLCanvasElement>) => {
      const sim = simRef.current;
      if (!sim || !draggingRef.current) return;
      draggingRef.current = null;
      onDragChange(null);
      event.currentTarget.releasePointerCapture?.(event.pointerId);
      // Release carries no fling: a pinned node has zero velocity, so letting
      // go drops it home rather than throwing it. Momentum would feel good and
      // encode nothing measured.
      sim.release();
      if (reduced) reSettleRef.current?.();
    },
    [reduced, onDragChange],
  );

  return (
    <div ref={hostRef} className="relative w-full">
      {/* When the field is not drawn it is not described either: a `role="img"`
       * whose description narrates a picture nobody can see is worse than no
       * picture at all. */}
      <div {...(narrow ? {} : { role: 'img', 'aria-label': description })}>
        <canvas
          ref={canvasRef}
          data-testid="trace-canvas"
          aria-hidden
          className={cn('block w-full touch-none', narrow && 'hidden')}
          onPointerMove={onPointerMove}
          onPointerDown={onPointerDown}
          onPointerUp={endDrag}
          onPointerCancel={endDrag}
          onPointerLeave={(event) => {
            endDrag(event);
            if (hoveredRef.current !== null) {
              hoveredRef.current = null;
              onHoverChange(null);
              if (reduced) reSettleRef.current?.();
            }
          }}
        />
      </div>
      {narrow && !unavailable ? (
        <p className="border border-edge-subtle bg-surface-1 p-4 text-center text-xs leading-relaxed text-text-muted">
          The field needs about {MIN_FIELD_WIDTH} px to be read and this column is
          narrower, so it is not drawn rather than drawn illegibly. Every symbol,
          hop and call-site count it carries is in the list below.
        </p>
      ) : null}
      {unavailable ? (
        <p className="p-4 text-center text-xs text-text-muted">
          This browser gave no 2D canvas, so the field is not drawn. Every symbol and
          call-site count it would carry is in the list below.
        </p>
      ) : null}
    </div>
  );
}
