/**
 * Host for a laid-out `TemporalSceneModel`.
 *
 * Two layers over one coordinate space. A Canvas2D substrate, painted once per
 * model, density or palette change by the selected renderer; nothing there
 * animates. A crisp SVG overlay carries every selectable mark, every label and
 * every title, so the surface stays complete when the canvas is missing and
 * every pointer action has a keyboard path. The overlay's roles, labels and
 * data attributes are the same for every renderer; a renderer only supplies
 * the paint inside them.
 *
 * This component draws. It never lays out, never grades, and never invents a
 * quantity: every coordinate comes from the model. Hover only inspects.
 */
import type { JSX, KeyboardEvent, PointerEvent as ReactPointerEvent } from 'react';
import { useEffect, useId, useMemo, useRef, useState } from 'react';
import { formatMoment } from '../../workspaces/loom/tracks.ts';
import type { SceneDensity } from './density.ts';
import { glyphLabel, TemporalLegend } from './glyphs.tsx';
import { resolveTemporalPalette, type TemporalPalette } from './palette.ts';
import type { SceneFrame, SceneRenderer } from './renderers/contract.ts';
import { currentRenderer } from './renderers/current.tsx';
import { focusAlpha } from './renderers/paint.ts';
import type {
  SceneCluster,
  SceneGap,
  SceneInterval,
  SceneLane,
  SceneNode,
  SceneWindow,
  TemporalSceneModel,
} from './types.ts';

export interface TemporalSceneProps {
  model: TemporalSceneModel;
  ariaLabel: string;
  onSelectLane: (laneId: string | null) => void;
  onSelectEvent: (eventId: string) => void;
  onToggleBranch: (laneId: string) => void;
  onSelectEncounter?: (encounterId: string) => void;
  onWindowChange: (window: SceneWindow) => void;
  /** Pixel width the caller should lay out for; reported when the host resizes. */
  onMeasure?: (width: number) => void;
  /** Newest loaded record label for the right marker, e.g. 'LOADED END' or 'NOW'. */
  tailLabel: string;
  reducedMotion: boolean;
  onInspect?: (node: SceneNode | null) => void;
  className?: string;
  /** The whole projection extent, so Fit has somewhere to return to. */
  fullWindow?: SceneWindow;
  /** Paint for the substrate and marks; the shipped weave when omitted. */
  renderer?: SceneRenderer;
  /** Per-lane density over the same window, for renderers that aggregate. */
  density?: SceneDensity | null;
}

/** Height of the time ruler strip across the top of the field. */
export const RULER = 36;
/** Pointer travel under which a drag on the background is a click. */
const CLICK_SLOP_PX = 3;
const LABEL_CHAR_PX = 6.2;
const TOGGLE_WIDTH = 30;
const OPEN_TAIL_PX = 18;

type SceneLayer = 'canvas' | 'unavailable';

interface DragState {
  pointerId: number;
  startClientX: number;
  startWindow: SceneWindow;
  moved: boolean;
}

/* ---- shared encodings ---------------------------------------------------- */

function proximityTone(tone: SceneInterval['tone']): string {
  switch (tone) {
    case 'candidate':
      return '#ffc04d';
    case 'overlap':
      return '#ff8a70';
    case 'conflict':
      return '#ff4d4f';
    case null:
      return 'var(--raw-graph-alert)';
    default: {
      const exhaustive: never = tone;
      throw new Error(`unknown proximity tone: ${String(exhaustive)}`);
    }
  }
}

function truncateLabel(text: string, maxPx: number): string {
  const maxChars = Math.max(3, Math.floor(maxPx / LABEL_CHAR_PX));
  return text.length > maxChars ? `${text.slice(0, maxChars - 1)}…` : text;
}

function isActivation(event: KeyboardEvent): boolean {
  return event.key === 'Enter' || event.key === ' ';
}

function spanOf(window: SceneWindow): number {
  return window.end - window.start;
}

function sameWindow(a: SceneWindow, b: SceneWindow, tolerance: number): boolean {
  return Math.abs(a.start - b.start) <= tolerance && Math.abs(a.end - b.end) <= tolerance;
}

/* ---- component ----------------------------------------------------------- */

export function TemporalScene(props: TemporalSceneProps): JSX.Element {
  const {
    model,
    ariaLabel,
    onSelectLane,
    onSelectEvent,
    onToggleBranch,
    onSelectEncounter,
    onWindowChange,
    onMeasure,
    tailLabel,
    reducedMotion,
    onInspect,
    className,
    fullWindow,
    renderer = currentRenderer,
    density = null,
  } = props;
  const clipId = useId();
  const hostRef = useRef<HTMLElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const overlayRef = useRef<SVGSVGElement>(null);
  const dragRef = useRef<DragState | null>(null);
  const [palette, setPalette] = useState<TemporalPalette | null>(null);
  const [layer, setLayer] = useState<SceneLayer>('canvas');
  const [hover, setHover] = useState<string | null>(null);

  const { viewport, height } = model;
  const width = viewport.width;
  const window = viewport.window;
  const windowSpan = Math.max(spanOf(window), Number.EPSILON);
  const fieldX0 = viewport.left;
  const fieldX1 = width - viewport.right;
  const fieldWidth = Math.max(1, fieldX1 - fieldX0);
  const frame: SceneFrame = useMemo(
    () => ({ model, density, fieldX0, fieldX1, top: RULER, height, tailLabel }),
    [model, density, fieldX0, fieldX1, height, tailLabel],
  );
  const { NodeMark, ClusterMark, FieldOverlay, GradeSwatch, LegendEncodings } = renderer;

  const lanes = useMemo(() => [...model.lanes].sort((a, b) => a.row - b.row), [model.lanes]);
  const clusterByLane = useMemo(
    () => new Map(model.clusters.map((cluster) => [cluster.laneId, cluster])),
    [model.clusters],
  );
  const nodesByLane = useMemo(() => {
    const groups = new Map<string, SceneNode[]>();
    for (const node of model.nodes) {
      const group = groups.get(node.laneId);
      if (group) group.push(node);
      else groups.set(node.laneId, [node]);
    }
    return groups;
  }, [model.nodes]);
  const intervalsByLane = useMemo(() => {
    const groups = new Map<string, SceneInterval[]>();
    for (const interval of model.intervals) {
      const group = groups.get(interval.laneId);
      if (group) group.push(interval);
      else groups.set(interval.laneId, [interval]);
    }
    return groups;
  }, [model.intervals]);
  const gapsByLane = useMemo(() => {
    const groups = new Map<string, SceneGap[]>();
    for (const gap of model.gaps) {
      if (gap.laneId === null || gap.x === null || gap.y === null) continue;
      const group = groups.get(gap.laneId);
      if (group) group.push(gap);
      else groups.set(gap.laneId, [gap]);
    }
    return groups;
  }, [model.gaps]);
  /** Visible descendants per lane, read off the preorder row list. */
  const visibleDescendants = useMemo(() => {
    const counts = new Map<string, number>();
    lanes.forEach((lane, index) => {
      let count = 0;
      for (let next = index + 1; next < lanes.length && lanes[next]!.depth > lane.depth; next += 1) {
        count += 1;
      }
      counts.set(lane.id, count);
    });
    return counts;
  }, [lanes]);
  const hoverLaneId = hover === null ? null : model.nodes.find((node) => node.id === hover)?.laneId ?? null;

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    setPalette(resolveTemporalPalette(host));
    if (typeof MutationObserver !== 'function') return;
    const observer = new MutationObserver(() => setPalette(resolveTemporalPalette(host)));
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme', 'data-contrast'] });
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    const host = hostRef.current;
    if (!host || !onMeasure) return;
    const initial = host.getBoundingClientRect().width;
    if (initial > 0) onMeasure(Math.round(initial));
    if (typeof ResizeObserver !== 'function') return;
    const observer = new ResizeObserver(([entry]) => {
      if (entry && entry.contentRect.width > 0) onMeasure(Math.round(entry.contentRect.width));
    });
    observer.observe(host);
    return () => observer.disconnect();
  }, [onMeasure]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !palette) return;
    const dpr = globalThis.devicePixelRatio || 1;
    canvas.width = Math.max(1, Math.round(width * dpr));
    canvas.height = Math.max(1, Math.round(height * dpr));
    const ctx = canvas.getContext('2d');
    if (!ctx) {
      setLayer('unavailable');
      return;
    }
    setLayer('canvas');
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, width, height);
    renderer.paint(ctx, frame, palette);
  }, [renderer, frame, palette, width, height]);

  /* ---- window arithmetic ---- */

  const xToTime = (x: number): number => window.start + ((x - fieldX0) / fieldWidth) * windowSpan;
  const clientToViewBox = (clientX: number, element: SVGSVGElement, viewBoxWidth: number): number => {
    const bounds = element.getBoundingClientRect();
    const scale = bounds.width > 0 ? viewBoxWidth / bounds.width : 1;
    return (clientX - bounds.left) * scale;
  };
  const shiftWindow = (seconds: number): void =>
    onWindowChange({ start: window.start + seconds, end: window.end + seconds });
  const scaleWindow = (factor: number, anchorTime: number): void => {
    const nextSpan = windowSpan * factor;
    const ratio = (anchorTime - window.start) / windowSpan;
    onWindowChange({ start: anchorTime - ratio * nextSpan, end: anchorTime + (1 - ratio) * nextSpan });
  };
  const centre = (window.start + window.end) / 2;
  const fitDisabled =
    fullWindow === undefined || sameWindow(window, fullWindow, Math.max(spanOf(fullWindow), 1) * 0.005);

  const wheelRef = useRef<(event: WheelEvent) => void>(() => {});
  wheelRef.current = (event: WheelEvent) => {
    const overlay = overlayRef.current;
    if (!overlay) return;
    if (event.ctrlKey || event.metaKey) {
      event.preventDefault();
      const px = Math.min(fieldX1, Math.max(fieldX0, clientToViewBox(event.clientX, overlay, width)));
      scaleWindow(event.deltaY > 0 ? 1.25 : 0.8, xToTime(px));
      return;
    }
    if (event.deltaX !== 0 || event.shiftKey) {
      event.preventDefault();
      const delta = event.deltaX !== 0 ? event.deltaX : event.deltaY;
      shiftWindow((delta / fieldWidth) * windowSpan);
    }
  };
  useEffect(() => {
    const overlay = overlayRef.current;
    if (!overlay) return;
    // React registers wheel as passive, so preventDefault has to go native.
    const listener = (event: WheelEvent) => wheelRef.current(event);
    overlay.addEventListener('wheel', listener, { passive: false });
    return () => overlay.removeEventListener('wheel', listener);
  }, []);

  const onBackgroundPointerDown = (event: ReactPointerEvent<SVGRectElement>): void => {
    if (event.button !== 0) return;
    dragRef.current = {
      pointerId: event.pointerId,
      startClientX: Number(event.clientX) || 0,
      startWindow: window,
      moved: false,
    };
    try {
      event.currentTarget.setPointerCapture?.(event.pointerId);
    } catch {
      // Pointer capture is a nicety; the drag still works without it.
    }
  };
  const onBackgroundPointerMove = (event: ReactPointerEvent<SVGRectElement>): void => {
    const drag = dragRef.current;
    const overlay = overlayRef.current;
    if (!drag || !overlay || drag.pointerId !== event.pointerId) return;
    const bounds = overlay.getBoundingClientRect();
    const scale = bounds.width > 0 ? width / bounds.width : 1;
    const dx = ((Number(event.clientX) || 0) - drag.startClientX) * scale;
    if (!drag.moved && Math.abs(dx) < CLICK_SLOP_PX) return;
    drag.moved = true;
    const seconds = (-dx / fieldWidth) * spanOf(drag.startWindow);
    onWindowChange({ start: drag.startWindow.start + seconds, end: drag.startWindow.end + seconds });
  };
  const onBackgroundPointerUp = (event: ReactPointerEvent<SVGRectElement>): void => {
    const drag = dragRef.current;
    if (!drag || drag.pointerId !== event.pointerId) return;
    dragRef.current = null;
    try {
      event.currentTarget.releasePointerCapture?.(event.pointerId);
    } catch {
      // Nothing was captured.
    }
    if (!drag.moved) onSelectLane(null);
  };

  /* ---- minimap arithmetic ---- */

  const minimap = model.minimap;
  const minimapToTime = (px: number): number => {
    if (fullWindow) return fullWindow.start + (px / Math.max(minimap.width, 1)) * spanOf(fullWindow);
    const minimapSpan = Math.max(minimap.window.x1 - minimap.window.x0, Number.EPSILON);
    return window.start + ((px - minimap.window.x0) / minimapSpan) * windowSpan;
  };
  const centreWindowAt = (time: number): void =>
    onWindowChange({ start: time - windowSpan / 2, end: time + windowSpan / 2 });
  const maxBinEvents = Math.max(1, ...minimap.bins.map((bin) => bin.events));
  const binMaxHeight = Math.max(1, minimap.height - 4);

  /* ---- node interaction ---- */

  const enterNode = (node: SceneNode): void => {
    setHover(node.id);
    onInspect?.(node);
  };
  const leaveNode = (): void => {
    setHover(null);
    onInspect?.(null);
  };
  const laneGroupStyle = (laneId: string) => ({
    opacity: hoverLaneId !== null && hoverLaneId !== laneId ? 0.55 : 1,
    transition: reducedMotion ? 'none' : 'opacity 120ms',
  });

  const branchToggleFor = (lane: SceneLane): { label: string; count: number; expanded: boolean } | null => {
    const visible = visibleDescendants.get(lane.id) ?? 0;
    if (lane.kind === 'bundle') {
      const cluster = clusterByLane.get(lane.id);
      return { label: `Expand branch ${lane.label}`, count: cluster?.counts.sessions ?? lane.collapsedDescendants, expanded: false };
    }
    if (lane.collapsedDescendants > 0 || visible > 0) {
      return { label: `Collapse branch ${lane.label}`, count: lane.collapsedDescendants > 0 ? lane.collapsedDescendants : visible, expanded: true };
    }
    return null;
  };

  const renderLaneField = (lane: SceneLane): JSX.Element => {
    const nodes = nodesByLane.get(lane.id) ?? [];
    const intervals = intervalsByLane.get(lane.id) ?? [];
    const gaps = gapsByLane.get(lane.id) ?? [];
    const cluster = clusterByLane.get(lane.id);
    return (
      <g key={lane.id} data-lane-group={lane.id} style={laneGroupStyle(lane.id)}>
        {lane.endSource === null && !lane.offscreen && lane.revealed && (
          <g data-lane-tail={lane.id}>
            <title>extent unknown</title>
            <line
              x1={lane.x1}
              x2={lane.x1 + OPEN_TAIL_PX}
              y1={lane.y}
              y2={lane.y}
              stroke="var(--raw-graph-dim)"
              strokeWidth={1.6}
              strokeDasharray="1 4"
              strokeLinecap="round"
            />
          </g>
        )}
        {intervals.map((interval) => renderInterval(interval))}
        {cluster && renderCluster(cluster, lane)}
        {gaps.map((gap) => renderGap(gap))}
        {nodes.map((node) => renderNode(node))}
      </g>
    );
  };

  const renderInterval = (interval: SceneInterval): JSX.Element => {
    const x1 = Math.max(interval.x1, interval.x0 + 2);
    switch (interval.kind) {
      case 'git_span':
        return (
          <line
            key={interval.id}
            data-interval={interval.id}
            data-interval-kind={interval.kind}
            x1={interval.x0}
            x2={x1}
            y1={interval.y}
            y2={interval.y}
            stroke="var(--raw-graph-edge)"
            strokeWidth={3}
            opacity={0.8}
            strokeLinecap="round"
          >
            <title>{interval.label}</title>
          </line>
        );
      case 'proximity': {
        const ref = interval.ref;
        const select = (): void => {
          if (ref !== null) onSelectEncounter?.(ref);
        };
        return (
          <g
            key={interval.id}
            role="button"
            tabIndex={0}
            aria-label={`Open encounter ${interval.label}`}
            className="cursor-pointer outline-none [&:focus>line]:stroke-white"
            onClick={select}
            onKeyDown={(event) => {
              if (isActivation(event)) {
                event.preventDefault();
                select();
              }
            }}
          >
            <title>{`${interval.label} · ${interval.grade}`}</title>
            <rect x={interval.x0 - 8} y={interval.y - 22} width={x1 - interval.x0 + 16} height={44} fill="transparent" />
            <line
              data-interval={interval.id}
              data-interval-kind={interval.kind}
              data-proximity-encounter={ref ?? undefined}
              x1={interval.x0}
              x2={x1}
              y1={interval.y}
              y2={interval.y}
              stroke={proximityTone(interval.tone)}
              strokeWidth={5}
              strokeLinecap="round"
              opacity={0.85}
            />
          </g>
        );
      }
      default: {
        const exhaustive: never = interval.kind;
        throw new Error(`unknown interval kind: ${String(exhaustive)}`);
      }
    }
  };

  const renderGap = (gap: SceneGap): JSX.Element | null => {
    if (gap.x === null || gap.y === null) return null;
    const label = `${gap.kind.replaceAll('_', ' ')}: ${gap.detail}`;
    const { x, y } = gap;
    return (
      <g key={gap.id} role="img" data-gap={gap.id} data-gap-kind={gap.kind} aria-label={label}>
        <title>{label}</title>
        {gap.grade === 'ambiguous' ? (
          <path
            d={`M ${x} ${y - 6} L ${x + 6} ${y} L ${x} ${y + 6} L ${x - 6} ${y} Z`}
            fill="none"
            stroke="var(--raw-graph-alert)"
            strokeWidth={1.2}
            strokeDasharray="2 2"
          />
        ) : (
          <circle cx={x} cy={y} r={5} fill="none" stroke="var(--raw-graph-dim)" strokeWidth={1.4} strokeDasharray="1 3" />
        )}
      </g>
    );
  };

  const renderCluster = (cluster: SceneCluster, lane: SceneLane): JSX.Element => {
    const x1 = Math.max(cluster.x1, cluster.x0 + 2);
    const top = cluster.y - cluster.height / 2;
    const label = `Expand branch ${lane.label} · ${cluster.counts.sessions} sessions · ${cluster.counts.subagents} subagents · ${cluster.counts.messages} messages`;
    return (
      <g
        role="button"
        tabIndex={0}
        data-cluster={cluster.id}
        aria-label={label}
        className="cursor-pointer outline-none [&:focus>path]:stroke-white"
        opacity={focusAlpha(cluster.focus)}
        onClick={() => onToggleBranch(cluster.laneId)}
        onKeyDown={(event) => {
          if (isActivation(event)) {
            event.preventDefault();
            onToggleBranch(cluster.laneId);
          }
        }}
      >
        <title>{label}</title>
        <rect x={cluster.x0} y={Math.min(top, cluster.y - 22)} width={x1 - cluster.x0} height={Math.max(cluster.height, 44)} fill="transparent" />
        <ClusterMark cluster={cluster} lane={lane} frame={frame} />
      </g>
    );
  };

  const renderNode = (node: SceneNode): JSX.Element => {
    const hovered = hover === node.id;
    const halfHit = Math.max(1, node.halfHit);
    const title = `${glyphLabel(node.kind)} · ${node.label}${node.detail ? ` · ${node.detail}` : ''} · ${node.grade}${node.xBasis === 'sequence' ? ' · recorded order, timestamp unrecorded' : ''}`;
    const select = (): void => onSelectEvent(node.id);
    return (
      <g
        key={node.id}
        role="button"
        tabIndex={0}
        data-event={node.id}
        data-lane={node.laneId}
        data-kind={node.kind}
        data-grade={node.grade}
        data-x-basis={node.xBasis}
        aria-label={`Select ${glyphLabel(node.kind)} ${node.label}`}
        aria-pressed={node.selected}
        className={renderer.nodeClassName}
        opacity={focusAlpha(node.focus)}
        onClick={select}
        onKeyDown={(event) => {
          if (isActivation(event)) {
            event.preventDefault();
            select();
          }
        }}
        onMouseEnter={() => enterNode(node)}
        onFocus={() => enterNode(node)}
        onMouseLeave={leaveNode}
        onBlur={leaveNode}
      >
        <title>{title}</title>
        <rect x={node.x - halfHit} y={node.y - 22} width={Math.max(2, halfHit * 2)} height={44} fill="transparent" />
        <NodeMark node={node} hovered={hovered} frame={frame} />
      </g>
    );
  };

  const renderLaneRow = (lane: SceneLane): JSX.Element => {
    const rowHeight = Math.max(22, lane.height);
    const indent = 8 + lane.depth * 10;
    const labelWidth = viewport.left - TOGGLE_WIDTH - indent - 6;
    const twoLine = lane.height >= 28;
    const toggle = branchToggleFor(lane);
    const bundleCount = clusterByLane.get(lane.id)?.counts.sessions ?? lane.collapsedDescendants;
    const detailLine =
      renderer.laneDetail?.(lane, frame) ??
      (lane.kind === 'bundle' ? `${lane.provider} · bundle · ${bundleCount} sessions` : lane.provider);
    // Lane rows and toggles are pointer affordances for the same actions the
    // branch navigator table offers as 44px DOM controls; they stay out of the
    // tab order so a dense page does not become hundreds of stops.
    return (
      <g key={lane.id}>
        <g
          role="button"
          tabIndex={-1}
          aria-label={`Open session ${lane.label}`}
          aria-pressed={lane.focus === 'selected'}
          data-lane-row={lane.id}
          className="cursor-pointer outline-none [&:focus>rect]:stroke-white"
          opacity={lane.offscreen || !lane.revealed ? 0.55 : focusAlpha(lane.focus)}
          onClick={() => onSelectLane(lane.id)}
          onKeyDown={(event) => {
            if (isActivation(event)) {
              event.preventDefault();
              onSelectLane(lane.id);
            }
          }}
        >
          <title>{`${lane.label} · ${detailLine}${lane.offscreen ? ' · outside the window' : ''}${lane.revealed ? '' : ' · starts after the playback cursor'}`}</title>
          <rect x={0} y={lane.y - rowHeight / 2} width={Math.max(1, viewport.left - TOGGLE_WIDTH)} height={rowHeight} fill="transparent" stroke="none" />
          <text x={indent} y={twoLine ? lane.y - 3 : lane.y + 4} fontSize={11} fill="var(--raw-graph-text)" pointerEvents="none">
            {truncateLabel(lane.label, labelWidth)}
          </text>
          {twoLine && (
            <text x={indent} y={lane.y + 10} fontSize={9} fill="var(--raw-graph-text)" opacity={0.7} pointerEvents="none">
              {truncateLabel(detailLine, labelWidth)}
            </text>
          )}
        </g>
        {toggle && (
          <g
            role="button"
            tabIndex={-1}
            aria-expanded={toggle.expanded}
            aria-label={toggle.label}
            data-branch-toggle={lane.id}
            className="cursor-pointer outline-none [&:focus>rect]:stroke-white"
            onClick={() => onToggleBranch(lane.id)}
            onKeyDown={(event) => {
              if (isActivation(event)) {
                event.preventDefault();
                onToggleBranch(lane.id);
              }
            }}
          >
            <title>{toggle.label}</title>
            <rect x={viewport.left - TOGGLE_WIDTH} y={lane.y - rowHeight / 2} width={TOGGLE_WIDTH} height={rowHeight} fill="transparent" stroke="none" />
            <text x={viewport.left - TOGGLE_WIDTH + 4} y={lane.y + 4} fontSize={10} fill="var(--raw-graph-accent)" pointerEvents="none">
              {toggle.expanded ? `−${toggle.count}` : `+${toggle.count}`}
            </text>
          </g>
        )}
      </g>
    );
  };

  const onMinimapKey = (event: KeyboardEvent<SVGSVGElement>): void => {
    if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
      event.preventDefault();
      shiftWindow(windowSpan * 0.25 * (event.key === 'ArrowRight' ? 1 : -1));
    }
  };

  return (
    <section
      ref={hostRef}
      aria-label={ariaLabel}
      data-scene-renderer={renderer.id}
      className={`td-optic relative min-w-0 ${className ?? ''}`.trim()}
    >
      <div role="toolbar" aria-label="Time window" className="relative z-10 flex min-h-10 flex-wrap items-center gap-1 border-b border-edge-subtle px-1 text-xs">
        <button type="button" className="td-hit" aria-label="Zoom in" onClick={() => scaleWindow(0.5, centre)}>+</button>
        <button type="button" className="td-hit" aria-label="Zoom out" onClick={() => scaleWindow(2, centre)}>−</button>
        <button type="button" className="td-hit" aria-label="Pan earlier" onClick={() => shiftWindow(-windowSpan * 0.25)}>←</button>
        <button type="button" className="td-hit" aria-label="Pan later" onClick={() => shiftWindow(windowSpan * 0.25)}>→</button>
        <button
          type="button"
          className="td-hit"
          aria-label="Fit"
          disabled={fitDisabled}
          onClick={() => {
            if (fullWindow) onWindowChange(fullWindow);
          }}
        >
          fit
        </button>
        <span className="td-value text-3xs" data-window-readout>
          {formatMoment(window.start)} – {formatMoment(window.end)}
        </span>
      </div>
      <div className="relative" data-scene-field>
        <canvas
          ref={canvasRef}
          aria-hidden="true"
          data-scene-layer={layer}
          className="pointer-events-none absolute inset-0 block h-full w-full"
        />
        {layer === 'unavailable' && (
          <p
            role="status"
            className="pointer-events-none absolute z-10 text-3xs text-text-muted"
            style={{ left: fieldX0 + 8, top: RULER + 6 }}
          >
            scene layer unavailable · exact overlay retains every event
          </p>
        )}
        <svg
          ref={overlayRef}
          role="group"
          aria-label="Temporal execution overlay"
          data-scene-layer="overlay"
          className="relative block touch-none select-none"
          width="100%"
          viewBox={`0 0 ${width} ${height}`}
        >
          <defs>
            <clipPath id={clipId}>
              <rect x={fieldX0} y={RULER} width={fieldWidth} height={Math.max(0, height - RULER)} />
            </clipPath>
          </defs>
          <rect x={0} y={0} width={width} height={RULER} fill="var(--raw-graph-substrate)" opacity={0.6} />
          {model.ticks.map((tick) => (
            <text key={`${tick.time}:${tick.x}`} x={tick.x} y={RULER - 10} fontSize={10} textAnchor="middle" fill="var(--raw-graph-text)" pointerEvents="none">
              {tick.label}
            </text>
          ))}
          <line x1={0} x2={width} y1={RULER - 0.5} y2={RULER - 0.5} stroke="var(--raw-graph-edge)" strokeWidth={1} />
          <rect
            data-field-background
            x={fieldX0}
            y={RULER}
            width={fieldWidth}
            height={Math.max(0, height - RULER)}
            fill="transparent"
            className="cursor-grab"
            onPointerDown={onBackgroundPointerDown}
            onPointerMove={onBackgroundPointerMove}
            onPointerUp={onBackgroundPointerUp}
            onPointerCancel={() => {
              dragRef.current = null;
            }}
          />
          {lanes.map((lane) => renderLaneRow(lane))}
          <g clipPath={`url(#${clipId})`}>
            {lanes.map((lane) => renderLaneField(lane))}
            {model.labels
              .filter((label) => label.group !== 'lane')
              .map((label) => (
                <text
                  key={label.id}
                  data-label={label.id}
                  x={label.x}
                  y={label.y}
                  fontSize={10}
                  textAnchor={label.anchor}
                  fill="var(--raw-graph-text)"
                  pointerEvents="none"
                >
                  {label.text}
                </text>
              ))}
          </g>
          <FieldOverlay frame={frame} />
        </svg>
      </div>
      <svg
        role="group"
        aria-label="Temporal minimap"
        tabIndex={0}
        data-minimap
        className="block border-t border-edge-subtle outline-none [&:focus-visible>.td-scene-viewport]:stroke-white"
        width="100%"
        viewBox={`0 0 ${minimap.width} ${minimap.height}`}
        onClick={(event) => centreWindowAt(minimapToTime(clientToViewBox(event.clientX, event.currentTarget, minimap.width)))}
        onKeyDown={onMinimapKey}
      >
        <title>Click to centre the window; Left and Right pan by a quarter of the span.</title>
        {minimap.bins.map((bin, index) => {
          const barHeight = bin.events > 0 ? Math.max(1, (bin.events / maxBinEvents) * binMaxHeight) : 0;
          return (
            <rect
              key={`${bin.x0}:${index}`}
              data-minimap-bin
              x={bin.x0}
              y={minimap.height - barHeight}
              width={Math.max(1, bin.x1 - bin.x0)}
              height={barHeight}
              fill="var(--raw-graph-accent)"
              opacity={0.55}
            />
          );
        })}
        {minimap.lanes.map((lane) => (
          <line
            key={lane.id}
            data-minimap-lane={lane.id}
            x1={lane.x0}
            x2={Math.max(lane.x1, lane.x0 + 2)}
            y1={lane.y}
            y2={lane.y}
            stroke="var(--raw-graph-text)"
            strokeWidth={1}
            strokeDasharray={lane.endSource === null ? '1 3' : undefined}
            opacity={0.75}
          />
        ))}
        <rect
          data-scene-viewport
          className="td-scene-viewport"
          x={minimap.window.x0}
          y={0.5}
          width={Math.max(2, minimap.window.x1 - minimap.window.x0)}
          height={Math.max(1, minimap.height - 1)}
          fill="none"
          stroke="var(--raw-graph-text)"
          strokeWidth={1}
        />
      </svg>
      <TemporalLegend gaps={model.gaps} {...(GradeSwatch ? { Swatch: GradeSwatch } : {})}>
        {LegendEncodings && <LegendEncodings frame={frame} />}
      </TemporalLegend>
    </section>
  );
}
