import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  BoxSelect,
  Flame,
  Hand,
  RotateCcw,
  X,
  ZoomIn,
  ZoomOut,
} from "lucide-react";
import { Badge, Button, Card, CardContent, CardHeader, CardTitle } from "./sdk";
import { api } from "./api";
import type {
  MemoryProjectionResponse,
  MemoryProjectionPoint,
} from "./types";
import { NUM_BADGE, truncate } from "./ui";
import { categoryColorMap } from "./viz/colors";
import { extent, padDomain, scaleLinear } from "./viz/scale";
import { useMeasuredWidth } from "./viz/useMeasure";
import { useVirtualList } from "./viz/useVirtualList";
import { VizLegend } from "./viz/Legend";
import { VizTooltip, TipRow, TipTitle, type TooltipState } from "./viz/Tooltip";
import { PanelError, PanelLoading } from "./viz/Status";

/* ------------------------------------------------------------------ scatter */

interface MapTransform {
  k: number;
  tx: number;
  ty: number;
}

const IDENTITY: MapTransform = { k: 1, tx: 0, ty: 0 };
const MIN_ZOOM = 0.5;
const MAX_ZOOM = 24;
/** Cap for programmatic fit-zoom so tiny clusters keep some context. */
const FIT_MAX_ZOOM = 8;
/** Spatial-grid cell size (base px) for O(1) hover hit-testing. */
const GRID_CELL = 48;
/** Hover snap radius in screen px. */
const HOVER_RADIUS = 22;
/** Density underlay kicks in automatically above this point count. */
const DENSITY_AUTO_THRESHOLD = 350;

interface PlacedPoint {
  point: MemoryProjectionPoint;
  x: number;
  y: number;
  r: number;
  color: string;
}

function buildGrid(placed: PlacedPoint[]): Map<string, number[]> {
  const grid = new Map<string, number[]>();
  placed.forEach((p, i) => {
    const key = `${Math.floor(p.x / GRID_CELL)},${Math.floor(p.y / GRID_CELL)}`;
    const cell = grid.get(key);
    if (cell) cell.push(i);
    else grid.set(key, [i]);
  });
  return grid;
}

function findNearest(
  placed: PlacedPoint[],
  grid: Map<string, number[]>,
  bx: number,
  by: number,
  radius: number,
): PlacedPoint | null {
  const c0 = Math.floor((bx - radius) / GRID_CELL);
  const c1 = Math.floor((bx + radius) / GRID_CELL);
  const r0 = Math.floor((by - radius) / GRID_CELL);
  const r1 = Math.floor((by + radius) / GRID_CELL);
  let best: PlacedPoint | null = null;
  let bestD = radius * radius;
  for (let cx = c0; cx <= c1; cx++) {
    for (let cy = r0; cy <= r1; cy++) {
      const cell = grid.get(`${cx},${cy}`);
      if (!cell) continue;
      for (const i of cell) {
        const p = placed[i];
        const dx = p.x - bx;
        const dy = p.y - by;
        const d = dx * dx + dy * dy;
        if (d <= bestD) {
          bestD = d;
          best = p;
        }
      }
    }
  }
  return best;
}

interface DensityCell {
  x: number;
  y: number;
  w: number;
  h: number;
  opacity: number;
}

function buildDensity(placed: PlacedPoint[], baseW: number, baseH: number): DensityCell[] {
  const cols = Math.max(8, Math.round(baseW / 30));
  const rows = Math.max(6, Math.round(baseH / 30));
  const cw = baseW / cols;
  const ch = baseH / rows;
  const counts = new Float32Array(cols * rows);
  for (const p of placed) {
    const cx = Math.min(cols - 1, Math.max(0, Math.floor(p.x / cw)));
    const cy = Math.min(rows - 1, Math.max(0, Math.floor(p.y / ch)));
    counts[cy * cols + cx] += 1;
  }
  // One smoothing pass (3x3 box blur) so the underlay reads as contours, not pixels.
  const blurred = new Float32Array(cols * rows);
  for (let y = 0; y < rows; y++) {
    for (let x = 0; x < cols; x++) {
      let sum = 0;
      let n = 0;
      for (let dy = -1; dy <= 1; dy++) {
        for (let dx = -1; dx <= 1; dx++) {
          const xx = x + dx;
          const yy = y + dy;
          if (xx < 0 || yy < 0 || xx >= cols || yy >= rows) continue;
          sum += counts[yy * cols + xx];
          n += 1;
        }
      }
      blurred[y * cols + x] = sum / n;
    }
  }
  let max = 0;
  for (const v of blurred) if (v > max) max = v;
  if (max <= 0) return [];
  const cells: DensityCell[] = [];
  for (let y = 0; y < rows; y++) {
    for (let x = 0; x < cols; x++) {
      const v = blurred[y * cols + x];
      if (v <= 0.01) continue;
      cells.push({
        x: x * cw,
        y: y * ch,
        w: cw + 0.5,
        h: ch + 0.5,
        opacity: Math.min(0.34, (v / max) * 0.34),
      });
    }
  }
  return cells;
}

/* --------------------------------------------------------------- component */

type DragState =
  | { kind: "pan"; startX: number; startY: number; origin: MapTransform; moved: boolean }
  | { kind: "select"; startX: number; startY: number; x: number; y: number };

/** Pointer capture can throw for synthetic/stale pointers; never let that kill the gesture. */
function capturePointer(target: Element, pointerId: number) {
  try {
    target.setPointerCapture(pointerId);
  } catch {
    /* noop */
  }
}

function releasePointer(target: Element, pointerId: number) {
  try {
    target.releasePointerCapture(pointerId);
  } catch {
    /* noop */
  }
}

/**
 * Cross-view navigation payload (e.g. "show this similarity pair on the
 * map"): facts to select, an optional fact to pin, and a token so repeat
 * navigations to the same facts re-apply.
 */
export interface SemanticMapFocus {
  ids: number[];
  pinId?: number;
  token: number;
}

export default function SemanticMap({
  query,
  focus,
}: {
  query?: string;
  focus?: SemanticMapFocus | null;
}) {
  const [data, setData] = useState<MemoryProjectionResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [selected, setSelected] = useState<MemoryProjectionPoint | null>(null);
  const [colorBy, setColorBy] = useState<"category" | "bank">("category");
  const [sizeBy, setSizeBy] = useState<"retrievals" | "connections">("retrievals");
  const [hiddenCats, setHiddenCats] = useState<ReadonlySet<string>>(new Set());
  const [selection, setSelection] = useState<ReadonlySet<number> | null>(null);
  const [transform, setTransform] = useState<MapTransform>(IDENTITY);
  const [mode, setMode] = useState<"pan" | "select">("pan");
  const [densityOverride, setDensityOverride] = useState<boolean | null>(null);
  const [tip, setTip] = useState<TooltipState | null>(null);
  const [hovered, setHovered] = useState<PlacedPoint | null>(null);
  const [drag, setDragState] = useState<DragState | null>(null);
  // Mirror drag state in a ref: pointerdown/up can land in the same task
  // (fast clicks, synthetic events) before React commits the state update.
  const dragRef = useRef<DragState | null>(null);
  const setDrag = useCallback((next: DragState | null) => {
    dragRef.current = next;
    setDragState(next);
  }, []);

  const svgRef = useRef<SVGSVGElement>(null);
  const zoomGroupRef = useRef<SVGGElement>(null);
  const transformRef = useRef(transform);
  const commitTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const rafRef = useRef(0);

  /**
   * Gesture transforms are applied imperatively (one style write on the zoom
   * group) and committed to React state only when the gesture settles. At a
   * few thousand points, a per-tick state commit plus the `--hv-k` CSS-var
   * invalidation (which recalcs every circle's calc() radius) produced
   * multi-hundred-ms tasks; deferring both keeps wheel/drag at frame rate.
   */
  const applyTransform = useCallback((next: MapTransform, settle = false) => {
    transformRef.current = next;
    const g = zoomGroupRef.current;
    if (g) {
      g.style.transform = `translate(${next.tx}px, ${next.ty}px) scale(${next.k})`;
      if (settle) g.style.setProperty("--hv-k", String(next.k));
    }
    if (commitTimerRef.current) clearTimeout(commitTimerRef.current);
    if (settle) {
      setTransform(next);
    } else {
      commitTimerRef.current = setTimeout(() => {
        const g2 = zoomGroupRef.current;
        if (g2) g2.style.setProperty("--hv-k", String(transformRef.current.k));
        setTransform(transformRef.current);
      }, 90);
    }
  }, []);

  useEffect(
    () => () => {
      if (commitTimerRef.current) clearTimeout(commitTimerRef.current);
    },
    [],
  );

  const [measureRef, width] = useMeasuredWidth<HTMLDivElement>(760);
  const height = Math.round(Math.min(640, Math.max(400, width * 0.62)));

  const [reloadKey, setReloadKey] = useState(0);
  const [lastQuery, setLastQuery] = useState(query);
  if (query !== lastQuery) {
    setLastQuery(query);
    setLoading(true);
    setError("");
    setSelection(null);
  }

  useEffect(() => {
    let cancelled = false;
    api
      // The server currently caps projection points at 200; ask for more so a
      // raised cap lights up automatically.
      .getMemoryProjection({ q: query, limit: 2000 })
      .then((resp) => {
        if (!cancelled) setData(resp);
      })
      .catch((err) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [query, reloadKey]);

  const points = useMemo(() => data?.points ?? [], [data]);

  // Older servers don't send bank/connection fields; hide those encodings.
  const hasBanks = useMemo(() => points.some((p) => !!p.bank_name), [points]);
  const hasConnections = useMemo(
    () => points.some((p) => p.connection_count != null),
    [points],
  );
  const effectiveColorBy = colorBy === "bank" && !hasBanks ? "category" : colorBy;
  const effectiveSizeBy =
    sizeBy === "connections" && !hasConnections ? "retrievals" : sizeBy;

  const groupOf = useCallback(
    (p: MemoryProjectionPoint) =>
      effectiveColorBy === "bank" ? p.bank_name || "no bank" : p.category,
    [effectiveColorBy],
  );
  const sizeValue = useCallback(
    (p: MemoryProjectionPoint) =>
      effectiveSizeBy === "connections"
        ? (p.connection_count ?? 0)
        : p.retrieval_count,
    [effectiveSizeBy],
  );

  const colorMap = useMemo(
    () => categoryColorMap(points.map(groupOf)),
    [points, groupOf],
  );

  const legendItems = useMemo(() => {
    const counts = new Map<string, number>();
    for (const p of points) {
      const g = groupOf(p);
      counts.set(g, (counts.get(g) ?? 0) + 1);
    }
    return Array.from(counts.entries())
      .sort((a, b) => b[1] - a[1])
      .map(([group, count]) => ({
        key: group,
        label: group,
        color: colorMap.get(group) ?? "var(--hm-primary)",
        count,
      }));
  }, [points, colorMap, groupOf]);

  const visiblePoints = useMemo(
    () => points.filter((p) => !hiddenCats.has(groupOf(p))),
    [points, hiddenCats, groupOf],
  );

  /** Base-space (k=1) layout: scales, placed points, hit grid, density. */
  const layout = useMemo(() => {
    const pad = 28;
    const xScale = scaleLinear(
      padDomain(extent(visiblePoints.map((p) => p.x))),
      [pad, width - pad],
    );
    const yScale = scaleLinear(
      padDomain(extent(visiblePoints.map((p) => p.y))),
      [height - pad, pad],
    );
    const maxSize = Math.max(1, ...visiblePoints.map(sizeValue));
    const placed: PlacedPoint[] = visiblePoints.map((p) => ({
      point: p,
      x: xScale(p.x),
      y: yScale(p.y),
      r: 3.5 + Math.sqrt(sizeValue(p) / maxSize) * 6.5,
      color: colorMap.get(groupOf(p)) ?? "var(--hm-primary)",
    }));
    return {
      placed,
      grid: buildGrid(placed),
      density: buildDensity(placed, width, height),
      byId: new Map(placed.map((p) => [p.point.fact_id, p])),
    };
  }, [visiblePoints, width, height, colorMap, groupOf, sizeValue]);

  const showDensity =
    densityOverride ?? visiblePoints.length >= DENSITY_AUTO_THRESHOLD;

  /* ------------------------------------------------------------ transforms */

  const screenToBase = useCallback((sx: number, sy: number) => {
    const { k, tx, ty } = transformRef.current;
    return [(sx - tx) / k, (sy - ty) / k] as const;
  }, []);

  const localPos = useCallback((event: { clientX: number; clientY: number }) => {
    const rect = svgRef.current?.getBoundingClientRect();
    return rect
      ? ([event.clientX - rect.left, event.clientY - rect.top] as const)
      : ([0, 0] as const);
  }, []);

  const zoomAt = useCallback(
    (sx: number, sy: number, factor: number) => {
      const prev = transformRef.current;
      const k = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, prev.k * factor));
      if (k === prev.k) return;
      const scale = k / prev.k;
      applyTransform({
        k,
        tx: sx - (sx - prev.tx) * scale,
        ty: sy - (sy - prev.ty) * scale,
      });
    },
    [applyTransform],
  );

  /**
   * Zoom/pan so the given points fill the viewport (with margin). Recovery
   * path for deep zooms into empty space and the landing move for cross-view
   * focus navigation.
   */
  const fitToPlaced = useCallback(
    (targets: PlacedPoint[]) => {
      if (targets.length === 0) return;
      let x0 = Infinity;
      let y0 = Infinity;
      let x1 = -Infinity;
      let y1 = -Infinity;
      for (const p of targets) {
        x0 = Math.min(x0, p.x);
        y0 = Math.min(y0, p.y);
        x1 = Math.max(x1, p.x);
        y1 = Math.max(y1, p.y);
      }
      const margin = 48;
      const spanX = Math.max(x1 - x0, 1e-6);
      const spanY = Math.max(y1 - y0, 1e-6);
      const fitK = Math.min(
        (width - margin * 2) / spanX,
        (height - margin * 2) / spanY,
      );
      const k = Math.min(FIT_MAX_ZOOM, Math.max(MIN_ZOOM, fitK));
      const cx = (x0 + x1) / 2;
      const cy = (y0 + y1) / 2;
      applyTransform(
        { k, tx: width / 2 - cx * k, ty: height / 2 - cy * k },
        true,
      );
    },
    [applyTransform, width, height],
  );

  // Apply a cross-view focus request once per token: select the facts, pin
  // the requested one, and zoom the viewport to them.
  const appliedFocusTokenRef = useRef<number | null>(null);
  useEffect(() => {
    if (!focus || loading) return;
    if (appliedFocusTokenRef.current === focus.token) return;
    const targets = focus.ids
      .map((id) => layout.byId.get(id))
      .filter((p): p is PlacedPoint => !!p);
    if (targets.length === 0) return;
    appliedFocusTokenRef.current = focus.token;
    setSelection(new Set(targets.map((p) => p.point.fact_id)));
    const pin =
      (focus.pinId != null ? layout.byId.get(focus.pinId) : undefined) ??
      targets[0];
    setSelected(pin.point);
    fitToPlaced(targets);
  }, [focus, loading, layout, fitToPlaced]);

  // True when at least one plotted fact is inside the viewport for the
  // committed transform; drives the empty-view recovery overlay.
  const hasVisiblePoint = useMemo(() => {
    const { k, tx, ty } = transform;
    for (const p of layout.placed) {
      const sx = p.x * k + tx;
      const sy = p.y * k + ty;
      if (sx >= 0 && sx <= width && sy >= 0 && sy <= height) return true;
    }
    return false;
  }, [layout, transform, width, height]);

  // Wheel zoom needs a non-passive native listener to preventDefault page
  // scroll. The svg mounts late (after loading), so bind via callback ref.
  const wheelCleanupRef = useRef<(() => void) | null>(null);
  const bindSvg = useCallback(
    (node: SVGSVGElement | null) => {
      svgRef.current = node;
      wheelCleanupRef.current?.();
      wheelCleanupRef.current = null;
      if (!node) return;
      const onWheel = (event: WheelEvent) => {
        event.preventDefault();
        const rect = node.getBoundingClientRect();
        const sx = event.clientX - rect.left;
        const sy = event.clientY - rect.top;
        cancelAnimationFrame(rafRef.current);
        rafRef.current = requestAnimationFrame(() =>
          zoomAt(sx, sy, Math.exp(-event.deltaY * 0.0021)),
        );
      };
      node.addEventListener("wheel", onWheel, { passive: false });
      wheelCleanupRef.current = () => {
        node.removeEventListener("wheel", onWheel);
        cancelAnimationFrame(rafRef.current);
      };
    },
    [zoomAt],
  );
  useEffect(() => () => wheelCleanupRef.current?.(), []);

  /* -------------------------------------------------------------- pointers */

  const updateHover = useCallback(
    (sx: number, sy: number) => {
      const [bx, by] = screenToBase(sx, sy);
      const radius = HOVER_RADIUS / transformRef.current.k;
      const hit = findNearest(layout.placed, layout.grid, bx, by, radius);
      setHovered(hit);
      if (!hit) {
        setTip(null);
        return;
      }
      setTip({
        x: sx,
        y: sy,
        content: (
          <>
            <TipTitle color={hit.color}>
              #{hit.point.fact_id} · {hit.point.category}
            </TipTitle>
            <p className="hv-tooltip-body">{truncate(hit.point.content, 160)}</p>
            <TipRow
              label="trust"
              value={Number(hit.point.trust_score ?? 0).toFixed(2)}
            />
            <TipRow label="retrievals" value={hit.point.retrieval_count} />
            {hit.point.bank_name && (
              <TipRow label="bank" value={hit.point.bank_name} />
            )}
            {hit.point.connection_count != null && (
              <TipRow label="connections" value={hit.point.connection_count} />
            )}
            {hit.point.entity_count != null && (
              <TipRow label="entities" value={hit.point.entity_count} />
            )}
          </>
        ),
      });
    },
    [layout, screenToBase],
  );

  const onPointerDown = (event: React.PointerEvent<SVGSVGElement>) => {
    if (event.button !== 0) return;
    capturePointer(event.currentTarget, event.pointerId);
    const [sx, sy] = localPos(event);
    const wantSelect = mode === "select" || event.shiftKey;
    setTip(null);
    if (wantSelect) {
      setDrag({ kind: "select", startX: sx, startY: sy, x: sx, y: sy });
    } else {
      setDrag({ kind: "pan", startX: sx, startY: sy, origin: transformRef.current, moved: false });
    }
  };

  const onPointerMove = (event: React.PointerEvent<SVGSVGElement>) => {
    const [sx, sy] = localPos(event);
    const drag = dragRef.current;
    if (!drag) {
      cancelAnimationFrame(rafRef.current);
      rafRef.current = requestAnimationFrame(() => updateHover(sx, sy));
      return;
    }
    if (drag.kind === "pan") {
      const dx = sx - drag.startX;
      const dy = sy - drag.startY;
      if (!drag.moved && Math.hypot(dx, dy) > 3) {
        setDrag({ ...drag, moved: true });
      }
      applyTransform({
        k: drag.origin.k,
        tx: drag.origin.tx + dx,
        ty: drag.origin.ty + dy,
      });
    } else {
      setDrag({ ...drag, x: sx, y: sy });
    }
  };

  const onPointerUp = (event: React.PointerEvent<SVGSVGElement>) => {
    releasePointer(event.currentTarget, event.pointerId);
    const current = dragRef.current;
    setDrag(null);
    if (!current) return;
    if (current.kind === "select") {
      const x0 = Math.min(current.startX, current.x);
      const x1 = Math.max(current.startX, current.x);
      const y0 = Math.min(current.startY, current.y);
      const y1 = Math.max(current.startY, current.y);
      if (x1 - x0 < 4 && y1 - y0 < 4) {
        setSelection(null);
        return;
      }
      const { k, tx, ty } = transformRef.current;
      const ids = new Set<number>();
      for (const p of layout.placed) {
        const sx = p.x * k + tx;
        const sy = p.y * k + ty;
        if (sx >= x0 && sx <= x1 && sy >= y0 && sy <= y1) ids.add(p.point.fact_id);
      }
      setSelection(ids.size > 0 ? ids : null);
      return;
    }
    // Pan gesture that never moved = click: pin the hovered point (or clear).
    if (!current.moved) {
      const [sx, sy] = localPos(event);
      const [bx, by] = screenToBase(sx, sy);
      const hit = findNearest(
        layout.placed,
        layout.grid,
        bx,
        by,
        HOVER_RADIUS / transformRef.current.k,
      );
      setSelected(hit ? hit.point : null);
    }
  };

  // Keyboard equivalents for pan/zoom/clear so the map isn't pointer-only.
  const onKeyDown = (event: React.KeyboardEvent<SVGSVGElement>) => {
    const PAN_STEP = 48;
    const pan = (dx: number, dy: number) => {
      const prev = transformRef.current;
      applyTransform({ ...prev, tx: prev.tx + dx, ty: prev.ty + dy });
    };
    switch (event.key) {
      case "ArrowLeft":
        pan(PAN_STEP, 0);
        break;
      case "ArrowRight":
        pan(-PAN_STEP, 0);
        break;
      case "ArrowUp":
        pan(0, PAN_STEP);
        break;
      case "ArrowDown":
        pan(0, -PAN_STEP);
        break;
      case "+":
      case "=":
        zoomAt(width / 2, height / 2, 1.3);
        break;
      case "-":
      case "_":
        zoomAt(width / 2, height / 2, 1 / 1.3);
        break;
      case "0":
        applyTransform(IDENTITY, true);
        break;
      case "Escape":
        setSelection(null);
        setSelected(null);
        break;
      default:
        return;
    }
    event.preventDefault();
  };

  /* ----------------------------------------------------------------- layers */

  const pointsLayer = useMemo(
    () => (
      <g style={{ pointerEvents: "none" }}>
        {layout.placed.map((p) => (
          <g
            key={p.point.fact_id}
            className="hv-pt"
            style={{ transform: `translate(${p.x.toFixed(1)}px, ${p.y.toFixed(1)}px)` }}
          >
            <circle
              r={p.r}
              vectorEffect="non-scaling-stroke"
              style={{
                // CSS geometry `r` lets the dots keep constant screen size while
                // the parent group zooms — no per-point re-render per frame.
                r: `calc(${p.r}px / var(--hv-k, 1))` as never,
                fill: p.color,
                stroke: p.color,
                fillOpacity: 0.62,
                strokeOpacity: 0.95,
              }}
            />
          </g>
        ))}
      </g>
    ),
    [layout],
  );

  const densityLayer = useMemo(() => {
    if (!showDensity) return null;
    return (
      <g style={{ pointerEvents: "none" }}>
        {layout.density.map((cell, i) => (
          <rect
            key={i}
            x={cell.x}
            y={cell.y}
            width={cell.w}
            height={cell.h}
            style={{ fill: "var(--hm-primary)", opacity: cell.opacity }}
          />
        ))}
      </g>
    );
  }, [layout, showDensity]);

  const markerFor = (factId: number | undefined, className: string) => {
    if (factId == null) return null;
    const placed = layout.byId.get(factId);
    if (!placed) return null;
    const { k, tx, ty } = transform;
    return (
      <circle
        cx={placed.x * k + tx}
        cy={placed.y * k + ty}
        r={placed.r + 5}
        className={className}
      />
    );
  };

  const selectionList = useMemo(() => {
    if (!selection) return layout.placed;
    return layout.placed.filter((p) => selection.has(p.point.fact_id));
  }, [layout, selection]);

  const degenerate =
    !!data && (data.method === "none" || !data.exists || data.points.length < 2);

  const resetView = () => applyTransform(IDENTITY, true);

  /* ----------------------------------------------------------------- render */

  return (
    <Card>
      <CardHeader>
        <CardTitle>Semantic Map</CardTitle>
      </CardHeader>
      <CardContent>
        <p className="mb-3 text-xs text-text-tertiary">
          Each dot is a fact; proximity ≈ semantic similarity from its HRR
          vector. Scroll to zoom, drag to pan, shift-drag (or box mode) to
          select a region, click a dot to pin it.
        </p>

        {loading ? (
          <PanelLoading label="Projecting facts…" />
        ) : error ? (
          <PanelError error={error} onRetry={() => setReloadKey((k) => k + 1)} />
        ) : degenerate ? (
          <div className="border border-border bg-background/30 px-3 py-4 text-xs leading-relaxed text-text-secondary">
            The semantic map needs at least two facts with holographic (HRR)
            embeddings to project them into 2D space. Once enough facts have HRR
            vectors, they will be plotted here by proximity.
            {data?.error && (
              <span className="mt-1 block font-mono-ui text-text-tertiary">
                {data.error}
              </span>
            )}
          </div>
        ) : data ? (
          <div className="grid min-w-0 items-start gap-4 xl:grid-cols-[minmax(0,1fr)_20rem]">
            <div className="min-w-0">
              {(hasBanks || hasConnections) && (
                <div
                  className="mb-2 flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 font-mono-ui text-[11px] text-text-tertiary"
                  role="group"
                  aria-label="Map encodings"
                >
                  {hasBanks && (
                    <span className="flex items-center gap-1">
                      color
                      {(["category", "bank"] as const).map((mode) => (
                        <button
                          key={mode}
                          type="button"
                          className={`hv-chip${effectiveColorBy === mode ? " hv-chip-active" : ""}`}
                          aria-pressed={effectiveColorBy === mode}
                          onClick={() => {
                            setColorBy(mode);
                            setHiddenCats(new Set());
                          }}
                        >
                          {mode}
                        </button>
                      ))}
                    </span>
                  )}
                  {hasConnections && (
                    <span className="flex items-center gap-1">
                      size
                      {(["retrievals", "connections"] as const).map((mode) => (
                        <button
                          key={mode}
                          type="button"
                          className={`hv-chip${effectiveSizeBy === mode ? " hv-chip-active" : ""}`}
                          aria-pressed={effectiveSizeBy === mode}
                          onClick={() => setSizeBy(mode)}
                        >
                          {mode}
                        </button>
                      ))}
                    </span>
                  )}
                </div>
              )}
              <div className="mb-2 flex min-w-0 flex-wrap items-center gap-2">
                <VizLegend
                  items={legendItems}
                  hidden={hiddenCats}
                  onToggle={(key) =>
                    setHiddenCats((prev) => {
                      const next = new Set(prev);
                      if (next.has(key)) next.delete(key);
                      else next.add(key);
                      return next;
                    })
                  }
                />
                <div className="ml-auto flex shrink-0 items-center gap-1">
                  <Button
                    ghost={mode !== "pan"}
                    size="xs"
                    onClick={() => setMode("pan")}
                    aria-label="Pan mode"
                    aria-pressed={mode === "pan"}
                    title="Pan / click to pin"
                  >
                    <Hand />
                  </Button>
                  <Button
                    ghost={mode !== "select"}
                    size="xs"
                    onClick={() => setMode("select")}
                    aria-label="Box select mode"
                    aria-pressed={mode === "select"}
                    title="Box select (or shift-drag)"
                  >
                    <BoxSelect />
                  </Button>
                  <Button
                    ghost={!showDensity}
                    size="xs"
                    onClick={() => setDensityOverride(!showDensity)}
                    aria-label="Toggle density underlay"
                    aria-pressed={showDensity}
                    title="Density underlay"
                  >
                    <Flame />
                  </Button>
                  <Button
                    ghost
                    size="xs"
                    onClick={() => {
                      const sx = width / 2;
                      const sy = height / 2;
                      zoomAt(sx, sy, 1.4);
                    }}
                    aria-label="Zoom in"
                  >
                    <ZoomIn />
                  </Button>
                  <Button
                    ghost
                    size="xs"
                    onClick={() => zoomAt(width / 2, height / 2, 1 / 1.4)}
                    aria-label="Zoom out"
                  >
                    <ZoomOut />
                  </Button>
                  <Button ghost size="xs" onClick={resetView} aria-label="Reset view">
                    <RotateCcw />
                  </Button>
                </div>
              </div>

              <div
                ref={measureRef}
                className="hv-chart min-w-0 border border-border bg-background/30"
              >
                <svg
                  ref={bindSvg}
                  width={width}
                  height={height}
                  className={`hv-map${drag?.kind === "pan" ? " hv-grabbing" : ""}${
                    mode === "select" ? " hv-crosshair" : ""
                  }`}
                  role="application"
                  tabIndex={0}
                  aria-label="Semantic map of facts. Arrow keys pan, plus and minus zoom, zero resets, Escape clears. The adjacent list exposes every plotted fact."
                  aria-keyshortcuts="ArrowUp ArrowDown ArrowLeft ArrowRight + - 0 Escape"
                  onKeyDown={onKeyDown}
                  onPointerDown={onPointerDown}
                  onPointerMove={onPointerMove}
                  onPointerUp={onPointerUp}
                  onPointerLeave={() => {
                    setTip(null);
                    setHovered(null);
                  }}
                  onDoubleClick={resetView}
                >
                  <g
                    ref={zoomGroupRef}
                    style={
                      {
                        transform: `translate(${transform.tx}px, ${transform.ty}px) scale(${transform.k})`,
                        "--hv-k": transform.k,
                      } as React.CSSProperties
                    }
                  >
                    {densityLayer}
                    {pointsLayer}
                  </g>
                  {markerFor(hovered?.point.fact_id, "hv-marker-hover")}
                  {markerFor(selected?.fact_id, "hv-marker-pin")}
                  {drag?.kind === "select" && (
                    <rect
                      x={Math.min(drag.startX, drag.x)}
                      y={Math.min(drag.startY, drag.y)}
                      width={Math.abs(drag.x - drag.startX)}
                      height={Math.abs(drag.y - drag.startY)}
                      className="hv-select-rect"
                    />
                  )}
                  <text x={10} y={height - 10} className="hv-axis-text">
                    dim 1 →
                  </text>
                  <text
                    x={10}
                    y={16}
                    className="hv-axis-text"
                  >
                    dim 2 ↑
                  </text>
                </svg>
                <VizTooltip tip={tip} />
                {!hasVisiblePoint && layout.placed.length > 0 && (
                  <div className="hv-map-recover" role="status">
                    <div className="hv-map-recover-panel">
                      <p className="m-0">No facts in this view.</p>
                      <Button
                        size="xs"
                        onClick={() => fitToPlaced(layout.placed)}
                      >
                        Re-center on data
                      </Button>
                    </div>
                  </div>
                )}
              </div>

              <div className="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 font-mono-ui text-[0.65rem] text-text-tertiary">
                <span>zoom {(transform.k * 100).toFixed(0)}%</span>
                <span>
                  {visiblePoints.length}/{points.length} facts · {data.method} · dim {data.dim}
                </span>
                {selection && (
                  <button
                    type="button"
                    className="hv-chip"
                    onClick={() => setSelection(null)}
                  >
                    {selection.size} selected · clear <X className="h-2.5 w-2.5" />
                  </button>
                )}
              </div>
            </div>

            <SidePanel
              placed={selectionList}
              selection={selection}
              selected={selected}
              onSelect={setSelected}
              onShowAllCategories={
                hiddenCats.size > 0 && points.length > 0
                  ? () => setHiddenCats(new Set())
                  : undefined
              }
            />
          </div>
        ) : null}
      </CardContent>
    </Card>
  );
}

/* ------------------------------------------------------------- side panel */

const ROW_HEIGHT = 46;

function SidePanel({
  placed,
  selection,
  selected,
  onSelect,
  onShowAllCategories,
}: {
  placed: PlacedPoint[];
  selection: ReadonlySet<number> | null;
  selected: MemoryProjectionPoint | null;
  onSelect: (point: MemoryProjectionPoint | null) => void;
  /** Present when legend filters hide every point; restores all categories. */
  onShowAllCategories?: () => void;
}) {
  const { containerRef, onScroll, start, end, totalHeight, offsetTop } =
    useVirtualList({ count: placed.length, rowHeight: ROW_HEIGHT });

  // Projection payloads truncate content at 200 chars; when the pinned fact
  // looks truncated, fetch the full row from the fact-detail endpoint.
  const selectedId = selected?.fact_id;
  const maybeTruncated = (selected?.content?.length ?? 0) >= 200;
  const [detail, setDetail] = useState<{ id: number; content: string } | null>(
    null,
  );
  const [detailFailedId, setDetailFailedId] = useState<number | null>(null);
  useEffect(() => {
    if (selectedId == null || !maybeTruncated) return;
    let cancelled = false;
    setDetailFailedId((prev) => (prev === selectedId ? null : prev));
    api
      .getMemoryFact(selectedId)
      .then((resp) => {
        if (!cancelled) setDetail({ id: selectedId, content: resp.fact.content });
      })
      .catch(() => {
        if (!cancelled) setDetailFailedId(selectedId);
      });
    return () => {
      cancelled = true;
    };
  }, [selectedId, maybeTruncated]);

  const fullContent =
    detail && detail.id === selectedId ? detail.content : null;
  const detailFailed = detailFailedId === selectedId;
  const detailLoading = maybeTruncated && !fullContent && !detailFailed;

  // Single tab stop + arrow-key roving selection (the rows themselves stay
  // buttons for pointer users, but are skipped in the tab order).
  const activeIndex = selected
    ? placed.findIndex((p) => p.point.fact_id === selected.fact_id)
    : -1;

  const moveTo = (index: number) => {
    if (placed.length === 0) return;
    const next = Math.max(0, Math.min(placed.length - 1, index));
    onSelect(placed[next].point);
    const node = containerRef.current;
    if (node) {
      const rowTop = next * ROW_HEIGHT;
      if (rowTop < node.scrollTop) node.scrollTop = rowTop;
      else if (rowTop + ROW_HEIGHT > node.scrollTop + node.clientHeight) {
        node.scrollTop = rowTop + ROW_HEIGHT - node.clientHeight;
      }
    }
  };

  const onListKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    switch (event.key) {
      case "ArrowDown":
        moveTo(activeIndex < 0 ? 0 : activeIndex + 1);
        break;
      case "ArrowUp":
        moveTo(activeIndex < 0 ? 0 : activeIndex - 1);
        break;
      case "Home":
        moveTo(0);
        break;
      case "End":
        moveTo(placed.length - 1);
        break;
      case "Escape":
        onSelect(null);
        break;
      default:
        return;
    }
    event.preventDefault();
  };

  return (
    <div className="flex min-w-0 flex-col gap-2 xl:sticky xl:top-4 xl:self-start">
      <div className="flex flex-wrap items-center gap-2 text-xs text-text-tertiary">
        <Badge tone="outline" className={NUM_BADGE}>
          {selection ? `${placed.length} selected` : `${placed.length} facts`}
        </Badge>
      </div>
      <div
        ref={containerRef}
        onScroll={onScroll}
        onKeyDown={onListKeyDown}
        className="hv-fact-list border border-border bg-background/30"
        role="listbox"
        tabIndex={0}
        aria-label={`${selection ? "Selected" : "Plotted"} facts. Use arrow keys to browse, Escape to unpin.`}
        aria-activedescendant={
          selected ? `hv-fact-opt-${selected.fact_id}` : undefined
        }
      >
        {placed.length === 0 ? (
          <div className="p-3 text-xs text-text-tertiary">
            <p>No facts in view.</p>
            {onShowAllCategories && (
              <button
                type="button"
                className="hv-chip mt-2"
                onClick={onShowAllCategories}
              >
                show all categories
              </button>
            )}
          </div>
        ) : (
          <div style={{ height: totalHeight, position: "relative" }}>
            <div style={{ transform: `translateY(${offsetTop}px)` }}>
              {placed.slice(start, end).map((p) => {
                const active = selected?.fact_id === p.point.fact_id;
                return (
                  <button
                    key={p.point.fact_id}
                    id={`hv-fact-opt-${p.point.fact_id}`}
                    type="button"
                    role="option"
                    tabIndex={-1}
                    aria-selected={active}
                    style={{ height: ROW_HEIGHT }}
                    onClick={() => onSelect(active ? null : p.point)}
                    className={`hv-fact-row${active ? " hv-fact-row-active" : ""}`}
                  >
                    <span className="hv-fact-row-meta">
                      <span className="hv-swatch" style={{ background: p.color }} />
                      <span>#{p.point.fact_id}</span>
                      <span className="min-w-0 truncate">{p.point.category}</span>
                    </span>
                    <span className="hv-fact-row-content">{p.point.content}</span>
                  </button>
                );
              })}
            </div>
          </div>
        )}
      </div>
      {selected ? (
        <div className="border border-border bg-background/50 p-3">
          <div className="mb-2 flex flex-wrap items-center gap-2">
            <Badge tone="secondary" className={NUM_BADGE}>
              {selected.category}
            </Badge>
            <span className="font-mono-ui text-xs text-text-tertiary">
              #{selected.fact_id}
            </span>
            <Badge tone="outline" className={NUM_BADGE}>
              trust {Number(selected.trust_score ?? 0).toFixed(2)}
            </Badge>
            <Badge tone="outline" className={NUM_BADGE}>
              used {selected.retrieval_count}
            </Badge>
            <Button
              ghost
              size="xs"
              className="ml-auto"
              onClick={() => onSelect(null)}
              aria-label="Unpin fact"
            >
              <X />
            </Button>
          </div>
          <p className="max-h-72 overflow-y-auto whitespace-pre-wrap text-sm leading-relaxed text-foreground">
            {fullContent ?? selected.content}
            {/* Make the projection payload's 200-char cut visible until the full row arrives. */}
            {!fullContent && maybeTruncated && "…"}
          </p>
          {detailLoading && (
            <p className="mt-1 text-xs text-text-tertiary" role="status">
              Loading full fact…
            </p>
          )}
          {detailFailed && (
            <p className="mt-1 text-xs text-text-tertiary">
              Couldn't load the full fact — showing the first 200 characters.
            </p>
          )}
        </div>
      ) : (
        <div className="border border-border bg-background/30 p-4 text-xs text-text-tertiary">
          <p className="text-text-secondary">No fact pinned</p>
          <p className="mt-1 leading-relaxed">
            Click a dot (or a row) to pin it here. Dot size and color follow
            the active encodings above the map.
          </p>
        </div>
      )}
    </div>
  );
}
