/**
 * Landing analytics for the Code Graph tab: orientation visuals (counts by
 * kind, language mix, hub symbols, largest files) rendered as compact
 * hand-rolled SVG charts in the shared design-token vocabulary.
 */

import React from "react";
import { Badge, Card, CardContent, CardHeader, CardTitle } from "./sdk";
import {
  colorForKind,
  KIND_FAMILY_COLORS,
  KIND_FAMILY_LABELS,
  kindFamily,
} from "./types";
import type { GraphNode, GraphOverview } from "./types";

const ShellCard = Card || "div";
const ShellCardHeader = CardHeader || "div";
const ShellCardTitle = CardTitle || "h3";
const ShellCardContent = CardContent || "div";
const ShellBadge = Badge || "span";

function fmt(n: number | undefined) {
  return Number(n || 0).toLocaleString();
}

const LANGUAGE_COLORS: Record<string, string> = {
  rust: "#f7c76a",
  typescript: "#7aa7ff",
  javascript: "#ffd97a",
  python: "#67e8a9",
  markdown: "#a8c8c0",
  json: "#6f9189",
  toml: "#6f9189",
  shell: "#75f4d2",
  web: "#ff7ab6",
};

function HBarChart({
  rows,
  colorFor,
  onPick,
}: {
  rows: Array<{ label: string; count: number; meta?: string }>;
  colorFor: (label: string) => string;
  onPick?: (label: string) => void;
}) {
  const max = Math.max(1, ...rows.map((row) => row.count));
  const rowH = 26;
  const height = rows.length * rowH;
  return (
    <svg
      className="tsg-chart"
      viewBox={`0 0 420 ${height}`}
      preserveAspectRatio="none"
      role="img"
    >
      {rows.map((row, index) => {
        const w = Math.max(3, (row.count / max) * 250);
        const y = index * rowH;
        return (
          <g
            key={row.label}
            className={onPick ? "tsg-chart-row tsg-chart-row-clickable" : "tsg-chart-row"}
            onClick={onPick ? () => onPick(row.label) : undefined}
          >
            <rect x="0" y={y} width="420" height={rowH} fill="transparent" />
            <text x="0" y={y + rowH / 2} className="tsg-chart-label" dominantBaseline="middle">
              {row.label.length > 18 ? `${row.label.slice(0, 17)}…` : row.label}
            </text>
            <rect
              x="130"
              y={y + 6}
              width={w}
              height={rowH - 12}
              rx="4"
              // style, not the fill attribute: presentation attributes can't
              // resolve the var()-based token colors.
              style={{ fill: colorFor(row.label) }}
              opacity="0.85"
            />
            <text
              x={134 + w}
              y={y + rowH / 2}
              className="tsg-chart-value"
              dominantBaseline="middle"
            >
              {fmt(row.count)}{row.meta ? `  ${row.meta}` : ""}
            </text>
          </g>
        );
      })}
    </svg>
  );
}

export default function OverviewPanel({
  overview,
  onFocusSymbol,
  onFilterKind,
  onFilterLanguage,
}: {
  overview: GraphOverview | null;
  onFocusSymbol: (node: Pick<GraphNode, "id" | "name">) => void;
  onFilterKind: (family: string) => void;
  onFilterLanguage: (language: string) => void;
}) {
  if (!overview) {
    return <div className="tsg-empty">Loading graph analytics…</div>;
  }

  // Aggregate raw kinds into visual families for the chart.
  const familyCounts = new Map<string, number>();
  for (const row of overview.nodes_by_kind) {
    const family = kindFamily(row.kind);
    familyCounts.set(family, (familyCounts.get(family) || 0) + row.count);
  }
  const familyRows = [...familyCounts.entries()]
    .sort((a, b) => b[1] - a[1])
    .map(([family, count]) => ({
      label: KIND_FAMILY_LABELS[family] || family,
      family,
      count,
    }));

  return (
    <div className="tsg-analytics">
      <div className="tsg-analytics-grid">
        <ShellCard>
          <ShellCardHeader><ShellCardTitle>Symbols by family</ShellCardTitle></ShellCardHeader>
          <ShellCardContent>
            <HBarChart
              rows={familyRows.map((row) => ({ label: row.label, count: row.count }))}
              colorFor={(label) => {
                const row = familyRows.find((r) => r.label === label);
                return KIND_FAMILY_COLORS[row?.family || "other"];
              }}
              onPick={(label) => {
                const row = familyRows.find((r) => r.label === label);
                if (row) onFilterKind(row.family);
              }}
            />
            <p className="tsg-chart-hint">Click a family to open the canvas filtered to it.</p>
          </ShellCardContent>
        </ShellCard>

        <ShellCard>
          <ShellCardHeader><ShellCardTitle>Files by language</ShellCardTitle></ShellCardHeader>
          <ShellCardContent>
            <HBarChart
              rows={overview.files_by_language.slice(0, 9).map((row) => ({
                label: row.language,
                count: row.count,
              }))}
              colorFor={(label) => LANGUAGE_COLORS[label] || "#6f9189"}
              onPick={onFilterLanguage}
            />
          </ShellCardContent>
        </ShellCard>

        <ShellCard>
          <ShellCardHeader><ShellCardTitle>Most connected symbols</ShellCardTitle></ShellCardHeader>
          <ShellCardContent>
            <div className="tsg-hub-list">
              {overview.top_connected.map((row) => (
                <button
                  key={row.id}
                  className="tsg-hub"
                  onClick={() => onFocusSymbol(row)}
                  title={`Open ${row.name} in the canvas`}
                >
                  <span className="tsg-hub-dot" style={{ background: colorForKind(row.kind) }} />
                  <span className="tsg-hub-name">{row.name}</span>
                  <span className="tsg-hub-meta">{row.kind}</span>
                  <span className="tsg-hub-degree">{fmt(row.degree)} edges</span>
                </button>
              ))}
            </div>
          </ShellCardContent>
        </ShellCard>

        <ShellCard>
          <ShellCardHeader><ShellCardTitle>Largest files</ShellCardTitle></ShellCardHeader>
          <ShellCardContent>
            <HBarChart
              rows={overview.largest_files.map((row) => {
                const short = row.path.split("/").slice(-2).join("/");
                return { label: short, count: row.node_count, meta: "symbols" };
              })}
              colorFor={() => "color-mix(in srgb, var(--ts-cyan, #75f4d2) 60%, transparent)"}
            />
          </ShellCardContent>
        </ShellCard>
      </div>

      <div className="tsg-edge-kind-strip">
        {overview.edges_by_kind.map((row) => (
          <ShellBadge key={row.kind}>{row.kind}: {fmt(row.count)}</ShellBadge>
        ))}
      </div>
    </div>
  );
}
