import { useEffect, useRef, useState } from 'react';
import {
  AXIS_HEIGHT,
  TRACK_GAP,
  TRACK_HEIGHT,
  axisTicks,
  pick,
  windowOf,
  xFor,
  type LoomSpan,
  type LoomTrack,
} from './tracks.ts';

/** Reads the resolved theme tokens the canvas needs. Canvas cannot consume CSS
 * variables directly, so draws re-sample on every theme flip. */
function paletteFrom(element: HTMLElement) {
  const style = getComputedStyle(element);
  const token = (name: string, fallback: string) =>
    style.getPropertyValue(name).trim() || fallback;
  return {
    axis: token('--raw-text-muted', '#888'),
    grid: token('--raw-edge-subtle', '#333'),
    mark: token('--raw-accent', '#7aa2f7'),
    label: token('--raw-text-secondary', '#aaa'),
  };
}

/** Loom track canvas: fixed lanes, device-pixel aware, hover picking, and a
 * knowledge-time axis. The Perfetto-model skeleton the deeper span sources
 * (hooks, automation runs, agent turns) plug into. */
export function TrackCanvas({ tracks }: { tracks: LoomTrack[] }) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [hover, setHover] = useState<{ track: LoomTrack; span: LoomSpan } | null>(null);
  const window_ = windowOf(tracks);
  const height = AXIS_HEIGHT + tracks.length * (TRACK_HEIGHT + TRACK_GAP);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !window_) return;
    const draw = () => {
      const context = canvas.getContext('2d');
      if (!context) return;
      const ratio = globalThis.devicePixelRatio || 1;
      const width = canvas.clientWidth;
      canvas.width = Math.round(width * ratio);
      canvas.height = Math.round(height * ratio);
      context.scale(ratio, ratio);
      context.clearRect(0, 0, width, height);
      const palette = paletteFrom(canvas);

      context.font = '10px system-ui, sans-serif';
      for (const tick of axisTicks(window_, width)) {
        context.strokeStyle = palette.grid;
        context.beginPath();
        context.moveTo(tick.x, AXIS_HEIGHT - 6);
        context.lineTo(tick.x, height);
        context.stroke();
        context.fillStyle = palette.axis;
        context.fillText(tick.label, tick.x + 3, 10);
      }

      tracks.forEach((track, row) => {
        const top = AXIS_HEIGHT + row * (TRACK_HEIGHT + TRACK_GAP);
        const maxWeight = Math.max(...track.spans.map((span) => span.weight), 1);
        for (const span of track.spans) {
          const x0 = xFor(span.start, window_, width);
          const x1 = Math.max(xFor(span.end, window_, width), x0 + 3);
          const magnitude = Math.max(0.25, span.weight / maxWeight);
          const barHeight = Math.max(4, (TRACK_HEIGHT - 6) * magnitude);
          const selected = hover?.span.id === span.id;
          context.fillStyle = palette.mark;
          context.globalAlpha = selected ? 1 : 0.6;
          context.beginPath();
          context.roundRect(
            x0,
            top + (TRACK_HEIGHT - barHeight) / 2,
            x1 - x0,
            barHeight,
            2,
          );
          context.fill();
          context.globalAlpha = 1;
        }
      });
    };
    draw();
    const observer = new MutationObserver(draw);
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme'],
    });
    const resize = new ResizeObserver(draw);
    resize.observe(canvas);
    return () => {
      observer.disconnect();
      resize.disconnect();
    };
  }, [tracks, window_ && `${window_.start}-${window_.end}`, hover?.span.id, height]);

  if (!window_ || tracks.length === 0) {
    return (
      <p className="p-6 text-center text-sm text-text-muted">
        no activity spans in the current window
      </p>
    );
  }

  return (
    <div className="flex">
      <div
        className="flex shrink-0 flex-col"
        style={{ paddingTop: AXIS_HEIGHT, gap: TRACK_GAP }}
        aria-hidden
      >
        {tracks.map((track) => (
          <span
            key={track.id}
            className="flex w-24 items-center truncate pr-2 text-2xs text-text-muted"
            style={{ height: TRACK_HEIGHT }}
          >
            {track.label}
          </span>
        ))}
      </div>
      <figure className="min-w-0 flex-1">
        <canvas
          ref={canvasRef}
          className="w-full"
          style={{ height }}
          role="img"
          aria-label={`Activity loom: ${tracks.length} tracks`}
          onMouseMove={(event) => {
            const canvas = canvasRef.current;
            if (!canvas) return;
            const rect = canvas.getBoundingClientRect();
            setHover(
              pick(
                tracks,
                window_,
                rect.width,
                event.clientX - rect.left,
                event.clientY - rect.top,
              ),
            );
          }}
          onMouseLeave={() => setHover(null)}
        />
        <figcaption className="tabular h-5 truncate text-2xs text-text-muted">
          {hover
            ? `${hover.track.label} · ${hover.span.label} · ${new Date(hover.span.start * 1000).toLocaleString()}`
            : 'hover a mark for details · knowledge time'}
        </figcaption>
      </figure>
    </div>
  );
}
