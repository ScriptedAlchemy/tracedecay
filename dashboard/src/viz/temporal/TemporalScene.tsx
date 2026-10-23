/**
 * Renderer for a laid-out `TemporalSceneModel`.
 *
 * Two layers over one coordinate space. A Canvas2D substrate (`scene/paint`),
 * painted once per model, density or palette change; nothing there animates.
 * A crisp SVG overlay carries every selectable mark, every label and every
 * title, so the surface stays complete when the canvas is missing and every
 * pointer action has a keyboard path.
 *
 * This component draws. It never lays out, never grades, and never invents a
 * quantity: every coordinate comes from the model. Hover only inspects.
 */
import type { JSX, KeyboardEvent, PointerEvent as ReactPointerEvent } from 'react';
import { useEffect, useId, useMemo, useRef, useState } from 'react';
import { formatMoment } from '../../workspaces/loom/tracks.ts';
import type { SceneDensity } from './density.ts';
import { glyphLabel, TemporalLegend } from './glyphs.tsx';
import { gradeDashArray, resolveTemporalPalette, type TemporalPalette } from './palette.ts';
import { pointsPath } from './spindle.ts';
import { sceneAnnotations } from './scene/annotations.ts';
import { focusAlpha, type SceneFrame, type TemporalSceneVariant } from './scene/frame.ts';
import { ClusterMark, FieldOverlay, LegendEncodings, laneDetail, NODE_CLASS, NodeMark, OverflowReadouts, RelationEvidence } from './scene/marks.tsx';
import { paintScene } from './scene/paint.ts';
import type {
  SceneCluster,
  SceneGap,
  SceneInterval,
  SceneLane,
  SceneNode,
  SceneWindow,
  TemporalSceneModel,
} from './types.ts';

export type { TemporalSceneVariant } from './scene/frame.ts';

export interface TemporalSceneProps {
  model: TemporalSceneModel;
  /** Visual treatment over the same laid-out evidence and interaction model. */
  variant?: TemporalSceneVariant;
  ariaLabel: string;
  onSelectLane: (laneId: string | null) => void;
  onSelectEvent: (eventId: string) => void;
  onToggleBranch: (laneId: string) => void;
  onSelectEncounter?: (encounterId: string) => void;
  onWindowChange: (window: SceneWindow) => void;
  /** Pixel width the caller should lay out for; reported when the host resizes. */
  onMeasure?: (width: number) => void;
  reducedMotion: boolean;
  onInspect?: (node: SceneNode | null) => void;
  className?: string;
  /** The whole projection extent, so Fit has somewhere to return to. */
  fullWindow?: SceneWindow;
  /** Per-lane density over the same window: summaries, totals, recency, NOW. */
  density?: SceneDensity | null;
}

/** Height of the time ruler strip across the top of the field. */
const RULER = 36;
/** Pointer travel under which a drag on the background is a click. */
const CLICK_SLOP_PX = 3;
const LABEL_CHAR_PX = 6.2;
const TOGGLE_WIDTH = 30;
const OPEN_TAIL_PX = 18;

type SceneLayer = 'canvas' | 'unavailable';

interface DragState {
  pointerId: number;
  startClientX: number;
  startClientY: number;
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
    reducedMotion,
    onInspect,
    className,
    fullWindow,
    density = null,
    variant = 'weave',
  } = props;
  const clipId = useId();
  const hostRef = useRef<HTMLElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const overlayRef = useRef<SVGSVGElement>(null);
  const dragRef = useRef<DragState | null>(null);
  const [palette, setPalette] = useState<TemporalPalette | null>(null);
  const [layer, setLayer] = useState<SceneLayer>('canvas');
  const [hover, setHover] = useState<string | null>(null);
  const [cursorId, setCursorId] = useState<string | null>(null);

  const { viewport, height } = model;
  const width = viewport.width;
  const window = viewport.window;
  const windowSpan = Math.max(spanOf(window), Number.EPSILON);
  const fieldX0 = viewport.left;
  const fieldX1 = width - viewport.right;
  const fieldWidth = Math.max(1, fieldX1 - fieldX0);
  const frame: SceneFrame = useMemo(
    () => ({ model, variant, density, fieldX0, fieldX1, top: RULER, height, highContrast: palette?.highContrast ?? false }),
    [model, variant, density, fieldX0, fieldX1, height, palette?.highContrast],
  );
  const annotations = useMemo(() => sceneAnnotations(frame), [frame]);

  const lanes = useMemo(() => [...model.lanes].sort((a, b) => a.row - b.row), [model.lanes]);
  const clusterByLane = useMemo(
    () => new Map(model.clusters.map((cluster) => [cluster.laneId, cluster])),
    [model.clusters],
  );
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
  /** Keyboard order: lanes top to bottom, then time within a lane. */
  const eventOrder = useMemo(() => {
    const rowOf = new Map(lanes.map((lane) => [lane.id, lane.row]));
    return [...model.nodes].sort(
      (a, b) => (rowOf.get(a.laneId) ?? 0) - (rowOf.get(b.laneId) ?? 0) || a.x - b.x || a.id.localeCompare(b.id),
    );
  }, [model.nodes, lanes]);
  const cursorIndex = (() => {
    const at = cursorId === null ? -1 : eventOrder.findIndex((node) => node.id === cursorId);
    if (at >= 0) return at;
    const selected = eventOrder.findIndex((node) => node.selected);
    return selected >= 0 ? selected : eventOrder.length > 0 ? 0 : -1;
  })();
  const cursorNode = cursorIndex >= 0 ? eventOrder[cursorIndex]! : null;
  const eventPosition = useMemo(() => new Map(eventOrder.map((node, index) => [node.id, index])), [eventOrder]);
  const eventDomId = (node: SceneNode): string => `${clipId}-event-${eventPosition.get(node.id) ?? 0}`;

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
    // Assigning a size reallocates the bitmap even when it is unchanged; a
    // window change repaints into the one it already has.
    const pixelWidth = Math.max(1, Math.round(width * dpr));
    const pixelHeight = Math.max(1, Math.round(height * dpr));
    if (canvas.width !== pixelWidth) canvas.width = pixelWidth;
    if (canvas.height !== pixelHeight) canvas.height = pixelHeight;
    const ctx = canvas.getContext('2d');
    if (!ctx) {
      setLayer('unavailable');
      return;
    }
    setLayer('canvas');
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, width, height);
    paintScene(ctx, frame, palette);
  }, [frame, palette, width, height]);

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
      startClientY: Number(event.clientY) || 0,
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
    const dy = ((Number(event.clientY) || 0) - drag.startClientY) * scale;
    // Let the page own a vertical touch scroll. A small horizontal drift must
    // not alter the time window before the browser cancels our pointer.
    if (!drag.moved && (Math.abs(dx) < CLICK_SLOP_PX || Math.abs(dy) > Math.abs(dx))) return;
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

  /** The nearest event in the next or previous lane that has any, by x. */
  const acrossLanes = (from: SceneNode, direction: 1 | -1): SceneNode | null => {
    const rows = [...new Set(eventOrder.map((node) => node.laneId))];
    const target = rows[rows.indexOf(from.laneId) + direction];
    if (target === undefined) return null;
    const candidates = eventOrder.filter((node) => node.laneId === target);
    return candidates.reduce((best, node) => (Math.abs(node.x - from.x) < Math.abs(best.x - from.x) ? node : best));
  };
  const onOverlayKey = (event: KeyboardEvent<SVGSVGElement>): void => {
    if (cursorNode === null) return;
    let next: SceneNode | null = null;
    switch (event.key) {
      case 'ArrowRight':
        next = eventOrder[Math.min(eventOrder.length - 1, cursorIndex + 1)]!;
        break;
      case 'ArrowLeft':
        next = eventOrder[Math.max(0, cursorIndex - 1)]!;
        break;
      case 'ArrowDown':
        next = acrossLanes(cursorNode, 1);
        break;
      case 'ArrowUp':
        next = acrossLanes(cursorNode, -1);
        break;
      case 'Home':
        next = eventOrder[0]!;
        break;
      case 'End':
        next = eventOrder[eventOrder.length - 1]!;
        break;
      case 'Enter':
      case ' ':
        event.preventDefault();
        onSelectEvent(cursorNode.id);
        return;
      default:
        return;
    }
    event.preventDefault();
    if (next === null) return;
    setCursorId(next.id);
    enterNode(next);
    // Focus follows the cursor when it already sits on a glyph; from the
    // overlay itself, aria-activedescendant carries it.
    if (event.target !== event.currentTarget) {
      (document.getElementById(eventDomId(next)) as SVGGElement | null)?.focus();
    }
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
      </g>
    );
  };

  const renderInterval = (interval: SceneInterval): JSX.Element => {
    const x1 = Math.max(interval.x1, interval.x0 + 2);
    if (interval.points) {
      const d = pointsPath(interval.points);
      if (interval.kind === 'git_span') return (
        <path key={interval.id} data-interval={interval.id} data-interval-kind={interval.kind} d={d} fill="none" stroke="var(--raw-graph-edge)" strokeWidth={3} opacity={0.8}>
          <title>{interval.label}</title>
        </path>
      );
      const select = (): void => { if (interval.ref !== null) onSelectEncounter?.(interval.ref); };
      return (
        <g key={interval.id} role="button" tabIndex={0} aria-label={`Open encounter ${interval.label}`} className="cursor-pointer outline-none [&:focus-visible>path]:stroke-white" onClick={select} onKeyDown={(event) => {
          if (isActivation(event)) { event.preventDefault(); select(); }
        }}>
          <title>{`${interval.label} · ${interval.grade}`}</title>
          <path d={d} fill="none" stroke="transparent" strokeWidth={44} />
          <path data-interval={interval.id} data-interval-kind={interval.kind} data-proximity-encounter={interval.ref ?? undefined} d={d} fill="none" stroke={proximityTone(interval.tone)} strokeWidth={5} strokeLinecap="round" opacity={0.85} pointerEvents="none" />
        </g>
      );
    }
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
        const hitWidth = Math.max(x1 - interval.x0 + 16, 44);
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
            <rect x={(interval.x0 + x1 - hitWidth) / 2} y={interval.y - 22} width={hitWidth} height={44} fill="transparent" />
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
        className="cursor-pointer outline-none [&:focus-visible>path]:stroke-[var(--raw-graph-accent)] [&:focus-visible>path]:[stroke-width:2]"
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
        <rect
          x={Math.min(cluster.x0, (cluster.x0 + x1) / 2 - 22)}
          y={Math.min(top, cluster.y - 22)}
          width={Math.max(x1 - cluster.x0, 44)}
          height={Math.max(cluster.height, 44)}
          fill="transparent"
        />
        <ClusterMark cluster={cluster} frame={frame} />
      </g>
    );
  };

  const renderNode = (node: SceneNode): JSX.Element => {
    const halfHit = Math.max(1, node.halfHit);
    const title = `${glyphLabel(node.kind)} · ${node.label}${node.detail ? ` · ${node.detail}` : ''} · ${node.grade}${node.xBasis === 'sequence' ? ' · recorded order, timestamp unrecorded' : ''}`;
    const select = (): void => {
      setCursorId(node.id);
      onSelectEvent(node.id);
    };
    return (
      <g
        key={node.id}
        id={eventDomId(node)}
        role="button"
        // One tab stop for the whole scene: the overlay owns it and arrow
        // keys walk the glyphs, so the hit band stays clear of neighbours.
        tabIndex={-1}
        data-event={node.id}
        data-event-current={node === cursorNode || undefined}
        data-lane={node.laneId}
        data-kind={node.kind}
        data-grade={node.grade}
        data-x-basis={node.xBasis}
        aria-label={`Select ${glyphLabel(node.kind)} ${node.label}`}
        aria-pressed={node.selected}
        className={NODE_CLASS}
        style={laneGroupStyle(node.laneId)}
        opacity={focusAlpha(node.focus)}
        onClick={select}
        onKeyDown={(event) => {
          if (isActivation(event)) {
            event.preventDefault();
            event.stopPropagation();
            select();
          }
        }}
        onMouseEnter={() => enterNode(node)}
        onFocus={() => {
          setCursorId(node.id);
          enterNode(node);
        }}
        onMouseLeave={leaveNode}
        onBlur={leaveNode}
      >
        <title>{title}</title>
        {node.hitRadius !== undefined
          ? <circle data-event-hit cx={node.x} cy={node.y} r={node.hitRadius} fill="transparent" />
          : <rect x={node.x - halfHit} y={node.y - 22} width={Math.max(2, halfHit * 2)} height={44} fill="transparent" />}
        <NodeMark node={node} frame={frame} />
      </g>
    );
  };

  const renderLaneRow = (lane: SceneLane): JSX.Element => {
    const rowHeight = Math.max(22, lane.height);
    const toggle = branchToggleFor(lane);
    const toggleWidth = toggle ? Math.max(TOGGLE_WIDTH, (String(toggle.count).length + 1) * LABEL_CHAR_PX + 8) : 0;
    // Deep hierarchy must not consume the whole compact label column. The
    // full name and exact branch action remain in the accessible label/title.
    const indent = Math.min(8 + lane.depth * 10, Math.max(8, viewport.left - toggleWidth - 70));
    const labelWidth = Math.max(0, viewport.left - toggleWidth - indent - 6);
    const twoLine = lane.height >= 28;
    const bundleCount = clusterByLane.get(lane.id)?.counts.sessions ?? lane.collapsedDescendants;
    const detailLine =
      laneDetail(lane, frame) ??
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
          <rect x={0} y={lane.y - rowHeight / 2} width={Math.max(1, viewport.left - toggleWidth)} height={rowHeight} fill="transparent" stroke="none" />
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
            <rect x={viewport.left - toggleWidth} y={lane.y - rowHeight / 2} width={toggleWidth} height={rowHeight} fill="transparent" stroke="none" />
            <text x={viewport.left - toggleWidth + 4} y={lane.y + 4} fontSize={10} fill="var(--raw-graph-accent)" pointerEvents="none">
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
      data-scene-variant={variant}
      data-scene-geometry={model.spindle ? 'spindle' : 'lanes'}
      data-scene-fibers={model.spindle?.fibers.length}
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
        {cursorNode ? (
          <span className="td-value ml-2 text-3xs text-text-muted" data-event-position>
            event {cursorIndex + 1} of {eventOrder.length}
          </span>
        ) : null}
      </div>
      <OverflowReadouts annotations={annotations} />
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
          aria-label="Temporal execution overlay. Arrow keys move between events, Home and End jump to the first and last, Enter selects."
          aria-activedescendant={cursorNode ? eventDomId(cursorNode) : undefined}
          tabIndex={eventOrder.length > 0 ? 0 : -1}
          data-scene-layer="overlay"
          onKeyDown={onOverlayKey}
          onBlur={(event) => {
            const next = event.relatedTarget;
            if (!(next instanceof Node) || !event.currentTarget.contains(next)) leaveNode();
          }}
          className="relative block touch-pan-y select-none outline-none [&:focus-visible_[data-event-current]>.td-focus-ring]:opacity-100"
          width="100%"
          viewBox={`0 0 ${width} ${height}`}
        >
          <defs>
            <clipPath id={clipId}>
              <rect x={fieldX0} y={RULER} width={fieldWidth} height={Math.max(0, height - RULER)} />
            </clipPath>
            <clipPath id={`${clipId}-lanes`}>
              <rect x={0} y={RULER} width={fieldX0} height={Math.max(0, height - RULER)} />
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
          <g clipPath={`url(#${clipId}-lanes)`}>
            {lanes.filter((lane) => !model.spindle || model.spindle.labelLaneIds.has(lane.id)).map((lane) => renderLaneRow(lane))}
          </g>
          <g clipPath={`url(#${clipId})`}>
            {model.spindle ? (
              <>
                {model.spindle.fibers.map((fiber) => (
                  <path
                    key={fiber.id}
                    data-spindle-fiber={fiber.id}
                    data-spindle-owner={fiber.ownerLaneId}
                    data-spindle-family={fiber.familyId}
                    role="button"
                    tabIndex={-1}
                    aria-label={`Inspect session thread ${fiber.label}`}
                    aria-pressed={fiber.focus === 'selected'}
                    d={pointsPath(fiber.points)}
                    fill="none"
                    stroke={layer === 'unavailable' ? frame.highContrast ? 'var(--raw-graph-text)' : fiber.color : 'transparent'}
                    strokeWidth={layer === 'unavailable' ? 1 : 8}
                    strokeDasharray={layer === 'unavailable' ? gradeDashArray(fiber.grade) || undefined : undefined}
                    className="cursor-pointer outline-none [&:focus-visible]:stroke-white"
                    onClick={() => onSelectLane(fiber.id)}
                    onKeyDown={(event) => {
                      if (isActivation(event)) { event.preventDefault(); onSelectLane(fiber.id); }
                    }}
                  >
                    <title>{`${fiber.label} · ${fiber.provider} · ${fiber.grade} · ${fiber.endSource ?? 'extent unknown'}`}</title>
                  </path>
                ))}
                {model.intervals.map((interval) => renderInterval(interval))}
                {model.gaps.map((gap) => renderGap(gap))}
              </>
            ) : lanes.map((lane) => renderLaneField(lane))}
            <g data-scene-events>
              {model.nodes.map((node) => renderNode(node))}
            </g>
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
          <FieldOverlay frame={frame} annotations={annotations} />
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
        {minimap.lanes.map((lane) => lane.points ? (
          <path
            key={lane.id}
            data-minimap-lane={lane.id}
            data-minimap-fiber
            d={pointsPath(lane.points)}
            fill="none"
            stroke={frame.highContrast ? 'var(--raw-graph-text)' : lane.color ?? 'var(--raw-graph-text)'}
            strokeWidth={0.75}
            strokeDasharray={lane.endSource === null ? '1 3' : undefined}
            opacity={0.7}
          />
        ) : (
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
      <RelationEvidence frame={frame} annotations={annotations} />
      <TemporalLegend gaps={model.gaps}>
        <LegendEncodings variant={variant} />
      </TemporalLegend>
    </section>
  );
}
