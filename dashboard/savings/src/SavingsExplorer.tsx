/**
 * Savings & Cost tab root: view switcher (Savings / Sessions / Models &
 * Pricing) + time-range selector, loading data from
 * `/api/plugins/savings/*` and the shared price table from `/pricing`.
 */

import React, { useCallback, useEffect, useMemo, useState } from "react";
import { cn } from "../../lib/sdk";
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
  // Bumped by the Retry button; every fetch effect below depends on it.
  const [retryToken, setRetryToken] = useState(0);

  const prices: PriceTable = useMemo(() => pricing?.models || {}, [pricing]);

  const retry = useCallback(() => setRetryToken((token) => token + 1), []);

  /**
   * Runs `fetch()` inside an effect, dropping the response after unmount or
   * a dependency change so stale results never overwrite newer ones.
   */
  function fetchIntoState<T>(
    fetch: () => Promise<T>,
    setState: (value: T) => void,
  ): () => void {
    let active = true;
    setError("");
    fetch().then(
      (data) => {
        if (active) setState(data);
      },
      (err) => {
        if (active) setError(String(err));
      },
    );
    return () => {
      active = false;
    };
  }

  // Each view fetches only what it renders. The overview (meta strip +
  // savings stats) and the price table take no range/page params, so they
  // load once; `sessions` is the only request that depends on the page, so
  // paging the sessions table costs exactly one request.
  useEffect(() => {
    const cancelOverview = fetchIntoState(() => api.overview(), setOverview);
    const cancelPricing = fetchIntoState(() => api.pricing(), setPricing);
    return () => {
      cancelOverview();
      cancelPricing();
    };
  }, [retryToken]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (view !== "savings") return;
    return fetchIntoState(() => api.ledger({ range }), setLedger);
  }, [view, range, retryToken]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (view !== "sessions") return;
    return fetchIntoState(
      () => api.sessions({ range, limit: PAGE_SIZE, offset: page * PAGE_SIZE }),
      setSessions,
    );
  }, [view, range, page, retryToken]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    if (view !== "models") return;
    return fetchIntoState(() => api.models({ range }), setModels);
  }, [view, range, retryToken]); // eslint-disable-line react-hooks/exhaustive-deps

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
            {fmtTokens(sessionStats.tokenized_messages)} tokenized (BPE) ·{" "}
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
          <button className="tss-retry" onClick={retry}>
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
