import React, { useCallback, useEffect, useState } from "react";
import { fetchJSON } from "../../lib/sdk";
import { ErrorPanel, SkeletonLines } from "../../lib/primitives";
import { API, fmtInt, friendlyError } from "./helpers";
import { TimeText } from "./components";

interface PayloadHealth {
  status?: string | null;
  externalized_count?: number | string | null;
  total_bytes?: number | string | null;
  reclaimable_bytes_after_grace?: number | string | null;
  orphan_file_count?: number | string | null;
  orphan_file_bytes?: number | string | null;
  missing_count?: number | string | null;
  missing_placeholder_file_count?: number | string | null;
  tombstoned_count?: number | string | null;
  gc_candidate_count?: number | string | null;
  last_gc_at?: number | string | null;
  last_gc_status?: string | null;
}

interface PayloadHealthResponse {
  payload_health?: PayloadHealth | null;
}

interface GcPhase {
  count?: number | string | null;
  bytes?: number | string | null;
}

interface GcTotals {
  files?: number | string | null;
  bytes?: number | string | null;
  rows_deleted?: number | string | null;
  placeholders_rewritten?: number | string | null;
}

interface GcReport {
  orphans?: GcPhase | null;
  unreferenced?: GcPhase | null;
  missing?: GcPhase | null;
  dangling?: GcPhase | null;
  deferred?: GcPhase | null;
  totals?: GcTotals | null;
}

interface GcResponse {
  provider?: string | null;
  session_id?: string | null;
  dry_run_token?: string | null;
  gc_report?: GcReport | null;
}

function fmtBytes(value: number | string | null | undefined): string {
  const bytes = Number(value) || 0;
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KiB", "MiB", "GiB", "TiB"];
  let scaled = bytes / 1024;
  let unit = 0;
  while (scaled >= 1024 && unit < units.length - 1) {
    scaled /= 1024;
    unit += 1;
  }
  return `${scaled >= 10 ? Math.round(scaled) : scaled.toFixed(1)} ${units[unit]}`;
}

function healthPillClass(status: string): string {
  if (status === "ok") return "hermes-lcm-pill hermes-lcm-pill-accent";
  return "hermes-lcm-pill";
}

function gcPhaseSummary(report: GcReport | null): string {
  if (!report) return "";
  const parts: string[] = [];
  const phases: Array<[string, GcPhase | null | undefined]> = [
    ["orphans", report.orphans],
    ["unreferenced", report.unreferenced],
    ["missing", report.missing],
    ["dangling", report.dangling],
  ];
  for (const [name, phase] of phases) {
    if (phase && phase.count) {
      parts.push(
        `${name}=${fmtInt(phase.count)}${phase.bytes ? ` (${fmtBytes(phase.bytes)})` : ""}`,
      );
    }
  }
  if (report.deferred && report.deferred.count) {
    parts.push(`deferred=${fmtInt(report.deferred.count)}`);
  }
  return parts.length ? parts.join(" · ") : "nothing to reclaim";
}

export function StoreHealthCard(): React.ReactElement {
  const [health, setHealth] = useState<PayloadHealthResponse | null>(null);
  const [healthLoading, setHealthLoading] = useState(false);
  const [healthError, setHealthError] = useState("");

  const [gcPreview, setGcPreview] = useState<GcResponse | null>(null);
  const [gcApplied, setGcApplied] = useState<GcResponse | null>(null);
  const [gcBusy, setGcBusy] = useState(false);
  const [gcError, setGcError] = useState("");

  const refresh = useCallback(function () {
    setHealthLoading(true);
    setHealthError("");
    fetchJSON<PayloadHealthResponse>(`${API}/payloads/health`)
      .then(function (json) {
        setHealth(json);
      })
      .catch(function (err) {
        setHealthError(friendlyError(err));
      })
      .finally(function () {
        setHealthLoading(false);
      });
  }, []);

  useEffect(
    function () {
      refresh();
    },
    [refresh],
  );

  const runPreview = useCallback(function () {
    setGcBusy(true);
    setGcError("");
    setGcApplied(null);
    fetchJSON<GcResponse>(`${API}/payloads/gc`)
      .then(function (json) {
        setGcPreview(json);
      })
      .catch(function (err) {
        setGcPreview(null);
        setGcError(friendlyError(err));
      })
      .finally(function () {
        setGcBusy(false);
      });
  }, []);

  const runApply = useCallback(
    function () {
      if (!gcPreview || !gcPreview.dry_run_token) return;
      setGcBusy(true);
      setGcError("");
      fetchJSON<GcResponse>(`${API}/payloads/gc`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          provider: gcPreview.provider,
          session_id: gcPreview.session_id || "",
          confirm: true,
          dry_run_token: gcPreview.dry_run_token,
        }),
      })
        .then(function (json: GcResponse) {
          setGcApplied(json);
          setGcPreview(null);
          refresh();
        })
        .catch(function (err) {
          setGcError(friendlyError(err));
        })
        .finally(function () {
          setGcBusy(false);
        });
    },
    [gcPreview, refresh],
  );

  const payload = (health && health.payload_health) || null;
  const previewReport = gcPreview && gcPreview.gc_report;
  const appliedReport = gcApplied && gcApplied.gc_report;
  const appliedTotals: GcTotals | null = appliedReport
    ? (appliedReport.totals ?? {})
    : null;
  const attentionCount = payload
    ? (Number(payload.missing_count) || 0) +
      (Number(payload.missing_placeholder_file_count) || 0) +
      (Number(payload.orphan_file_count) || 0) +
      (Number(payload.gc_candidate_count) || 0)
    : 0;

  return (
    <div className="hermes-lcm-card hermes-lcm-wide">
      <div className="hermes-lcm-section-head">
        <h3>Store Maintenance (external payloads)</h3>
        <div className="hermes-lcm-row-meta">
          {payload ? (
            <span className={healthPillClass(String(payload.status || ""))}>
              {String(payload.status || "unknown")}
            </span>
          ) : null}
          <button
            type="button"
            className="hermes-lcm-btn"
            disabled={healthLoading}
            onClick={refresh}
          >
            Refresh
          </button>
        </div>
      </div>
      {healthError ? (
        <ErrorPanel
          error={healthError}
          onRetry={refresh}
          className="hermes-lcm-error"
        />
      ) : null}
      {!payload && healthLoading ? (
        <SkeletonLines count={3} widths={["90%", "75%", "60%"]} />
      ) : null}
      {payload ? (
        <>
          <div className="hermes-lcm-row-meta hermes-lcm-storehealth-stats">
            <span className="hermes-lcm-pill">
              {fmtInt(payload.externalized_count)} externalized ·{" "}
              {fmtBytes(payload.total_bytes)}
            </span>
            <span className="hermes-lcm-pill">
              reclaimable {fmtBytes(payload.reclaimable_bytes_after_grace)}
            </span>
            {payload.orphan_file_count ? (
              <span className="hermes-lcm-pill">
                {fmtInt(payload.orphan_file_count)} orphan files ·{" "}
                {fmtBytes(payload.orphan_file_bytes)}
              </span>
            ) : null}
            {payload.missing_count ? (
              <span className="hermes-lcm-pill">
                {fmtInt(payload.missing_count)} missing payloads
              </span>
            ) : null}
            {payload.missing_placeholder_file_count ? (
              <span className="hermes-lcm-pill">
                {fmtInt(payload.missing_placeholder_file_count)} missing
                placeholder refs
              </span>
            ) : null}
            {payload.tombstoned_count ? (
              <span className="hermes-lcm-pill">
                {fmtInt(payload.tombstoned_count)} tombstoned
              </span>
            ) : null}
          </div>
          <div className="hermes-lcm-row-meta hermes-lcm-storehealth-actions">
            <button
              type="button"
              className="hermes-lcm-btn"
              disabled={gcBusy}
              onClick={runPreview}
            >
              Preview GC
            </button>
            <button
              type="button"
              className="hermes-lcm-btn"
              disabled={gcBusy || !gcPreview || !gcPreview.dry_run_token}
              title={
                gcPreview
                  ? "Apply the previewed GC (deletes unreferenced payload files)"
                  : "Run a preview first"
              }
              onClick={runApply}
            >
              Apply GC
            </button>
            {payload.last_gc_at ? (
              <span className="hermes-lcm-dim">
                last GC <TimeText epoch={payload.last_gc_at} />
                {payload.last_gc_status ? ` · ${payload.last_gc_status}` : ""}
              </span>
            ) : (
              <span className="hermes-lcm-dim">
                GC has never run for this store
              </span>
            )}
            {attentionCount === 0 && !gcPreview && !gcApplied ? (
              <span className="hermes-lcm-dim">nothing needs attention</span>
            ) : null}
          </div>
          {gcError ? (
            <ErrorPanel error={gcError} className="hermes-lcm-error" />
          ) : null}
          {previewReport ? (
            <div className="hermes-lcm-dim hermes-lcm-storehealth-report">
              dry run: {gcPhaseSummary(previewReport)}
            </div>
          ) : null}
          {appliedTotals ? (
            <div className="hermes-lcm-dim hermes-lcm-storehealth-report">
              applied: removed {fmtInt(appliedTotals.files)} files
              {" · "}
              {fmtBytes(appliedTotals.bytes)} reclaimed
              {" · "}
              {fmtInt(appliedTotals.rows_deleted)} rows deleted
              {" · "}
              {fmtInt(appliedTotals.placeholders_rewritten)} placeholders
              rewritten
            </div>
          ) : null}
        </>
      ) : null}
    </div>
  );
}
