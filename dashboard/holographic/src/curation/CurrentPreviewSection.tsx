import { formatCounts, formatHistoryTime } from "./format";
import { MetadataRow } from "./MetadataRow";
import type { MemoryCurateResponse } from "../types";

export function CurrentPreviewSection({
  report,
  previewSavedAt,
  previewStale,
  previewStaleReason,
  actionsLength,
  actionCounts,
  diagnosticCounts,
  isPlan,
}: {
  report: MemoryCurateResponse | null;
  previewSavedAt: string | null;
  previewStale: boolean;
  previewStaleReason: string;
  actionsLength: number;
  actionCounts: Array<[string, number]>;
  diagnosticCounts: Array<[string, number]>;
  isPlan: boolean;
}) {
  if (!report) {
    return (
      <div className="border border-border bg-background/30 px-3 py-4 text-xs text-text-tertiary">
        Preview a plan to see current run metadata, signals, and coverage.
      </div>
    );
  }

  return (
    <>
      <div className="text-xs font-medium text-foreground">
        Current Preview
      </div>
      <div className="border border-border bg-background/30 px-3">
        <MetadataRow label="Run mode" value={isPlan ? "preview" : "applied"} />
        {previewSavedAt ? (
          <MetadataRow label="Saved" value={formatHistoryTime(previewSavedAt)} />
        ) : null}
        {previewStale ? (
          <MetadataRow
            label="Preview state"
            value={previewStaleReason || "stale"}
          />
        ) : null}
        <MetadataRow label="Actions" value={actionsLength} />
        <MetadataRow label="Counts" value={formatCounts(actionCounts)} />
        <MetadataRow label="Signals" value={formatCounts(diagnosticCounts)} />
        <MetadataRow label="LLM calls" value={report.llm_calls} />
        {report.skipped_actions != null ? (
          <MetadataRow label="Skipped" value={report.skipped_actions} />
        ) : null}
        {report.snapshot ? (
          <MetadataRow label="Snapshot" value={report.snapshot} />
        ) : null}
      </div>
      {report.coverage ? (
        <div className="border border-border bg-background/30 px-3">
          <MetadataRow
            label="Facts scanned"
            value={`${report.coverage.scanned}/${report.coverage.active_total}`}
          />
          <MetadataRow
            label="Facts due"
            value={report.coverage.due_remaining}
          />
          {report.coverage.entity_total != null ? (
            <MetadataRow
              label="Entities scanned"
              value={`${report.coverage.entities_scanned ?? 0}/${report.coverage.entity_total}`}
            />
          ) : null}
          {report.coverage.entity_scan_remaining != null ? (
            <MetadataRow
              label="Entities due"
              value={report.coverage.entity_scan_remaining}
            />
          ) : null}
        </div>
      ) : null}
    </>
  );
}
