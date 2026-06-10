/**
 * Aggregate cost summaries: per-model and per-day rollups from the session
 * store, the `turns` accounting (actual costs from `tokensave cost`), and
 * the model-pricing source panel.
 */

import React from "react";
import { Badge, Card, CardContent, CardHeader, CardTitle, timeAgo } from "./sdk";
import { fillDailySeries, fmtTokens, fmtUsd } from "./logic";
import { rowCost } from "./pricing";
import type { PriceTable } from "./pricing";
import { DailyBars } from "./charts";
import { BasisBadge } from "./SessionsPanel";
import type { ModelsResponse, PricingResponse } from "./types";

const ShellCard = Card || "div";
const ShellCardHeader = CardHeader || "div";
const ShellCardTitle = CardTitle || "h3";
const ShellCardContent = CardContent || "div";
const ShellBadge = Badge || "span";

function PricingSourceCard({ pricing }: { pricing: PricingResponse | null }) {
  if (!pricing) return null;
  const sourceLabel =
    pricing.source === "cache"
      ? "OpenRouter (cached fetch)"
      : "bundled snapshot (offline fallback)";
  return (
    <ShellCard>
      <ShellCardHeader>
        <ShellCardTitle>Model pricing source</ShellCardTitle>
      </ShellCardHeader>
      <ShellCardContent>
        <div className="tss-meta-strip">
          <ShellBadge>{sourceLabel}</ShellBadge>
          <ShellBadge>{fmtTokens(pricing.model_count)} models</ShellBadge>
          {pricing.fetched_at && <ShellBadge>fetched {timeAgo(pricing.fetched_at)}</ShellBadge>}
          {pricing.offline && <ShellBadge>TOKENSAVE_OFFLINE=1 — network disabled</ShellBadge>}
        </div>
        <p className="tss-chart-hint">
          Prices come from OpenRouter’s public model list, cached at{" "}
          <code>{pricing.cache_path || "~/.tokensave/model-prices.json"}</code>{" "}
          and refreshed in the background at most once per day. When the cache
          is missing and the network is unavailable, a snapshot bundled with
          the binary keeps this tab working. Transcript model ids are
          fuzzy-matched to OpenRouter slugs; unmatched models show{" "}
          <em>no price data</em>.
        </p>
      </ShellCardContent>
    </ShellCard>
  );
}

export default function ModelsPanel({
  data,
  pricing,
  prices,
}: {
  data: ModelsResponse | null;
  pricing: PricingResponse | null;
  prices: PriceTable;
}) {
  if (!data) {
    return <div className="tss-empty">Loading model aggregates…</div>;
  }

  const dailyTokens = fillDailySeries(
    data.daily.map((row) => ({
      day: row.day,
      value:
        row.actual.input_tokens +
        row.actual.output_tokens +
        row.estimated.input_tokens +
        row.estimated.output_tokens,
    })),
    (row) => row.value,
  );
  const dailyCost = fillDailySeries(
    data.daily
      .map((row) => {
        const cost = rowCost(row, prices);
        return { day: row.day, value: cost.usd ?? 0 };
      })
      .concat(
        (data.turns.by_day || []).map((row) => ({
          day: row.day,
          value: row.cost_usd || 0,
        })),
      ),
    (row) => row.value,
  );

  return (
    <div className="tss-grid">
      <ShellCard>
        <ShellCardHeader>
          <ShellCardTitle>Cost by model</ShellCardTitle>
        </ShellCardHeader>
        <ShellCardContent>
          {data.models.length === 0 ? (
            <div className="tss-empty-mini">No session messages in this range.</div>
          ) : (
            <div className="tss-table-scroll">
              <table className="tss-table">
                <thead>
                  <tr>
                    <th>Model</th>
                    <th>OpenRouter slug</th>
                    <th>Sessions</th>
                    <th>Messages</th>
                    <th>Tokens in</th>
                    <th>Tokens out</th>
                    <th>$/MTok in·out</th>
                    <th>Cost</th>
                    <th>Basis</th>
                  </tr>
                </thead>
                <tbody>
                  {data.models.map((row, index) => {
                    const cost = rowCost(row, prices);
                    const inputTokens =
                      row.actual.input_tokens + row.estimated.input_tokens;
                    const outputTokens =
                      row.actual.output_tokens + row.estimated.output_tokens;
                    return (
                      <tr key={`${row.model || "unknown"}-${index}`}>
                        <td>{row.model || <em>unknown model</em>}</td>
                        <td>
                          {cost.resolved ? (
                            <span className="tss-slug">{cost.resolved.slug}</span>
                          ) : (
                            <em>no price data</em>
                          )}
                        </td>
                        <td>{fmtTokens(row.sessions)}</td>
                        <td>{fmtTokens(row.messages)}</td>
                        <td>{fmtTokens(inputTokens)}</td>
                        <td>{fmtTokens(outputTokens)}</td>
                        <td>
                          {cost.resolved
                            ? `${fmtUsd(cost.resolved.price.prompt_per_mtok)} · ${fmtUsd(cost.resolved.price.completion_per_mtok)}`
                            : "—"}
                        </td>
                        <td>{cost.usd === null ? "—" : fmtUsd(cost.usd)}</td>
                        <td>
                          <BasisBadge basis={row.cost_basis} />
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          )}
          {data.models.some((row) => !row.model) && (
            <p className="tss-chart-hint">
              <em>unknown model</em> rows aggregate messages whose transcripts
              recorded no model id — their tokens are counted but never priced.
            </p>
          )}
        </ShellCardContent>
      </ShellCard>

      <div className="tss-card-grid">
        <ShellCard>
          <ShellCardHeader>
            <ShellCardTitle>Tokens by day</ShellCardTitle>
          </ShellCardHeader>
          <ShellCardContent>
            <DailyBars
              series={dailyTokens}
              emptyText="No timestamped messages — Cursor hook ingests carry no per-message timestamps, so daily series need transcripts from providers that do (e.g. Claude Code)."
            />
          </ShellCardContent>
        </ShellCard>

        <ShellCard>
          <ShellCardHeader>
            <ShellCardTitle>Estimated cost by day</ShellCardTitle>
          </ShellCardHeader>
          <ShellCardContent>
            <DailyBars
              series={dailyCost}
              color="var(--ts-amber, #f7c76a)"
              valueLabel={(value) => fmtUsd(value)}
              emptyText="No dated cost data yet."
            />
          </ShellCardContent>
        </ShellCard>
      </div>

      {data.turns.available && data.turns.by_model.length > 0 && (
        <ShellCard>
          <ShellCardHeader>
            <ShellCardTitle>Claude Code accounting (actual)</ShellCardTitle>
          </ShellCardHeader>
          <ShellCardContent>
            <div className="tss-table-scroll">
              <table className="tss-table">
                <thead>
                  <tr>
                    <th>Model</th>
                    <th>Total tokens</th>
                    <th>Cost</th>
                    <th>Basis</th>
                  </tr>
                </thead>
                <tbody>
                  {data.turns.by_model.map((row) => (
                    <tr key={row.model}>
                      <td>{row.model}</td>
                      <td>{fmtTokens(row.total_tokens)}</td>
                      <td>{fmtUsd(row.cost_usd)}</td>
                      <td>
                        <BasisBadge basis="actual" />
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <p className="tss-chart-hint">
              Imported by <code>tokensave cost</code> from Claude Code
              transcripts, which record real usage data per turn.
            </p>
          </ShellCardContent>
        </ShellCard>
      )}

      <PricingSourceCard pricing={pricing} />
    </div>
  );
}
