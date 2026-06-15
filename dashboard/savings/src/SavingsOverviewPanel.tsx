/**
 * Token-savings view: ledger totals + per-tool / per-project breakdowns +
 * daily series, plus the legacy lifetime per-project counters.
 */

import React from "react";
import { Badge, Card, CardContent, CardHeader, CardTitle } from "../../lib/sdk";
import { fillDailySeries, fmtTokens, fmtUsd, projectLabel } from "./logic";
import { savedTokensUsd } from "./pricing";
import type { PriceTable } from "./pricing";
import { DailyBars, HBarChart } from "./charts";
import type { LedgerResponse, SavingsOverview } from "./types";

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
          The savings ledger lives in <code>~/.tracedecay/global.db</code>{" "}
          (override: <code>TRACEDECAY_GLOBAL_DB</code>), which could not be
          opened.
        </p>
      </div>
    );
  }

  const total = ledger?.total || savings.ledger?.all_time || { saved_tokens: 0, calls: 0 };
  const usd = savedTokensUsd(total.saved_tokens, prices);
  const lifetime = savings.lifetime_counters;
  const recording = savings.recording;
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

      {ledgerEmpty && recording && !recording.enabled && (
        <div className="tss-note tss-note-warn" role="note">
          <strong>Ledger recording is disabled by environment.</strong>{" "}
          {recording.mode === "disabled_by_env" &&
            "TRACEDECAY_DISABLE_GLOBAL_DB (or a falsy TRACEDECAY_ENABLE_GLOBAL_DB) is set, so MCP servers do not append savings_ledger rows. "}
          Unset it (or set <code>TRACEDECAY_ENABLE_GLOBAL_DB=1</code>) and
          restart your agent&apos;s tracedecay MCP server to start recording.
        </div>
      )}

      {ledgerEmpty && (!recording || recording.enabled) && (
        <div className="tss-note" role="note">
          The savings ledger has no events
          {ledger?.range && ledger.range !== "all" ? ` in range "${ledger.range}"` : ""} yet —
          rows are appended when tracedecay MCP tools return trimmed context
          (each row records before/after token counts per tool call).
          Recording is enabled in this environment, so an empty all-time
          ledger usually means the running MCP server was started from an
          older build (or with recording disabled) —{" "}
          <strong>restart/reload your agent&apos;s tracedecay MCP server</strong>{" "}
          to pick up ledger recording. The lifetime counters below come from
          the older <code>projects.tokens_saved</code> tally that{" "}
          <code>tracedecay gain</code> also reports.
        </div>
      )}

      <div className="tss-card-grid">
        <Card>
          <CardHeader>
            <CardTitle>Savings by day</CardTitle>
          </CardHeader>
          <CardContent>
            <DailyBars
              series={series}
              emptyText="No ledger events to chart yet."
            />
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Savings by tool</CardTitle>
          </CardHeader>
          <CardContent>
            <HBarChart
              rows={(ledger?.by_tool || []).slice(0, 12).map((row) => ({
                label: row.tool,
                count: row.saved_tokens,
                meta: `· ${fmtTokens(row.calls)} calls`,
              }))}
              emptyText="No per-tool ledger entries yet."
            />
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Savings by project (ledger)</CardTitle>
          </CardHeader>
          <CardContent>
            <HBarChart
              rows={(ledger?.by_project || []).slice(0, 12).map((row) => ({
                label: projectLabel(row.project),
                count: row.saved_tokens,
              }))}
              color="var(--ts-blue, #7aa7ff)"
              emptyText="No per-project ledger entries yet."
            />
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Lifetime counters by project</CardTitle>
          </CardHeader>
          <CardContent>
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
              number <code>tracedecay gain</code> reports as lifetime savings).
            </p>
          </CardContent>
        </Card>
      </div>

      <div className="tss-meta-strip">
        <Badge>ledger db: {savings.db}</Badge>
        <Badge>
          ledger calls (all time): {fmtTokens(savings.ledger?.all_time.calls)}
        </Badge>
        {recording && (
          <Badge>
            recording: {recording.enabled ? "on" : "off"} ({recording.mode})
          </Badge>
        )}
      </div>
    </div>
  );
}
