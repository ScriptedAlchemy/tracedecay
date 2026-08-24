/**
 * The drawn CORTEX field: one canvas, one model, and the wiring the renderer
 * needs to live in a browser — a viewport measured from the host box, a palette
 * resampled when the theme flips, and pointer hits translated into world
 * coordinates. Nothing here reads a payload or writes a caption.
 *
 * There is no simulation. The plan's performance boundary (`:175`) puts springs
 * on the ≤250-node TRACE subgraph and says the cortex "simulates dozens of
 * bodies, not thousands"; at this altitude the bodies are aggregated regions
 * whose positions are the MEASUREMENT — depth and cluster order — so moving
 * them would destroy the only thing they encode. The relief is a still map, on
 * purpose, and that is also why it costs one paint rather than 60 a second.
 *
 * Two states are printed instead of drawn, because a field that cannot be
 * measured must not be shown: a column narrower than `MIN_FIELD_WIDTH`, and a
 * browser that hands back no 2D context. In both, the table below carries every
 * region this would have drawn — which is what keeps the canvas legitimately
 * one `role="img"` rather than the only copy of the data.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';

import { cn } from '../../ui/cn';
import { cortexDescription, type CortexModel } from './cortexRelief.ts';
import {
  createCortexRenderer,
  hitRegion,
  resolveCortexPalette,
  type CortexRenderer,
} from './cortexRender.ts';

/** Narrowest column the relief can be read in. Below it, the table stands
 * alone: region labels overprint and the contour rings fall under a pixel. */
const MIN_FIELD_WIDTH = 480;

export function CortexCanvas({
  model,
  selected,
  focusedDirectory,
  onSelect,
}: {
  model: CortexModel;
  selected: string | null;
  focusedDirectory: string | null;
  onSelect: (directory: string | null) => void;
}) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const rendererRef = useRef<CortexRenderer | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [narrow, setNarrow] = useState(false);
  const narrowRef = useRef(false);
  const frameRef = useRef({ selected, focused: focusedDirectory });
  frameRef.current = { selected, focused: focusedDirectory };

  const description = useMemo(() => cortexDescription(model), [model]);

  useEffect(() => {
    const host = hostRef.current;
    const canvas = canvasRef.current;
    if (!host || !canvas) return;

    let renderer: CortexRenderer;
    try {
      renderer = createCortexRenderer(canvas, model);
    } catch {
      // jsdom and any context-starved environment land here. The relief is a
      // picture of data that is fully present in the table below, so the
      // surface stays usable rather than blank-screening.
      setUnavailable(true);
      return;
    }
    rendererRef.current = renderer;
    renderer.setPalette(resolveCortexPalette(host));

    const paint = () => {
      if (narrowRef.current) return;
      renderer.draw(frameRef.current);
    };

    const resize = () => {
      const box = host.getBoundingClientRect();
      const width = Math.max(320, Math.round(box.width));
      // A zero measurement means "not laid out yet" (or a detached/jsdom
      // tree), which is not the same claim as "too narrow".
      const tooNarrow = box.width > 0 && box.width < MIN_FIELD_WIDTH;
      narrowRef.current = tooNarrow;
      setNarrow(tooNarrow);
      const height = Math.round((width * model.world.height) / model.world.width);
      renderer.setViewport({ width, height, dpr: window.devicePixelRatio || 1 });
      paint();
    };
    resize();

    const observer = typeof ResizeObserver === 'function' ? new ResizeObserver(resize) : null;
    observer?.observe(host);

    const themeObserver =
      typeof MutationObserver === 'function'
        ? new MutationObserver(() => {
            renderer.setPalette(resolveCortexPalette(host));
            paint();
          })
        : null;
    themeObserver?.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme'],
    });

    return () => {
      observer?.disconnect();
      themeObserver?.disconnect();
      rendererRef.current = null;
    };
  }, [model]);

  // Selection and "you are here" are frame state, not model state, so a change
  // repaints instead of rebuilding the renderer.
  useEffect(() => {
    if (narrowRef.current) return;
    rendererRef.current?.draw({ selected, focused: focusedDirectory });
  }, [selected, focusedDirectory]);

  const onClick = useCallback(
    (event: React.PointerEvent<HTMLCanvasElement>) => {
      const renderer = rendererRef.current;
      if (!renderer) return;
      const rect = event.currentTarget.getBoundingClientRect();
      const world = renderer.toWorld(event.clientX - rect.left, event.clientY - rect.top);
      onSelect(hitRegion(model, world.x, world.y));
    },
    [model, onSelect],
  );

  return (
    <div ref={hostRef} className="relative w-full">
      {/* The description is a summary of the MEASUREMENTS, not a narration of
       * pixels, so it stands whether or not this browser gave a canvas — which
       * is what makes it a text alternative rather than a caption. It is
       * dropped only at widths where the relief is not drawn at all and the
       * table sits immediately below it, because a reader there would be told
       * the same thing twice. Same rule as `TraceCanvas`. */}
      <div {...(narrow ? {} : { role: 'img', 'aria-label': description })}>
        <canvas
          ref={canvasRef}
          data-testid="cortex-canvas"
          aria-hidden
          className={cn('block w-full touch-none', (narrow || unavailable) && 'hidden')}
          onPointerDown={onClick}
        />
      </div>
      {narrow && !unavailable ? (
        <p className="border border-edge-subtle bg-surface-1 p-4 text-center text-xs leading-relaxed text-text-muted">
          The relief needs about {MIN_FIELD_WIDTH} px to be read and this column is
          narrower, so it is not drawn rather than drawn illegibly. Every region, depth
          and edge count it carries is in the table below.
        </p>
      ) : null}
      {unavailable ? (
        <p className="p-4 text-center text-xs text-text-muted">
          This browser gave no 2D canvas, so the relief is not drawn. Every region, depth
          and edge count it would carry is in the table below.
        </p>
      ) : null}
    </div>
  );
}
