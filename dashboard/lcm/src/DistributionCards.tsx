/* eslint-disable @typescript-eslint/no-explicit-any */

/**
 * Distribution cards of the hermes-lcm dashboard: By Source, By Role, and
 * Summary Depth bar lists. Extracted 1:1 from `App.tsx` — DOM structure and
 * class names unchanged.
 */

import React from "react";
import { fmtInt } from "./helpers";
import { BarList } from "../../lib/primitives";

interface DistributionCardsProps {
  sources: any[];
  roleCounts: any[];
  depthCounts: any[];
  onSourceChange: (value: string) => void;
  onRoleChange: (value: string) => void;
}

export function DistributionCards({
  sources,
  roleCounts,
  depthCounts,
  onSourceChange,
  onRoleChange,
}: DistributionCardsProps): React.ReactElement {
  return (
    <div className="hermes-lcm-grid">
      <div className="hermes-lcm-card">
        <h3>By Source</h3>
        <BarList
          rows={(sources || []).map(function (s) {
            return {
              source: s.source == null ? "(none)" : s.source,
              count: s.count,
              value: fmtInt(s.count),
            };
          })}
          keyName="source"
          proportional
          valueName="count"
          emptyText="No data"
          onPick={function (row) {
            const v = String(row.source);
            onSourceChange(v === "(none)" ? "unknown" : v);
          }}
        />
      </div>
      <div className="hermes-lcm-card">
        <h3>By Role</h3>
        <BarList
          rows={(roleCounts || []).map(function (r) {
            return {
              role: r.role == null ? "(none)" : r.role,
              count: r.count,
              value: fmtInt(r.count),
            };
          })}
          keyName="role"
          proportional
          valueName="count"
          emptyText="No data"
          onPick={function (row) {
            onRoleChange(String(row.role));
          }}
        />
      </div>
      <div className="hermes-lcm-card">
        <h3>Summary Depth</h3>
        <BarList
          rows={(depthCounts || []).map(function (r) {
            return {
              depth: r.depth == null ? "(none)" : r.depth,
              count: r.count,
              value: fmtInt(r.count),
            };
          })}
          keyName="depth"
          proportional
          valueName="count"
          emptyText="No data"
        />
      </div>
    </div>
  );
}
