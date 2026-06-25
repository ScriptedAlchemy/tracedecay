import React, { useCallback, useEffect, useMemo, useState } from "react";
import { cn } from "../../lib/sdk";
import { ErrorPanel } from "../../lib/primitives";
import { api } from "./api";
import { fmtTokens } from "./logic";
import type { PriceTable } from "./pricing";
import SavingsOverviewPanel from "./SavingsOverviewPanel";
import SessionsPanel from "./SessionsPanel";
import ModelsPanel from "./ModelsPanel";
import DiagnosticsPanel from "./DiagnosticsPanel";
import type {
  DiagnosticsResponse,
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
  { id: "diagnostics", label: "Diagnostics" },
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
  const [diagnostics, setDiagnostics] = useState<DiagnosticsResponse | null>(null);
  const [pricing, setPricing] = useState<PricingResponse | null>(null);
  const [error, setError] = useState<string>("");
  // Bumped by the Retry button; every fetch effect below depends on it.
  const [retryToken, setRetryToken] = useState(0);

  const prices: PriceTable = useMemo(() => pricing?.models || {}, [pricing]);

  const retry = useCallback(() => setRetryToken((token) => token + 1), []);

  const fetchIntoState = useCallback(<T,>(
    fetch: () => Promise<T>,
    setState: (value: T | null) => void,
    { clearBeforeLoad = false }: { clearBeforeLoad?: boolean } = {},
  ): () => void => {
    let active = true;
    setError("");
    if (clearBeforeLoad) setState(null);
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
  }, []);

  useEffect(() => {
    const cancelOverview = fetchIntoState(() => api.overview(), setOverview);
    const cancelPricing = fetchIntoState(() => api.pricing(), setPricing);
    return () => {
      cancelOverview();
      cancelPricing();
    };
  }, [fetchIntoState, retryToken]);

  useEffect(() => {
    if (view !== "savings") return;
    return fetchIntoState(() => api.ledger({ range }), setLedger, { clearBeforeLoad: true });
  }, [fetchIntoState, view, range, retryToken]);

  useEffect(() => {
    if (view !== "sessions") return;
    return fetchIntoState(
      () => api.sessions({ range, limit: PAGE_SIZE, offset: page * PAGE_SIZE }),
      setSessions,
      { clearBeforeLoad: true },
    );
  }, [fetchIntoState, view, range, page, retryToken]);

  useEffect(() => {
    if (view !== "models") return;
    return fetchIntoState(() => api.models({ range }), setModels, { clearBeforeLoad: true });
  }, [fetchIntoState, view, range, retryToken]);

  useEffect(() => {
    if (view !== "diagnostics") return;
    return fetchIntoState(() => api.diagnostics(), setDiagnostics, { clearBeforeLoad: true });
  }, [fetchIntoState, view, retryToken]);

  const sessionStats = overview?.sessions;

  return (
    <div className="tss-root">
      <div className="tss-toolbar">
        <div className="tss-toolbar-left">
          <span className="tss-kicker">Savings &amp; Cost</span>
          <div className="tss-views" role="tablist" aria-label="Savings views">
            {VIEWS.map((entry) => (
              <button
                key={entry.id}
                role="tab"
                aria-selected={view === entry.id}
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
        <ErrorPanel error={`Failed to load savings data: ${error}`} onRetry={retry} />
      )}

      {view === "savings" && (
        <SavingsOverviewPanel overview={overview} ledger={ledger} range={range} prices={prices} />
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
      {view === "diagnostics" && <DiagnosticsPanel data={diagnostics} />}
    </div>
  );
}
