/**
 * Savings & Cost tab root: view switcher (Savings / Sessions / Models &
 * Pricing) + time-range selector, loading data from
 * `/api/plugins/savings/*` and the shared price table from `/pricing`.
 */

import React, { useCallback, useEffect, useMemo, useState } from "react";
import { cn } from "./sdk";
import { api } from "./api";
import { fmtTokens } from "./logic";
import type { PriceTable } from "./pricing";
import SavingsOverviewPanel from "./SavingsOverviewPanel";
import SessionsPanel from "./SessionsPanel";
import ModelsPanel from "./ModelsPanel";
import type {
  LedgerResponse,
  ModelsResponse,
  PricingResponse,
  SavingsOverview,
  SessionsResponse,
} from "./types";

const VIEWS = [
  { id: "savings", label: "Savings" },
  { id: "sessions", label: "Sessions" },
  { id: "models", label: "Models & Pricing" },
] as const;

type ViewId = (typeof VIEWS)[number]["id"];

const RANGES = [
  { id: "all", label: "All time" },
  { id: "today", label: "Today" },
  { id: "7d", label: "7 days" },
  { id: "30d", label: "30 days" },
] as const;

const PAGE_SIZE = 25;

export default function SavingsExplorer() {
  const [view, setView] = useState<ViewId>("savings");
  const [range, setRange] = useState<string>("all");
  const [page, setPage] = useState(0);
  const [overview, setOverview] = useState<SavingsOverview | null>(null);
  const [ledger, setLedger] = useState<LedgerResponse | null>(null);
  const [sessions, setSessions] = useState<SessionsResponse | null>(null);
  const [models, setModels] = useState<ModelsResponse | null>(null);
  const [pricing, setPricing] = useState<PricingResponse | null>(null);
  const [error, setError] = useState<string>("");

  const prices: PriceTable = useMemo(() => pricing?.models || {}, [pricing]);

  const load = useCallback(async () => {
    setError("");
    try {
      const [overviewData, ledgerData, sessionsData, modelsData, pricingData] =
        await Promise.all([
          api.overview(),
          api.ledger({ range }),
          api.sessions({ range, limit: PAGE_SIZE, offset: page * PAGE_SIZE }),
          api.models({ range }),
          api.pricing(),
        ]);
      setOverview(overviewData);
      setLedger(ledgerData);
      setSessions(sessionsData);
      setModels(modelsData);
      setPricing(pricingData);
    } catch (err) {
      setError(String(err));
    }
  }, [range, page]);

  useEffect(() => {
    load();
  }, [load]);

  const sessionStats = overview?.sessions;

  return (
    <div className="tss-root">
      <div className="tss-toolbar">
        <div className="tss-toolbar-left">
          <span className="tss-kicker">Savings &amp; Cost</span>
          <div className="tss-views" role="group" aria-label="Savings views">
            {VIEWS.map((entry) => (
              <button
                key={entry.id}
                className={cn("tss-view-tab", view === entry.id && "tss-view-tab-active")}
                onClick={() => setView(entry.id)}
              >
                {entry.label}
              </button>
            ))}
          </div>
        </div>
        <div className="tss-toolbar-right">
          <label className="tss-range-label" htmlFor="tss-range">
            Range
          </label>
          <select
            id="tss-range"
            className="tss-range"
            value={range}
            onChange={(event) => {
              setRange(event.target.value);
              setPage(0);
            }}
          >
            {RANGES.map((entry) => (
              <option key={entry.id} value={entry.id}>
                {entry.label}
              </option>
            ))}
          </select>
        </div>
      </div>

      {sessionStats?.available && (
        <div className="tss-meta-strip tss-meta-strip-top">
          <span className="tss-meta-item">
            {fmtTokens(sessionStats.session_count)} sessions ·{" "}
            {fmtTokens(sessionStats.messages)} messages
          </span>
          <span className="tss-meta-item">
            {fmtTokens(sessionStats.usage_messages)} with transcript usage ·{" "}
            {fmtTokens(sessionStats.estimated_messages)} estimated (~4 chars/token)
          </span>
          {(sessionStats.unknown_model_messages || 0) > 0 && (
            <span className="tss-meta-item">
              {fmtTokens(sessionStats.unknown_model_messages)} messages without a model id
            </span>
          )}
        </div>
      )}

      {error && (
        <div className="tss-error" role="alert">
          Failed to load savings data: {error}{" "}
          <button className="tss-retry" onClick={load}>
            Retry
          </button>
        </div>
      )}

      {view === "savings" && (
        <SavingsOverviewPanel overview={overview} ledger={ledger} prices={prices} />
      )}
      {view === "sessions" && (
        <SessionsPanel
          data={sessions}
          prices={prices}
          page={page}
          onPage={setPage}
          pageSize={PAGE_SIZE}
        />
      )}
      {view === "models" && (
        <ModelsPanel data={models} pricing={pricing} prices={prices} />
      )}
    </div>
  );
}
