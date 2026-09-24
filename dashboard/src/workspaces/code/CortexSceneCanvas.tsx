/**
 * Browser wiring for the Cortex field's canvas.
 *
 * One canvas, one camera and one interaction model around a pure painter:
 * hover inspects without changing selection, click pins, the wheel zooms about
 * the pointer, a drag pans, and the keyboard walks the symbols in the ledger's
 * own order with a visible 2px cyan outline on the field and a bracket on the
 * symbol. Nothing animates: a repaint happens on input, on a strike of the
 * activation field, and while struck heat decays unless motion is reduced, in
 * which case the heat is drawn where it stands.
 *
 * Two states are printed rather than drawn, because a field that cannot be
 * composed must not be shown: a browser that hands back no 2D context, and a
 * layout that failed. The ledger beside the field keeps every symbol either
 * way.
 */
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';

import type { ActivationField } from '../../viz/graph/activation.ts';
import { useReducedMotion } from '../../viz/trace/reducedMotion.ts';
import { EvidencePattern } from '../../ui/EvidencePattern.tsx';
import { cn } from '../../ui/cn';
import {
  fitCamera,
  keyboardOrder,
  neighbourhood,
  project,
  zoomAt,
  type Camera,
  type CortexPainter,
  type CortexScene,
  type ScenePalette,
} from './cortexScene.ts';

const ZOOM_LIMITS = { min: 0.5, max: 8 } as const;
const DRAG_THRESHOLD_PX = 4;

function resolvePalette(element: HTMLElement): ScenePalette {
  const style = getComputedStyle(element);
  const token = (name: string, fallback: string): string =>
    style.getPropertyValue(name).trim() || fallback;
  return {
    substrate: token('--raw-graph-substrate', '#0b0e16'),
    dim: token('--raw-graph-dim', '#232c3f'),
    text: token('--raw-graph-text', '#c3cde0'),
    muted: token('--raw-text-muted', '#8d919b'),
    edge: token('--raw-graph-edge', '#39557a'),
    accent: token('--raw-graph-accent', '#5fd0e0'),
    unknown: token('--raw-state-unknown', '#8d919b'),
    danger: token('--raw-state-error', '#e0604f'),
  };
}

type LayoutState<L> =
  | { status: 'pending' }
  | { status: 'ready'; layout: L }
  | { status: 'failed'; reason: string };

export function CortexSceneCanvas<L>({
  scene,
  painter,
  selectedId,
  inspectedId,
  onSelect,
  onInspect,
  activation,
  ariaLabel,
  overlay,
  className,
}: {
  scene: CortexScene;
  painter: CortexPainter<L>;
  selectedId: string | null;
  inspectedId: string | null;
  onSelect: (id: string | null) => void;
  onInspect: (id: string | null) => void;
  activation?: ActivationField | undefined;
  ariaLabel: string;
  overlay?: ReactNode;
  className?: string;
}) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [box, setBox] = useState({ width: 0, height: 0 });
  const [noContext, setNoContext] = useState(false);
  const [layoutState, setLayoutState] = useState<LayoutState<L>>({ status: 'pending' });
  const [zoomPercent, setZoomPercent] = useState(100);
  const [cursor, setCursor] = useState<string | null>(null);
  const [hovered, setHovered] = useState<string | null>(null);
  const { reduced } = useReducedMotion();

  const cameraRef = useRef<Camera | null>(null);
  const fitKRef = useRef(1);
  const paletteRef = useRef<ScenePalette | null>(null);
  const frameRequest = useRef<number | null>(null);
  const state = useRef({ selectedId, inspectedId, hovered, cursor, reduced, layoutState, box });
  state.current = { selectedId, inspectedId, hovered, cursor, reduced, layoutState, box };

  const order = useMemo(() => keyboardOrder(scene), [scene]);

  const paint = useCallback(() => {
    frameRequest.current = null;
    const canvas = canvasRef.current;
    const host = hostRef.current;
    const current = state.current;
    if (!canvas || !host || current.layoutState.status !== 'ready') return;
    const { width, height } = current.box;
    if (width === 0 || height === 0) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;
    const dpr = window.devicePixelRatio || 1;
    const pixelWidth = Math.round(width * dpr);
    const pixelHeight = Math.round(height * dpr);
    if (canvas.width !== pixelWidth || canvas.height !== pixelHeight) {
      canvas.width = pixelWidth;
      canvas.height = pixelHeight;
    }
    paletteRef.current ??= resolvePalette(host);
    const layout = current.layoutState.layout;
    cameraRef.current ??= fitCamera(painter.bounds(layout), width, height, painter.fitPad);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, width, height);
    const emphasisId = current.hovered ?? current.inspectedId ?? current.cursor ?? current.selectedId;
    painter.draw(layout, scene, {
      ctx,
      width,
      height,
      camera: cameraRef.current,
      fitK: fitKRef.current,
      palette: paletteRef.current,
      hovered: current.hovered ?? current.inspectedId,
      selected: current.selectedId,
      cursor: current.cursor,
      emphasis: neighbourhood(scene, emphasisId),
      heat: (id) => activation?.heatOf(id) ?? 0,
    });
    // Struck heat decays in real time; with motion reduced it is held.
    if (activation?.warm && !current.reduced) {
      activation.tick(performance.now());
      frameRequest.current = requestAnimationFrame(paint);
    }
  }, [activation, painter, scene]);

  const schedule = useCallback(() => {
    if (frameRequest.current === null) frameRequest.current = requestAnimationFrame(paint);
  }, [paint]);

  useEffect(
    () => () => {
      if (frameRequest.current !== null) cancelAnimationFrame(frameRequest.current);
      frameRequest.current = null;
    },
    [],
  );

  // A 2D context is the one capability this surface needs.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    let ctx: CanvasRenderingContext2D | null = null;
    try {
      ctx = canvas.getContext('2d');
    } catch {
      ctx = null;
    }
    if (!ctx) setNoContext(true);
  }, []);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const measure = () => {
      const rect = host.getBoundingClientRect();
      setBox((previous) =>
        previous.width === Math.round(rect.width) && previous.height === Math.round(rect.height)
          ? previous
          : { width: Math.round(rect.width), height: Math.round(rect.height) },
      );
    };
    measure();
    if (typeof ResizeObserver !== 'function') return;
    const observer = new ResizeObserver(measure);
    observer.observe(host);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    const observer = new MutationObserver(() => {
      paletteRef.current = null;
      schedule();
    });
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme', 'data-contrast'],
    });
    return () => observer.disconnect();
  }, [schedule]);

  const hasBox = box.width > 0 && box.height > 0;
  useEffect(() => {
    if (noContext || !hasBox) return;
    let cancelled = false;
    setLayoutState({ status: 'pending' });
    cameraRef.current = null;
    Promise.resolve()
      .then(() => painter.layout(scene, state.current.box))
      .then(
        (layout) => {
          if (!cancelled) setLayoutState({ status: 'ready', layout });
        },
        (error: unknown) => {
          if (!cancelled) {
            setLayoutState({
              status: 'failed',
              reason: error instanceof Error ? error.message : 'the layout did not complete',
            });
          }
        },
      );
    return () => {
      cancelled = true;
    };
  }, [scene, painter, noContext, hasBox]);

  // Fit on a fresh layout or a resized box; keep the reader's camera otherwise.
  useEffect(() => {
    if (layoutState.status !== 'ready' || !hasBox) return;
    const fit = fitCamera(painter.bounds(layoutState.layout), box.width, box.height, painter.fitPad);
    const previousFit = fitKRef.current;
    fitKRef.current = fit.k;
    const camera = cameraRef.current;
    if (camera === null) {
      cameraRef.current = fit;
      setZoomPercent(100);
    } else {
      // Keep the zoom ratio across a resize, recentred on the new box.
      const ratio = camera.k / previousFit;
      cameraRef.current = zoomAt(fit, box.width / 2, box.height / 2, ratio, {
        min: fit.k * ZOOM_LIMITS.min,
        max: fit.k * ZOOM_LIMITS.max,
      });
    }
    schedule();
  }, [layoutState, box.width, box.height, hasBox, painter, schedule]);

  useEffect(() => {
    schedule();
  }, [selectedId, inspectedId, hovered, cursor, reduced, schedule]);

  useEffect(() => activation?.subscribe(schedule), [activation, schedule]);

  const setCamera = useCallback(
    (camera: Camera) => {
      cameraRef.current = camera;
      setZoomPercent(Math.round((camera.k / fitKRef.current) * 100));
      schedule();
    },
    [schedule],
  );

  const zoomBy = useCallback(
    (factor: number, px?: number, py?: number) => {
      const camera = cameraRef.current;
      if (!camera) return;
      const { width, height } = state.current.box;
      setCamera(
        zoomAt(camera, px ?? width / 2, py ?? height / 2, factor, {
          min: fitKRef.current * ZOOM_LIMITS.min,
          max: fitKRef.current * ZOOM_LIMITS.max,
        }),
      );
    },
    [setCamera],
  );

  const fit = useCallback(() => {
    const current = state.current;
    if (current.layoutState.status !== 'ready') return;
    setCamera(
      fitCamera(
        painter.bounds(current.layoutState.layout),
        current.box.width,
        current.box.height,
        painter.fitPad,
      ),
    );
  }, [painter, setCamera]);

  // Wheel zoom must be able to cancel the page scroll, so it is non-passive.
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const onWheel = (event: WheelEvent) => {
      if (state.current.layoutState.status !== 'ready') return;
      event.preventDefault();
      const rect = host.getBoundingClientRect();
      zoomBy(Math.exp(-event.deltaY * 0.0015), event.clientX - rect.left, event.clientY - rect.top);
    };
    host.addEventListener('wheel', onWheel, { passive: false });
    return () => host.removeEventListener('wheel', onWheel);
  }, [zoomBy]);

  const hitAt = useCallback(
    (sx: number, sy: number): string | null => {
      const current = state.current;
      const camera = cameraRef.current;
      if (current.layoutState.status !== 'ready' || !camera) return null;
      const layout = current.layoutState.layout;
      const frame = { camera, fitK: fitKRef.current };
      let best: string | null = null;
      let bestDistance = Infinity;
      for (const node of scene.nodes) {
        const world = painter.position(layout, node.id);
        if (!world) continue;
        const point = project(camera, world.x, world.y);
        const distance = Math.hypot(point.x - sx, point.y - sy);
        const reach = Math.max(8, painter.hitRadius(layout, scene, node.id, frame) + 4);
        if (distance <= reach && distance < bestDistance) {
          best = node.id;
          bestDistance = distance;
        }
      }
      return best;
    },
    [painter, scene],
  );

  const drag = useRef<{ x: number; y: number; camera: Camera; moved: boolean } | null>(null);

  const localPoint = (event: React.PointerEvent) => {
    const rect = event.currentTarget.getBoundingClientRect();
    return { x: event.clientX - rect.left, y: event.clientY - rect.top };
  };

  const onPointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    const camera = cameraRef.current;
    if (!camera || event.button !== 0) return;
    const point = localPoint(event);
    drag.current = { ...point, camera, moved: false };
  };

  const onPointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    const point = localPoint(event);
    const active = drag.current;
    if (active && event.buttons === 1) {
      const dx = point.x - active.x;
      const dy = point.y - active.y;
      if (active.moved || Math.hypot(dx, dy) > DRAG_THRESHOLD_PX) {
        if (!active.moved) event.currentTarget.setPointerCapture?.(event.pointerId);
        active.moved = true;
        setCamera({ k: active.camera.k, tx: active.camera.tx + dx, ty: active.camera.ty + dy });
        return;
      }
    }
    const hit = hitAt(point.x, point.y);
    if (hit !== state.current.hovered) {
      setHovered(hit);
      onInspect(hit);
    }
  };

  const onPointerUp = (event: React.PointerEvent<HTMLDivElement>) => {
    const active = drag.current;
    drag.current = null;
    if (!active || active.moved) return;
    const point = localPoint(event);
    const hit = hitAt(point.x, point.y);
    if (hit !== null) onSelect(hit);
  };

  const onPointerLeave = () => {
    drag.current = null;
    if (state.current.hovered !== null) {
      setHovered(null);
      onInspect(null);
    }
  };

  /** Pan just enough to bring the keyboard cursor's symbol inside the box. */
  const reveal = useCallback(
    (id: string) => {
      const current = state.current;
      const camera = cameraRef.current;
      if (current.layoutState.status !== 'ready' || !camera) return;
      const world = painter.position(current.layoutState.layout, id);
      if (!world) return;
      const point = project(camera, world.x, world.y);
      const margin = 40;
      const { width, height } = current.box;
      const dx =
        point.x < margin ? margin - point.x : point.x > width - margin ? width - margin - point.x : 0;
      const dy =
        point.y < margin ? margin - point.y : point.y > height - margin ? height - margin - point.y : 0;
      if (dx !== 0 || dy !== 0) setCamera({ k: camera.k, tx: camera.tx + dx, ty: camera.ty + dy });
    },
    [painter, setCamera],
  );

  const reachable = useMemo(() => {
    if (layoutState.status !== 'ready') return [];
    const layout = layoutState.layout;
    return order.filter((id) => painter.position(layout, id) !== null);
  }, [layoutState, order, painter]);

  const moveCursor = (step: number | 'first' | 'last') => {
    if (reachable.length === 0) return;
    const index = cursor === null ? -1 : reachable.indexOf(cursor);
    const next =
      step === 'first'
        ? 0
        : step === 'last'
          ? reachable.length - 1
          : index < 0
            ? step > 0
              ? 0
              : reachable.length - 1
            : (index + step + reachable.length) % reachable.length;
    const id = reachable[next]!;
    setCursor(id);
    onInspect(id);
    reveal(id);
  };

  const onKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    switch (event.key) {
      case 'ArrowRight':
      case 'ArrowDown':
        event.preventDefault();
        moveCursor(1);
        return;
      case 'ArrowLeft':
      case 'ArrowUp':
        event.preventDefault();
        moveCursor(-1);
        return;
      case 'Home':
        event.preventDefault();
        moveCursor('first');
        return;
      case 'End':
        event.preventDefault();
        moveCursor('last');
        return;
      case 'Enter':
      case ' ':
        if (cursor !== null) {
          event.preventDefault();
          onSelect(cursor);
        }
        return;
      case 'Escape':
        if (cursor !== null) {
          event.preventDefault();
          setCursor(null);
          onInspect(null);
        }
        return;
      case '+':
      case '=':
        event.preventDefault();
        zoomBy(1.25);
        return;
      case '-':
        event.preventDefault();
        zoomBy(0.8);
        return;
      case '0':
        event.preventDefault();
        fit();
        return;
      default:
        return;
    }
  };

  const cursorNode = cursor === null ? null : (scene.byId.get(cursor) ?? null);
  const announcement =
    cursorNode === null
      ? ''
      : `${cursorNode.label}, ${cursorNode.kind}, degree ${cursorNode.degree ?? 'absent'}, ${cursorNode.module}. ${reachable.indexOf(cursorNode.id) + 1} of ${reachable.length}. Enter pins.`;

  if (noContext) {
    return (
      <FieldUnavailable>
        this browser gave no 2D canvas, so the {scene.nodes.length.toLocaleString()}-symbol field
        is not drawn; the symbol list and inspector beside it carry every symbol
      </FieldUnavailable>
    );
  }
  if (layoutState.status === 'failed') {
    return (
      <FieldUnavailable>
        the {painter.name} layout could not be completed ({layoutState.reason}), so the{' '}
        {scene.nodes.length.toLocaleString()}-symbol field has no positions to draw; the symbol
        list and inspector beside it carry every symbol
      </FieldUnavailable>
    );
  }
  return (
    <div className={cn('td-night relative flex min-h-0 flex-1 flex-col', className)}>
      <div
        ref={hostRef}
        role="group"
        aria-roledescription="symbol field"
        aria-label={`${ariaLabel} Arrow keys walk the symbols by degree, Enter pins, plus and minus zoom, 0 fits.`}
        tabIndex={0}
        onKeyDown={onKeyDown}
        onBlur={() => setCursor(null)}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerLeave={onPointerLeave}
        className={cn(
          'td-graph-field relative min-h-[52vw] flex-1 touch-none overflow-hidden border border-edge-subtle/60 md:min-h-[46vh] lg:min-h-[18rem]',
          'focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent',
          hovered !== null ? 'cursor-pointer' : 'cursor-grab',
        )}
        data-cortex-render={painter.name}
        data-layout={layoutState.status}
      >
        <canvas ref={canvasRef} aria-hidden className="absolute inset-0 block size-full" />
      </div>
      {layoutState.status === 'pending' ? (
        <p
          role="status"
          data-state="loading"
          className="absolute left-3 top-3 bg-surface-0/90 p-2 text-2xs text-text-secondary"
        >
          Calculating positions. The symbol list remains available.
        </p>
      ) : null}
      {overlay ? (
        <div className="pointer-events-none absolute inset-0 z-[5]" data-graph-overlay>
          {overlay}
        </div>
      ) : null}
      <div
        role="group"
        aria-label="Field camera"
        className="absolute right-3 top-3 z-10 flex items-stretch border border-edge-subtle bg-surface-0/90"
      >
        <CameraButton label="Zoom out field" onClick={() => zoomBy(0.8)}>
          −
        </CameraButton>
        <span
          className="td-value flex min-w-12 items-center justify-center border-r border-edge-subtle px-1.5 text-2xs text-text-secondary"
          aria-label={`zoom ${zoomPercent} percent of fit`}
        >
          {zoomPercent}%
        </span>
        <CameraButton label="Zoom in field" onClick={() => zoomBy(1.25)}>
          +
        </CameraButton>
        <CameraButton label="Fit field" onClick={fit} last>
          Fit
        </CameraButton>
      </div>
      <p className="sr-only" aria-live="polite">
        {announcement}
      </p>
    </div>
  );
}

function CameraButton({
  label,
  onClick,
  children,
  last = false,
}: {
  label: string;
  onClick: () => void;
  children: ReactNode;
  last?: boolean;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      onClick={onClick}
      className={cn(
        'td-hit px-2 py-1 text-xs text-text-secondary hover:bg-surface-2 hover:text-text-primary',
        'focus-visible:outline focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent',
        !last && 'border-r border-edge-subtle',
      )}
    >
      {children}
    </button>
  );
}

/**
 * A field that is not being drawn. Distinct from an empty field on purpose:
 * it wears the dashed `unknown` evidence pattern rather than the aperture, so
 * a failure never looks like a sparse graph.
 */
function FieldUnavailable({ children }: { children: ReactNode }) {
  return (
    <div
      data-state="unavailable"
      role="status"
      aria-live="polite"
      className="flex flex-col items-center gap-2 border-y border-dashed border-edge-strong bg-surface-1 p-6 text-center"
    >
      <span
        aria-hidden
        className="h-1 w-full max-w-40 opacity-70"
        style={{ backgroundImage: 'var(--ev-unknown)' }}
      />
      <p className="text-sm text-text-secondary">{children}</p>
      <EvidencePattern quality="unknown" />
    </div>
  );
}
