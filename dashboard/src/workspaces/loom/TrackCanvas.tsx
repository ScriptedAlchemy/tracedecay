import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { ChevronRight, Minus, Plus, Maximize2 } from 'lucide-react';
import { cn } from '../../ui/cn';
import {
  AXIS_HEIGHT,
  DAY_BAND_HEIGHT,
  LANE_GAP,
  LANE_HEIGHT,
  MINIMAP_HEIGHT,
  MIN_MARK_PX,
  TRACK_PAD,
  axisTicks,
  clampWindow,
  dayBands,
  densityProfile,
  fittedWindow,
  formatDuration,
  formatMoment,
  formatWindowSpan,
  isFitted,
  layoutHeight,
  layoutTracks,
  peakConcurrency,
  pick,
  timeFor,
  windowOf,
  xFor,
  zoomWindow,
  type LoomSpan,
  type LoomTrack,
  type LoomWindow,
  type TrackLayout,
} from './tracks.ts';

/** Reads the resolved theme tokens the canvas needs. Canvas cannot consume CSS
 * variables directly, so draws re-sample on every theme flip. Canvas 2D parses
 * the `lab(...)` these oklch tokens resolve to natively. */
function paletteFrom(element: HTMLElement) {
  const style = getComputedStyle(element);
  const token = (name: string, fallback: string) =>
    style.getPropertyValue(name).trim() || fallback;
  return {
    axis: token('--raw-text-muted', '#888'),
    axisStrong: token('--raw-text-secondary', '#aaa'),
    grid: token('--raw-edge-subtle', '#333'),
    edge: token('--raw-edge-strong', '#555'),
    band: token('--raw-surface-2', '#222'),
    mark: token('--raw-accent', '#7aa2f7'),
    markHot: token('--raw-accent-emphasis', '#4f7de0'),
    now: token('--raw-state-ready', '#4ade80'),
  };
}

type Selection = { track: LoomTrack; span: LoomSpan } | null;

/** Loom track canvas: a packed, zoomable knowledge-time weave. Fixed-height
 * sub-lanes per provider, a two-tier calendar axis, a density overview with a
 * draggable viewport brush, and hover inspection that reports below the plot
 * rather than on top of it. Device-pixel aware; picking is done against the
 * same pure layout the renderer draws. */
export function TrackCanvas({ tracks }: { tracks: LoomTrack[] }) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const paneRef = useRef<HTMLDivElement | null>(null);
  const [width, setWidth] = useState(0);
  const [paneHeight, setPaneHeight] = useState(0);
  const [hover, setHover] = useState<Selection>(null);
  const [pinned, setPinned] = useState<Selection>(null);
  const [cursorTime, setCursorTime] = useState<number | null>(null);
  const extent = useMemo(() => windowOf(tracks), [tracks]);
  const [view, setView] = useState<LoomWindow | null>(null);

  // Adopt the fitted extent whenever the data's own bounds change.
  const extentKey = extent ? `${extent.start}-${extent.end}` : 'none';
  useEffect(() => {
    setView(extent ? fittedWindow(extent) : null);
    setPinned(null);
    setHover(null);
  }, [extentKey]);

  const activeView = view ?? (extent ? fittedWindow(extent) : null);
  const layouts = useMemo(
    () => (activeView ? layoutTracks(tracks, activeView, width, paneHeight) : []),
    [tracks, activeView?.start, activeView?.end, width, paneHeight],
  );
  const plotHeight = layoutHeight(layouts);
  const height = AXIS_HEIGHT + plotHeight;
  const maxWeight = useMemo(
    () =>
      Math.max(
        1,
        ...tracks.flatMap((track) => track.spans.map((span) => span.weight)),
      ),
    [tracks],
  );
  const focus = hover ?? pinned;

  const zoomBy = useCallback(
    (factor: number) => {
      if (!extent || !activeView) return;
      const centre = (activeView.start + activeView.end) / 2;
      setView(zoomWindow(activeView, extent, factor, centre));
    },
    [extent, activeView?.start, activeView?.end],
  );

  /* ---------------- draw ---------------- */
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !activeView || width <= 0 || height <= 0) return;
    const draw = () => {
      const context = canvas.getContext('2d');
      if (!context) return;
      const ratio = globalThis.devicePixelRatio || 1;
      canvas.width = Math.round(width * ratio);
      canvas.height = Math.round(height * ratio);
      context.setTransform(ratio, 0, 0, ratio, 0, 0);
      context.clearRect(0, 0, width, height);
      const palette = paletteFrom(canvas);
      const bands = dayBands(activeView, width);
      const ticks = axisTicks(activeView, width);

      /* Calendar bands: the weave's warp. Alternating days tint the whole
       * plot so the eye reads rhythm before it reads any single mark. */
      context.fillStyle = palette.band;
      for (const band of bands) {
        if (!band.odd) continue;
        context.globalAlpha = 0.45;
        context.fillRect(band.x0, AXIS_HEIGHT, Math.max(band.x1 - band.x0, 0), plotHeight);
        context.globalAlpha = 1;
      }

      /* Day-band header row. */
      context.font =
        '600 10px Inter Variable, Inter, system-ui, sans-serif';
      context.textBaseline = 'middle';
      for (const band of bands) {
        const bandWidth = band.x1 - band.x0;
        context.strokeStyle = palette.grid;
        context.globalAlpha = 0.9;
        context.beginPath();
        context.moveTo(Math.round(band.x0) + 0.5, 0);
        context.lineTo(Math.round(band.x0) + 0.5, AXIS_HEIGHT + plotHeight);
        context.stroke();
        context.globalAlpha = 1;
        if (bandWidth < 52) continue;
        context.fillStyle = palette.axisStrong;
        context.save();
        context.beginPath();
        context.rect(band.x0 + 4, 0, bandWidth - 8, DAY_BAND_HEIGHT);
        context.clip();
        context.fillText(band.label, band.x0 + 5, DAY_BAND_HEIGHT / 2);
        context.restore();
      }

      /* Fine tick row + plot grid. */
      context.font = '10px Inter Variable, Inter, system-ui, sans-serif';
      for (const tick of ticks) {
        context.strokeStyle = palette.grid;
        context.globalAlpha = 0.5;
        context.beginPath();
        context.moveTo(Math.round(tick.x) + 0.5, AXIS_HEIGHT - 4);
        context.lineTo(Math.round(tick.x) + 0.5, AXIS_HEIGHT + plotHeight);
        context.stroke();
        context.globalAlpha = 1;
        context.fillStyle = palette.axis;
        context.fillText(tick.label, tick.x + 4, AXIS_HEIGHT - 9);
      }

      /* Axis baseline. */
      context.strokeStyle = palette.edge;
      context.beginPath();
      context.moveTo(0, AXIS_HEIGHT - 0.5);
      context.lineTo(width, AXIS_HEIGHT - 0.5);
      context.stroke();

      /* Marks. Height and value both carry message volume on one hue; the
       * bright cap is the moment the session stopped. */
      for (const layout of layouts) {
        if (layout.top > 0) {
          context.strokeStyle = palette.grid;
          context.globalAlpha = 0.7;
          context.beginPath();
          context.moveTo(0, AXIS_HEIGHT + layout.top + 0.5);
          context.lineTo(width, AXIS_HEIGHT + layout.top + 0.5);
          context.stroke();
          context.globalAlpha = 1;
        }
        layout.lanes.forEach((lane, laneIndex) => {
          const laneTop =
            AXIS_HEIGHT +
            layout.top +
            TRACK_PAD +
            laneIndex * (layout.laneHeight + LANE_GAP);
          for (const span of lane) {
            const x0 = xFor(span.start, activeView, width);
            const x1 = Math.max(xFor(span.end, activeView, width), x0 + MIN_MARK_PX);
            if (x1 < -8 || x0 > width + 8) continue;
            const magnitude = Math.sqrt(Math.min(span.weight / maxWeight, 1));
            const barHeight = Math.max(5, (layout.laneHeight - 3) * magnitude);
            const top = laneTop + (layout.laneHeight - barHeight) / 2;
            const isFocus = focus?.span.id === span.id;
            const dimmed = focus != null && !isFocus;
            context.fillStyle = isFocus ? palette.markHot : palette.mark;
            context.globalAlpha = dimmed ? 0.22 : 0.42 + 0.5 * magnitude;
            context.beginPath();
            context.roundRect(x0, top, x1 - x0, barHeight, 2);
            context.fill();
            context.globalAlpha = dimmed ? 0.3 : 0.95;
            context.fillRect(Math.max(x1 - 1.5, x0), top, 1.5, barHeight);
            if (isFocus) {
              context.globalAlpha = 1;
              context.strokeStyle = palette.markHot;
              context.lineWidth = 1;
              context.beginPath();
              context.roundRect(x0 - 1.5, top - 1.5, x1 - x0 + 3, barHeight + 3, 3);
              context.stroke();
            }
            context.globalAlpha = 1;
          }
        });
      }

      /* Crosshair: a hairline at the pointer with its time read in the axis,
       * so inspection never covers the mark being inspected. */
      if (cursorTime != null) {
        const x = xFor(cursorTime, activeView, width);
        if (x >= 0 && x <= width) {
          context.strokeStyle = palette.edge;
          context.setLineDash([2, 3]);
          context.beginPath();
          context.moveTo(Math.round(x) + 0.5, AXIS_HEIGHT);
          context.lineTo(Math.round(x) + 0.5, AXIS_HEIGHT + plotHeight);
          context.stroke();
          context.setLineDash([]);
        }
      }

      /* Now. Only drawn when the present is actually in frame. */
      const now = Date.now() / 1000;
      if (now >= activeView.start && now <= activeView.end) {
        const x = xFor(now, activeView, width);
        context.strokeStyle = palette.now;
        context.globalAlpha = 0.85;
        context.beginPath();
        context.moveTo(Math.round(x) + 0.5, AXIS_HEIGHT - DAY_BAND_HEIGHT / 2);
        context.lineTo(Math.round(x) + 0.5, AXIS_HEIGHT + plotHeight);
        context.stroke();
        context.fillStyle = palette.now;
        context.beginPath();
        context.moveTo(x - 3.5, AXIS_HEIGHT - 4);
        context.lineTo(x + 3.5, AXIS_HEIGHT - 4);
        context.lineTo(x, AXIS_HEIGHT + 1);
        context.closePath();
        context.fill();
        context.globalAlpha = 1;
      }
    };
    draw();
    const observer = new MutationObserver(draw);
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme', 'data-contrast'],
    });
    return () => observer.disconnect();
  }, [layouts, activeView?.start, activeView?.end, width, height, focus?.span.id, cursorTime, maxWeight]);

  /* ---------------- measurement ----------------
   * Width comes from the canvas; height comes from the scrolling pane that
   * *contains* it, so growing the canvas can never feed back into the
   * measurement that sized it. */
  useEffect(() => {
    const canvas = canvasRef.current;
    const pane = paneRef.current;
    if (!canvas || !pane) return;
    const measure = () => {
      setWidth(canvas.clientWidth);
      setPaneHeight(pane.clientHeight);
    };
    measure();
    const resize = new ResizeObserver(measure);
    resize.observe(canvas);
    resize.observe(pane);
    return () => resize.disconnect();
  }, []);

  /* ---------------- pointer: hover picking + drag to pan ---------------- */
  const dragRef = useRef<{ x: number; view: LoomWindow; moved: boolean } | null>(null);

  const onPointerMove = (event: React.PointerEvent<HTMLCanvasElement>) => {
    const canvas = canvasRef.current;
    if (!canvas || !activeView || !extent) return;
    const rect = canvas.getBoundingClientRect();
    const x = event.clientX - rect.left;
    const drag = dragRef.current;
    if (drag) {
      if (Math.abs(x - drag.x) > 2) drag.moved = true;
      const seconds =
        ((drag.x - x) / rect.width) * (drag.view.end - drag.view.start);
      setView(
        clampWindow(
          { start: drag.view.start + seconds, end: drag.view.end + seconds },
          extent,
        ),
      );
      return;
    }
    setCursorTime(timeFor(x, activeView, rect.width));
    setHover(pick(layouts, activeView, rect.width, x, event.clientY - rect.top));
  };

  return (
    <figure className="flex min-h-0 min-w-0 flex-1 flex-col gap-2">
      <ViewportToolbar
        view={activeView}
        fitted={!extent || !activeView || isFitted(activeView, extent)}
        onZoomIn={() => zoomBy(0.5)}
        onZoomOut={() => zoomBy(2)}
        onFit={() => setView(extent ? fittedWindow(extent) : null)}
      />

      <div
        ref={paneRef}
        className="flex min-h-52 min-w-0 flex-1 overflow-auto rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1"
      >
        <TrackGutter layouts={layouts} />
        <div className="min-w-0 flex-1">
          <canvas
            ref={canvasRef}
            className="block w-full cursor-crosshair touch-none select-none"
            style={{ height: Math.max(height, AXIS_HEIGHT + LANE_HEIGHT) }}
            role="img"
            aria-label={canvasSummary(tracks, activeView)}
            onPointerDown={(event) => {
              if (!activeView) return;
              const rect = event.currentTarget.getBoundingClientRect();
              dragRef.current = {
                x: event.clientX - rect.left,
                view: activeView,
                moved: false,
              };
              event.currentTarget.setPointerCapture(event.pointerId);
            }}
            onPointerMove={onPointerMove}
            onPointerUp={(event) => {
              const drag = dragRef.current;
              dragRef.current = null;
              event.currentTarget.releasePointerCapture(event.pointerId);
              if (drag && !drag.moved) setPinned(hover);
            }}
            onPointerLeave={() => {
              dragRef.current = null;
              setHover(null);
              setCursorTime(null);
            }}
          />
        </div>
      </div>

      {extent && activeView ? (
        <Minimap
          tracks={tracks}
          extent={extent}
          view={activeView}
          onChange={(next) => setView(clampWindow(next, extent))}
        />
      ) : null}

      <figcaption className="sr-only">
        {canvasSummary(tracks, activeView)}. Drag the weave to pan, drag the overview
        strip below it to choose a window, or use the zoom controls. Every session is
        also listed in the session ledger table.
      </figcaption>

      <InspectorStrip focus={focus} cursorTime={cursorTime} pinned={pinned != null} />
      <SpanLedger tracks={tracks} focusId={focus?.span.id ?? null} onFocus={setPinned} />
    </figure>
  );
}

function canvasSummary(tracks: LoomTrack[], view: LoomWindow | null): string {
  const spans = tracks.reduce((sum, track) => sum + track.spans.length, 0);
  const window_ = view
    ? `, window ${formatMoment(view.start)} to ${formatMoment(view.end)}`
    : '';
  return `Knowledge-time weave: ${spans} sessions across ${tracks.length} provider ${
    tracks.length === 1 ? 'track' : 'tracks'
  }${window_}`;
}

/* ------------------------------------------------------------------ */

function ViewportToolbar({
  view,
  fitted,
  onZoomIn,
  onZoomOut,
  onFit,
}: {
  view: LoomWindow | null;
  fitted: boolean;
  onZoomIn: () => void;
  onZoomOut: () => void;
  onFit: () => void;
}) {
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
      <span className="flex items-stretch text-2xs">
        <span aria-hidden className="w-1.5 border-y border-l border-accent/40" />
        <span className="tabular px-2 py-0.5 text-text-secondary">
          {view ? `${formatMoment(view.start)} → ${formatMoment(view.end)}` : '—'}
        </span>
        <span aria-hidden className="w-1.5 border-y border-r border-accent/40" />
      </span>
      <span className="tabular text-2xs uppercase tracking-wider text-text-muted">
        {view ? formatWindowSpan(view) : '—'} in frame
      </span>
      <div className="ml-auto flex items-center gap-1">
        <ToolbarButton label="Zoom out" onClick={onZoomOut}>
          <Minus aria-hidden size={13} />
        </ToolbarButton>
        <ToolbarButton label="Zoom in" onClick={onZoomIn}>
          <Plus aria-hidden size={13} />
        </ToolbarButton>
        <ToolbarButton label="Fit all sessions" onClick={onFit} disabled={fitted}>
          <Maximize2 aria-hidden size={12} />
        </ToolbarButton>
      </div>
    </div>
  );
}

function ToolbarButton({
  label,
  onClick,
  disabled,
  children,
}: {
  label: string;
  onClick: () => void;
  disabled?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      aria-label={label}
      title={label}
      className={cn(
        'flex size-6 items-center justify-center rounded-[var(--radius-chip)] border border-edge-subtle text-text-secondary',
        'transition-colors duration-[var(--dur-state)] hover:border-edge-strong hover:text-text-primary',
        'disabled:opacity-40',
      )}
    >
      {children}
    </button>
  );
}

/** Left gutter: one label block per track, aligned to its packed band. Reports
 * how many lanes the track needed — i.e. how concurrent that provider is. */
function TrackGutter({ layouts }: { layouts: TrackLayout[] }) {
  return (
    <div
      className="sticky left-0 z-10 w-24 shrink-0 border-r border-edge-subtle bg-surface-1 sm:w-32"
      style={{ paddingTop: AXIS_HEIGHT }}
      aria-hidden
    >
      {layouts.map((layout) => {
        const overlap = peakConcurrency(layout.track);
        return (
          <div
            key={layout.track.id}
            className="flex flex-col justify-center overflow-hidden border-t border-edge-subtle px-2 first:border-t-0"
            style={{ height: layout.height }}
          >
            <span className="truncate text-2xs font-medium text-text-secondary">
              {layout.track.label}
            </span>
            <span className="tabular truncate text-2xs text-text-muted">
              {layout.track.spans.length} sessions
              {overlap > 1 ? ` · ×${overlap} deep` : ''}
            </span>
          </div>
        );
      })}
    </div>
  );
}

/** Overview strip: the whole extent's weight profile with the viewport drawn
 * over it. Drag anywhere on it to choose a window — the pan/zoom affordance
 * that needs no scroll hijacking and works on touch. */
function Minimap({
  tracks,
  extent,
  view,
  onChange,
}: {
  tracks: LoomTrack[];
  extent: LoomWindow;
  view: LoomWindow;
  onChange: (view: LoomWindow) => void;
}) {
  const full = fittedWindow(extent);
  const buckets = 120;
  const profile = useMemo(
    () => densityProfile(tracks, full, buckets),
    [tracks, full.start, full.end],
  );
  const peak = Math.max(...profile, 1);
  const dragRef = useRef<{ anchor: number; moved: boolean } | null>(null);
  const span = view.end - view.start;
  const x0 = ((view.start - full.start) / (full.end - full.start)) * 100;
  const x1 = ((view.end - full.start) / (full.end - full.start)) * 100;

  const timeAt = (event: React.PointerEvent<HTMLDivElement>): number => {
    const rect = event.currentTarget.getBoundingClientRect();
    const ratio = (event.clientX - rect.left) / Math.max(rect.width, 1);
    return full.start + ratio * (full.end - full.start);
  };

  const path = useMemo(() => {
    const steps = profile.map((value, index) => {
      const x = (index / Math.max(profile.length - 1, 1)) * 100;
      const y = 100 - (value / peak) * 92;
      return `${index === 0 ? 'M' : 'L'}${x.toFixed(2)} ${y.toFixed(2)}`;
    });
    return `${steps.join(' ')} L100 100 L0 100 Z`;
  }, [profile, peak]);

  return (
    <div
      className="relative cursor-ew-resize overflow-hidden rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1"
      style={{ height: MINIMAP_HEIGHT }}
      onPointerDown={(event) => {
        dragRef.current = { anchor: timeAt(event), moved: false };
        event.currentTarget.setPointerCapture(event.pointerId);
        const centre = timeAt(event);
        onChange({ start: centre - span / 2, end: centre + span / 2 });
      }}
      onPointerMove={(event) => {
        if (!dragRef.current) return;
        const anchor = dragRef.current.anchor;
        const now = timeAt(event);
        dragRef.current.moved = true;
        const start = Math.min(anchor, now);
        const end = Math.max(anchor, now);
        // A real drag selects a range; a tap recentres the current one.
        if (end - start > (full.end - full.start) / 200) onChange({ start, end });
        else onChange({ start: now - span / 2, end: now + span / 2 });
      }}
      onPointerUp={(event) => {
        dragRef.current = null;
        event.currentTarget.releasePointerCapture(event.pointerId);
      }}
    >
      <svg
        className="absolute inset-0 size-full"
        viewBox="0 0 100 100"
        preserveAspectRatio="none"
        aria-hidden
      >
        <path d={path} fill="var(--raw-accent)" fillOpacity={0.28} />
      </svg>
      <div
        aria-hidden
        className="absolute inset-y-0 border-x border-accent bg-accent/12"
        style={{ left: `${x0}%`, width: `${Math.max(x1 - x0, 0.6)}%` }}
      />
      <span className="pointer-events-none absolute right-2 top-1/2 -translate-y-1/2 text-2xs uppercase tracking-wider text-text-muted">
        full extent
      </span>
    </div>
  );
}

/** The readout. Fixed height and fixed position below the plot: inspecting a
 * mark never occludes its neighbours, and nothing reflows on hover. */
function InspectorStrip({
  focus,
  cursorTime,
  pinned,
}: {
  focus: { track: LoomTrack; span: LoomSpan } | null;
  cursorTime: number | null;
  pinned: boolean;
}) {
  const duration = focus ? Math.max(focus.span.end - focus.span.start, 0) : 0;
  const rate =
    focus && duration > 0 ? (focus.span.weight / (duration / 3600)).toFixed(1) : null;
  return (
    <div
      aria-live="polite"
      className="min-h-16 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 px-3 py-2"
    >
      {focus ? (
        <>
          <dl className="grid grid-cols-2 gap-x-4 gap-y-1 sm:grid-cols-4">
            <Readout label={pinned ? 'session · pinned' : 'session'}>
              <span className="truncate font-mono">{focus.span.label}</span>
            </Readout>
            <Readout label="provider">{focus.track.label}</Readout>
            <Readout label="span">
              {formatDuration(duration)}
              {rate ? <span className="ml-1 text-text-muted">· {rate}/h</span> : null}
            </Readout>
            <Readout label="messages">{focus.span.weight.toLocaleString()}</Readout>
          </dl>
          <p className="tabular mt-1 text-2xs text-text-muted">
            {formatMoment(focus.span.start)} → {formatMoment(focus.span.end)}
          </p>
        </>
      ) : (
        <>
          <dl className="grid grid-cols-2 gap-x-4 gap-y-1 sm:grid-cols-4">
            <Readout label="pointer">
              {cursorTime != null ? formatMoment(cursorTime) : 'off the weave'}
            </Readout>
          </dl>
          <p className="mt-1 text-2xs text-text-muted">
            Hover a mark to read it here; click to pin. Drag the weave to pan, or drag
            the overview strip to choose a window.
          </p>
        </>
      )}
    </div>
  );
}

function Readout({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex min-w-0 flex-col">
      <dt className="text-2xs uppercase tracking-wider text-text-muted">{label}</dt>
      <dd className="tabular min-w-0 truncate text-xs text-text-primary">{children}</dd>
    </div>
  );
}

/** The canvas's text equivalent: every drawn mark as a real, readable,
 * keyboard-reachable row. Selecting a row pins it in the plot. */
function SpanLedger({
  tracks,
  focusId,
  onFocus,
}: {
  tracks: LoomTrack[];
  focusId: string | null;
  onFocus: (selection: { track: LoomTrack; span: LoomSpan }) => void;
}) {
  const rows = tracks
    .flatMap((track) => track.spans.map((span) => ({ track, span })))
    .sort((a, b) => b.span.end - a.span.end);
  return (
    <details className="group shrink-0 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1">
      <summary className="flex cursor-pointer list-none items-center gap-1.5 px-3 py-2 text-2xs uppercase tracking-wider text-text-muted marker:content-none hover:text-text-secondary">
        <ChevronRight
          aria-hidden
          size={12}
          className="transition-transform duration-[var(--dur-state)] group-open:rotate-90"
        />
        Session ledger · {rows.length} rows
      </summary>
      <div className="max-h-72 overflow-auto border-t border-edge-subtle">
        <table className="w-full border-collapse text-2xs">
          <caption className="sr-only">
            Every session drawn on the weave, newest first, with its provider,
            start, duration and message count.
          </caption>
          <thead className="sticky top-0 bg-surface-2 text-text-muted">
            <tr>
              <th scope="col" className="px-3 py-1.5 text-left font-medium">
                session
              </th>
              <th scope="col" className="px-3 py-1.5 text-left font-medium">
                provider
              </th>
              <th scope="col" className="px-3 py-1.5 text-left font-medium">
                started
              </th>
              <th scope="col" className="px-3 py-1.5 text-right font-medium">
                span
              </th>
              <th scope="col" className="px-3 py-1.5 text-right font-medium">
                messages
              </th>
            </tr>
          </thead>
          <tbody>
            {rows.map(({ track, span }) => (
              <tr
                key={span.id}
                className={cn(
                  'border-t border-edge-subtle',
                  focusId === span.id && 'bg-accent/10',
                )}
              >
                <th scope="row" className="max-w-0 px-3 py-1 text-left font-normal">
                  <button
                    type="button"
                    onClick={() => onFocus({ track, span })}
                    className="block w-full truncate text-left font-mono text-text-secondary hover:text-text-primary"
                  >
                    {span.id}
                  </button>
                </th>
                <td className="px-3 py-1 text-text-muted">{track.label}</td>
                <td className="tabular px-3 py-1 text-text-muted">
                  {formatMoment(span.start)}
                </td>
                <td className="tabular px-3 py-1 text-right text-text-muted">
                  {formatDuration(Math.max(span.end - span.start, 0))}
                </td>
                <td className="tabular px-3 py-1 text-right text-text-secondary">
                  {span.weight.toLocaleString()}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </details>
  );
}
