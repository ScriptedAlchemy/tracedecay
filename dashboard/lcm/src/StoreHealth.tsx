/* eslint-disable @typescript-eslint/no-explicit-any */

/**
 * Store maintenance card: surfaces the LCM payload-health diagnostics
 * (`GET /payloads/health`) and drives the two-step payload GC flow
 * (`GET /payloads/gc` dry-run preview, then `POST /payloads/gc` with the
 * returned token to apply).
 */

import React, { useCallback, useEffect, useState } from "react";
import { fetchJSON } from "../../lib/sdk";
import { ErrorPanel, SkeletonLines } from "../../lib/primitives";
import { API, fmtInt, friendlyError } from "./helpers";
import { TimeText } from "./components";

function fmtBytes(value: any): string {
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

function gcPhaseSummary(report: any): string {
  if (!report) return "";
  const parts: string[] = [];
  const phases: Array<[string, any]> = [
    ["orphans", report.orphans],
    ["unreferenced", report.unreferenced],
    ["missing", report.missing],
    ["dangling", report.dangling],
  ];
  for (const [name, phase] of phases) {
    if (phase && phase.count) {
      parts.push(`${name}=${fmtInt(phase.count)}${phase.bytes ? ` (${fmtBytes(phase.bytes)})` : ""}`);
    }
  }
  if (report.deferred && report.deferred.count) {
    parts.push(`deferred=${fmtInt(report.deferred.count)}`);
  }
  return parts.length ? parts.join(" · ") : "nothing to reclaim";
}

export function StoreHealthCard(): React.ReactElement {
  const [health, setHealth] = useState<any>(null);
  const [healthLoading, setHealthLoading] = useState(false);
  const [healthError, setHealthError] = useState("");
  const [reloadToken, setReloadToken] = useState(0);

  const [gcPreview, setGcPreview] = useState<any>(null);
  const [gcApplied, setGcApplied] = useState<any>(null);
  const [gcBusy, setGcBusy] = useState(false);
  const [gcError, setGcError] = useState("");

  useEffect(function () {
    let active = true;
    setHealthLoading(true);
    setHealthError("");
    fetchJSON(`${API}/payloads/health`).then(function (json) {
      if (active) setHealth(json);
    }).catch(function (err) {
      if (active) setHealthError(friendlyError(err));
    }).finally(function () {
      if (active) setHealthLoading(false);
    });
    return function () { active = false; };
  }, [reloadToken]);

  const refresh = useCallback(function () {
    setReloadToken(function (n) { return n + 1; });
  }, []);

  const runPreview = useCallback(function () {
    setGcBusy(true);
    setGcError("");
    setGcApplied(null);
    fetchJSON(`${API}/payloads/gc`).then(function (json: any) {
      setGcPreview(json);
    }).catch(function (err) {
      setGcPreview(null);
      setGcError(friendlyError(err));
    }).finally(function () {
      setGcBusy(false);
    });
  }, []);

  const runApply = useCallback(function () {
    if (!gcPreview || !gcPreview.dry_run_token) return;
    setGcBusy(true);
    setGcError("");
    fetchJSON(`${API}/payloads/gc`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        provider: gcPreview.provider,
        session_id: gcPreview.session_id || "",
        confirm: true,
        dry_run_token: gcPreview.dry_run_token,
      }),
    }).then(function (json: any) {
      setGcApplied(json);
      setGcPreview(null);
      refresh();
    }).catch(function (err) {
      setGcError(friendlyError(err));
    }).finally(function () {
      setGcBusy(false);
    });
  }, [gcPreview, refresh]);

  const payload = (health && health.payload_health) || null;
  const previewReport = gcPreview && gcPreview.gc_report;
  const appliedReport = gcApplied && gcApplied.gc_report;
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
      {healthError
        ? <ErrorPanel error={healthError} onRetry={refresh} className="hermes-lcm-error" />
        : null}
      {!payload && healthLoading
        ? <SkeletonLines count={3} widths={["90%", "75%", "60%"]} />
        : null}
      {payload ? (
        <>
          <div className="hermes-lcm-row-meta hermes-lcm-storehealth-stats">
            <span className="hermes-lcm-pill">
              {fmtInt(payload.externalized_count)} externalized · {fmtBytes(payload.total_bytes)}
            </span>
            <span className="hermes-lcm-pill">
              reclaimable {fmtBytes(payload.reclaimable_bytes_after_grace)}
            </span>
            {payload.orphan_file_count ? (
              <span className="hermes-lcm-pill">
                {fmtInt(payload.orphan_file_count)} orphan files · {fmtBytes(payload.orphan_file_bytes)}
              </span>
            ) : null}
            {payload.missing_count ? (
              <span className="hermes-lcm-pill">{fmtInt(payload.missing_count)} missing payloads</span>
            ) : null}
            {payload.missing_placeholder_file_count ? (
              <span className="hermes-lcm-pill">
                {fmtInt(payload.missing_placeholder_file_count)} missing placeholder refs
              </span>
            ) : null}
            {payload.tombstoned_count ? (
              <span className="hermes-lcm-pill">{fmtInt(payload.tombstoned_count)} tombstoned</span>
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
              title={gcPreview ? "Apply the previewed GC (deletes unreferenced payload files)" : "Run a preview first"}
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
              <span className="hermes-lcm-dim">GC has never run for this store</span>
            )}
            {attentionCount === 0 && !gcPreview && !gcApplied ? (
              <span className="hermes-lcm-dim">nothing needs attention</span>
            ) : null}
          </div>
          {gcError ? <ErrorPanel error={gcError} className="hermes-lcm-error" /> : null}
          {previewReport ? (
            <div className="hermes-lcm-dim hermes-lcm-storehealth-report">
              dry run: {gcPhaseSummary(previewReport)}
            </div>
          ) : null}
          {appliedReport ? (
            <div className="hermes-lcm-dim hermes-lcm-storehealth-report">
              applied: removed {fmtInt(appliedReport.totals && appliedReport.totals.files)} files
              {" · "}{fmtBytes(appliedReport.totals && appliedReport.totals.bytes)} reclaimed
              {" · "}{fmtInt(appliedReport.totals && appliedReport.totals.rows_deleted)} rows deleted
              {" · "}{fmtInt(appliedReport.totals && appliedReport.totals.placeholders_rewritten)} placeholders rewritten
            </div>
          ) : null}
        </>
      ) : null}
    </div>
  );
}
