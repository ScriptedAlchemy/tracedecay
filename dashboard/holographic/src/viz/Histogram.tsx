import type * as React from "react";
import { useCallback, useMemo, useRef, useState, type ReactNode } from "react";
import { AxisBottom } from "./Axis";
import { scaleLinear, type Bin } from "./scale";
import { useMeasuredWidth } from "./useMeasure";
import { VizTooltip, TipRow, TipTitle, type TooltipState } from "./Tooltip";

const MARGIN = { top: 6, right: 4, bottom: 22, left: 4 };

/**
 * Shared histogram with optional drag-to-brush range selection.
 *
 * Bars inside the brushed range render in the accent color; bars outside dim.
 * A plain click (no drag) clears the brush. Pure SVG, theme-token colors.
 */
export function BrushableHistogram({
  bins,
  height = 96,
  color = "var(--hm-primary)",
  brush,
  onBrush,
  format,
  tipLabel = "count",
  title,
}: {
  bins: Bin[];
  height?: number;
  color?: string;
  brush?: [number, number] | null;
  onBrush?: (range: [number, number] | null) => void;
  format?: (value: number) => string;
  tipLabel?: string;
  title?: (bin: Bin) => ReactNode;
}) {
  const [ref, width] = useMeasuredWidth<HTMLDivElement>(420);
  const [tip, setTip] = useState<TooltipState | null>(null);
  const dragRef = useRef<{ startX: number; moved: boolean } | null>(null);
  const svgRef = useRef<SVGSVGElement>(null);

  const innerW = Math.max(40, width - MARGIN.left - MARGIN.right);
  const innerH = height - MARGIN.top - MARGIN.bottom;

  const domain = useMemo<[number, number]>(() => {
    if (bins.length === 0) return [0, 1];
    return [bins[0].x0, bins[bins.length - 1].x1];
  }, [bins]);

  const x = useMemo(
    () => scaleLinear(domain, [MARGIN.left, MARGIN.left + innerW]),
    [domain, innerW],
  );
  const maxCount = useMemo(
    () => Math.max(1, ...bins.map((b) => b.count)),
    [bins],
  );

  const fmt = format ?? ((v: number) => v.toPrecision(3));

  const localX = useCallback((event: React.PointerEvent) => {
    const rect = svgRef.current?.getBoundingClientRect();
    return rect ? event.clientX - rect.left : 0;
  }, []);

  const clampDomain = useCallback(
    (px: number) => Math.min(domain[1], Math.max(domain[0], x.invert(px))),
    [domain, x],
  );

  const onPointerDown = (event: React.PointerEvent) => {
    if (!onBrush) return;
    try {
      event.currentTarget.setPointerCapture(event.pointerId);
    } catch {
      /* synthetic/stale pointers can't be captured; the brush still works */
    }
    dragRef.current = { startX: localX(event), moved: false };
  };

  const onPointerMove = (event: React.PointerEvent) => {
    const px = localX(event);
    const drag = dragRef.current;
    if (drag && onBrush) {
      if (Math.abs(px - drag.startX) > 3) drag.moved = true;
      if (drag.moved) {
        const a = clampDomain(drag.startX);
        const b = clampDomain(px);
        onBrush(a <= b ? [a, b] : [b, a]);
      }
      return;
    }
    // Hover tooltip (only while not brushing).
    const value = x.invert(px);
    const bin = bins.find((b) => value >= b.x0 && value <= b.x1);
    if (!bin) {
      setTip(null);
      return;
    }
    const rect = svgRef.current?.getBoundingClientRect();
    setTip({
      x: px,
      y: rect ? event.clientY - rect.top : 0,
      content: (
        <>
          <TipTitle color={color}>
            {title ? title(bin) : `${fmt(bin.x0)} – ${fmt(bin.x1)}`}
          </TipTitle>
          <TipRow label={tipLabel} value={bin.count} />
        </>
      ),
    });
  };

  const onPointerUp = (event: React.PointerEvent) => {
    const drag = dragRef.current;
    dragRef.current = null;
    if (drag && !drag.moved && onBrush) onBrush(null);
    try {
      event.currentTarget.releasePointerCapture(event.pointerId);
    } catch {
      /* noop */
    }
  };

  const barW = bins.length > 0 ? innerW / bins.length : innerW;

  // Keyboard brush: arrows nudge the window, shift+arrows resize, Escape clears.
  const onKeyDown = (event: React.KeyboardEvent) => {
    if (!onBrush || bins.length === 0) return;
    const span = domain[1] - domain[0];
    const step = span / Math.max(8, bins.length);
    const current: [number, number] = brush ?? [
      domain[0] + span * 0.35,
      domain[1] - span * 0.35,
    ];
    const clamp = (v: number) => Math.min(domain[1], Math.max(domain[0], v));
    let next: [number, number] | null = null;
    if (event.key === "Escape") {
      next = null;
    } else if (event.key === "ArrowLeft") {
      next = event.shiftKey
        ? [current[0], clamp(current[1] - step)]
        : [clamp(current[0] - step), clamp(current[1] - step)];
    } else if (event.key === "ArrowRight") {
      next = event.shiftKey
        ? [current[0], clamp(current[1] + step)]
        : [clamp(current[0] + step), clamp(current[1] + step)];
    } else {
      return;
    }
    if (next && next[1] <= next[0]) next = [next[0], clamp(next[0] + step)];
    onBrush(next);
    event.preventDefault();
  };

  return (
    <div ref={ref} className="hv-chart" style={{ height }}>
      <svg
        ref={svgRef}
        width={width}
        height={height}
        className={onBrush ? "hv-histogram hv-brushable" : "hv-histogram"}
        role={onBrush ? "application" : "img"}
        tabIndex={onBrush ? 0 : undefined}
        aria-label={
          onBrush
            ? "Score distribution histogram. Drag or use arrow keys to filter by range; shift with arrows resizes; Escape clears."
            : "Score distribution histogram"
        }
        onKeyDown={onBrush ? onKeyDown : undefined}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerLeave={() => setTip(null)}
      >
        {bins.map((bin, i) => {
          const h = (bin.count / maxCount) * innerH;
          const inBrush =
            !brush || (bin.x1 >= brush[0] && bin.x0 <= brush[1]);
          return (
            <rect
              key={i}
              x={x(bin.x0) + 0.5}
              y={MARGIN.top + innerH - h}
              width={Math.max(1, barW - 1)}
              height={Math.max(bin.count > 0 ? 1.5 : 0, h)}
              style={{ fill: color, opacity: inBrush ? 0.88 : 0.18 }}
            />
          );
        })}
        {brush && (
          <g>
            <rect
              x={x(brush[0])}
              y={MARGIN.top}
              width={Math.max(1, x(brush[1]) - x(brush[0]))}
              height={innerH}
              className="hv-brush-region"
            />
            <line x1={x(brush[0])} x2={x(brush[0])} y1={MARGIN.top} y2={MARGIN.top + innerH} className="hv-brush-handle" />
            <line x1={x(brush[1])} x2={x(brush[1])} y1={MARGIN.top} y2={MARGIN.top + innerH} className="hv-brush-handle" />
          </g>
        )}
        <AxisBottom scale={x} y={MARGIN.top + innerH} tickCount={5} format={fmt} />
      </svg>
      <VizTooltip tip={tip} />
    </div>
  );
}
