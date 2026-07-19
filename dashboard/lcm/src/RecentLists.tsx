/* eslint-disable @typescript-eslint/no-explicit-any */

/**
 * Recent-activity lists of the hermes-lcm dashboard: Recent Sessions and
 * Latest Summaries row lists with empty / skeleton states. Extracted 1:1
 * from `App.tsx` — DOM structure and class names unchanged.
 */

import React from "react";
import {
  fmtInt,
  sessionLabel,
  sessionTail,
  short,
  stripMd,
  summaryTitle,
} from "./helpers";
import { TimeText } from "./components";
import { EmptyState, SkeletonLines } from "../../lib/primitives";

interface RecentListsProps {
  data: any;
  onOpenSession: (id: any, opts?: any) => void;
  onOpenNode: (id: any) => void;
}

export function RecentLists({
  data,
  onOpenSession,
  onOpenNode,
}: RecentListsProps): React.ReactElement {
  return (
    <div className="hermes-lcm-grid">
      <div className="hermes-lcm-card">
        <h3>Recent Sessions</h3>
        <div className="hermes-lcm-rows">
          {((data && data.latest_sessions) || []).length ? (
            ((data && data.latest_sessions) || []).map(function (s, idx) {
              const tail = sessionTail(s.session_id);
              return (
                <button
                  key={s.session_id + ":" + idx}
                  type="button"
                  className="hermes-lcm-row"
                  onClick={function () {
                    onOpenSession(s.session_id);
                  }}
                >
                  <div className="hermes-lcm-row-main">
                    <span className="hermes-lcm-row-title">
                      {sessionLabel(s.session_id)}
                    </span>
                    {tail ? (
                      <span className="hermes-lcm-row-id">{tail}</span>
                    ) : null}
                  </div>
                  <div className="hermes-lcm-row-meta">
                    <span className="hermes-lcm-pill">
                      {fmtInt(s.message_count) + " msgs"}
                    </span>
                    <TimeText
                      className="hermes-lcm-dim"
                      epoch={s.last_timestamp}
                    />
                  </div>
                </button>
              );
            })
          ) : data ? (
            <EmptyState className="hermes-lcm-empty">No sessions</EmptyState>
          ) : (
            <SkeletonLines count={3} widths={["92%", "84%", "76%"]} />
          )}
        </div>
      </div>
      <div className="hermes-lcm-card">
        <h3>Latest Summaries</h3>
        <div className="hermes-lcm-rows">
          {((data && data.latest_summary_nodes) || []).length ? (
            ((data && data.latest_summary_nodes) || []).map(function (n) {
              const title = summaryTitle(n.summary);
              const preview = stripMd(n.summary);
              return (
                <button
                  key={n.node_id}
                  type="button"
                  className="hermes-lcm-row"
                  onClick={function () {
                    onOpenNode(n.node_id);
                  }}
                >
                  <div className="hermes-lcm-row-meta">
                    <span className="hermes-lcm-pill hermes-lcm-pill-accent">
                      {"D" + n.depth}
                    </span>
                    {n.category ? (
                      <span className="hermes-lcm-pill">{n.category}</span>
                    ) : null}
                    <span className="hermes-lcm-dim">
                      {sessionLabel(n.session_id)}
                    </span>
                    {n.token_count != null ? (
                      <span className="hermes-lcm-dim">
                        {fmtInt(n.token_count) + " tok"}
                      </span>
                    ) : null}
                  </div>
                  <div className="hermes-lcm-row-title">
                    {short(title, 80)}
                  </div>
                  <div className="hermes-lcm-row-sub">
                    {short(preview, 150)}
                  </div>
                </button>
              );
            })
          ) : data ? (
            <EmptyState className="hermes-lcm-empty">No summaries</EmptyState>
          ) : (
            <SkeletonLines count={3} widths={["90%", "82%", "74%"]} />
          )}
        </div>
      </div>
    </div>
  );
}
