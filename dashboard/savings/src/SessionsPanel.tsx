/**
 * Session cost accounting: one row per session with model(s), token counts,
 * estimated/actual cost, and explicit cost-basis labeling.
 */

import React, { useState } from "react";
import { Badge, Button, Card, CardContent, CardHeader, CardTitle, cn, timeAgo } from "./sdk";
import { BASIS_LABELS, cleanTitle, fmtTokens, fmtUsd } from "./logic";
import type { CostBasis } from "./logic";
import { rowCost, summarizeCosts } from "./pricing";
import type { PriceTable } from "./pricing";
import type { SessionRow, SessionsResponse } from "./types";

const ShellCard = Card || "div";
const ShellCardHeader = CardHeader || "div";
const ShellCardTitle = CardTitle || "h3";
const ShellCardContent = CardContent || "div";
const ShellBadge = Badge || "span";
const ShellButton = Button || "button";

export function BasisBadge({ basis }: { basis: CostBasis }) {
  return (
    <span className={cn("tss-basis", `tss-basis-${basis}`)} title={BASIS_LABELS[basis]}>
      {basis}
    </span>
  );
}

function SessionDetails({ session, prices }: { session: SessionRow; prices: PriceTable }) {
  return (
    <div className="tss-session-detail">
      <table className="tss-table tss-table-inner">
        <thead>
          <tr>
            <th>Model</th>
            <th>Messages</th>
            <th>Input tokens</th>
            <th>Output tokens</th>
            <th>Cost</th>
            <th>Basis</th>
          </tr>
        </thead>
        <tbody>
          {session.models.map((modelRow, index) => {
            const cost = rowCost(modelRow, prices);
            const inputTokens =
              modelRow.actual.input_tokens + modelRow.estimated.input_tokens;
            const outputTokens =
              modelRow.actual.output_tokens + modelRow.estimated.output_tokens;
            return (
              <tr key={`${modelRow.model || "unknown"}-${index}`}>
                <td>
                  {modelRow.model || <em>unknown model</em>}
                  {cost.resolved && (
                    <span className="tss-slug"> → {cost.resolved.slug}</span>
                  )}
                </td>
                <td>{fmtTokens(modelRow.messages)}</td>
                <td>{fmtTokens(inputTokens)}</td>
                <td>{fmtTokens(outputTokens)}</td>
                <td>{cost.usd === null ? <em>no price data</em> : fmtUsd(cost.usd)}</td>
                <td>
                  <BasisBadge basis={modelRow.cost_basis} />
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
      {session.models.some((modelRow) => !modelRow.model) && (
        <p className="tss-chart-hint">
          Messages without a recorded model id keep their token counts but get
          no cost — there is nothing honest to price them against.
        </p>
      )}
    </div>
  );
}

export default function SessionsPanel({
  data,
  prices,
  page,
  onPage,
  pageSize,
}: {
  data: SessionsResponse | null;
  prices: PriceTable;
  page: number;
  onPage: (page: number) => void;
  pageSize: number;
}) {
  const [expanded, setExpanded] = useState<string | null>(null);

  if (!data) {
    return <div className="tss-empty">Loading session accounting…</div>;
  }
  if (!data.available) {
    return (
      <div className="tss-empty">
        <h3>Session store unavailable</h3>
        <p>No session database could be opened for this project.</p>
      </div>
    );
  }
  if (!data.sessions.length) {
    return (
      <div className="tss-empty">
        <h3>No sessions in this range</h3>
        <p>
          Sessions appear here once agent transcripts are ingested into the
          session store ({data.db}). Sessions without timestamps are only
          listed in the “All time” range.
        </p>
      </div>
    );
  }

  const pageCount = Math.max(1, Math.ceil(data.total / pageSize));

  return (
    <ShellCard>
      <ShellCardHeader>
        <ShellCardTitle>Session cost accounting</ShellCardTitle>
      </ShellCardHeader>
      <ShellCardContent>
        <div className="tss-table-scroll">
          <table className="tss-table">
            <thead>
              <tr>
                <th>Session</th>
                <th>Models</th>
                <th>Messages</th>
                <th>Tokens in</th>
                <th>Tokens out</th>
                <th>Cost</th>
                <th>Basis</th>
              </tr>
            </thead>
            <tbody>
              {data.sessions.map((session) => {
                const key = `${session.provider}:${session.session_id}`;
                const cost = summarizeCosts(session.models, prices);
                const inputTokens = session.models.reduce(
                  (sum, row) => sum + row.actual.input_tokens + row.estimated.input_tokens,
                  0,
                );
                const outputTokens = session.models.reduce(
                  (sum, row) => sum + row.actual.output_tokens + row.estimated.output_tokens,
                  0,
                );
                const isOpen = expanded === key;
                const when = session.started_at || session.last_message_at;
                return (
                  <React.Fragment key={key}>
                    <tr
                      className={cn("tss-session-row", isOpen && "tss-session-row-open")}
                      onClick={() => setExpanded(isOpen ? null : key)}
                    >
                      <td className="tss-session-title">
                        <span className="tss-caret">{isOpen ? "▾" : "▸"}</span>
                        {cleanTitle(session.title)}
                        {session.is_subagent && <ShellBadge>subagent</ShellBadge>}
                        <span className="tss-session-meta">
                          {session.session_id.slice(0, 8)}
                          {when ? ` · ${timeAgo(when)}` : " · no timestamp"}
                        </span>
                      </td>
                      <td>
                        <span className="tss-model-chips">
                          {session.models.slice(0, 3).map((modelRow, index) => (
                            <span key={index} className="tss-chip">
                              {modelRow.model || "unknown model"}
                            </span>
                          ))}
                          {session.models.length > 3 && (
                            <span className="tss-chip">+{session.models.length - 3}</span>
                          )}
                        </span>
                      </td>
                      <td>{fmtTokens(session.messages)}</td>
                      <td>{fmtTokens(inputTokens)}</td>
                      <td>{fmtTokens(outputTokens)}</td>
                      <td>
                        {cost.priced_rows === 0 ? (
                          <em>no price data</em>
                        ) : (
                          <>
                            {fmtUsd(cost.priced_usd)}
                            {cost.unpriced_models.length > 0 && (
                              <span className="tss-partial" title="Some models in this session have no price data; their tokens are excluded from the total.">
                                {" "}
                                partial
                              </span>
                            )}
                          </>
                        )}
                      </td>
                      <td>
                        <BasisBadge basis={session.cost_basis} />
                      </td>
                    </tr>
                    {isOpen && (
                      <tr className="tss-session-detail-row">
                        <td colSpan={7}>
                          <SessionDetails session={session} prices={prices} />
                        </td>
                      </tr>
                    )}
                  </React.Fragment>
                );
              })}
            </tbody>
          </table>
        </div>

        <div className="tss-pager">
          <ShellButton
            size="sm"
            ghost
            disabled={page <= 0}
            onClick={() => onPage(page - 1)}
          >
            ← Prev
          </ShellButton>
          <span className="tss-pager-label">
            page {page + 1} / {pageCount} · {fmtTokens(data.total)} sessions
          </span>
          <ShellButton
            size="sm"
            ghost
            disabled={page + 1 >= pageCount}
            onClick={() => onPage(page + 1)}
          >
            Next →
          </ShellButton>
        </div>
        <p className="tss-chart-hint">
          Estimated token counts use the same ~4 chars/token heuristic as the
          LCM views and only cover stored message text — real context windows
          (resent history, tool payloads) are larger, so estimated costs are a
          lower bound. Rows marked <strong>actual</strong> come from usage
          records in the transcript itself.
        </p>
      </ShellCardContent>
    </ShellCard>
  );
}
