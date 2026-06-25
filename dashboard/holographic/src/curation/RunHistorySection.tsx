import { formatHistoryTime } from "./format";
import { MetadataRow } from "./MetadataRow";
import type { MemoryCuratorStatusResponse } from "../types";

export function RunHistorySection({
  status,
}: {
  status: MemoryCuratorStatusResponse;
}) {
  return (
    <div className="border border-border bg-background/30 px-3">
      <div className="pt-2 text-[11px] uppercase tracking-[0.08em] text-text-tertiary">
        Run history
      </div>
      <MetadataRow label="Provider" value={status.provider || "none"} />
      <MetadataRow label="Run count" value={status.state.run_count} />
      <MetadataRow
        label="Last apply"
        value={formatHistoryTime(status.state.last_run_at) || "never"}
      />
      <MetadataRow
        label="Last applied summary"
        value={status.state.last_run_summary || "none"}
      />
      <MetadataRow
        label="Last preview"
        value={formatHistoryTime(status.state.last_preview_at) || "never"}
      />
      <MetadataRow
        label="Last preview summary"
        value={status.state.last_preview_summary || "none"}
      />
    </div>
  );
}
