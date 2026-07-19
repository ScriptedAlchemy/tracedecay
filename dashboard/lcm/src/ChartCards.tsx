/* eslint-disable @typescript-eslint/no-explicit-any */

/**
 * Chart cards of the hermes-lcm dashboard: the per-day message timeline and
 * the per-session compression bars, each with error / skeleton fallbacks.
 * Extracted 1:1 from `App.tsx` — DOM structure and class names unchanged.
 */

import React from "react";
import { CompressionBars, TimelineChart } from "./components";
import { ErrorPanel, SkeletonLines } from "../../lib/primitives";

interface ChartCardsProps {
  chartsError: string;
  timeline: any;
  compression: any;
  chartsLoading: boolean;
  onRetry: () => void;
  onOpenSession: (id: any, opts?: any) => void;
}

export function ChartCards({
  chartsError,
  timeline,
  compression,
  chartsLoading,
  onRetry,
  onOpenSession,
}: ChartCardsProps): React.ReactElement {
  return (
    <div className="hermes-lcm-grid">
      <div className="hermes-lcm-card hermes-lcm-wide">
        <h3>Message Timeline (per day · dots = summaries)</h3>
        {chartsError && !timeline ? (
          <ErrorPanel
            error={chartsError}
            onRetry={onRetry}
            className="hermes-lcm-error"
          />
        ) : chartsLoading && !timeline ? (
          <SkeletonLines
            count={5}
            widths={["100%", "95%", "90%", "92%", "88%"]}
          />
        ) : (
          <TimelineChart
            buckets={(timeline && timeline.buckets) || []}
            nodeBuckets={(timeline && timeline.node_buckets) || []}
            undatedCount={
              (timeline && timeline.undated && timeline.undated.count) || 0
            }
          />
        )}
      </div>
      <div className="hermes-lcm-card hermes-lcm-wide">
        <h3>Compression by Session (kept vs saved)</h3>
        {chartsError && !compression ? (
          <ErrorPanel
            error={chartsError}
            onRetry={onRetry}
            className="hermes-lcm-error"
          />
        ) : chartsLoading && !compression ? (
          <SkeletonLines count={4} widths={["98%", "90%", "84%", "88%"]} />
        ) : (
          <CompressionBars
            groups={(compression && compression.groups) || []}
            onPick={function (g) {
              onOpenSession(g.session_id != null ? g.session_id : g.key);
            }}
          />
        )}
      </div>
    </div>
  );
}
