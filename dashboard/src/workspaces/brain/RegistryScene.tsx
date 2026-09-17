import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import type { ActivationField } from '../../viz/graph/activation.ts';
import { GraphUnavailable } from '../../viz/graph/GraphCanvas.tsx';
import { hasWebGl, watchWebGlContext } from '../../viz/graph/renderer.ts';
import { labelBudget, selectLabels, type LabelCandidate } from '../../viz/scene/labelPriority.ts';
import {
  createRegistryRuntime,
  type RegistryRuntime,
  type SceneView,
} from '../../viz/scene/registryRuntime.ts';
import {
  spreadExtent,
  spreadX,
  type RegistrySceneModel,
  type SceneBody,
} from '../../viz/scene/registrySceneModel.ts';
import {
  fitBounds,
  project,
  visibleBounds,
  zoomLevel,
  type Viewport,
} from '../../viz/scene/sceneCamera.ts';
import { useReducedMotion } from '../../viz/trace/reducedMotion.ts';
import { cn } from '../../ui/cn';

/**
 * The registry field's React host: the DOM half of the hybrid.
 *
 * Owns the container box, the WebGL availability and context-loss states, the
 * pointer wiring, the crisp overlays (axis, labels, minimap, camera readout)
 * and the camera controls. The luminous field itself is drawn by
 * `createRegistryRuntime`, which this component builds when the container
 * has a box and tears down when it loses one. Nothing here creates activity:
 * pointer focus and keyboard focus both arrive at the runtime as inspection,
 * and only the caller's `ActivationField` can warm a body.
 */
export function RegistryScene({
  model,
  activation,
  inspectedId,
  onInspect,
  onSelect,
  emphasis,
  ariaLabel,
  fallbackDescription,
  caption,
  detail,
  canvasClassName,
}: {
  model: RegistrySceneModel;
  activation: ActivationField;
  inspectedId: string | null;
  /** Pointer inspection leaving the field. `null` when the pointer leaves. */
  onInspect: (id: string | null) => void;
  /** A project body was clicked. Hubs never arrive here. */
  onSelect: (id: string) => void;
  /** Repository zoom: bodies to focus the camera on; others recede. */
  emphasis: ReadonlySet<string> | null;
  ariaLabel: string;
  fallbackDescription: string;
  caption: ReactNode;
  /** Secondary label lines for a body, printed when the zoom earns them. */
  detail: (body: SceneBody) => readonly string[];
  canvasClassName?: string;
}) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const runtimeRef = useRef<RegistryRuntime | null>(null);
  const contextWatchRef = useRef<(() => void) | null>(null);
  const resizeObserverRef = useRef<ResizeObserver | null>(null);
  const webglRef = useRef<boolean | null>(null);
  if (webglRef.current === null) webglRef.current = hasWebGl();
  const [box, setBox] = useState<Viewport>({ width: 0, height: 0 });
  const hasBox = box.width > 0 && box.height > 0;
  const [contextLostFor, setContextLostFor] = useState<RegistrySceneModel | null>(null);
  const [generation, setGeneration] = useState(0);
  const [view, setView] = useState<SceneView | null>(null);
  const { reduced } = useReducedMotion();
  const reducedRef = useRef(reduced);
  reducedRef.current = reduced;
  const onInspectRef = useRef(onInspect);
  onInspectRef.current = onInspect;
  const onSelectRef = useRef(onSelect);
  onSelectRef.current = onSelect;
  const emphasisRef = useRef(emphasis);
  emphasisRef.current = emphasis;
  const inspectedRef = useRef(inspectedId);
  inspectedRef.current = inspectedId;

  const attachContainer = useCallback((node: HTMLDivElement | null) => {
    containerRef.current = node;
    resizeObserverRef.current?.disconnect();
    resizeObserverRef.current = null;
    if (!node) {
      setBox({ width: 0, height: 0 });
      return;
    }
    const measure = (): void => {
      const width = node.clientWidth;
      const height = node.clientHeight;
      setBox((previous) =>
        previous.width === width && previous.height === height ? previous : { width, height },
      );
    };
    measure();
    if (typeof ResizeObserver !== 'function') return;
    const observer = new ResizeObserver(measure);
    observer.observe(node);
    resizeObserverRef.current = observer;
  }, []);

  useEffect(() => () => contextWatchRef.current?.(), []);

  // The runtime's lifetime: one per model while the container has a box.
  useEffect(() => {
    const container = containerRef.current;
    if (!container || !hasBox || !webglRef.current || model.bodies.length === 0) return;
    let runtime: RegistryRuntime;
    try {
      runtime = createRegistryRuntime({
        container,
        model,
        field: activation,
        isReduced: () => reducedRef.current,
        onView: setView,
      });
    } catch {
      // A renderer that cannot construct is the same absence as no context.
      webglRef.current = false;
      setGeneration((value) => value + 1);
      return;
    }
    runtimeRef.current = runtime;
    runtime.resize(box);
    if (emphasisRef.current) runtime.emphasize(emphasisRef.current);
    if (inspectedRef.current) runtime.focus(inspectedRef.current);
    contextWatchRef.current?.();
    contextWatchRef.current = watchWebGlContext([runtime.canvas], {
      onLost: () => {
        runtime.dispose();
        if (runtimeRef.current === runtime) runtimeRef.current = null;
        setContextLostFor(model);
      },
      onRestored: () => {
        setContextLostFor(null);
        setGeneration((value) => value + 1);
      },
    });
    return () => {
      runtime.dispose();
      if (runtimeRef.current === runtime) runtimeRef.current = null;
    };
    // The measured box is applied through `resize` below; only its presence
    // decides whether a runtime may exist.
  }, [model, activation, hasBox, generation]);

  useEffect(() => {
    if (hasBox) runtimeRef.current?.resize(box);
  }, [hasBox, box]);

  useEffect(() => {
    runtimeRef.current?.focus(inspectedId);
  }, [inspectedId]);

  useEffect(() => {
    runtimeRef.current?.emphasize(emphasis);
  }, [emphasis]);

  useEffect(() => {
    if (reduced) runtimeRef.current?.settle();
  }, [reduced]);

  useEffect(() => {
    const observer = new MutationObserver(() => runtimeRef.current?.retheme());
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme', 'data-contrast'] });
    return () => observer.disconnect();
  }, []);

  // Wheel zoom must cancel the page scroll, which a React `onWheel` (passive)
  // cannot, so it is attached natively for the container's lifetime.
  useEffect(() => {
    const container = containerRef.current;
    if (!container || !hasBox) return;
    const onWheel = (event: WheelEvent): void => {
      const runtime = runtimeRef.current;
      if (!runtime) return;
      event.preventDefault();
      const bounds = container.getBoundingClientRect();
      const factor = event.deltaY < 0 ? 1.12 : 1 / 1.12;
      runtime.zoomAt(factor, { px: event.clientX - bounds.left, py: event.clientY - bounds.top });
    };
    container.addEventListener('wheel', onWheel, { passive: false });
    return () => container.removeEventListener('wheel', onWheel);
  }, [hasBox]);

  const drag = useRef<{ x: number; y: number; moved: boolean } | null>(null);
  const hoveredRef = useRef<string | null>(null);

  const pickAt = (event: { clientX: number; clientY: number }): SceneBody | null => {
    const container = containerRef.current;
    const runtime = runtimeRef.current;
    if (!container || !runtime) return null;
    const bounds = container.getBoundingClientRect();
    return runtime.pick(event.clientX - bounds.left, event.clientY - bounds.top);
  };

  const labels = useMemo(() => {
    if (!view) return [];
    const { camera, viewport, spread, fit } = view;
    const zoom = zoomLevel(camera, fit);
    const detailed = zoom >= 1.5;
    const byMass = [...model.bodies]
      .filter((body) => body.kind === 'project')
      .sort((a, b) => b.mass - a.mass)
      .slice(0, 6)
      .map((body) => body.id);
    // Hover isolates the drawn neighbourhood: the inspected body and anything
    // a path joins it to keep full ink; the rest recedes with the scene.
    const neighborhood = inspectedId === null
      ? null
      : new Set([inspectedId, ...(model.pathsByBody.get(inspectedId) ?? []).flatMap((path) => [path.from, path.to])]);
    // A narrow aperture has no room for secondary lines at all.
    const roomy = viewport.width >= 480;
    const candidates: Array<LabelCandidate & { body: SceneBody; lines: readonly string[]; dimmed: boolean }> = model.bodies.map((body) => {
      const anchor = project(camera, viewport, spreadX(body.x, spread), body.y);
      const crownPx = body.radius / camera.scale;
      const lines =
        body.kind === 'repository'
          ? ['hub · massless']
          : roomy && (detailed || byMass.includes(body.id))
            ? detail(body)
            : [];
      const longest = Math.max(body.label.length + (body.kind === 'repository' ? 5 : 0), ...lines.map((line) => line.length));
      const width = longest * 6.6 + 8;
      const height = 14 + lines.length * 12;
      const offset = crownPx * (body.kind === 'repository' ? 2.6 : 1.05) + 6;
      // Beside the crown first, then the other side, then under the tail,
      // then above the crown: the first placement that prints whole wins.
      const placements = [
        { px: anchor.px + offset, py: anchor.py - 8 },
        { px: anchor.px - offset - width, py: anchor.py - 8 },
        { px: anchor.px - width / 2, py: anchor.py + crownPx * (body.kind === 'repository' ? 2.6 : 1.9) + 4 },
        { px: anchor.px - width / 2, py: anchor.py - crownPx * (body.kind === 'repository' ? 2.6 : 1.1) - height - 4 },
      ];
      return {
        id: body.id,
        priority: body.kind === 'repository' ? Number.MAX_SAFE_INTEGER : body.mass,
        placements,
        width,
        height,
        forced: body.id === inspectedId || emphasis?.has(body.id) === true,
        body,
        lines,
        dimmed: (neighborhood !== null && !neighborhood.has(body.id)) || (emphasis !== null && !emphasis.has(body.id)),
      };
    });
    // In a repository view only the members and an inspected body are named;
    // everything else has receded to context and keeps its name in the rail.
    const named = emphasis === null
      ? candidates
      : candidates.filter((candidate) => emphasis.has(candidate.id) || candidate.id === inspectedId);
    const chosen = selectLabels(named, viewport, labelBudget(zoom, model.bodies.length, viewport));
    return named.flatMap((candidate) => {
      const placed = chosen.get(candidate.id);
      return placed ? [{ ...candidate, px: placed.px, py: placed.py, side: placed.placement }] : [];
    });
  }, [view, model, detail, inspectedId, emphasis]);

  if (model.bodies.length === 0) {
    return <p className="p-6 text-center text-sm text-text-muted">no registered project to draw</p>;
  }
  if (!webglRef.current) {
    return (
      <GraphUnavailable>
        this browser has no WebGL context, so the {model.bodies.length.toLocaleString()}-body
        registry field cannot draw — {fallbackDescription}
      </GraphUnavailable>
    );
  }
  if (contextLostFor === model) {
    return (
      <GraphUnavailable>
        the registry field lost its WebGL context, so it is no longer being drawn —{' '}
        {fallbackDescription}, and the field returns if the browser restores the context
      </GraphUnavailable>
    );
  }

  const zoom = view ? zoomLevel(view.camera, view.fit) : 1;
  return (
    <figure className="relative flex h-full min-h-0 flex-col gap-1.5">
      <div
        ref={attachContainer}
        role="img"
        aria-label={ariaLabel}
        className={cn(
          'relative min-h-0 flex-1 cursor-crosshair overflow-hidden rounded-[var(--radius-card)] border border-edge-subtle/60 md:max-h-[62vw] lg:max-h-none',
          'td-graph-field td-grain td-scanlines shadow-[var(--shadow-field)]',
          canvasClassName,
        )}
        onPointerDown={(event) => {
          if (event.button !== 0) return;
          drag.current = { x: event.clientX, y: event.clientY, moved: false };
          event.currentTarget.setPointerCapture(event.pointerId);
        }}
        onPointerMove={(event) => {
          const runtime = runtimeRef.current;
          if (!runtime) return;
          if (drag.current && event.buttons === 1) {
            const dx = event.clientX - drag.current.x;
            const dy = event.clientY - drag.current.y;
            if (Math.abs(dx) + Math.abs(dy) > 2) drag.current.moved = true;
            if (drag.current.moved) {
              runtime.panBy(dx, dy);
              drag.current.x = event.clientX;
              drag.current.y = event.clientY;
            }
            return;
          }
          const hit = pickAt(event);
          const id = hit?.id ?? null;
          if (id === hoveredRef.current) return;
          hoveredRef.current = id;
          runtime.focus(id);
          onInspectRef.current(id);
        }}
        onPointerUp={(event) => {
          const wasDrag = drag.current?.moved === true;
          drag.current = null;
          event.currentTarget.releasePointerCapture(event.pointerId);
          if (wasDrag) return;
          const hit = pickAt(event);
          // Selection scopes only through a project body; a repository hub is
          // an identity that does not narrow scope.
          if (hit && hit.kind === 'project') onSelectRef.current(hit.id);
        }}
        onPointerCancel={() => {
          drag.current = null;
        }}
        onPointerLeave={() => {
          drag.current = null;
          if (hoveredRef.current === null) return;
          hoveredRef.current = null;
          runtimeRef.current?.focus(null);
          onInspectRef.current(null);
        }}
      >
        {view ? <AxisOverlay model={model} view={view} /> : null}
        {view ? (
          <div aria-hidden className="pointer-events-none absolute inset-0 z-10 select-none">
            {labels.map((label) => (
              <div
                key={label.id}
                className={cn(
                  'absolute flex flex-col leading-[12px] transition-opacity duration-150',
                  label.body.kind === 'repository' ? 'text-text-secondary' : 'text-text-primary',
                  label.dimmed && 'opacity-40',
                  label.side === 1 && 'items-end text-right',
                  label.side >= 2 && 'items-center text-center',
                )}
                style={{ left: label.px, top: label.py, width: label.width }}
              >
                <span className={cn('td-value whitespace-nowrap text-[11px]', label.id === inspectedId && 'text-accent')}>
                  {label.body.kind === 'repository' ? `repo:${label.body.label}` : label.body.label}
                </span>
                {label.lines.map((line) => (
                  <span key={line} className="td-value whitespace-nowrap text-[10px] text-text-muted">
                    {line}
                  </span>
                ))}
              </div>
            ))}
          </div>
        ) : null}
        <div
          role="group"
          aria-label="Registry field camera controls"
          className="absolute bottom-3 left-3 z-20 flex items-stretch border border-edge-subtle bg-surface-0/90 shadow-sm backdrop-blur-sm"
        >
          <button
            type="button"
            aria-label="Zoom out registry field"
            onClick={() => runtimeRef.current?.zoomOut()}
            className="td-hit border-r border-edge-subtle px-2 py-1 text-xs text-text-secondary hover:bg-surface-2 hover:text-text-primary"
          >
            −
          </button>
          <output
            aria-label="Registry field zoom"
            className="td-value flex min-w-12 items-center justify-center border-r border-edge-subtle px-2 text-2xs text-text-secondary"
          >
            {Math.round(zoom * 100)}%
          </output>
          <button
            type="button"
            aria-label="Zoom in registry field"
            onClick={() => runtimeRef.current?.zoomIn()}
            className="td-hit border-r border-edge-subtle px-2 py-1 text-xs text-text-secondary hover:bg-surface-2 hover:text-text-primary"
          >
            +
          </button>
          <button
            type="button"
            onClick={() => runtimeRef.current?.fit()}
            className="td-hit px-2 py-1 text-2xs text-text-secondary hover:bg-surface-2 hover:text-text-primary"
          >
            Fit
          </button>
        </div>
        {view && (emphasis !== null || Math.abs(zoom - 1) > 0.02) ? (
          <Minimap model={model} view={view} emphasis={emphasis} />
        ) : null}
      </div>
      <figcaption className="flex flex-col gap-1.5 text-2xs text-text-muted">
        <SceneKey />
        <div>{caption}</div>
      </figcaption>
    </figure>
  );
}

/** Column dividers, recency ticks and the mass axis, drawn crisp in screen
 * space from the same camera the scene uses. */
function AxisOverlay({ model, view }: { model: RegistrySceneModel; view: SceneView }) {
  const { camera, viewport, spread } = view;
  const top = project(camera, viewport, 0, model.massAxis.high);
  const bottom = project(camera, viewport, 0, model.massAxis.low);
  const axisX = project(camera, viewport, spreadX(model.extent.x[0], spread) + 0.06, 0).px;
  // Tick text degrades with the room a column has: both lines, the bound
  // alone, or dividers only. The caption below the field always prints every
  // column and its count in the DOM flow.
  const columnPx = spread / camera.scale;
  const tickLines = columnPx >= 104 ? 2 : columnPx >= 58 ? 1 : 0;
  return (
    <svg
      aria-hidden
      className="pointer-events-none absolute inset-0 z-[5] h-full w-full text-text-muted"
      width={viewport.width}
      height={viewport.height}
    >
      {model.columnDividers.map((x) => {
        const { px } = project(camera, viewport, spreadX(x, spread), 0);
        return (
          <line
            key={x}
            x1={px}
            x2={px}
            y1={0}
            y2={viewport.height}
            stroke="currentColor"
            strokeOpacity={0.18}
            strokeDasharray="2 6"
          />
        );
      })}
      {tickLines > 0
        ? model.columns.map((column) => {
            const { px } = project(camera, viewport, spreadX(column.x, spread), 0);
            return (
              <g key={column.id} transform={`translate(${px} 14)`} textAnchor="middle" className="td-legend">
                <text fill="currentColor" fillOpacity={0.9} fontSize={9} letterSpacing="0.14em">
                  {column.bound.toUpperCase()}
                </text>
                {tickLines > 1 ? (
                  <text y={12} fill="currentColor" fillOpacity={0.6} fontSize={9}>
                    {column.label} · {column.count}
                  </text>
                ) : null}
              </g>
            );
          })
        : null}
      <line
        x1={axisX}
        x2={axisX}
        y1={top.py}
        y2={bottom.py}
        stroke="currentColor"
        strokeOpacity={0.35}
        markerEnd="url(#td-mass-arrow)"
      />
      <defs>
        <marker id="td-mass-arrow" viewBox="0 0 8 8" refX={4} refY={4} markerWidth={6} markerHeight={6} orient="auto-start-reverse">
          <path d="M1 7 L4 1 L7 7" fill="none" stroke="currentColor" strokeOpacity={0.5} />
        </marker>
      </defs>
      <text x={axisX + 6} y={top.py + 4} fill="currentColor" fillOpacity={0.7} fontSize={9} className="td-legend">
        high mass
      </text>
      <text x={axisX + 6} y={bottom.py - 2} fill="currentColor" fillOpacity={0.7} fontSize={9} className="td-legend">
        low mass
      </text>
    </svg>
  );
}

function Minimap({
  model,
  view,
  emphasis,
}: {
  model: RegistrySceneModel;
  view: SceneView;
  emphasis: ReadonlySet<string> | null;
}) {
  const width = 150;
  const height = 84;
  const mini: Viewport = { width, height };
  const fit = fitBounds(spreadExtent(model.extent, view.spread), mini, 4);
  const window = visibleBounds(view.camera, view.viewport);
  const a = project(fit, mini, window.x[0], window.y[1]);
  const b = project(fit, mini, window.x[1], window.y[0]);
  return (
    <svg
      role="img"
      aria-label={
        emphasis
          ? `Registry minimap: ${emphasis.size} highlighted bodies belong to the focused repository; the frame is the current camera window`
          : 'Registry minimap: the frame is the current camera window over the whole field'
      }
      width={width}
      height={height}
      className="absolute bottom-3 right-3 z-20 border border-edge-subtle bg-surface-0/85 text-text-muted backdrop-blur-sm"
    >
      {model.bodies.map((body) => {
        const { px, py } = project(fit, mini, spreadX(body.x, view.spread), body.y);
        const highlighted = emphasis?.has(body.id) === true;
        return (
          <circle
            key={body.id}
            cx={px}
            cy={py}
            r={highlighted ? 2.4 : body.kind === 'repository' ? 1 : 1.5}
            fill="currentColor"
            className={highlighted ? 'text-accent' : undefined}
            fillOpacity={highlighted ? 1 : 0.35 + 0.5 * body.vitality}
          />
        );
      })}
      <rect
        x={Math.max(0, a.px)}
        y={Math.max(0, a.py)}
        width={Math.max(2, Math.min(width, b.px) - Math.max(0, a.px))}
        height={Math.max(2, Math.min(height, b.py) - Math.max(0, a.py))}
        fill="none"
        stroke="currentColor"
        strokeOpacity={0.8}
        className="text-accent"
      />
    </svg>
  );
}

function SceneKey() {
  const items = [
    ['body', 'one project'],
    ['size', 'indexed mass'],
    ['hue', 'project kind'],
    ['brightness · depth', 'recency; dormant recedes'],
    ['path', 'shared git directory (exact)'],
    ['amber bloom', 'admitted activity'],
    ['cyan ring', 'inspection only'],
  ] as const;
  return (
    <div
      aria-label="Registry field visual key"
      className="grid grid-cols-2 items-start gap-x-3 gap-y-1 border-y border-edge-subtle/70 py-1 sm:flex sm:flex-wrap sm:items-center sm:gap-x-4"
    >
      {items.map(([label, value]) => (
        <span key={label} className="flex min-w-0 flex-col gap-0.5 sm:inline-flex sm:flex-row sm:items-center sm:gap-1.5">
          <span className="td-legend">{label}</span>
          <span aria-hidden className="hidden text-text-muted sm:inline">·</span>
          <span className="td-value min-w-0 text-3xs leading-tight text-text-secondary">{value}</span>
        </span>
      ))}
    </div>
  );
}
