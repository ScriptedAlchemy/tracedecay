/* eslint-disable @typescript-eslint/no-explicit-any */

/**
 * Overview stat row of the hermes-lcm dashboard (messages / sessions /
 * summary nodes / compression / tokens kept) with its loading skeleton.
 * Extracted 1:1 from `App.tsx` — DOM structure and class names unchanged.
 */

import React from "react";
import { fmtInt } from "./helpers";
import { SkeletonLines, Stat } from "../../lib/primitives";

interface StatRowProps {
  data: any;
  overviewLoading: boolean;
}

export function StatRow({
  data,
  overviewLoading,
}: StatRowProps): React.ReactElement | null {
  const overview = (data && data.overview) || {};
  const comp = overview.compression || {};
  // Stats render only from a successful overview payload; zeros are then
  // genuinely "empty database", never a masked fetch failure.
  return data ? (
    <div className="hermes-lcm-statrow">
      <Stat
        className="hermes-lcm-stat"
        variant="compact"
        value={fmtInt(overview.messages_total)}
        label="messages"
      />
      <Stat
        className="hermes-lcm-stat"
        variant="compact"
        value={fmtInt(overview.sessions_total)}
        label="sessions"
      />
      <Stat
        className="hermes-lcm-stat"
        variant="compact"
        value={fmtInt(overview.summary_nodes_total)}
        label="summary nodes"
      />
      <Stat
        className="hermes-lcm-stat"
        variant="compact"
        value={comp.ratio ? comp.ratio + "×" : "—"}
        label="compression"
      />
      <Stat
        className="hermes-lcm-stat"
        variant="compact"
        value={`${fmtInt(comp.source_token_count)}→${fmtInt(comp.token_count)}`}
        label="tokens kept"
      />
    </div>
  ) : overviewLoading ? (
    <div className="hermes-lcm-statrow">
      <div className="hermes-lcm-stat hermes-lcm-skeleton">
        <SkeletonLines count={2} widths={["55%", "35%"]} />
      </div>
      <div className="hermes-lcm-stat hermes-lcm-skeleton">
        <SkeletonLines count={2} widths={["45%", "30%"]} />
      </div>
      <div className="hermes-lcm-stat hermes-lcm-skeleton">
        <SkeletonLines count={2} widths={["62%", "38%"]} />
      </div>
      <div className="hermes-lcm-stat hermes-lcm-skeleton">
        <SkeletonLines count={2} widths={["50%", "36%"]} />
      </div>
      <div className="hermes-lcm-stat hermes-lcm-skeleton">
        <SkeletonLines count={2} widths={["60%", "42%"]} />
      </div>
    </div>
  ) : null;
}
