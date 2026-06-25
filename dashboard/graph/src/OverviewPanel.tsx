/**
 * Landing analytics for the Code Graph tab: orientation visuals (counts by
 * kind, language mix, hub symbols, largest files) rendered with the shared
 * dashboard primitives (BarList) in the shared design-token vocabulary.
 */

import React from "react";
import { Badge, Card, CardContent, CardHeader, CardTitle } from "../../lib/sdk";
import { BarList, EmptyState } from "../../lib/primitives";
import { fmt } from "../../lib/format";
import {
  colorForKind,
  KIND_FAMILY_COLORS,
  KIND_FAMILY_LABELS,
  kindFamily,
} from "./types";
import type { GraphNode, GraphOverview } from "./types";

// Language → design token (with the historical dark hex as fallback), mirroring
// KIND_FAMILY_COLORS in ./types so the bar swatches re-theme with the shell's
// light palette instead of pinning dark-only hex. Each language rides a
// matching --ts-* accent token (javascript shares --ts-amber by hue family).
const LANGUAGE_COLORS: Record<string, string> = {
  rust: "var(--ts-amber, #f7c76a)",
  typescript: "var(--ts-blue, #7aa7ff)",
  // Reuse the warm amber token (same hue family as the historical JS yellow)
  // with the original dark hex as fallback, so the swatch re-themes in light
  // mode instead of pinning a dark-only literal. See ./types KIND_FAMILY_COLORS.
  javascript: "var(--ts-amber, #ffd97a)",
  python: "var(--ts-green, #67e8a9)",
  markdown: "var(--ts-text-2, #a8c8c0)",
  json: "var(--ts-text-3, #6f9189)",
  toml: "var(--ts-text-3, #6f9189)",
  shell: "var(--ts-cyan, #75f4d2)",
  web: "var(--ts-pink, #ff7ab6)",
};

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
    return <EmptyState>Loading graph analytics…</EmptyState>;
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
        <Card>
          <CardHeader><CardTitle>Symbols by family</CardTitle></CardHeader>
          <CardContent>
            <BarList
              keyName="label"
              rows={familyRows.map((row) => ({
                label: row.label,
                color: KIND_FAMILY_COLORS[row.family] || KIND_FAMILY_COLORS.other,
                value: fmt(row.count),
                family: row.family,
              }))}
              onPick={(row) => onFilterKind(String(row.family))}
            />
            <p className="tsg-chart-hint">Click a family to open the canvas filtered to it.</p>
          </CardContent>
        </Card>

        <Card>
          <CardHeader><CardTitle>Files by language</CardTitle></CardHeader>
          <CardContent>
            <BarList
              keyName="label"
              rows={overview.files_by_language.slice(0, 9).map((row) => ({
                label: row.language,
                color: LANGUAGE_COLORS[row.language] || "var(--ts-text-3, #6f9189)",
                value: fmt(row.count),
              }))}
              onPick={(row) => onFilterLanguage(String(row.label))}
            />
          </CardContent>
        </Card>

        <Card>
          <CardHeader><CardTitle>Most connected symbols</CardTitle></CardHeader>
          <CardContent>
            <BarList
              keyName="label"
              rows={overview.top_connected.map((row) => ({
                label: row.name,
                color: colorForKind(row.kind),
                meta: row.kind,
                value: `${fmt(row.degree)} edges`,
                node: row,
              }))}
              rowKey={(row) => String(row.node.id)}
              titleFor={(row) => `Open ${String(row.label)} in the canvas`}
              onPick={(row) => onFocusSymbol(row.node)}
            />
          </CardContent>
        </Card>

        <Card>
          <CardHeader><CardTitle>Largest files</CardTitle></CardHeader>
          <CardContent>
            <BarList
              keyName="label"
              rows={overview.largest_files.map((row) => {
                const short = row.path.split("/").slice(-2).join("/");
                return {
                  label: short,
                  path: row.path,
                  color: "color-mix(in srgb, var(--ts-cyan, #75f4d2) 60%, transparent)",
                  value: `${fmt(row.node_count)} symbols`,
                };
              })}
              rowKey={(row) => String(row.path)}
            />
          </CardContent>
        </Card>
      </div>

      <div className="tsg-edge-kind-strip">
        {overview.edges_by_kind.map((row) => (
          <Badge key={row.kind}>{row.kind}: {fmt(row.count)}</Badge>
        ))}
      </div>
    </div>
  );
}
