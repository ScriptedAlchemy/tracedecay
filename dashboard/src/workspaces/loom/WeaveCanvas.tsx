import type { KeyboardEvent, ReactNode } from 'react';
import { useEffect, useId, useMemo, useRef, useState } from 'react';
import { useSearchParams } from 'react-router';
import { axisTicks, clampWindow, fittedWindow, formatMoment, zoomWindow, type LoomWindow } from './tracks.ts';
import type { Weave } from './weave.ts';
import type { LoomPlaybackFrame } from './playback.ts';
import type { AnalyticsSubagentTreePayloadV1 } from '../../contracts/generated.ts';
import { kindColorVars } from '../../viz/graph/kindColor.ts';

// Packing scale for the bounded session overview. This is presentation geometry,
// not an event time or a claim that sessions have parent/child relations.
export const PLOT_WIDTH = 832;
export const MARK_PITCH_PX = 24;
const WIDTH = 960;
const RIGHT = 28;

export function WeaveCanvas({ weave, selectedId, onSelect, ariaLabel, hierarchy, initialWindow, onWindowChange }: {
  weave: Weave;
  hierarchy?: AnalyticsSubagentTreePayloadV1;
  initialWindow?: LoomWindow | null;
  onWindowChange?: (window: LoomWindow | null) => void;
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  ariaLabel: string;
}) {
  const clipId = useId();
  const field = useRef<HTMLDivElement>(null);
  const [vertical, setVertical] = useState({ start: 0, fraction: 1 });
  const measureViewport = () => {
    const element = field.current;
    if (element && element.scrollHeight > 0) setVertical({ start: element.scrollTop / element.scrollHeight, fraction: element.clientHeight / element.scrollHeight });
  };
  const [zoomed, updateWindow] = useState<LoomWindow | null>(initialWindow ?? null);
  const setZoomed = (window: LoomWindow | null) => { updateWindow(window); onWindowChange?.(window); };
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set());
  const extent = weave.extent;
  const view = extent ? zoomed ? clampWindow(zoomed, extent) : fittedWindow(extent) : null;
  const nodes = hierarchy?.available && !hierarchy.error ? hierarchy.nodes : [];
  const nodeById = new Map(nodes.map((node) => [JSON.stringify([node.provider, node.session_id]), node]));
  const threadById = new Map(weave.threads.map((thread) => [thread.id, thread]));
  const parentById = new Map(nodes.flatMap((node) => {
    const childId = JSON.stringify([node.provider, node.session_id]);
    const parentId = JSON.stringify([node.provider, node.parent_session_id]);
    return node.link === 'linked' && threadById.has(childId) && threadById.has(parentId)
      ? [[childId, parentId] as const] : [];
  }));
  const ancestors = (id: string) => {
    const visited = new Set<string>();
    let parent = parentById.get(id);
    while (parent && !visited.has(parent)) {
      visited.add(parent);
      parent = parentById.get(parent);
    }
    return [...visited];
  };
  // The canonical tree is already preorder. Retain it without changing any
  // source timestamp; unrelated sessions keep the temporal read's ordering.
  const ordered = [...new Set([...nodeById.keys(), ...threadById.keys()])]
    .flatMap((id) => { const thread = threadById.get(id); return thread ? [thread] : []; });
  const ancestorsById = new Map(ordered.map((thread) => [thread.id, ancestors(thread.id)]));
  const visible = ordered.filter((thread) => !ancestorsById.get(thread.id)?.some((id) => collapsed.has(id)));
  const descendantCounts = new Map<string, number>();
  for (const parents of ancestorsById.values()) {
    for (const parent of parents) descendantCounts.set(parent, (descendantCounts.get(parent) ?? 0) + 1);
  }
  const height = Math.max(440, visible.length * 42 + 70);
  useEffect(() => {
    measureViewport();
    const element = field.current;
    if (!element || typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(measureViewport);
    observer.observe(element);
    return () => observer.disconnect();
  }, [height]);
  const rowById = new Map(visible.map((thread, index) => [thread.id, index]));
  const left = 240, span = WIDTH - left - RIGHT;
  const x = (time: number) => left + (time - view!.start) / (view!.end - view!.start) * span;
  const y = (id: string) => 65 + (rowById.get(id) ?? 0) * Math.min(42, (height - 90) / Math.max(visible.length, 1));
  const links = visible.flatMap((child) => {
    const parentId = parentById.get(child.id);
    const parent = parentId && rowById.has(parentId) ? threadById.get(parentId) : undefined;
    return parent ? [{ parent, child }] : [];
  });
  const linkPath = (parent: typeof visible[number], child: typeof visible[number], projectX: (time: number) => number, projectY: (id: string) => number) => {
    const px = projectX(parent.start), cx = projectX(child.start), bend = (px + cx) / 2;
    return `M ${px} ${projectY(parent.id)} C ${bend} ${projectY(parent.id)}, ${bend} ${projectY(child.id)}, ${cx} ${projectY(child.id)}`;
  };
  const zoom = (factor: number) => {
    if (extent && view) setZoomed(zoomWindow(view, extent, factor, (view.start + view.end) / 2));
  };
  const pan = (direction: number) => {
    if (!extent || !view) return;
    const delta = (view.end - view.start) * .25 * direction;
    setZoomed(clampWindow({ start: view.start + delta, end: view.end + delta }, extent));
  };
  const pickKey = (event: KeyboardEvent, id: string) => {
    if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); onSelect(id); }
  };
  const toggleBranch = (id: string) => setCollapsed((previous) => {
    const next = new Set(previous);
    if (next.has(id)) next.delete(id); else next.add(id);
    return next;
  });
  const extentPattern = (thread: typeof visible[number]) => thread.endSource === 'session_end' ? undefined
    : thread.endSource === 'last_message' ? '6 2' : '2 5';
  const knownAgents = new Set(nodes.filter((node) => threadById.has(JSON.stringify([node.provider, node.session_id])) && node.agent != null).map((node) => node.agent));
  const full = extent ? fittedWindow(extent) : null;
  const miniX = (time: number) => left + (time - full!.start) / (full!.end - full!.start) * span;
  const miniY = (id: string) => 8 + y(id) / height * 64;
  return <div className="min-w-0">
    <div role="toolbar" aria-label="Time window" className="flex min-h-10 flex-wrap items-center gap-2 border border-edge-subtle px-1 text-xs">
      <button className="min-h-8 min-w-8" aria-label="Zoom in" onClick={() => zoom(.5)}>+</button>
      <button className="min-h-8 min-w-8" aria-label="Zoom out" onClick={() => zoom(2)}>−</button>
      <button className="min-h-8 min-w-8" aria-label="Pan to earlier sessions" disabled={!zoomed} onClick={() => pan(-1)}>←</button>
      <button className="min-h-8 min-w-8" aria-label="Pan to later sessions" disabled={!zoomed} onClick={() => pan(1)}>→</button>
      <button className="min-h-8 min-w-8" aria-label="Fit the whole extent" disabled={!zoomed} onClick={() => setZoomed(null)}>fit</button>
      <span>{!zoomed ? 'whole extent' : `${formatMoment(view!.start)} – ${formatMoment(view!.end)}`}</span>
      <span>{visible.length} / {ordered.length} loaded sessions · {links.length} visible parent links · {knownAgents.size} recorded agent labels</span>
    </div>
    <div ref={field} onScroll={measureViewport} className="td-optic max-h-[60vh] overflow-auto" role="region" aria-label="Session lane viewport" tabIndex={0}>
      <svg aria-label="Session time axis" role="img" width="100%" style={{ minWidth: 720 }} className="sticky top-0 z-10 bg-surface-0" viewBox={`0 0 ${WIDTH} 32`}>
        {view && axisTicks(view, span).map((tick) => <text key={tick.time} x={left + tick.x} y={22} textAnchor="middle" fill="var(--raw-graph-text)" fontSize={11}>{tick.label}</text>)}
      </svg>
      <svg role="group" aria-label={ariaLabel} width="100%" style={{ minWidth: 720 }} viewBox={`0 0 ${WIDTH} ${height}`}>
        <defs><clipPath id={clipId}><rect x={left} y={35} width={span} height={height - 40} /></clipPath></defs>
        {view && axisTicks(view, span).map((tick) => <g key={tick.time}>
          <line x1={left + tick.x} x2={left + tick.x} y1={35} y2={height - 20} stroke="var(--raw-graph-edge)" opacity={.3} />
        </g>)}
        {visible.map((thread) => {
          const depth = ancestorsById.get(thread.id)?.length ?? 0, count = descendantCounts.get(thread.id) ?? 0;
          const node = nodeById.get(thread.id);
          const quality = node?.link === 'linked' && !parentById.has(thread.id) ? 'parent outside loaded page' : node?.link ?? 'parentage unavailable';
          return <g key={thread.id} style={kindColorVars(thread.host)}>
            <g role="button" tabIndex={0} aria-label={`Open session ${thread.label}`} className="cursor-pointer" onClick={() => onSelect(thread.id)} onKeyDown={(event) => pickKey(event, thread.id)}>
              <rect x={8} y={y(thread.id) - 18} width={192} height={36} fill="transparent" />
              <text x={12 + Math.min(depth, 4) * 10} y={y(thread.id) - 3} fill="var(--kind-dark)" fontSize={11}>{thread.label.length > 26 ? `${thread.label.slice(0, 25)}…` : thread.label}</text>
              <text x={12 + Math.min(depth, 4) * 10} y={y(thread.id) + 12} fill="var(--raw-graph-text)" fontSize={9}>{thread.host} · {quality}</text>
            </g>
            {count > 0 && <g role="button" tabIndex={0} aria-label={`${collapsed.has(thread.id) ? 'Expand' : 'Collapse'} branch ${thread.label}`} aria-expanded={!collapsed.has(thread.id)} className="cursor-pointer" onClick={() => toggleBranch(thread.id)} onKeyDown={(event) => { if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); toggleBranch(thread.id); } }}>
              <rect x={200} y={y(thread.id) - 18} width={36} height={36} fill="transparent" />
              <text x={205} y={y(thread.id) + 4} fill="var(--raw-graph-text)" fontSize={11}>{collapsed.has(thread.id) ? '+' : '−'}{count}</text>
            </g>}
          </g>;
        })}
        <g clipPath={`url(#${clipId})`}>
          {view && links.map(({ parent, child }) => {
            const label = `Recorded parent ${parent.label} of ${child.label}`;
            return <g key={child.id} role="button" tabIndex={0} aria-label={label} data-parent-session={parent.sessionId} data-child-session={child.sessionId} className="cursor-pointer" onClick={() => onSelect(parent.id)} onKeyDown={(event) => pickKey(event, parent.id)}>
              <title>{label}. Session-bound placement; spawn time unavailable. Parent tool use: {nodeById.get(child.id)?.parent_tool_use_id ?? 'unrecorded'}.</title>
              <path d={linkPath(parent, child, x, y)} fill="none" stroke="var(--raw-graph-text)" strokeDasharray="4 3" strokeWidth={1.5} />
              <path d={linkPath(parent, child, x, y)} fill="none" stroke="transparent" strokeWidth={14} />
            </g>;
          })}
          {view && visible.filter((thread) => thread.start <= view.end && (thread.end ?? thread.start) >= view.start).map((thread) => {
            const start = x(thread.start), end = thread.end == null ? start + 18 : Math.max(start + 4, x(thread.end));
            const middle = y(thread.id), thickness = .7 + thread.weight * 1.3;
            return <g key={thread.id} data-thread={thread.id} role="button" tabIndex={0} aria-label={`Inspect session ${thread.label}`} style={kindColorVars(thread.host)} opacity={selectedId && selectedId !== thread.id ? .25 : 1} onClick={() => onSelect(thread.id)} onKeyDown={(event) => pickKey(event, thread.id)} className="cursor-pointer">
              <title>{thread.host} · {formatMoment(thread.start)} · {thread.messages} messages · {thread.endSource ?? 'end unavailable'}</title>
              <line x1={start} x2={end} y1={middle} y2={middle} stroke="var(--kind-dark)" strokeWidth={thickness + 7} opacity={.12} />
              <line x1={start} x2={end} y1={middle} y2={middle} stroke="var(--kind-dark)" strokeWidth={thickness} strokeLinecap="round" strokeDasharray={extentPattern(thread)} />
              <circle cx={start} cy={middle} r={4} fill={thread.hollow ? 'var(--raw-graph-bg)' : 'var(--kind-dark)'} stroke="var(--kind-dark)" />
              <rect x={start - 8} y={middle - 14} width={Math.max(end - start + 16, 24)} height={28} fill="transparent" />
            </g>;
          })}
        </g>
      </svg>
    </div>
    {full && view && <div className="td-optic">
      <svg role="group" aria-label="Session hierarchy minimap" tabIndex={0} width="100%" viewBox={`0 0 ${WIDTH} 80`}
        onClick={(event) => {
          const bounds = event.currentTarget.getBoundingClientRect();
          const fraction = Math.max(0, Math.min(1, ((event.clientY - bounds.top) / bounds.height * 80 - 8) / 64));
          field.current?.scrollTo({ top: fraction * field.current.scrollHeight - field.current.clientHeight / 2 });
        }}
        onKeyDown={(event) => {
          if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
            event.preventDefault();
            field.current?.scrollBy({ top: (event.key === 'ArrowDown' ? 1 : -1) * field.current.clientHeight * .75 });
          }
        }}>
        <title>Click to locate session lanes; Up and Down scroll the lane viewport.</title>
        {links.map(({ parent, child }) => <path key={child.id} data-minimap-parent={parent.sessionId} d={linkPath(parent, child, miniX, miniY)} fill="none" stroke="var(--raw-graph-text)" strokeDasharray="4 3" strokeWidth={1} />)}
        {visible.map((thread) => <line key={thread.id} data-minimap-session={thread.sessionId} style={kindColorVars(thread.host)} x1={miniX(thread.start)} x2={thread.end == null ? miniX(thread.start) + 3 : miniX(thread.end)} y1={miniY(thread.id)} y2={miniY(thread.id)} stroke="var(--kind-dark)" strokeDasharray={extentPattern(thread)} />)}
        <rect data-session-viewport x={miniX(view.start)} y={8 + vertical.start * 64} width={miniX(view.end) - miniX(view.start)} height={Math.max(1, vertical.fraction * 64)} fill="none" stroke="var(--raw-graph-text)" />
      </svg>
      <label className="flex items-center gap-2 text-3xs text-text-muted">Session window
        <input className="flex-1" aria-label="Session minimap viewport" type="range" min={full.start} max={Math.max(full.start, full.end - (view.end - view.start))} step="any" value={view.start} disabled={!zoomed} onChange={(event) => { const start = Number(event.currentTarget.value); setZoomed({ start, end: start + view.end - view.start }); }} />
      </label>
    </div>}
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
      y: frame.timestamp == null ? 350 : 200,
      frame,
    })),
  };
}

export function LoadedEventCanvas({ frames, visible, activeId, onSelect, onInspect, toolbar, scrubber }: {
  toolbar: ReactNode;
  scrubber: ReactNode;
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
  const laneY = hasUndated ? [200, 350] : [200];
  const active = revealed.find((point) => point.id === activeId);
  const x = (position: number) => left + (position - window.start) / (window.end - window.start) * spanPx;
  // Partition hit regions at neighboring time positions. A dense page must
  // not let a later SVG rectangle steal clicks from an earlier event.
  const ordered = [...points].sort((a, b) => a.x - b.x);
  const markSpacing = new Map(ordered.map((point, index) => {
    const before = ordered[index - 1], after = ordered[index + 1];
    const gap = Math.min(before ? x(point.x) - x(before.x) : 44, after ? x(after.x) - x(point.x) : 44);
    return [point.id, Math.max(.5, Math.min(22, gap / 2))];
  }));
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
    <div className="flex min-h-10 flex-wrap items-center gap-3 border border-edge-subtle px-1 text-xs">
      {toolbar}
      <div className="flex items-center gap-2" role="toolbar" aria-label="Execution viewport">
      <button className="min-h-8 min-w-8" aria-label="Zoom into execution" disabled={span < .02} onClick={() => move(window.start + span / 4, window.end - span / 4)}>+ Zoom</button>
      <button className="min-h-8 min-w-8" aria-label="Fit loaded execution" onClick={() => move(0, 1)}>Fit</button>
      <button className="min-h-8 min-w-8" aria-label="Pan execution earlier" disabled={window.start === 0} onClick={() => { const start = Math.max(0, window.start - span / 4); move(start, start + span); }}>← Older</button>
      <button className="min-h-8 min-w-8" aria-label="Pan execution later" disabled={window.end === 1} onClick={() => { const end = Math.min(1, window.end + span / 4); move(end - span, end); }}>Later →</button>
      </div>
    </div>
    {width < 480 && <p className="text-xs text-text-muted">{geometry.start == null ? 'No source timestamps' : formatMoment(geometry.start)} → {geometry.end == null ? 'loaded source order' : `${formatMoment(geometry.end)} · loaded end`}</p>}
    <div className="td-optic">
      <svg role="group" aria-label="Revealed execution events" width="100%" viewBox={`0 0 ${width} 460`}>
        <defs><clipPath id={clipId}><rect x={left - 12} y={40} width={spanPx + 24} height={400} /></clipPath></defs>
        <text visibility={width < 480 ? "hidden" : "visible"} x={left} y={24} fill="var(--raw-graph-text)" fontSize={12}>{geometry.start == null ? 'No source timestamps' : formatMoment(geometry.start)}</text>
        <text visibility={width < 480 ? "hidden" : "visible"} x={width - RIGHT} y={24} textAnchor="end" fill="var(--raw-graph-text)" fontSize={12}>{geometry.end == null ? 'Loaded source order' : `${formatMoment(geometry.end)} · LOADED END`}</text>
        <text x={12} y={170} fill="var(--raw-graph-text)" fontSize={11}>Recorded time</text>
        {hasUndated && <text x={12} y={320} fill="var(--raw-graph-text)" fontSize={11}>Undated</text>}
        {hasUndated && <text x={12} y={335} fill="var(--raw-graph-text)" fontSize={10}>source order</text>}
        {laneY.map((y) => <line key={y} x1={left} x2={width - RIGHT} y1={y} y2={y} stroke="var(--raw-graph-edge)" strokeDasharray="2 5" />)}
        <g clipPath={`url(#${clipId})`}>
          {sequences.map(({ previous, point }) => {
            const path = orderPath(previous, point, x);
            return <g key={`${previous.id}:${point.id}`} aria-label="Recorded message order">
              <path d={path} fill="none" stroke="#58daec" strokeWidth={8} opacity={.08} />
              <path d={path} fill="none" stroke="#58daec" strokeWidth={1} opacity={.6} />
            </g>;
          })}
          {active && <line x1={x(active.x)} x2={x(active.x)} y1={42} y2={430} stroke="var(--raw-graph-text)" strokeWidth={1} opacity={.55} />}
          {points.map((point) => {
            const halfHit = markSpacing.get(point.id) ?? 22;
            const radius = Math.min(6, Math.max(1.5, halfHit * .6));
            return <g key={point.id} data-event={point.id} role="button" tabIndex={0} aria-label={`Select stored event ${point.id}`} aria-pressed={point.id === activeId} onClick={() => onSelect(point.id)} onKeyDown={(event) => { if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); onSelect(point.id); } }} className="cursor-pointer outline-none [&:focus>rect]:stroke-white">
            <rect x={x(point.x) - halfHit} y={point.y - 22} width={halfHit * 2} height={70} fill="transparent" />
            <title>{point.frame.role} · {point.frame.tool ?? 'message'} · {point.id}</title>
            <circle pointerEvents="none" cx={x(point.x)} cy={point.y} r={point.id === activeId ? 19 : radius * 2} fill="#35d6ee" opacity={.08} />
            <circle pointerEvents="none" cx={x(point.x)} cy={point.y} r={point.id === activeId ? 10 : radius} fill="var(--raw-graph-bg)" stroke="#58daec" strokeWidth={point.id === activeId ? 2 : 1} />
            <circle pointerEvents="none" cx={x(point.x)} cy={point.y} r={Math.min(2, radius / 2)} fill="#c1f8ff" />
            <text pointerEvents="none" x={x(point.x)} y={point.y + 35} textAnchor="middle" fill="var(--raw-graph-text)" fontSize={11}>{points.length <= 12 || point.id === activeId ? point.frame.tool ?? point.frame.role : ''}</text>
          </g>;
          })}
        </g>
      </svg>
      <svg role="group" aria-label="Execution minimap" width="100%" viewBox={`0 0 ${width} 64`}>
        {laneY.map((y) => <line key={y} x1={left} x2={width - RIGHT} y1={y / 8} y2={y / 8} stroke="var(--raw-graph-edge)" />)}
        {sequences.map(({ previous, point }) => <path key={`${previous.id}:${point.id}`} d={orderPath(previous, point, (position) => left + position * spanPx, .125)} fill="none" stroke="#58daec" opacity={.5} />)}
        {revealed.map((point) => <circle key={point.id} data-minimap-event={point.id} cx={left + point.x * spanPx} cy={point.y / 8} r={point.id === activeId ? 4 : 2} fill="#58daec" />)}
        <rect x={left + window.start * spanPx} y={8} width={span * spanPx} height={48} fill="none" stroke="var(--raw-graph-text)" />
      </svg>
    </div>
    <label className="flex flex-wrap items-center gap-2 text-xs text-text-muted">Minimap position
      <input type="range" aria-label="Minimap viewport" min={0} max={1 - span} step={.001} value={window.start} disabled={span === 1} onChange={(event) => move(Number(event.currentTarget.value), Number(event.currentTarget.value) + span)} />
    </label>
    {scrubber}
    <details><summary className="text-3xs text-text-muted">Recorded message order · causality unavailable</summary><p className="text-3xs text-text-muted">Each dot is a stored message. Placement measures time, or explicitly undated source order; thin connections show canonical message order, not causal attribution. Proximity is not a causal edge. Exact source selection remains available in the sequence below.</p></details>
  </section>;
}
