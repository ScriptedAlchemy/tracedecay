import { formatTick, type LinearScale } from "./scale";

/**
 * Shared SVG axes so every chart ticks/labels the same way.
 * Render inside an `<svg>`; coordinates are in the chart's pixel space.
 */

export function AxisBottom({
  scale,
  y,
  tickCount = 5,
  format,
}: {
  scale: LinearScale;
  y: number;
  tickCount?: number;
  format?: (value: number) => string;
}) {
  const [d0, d1] = scale.domain;
  const span = Math.abs(d1 - d0);
  const fmt = format ?? ((v: number) => formatTick(v, span));
  return (
    <g className="hv-axis">
      <line x1={scale.range[0]} x2={scale.range[1]} y1={y} y2={y} className="hv-axis-line" />
      {scale.ticks(tickCount).map((t) => (
        <g key={t} transform={`translate(${scale(t)},${y})`}>
          <line y2={4} className="hv-axis-line" />
          <text y={14} textAnchor="middle" className="hv-axis-text">
            {fmt(t)}
          </text>
        </g>
      ))}
    </g>
  );
}

export function AxisLeft({
  scale,
  x,
  tickCount = 4,
  format,
  grid,
  gridX2,
}: {
  scale: LinearScale;
  x: number;
  tickCount?: number;
  format?: (value: number) => string;
  /** Draw faint horizontal grid lines across to `gridX2`. */
  grid?: boolean;
  gridX2?: number;
}) {
  const [d0, d1] = scale.domain;
  const span = Math.abs(d1 - d0);
  const fmt = format ?? ((v: number) => formatTick(v, span));
  return (
    <g className="hv-axis">
      {scale.ticks(tickCount).map((t) => (
        <g key={t} transform={`translate(${x},${scale(t)})`}>
          {grid && <line x2={(gridX2 ?? x) - x} className="hv-grid-line" />}
          <text x={-6} dy="0.32em" textAnchor="end" className="hv-axis-text">
            {fmt(t)}
          </text>
        </g>
      ))}
    </g>
  );
}
