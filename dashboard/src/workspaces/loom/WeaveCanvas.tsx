import { useEffect, useId, useMemo, useRef, useState } from 'react';
import { useSearchParams } from 'react-router';
import { axisTicks, clampWindow, fittedWindow, formatMoment, zoomWindow, type LoomWindow } from './tracks.ts';
import type { Weave } from './weave.ts';
import type { LoomPlaybackFrame } from './playback.ts';
import { kindColorVars } from '../../viz/graph/kindColor.ts';

// Packing scale for the bounded session overview. This is presentation geometry,
// not an event time or a claim that sessions have parent/child relations.
export const PLOT_WIDTH = 832;
export const MARK_PITCH_PX = 24;
const WIDTH = 960;
const LEFT = 100;
const RIGHT = 28;
const SPAN = WIDTH - LEFT - RIGHT;

export function WeaveCanvas({ weave, selectedId, onSelect, ariaLabel }: {
  weave: Weave;
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  ariaLabel: string;
}) {
  const clipId = useId();
  const [zoomed, setZoomed] = useState<LoomWindow | null>(null);
  const extent = weave.extent;
  const view = extent ? zoomed ?? fittedWindow(extent) : null;
  const rows = Math.max(weave.hosts.reduce((sum, host) => sum + host.lanes, 0), 1);
  const height = Math.max(300, Math.min(560, rows * 36 + 80));
  const x = (time: number) => LEFT + (time - view!.start) / (view!.end - view!.start) * SPAN;
  const lane = (column: number, offset: number) => weave.hosts.slice(0, column).reduce((sum, host) => sum + host.lanes, 0) + offset;
  const y = (row: number) => 65 + row / rows * (height - 100);
  const zoom = (factor: number) => {
    if (extent && view) setZoomed(zoomWindow(view, extent, factor, (view.start + view.end) / 2));
  };
  const pan = (direction: number) => {
    if (!extent || !view) return;
    const delta = (view.end - view.start) * .25 * direction;
    setZoomed(clampWindow({ start: view.start + delta, end: view.end + delta }, extent));
  };
  return <div className="min-w-0">
    <div role="toolbar" aria-label="Time window" className="flex flex-wrap items-center gap-2 border border-edge-subtle p-2 text-xs">
      <button className="td-hit" aria-label="Zoom in" onClick={() => zoom(.5)}>+</button>
      <button className="td-hit" aria-label="Zoom out" onClick={() => zoom(2)}>−</button>
      <button className="td-hit" aria-label="Pan to earlier sessions" disabled={!zoomed} onClick={() => pan(-1)}>←</button>
      <button className="td-hit" aria-label="Pan to later sessions" disabled={!zoomed} onClick={() => pan(1)}>→</button>
      <button className="td-hit" aria-label="Fit the whole extent" disabled={!zoomed} onClick={() => setZoomed(null)}>fit</button>
      <span>{!zoomed ? 'whole extent' : `${formatMoment(zoomed.start)} – ${formatMoment(zoomed.end)}`}</span>
    </div>
    <div className="td-optic">
      <svg role="img" aria-label={ariaLabel} width="100%" viewBox={`0 0 ${WIDTH} ${height}`}>
        <defs><clipPath id={clipId}><rect x={LEFT} y={35} width={SPAN} height={height - 40} /></clipPath></defs>
        {view && axisTicks(view, SPAN).map((tick) => <g key={tick.time}>
          <line x1={LEFT + tick.x} x2={LEFT + tick.x} y1={35} y2={height - 20} stroke="var(--raw-graph-edge)" opacity={.3} />
          <text x={LEFT + tick.x} y={22} textAnchor="middle" fill="var(--raw-graph-text)" fontSize={11}>{tick.label}</text>
        </g>)}
        {weave.hosts.map((host, index) => <text key={host.id} x={12} y={y(lane(index, 0)) + 4} fill="var(--raw-graph-text)" fontSize={11}>{host.label}</text>)}
        <g clipPath={`url(#${clipId})`}>
          {view && weave.threads.filter((thread) => thread.start <= view.end && (thread.end ?? thread.start) >= view.start).map((thread) => {
            const start = x(thread.start);
            const end = thread.end == null ? start + 18 : Math.max(start + 4, x(thread.end));
            const middle = y(lane(thread.column, thread.lane));
            const thickness = 2 + thread.weight * 7;
            const evidence = thread.endSource === 'session_end' ? 'var(--ev-measured)'
              : thread.endSource === 'last_message' ? 'var(--ev-associated)' : 'var(--ev-unknown)';
            return <g key={thread.id} data-thread={thread.id} style={kindColorVars(thread.host)} opacity={selectedId && selectedId !== thread.id ? .25 : 1} onClick={() => onSelect(thread.id)} className="cursor-pointer">
              <title>{thread.host} · {formatMoment(thread.start)} · {thread.messages} messages · {thread.endSource}</title>
              <line x1={start} x2={end} y1={middle} y2={middle} stroke="var(--kind-dark)" strokeWidth={thickness + 7} opacity={.12} />
              <line x1={start} x2={end} y1={middle} y2={middle} stroke={evidence} strokeWidth={thickness} strokeDasharray={thread.end == null ? '3 4' : undefined} />
              <circle cx={start} cy={middle} r={4} fill={thread.hollow ? 'var(--raw-graph-bg)' : 'var(--kind-dark)'} stroke="var(--kind-dark)" />
              <rect x={start - 8} y={middle - 14} width={Math.max(end - start + 16, 24)} height={28} fill="transparent" />
            </g>;
          })}
        </g>
      </svg>
    </div>
  </div>;
}

/** Canonical source positions; both the field and minimap use this projection. */
export function eventPositions(frames: readonly LoomPlaybackFrame[]) {
  const times = frames.flatMap((frame) => frame.timestamp == null ? [] : [frame.timestamp]);
  const start = times.length ? Math.min(...times) : null;
  const end = times.length ? Math.max(...times) : null;
  return {
    start, end,
    points: frames.map((frame, index) => ({
      id: frame.id,
      x: frame.timestamp != null && start != null && end != null
        ? end === start ? .5 : (frame.timestamp - start) / (end - start)
        : frames.length <= 1 ? .5 : index / (frames.length - 1),
      y: frame.timestamp == null ? 225 : 115,
      frame,
    })),
  };
}

export function LoadedEventCanvas({ frames, visible, activeId, onSelect, onInspect }: {
  frames: readonly LoomPlaybackFrame[];
  visible: readonly LoomPlaybackFrame[];
  activeId: string | null;
  onSelect: (id: string) => void;
  onInspect: () => void;
}) {
  const clipId = useId();
  const host = useRef<HTMLElement>(null);
  const [width, setWidth] = useState(960);
  useEffect(() => {
    if (!host.current || typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(([entry]) => {
      if (entry && entry.contentRect.width > 0) setWidth(entry.contentRect.width);
    });
    observer.observe(host.current);
    return () => observer.disconnect();
  }, []);
  const left = width < 480 ? 28 : 100;
  const spanPx = Math.max(width - left - RIGHT, 1);
  const [params, setParams] = useSearchParams();
  const raw = params.get('loomWindow')?.split(',').map(Number);
  const window = raw?.length === 2 && raw.every(Number.isFinite) && raw[0]! >= 0 && raw[1]! <= 1 && raw[1]! > raw[0]!
    ? { start: raw[0]!, end: raw[1]! } : { start: 0, end: 1 };
  const geometry = useMemo(() => eventPositions(frames), [frames]);
  const ids = new Set(visible.map((frame) => frame.id));
  const revealed = geometry.points.filter((point) => ids.has(point.id));
  const sequences = geometry.points.slice(1).flatMap((point, index) => {
    const previous = geometry.points[index]!;
    return ids.has(previous.id) && ids.has(point.id) ? [{ previous, point }] : [];
  });
  const points = revealed.filter((point) => point.x >= window.start && point.x <= window.end);
  const hasUndated = revealed.some((point) => point.frame.timestamp == null);
  const laneY = hasUndated ? [115, 225] : [115];
  const active = revealed.find((point) => point.id === activeId);
  const x = (position: number) => left + (position - window.start) / (window.end - window.start) * spanPx;
  const move = (start: number, end: number) => {
    // One URL update also suspends tail-follow; separate updates could race and
    // lose either the cursor or the viewport under React Router batching.
    const search = new URLSearchParams(params);
    if (activeId) search.set('loomEvent', activeId);
    search.set('loomWindow', `${Math.max(0, start)},${Math.min(1, end)}`);
    setParams(search, { replace: true });
    onInspect();
  };
  const span = window.end - window.start;
  const orderPath = (previous: typeof geometry.points[number], point: typeof geometry.points[number], projectX: (position: number) => number, scaleY = 1) => {
    const mid = (projectX(previous.x) + projectX(point.x)) / 2;
    return `M ${projectX(previous.x)} ${previous.y * scaleY} C ${mid} ${previous.y * scaleY}, ${mid} ${point.y * scaleY}, ${projectX(point.x)} ${point.y * scaleY}`;
  };
  return <section ref={host} aria-label="Loaded execution field" className="min-w-0">
    <div className="flex flex-wrap gap-2 border border-edge-subtle p-2 text-xs" role="toolbar" aria-label="Execution viewport">
      <button className="td-hit" aria-label="Zoom into execution" disabled={span < .02} onClick={() => move(window.start + span / 4, window.end - span / 4)}>+ Zoom</button>
      <button className="td-hit" aria-label="Fit loaded execution" onClick={() => move(0, 1)}>Fit</button>
      <button className="td-hit" aria-label="Pan execution earlier" disabled={window.start === 0} onClick={() => { const start = Math.max(0, window.start - span / 4); move(start, start + span); }}>← Older</button>
      <button className="td-hit" aria-label="Pan execution later" disabled={window.end === 1} onClick={() => { const end = Math.min(1, window.end + span / 4); move(end - span, end); }}>Later →</button>
    </div>
    {width < 480 && <p className="text-xs text-text-muted">{geometry.start == null ? 'No source timestamps' : formatMoment(geometry.start)} → {geometry.end == null ? 'loaded source order' : `${formatMoment(geometry.end)} · loaded end`}</p>}
    <div className="td-optic">
      <svg role="group" aria-label="Revealed execution events" width="100%" viewBox={`0 0 ${width} ${hasUndated ? 295 : 235}`}>
        <defs><clipPath id={clipId}><rect x={left - 12} y={40} width={spanPx + 24} height={240} /></clipPath></defs>
        <text visibility={width < 480 ? "hidden" : "visible"} x={left} y={24} fill="var(--raw-graph-text)" fontSize={12}>{geometry.start == null ? 'No source timestamps' : formatMoment(geometry.start)}</text>
        <text visibility={width < 480 ? "hidden" : "visible"} x={width - RIGHT} y={24} textAnchor="end" fill="var(--raw-graph-text)" fontSize={12}>{geometry.end == null ? 'Loaded source order' : `${formatMoment(geometry.end)} · LOADED END`}</text>
        <text x={12} y={85} fill="var(--raw-graph-text)" fontSize={11}>Recorded time</text>
        {hasUndated && <text x={12} y={195} fill="var(--raw-graph-text)" fontSize={11}>Undated</text>}
        {hasUndated && <text x={12} y={210} fill="var(--raw-graph-text)" fontSize={10}>source order</text>}
        {laneY.map((y) => <line key={y} x1={left} x2={width - RIGHT} y1={y} y2={y} stroke="var(--raw-graph-edge)" strokeDasharray="2 5" />)}
        <g clipPath={`url(#${clipId})`}>
          {sequences.map(({ previous, point }) => {
            const path = orderPath(previous, point, x);
            return <g key={`${previous.id}:${point.id}`} aria-label="Recorded message order">
              <path d={path} fill="none" stroke="#58daec" strokeWidth={8} opacity={.08} />
              <path d={path} fill="none" stroke="#58daec" strokeWidth={1} opacity={.6} />
            </g>;
          })}
          {active && <line x1={x(active.x)} x2={x(active.x)} y1={42} y2={270} stroke="var(--raw-graph-text)" strokeWidth={1} opacity={.55} />}
          {points.map((point) => <g key={point.id} data-event={point.id} role="button" tabIndex={0} aria-label={`Select stored event ${point.id}`} aria-pressed={point.id === activeId} onClick={() => onSelect(point.id)} onKeyDown={(event) => { if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); onSelect(point.id); } }} className="cursor-pointer outline-none [&:focus>rect]:stroke-white">
            <rect x={x(point.x) - 22} y={point.y - 22} width={44} height={70} fill="transparent" />
            <title>{point.frame.role} · {point.frame.tool ?? 'message'} · {point.id}</title>
            <circle cx={x(point.x)} cy={point.y} r={point.id === activeId ? 19 : 12} fill="#35d6ee" opacity={.08} />
            <circle cx={x(point.x)} cy={point.y} r={point.id === activeId ? 10 : 6} fill="var(--raw-graph-bg)" stroke="#58daec" strokeWidth={point.id === activeId ? 2 : 1} />
            <circle cx={x(point.x)} cy={point.y} r={2} fill="#c1f8ff" />
            <text x={x(point.x)} y={point.y + 35} textAnchor="middle" fill="var(--raw-graph-text)" fontSize={11}>{points.length <= 12 || point.id === activeId ? point.frame.tool ?? point.frame.role : ''}</text>
          </g>)}
        </g>
      </svg>
      <svg role="group" aria-label="Execution minimap" width="100%" viewBox={`0 0 ${width} 64`}>
        {laneY.map((y) => <line key={y} x1={left} x2={width - RIGHT} y1={y / 5} y2={y / 5} stroke="var(--raw-graph-edge)" />)}
        {sequences.map(({ previous, point }) => <path key={`${previous.id}:${point.id}`} d={orderPath(previous, point, (position) => left + position * spanPx, .2)} fill="none" stroke="#58daec" opacity={.5} />)}
        {revealed.map((point) => <circle key={point.id} data-minimap-event={point.id} cx={left + point.x * spanPx} cy={point.y / 5} r={point.id === activeId ? 4 : 2} fill="#58daec" />)}
        <rect x={left + window.start * spanPx} y={8} width={span * spanPx} height={48} fill="none" stroke="var(--raw-graph-text)" />
      </svg>
    </div>
    <label className="flex flex-wrap items-center gap-2 text-xs text-text-muted">Minimap position
      <input type="range" aria-label="Minimap viewport" min={0} max={1 - span} step={.001} value={window.start} disabled={span === 1} onChange={(event) => move(Number(event.currentTarget.value), Number(event.currentTarget.value) + span)} />
    </label>
    <p className="text-3xs text-text-muted">Each dot is a stored message. Placement measures time, or explicitly undated source order; thin connections show canonical message order, not causal attribution. Proximity is not a causal edge. Exact source selection remains available in the sequence below.</p>
  </section>;
}
