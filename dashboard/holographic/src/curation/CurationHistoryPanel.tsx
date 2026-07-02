import { Button } from "../sdk";
import { Spinner } from "../Spinner";
import { CurrentPreviewSection } from "./CurrentPreviewSection";
import { MemoryOperationsSection } from "./MemoryOperationsSection";
import { RunHistorySection } from "./RunHistorySection";
import { SnapshotsSection } from "./SnapshotsSection";
import type {
  MemoryCurateResponse,
  MemoryCuratorStatusResponse,
  MemoryOplogEvent,
} from "../types";

interface CurationHistoryPanelProps {
  report: MemoryCurateResponse | null;
  previewSavedAt: string | null;
  previewStale: boolean;
  previewStaleReason: string;
  actionsLength: number;
  actionCounts: Array<[string, number]>;
  diagnosticCounts: Array<[string, number]>;
  isPlan: boolean;
  status: MemoryCuratorStatusResponse | null;
  statusLoading: boolean;
  statusError: string;
  oplog: MemoryOplogEvent[];
  oplogError: string;
  loadStatus: () => void;
  loadOplog: () => void;
}

export function CurationHistoryPanel({
  report,
  previewSavedAt,
  previewStale,
  previewStaleReason,
  actionsLength,
  actionCounts,
  diagnosticCounts,
  isPlan,
  status,
  statusLoading,
  statusError,
  oplog,
  oplogError,
  loadStatus,
  loadOplog,
}: CurationHistoryPanelProps) {
  return (
    <div
      role="tabpanel"
      id="curation-panel-history"
      aria-labelledby="curation-tab-history"
      className="flex flex-1 min-h-0 flex-col gap-3 overflow-y-auto overflow-x-hidden pr-1"
    >
      <div className="flex min-w-0 items-center justify-between gap-2 shrink-0">
        <div className="min-w-0">
          <div className="text-xs font-medium text-foreground">
            Curator History
          </div>
          <div className="text-[11px] text-text-tertiary">
            Run history, recent snapshots, and the memory operation log.
          </div>
        </div>
        <Button
          size="xs"
          ghost
          disabled={statusLoading}
          onClick={() => {
            loadStatus();
            loadOplog();
          }}
          className="shrink-0 gap-2"
        >
          {statusLoading ? <Spinner /> : null}
          Refresh
        </Button>
      </div>
      {statusError ? (
        <div className="border border-destructive/30 bg-destructive/10 px-3 py-2 text-xs text-destructive shrink-0">
          {statusError}
        </div>
      ) : null}
      {status ? (
        <>
          <RunHistorySection status={status} />
          <SnapshotsSection snapshots={status.snapshots} />
        </>
      ) : null}
      <MemoryOperationsSection events={oplog} error={oplogError} />
      <CurrentPreviewSection
        report={report}
        previewSavedAt={previewSavedAt}
        previewStale={previewStale}
        previewStaleReason={previewStaleReason}
        actionsLength={actionsLength}
        actionCounts={actionCounts}
        diagnosticCounts={diagnosticCounts}
        isPlan={isPlan}
      />
    </div>
  );
}
