/**
 * Compact hand-rolled SVG charts in the shared design-token vocabulary
 * (same approach as the Code Graph tab — no charting dependency).
 */

import React from "react";
import { fmtDay, fmtTokens } from "./logic";

export function HBarChart({
  rows,
  color,
  emptyText,
}: {
  rows: Array<{ label: string; count: number; meta?: string }>;
  color?: string;
  emptyText?: string;
}) {
  if (!rows.length) {
    return <div className="tss-empty-mini">{emptyText || "No data yet."}</div>;
  }
  const max = Math.max(1, ...rows.map((row) => row.count));
  const rowH = 26;
  const height = rows.length * rowH;
  return (
    <svg
      className="tss-chart"
      viewBox={`0 0 420 ${height}`}
      preserveAspectRatio="none"
      role="img"
    >
      {rows.map((row, index) => {
        const w = Math.max(3, (row.count / max) * 230);
        const y = index * rowH;
        return (
          <g key={`${row.label}-${index}`}>
            <text
              x="0"
              y={y + rowH / 2}
              className="tss-chart-label"
              dominantBaseline="middle"
            >
              {row.label.length > 20 ? `${row.label.slice(0, 19)}…` : row.label}
            </text>
            <rect
              x="150"
              y={y + 6}
              width={w}
              height={rowH - 12}
              rx="4"
              style={{ fill: color || "var(--ts-cyan, #75f4d2)" }}
              opacity="0.85"
            />
            <text
              x={154 + w}
              y={y + rowH / 2}
              className="tss-chart-value"
              dominantBaseline="middle"
            >
              {fmtTokens(row.count)}
              {row.meta ? `  ${row.meta}` : ""}
            </text>
          </g>
        );
      })}
    </svg>
  );
}

export function DailyBars({
  series,
  color,
  emptyText,
  valueLabel,
}: {
  series: Array<{ day: number; value: number }>;
  color?: string;
  emptyText?: string;
  valueLabel?: (value: number) => string;
}) {
  if (!series.length) {
    return <div className="tss-empty-mini">{emptyText || "No dated entries yet."}</div>;
  }
  const width = 420;
  const height = 120;
  const max = Math.max(1, ...series.map((point) => point.value));
  const barW = Math.max(2, Math.min(26, (width - 8) / series.length - 2));
  const step = (width - 8) / series.length;
  const label = valueLabel || fmtTokens;
  return (
    <svg
      className="tss-chart tss-daily"
      viewBox={`0 0 ${width} ${height}`}
      preserveAspectRatio="none"
      role="img"
    >
      {series.map((point, index) => {
        const h = Math.max(2, (point.value / max) * (height - 28));
        const x = 4 + index * step;
        return (
          <g key={point.day}>
            <rect
              x={x}
              y={height - 16 - h}
              width={barW}
              height={h}
              rx="2"
              style={{ fill: color || "var(--ts-cyan, #75f4d2)" }}
              opacity={point.value > 0 ? 0.85 : 0.25}
            >
              <title>{`${fmtDay(point.day)}: ${label(point.value)}`}</title>
            </rect>
            {(index === 0 || index === series.length - 1) && (
              <text
                x={x + barW / 2}
                y={height - 4}
                className="tss-chart-label"
                textAnchor="middle"
              >
                {fmtDay(point.day)}
              </text>
            )}
          </g>
        );
      })}
    </svg>
  );
}
