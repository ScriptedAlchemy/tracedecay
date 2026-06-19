import type * as React from "react";
import { useMemo, useState } from "react";
import { scaleLinear } from "./scale";
import { useMeasuredWidth } from "./useMeasure";
import { VizTooltip, TipRow, TipTitle, type TooltipState } from "./Tooltip";

export interface SparkPoint {
  label: string;
  value: number;
}

const MARGIN = { top: 6, right: 6, bottom: 6, left: 6 };

/**
 * Area+line sparkline with a hover scrub (vertical guide + tooltip).
 * Used for the facts/day trend; generic over any labeled series.
 */
export function Sparkline({
  points,
  height = 72,
  color = "var(--hm-primary)",
  valueLabel = "facts",
}: {
  points: SparkPoint[];
  height?: number;
  color?: string;
  valueLabel?: string;
}) {
  const [ref, width] = useMeasuredWidth<HTMLDivElement>(420);
  const [hover, setHover] = useState<number | null>(null);
  const [tip, setTip] = useState<TooltipState | null>(null);

  const innerW = Math.max(20, width - MARGIN.left - MARGIN.right);
  const innerH = height - MARGIN.top - MARGIN.bottom;
  const n = points.length;

  const { xs, ys, linePath, areaPath } = useMemo(() => {
    const maxV = Math.max(1, ...points.map((p) => p.value));
    const x = scaleLinear([0, Math.max(1, n - 1)], [MARGIN.left, MARGIN.left + innerW]);
    const y = scaleLinear([0, maxV], [MARGIN.top + innerH, MARGIN.top + 2]);
    const xsArr = points.map((_, i) => (n === 1 ? MARGIN.left + innerW / 2 : x(i)));
    const ysArr = points.map((p) => y(p.value));
    // A single point draws nothing on its own; span it into a flat line.
    const px = n === 1 ? [MARGIN.left, MARGIN.left + innerW] : xsArr;
    const py = n === 1 ? [ysArr[0], ysArr[0]] : ysArr;
    const line = px
      .map((vx, i) => `${i === 0 ? "M" : "L"}${vx.toFixed(2)},${py[i].toFixed(2)}`)
      .join(" ");
    const base = MARGIN.top + innerH;
    const area = `M${px[0].toFixed(2)},${base} ${px
      .map((vx, i) => `L${vx.toFixed(2)},${py[i].toFixed(2)}`)
      .join(" ")} L${px[px.length - 1].toFixed(2)},${base} Z`;
    return { xs: xsArr, ys: ysArr, linePath: line, areaPath: area };
  }, [points, n, innerW, innerH]);

  const onMove = (event: React.PointerEvent<SVGSVGElement>) => {
    if (n === 0) return;
    const rect = event.currentTarget.getBoundingClientRect();
    const px = event.clientX - rect.left;
    let best = 0;
    let bestDist = Infinity;
    for (let i = 0; i < n; i++) {
      const d = Math.abs(xs[i] - px);
      if (d < bestDist) {
        bestDist = d;
        best = i;
      }
    }
    setHover(best);
    setTip({
      x: xs[best],
      y: ys[best],
      content: (
        <>
          <TipTitle color={color}>{points[best].label}</TipTitle>
          <TipRow label={valueLabel} value={points[best].value} />
        </>
      ),
    });
  };

  if (n === 0) return null;

  return (
    <div ref={ref} className="hv-chart" style={{ height }}>
      <svg
        width={width}
        height={height}
        role="img"
        aria-label={`${valueLabel} trend`}
        onPointerMove={onMove}
        onPointerLeave={() => {
          setHover(null);
          setTip(null);
        }}
      >
        <path d={areaPath} style={{ fill: color, opacity: 0.14 }} />
        <path
          d={linePath}
          fill="none"
          strokeWidth={1.5}
          strokeLinecap="round"
          strokeLinejoin="round"
          style={{ stroke: color }}
        />
        {hover != null && (
          <g>
            <line
              x1={xs[hover]}
              x2={xs[hover]}
              y1={MARGIN.top}
              y2={MARGIN.top + innerH}
              className="hv-guide-line"
            />
            <circle cx={xs[hover]} cy={ys[hover]} r={3.5} style={{ fill: color }} />
          </g>
        )}
      </svg>
      <VizTooltip tip={tip} />
    </div>
  );
}
