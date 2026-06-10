/**
 * Token-savings view: ledger totals + per-tool / per-project breakdowns +
 * daily series, plus the legacy lifetime per-project counters.
 */

import React from "react";
import { Badge, Card, CardContent, CardHeader, CardTitle } from "./sdk";
import { fillDailySeries, fmtTokens, fmtUsd, projectLabel } from "./logic";
import { savedTokensUsd } from "./pricing";
import type { PriceTable } from "./pricing";
import { DailyBars, HBarChart } from "./charts";
import type { LedgerResponse, SavingsOverview } from "./types";

const ShellCard = Card || "div";
const ShellCardHeader = CardHeader || "div";
const ShellCardTitle = CardTitle || "h3";
const ShellCardContent = CardContent || "div";
const ShellBadge = Badge || "span";

function StatCard({
  label,
  value,
  hint,
}: {
  label: string;
  value: string;
  hint?: string;
}) {
  return (
    <div className="tss-stat">
      <div className="tss-stat-value">{value}</div>
      <div className="tss-stat-label">{label}</div>
      {hint && <div className="tss-stat-hint">{hint}</div>}
    </div>
  );
}

export default function SavingsOverviewPanel({
  overview,
  ledger,
  prices,
}: {
  overview: SavingsOverview | null;
  ledger: LedgerResponse | null;
  prices: PriceTable;
}) {
  if (!overview) {
    return <div className="tss-empty">Loading savings analytics…</div>;
  }
  const savings = overview.savings;
  if (!savings.available) {
    return (
      <div className="tss-empty">
        <h3>Global accounting database unavailable</h3>
        <p>
          The savings ledger lives in <code>~/.tokensave/global.db</code>{" "}
          (override: <code>TOKENSAVE_GLOBAL_DB</code>), which could not be
          opened.
        </p>
      </div>
    );
  }

  const total = ledger?.total || savings.ledger?.all_time || { saved_tokens: 0, calls: 0 };
  const usd = savedTokensUsd(total.saved_tokens, prices);
  const lifetime = savings.lifetime_counters;
  const series = fillDailySeries(
    (ledger?.by_day || []).map((day) => ({ day: day.day, value: day.saved_tokens })),
    (row) => row.value,
  );
  const ledgerEmpty = total.calls === 0;

  return (
    <div className="tss-grid">
      <div className="tss-stat-row">
        <StatCard
          label={`Tokens saved (${ledger?.range || "all"})`}
          value={fmtTokens(total.saved_tokens)}
          hint={`${fmtTokens(total.calls)} tool calls in the ledger`}
        />
        <StatCard
          label="Estimated value saved"
          value={usd === null ? "no price data" : fmtUsd(usd)}
          hint="estimated (computed) at the Claude Sonnet input rate"
        />
        <StatCard
          label="Saved last 7 days"
          value={fmtTokens(savings.ledger?.last_7d.saved_tokens)}
          hint={`today: ${fmtTokens(savings.ledger?.today.saved_tokens)}`}
        />
        <StatCard
          label="Lifetime counter (all projects)"
          value={fmtTokens(lifetime?.total_tokens_saved)}
          hint="legacy gross counters, predates the ledger — see note below"
        />
      </div>

      <div className="tss-note" role="note">
        <strong>How savings are measured.</strong> Each MCP tool call records{" "}
        <em>before</em> = the indexed size of every file the answer references
        (bytes ÷ 4, as if the agent had read each file in full) and{" "}
        <em>after</em> = the size of the tool&apos;s actual response (chars ÷
        4). Saved = max(0, before − after) per call. This counterfactual is an{" "}
        <strong>estimated upper bound</strong>: chars ÷ 4 only approximates
        real tokenization, repeated calls re-count the same files, and an
        agent would not always have read every referenced file raw. Lifetime
        counters accumulated before the ledger existed credited the gross{" "}
        <em>before</em> estimate without subtracting responses, so treat them
        as looser upper bounds than the ledger.
      </div>

      {ledgerEmpty && (
        <div className="tss-note" role="note">
          The savings ledger has no events
          {ledger?.range && ledger.range !== "all" ? ` in range "${ledger.range}"` : ""} yet —
          rows are appended when tokensave MCP tools return trimmed context
          (each row records before/after token counts per tool call). The
          lifetime counters below come from the older{" "}
          <code>projects.tokens_saved</code> tally that <code>tokensave gain</code>{" "}
          also reports.
        </div>
      )}

      <div className="tss-card-grid">
        <ShellCard>
          <ShellCardHeader>
            <ShellCardTitle>Savings by day</ShellCardTitle>
          </ShellCardHeader>
          <ShellCardContent>
            <DailyBars
              series={series}
              emptyText="No ledger events to chart yet."
            />
          </ShellCardContent>
        </ShellCard>

        <ShellCard>
          <ShellCardHeader>
            <ShellCardTitle>Savings by tool</ShellCardTitle>
          </ShellCardHeader>
          <ShellCardContent>
            <HBarChart
              rows={(ledger?.by_tool || []).slice(0, 12).map((row) => ({
                label: row.tool,
                count: row.saved_tokens,
                meta: `· ${fmtTokens(row.calls)} calls`,
              }))}
              emptyText="No per-tool ledger entries yet."
            />
          </ShellCardContent>
        </ShellCard>

        <ShellCard>
          <ShellCardHeader>
            <ShellCardTitle>Savings by project (ledger)</ShellCardTitle>
          </ShellCardHeader>
          <ShellCardContent>
            <HBarChart
              rows={(ledger?.by_project || []).slice(0, 12).map((row) => ({
                label: projectLabel(row.project),
                count: row.saved_tokens,
              }))}
              color="var(--ts-blue, #7aa7ff)"
              emptyText="No per-project ledger entries yet."
            />
          </ShellCardContent>
        </ShellCard>

        <ShellCard>
          <ShellCardHeader>
            <ShellCardTitle>Lifetime counters by project</ShellCardTitle>
          </ShellCardHeader>
          <ShellCardContent>
            <HBarChart
              rows={(lifetime?.projects || []).slice(0, 12).map((row) => ({
                label: projectLabel(row.path),
                count: row.tokens_saved,
              }))}
              color="var(--ts-green, #67e8a9)"
              emptyText="No projects with recorded savings yet."
            />
            <p className="tss-chart-hint">
              Running totals kept since each project was initialized (the
              number <code>tokensave gain</code> reports as lifetime savings).
            </p>
          </ShellCardContent>
        </ShellCard>
      </div>

      <div className="tss-meta-strip">
        <ShellBadge>ledger db: {savings.db}</ShellBadge>
        <ShellBadge>
          ledger calls (all time): {fmtTokens(savings.ledger?.all_time.calls)}
        </ShellBadge>
      </div>
    </div>
  );
}
